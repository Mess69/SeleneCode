//! Scan pipeline (Phase 2 Tasks 16–17): which files are in scope, and how
//! they are enumerated. Ported from CodeGraph's `src/extraction/index.ts`
//! scan block (extraction-core map §1).
//!
//! [`ignore`] (Task 16) carries the scope-ignore semantics — the built-in
//! default ignore dirs, the defensive `.gitignore` reader, and
//! [`ScopeIgnore`](crate::ScopeIgnore), the single source of truth for
//! indexer and watcher scope. This module (Task 17) is the enumerator:
//!
//! - **Git fast path**: `git rev-parse --show-toplevel`; when the scan root
//!   is not the toplevel AND `git check-ignore -q <root>` says the root is
//!   itself gitignored, git would list nothing — fall back to the FS walk.
//!   Otherwise `git ls-files -z -s --recurse-submodules` (tracked; `-z`
//!   NUL-delimited so non-ASCII paths survive verbatim, #541; `-s` so
//!   mode-160000 gitlink entries are visible) plus
//!   `git ls-files -z -o --exclude-standard` (untracked). Embedded repos —
//!   nested non-submodule clones git refuses to descend into (#193) — are
//!   recursed as their own repos: the untracked kind (an opaque trailing
//!   `dir/` entry whose `.git` is a real directory), the tracked-gitlink
//!   kind with a real checkout on disk (#1031/#1033), and — **opt-in
//!   only** — the gitignored kind (#514) via
//!   [`ScanOverrides::include_ignored`] (#622/#699; by default `.gitignore`
//!   is respected, #970/#976). A `.git` **file** whose `gitdir:` pointer
//!   matches the worktree shape is a duplicate working view and is skipped
//!   (#848/#945). The `./` whole-cwd sentinel from `--directory` listings
//!   is dropped (#936).
//! - **Bounded git**: every invocation runs through [`run_git`] — a
//!   deadline (the `wait-timeout` crate; `std::process` has none) of 5 s
//!   for `rev-parse`/`check-ignore` and 30 s for `ls-files` per map §1 (the
//!   TS 10 s tier belongs to `git status` in the sync path, not ported
//!   here), plus a 50 MB output cap. On timeout/failure the scan falls back
//!   to the FS walk — a hung or hostile git never errors (or hangs) the
//!   scan.
//! - **FS fallback**: a recursive walk layering per-directory scoped
//!   `.gitignore` matchers (patterns in a nested `.gitignore` are relative
//!   to its directory, exactly as git applies them), with a symlink-cycle
//!   guard via a `fs::canonicalize` visited set.
//! - **Determinism**: results are root-relative, forward-slash paths,
//!   collected into ordered sets — every public function returns sorted
//!   output.
//!
//! `include`/`exclude` overrides are threaded into [`ScopeIgnore`]
//! (`exclude` drops even tracked files, #999). Note the TS force-include
//! *collection* (discovering gitignored `include`-matched files off disk,
//! which `git ls-files` never lists) is config-driven and lands with
//! Phase 8's config loading; until then `include` only affects paths the
//! enumerator already visits.

pub(crate) mod ignore;

use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::LazyLock;
use std::time::Duration;

use ::ignore::gitignore::Gitignore;
use regex::Regex;

use crate::language::is_source_file;
use ignore::{
    ScopeIgnore, ScopeOverrides, build_default_ignore, defaults_only_ignore, matcher_from_lines,
    matches_rel, read_gitignore_lines,
};

/// Deadline for the cheap git probes (`rev-parse`, `check-ignore`).
const GIT_TIMEOUT_SHORT: Duration = Duration::from_secs(5);
/// Deadline for the listing commands (`ls-files` variants).
const GIT_TIMEOUT_LS_FILES: Duration = Duration::from_secs(30);
/// Output cap per git invocation — past this the scan falls back rather
/// than buffering unbounded output.
const MAX_GIT_OUTPUT: usize = 50 * 1024 * 1024;
/// Max directory depth searched below an ignored/untracked dir for nested
/// `.git` roots.
const EMBEDDED_REPO_SEARCH_DEPTH: usize = 4;
/// Max directories examined per nested-repo search — a huge ignored data
/// dir must never stall a scan.
const EMBEDDED_REPO_SEARCH_ENTRIES: usize = 2000;
/// Cap on how many skipped gitignored repos [`find_unindexed_ignored_repos`]
/// enumerates — enough to make the point; callers say "+N more" past it.
const UNINDEXED_IGNORED_REPO_HINT_CAP: usize = 100;

