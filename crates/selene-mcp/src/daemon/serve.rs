//! The serve entry point and the three modes it fans out to.
//!
//! `selene serve --mcp` reaches [`launch`], which picks exactly one of:
//!
//! - **direct** — no daemon at all: run an MCP session straight over this process's stdio. Chosen
//!   when `SELENE_NO_DAEMON=1`, when there is no project root, or as the fallback when a daemon
//!   cannot be reached. This is the pre-daemon behavior, preserved verbatim.
//! - **daemon** — this process *is* the elected daemon (`SELENE_DAEMON_INTERNAL=1`, set only on the
//!   detached child we spawn). It holds the warm store and multiplexes MCP sessions over a Unix
//!   socket. See [`run_daemon`].
//! - **proxy** — the common case: connect to the project's live, version-matched daemon and pump
//!   this stdio session across the socket. If none exists, spawn a detached daemon and connect to
//!   it. See [`proxy_or_spawn`].
//!
//! # Why proxying, not sharing the store directly
//!
//! Two processes cannot both open the exclusive RocksDB store. So the agent's `serve --mcp` never
//! opens the store — it forwards raw MCP bytes to the one daemon that did. The launcher does no MCP
//! parsing; [`copy_bidirectional`](tokio::io::copy_bidirectional) shuttles bytes both ways until
//! either side hangs up.

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use tokio::net::{UnixListener, UnixStream};

use super::lock::{self, Acquire, PidRecord};
use super::proc::{is_alive, own_pid};
use super::{paths, registry};

/// This binary's version — the daemon and its proxies must match it **exactly** before a proxy will
/// forward to a daemon (a different version may speak a different protocol).
const OWN_VERSION: &str = env!("CARGO_PKG_VERSION");

const NO_DAEMON_ENV: &str = "SELENE_NO_DAEMON";
const INTERNAL_ENV: &str = "SELENE_DAEMON_INTERNAL";
const IDLE_TIMEOUT_ENV: &str = "SELENE_DAEMON_IDLE_TIMEOUT_MS";

/// Default idle reap: a daemon with zero clients for this long exits. `0` disables it.
const DEFAULT_IDLE_TIMEOUT_MS: u64 = 300_000;
/// Poll-connect budget after spawning a daemon: 240 × 25 ms ≈ 6 s.
const DAEMON_CONNECT_MAX_RETRIES: u32 = 240;
const CONNECT_RETRY_DELAY: Duration = Duration::from_millis(25);
/// Election retries when clearing a stale lock (a dead daemon's corpse).
const TAKEOVER_MAX_RETRIES: u32 = 5;
const TAKEOVER_DELAY: Duration = Duration::from_millis(100);

