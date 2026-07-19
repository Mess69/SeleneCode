//! `selene viz` — the graph export, and the `--watch` live-map HTTP server.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use selene_db::SurrealStore;

use crate::exit::Outcome;

use super::{query_root, query_root_direct};

// ---- viz (self-contained interactive graph) ------------------------------------------------

/// `selene viz` — export the whole graph to ONE self-contained interactive HTML
/// page (a dark "galaxy"). Opens the store like every other query command, reads
/// all nodes + edges, caps/filters them (see [`crate::viz`]), and writes the file.
/// The written path goes to **stdout** (so a script can capture it); the summary
/// and any browser-open go to stderr.
pub async fn viz(
    path: Option<PathBuf>,
    out: Option<PathBuf>,
    max_nodes: usize,
    all_kinds: bool,
    open: bool,
    watch: bool,
) -> Outcome {
    if watch {
        // Watch mode WANTS a daemon (it reads through it, no lock fight) — so no
        // `query_root_direct` refusal here.
        let root = match query_root(path) {
            Ok(r) => r,
            Err(o) => return o,
        };
        return viz_watch(root, max_nodes, all_kinds, open).await;
    }
    let root = match query_root_direct(path) {
        Ok(r) => r,
        Err(o) => return o,
    };
    match viz_inner(&root, out, max_nodes, all_kinds).await {
        Ok((dest, doc)) => {
            eprintln!(
                "selene viz: {} of {} nodes, {} of {} edges",
                doc.shown_nodes, doc.total_nodes, doc.shown_edges, doc.total_edges
            );
            println!("{}", dest.display());
            if open {
                open_in_browser(&dest);
            }
            Outcome::Ok
        }
        Err(e) => {
            eprintln!("selene viz: {e:#}");
            Outcome::Failure
        }
    }
}

async fn viz_inner(
    root: &Path,
    out: Option<PathBuf>,
    max_nodes: usize,
    all_kinds: bool,
) -> Result<(PathBuf, crate::viz::VizDoc)> {
    let store = SurrealStore::open(&root.join(".selene"))
        .await
        .context("could not open the index")?;
    let nodes = store.all_nodes().await.context("read nodes")?;
    let edges = store.all_edges().await.context("read edges")?;

    let opts = crate::viz::VizOptions {
        max_nodes,
        all_kinds,
        root_label: root.display().to_string(),
        watch: false,
    };
    let doc = crate::viz::build_html(&nodes, &edges, &opts);

    let dest = out.unwrap_or_else(|| root.join("selene-graph.html"));
    std::fs::write(&dest, &doc.html)
        .with_context(|| format!("could not write {}", dest.display()))?;
    Ok((dest, doc))
}

// ---- viz --watch: the live map --------------------------------------------------------------

/// Read the whole graph, through whichever door is open: a running daemon (it
/// owns the RocksDB lock — ask it over the control socket) or, with no daemon,
/// a brief direct open that is dropped immediately so a daemon can still start.
async fn fetch_graph(
    root: &Path,
) -> Result<(Vec<selene_core::Node>, Vec<selene_core::Edge>, bool)> {
    if let Ok(Some(reply)) = selene_mcp::daemon::route_to_daemon(root, "graph").await {
        if !reply.ok {
            // A live daemon that errored: do NOT fall through to a direct open —
            // that is a guaranteed lock fight. Surface and let the caller retry.
            anyhow::bail!(
                "daemon graph dump failed: {}",
                reply.error.unwrap_or_else(|| "unknown error".into())
            );
        }
        let mut data = reply.data.context("daemon graph reply carried no data")?;
        let nodes: Vec<selene_core::Node> = serde_json::from_value(
            data.get_mut("nodes")
                .map(serde_json::Value::take)
                .context("daemon graph reply missing nodes")?,
        )
        .context("daemon graph nodes unparseable")?;
        let edges: Vec<selene_core::Edge> = serde_json::from_value(
            data.get_mut("edges")
                .map(serde_json::Value::take)
                .context("daemon graph reply missing edges")?,
        )
        .context("daemon graph edges unparseable")?;
        return Ok((nodes, edges, true));
    }
    let store = SurrealStore::open(&root.join(".selene"))
        .await
        .context("could not open the index")?;
    let nodes = store.all_nodes().await.context("read nodes")?;
    let edges = store.all_edges().await.context("read edges")?;
    drop(store); // release the lock NOW — an agent's daemon may want to start
    Ok((nodes, edges, false))
}