/// A git worktree's `.git`-file `gitdir:` pointer lives under some repo's
/// `.git/worktrees/<name>` — either the top-level repo's, or (for a
/// worktree of a submodule, #945) that submodule's gitdir
/// (`.git/modules/<module>/worktrees/`). Both separators are matched so a
/// Windows-style pointer is recognized too. Ported verbatim from
/// `classifyGitDir` in CodeGraph's index.ts.
static WORKTREE_GITDIR_RE: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)] // literal pattern, compile-time known good
    Regex::new(r"(^|[\\/])\.git[\\/](modules[\\/][^\\/]+[\\/])?worktrees[\\/]").unwrap()
});

/// Scan-scope overrides — gitignore-style pattern lists matched against
/// root-relative paths. Config-file loading is Phase 8; until then callers
/// pass these directly (default: empty, the zero-config behavior).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScanOverrides {
    /// First-party source forced INTO the index despite `.gitignore` — see
    /// [`crate::ScopeOverrides::include`].
    pub include: Vec<String>,
    /// Paths kept OUT of the index even when git-tracked (#999) — see
    /// [`crate::ScopeOverrides::exclude`].
    pub exclude: Vec<String>,
    /// Directories whose **gitignored embedded git repos** are opted into
    /// the index (#622/#699). By default `.gitignore` is fully respected and
    /// a gitignored directory — even one holding nested repos — is never
    /// walked or indexed (#970/#976).
    pub include_ignored: Vec<String>,
}

impl ScanOverrides {
    fn scope(&self) -> ScopeOverrides {
        ScopeOverrides {
            include: self.include.clone(),
            exclude: self.exclude.clone(),
        }
    }

    /// The `include_ignored` matcher, or `None` when nothing was opted in
    /// (the zero-config default).
    fn include_ignored_matcher(&self) -> Option<Gitignore> {
        let usable: Vec<&str> = self
            .include_ignored
            .iter()
            .map(|p| p.trim())
            .filter(|p| !p.is_empty())
            .collect();
        if usable.is_empty() {
            None
        } else {
            Some(matcher_from_lines(usable))
        }
    }
}

/// Recursively enumerate the in-scope **source files** under `root`:
/// sorted, root-relative, forward-slash paths.
///
/// Git fast path when `root` is inside a work tree (and not itself
/// gitignored by an enclosing repo); FS walk otherwise. Both paths apply
/// the same [`ScopeIgnore`] semantics and the [`is_source_file`]
/// indexability predicate. Git trouble never errors (it falls back); `Err`
/// only when `root` itself is not a readable directory.
pub fn scan_directory(root: &Path, overrides: &ScanOverrides) -> std::io::Result<Vec<String>> {
    let meta = std::fs::metadata(root)?;
    if !meta.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("scan root is not a directory: {}", root.display()),
        ));
    }

    if let Some((files, embedded_roots)) = git_visible_files(root, overrides) {
        let embedded: Vec<String> = embedded_roots.into_iter().collect();
        let scope = ScopeIgnore::build(root, &embedded, &overrides.scope());
        return Ok(files
            .into_iter()
            .filter(|f| !scope.ignores(f) && is_source_file(f))
            .collect());
    }

    Ok(scan_directory_walk(root, overrides))
}

/// Standalone discovery of every embedded repo root under `root` (sorted,
/// root-relative, trailing-slashed) — the untracked kind (#193) and tracked
/// gitlinks with a real checkout (#1031/#1033). The gitignored kind is
/// opt-in via config (Phase 8) and not discovered here — `.gitignore` is
/// respected (#970/#976). Returns `[]` for non-git roots: the filesystem
/// walk handles nested repos there already.
pub fn discover_embedded_repo_roots(root: &Path) -> Vec<String> {
    if run_git(root, &["rev-parse", "--git-dir"], GIT_TIMEOUT_SHORT)
        .filter(|out| out.success)
        .is_none()
    {
        return Vec::new();
    }
    let mut out: Vec<String> = Vec::new();
    discover_embedded_repos_visit(root, "", None, &mut out);
    out.sort();
    out
}

