//! Rust `crate::`/`self::`/`super::` (and bare) module paths → the leaf symbol
//! in the module's file.

use selene_core::{NodeKind, UnresolvedRef};

use crate::context::ResolutionContext;
use crate::imports::imported;
use crate::imports::path_helpers::{join_rel, parent_dir};
use crate::types::ResolvedRef;

/// Rust `crate::m::Item` / `self::sub::Item` / `super::m::func` → the leaf symbol
/// in the module's file (**0.9**).
///
/// Disambiguates the common-name `pub use self::read::read` re-export that
/// name-matching lands on the wrong same-named symbol.
pub(super) fn resolve_rust_path_reference<C: ResolutionContext>(
    r: &UnresolvedRef,
    ctx: &C,
) -> Option<ResolvedRef> {
    let segments: Vec<&str> = r
        .reference_name
        .split("::")
        .filter(|s| !s.is_empty())
        .collect();
    if segments.len() < 2 {
        return None;
    }
    let leaf = segments[segments.len() - 1];
    let mod_segs = &segments[..segments.len() - 1];

    let file = resolve_rust_module_file(mod_segs, &r.file_path, ctx)?;
    if file == r.file_path {
        return None;
    }

    let group = ctx.nodes_in_file(&file);
    let target = group.iter().find(|n| {
        n.name == leaf
            && matches!(
                n.kind,
                NodeKind::Function
                    | NodeKind::Struct
                    | NodeKind::Enum
                    | NodeKind::Trait
                    | NodeKind::TypeAlias
                    | NodeKind::Constant
                    | NodeKind::Method
                    | NodeKind::Class
                    | NodeKind::Interface
            )
    })?;
    Some(imported(r, &target.id, 0.9))
}

/// The crate-root directory (the one holding `lib.rs`/`main.rs`), walking up.
///
/// Capped at **64** levels — a repo nested deeper than that is pathological, and
/// an uncapped walk on a symlinked tree does not terminate.
fn rust_crate_root_dir<C: ResolutionContext>(from_file: &str, ctx: &C) -> Option<String> {
    let mut dir = parent_dir(from_file);
    for _ in 0..64 {
        let lib = join_rel(&dir, "lib.rs");
        let main = join_rel(&dir, "main.rs");
        if ctx.file_exists(&lib) || ctx.file_exists(&main) {
            return Some(dir);
        }
        if dir.is_empty() {
            return None;
        }
        dir = parent_dir(&dir);
    }
    None
}

/// The directory under which THIS file's module declares its submodules.
///
/// `mod.rs`/`lib.rs`/`main.rs` own their directory; `foo.rs`'s submodules live
/// in `foo/`.
fn rust_self_module_dir(from_file: &str) -> String {
    let dir = parent_dir(from_file);
    let base = from_file.rsplit('/').next().unwrap_or(from_file);
    if matches!(base, "mod.rs" | "lib.rs" | "main.rs") {
        return dir;
    }
    let stem = base.strip_suffix(".rs").unwrap_or(base);
    join_rel(&dir, stem)
}

/// Walk module segments down from `start_dir`, mapping each to `<seg>.rs` or
/// `<seg>/mod.rs`. `None` if any segment has no file.
fn resolve_rust_under<C: ResolutionContext>(
    start_dir: Option<String>,
    rest: &[&str],
    ctx: &C,
) -> Option<String> {
    let mut dir = start_dir?;
    let mut target: Option<String> = None;
    for seg in rest {
        if matches!(*seg, "self" | "crate" | "super") {
            continue;
        }
        let as_file = join_rel(&dir, &format!("{seg}.rs"));
        let as_mod = join_rel(&dir, &format!("{seg}/mod.rs"));
        if ctx.file_exists(&as_file) {
            target = Some(as_file);
        } else if ctx.file_exists(&as_mod) {
            target = Some(as_mod);
        } else {
            return None;
        }
        dir = join_rel(&dir, seg);
    }
    target
}

/// A Rust module path (segments WITHOUT the leaf symbol) → the last module
/// segment's file.
fn resolve_rust_module_file<C: ResolutionContext>(
    segments: &[&str],
    from_file: &str,
    ctx: &C,
) -> Option<String> {
    let first = *segments.first()?;

    match first {
        "crate" => resolve_rust_under(rust_crate_root_dir(from_file, ctx), &segments[1..], ctx),
        "self" => resolve_rust_under(Some(rust_self_module_dir(from_file)), &segments[1..], ctx),
        "super" => {
            let supers = segments.iter().take_while(|s| **s == "super").count();
            let mut dir = Some(rust_self_module_dir(from_file));
            for _ in 0..supers {
                dir = dir.filter(|d| !d.is_empty()).map(|d| parent_dir(&d));
            }
            resolve_rust_under(dir, &segments[supers..], ctx)
        }
        // A BARE path. In expression position (`submodule::item()` — the
        // router-assembly and general cross-module-call pattern) the prefix is a
        // SUBMODULE of the current module, i.e. 2018 `self::`-relative — so try
        // self-relative FIRST, then crate-relative for 2015-edition / crate-root
        // items. An external crate path (`serde::de::Error`) misses both and
        // falls through to name-matching.
        _ => resolve_rust_under(Some(rust_self_module_dir(from_file)), segments, ctx)
            .or_else(|| resolve_rust_under(rust_crate_root_dir(from_file, ctx), segments, ctx)),
    }
}