/// A cheap freshness fingerprint of the index: (name, len, mtime) of the
/// RocksDB data files under `.selene/` (WAL/SSTs are `<digits>.log`/`.sst`,
/// plus MANIFEST/CURRENT). Info logs (`LOG*`, `daemon.log`) are excluded — they
/// churn without the graph changing.
fn index_fingerprint(selene_dir: &Path) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut h = DefaultHasher::new();
    let Ok(entries) = std::fs::read_dir(selene_dir) else {
        return 0;
    };
    let mut rows: Vec<(String, u64, u128)> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_data = name.starts_with(|c: char| c.is_ascii_digit())
            || name.starts_with("MANIFEST")
            || name == "CURRENT";
        if !is_data {
            continue;
        }
        let Ok(md) = entry.metadata() else { continue };
        let mtime = md
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        rows.push((name, md.len(), mtime));
    }
    rows.sort();
    rows.hash(&mut h);
    h.finish()
}

/// Total bytes on disk under `dir`, recursively. Cheap for `.selene/` (a flat
/// RocksDB dir plus a handful of daemon files).
fn dir_size(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .map(|e| match e.metadata() {
            Ok(md) if md.is_dir() => dir_size(&e.path()),
            Ok(md) => md.len(),
            Err(_) => 0,
        })
        .sum()
}

/// Current RSS of a process in bytes, via `ps` (KiB on both macOS and Linux).
fn rss_of(pid: i32) -> Option<u64> {
    let out = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse::<u64>()
        .ok()
        .map(|kib| kib * 1024)
}

/// The live memory line the watch page shows: the index's on-disk footprint,
/// and the RAM of whoever is actually holding the graph — the daemon when one
/// is up (the number that matters while an agent works), else this server.
fn memory_probe(root: &Path) -> serde_json::Value {
    let index_bytes = dir_size(&root.join(".selene"));
    if let Some(rss) = selene_mcp::daemon::running_pid(root).and_then(rss_of) {
        return serde_json::json!({ "index": index_bytes, "rss": rss, "src": "daemon" });
    }
    let own = rss_of(std::process::id() as i32).unwrap_or(0);
    serde_json::json!({ "index": index_bytes, "rss": own, "src": "server" })
}

/// `selene viz --watch` — serve the live map over local HTTP. The page polls
/// `/data`; every index change (agent edits, syncs, commits) re-reads the graph
/// and bumps the generation, and the page animates the difference in place.
async fn viz_watch(root: PathBuf, max_nodes: usize, all_kinds: bool, open: bool) -> Outcome {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    let opts = crate::viz::VizOptions {
        max_nodes,
        all_kinds,
        root_label: root.display().to_string(),
        watch: true,
    };

    let (nodes, edges, via_daemon) = match fetch_graph(&root).await {
        Ok(g) => g,
        Err(e) => {
            eprintln!("selene viz --watch: {e:#}");
            return Outcome::Failure;
        }
    };
    let mut data = crate::viz::build_data(&nodes, &edges, &opts);
    // The compare/serve line stays mem-free (memory is spliced in fresh at
    // serve time — inside `last_content` it would fake graph changes); the
    // initial page embed gets it so the first paint shows memory immediately.
    let data_line = serde_json::to_string(&data.json).unwrap_or_default();
    data.json["mem"] = memory_probe(&root);
    let html = crate::viz::render(&data.json, &opts.root_label);

    let listener = match tokio::net::TcpListener::bind(("127.0.0.1", 0)).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("selene viz --watch: could not bind a local port: {e}");
            return Outcome::Failure;
        }
    };
    let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
    let url = format!("http://127.0.0.1:{port}");

    let generation = Arc::new(AtomicU64::new(0));
    let latest = Arc::new(tokio::sync::RwLock::new(data_line));
    let html = Arc::new(html);

    eprintln!(
        "selene viz --watch: {} nodes, {} edges · source: {}",
        data.shown_nodes,
        data.shown_edges,
        if via_daemon {
            "daemon (live index)"
        } else {
            "direct reads (no daemon)"
        }
    );
    eprintln!("selene viz --watch: serving {url}  — Ctrl-C to stop");
    println!("{url}");
    if open {
        open_url_in_browser(&url);
    }

    // The HTTP loop: tiny, GET-only, connection-per-request.
    {
        let generation = generation.clone();
        let latest = latest.clone();
        let html = html.clone();
        let probe_root = root.clone();
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    continue;
                };
                let (generation, latest, html) = (generation.clone(), latest.clone(), html.clone());
                let probe_root = probe_root.clone();
                tokio::spawn(async move {
                    let _ = serve_watch_conn(stream, generation, latest, html, probe_root).await;
                });
            }
        });
    }

    // The refresh loop: fingerprint the index every 1.2s; on change, wait for
    // one quiet tick (writes settled), re-read, re-transform, bump the gen.
    let selene_dir = root.join(".selene");
    let mut last_fp = index_fingerprint(&selene_dir);
    let mut dirty = false;
    // The gen-0 serialization of what we're serving — the content-change probe.
    // (`latest` itself carries a bumped `gen`, so it can't be the comparand.)
    let mut last_content = latest.read().await.clone();
    loop {
        tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
        let fp = index_fingerprint(&selene_dir);
        if fp != last_fp {
            last_fp = fp;
            dirty = true;
            continue; // still moving — let the writes settle one tick
        }
        if !dirty {
            continue;
        }
        match fetch_graph(&root).await {
            Ok((nodes, edges, _)) => {
                // Our own direct open just churned the RocksDB files (fresh WAL,
                // MANIFEST bump) — rebase the fingerprint on the post-read state
                // or we would chase our own tail forever.
                last_fp = index_fingerprint(&selene_dir);
                dirty = false;

                let mut data = crate::viz::build_data(&nodes, &edges, &opts);
                let line_probe = serde_json::to_string(&data.json).unwrap_or_default();
                // Only bump the generation when the GRAPH changed — file churn
                // with identical content (compactions, no-op syncs) stays silent.
                if line_probe == last_content {
                    continue;
                }
                last_content = line_probe;
                let g = generation.fetch_add(1, Ordering::SeqCst) + 1;
                data.json["gen"] = serde_json::json!(g);
                *latest.write().await = serde_json::to_string(&data.json).unwrap_or_default();
                eprintln!(
                    "selene viz --watch: ↻ update {g}: {} nodes, {} edges",
                    data.shown_nodes, data.shown_edges
                );
            }
            Err(e) => {
                // Index mid-write or daemon busy — keep serving the old data, retry.
                eprintln!("selene viz --watch: refresh failed ({e:#}); retrying");
            }
        }
    }
}