/// One repo level of [`discover_embedded_repo_roots`]: candidates from the
/// untracked `--directory` listing (nested `.git` roots under each
/// collapsed untracked dir), from unexpanded mode-160000 gitlinks with a
/// real checkout, and — when `include_ignored` opted directories in — from
/// the repo's own gitignored dirs; then recurse into each candidate.
fn discover_embedded_repos_visit(
    repo_abs: &Path,
    prefix: &str,
    include_ignored: Option<&Gitignore>,
    out: &mut Vec<String>,
) {
    let defaults = defaults_only_ignore();
    let mut candidates: Vec<String> = Vec::new();

    if let Some(listing) = run_git(
        repo_abs,
        &["ls-files", "-z", "-o", "--exclude-standard", "--directory"],
        GIT_TIMEOUT_LS_FILES,
    )
    .filter(|o| o.success)
    {
        for entry in nul_entries(&listing.stdout) {
            if entry.ends_with('/') && !is_whole_cwd_entry(entry) && !matches_rel(&defaults, entry)
            {
                candidates.extend(find_nested_git_repos(&repo_abs.join(entry), entry));
            }
        }
    }

    // Unexpanded gitlinks (mode 160000) with a real checkout on disk — the
    // untracked listing can't see them (they're tracked). (#1031, #1033)
    if let Some(staged) = run_git(
        repo_abs,
        &["ls-files", "-z", "-s", "--recurse-submodules"],
        GIT_TIMEOUT_LS_FILES,
    )
    .filter(|o| o.success)
    {
        let repo_ignore = build_default_ignore(repo_abs);
        for entry in nul_entries(&staged.stdout) {
            let Some((rel, is_gitlink)) = parse_stage_entry(entry) else {
                continue;
            };
            if !is_gitlink {
                continue;
            }
            let rel_dir = ensure_trailing_slash(rel);
            if gitlink_embedded_repo_skipped(
                &rel_dir,
                prefix,
                &defaults,
                &repo_ignore,
                include_ignored,
            ) {
                continue;
            }
            if classify_git_dir(&repo_abs.join(rel)) == GitDirClass::Embedded {
                candidates.push(rel_dir);
            }
        }
    }

    candidates.extend(find_ignored_embedded_repos(
        repo_abs,
        include_ignored,
        prefix,
    ));

    for rel in candidates {
        let full = format!("{prefix}{rel}");
        out.push(full.clone());
        discover_embedded_repos_visit(&repo_abs.join(&rel), &full, include_ignored, out);
    }
}

/// Nested git repositories under a gitignored directory that were **not**
/// opted in — the repos a default scan deliberately skips because
/// `.gitignore` excludes them (#970/#976). CLI surfaces use this to turn a
/// silently-near-empty index into an actionable hint (#1156). Sorted,
/// root-relative, trailing-slashed; discovery is capped at
/// [`UNINDEXED_IGNORED_REPO_HINT_CAP`] entries (applied in discovery order,
/// before the sort). `[]` for non-git roots.
pub fn find_unindexed_ignored_repos(root: &Path) -> Vec<String> {
    if run_git(root, &["rev-parse", "--git-dir"], GIT_TIMEOUT_SHORT)
        .filter(|out| out.success)
        .is_none()
    {
        return Vec::new();
    }
    let defaults = defaults_only_ignore();
    let mut repos: Vec<String> = Vec::new();
    'outer: for dir in list_ignored_dirs(root) {
        if matches_rel(&defaults, &dir) {
            continue; // node_modules etc. — never project code
        }
        for repo in find_nested_git_repos(&root.join(&dir), &dir) {
            repos.push(repo);
            if repos.len() >= UNINDEXED_IGNORED_REPO_HINT_CAP {
                break 'outer;
            }
        }
    }
    repos.sort();
    repos
}

// =============================================================================
// Git plumbing
// =============================================================================

/// One bounded git invocation's outcome.
struct GitOutput {
    success: bool,
    stdout: Vec<u8>,
}

