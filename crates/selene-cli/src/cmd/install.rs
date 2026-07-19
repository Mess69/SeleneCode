//! The installer surface (install/uninstall), plus version and self-upgrade.

use std::path::{Path, PathBuf};

use crate::exit::Outcome;

use super::lifecycle::init;

/// `selene install` — wire SeleneCode into one or more agents' MCP configs. `--target` accepts
/// `auto` (agents whose config exists), `all`, `none`, or a list of ids; empty defaults to `claude`.
/// The binary path written is `current_exe()`'s ABSOLUTE path (a static binary is not guaranteed on
/// PATH; a bad path fails silently — map Q8). Only an unknown `--target` or bad `--location` exit 1.
pub async fn install(targets: Vec<String>, location: String, print_config: bool) -> Outcome {
    use selene_installer::Ctx;
    let binary = match std::env::current_exe() {
        Ok(b) => b,
        Err(e) => {
            eprintln!("selene install: cannot find my own path: {e}");
            return Outcome::Failure;
        }
    };
    let ctx = match Ctx::from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("selene install: {e}");
            return Outcome::Failure;
        }
    };
    if print_config {
        println!("{}", selene_installer::print_config(&binary, &ctx));
        return Outcome::Ok;
    }

    // `install` IS the one-command onboarding: a project that has no index yet
    // gets `init` (index + git sync hooks) before the MCP config is written.
    // Without this, the first agent question hits "not indexed" guidance and
    // the user has to come back for a second command nobody told them about.
    if !Path::new(".selene").exists() {
        eprintln!("no index here yet — running `selene init` first…");
        if let Outcome::Failure = init(PathBuf::from("."), false, false).await {
            eprintln!("selene install: init failed — MCP config not written.");
            return Outcome::Failure;
        }
    }

    let (loc, ids) = match resolve_targets("install", &targets, &location, &ctx) {
        Ok(v) => v,
        Err(o) => return o,
    };
    if ids.is_empty() {
        eprintln!("selene install: no matching agents (try `--target all` or name one).");
        return Outcome::Ok; // an empty selection is a valid, success-shaped no-op
    }
    let results = selene_installer::install(&ids, loc, &binary, &ctx);
    report_targets("install", &results);
    eprintln!("Restart the agent (or reload its MCP servers) to pick up selene.");
    Outcome::Ok
}

/// `selene uninstall` — remove SeleneCode from agents' MCP configs. Empty `--target` defaults to
/// `all` (strip selene everywhere). Success-shaped even when nothing was configured.
pub async fn uninstall(targets: Vec<String>, location: String) -> Outcome {
    use selene_installer::Ctx;
    let ctx = match Ctx::from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("selene uninstall: {e}");
            return Outcome::Failure;
        }
    };
    let targets = if targets.is_empty() {
        vec!["all".to_string()]
    } else {
        targets
    };
    let (loc, ids) = match resolve_targets("uninstall", &targets, &location, &ctx) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let results = selene_installer::uninstall(&ids, loc, &ctx);
    report_targets("uninstall", &results);
    Outcome::Ok
}

/// Parse `--location` and resolve the `--target` flag to concrete ids. The ONLY two exit-1 cases in
/// the installer surface: an unknown target id and an invalid location.
pub(super) fn resolve_targets(
    cmd: &str,
    targets: &[String],
    location: &str,
    ctx: &selene_installer::Ctx,
) -> Result<(selene_installer::Location, Vec<String>), Outcome> {
    let loc = selene_installer::Location::parse(location).map_err(|e| {
        eprintln!("selene {cmd}: {e}");
        Outcome::Failure
    })?;
    // Empty → "claude"; a single special word (auto/all/none) passes through; else a CSV of ids.
    let flag = if targets.is_empty() {
        "claude".to_string()
    } else {
        targets.join(",")
    };
    let ids = selene_installer::resolve_target_flag(&flag, ctx, loc).map_err(|e| {
        eprintln!("selene {cmd}: {e}");
        Outcome::Failure
    })?;
    Ok((loc, ids))
}

/// Print one line per target result.
pub(super) fn report_targets(cmd: &str, results: &[selene_installer::TargetResult]) {
    for r in results {
        let where_ = r
            .path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let note = r
            .note
            .as_deref()
            .map(|n| format!(" — {n}"))
            .unwrap_or_default();
        eprintln!("  {:<11} {} {}{}", r.id, r.action.as_str(), where_, note);
    }
    if results.is_empty() {
        eprintln!("selene {cmd}: no targets selected.");
    }
}