/// Answer one HTTP connection: `/` → the page, `/data?known=N` → the latest
/// data (or a tiny `{"gen":N}` when the client is already current).
async fn serve_watch_conn(
    mut stream: tokio::net::TcpStream,
    generation: std::sync::Arc<std::sync::atomic::AtomicU64>,
    latest: std::sync::Arc<tokio::sync::RwLock<String>>,
    html: std::sync::Arc<String>,
    probe_root: PathBuf,
) -> Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let (rh, mut wh) = stream.split();
    let mut reader = BufReader::new(rh);
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    // Drain the headers so the client's socket isn't reset mid-request.
    let mut hdr = String::new();
    while reader.read_line(&mut hdr).await? > 2 {
        hdr.clear();
    }

    let path = line.split_whitespace().nth(1).unwrap_or("/");
    let (status, ctype, body): (&str, &str, String) = if path == "/" {
        ("200 OK", "text/html; charset=utf-8", html.as_ref().clone())
    } else if let Some(q) = path.strip_prefix("/data") {
        let cur = generation.load(std::sync::atomic::Ordering::SeqCst);
        let known: Option<u64> = q.strip_prefix("?known=").and_then(|v| v.parse().ok());
        // Memory is spliced in FRESH on every poll — the stored data line only
        // changes when the graph does, but RAM/disk move continuously.
        let mem = memory_probe(&probe_root);
        if known == Some(cur) {
            (
                "200 OK",
                "application/json",
                format!(r#"{{"gen":{cur},"mem":{mem}}}"#),
            )
        } else {
            let stored = latest.read().await.clone();
            // stored is a JSON object ("{...}") — prepend mem inside it.
            (
                "200 OK",
                "application/json",
                format!("{{\"mem\":{mem},{}", &stored[1..]),
            )
        }
    } else {
        ("404 Not Found", "text/plain", "not found".to_string())
    };

    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    wh.write_all(head.as_bytes()).await?;
    wh.write_all(body.as_bytes()).await?;
    wh.flush().await?;
    Ok(())
}

/// Best-effort: open a URL in the OS default browser (same contract as
/// [`open_in_browser`], different argument shape).
fn open_url_in_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let program = "open";
    #[cfg(target_os = "windows")]
    let program = "explorer";
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    let program = "xdg-open";

    if let Err(e) = std::process::Command::new(program).arg(url).spawn() {
        eprintln!("selene viz --watch: could not open a browser ({e}); open {url} yourself");
    }
}

/// Best-effort: open `path` in the OS default browser. Never fails the command —
/// the file is already written and its path was printed.
fn open_in_browser(path: &Path) {
    #[cfg(target_os = "macos")]
    let program = "open";
    #[cfg(target_os = "windows")]
    let program = "explorer";
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    let program = "xdg-open";

    if let Err(e) = std::process::Command::new(program).arg(path).spawn() {
        eprintln!(
            "selene viz: could not open a browser ({e}); open {} yourself",
            path.display()
        );
    }
}