/// Run `git <args>` in `dir` with a hard deadline and a [`MAX_GIT_OUTPUT`]
/// cap. `None` on spawn failure, timeout (the child is killed), an
/// unjoinable reader, or a blown output cap — callers treat all of those as
/// "git unavailable" and fall back. A non-zero exit is NOT `None`: it comes
/// back as `success: false` (`check-ignore` callers need the exit-code
/// distinction).
///
/// The child's stdout is drained on a dedicated thread — a pipe left unread
/// past the OS buffer would deadlock the child and turn every big listing
/// into a timeout kill. Past the cap the drain keeps consuming (so the
/// child can exit promptly) but stops storing, and the overflow surfaces as
/// `None`.
fn run_git(dir: &Path, args: &[&str], timeout: Duration) -> Option<GitOutput> {
    let mut child = Command::new("git")
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut buf: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 64 * 1024];
        let mut overflowed = false;
        loop {
            match stdout.read(&mut chunk) {
                Ok(0) => return (buf, overflowed),
                Ok(n) => {
                    if overflowed || buf.len() + n > MAX_GIT_OUTPUT {
                        overflowed = true; // keep draining so the child can exit
                    } else {
                        buf.extend_from_slice(&chunk[..n]);
                    }
                }
                Err(_) => return (buf, true),
            }
        }
    });

    use wait_timeout::ChildExt;
    let status = match child.wait_timeout(timeout) {
        Ok(Some(status)) => status,
        Ok(None) | Err(_) => {
            // Deadline passed (or the wait itself failed) — kill the child,
            // which also closes the pipe and ends the reader.
            let _ = child.kill();
            let _ = child.wait();
            let _ = reader.join();
            return None;
        }
    };
    let (stdout, overflowed) = reader.join().ok()?;
    if overflowed {
        return None;
    }
    Some(GitOutput {
        success: status.success(),
        stdout,
    })
}

/// NUL-delimited entries of a `git ls-files -z` output, as UTF-8 strings
/// (non-UTF-8 paths are skipped — they cannot round-trip through the
/// `String`-typed scan surface).
fn nul_entries(bytes: &[u8]) -> impl Iterator<Item = &str> {
    bytes
        .split(|b| *b == 0)
        .filter_map(|chunk| std::str::from_utf8(chunk).ok())
        .filter(|s| !s.is_empty())
}

/// Parse one `ls-files -s` entry (`"<mode> <object> <stage>\t<path>"`) into
/// `(path, is_gitlink)`. `None` for malformed entries.
fn parse_stage_entry(entry: &str) -> Option<(&str, bool)> {
    let (meta, rel) = entry.split_once('\t')?;
    Some((rel, meta.starts_with("160000")))
}

/// The `./` whole-cwd sentinel `git ls-files --directory` emits when the
/// command's own cwd is a wholly-ignored directory (#936) — not a real
/// nested path; dropped wherever `--directory` output is consumed.
fn is_whole_cwd_entry(entry: &str) -> bool {
    entry == "./" || entry == "." || entry.is_empty()
}

fn ensure_trailing_slash(rel: &str) -> String {
    if rel.ends_with('/') {
        rel.to_string()
    } else {
        format!("{rel}/")
    }
}

/// How a directory's `.git` entry classifies for embedded-repo discovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GitDirClass {
    /// A `.git` **directory** (an embedded clone — index it, #193/#514), or
    /// a `.git` file pointing at a plain submodule gitdir
    /// (`.git/modules/<m>`, no `worktrees/` segment — distinct code).
    Embedded,
    /// A `.git` **file** whose `gitdir:` points into some repo's
    /// `worktrees/` — a duplicate working view of an already-indexed repo;
    /// skip (#848, #945).
    Worktree,
    /// No `.git` entry here.
    None,
}

/// Classify `abs_dir`'s `.git` entry. An unreadable `.git` pointer file
/// falls back to `Embedded` (the prior "index it" behavior).
fn classify_git_dir(abs_dir: &Path) -> GitDirClass {
    let git_path = abs_dir.join(".git");
    let Ok(meta) = std::fs::metadata(&git_path) else {
        return GitDirClass::None;
    };
    if meta.is_dir() {
        return GitDirClass::Embedded;
    }
    if !meta.is_file() {
        return GitDirClass::None;
    }
    if let Ok(content) = std::fs::read_to_string(&git_path) {
        let gitdir = content
            .lines()
            .find_map(|line| line.strip_prefix("gitdir:"))
            .map(str::trim);
        if let Some(gitdir) = gitdir
            && is_worktree_gitdir_pointer(gitdir)
        {
            return GitDirClass::Worktree;
        }
    }
    GitDirClass::Embedded
}

/// Whether a `.git`-file `gitdir:` pointer names a worktree (see
/// [`WORKTREE_GITDIR_RE`]).
fn is_worktree_gitdir_pointer(gitdir: &str) -> bool {
    WORKTREE_GITDIR_RE.is_match(gitdir)
}

