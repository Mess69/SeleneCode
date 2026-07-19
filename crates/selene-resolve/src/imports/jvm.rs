//! Java/Kotlin: FQN imports through the qualified-name index (ladder step 5)
//! and references whose receiver is the simple name of an imported FQN.

use std::sync::Arc;

use selene_core::{Language, Node, UnresolvedRef};

use crate::context::ResolutionContext;
use crate::imports::imported;
use crate::types::{ImportMapping, ResolvedRef};

/// A Java/Kotlin `import com.example.Bar` → the `Bar` declared in package
/// `com.example`, through the **qualified-name index** — confidence **0.95**.
///
/// Ladder step 5, ahead of the frameworks and the name matcher: a JVM FQN is
/// unambiguous even when several `Bar` classes exist in different packages,
/// which is exactly the collision the path-proximity matcher cannot resolve
/// (#314). JVM imports are decoupled from filenames (a Kotlin `Utils.kt` can
/// export `Bar`), so the JS-style filesystem walk misses them entirely.
pub fn resolve_jvm_import<C: ResolutionContext>(r: &UnresolvedRef, ctx: &C) -> Option<ResolvedRef> {
    if r.reference_kind != "imports" {
        return None;
    }
    let lang = r.language;
    if !matches!(lang, Language::Java | Language::Kotlin) {
        return None;
    }

    let fqn = r.reference_name.as_str();
    let last_dot = fqn.rfind('.')?;
    if last_dot == 0 {
        return None;
    }
    let (pkg, sym) = (&fqn[..last_dot], &fqn[last_dot + 1..]);
    // A wildcard import names no single symbol — it punts to name-matching.
    if sym == "*" {
        return None;
    }

    let candidates = ctx.nodes_by_qualified_name(&format!("{pkg}::{sym}"));
    let best = match candidates.len() {
        0 => return None,
        1 => candidates.first().cloned()?,
        _ => pick_closest_jvm_candidate(&candidates, &r.file_path)?,
    };
    Some(imported(r, &best.id, 0.95))
}

/// Among same-FQN candidates, the one **closest to the importing file** by
/// shared directory prefix, preferring an `expect` declaration on a tie.
///
/// Kotlin Multiplatform: an `expect` declaration and its `actual`s share one FQN
/// across source sets (commonMain / androidMain / appleMain). Taking the first
/// candidate let a single platform `actual` absorb every common-side import, so
/// the `expect` — the canonical API a commonMain file imports — looked unused.
fn pick_closest_jvm_candidate(candidates: &[Arc<Node>], from_path: &str) -> Option<Arc<Node>> {
    let from_dirs: Vec<&str> = from_path.split('/').collect();
    let from_dirs = &from_dirs[..from_dirs.len().saturating_sub(1)];

    let shared_prefix = |p: &str| -> usize {
        let parts: Vec<&str> = p.split('/').collect();
        let dirs = &parts[..parts.len().saturating_sub(1)];
        from_dirs
            .iter()
            .zip(dirs.iter())
            .take_while(|(a, b)| a == b)
            .count()
    };
    let is_expect = |n: &Node| n.decorators.iter().any(|d| d == "expect");

    let mut best: Option<&Arc<Node>> = None;
    let mut best_prox = 0usize;
    for c in candidates {
        let prox = shared_prefix(&c.file_path);
        let take = match best {
            None => true,
            Some(b) => prox > best_prox || (prox == best_prox && is_expect(c) && !is_expect(b)),
        };
        if take {
            best_prox = prox;
            best = Some(c);
        }
    }
    best.cloned()
}

/// Java/Kotlin: a reference whose receiver is the simple name of an imported FQN
/// (**0.9**).
///
/// `import com.example.Foo;` + `Foo.bar()` → the FQN becomes a **path suffix**
/// (`com/example/Foo.java`), which uniquely identifies the right symbol when
/// several classes share a simple name (#314). The file may live under any source
/// root (`src/main/java/`, `src/`, …), so it is matched by suffix, never by exact
/// path. `import static com.example.Foo.bar;` uses the OWNER's path instead.
pub(super) fn resolve_jvm_imported_reference<C: ResolutionContext>(
    r: &UnresolvedRef,
    lang: Language,
    imports: &[ImportMapping],
    ctx: &C,
) -> Option<ResolvedRef> {
    let ext = if lang == Language::Kotlin {
        ".kt"
    } else {
        ".java"
    };

    for imp in imports {
        let matches_bare = imp.local_name == r.reference_name;
        let matches_qualified = r
            .reference_name
            .starts_with(&format!("{}.", imp.local_name));
        if !matches_bare && !matches_qualified {
            continue;
        }

        let fqn_path = format!("{}{ext}", imp.source.replace('.', "/"));
        let member_name = if matches_bare {
            imp.local_name.clone()
        } else {
            r.reference_name[imp.local_name.len() + 1..].to_string()
        };

        let candidates = ctx.nodes_by_name(&member_name);
        for node in candidates.iter() {
            if node.language != lang {
                continue;
            }
            let fp: std::borrow::Cow<'_, str> = if node.file_path.contains('\\') {
                node.file_path.replace('\\', "/").into()
            } else {
                node.file_path.as_str().into()
            };
            if fp.ends_with(&fqn_path) {
                return Some(imported(r, &node.id, 0.9));
            }
        }

        // `import static com.example.Util.helper;` — the FQN's tail IS the
        // member, so the owner class's path is what identifies it.
        if matches_bare
            && let Some(dot) = imp.source.rfind('.')
            && dot > 0
        {
            let owner_path = format!("{}{ext}", imp.source[..dot].replace('.', "/"));
            for node in candidates.iter() {
                if node.language != lang {
                    continue;
                }
                let fp: std::borrow::Cow<'_, str> = if node.file_path.contains('\\') {
                    node.file_path.replace('\\', "/").into()
                } else {
                    node.file_path.as_str().into()
                };
                if fp.ends_with(&owner_path) {
                    return Some(imported(r, &node.id, 0.9));
                }
            }
        }
    }
    None
}
