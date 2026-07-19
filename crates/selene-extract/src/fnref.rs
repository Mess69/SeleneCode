//! Function-as-value capture (#756) — the `FN_REF_SPECS` port
//! (`../codegraph/src/extraction/function-ref.ts`; design contract:
//! `docs/reference/from-codegraph/design/function-ref-capture.md`).
//!
//! A function name used as a VALUE — passed as a call argument
//! (`register_handler(target_cb)`), assigned to a field or function pointer
//! (`o->cb = target_cb`), placed in a struct/object initializer
//! (`{ .recv_cb = my_cb }`, `{ recv: targetCb }`), or listed in a function
//! table (`static cb_t table[] = { cb_a, cb_b }`) — is a real dependency that
//! call extraction misses entirely: `callers(target_cb)` showed nothing but
//! direct calls, so every registered callback looked dead and its
//! registration sites were invisible to impact analysis.
//!
//! This module is the CAPTURE side only: the per-language spec table + the pure
//! "pull candidate names out of a dispatched container" function. The walkers
//! drive it ([`Session::maybe_capture_fn_refs`] and [`scan_fn_ref_subtree`] in
//! `walker/body.rs`) and the GATE (`flush_fn_ref_candidates`, same file) decides
//! which candidates become `function_ref` [`crate::UnresolvedReference`]s.
//! Resolution (unique-or-drop, class-scoped `this.X`, overload refusal) is
//! Phase 3.
//!
//! `function_ref` is an INTERNAL reference kind: resolution maps it to a
//! `references` edge (`metadata.fnRef = true`) — it never persists as an edge
//! kind (map §Wire).
//!
//! **Coverage.** All 13 v0 languages have rows: C, C++, TS/TSX/JS/JSX and
//! Python (Task 15a) plus Go, Rust, Java, Kotlin, C#, Ruby and PHP (Task 15b).
//! ObjC/Swift/Scala/Dart/Lua/Luau/Pascal rows exist in the TS source and land
//! with their grammars in wave 2.
//!
//! **Known gap (Task 15b):** Ruby's hook-DSL symbols (`before_action :auth`)
//! are specified here but never reach capture — the walker's import branch
//! consumes every class-scope Ruby `call` (Ruby's `import_types` is `["call"]`)
//! without walking its children, diverging from TS (`tree-sitter.ts:1173-1175`
//! leaves `skipChildren` false). The three `#[ignore]`d tests in
//! `tests/fnref_test.rs` flip green the moment that one-line walker fix lands.

mod capture;
mod normalize;
mod regexes;
mod spec;