/// Find git repositories nested under `abs_dir` (inclusive), shallow
/// bounded BFS (depth [`EMBEDDED_REPO_SEARCH_DEPTH`], entries
/// [`EMBEDDED_REPO_SEARCH_ENTRIES`]). Stops descending at each repo root
/// found — its contents belong to that repo's own enumeration. Skips
/// default-ignored dirs (`node_modules` can contain `.git` from npm
/// git-dependencies) and the `.selene` data dir; worktrees are skipped
/// (#848). Returned rels are `rel_prefix`-prefixed and trailing-slashed.
/// Children are visited in name order so the entry cap cuts
/// deterministically.
fn find_nested_git_repos(abs_dir: &Path, rel_prefix: &str) -> Vec<String> {
    let defaults = defaults_only_ignore();
    let mut found: Vec<String> = Vec::new();
    let mut queue: std::collections::VecDeque<(PathBuf, String, usize)> =
        std::collections::VecDeque::from([(abs_dir.to_path_buf(), rel_prefix.to_string(), 0)]);
    let mut examined = 0usize;
    while let Some((abs, rel, depth)) = queue.pop_front() {
        examined += 1;
        if examined > EMBEDDED_REPO_SEARCH_ENTRIES {
            break; // deeper repos (if any) not discovered — bounded by design
        }
        match classify_git_dir(&abs) {
            GitDirClass::Worktree => continue,
            GitDirClass::Embedded => {
                found.push(rel);
                continue; // its own git handles everything below
            }
            GitDirClass::None => {}
        }
        if depth >= EMBEDDED_REPO_SEARCH_DEPTH {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&abs) else {
            continue;
        };
        let mut children: Vec<String> = entries
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|name| name != ".git" && !is_data_dir(name))
            .collect();
        children.sort();
        for name in children {
            let child_rel = format!("{rel}{name}/");
            if matches_rel(&defaults, &child_rel) {
                continue;
            }
            queue.push_back((abs.join(&name), child_rel, depth + 1));
        }
    }
    found
}

/// SeleneCode's per-project data directory — never scanned, never treated
/// as an embedded-repo location.
fn is_data_dir(name: &str) -> bool {
    name == ".selene"
}

/// The gitignored DIRECTORIES of a repo (collapsed, trailing-slash form),
/// relative to `repo_dir` — invisible to every other `ls-files` mode, and
/// exactly where nested project repos live in a multi-repo workspace
/// (#514).
fn list_ignored_dirs(repo_dir: &Path) -> Vec<String> {
    let Some(out) = run_git(
        repo_dir,
        &[
            "ls-files",
            "-z",
            "-o",
            "-i",
            "--exclude-standard",
            "--directory",
        ],
        GIT_TIMEOUT_LS_FILES,
    )
    .filter(|o| o.success) else {
        return Vec::new();
    };
    nul_entries(&out.stdout)
        .filter(|e| e.ends_with('/') && !is_whole_cwd_entry(e))
        .map(str::to_string)
        .collect()
}

/// Whether an embedded repo found as a **tracked gitlink** must be skipped
/// (#1065): (1) it sits in a built-in default-ignored location (not even an
/// explicit opt-in revives it), or (2) the parent repo's own `.gitignore`
/// covers it and the project did NOT opt that path in via
/// `include_ignored` — the same scope rule as the untracked-ignored kind
/// (#514, #970, #976). `prefix` is the repo's scan-root-relative path so
/// the opt-in matches on the full path.
fn gitlink_embedded_repo_skipped(
    rel_dir: &str,
    prefix: &str,
    defaults: &Gitignore,
    repo_ignore: &Gitignore,
    include_ignored: Option<&Gitignore>,
) -> bool {
    if matches_rel(defaults, rel_dir) {
        return true;
    }
    if !matches_rel(repo_ignore, rel_dir) {
        return false; // not ignored at all — index as before (#1031/#1033)
    }
    // Gitignored by the repo's own rules — skip unless opted in.
    !include_ignored.is_some_and(|m| matches_rel(m, &format!("{prefix}{rel_dir}")))
}

/// Embedded repos hidden by `repo_dir`'s OWN gitignore rules — **opt-in
/// only** (#622/#699): without `include_ignored`, `.gitignore` is respected
/// and this returns `[]` (#970/#976). Built-in default excludes are always
/// skipped. Returned rels are repo-relative, trailing-slashed.
fn find_ignored_embedded_repos(
    repo_dir: &Path,
    include_ignored: Option<&Gitignore>,
    prefix: &str,
) -> Vec<String> {
    let Some(matcher) = include_ignored else {
        return Vec::new();
    };
    let defaults = defaults_only_ignore();
    let mut repos: Vec<String> = Vec::new();
    for dir in list_ignored_dirs(repo_dir) {
        if matches_rel(&defaults, &dir) {
            continue;
        }
        if !matches_rel(matcher, &format!("{prefix}{dir}")) {
            continue;
        }
        repos.extend(find_nested_git_repos(&repo_dir.join(&dir), &dir));
    }
    repos
}

