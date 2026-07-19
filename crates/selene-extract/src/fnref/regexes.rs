//! The fnref regex statics, shared across the capture/normalize submodules.

use std::sync::LazyLock;

use regex::Regex;

// ---------------------------------------------------------------------------
// Regexes (compile-time literals — the house `unwrap` idiom, each exercised by
// a test below).
// ---------------------------------------------------------------------------

/// A Ruby method symbol — `?`/`!` suffixes are legal method names
/// (function-ref.ts:803).
pub(super) static RUBY_SYMBOL_NAME: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)] // compile-time literal, pinned by `ruby_hook_call_detection`
    Regex::new(r"^[A-Za-z_][A-Za-z0-9_?!]*$").unwrap()
});

/// A PHP plain string callable (`'cmp_items'`) (function-ref.ts:759).
pub(super) static PHP_PLAIN_CALLABLE: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)] // compile-time literal, pinned by `php_callable_regexes`
    Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*$").unwrap()
});

/// A PHP qualified string callable (`'Cls::method'`) (function-ref.ts:762).
pub(super) static PHP_QUALIFIED_CALLABLE: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)] // compile-time literal, pinned by `php_callable_regexes`
    Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*::[A-Za-z_][A-Za-z0-9_]*$").unwrap()
});

/// The trailing simple name of an assignment LHS (`o->cb` → `cb`,
/// `this.status` → `status`) — the param-forward skip's comparison key
/// (function-ref.ts:441).
pub(super) static LHS_LAST_NAME: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)] // compile-time literal, pinned by `lhs_last_name_regex`
    Regex::new(r"([A-Za-z_$][A-Za-z0-9_$]*)\s*$").unwrap()
});

/// A C++ member-pointer target (`Widget::on_click`) — ASCII-explicit because
/// Rust's `\w` is Unicode-aware while the TS source's was not
/// (function-ref.ts:586).
pub(super) static CPP_QUALIFIED_NAME: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)] // compile-time literal, pinned by `cpp_qualified_name_regex`
    Regex::new(r"^[A-Za-z_][0-9A-Za-z_:]*$").unwrap()
});

/// A gate-eligible simple binding name (tree-sitter.ts:625 `SIMPLE_NAME`).
pub(crate) static SIMPLE_NAME: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)] // compile-time literal, pinned by `import_name_regexes`
    Regex::new(r"^[A-Za-z_$][A-Za-z0-9_$]*$").unwrap()
});

/// A dotted/backslashed import whose LAST segment is the simple name code
/// actually references — JVM `import com.example.OtherClass`, PHP
/// `use App\Services\Mailer` (tree-sitter.ts:629 `QUALIFIED_IMPORT`).
pub(crate) static QUALIFIED_IMPORT: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)] // compile-time literal, pinned by `import_name_regexes`
    Regex::new(r"^[A-Za-z_$][A-Za-z0-9_$.\\]*[.\\]([A-Za-z_$][A-Za-z0-9_$]*)$").unwrap()
});