/// `selene version` — the crate version. Exit 0.
pub fn version() -> Outcome {
    println!("selene {}", env!("CARGO_PKG_VERSION"));
    Outcome::Ok
}

/// `selene upgrade [version] [--check] [--force]` — self-update from GitHub
/// Releases (axoupdater, the `uv self update` engine).
///
/// Two install identities, detected structurally:
/// - **Installer/receipt install** (`curl … selene-installer.sh | sh`): the dist
///   receipt says where the binary lives and which release channel it came
///   from — upgrade replaces it in place, checksum-verified.
/// - **No receipt** (a source build, `cargo build`): upgrading in place would
///   overwrite a build product the user compiled themselves — refuse with the
///   exact commands instead. `--check` still works via the repo release feed.
pub async fn upgrade(version: Option<String>, check: bool, force: bool) -> Outcome {
    use axoupdater::{AxoUpdater, ReleaseSource, ReleaseSourceType, UpdateRequest, Version};

    let current = env!("CARGO_PKG_VERSION");
    let mut updater = AxoUpdater::new_for("selene");

    // The receipt is the source of truth for WHERE to upgrade. Without one,
    // fall back to the repo's release feed — enough for `--check`, and enough
    // to name the installer for everything else.
    let has_receipt = updater.load_receipt().is_ok();
    if !has_receipt {
        // `repository` from Cargo.toml, overridable for forks/mirrors.
        let repo = std::env::var("SELENE_GITHUB_REPO")
            .unwrap_or_else(|_| env!("CARGO_PKG_REPOSITORY").to_string());
        let (owner, name) = match repo
            .trim_start_matches("https://github.com/")
            .split_once('/')
        {
            Some((o, n)) => (o.to_string(), n.to_string()),
            None => {
                eprintln!("selene upgrade: cannot parse repository from `{repo}`");
                return Outcome::Failure;
            }
        };
        updater.set_release_source(ReleaseSource {
            release_type: ReleaseSourceType::GitHub,
            owner,
            name,
            app_name: "selene".to_string(),
        });
    }

    if let Some(v) = &version {
        let tag = if v.starts_with('v') {
            v.clone()
        } else {
            format!("v{v}")
        };
        updater.configure_version_specifier(UpdateRequest::SpecificTag(tag));
    }

    if check {
        // `query_new_version` needs only the release source — it works for
        // receipt installs AND source builds (is_update_needed does not: it
        // insists on a receipt's install_prefix).
        return match updater.query_new_version().await {
            Ok(Some(latest)) => {
                let newer = Version::parse(current)
                    .map(|cur| *latest > cur)
                    .unwrap_or(true);
                if newer {
                    println!("selene {current} → {latest} is available. Run `selene upgrade`.");
                } else {
                    println!("selene {current} is up to date (latest release: {latest}).");
                }
                Outcome::Ok
            }
            Ok(None) => {
                println!("selene {current}: no published release found.");
                Outcome::Ok
            }
            Err(e) => {
                eprintln!(
                    "selene upgrade --check: could not reach the release feed: {e}\n\
                     (no release published yet, offline, or the repository in Cargo.toml \
                     is not live — override with SELENE_GITHUB_REPO=owner/name)"
                );
                Outcome::Failure
            }
        };
    }

    if !has_receipt {
        eprintln!(
            "selene upgrade: this binary was built from source (no install receipt), so \
             upgrading in place would overwrite your own build.\n\
             - source build:  git pull && cargo build --release -p selene\n\
             - or switch to the managed install:  curl -fsSL \
             {}/releases/latest/download/selene-installer.sh | sh",
            env!("CARGO_PKG_REPOSITORY")
        );
        return Outcome::ExpectedNoOp;
    }

    if force {
        updater.always_update(true);
    }
    match updater.run().await {
        Ok(Some(result)) => {
            println!(
                "upgraded: selene {current} → {} (restart running agents to pick it up)",
                result.new_version
            );
            Outcome::Ok
        }
        Ok(None) => {
            println!("selene {current} is up to date.");
            Outcome::Ok
        }
        Err(e) => {
            eprintln!("selene upgrade: {e}");
            Outcome::Failure
        }
    }
}