/// Collect git-visible files (tracked + untracked, `.gitignore`-respected)
/// from the repo at `repo_dir` into `files`, `prefix`-prepended so paths
/// stay relative to the original scan root; embedded repo roots (however
/// found) are recorded in `embedded_roots` and recursed into. `None` when a
/// listing failed (caller falls back to the walk).
fn collect_git_files(
    repo_dir: &Path,
    prefix: &str,
    files: &mut BTreeSet<String>,
    embedded_roots: &mut BTreeSet<String>,
    include_ignored: Option<&Gitignore>,
) -> Option<()> {
    // Tracked files; `--recurse-submodules` expands ACTIVE submodules
    // (#147), and `-s` exposes the mode-160000 gitlinks it did NOT expand
    // (a nested repo `git add`ed without `.gitmodules`, or an inactive
    // submodule) — collected and recursed below (#1031/#1033).
    let tracked = run_git(
        repo_dir,
        &["ls-files", "-z", "-s", "--recurse-submodules"],
        GIT_TIMEOUT_LS_FILES,
    )
    .filter(|o| o.success)?;
    let mut gitlink_rels: Vec<String> = Vec::new();
    for entry in nul_entries(&tracked.stdout) {
        let Some((rel, is_gitlink)) = parse_stage_entry(entry) else {
            continue;
        };
        if is_gitlink {
            gitlink_rels.push(rel.to_string());
            continue;
        }
        files.insert(format!("{prefix}{rel}"));
    }

    // Untracked files. An embedded repo surfaces as a single trailing-slash
    // "subdir/" entry git refuses to descend into (#193); a worktree
    // surfaces the same way and is skipped as a duplicate view (#848).
    // Never descend into default-ignored locations — an embedded repo
    // inside node_modules is an npm git-dependency, not project code.
    let untracked = run_git(
        repo_dir,
        &["ls-files", "-z", "-o", "--exclude-standard"],
        GIT_TIMEOUT_LS_FILES,
    )
    .filter(|o| o.success)?;
    let defaults = defaults_only_ignore();
    for rel in nul_entries(&untracked.stdout) {
        if rel.ends_with('/') {
            let child = repo_dir.join(rel);
            if classify_git_dir(&child) == GitDirClass::Embedded && !matches_rel(&defaults, rel) {
                let full = format!("{prefix}{rel}");
                embedded_roots.insert(full.clone());
                collect_git_files(&child, &full, files, embedded_roots, include_ignored);
            }
            continue;
        }
        files.insert(format!("{prefix}{rel}"));
    }

    // Gitlinks with a real checkout on disk — skipped when gitignored and
    // not opted in (#1065), skipped when a worktree (#945), and left alone
    // when uninitialized (no `.git` on disk — nothing to index).
    if !gitlink_rels.is_empty() {
        let repo_ignore = build_default_ignore(repo_dir);
        for rel in gitlink_rels {
            let rel_dir = ensure_trailing_slash(&rel);
            if gitlink_embedded_repo_skipped(
                &rel_dir,
                prefix,
                &defaults,
                &repo_ignore,
                include_ignored,
            ) {
                continue;
            }
            let child = repo_dir.join(&rel);
            if classify_git_dir(&child) != GitDirClass::Embedded {
                continue;
            }
            let full = format!("{prefix}{rel_dir}");
            embedded_roots.insert(full.clone());
            collect_git_files(&child, &full, files, embedded_roots, include_ignored);
        }
    }

    // Embedded repos hidden by THIS repo's ignore rules — opt-in only.
    for rel in find_ignored_embedded_repos(repo_dir, include_ignored, prefix) {
        let full = format!("{prefix}{rel}");
        embedded_roots.insert(full.clone());
        collect_git_files(
            &repo_dir.join(&rel),
            &full,
            files,
            embedded_roots,
            include_ignored,
        );
    }
    Some(())
}