fn env_truthy(key: &str) -> bool {
    matches!(std::env::var(key).ok().as_deref(), Some(v) if v != "0" && !v.is_empty())
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// The `serve --mcp` entry. Dispatches to direct / daemon / proxy (see the module docs).
pub async fn launch(root: Option<PathBuf>) -> Result<()> {
    // The elected daemon child: be the daemon.
    if env_truthy(INTERNAL_ENV) {
        let root = root.context("the internal daemon requires a project root")?;
        return run_daemon(root).await;
    }
    // Opt-out, or nothing to share → the classic in-process session.
    match root {
        Some(root) if !env_truthy(NO_DAEMON_ENV) => proxy_or_spawn(root).await,
        other => direct_serve(other).await,
    }
}

/// A single MCP session over this process's stdio — no daemon involved.
async fn direct_serve(root: Option<PathBuf>) -> Result<()> {
    use rmcp::ServiceExt;
    let service = crate::SeleneMcp::new(root)
        .serve(rmcp::transport::stdio())
        .await
        .context("MCP handshake failed")?;
    service.waiting().await.context("MCP server stopped")?;
    Ok(())
}

// --- proxy --------------------------------------------------------------------------------------

/// What a probe of the project's pidfile + socket found.
enum Existing {
    /// A live, version-matched daemon we connected to.
    Connected(UnixStream),
    /// A live daemon of a *different* version — do not proxy; serve in-process instead.
    Incompatible,
    /// No usable daemon (absent, or a dead corpse we cleared).
    None,
}

/// Probe for an existing daemon and connect if it is live and compatible.
async fn connect_existing(root: &std::path::Path) -> Existing {
    let pid_path = paths::pid_path(root);
    let Some(rec) = lock::read(&pid_path) else {
        return Existing::None;
    };
    if !is_alive(rec.pid) {
        lock::clear_stale(&pid_path, rec.pid);
        return Existing::None;
    }
    if rec.version != OWN_VERSION {
        return Existing::Incompatible;
    }
    // Live and compatible — connect, tolerating a socket that is still coming up.
    let sock = paths::socket_path(root);
    for _ in 0..40 {
        match UnixStream::connect(&sock).await {
            Ok(s) => return Existing::Connected(s),
            Err(_) => tokio::time::sleep(CONNECT_RETRY_DELAY).await,
        }
    }
    // Pidfile says alive but the socket never answered — treat as unusable.
    Existing::None
}

/// Connect to the project's daemon (spawning one if needed) and proxy this stdio session to it.
async fn proxy_or_spawn(root: PathBuf) -> Result<()> {
    match connect_existing(&root).await {
        Existing::Connected(stream) => return proxy(stream).await,
        Existing::Incompatible => {
            eprintln!("selene: a different-version daemon is running; serving in-process");
            return direct_serve(Some(root)).await;
        }
        Existing::None => {}
    }

    // No daemon — spawn a detached one and connect to it.
    spawn_detached_daemon(&root).context("spawn daemon")?;
    let sock = paths::socket_path(&root);
    for _ in 0..DAEMON_CONNECT_MAX_RETRIES {
        if let Ok(stream) = UnixStream::connect(&sock).await {
            return proxy(stream).await;
        }
        tokio::time::sleep(CONNECT_RETRY_DELAY).await;
    }

    eprintln!("selene: daemon did not come up in time; serving in-process");
    direct_serve(Some(root)).await
}

/// Shuttle bytes between this process's stdio and the daemon socket until either side closes.
async fn proxy(mut stream: UnixStream) -> Result<()> {
    let mut client = tokio::io::join(tokio::io::stdin(), tokio::io::stdout());
    tokio::io::copy_bidirectional(&mut client, &mut stream)
        .await
        .context("proxy to daemon")?;
    Ok(())
}

/// Re-exec ourselves as a detached daemon: `selene serve --mcp --path <root>` with the internal
/// env set. Detached into its own process group so the launching shell's Ctrl-C never reaps it; its
/// stdio goes to `.selene/daemon.log` (best effort). We do **not** wait on it — it outlives us.
fn spawn_detached_daemon(root: &std::path::Path) -> Result<()> {
    use std::os::unix::process::CommandExt; // for `process_group`
    let exe = std::env::current_exe().context("locate own binary")?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(paths::data_dir(root).join("daemon.log"));

    let mut cmd = std::process::Command::new(exe);
    cmd.args(["serve", "--mcp", "--path"])
        .arg(root)
        .env(INTERNAL_ENV, "1")
        .env_remove(NO_DAEMON_ENV)
        .stdin(std::process::Stdio::null());
    match log {
        Ok(f) => {
            let f2 = f.try_clone().context("clone daemon log handle")?;
            cmd.stdout(f).stderr(f2);
        }
        Err(_) => {
            cmd.stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        }
    }
    // New process group: terminal signals to the launcher do not propagate to the daemon.
    cmd.process_group(0);
    cmd.spawn().context("spawn detached daemon")?;
    Ok(())
}

// --- daemon -------------------------------------------------------------------------------------

/// Be the daemon for `root`: win the election, bind the socket, hold the warm store, and multiplex
/// MCP sessions until idle (or signalled).
async fn run_daemon(root: PathBuf) -> Result<()> {
    let pid_path = paths::pid_path(&root);
    let sock = paths::socket_path(&root);

    // --- election: exactly one process serves ---------------------------------------------------
    let mut elected = false;
    for _ in 0..TAKEOVER_MAX_RETRIES {
        let record = PidRecord {
            pid: own_pid(),
            version: OWN_VERSION.to_string(),
            socket_path: sock.to_string_lossy().into_owned(),
            started_at: now_ms(),
        };
        match lock::acquire(&pid_path, &record).context("acquire daemon lock")? {
            Acquire::Acquired => {
                elected = true;
                break;
            }
            Acquire::Taken(Some(rec)) if is_alive(rec.pid) => {
                eprintln!("[selene daemon] another daemon is already running (pid {})", rec.pid);
                return Ok(());
            }
            Acquire::Taken(holder) => {
                // A dead corpse (or an unreadable file) — clear it and retry the election.
                if let Some(rec) = holder {
                    lock::clear_stale(&pid_path, rec.pid);
                }
                tokio::time::sleep(TAKEOVER_DELAY).await;
            }
        }
    }
    if !elected {
        eprintln!("[selene daemon] could not win the election; exiting");
        return Ok(());
    }

    // --- bind ----------------------------------------------------------------------------------
    // We hold the lock, so any socket file is a dead daemon's leftover — unlink then bind.
    let _ = std::fs::remove_file(&sock);
    let listener = match UnixListener::bind(&sock) {
        Ok(l) => l,
        Err(e) => {
            lock::release(&pid_path, own_pid());
            return Err(e).with_context(|| format!("bind {}", sock.display()));
        }
    };
    let _ = set_mode_0600(&sock);
    registry::register(&root, own_pid(), &sock);

    let idle_ms = std::env::var(IDLE_TIMEOUT_ENV)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_IDLE_TIMEOUT_MS);
    eprintln!(
        "[selene daemon] listening on {} (pid {}, v{OWN_VERSION}); idle timeout {idle_ms}ms",
        sock.display(),
        own_pid()
    );

    // Warm the store now so the first client's first query is already fast.
    crate::handlers::prewarm(&root).await;

    let outcome = accept_loop(listener, root.clone(), idle_ms).await;

    // --- shutdown ------------------------------------------------------------------------------
    registry::deregister(&root);
    let _ = std::fs::remove_file(&sock);
    lock::release(&pid_path, own_pid());
    outcome
}