pub(crate) use capture::{FnRefCandidate, capture_fn_ref_candidates};
pub(crate) use regexes::{QUALIFIED_IMPORT, SIMPLE_NAME};
pub(crate) use spec::{CaptureMode, FnRefSpec, fn_ref_spec};

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::normalize::capitalized_receiver;
    use super::regexes::{
        CPP_QUALIFIED_NAME, LHS_LAST_NAME, PHP_PLAIN_CALLABLE, PHP_QUALIFIED_CALLABLE,
        RUBY_SYMBOL_NAME,
    };
    use super::spec::{PHP_CALLABLE_HOFS, is_ruby_hook_call};
    use super::*;
    use crate::Language;

    #[test]
    fn lhs_last_name_regex() {
        let last = |s: &str| {
            LHS_LAST_NAME
                .captures(s)
                .and_then(|c| c.get(1))
                .map(|m| m.as_str().to_string())
        };
        assert_eq!(last("o->cb"), Some("cb".to_string()));
        assert_eq!(last("this.status"), Some("status".to_string()));
        assert_eq!(last("obj.handlers.onClick "), Some("onClick".to_string()));
        assert_eq!(last("cb"), Some("cb".to_string()));
        // A subscript LHS has no trailing simple name.
        assert_eq!(last("table[0]"), None);
    }

    #[test]
    fn cpp_qualified_name_regex() {
        assert!(CPP_QUALIFIED_NAME.is_match("Widget::on_click"));
        assert!(CPP_QUALIFIED_NAME.is_match("ns::Widget::on_click"));
        assert!(CPP_QUALIFIED_NAME.is_match("plain"));
        assert!(!CPP_QUALIFIED_NAME.is_match("Widget::on_click(int)"));
        assert!(!CPP_QUALIFIED_NAME.is_match("1bad"));
    }

    #[test]
    fn import_name_regexes() {
        assert!(SIMPLE_NAME.is_match("assist"));
        assert!(SIMPLE_NAME.is_match("$jq"));
        assert!(!SIMPLE_NAME.is_match("com.example.Other"));

        let last = |s: &str| {
            QUALIFIED_IMPORT
                .captures(s)
                .and_then(|c| c.get(1))
                .map(|m| m.as_str().to_string())
        };
        assert_eq!(
            last("com.example.OtherClass"),
            Some("OtherClass".to_string())
        );
        assert_eq!(last(r"App\Services\Mailer"), Some("Mailer".to_string()));
        assert_eq!(last("plain"), None);
    }

    #[test]
    fn spec_registry_covers_every_v0_language() {
        for l in [
            Language::C,
            Language::Cpp,
            Language::Typescript,
            Language::Tsx,
            Language::Javascript,
            Language::Jsx,
            Language::Python,
            Language::Go,
            Language::Rust,
            Language::Java,
            Language::Kotlin,
            Language::CSharp,
            Language::Ruby,
            Language::Php,
        ] {
            assert!(fn_ref_spec(l).is_some(), "no fn-ref spec for {l:?}");
        }
        // Wave 2 — their TS rows exist but the grammars don't ship in v0.
        assert!(fn_ref_spec(Language::Swift).is_none());
        assert!(fn_ref_spec(Language::Objc).is_none());
    }

    #[test]
    fn languages_without_bare_identifier_function_values() {
        // Java/Kotlin/Ruby/PHP have EMPTY `id_types`: a bare identifier is a
        // call, a local or a param — never a function value.
        for l in [
            Language::Java,
            Language::Kotlin,
            Language::Ruby,
            Language::Php,
        ] {
            assert!(
                !fn_ref_spec(l).unwrap().is_id_type("identifier"),
                "{l:?} must not accept bare identifiers"
            );
        }
        // …while C/Go/Rust/C# do.
        for l in [Language::C, Language::Go, Language::Rust, Language::CSharp] {
            assert!(fn_ref_spec(l).unwrap().is_id_type("identifier"));
        }
    }

    #[test]
    fn ruby_hook_call_detection() {
        // Lifecycle callbacks + the named hook DSLs.
        for name in [
            "before_action",
            "after_save",
            "around_create",
            "skip_before_action",
            "validate",
            "set_callback",
            "helper_method",
            "rescue_from",
        ] {
            assert!(is_ruby_hook_call(name), "{name} should be a hook");
        }
        // `validates` (PLURAL) is EXCLUDED — its symbols are ATTRIBUTES.
        assert!(!is_ruby_hook_call("validates"));
        // Not hooks.
        for name in ["register", "before", "after", "each", "attr_accessor"] {
            assert!(!is_ruby_hook_call(name), "{name} should NOT be a hook");
        }
        // Symbol names may carry `?`/`!`.
        assert!(RUBY_SYMBOL_NAME.is_match("valid?"));
        assert!(RUBY_SYMBOL_NAME.is_match("save!"));
        assert!(!RUBY_SYMBOL_NAME.is_match("2bad"));
    }

    #[test]
    fn php_callable_regexes() {
        assert!(PHP_PLAIN_CALLABLE.is_match("cmp_items"));
        assert!(!PHP_PLAIN_CALLABLE.is_match("Cls::m"));
        assert!(PHP_QUALIFIED_CALLABLE.is_match("Cls::m"));
        assert!(!PHP_QUALIFIED_CALLABLE.is_match("just_a_string with spaces"));
        // The HOF list is the positional prior that makes a bare string
        // trustworthy — verbatim from function-ref.ts:347 (27 entries).
        assert_eq!(PHP_CALLABLE_HOFS.len(), 27);
        assert!(PHP_CALLABLE_HOFS.contains(&"usort"));
        assert!(PHP_CALLABLE_HOFS.contains(&"is_callable"));
        assert!(!PHP_CALLABLE_HOFS.contains(&"add_action")); // WordPress — deliberately not core
    }

    #[test]
    fn capitalized_receiver_gate() {
        assert_eq!(capitalized_receiver("Main::cb"), Some("Main"));
        // A lowercase receiver is a VARIABLE — unknown type, no edge.
        assert_eq!(capitalized_receiver("subscriber::onNext"), None);
        assert_eq!(capitalized_receiver("nocolons"), None);
    }

    #[test]
    fn cpp_is_address_of_only_but_c_is_not() {
        // Design doc rule 4 — the ONE difference between the two C-family rows.
        assert!(!fn_ref_spec(Language::C).unwrap().address_of_only);
        assert!(fn_ref_spec(Language::Cpp).unwrap().address_of_only);
    }

    #[test]
    fn c_family_ungates_only_initializer_modes() {
        let c = fn_ref_spec(Language::C).unwrap();
        assert!(c.is_ungated_mode(CaptureMode::Value));
        assert!(c.is_ungated_mode(CaptureMode::List));
        // `rhs`/`varinit` stay gated (design doc rule 2).
        assert!(!c.is_ungated_mode(CaptureMode::Rhs));
        assert!(!c.is_ungated_mode(CaptureMode::VarInit));
        assert!(!c.is_ungated_mode(CaptureMode::Args));
        // TS/JS ungate nothing.
        let ts = fn_ref_spec(Language::Typescript).unwrap();
        assert!(!ts.is_ungated_mode(CaptureMode::Value));
    }
}