/// The git fast path: all git-visible files + discovered embedded roots,
/// or `None` when git is unavailable, a listing failed, or the root is
/// itself gitignored by an enclosing repo (its `ls-files` would be empty) —
/// the caller falls back to the FS walk.
fn git_visible_files(
    root: &Path,
    overrides: &ScanOverrides,
) -> Option<(BTreeSet<String>, BTreeSet<String>)> {
    let toplevel = run_git(root, &["rev-parse", "--show-toplevel"], GIT_TIMEOUT_SHORT)
        .filter(|o| o.success)?;
    let toplevel = std::str::from_utf8(&toplevel.stdout)
        .ok()?
        .trim()
        .to_string();

    // Canonicalize both sides (macOS: /tmp vs /private/tmp) before deciding
    // whether the scan root IS the repo toplevel.
    let root_canon = std::fs::canonicalize(root).ok()?;
    let toplevel_canon =
        std::fs::canonicalize(&toplevel).unwrap_or_else(|_| PathBuf::from(&toplevel));
    if toplevel_canon != root_canon {
        // `git check-ignore -q` exits 0 when the path IS ignored — the
        // enclosing repo would list nothing for it; walk instead. A failed
        // probe (timeout/spawn) means "not known ignored": proceed.
        if let Some(out) = run_git(
            root,
            &["check-ignore", "-q", &root_canon.to_string_lossy()],
            GIT_TIMEOUT_SHORT,
        ) && out.success
        {
            return None;
        }
    }

    let include_ignored = overrides.include_ignored_matcher();
    let mut files = BTreeSet::new();
    let mut embedded_roots = BTreeSet::new();
    collect_git_files(
        root,
        "",
        &mut files,
        &mut embedded_roots,
        include_ignored.as_ref(),
    )?;
    Some((files, embedded_roots))
}

// =============================================================================
// Filesystem-walk fallback (non-git projects, gitignored roots)
// =============================================================================

/// A `.gitignore` matcher scoped to the directory that declared it —
/// nested `.gitignore` patterns are relative to their directory, so paths
/// are tested relative to `dir`, mirroring how git layers `.gitignore`
/// files at every level.
struct ScopedIgnore {
    dir: PathBuf,
    matcher: Gitignore,
}

/// Recursive FS walk: per-directory scoped `.gitignore`s over a base of the
/// built-in defaults + root `.gitignore` (+ the `exclude` override, matched
/// root-relative like the git path's ScopeIgnore), with a
/// canonicalized-visited-set symlink-cycle guard. Returns sorted,
/// root-relative source files.
fn scan_directory_walk(root: &Path, overrides: &ScanOverrides) -> Vec<String> {
    let mut files: BTreeSet<String> = BTreeSet::new();
    let mut visited: BTreeSet<PathBuf> = BTreeSet::new();

    let mut matchers: Vec<ScopedIgnore> = vec![ScopedIgnore {
        dir: root.to_path_buf(),
        matcher: build_default_ignore(root),
    }];
    let exclude_patterns: Vec<&str> = overrides
        .exclude
        .iter()
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .collect();
    if !exclude_patterns.is_empty() {
        matchers.push(ScopedIgnore {
            dir: root.to_path_buf(),
            matcher: matcher_from_lines(exclude_patterns),
        });
    }

    walk(root, root, &mut matchers, &mut files, &mut visited);
    files.into_iter().collect()
}

/// Whether `full` is ignored by any scoped matcher — tested relative to
/// each matcher's own directory, with the trailing-slash directory
/// convention.
fn walk_is_ignored(full: &Path, is_dir: bool, matchers: &[ScopedIgnore]) -> bool {
    matchers.iter().any(|scoped| {
        let Ok(rel) = full.strip_prefix(&scoped.dir) else {
            return false; // not under this matcher's dir
        };
        let mut rel = rel_to_slash_string(rel);
        if rel.is_empty() {
            return false;
        }
        if is_dir {
            rel.push('/');
        }
        matches_rel(&scoped.matcher, &rel)
    })
}