/// Accept connections, one MCP session per connection over the warm store, until an idle timeout
/// with no clients (or SIGINT/SIGTERM) ends the loop.
async fn accept_loop(listener: UnixListener, root: PathBuf, idle_ms: u64) -> Result<()> {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::Notify;

    let active = Arc::new(AtomicUsize::new(0));
    let went_idle = Arc::new(Notify::new());
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;

    loop {
        let idle_armed = idle_ms > 0 && active.load(Ordering::SeqCst) == 0;
        let idle_sleep = tokio::time::sleep(Duration::from_millis(idle_ms));

        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _) = accepted.context("accept")?;
                active.fetch_add(1, Ordering::SeqCst);
                let root = root.clone();
                let active = active.clone();
                let went_idle = went_idle.clone();
                tokio::spawn(async move {
                    if let Err(e) = serve_connection(stream, root).await {
                        eprintln!("[selene daemon] session ended: {e:#}");
                    }
                    if active.fetch_sub(1, Ordering::SeqCst) == 1 {
                        went_idle.notify_one();
                    }
                });
            }
            // A session ended — re-enter the loop so the idle guard is re-evaluated.
            _ = went_idle.notified() => {}
            _ = idle_sleep, if idle_armed => {
                eprintln!("[selene daemon] idle for {idle_ms}ms with no clients; exiting");
                return Ok(());
            }
            _ = sigterm.recv() => { eprintln!("[selene daemon] SIGTERM; shutting down"); return Ok(()); }
            _ = sigint.recv()  => { eprintln!("[selene daemon] SIGINT; shutting down");  return Ok(()); }
        }
    }
}

/// Serve one accepted connection. The first line decides: a control frame (`selene sync` routed
/// here) is handled and the connection closes; anything else is the MCP `initialize` request, which
/// is replayed into a full MCP session over the warm store.
async fn serve_connection(stream: UnixStream, root: PathBuf) -> Result<()> {
    use tokio::io::{AsyncBufReadExt, BufReader};

    let (rh, wh) = stream.into_split();
    let mut reader = BufReader::new(rh);
    let mut first = String::new();
    reader.read_line(&mut first).await.context("read first line")?;

    if let Some(req) = super::control::parse_request(&first) {
        return handle_control(req, root, wh).await;
    }

    // MCP: replay the first line — plus any bytes BufReader read past it — ahead of the rest.
    use rmcp::ServiceExt;
    let buffered = reader.buffer().to_vec();
    let tail = reader.into_inner();
    let mut head = first.into_bytes();
    head.extend_from_slice(&buffered);
    let replay = Prefixed { head, pos: 0, tail };

    let service = crate::SeleneMcp::new(Some(root))
        .serve((replay, wh))
        .await
        .context("MCP handshake over socket")?;
    service.waiting().await.context("session")?;
    Ok(())
}

/// Handle a control request against the daemon's warm store, then reply one line and close.
async fn handle_control(
    req: super::control::ControlRequest,
    root: PathBuf,
    mut wh: tokio::net::unix::OwnedWriteHalf,
) -> Result<()> {
    use tokio::io::AsyncWriteExt;

    let reply = match req.selene_control.as_str() {
        "sync" => run_sync(&root).await,
        other => super::control::ControlReply::failure(format!("unknown control verb '{other}'")),
    };
    let mut line = serde_json::to_string(&reply).unwrap_or_default();
    line.push('\n');
    let _ = wh.write_all(line.as_bytes()).await;
    let _ = wh.flush().await;
    Ok(())
}

/// Run an incremental sync against the daemon's warm store — no second `open`, so no lock fight.
async fn run_sync(root: &std::path::Path) -> super::control::ControlReply {
    let store = match crate::handlers::warm_store_for_root(root).await {
        Ok(s) => s,
        Err(e) => return super::control::ControlReply::failure(e),
    };
    match selene_sync::sync_project_with_store(root, store).await {
        Ok(stats) => super::control::ControlReply {
            ok: true,
            changed: stats.changed,
            removed: stats.removed,
            unchanged: stats.unchanged,
            error: None,
        },
        Err(e) => super::control::ControlReply::failure(format!("{e:#}")),
    }
}

/// An `AsyncRead` that yields owned `head` bytes first, then delegates to `tail`. Lets the daemon
/// peek the first line and still hand a *complete* stream (initialize included) to rmcp.
struct Prefixed<R> {
    head: Vec<u8>,
    pos: usize,
    tail: R,
}

impl<R: tokio::io::AsyncRead + Unpin> tokio::io::AsyncRead for Prefixed<R> {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        if self.pos < self.head.len() {
            let remaining = &self.head[self.pos..];
            let n = remaining.len().min(buf.remaining());
            buf.put_slice(&remaining[..n]);
            self.pos += n;
            return std::task::Poll::Ready(Ok(()));
        }
        std::pin::Pin::new(&mut self.tail).poll_read(cx, buf)
    }
}

fn set_mode_0600(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}