/// `Path` → forward-slash string (components joined with `/`; non-UTF-8
/// segments are lossily converted — such paths cannot round-trip through
/// the `String` scan surface anyway).
fn rel_to_slash_string(rel: &Path) -> String {
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// One directory level of the FS walk. `matchers` is a stack: this
/// directory's own `.gitignore` (if any) is pushed for the recursion below
/// it and popped on the way out.
fn walk(
    dir: &Path,
    root: &Path,
    matchers: &mut Vec<ScopedIgnore>,
    files: &mut BTreeSet<String>,
    visited: &mut BTreeSet<PathBuf>,
) {
    let Ok(real_dir) = std::fs::canonicalize(dir) else {
        return; // unresolvable — skip
    };
    if !visited.insert(real_dir) {
        return; // symlink cycle — this physical directory was already walked
    }

    // This directory's own .gitignore applies to everything below it. The
    // root's is already merged into the seeded base matcher (so a negation
    // there can override a built-in default) — skip it here.
    let mut pushed = false;
    if dir != root {
        let lines = read_gitignore_lines(&dir.join(".gitignore"));
        if !lines.is_empty() {
            matchers.push(ScopedIgnore {
                dir: dir.to_path_buf(),
                matcher: matcher_from_lines(lines.iter().map(String::as_str)),
            });
            pushed = true;
        }
    }

    if let Ok(entries) = std::fs::read_dir(dir) {
        let mut names: Vec<(String, std::fs::DirEntry)> = entries
            .filter_map(|e| e.ok())
            .filter_map(|e| e.file_name().into_string().ok().map(|n| (n, e)))
            .collect();
        names.sort_by(|a, b| a.0.cmp(&b.0));

        for (name, entry) in names {
            // Never descend into git internals or the data dir.
            if name == ".git" || is_data_dir(&name) {
                continue;
            }
            let full = dir.join(&name);
            let Ok(rel) = full.strip_prefix(root) else {
                continue;
            };
            let rel_str = rel_to_slash_string(rel);
            let Ok(file_type) = entry.file_type() else {
                continue;
            };

            // Symlinks resolve to their target's type (the cycle guard above
            // keeps a link loop finite); broken links are skipped.
            let (is_dir, is_file) = if file_type.is_symlink() {
                match std::fs::metadata(&full) {
                    Ok(target) => (target.is_dir(), target.is_file()),
                    Err(_) => continue,
                }
            } else {
                (file_type.is_dir(), file_type.is_file())
            };

            if is_dir {
                if !walk_is_ignored(&full, true, matchers) {
                    walk(&full, root, matchers, files, visited);
                }
            } else if is_file
                && !walk_is_ignored(&full, false, matchers)
                && is_source_file(&rel_str)
            {
                files.insert(rel_str);
            }
        }
    }

    if pushed {
        matchers.pop();
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    /// The gitdir-pointer regex cases from CodeGraph's worktree handling
    /// (#848/#945), including Windows separators — a `.git` file pointing
    /// into `worktrees/` is a duplicate view; a plain submodule gitdir
    /// (`modules/<m>` without a `worktrees/` segment) is distinct code.
    #[test]
    fn worktree_gitdir_pointer_regex_cases() {
        for wt in [
            "/home/u/repo/.git/worktrees/feature",
            "../.git/worktrees/feature",
            ".git/worktrees/wt",
            "C:\\Users\\u\\repo\\.git\\worktrees\\feature",
            "/r/.git/modules/sub/worktrees/feature", // submodule worktree (#945)
            "C:\\r\\.git\\modules\\sub\\worktrees\\f",
            "/mixed/.git\\worktrees/x",
        ] {
            assert!(is_worktree_gitdir_pointer(wt), "must match: {wt}");
        }
        for not_wt in [
            "/r/.git/modules/sub",        // plain submodule — distinct code
            "/r/.git/modules/sub/deeper", // still no worktrees/ segment
            "/somewhere/else/entirely",   // unrelated path
            "/r/.git",                    // bare gitdir
            "worktrees/x",                // no .git/ anchor
            "/r/.gitx/worktrees/x",       // .git must be a whole path segment
        ] {
            assert!(
                !is_worktree_gitdir_pointer(not_wt),
                "must NOT match: {not_wt}"
            );
        }
    }

    #[test]
    fn stage_entry_parse_and_whole_cwd_sentinels() {
        assert_eq!(
            parse_stage_entry("100644 abc123 0\tsrc/main.rs"),
            Some(("src/main.rs", false))
        );
        assert_eq!(
            parse_stage_entry("160000 abc123 0\ttool"),
            Some(("tool", true))
        );
        assert_eq!(parse_stage_entry("garbage-without-tab"), None);

        assert!(is_whole_cwd_entry("./"));
        assert!(is_whole_cwd_entry("."));
        assert!(is_whole_cwd_entry(""));
        assert!(!is_whole_cwd_entry("child/"));
    }
}
