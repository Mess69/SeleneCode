//! C/C++/C# offset-preserving pre-parse blankers (Task 12) — the pure
//! string→string half of the C-family configs (Task 13 wires them into
//! `pre_parse`), ported from `../codegraph/src/extraction/languages/c-cpp.ts`
//! and `csharp.ts`.
//!
//! ## The byte-preservation contract (Global Constraints)
//!
//! Every blanker replaces matched **byte ranges** with equal-length runs of
//! space bytes, preserving newline bytes — positions feed node ids, so byte
//! offsets (and thus every line/column) must survive exactly. The TS
//! originals replaced per *char* (`' '.repeat(m.length)`), which was only
//! correct because the same JS string went to the parser; here the parser
//! consumes bytes, so a non-ASCII char inside a blanked range becomes one
//! space **per byte** (pinned by the multibyte test). Two deliberate
//! spelling ports on newline handling: most blankers keep both `\n` and
//! `\r`; [`blank_cuda_constructs`]' launch-config replacer keeps only `\n`
//! (the TS spelling — `\r` inside a launch config becomes a space, which is
//! still byte-preserving).
//!
//! ## Lookahead emulation
//!
//! The TS regexes lean on `(?=…)` lookahead, which the `regex` crate does
//! not support. Each is emulated as an explicit scan loop: find the token,
//! test the following text with a `^`-anchored "after" regex, blank only the
//! token, and resume the scan **at the token's end** — exactly the
//! `lastIndex` walk the TS `g`-regex performed (pinned by the
//! adjacent-macros test, which a consuming translation would fail).
//!
//! Balanced-paren blankers ([`blank_cpp_inline_annotation_macros`],
//! [`blank_cpp_annotation_macro_calls`]) interleave a regex find with a
//! manual byte scan that skips string/char literals (an embedded `)` can't
//! mis-close the balance) — offsets driven explicitly since there is no
//! `lastIndex`.

// Staging allowance: these pure functions are wired by the *config* tasks —
// C/C++/Metal/CUDA by Task 13 (core chain), C# by Task 14 — so between this
// commit and those, the compiler sees them as unused. Task 13 removes this.
#![allow(dead_code)]

use std::borrow::Cow;
use std::sync::LazyLock;

use regex::Regex;

/// Compile a literal, compile-time-known-good pattern (the crate's
/// established Regex idiom — see `src/generated.rs`).
macro_rules! literal_regex {
    ($pat:expr) => {{
        LazyLock::new(|| {
            #[allow(clippy::unwrap_used)] // literal pattern, compile-time known good
            Regex::new($pat).unwrap()
        })
    }};
}

/// Blank `range` in `bytes` with spaces, keeping `\n` (and `\r` unless
/// `keep_cr` is false — the CUDA launch-config replacer's TS spelling).
fn blank_bytes(bytes: &mut [u8], range: std::ops::Range<usize>, keep_cr: bool) {
    for b in &mut bytes[range] {
        if *b == b'\n' || (keep_cr && *b == b'\r') {
            continue;
        }
        *b = b' ';
    }
}

/// Finalize a byte-blanked buffer. Safe by construction (only whole-char
/// ranges are overwritten with ASCII spaces); the fallback can't be hit but
/// keeps the no-panic contract.
fn into_string(bytes: Vec<u8>, original: &str) -> Cow<'_, str> {
    match String::from_utf8(bytes) {
        Ok(s) => Cow::Owned(s),
        Err(_) => Cow::Borrowed(original),
    }
}

// =============================================================================
// C++ export / inline / API-prefix macros
// =============================================================================

/// `class MACRO Name …` definition headers: `\b(class|struct)\s+MACRO`, with
/// the definition guard (`Name [final] :` or `{`) tested as an explicit
/// after-check (the TS lookahead). Only the macro is blanked.
static CPP_EXPORT_RE: LazyLock<Regex> = literal_regex!(r"\b(class|struct)(\s+)([A-Z][A-Z0-9_]+)");
static CPP_EXPORT_AFTER_RE: LazyLock<Regex> =
    literal_regex!(r"^\s+[A-Za-z_]\w*(?:\s+final)?\s*[:{]");

/// Blank an export/visibility macro in a `class/struct EXPORT_MACRO Name …`
/// *definition* header (#946/#1061): tree-sitter reads `class MACRO` as an
/// elaborated type and the whole class drops out of the index. The trailing
/// `[:{]` guard fires only on definitions, so elaborated-type variable
/// declarations (`struct FOO var;`) and value uses stay untouched. C++-only
/// (wired by Task 13).
pub(crate) fn blank_cpp_export_macros(source: &str) -> Cow<'_, str> {
    if !source.contains("class") && !source.contains("struct") {
        return Cow::Borrowed(source);
    }
    let mut bytes: Option<Vec<u8>> = None;
    for caps in CPP_EXPORT_RE.captures_iter(source) {
        let Some(macro_m) = caps.get(3) else { continue };
        let Some(full) = caps.get(0) else { continue };
        if !CPP_EXPORT_AFTER_RE.is_match(&source[full.end()..]) {
            continue;
        }
        blank_bytes(
            bytes.get_or_insert_with(|| source.as_bytes().to_vec()),
            macro_m.range(),
            true,
        );
    }
    match bytes {
        Some(b) => into_string(b, source),
        None => Cow::Borrowed(source),
    }
}

/// The curated inline-specifier tokens, copied **verbatim** (values and
/// grouping) from `CPP_INLINE_MACROS` in c-cpp.ts. To cover a new codebase's
/// inline macro, add its exact token here.
const CPP_INLINE_MACROS: [&str; 60] = [
    // Unreal Engine
    "FORCEINLINE_DEBUGGABLE",
    "FORCENOINLINE",
    "FORCEINLINE",
    // pugixml (ubiquitous vendored XML parser): `#define PUGI__FN inline`
    // before the return type, plus `PUGIXML_FUNCTION` (linkage macro) between
    // the return type and the name — the blank mechanism handles both.
    "PUGI__FN_NO_INLINE",
    "PUGI__FN",
    "PUGIXML_FUNCTION",
    // Godot
    "_ALWAYS_INLINE_",
    "_FORCE_INLINE_",
    // Boost
    "BOOST_FORCEINLINE",
    "BOOST_NOINLINE",
    // Qt (per-method markers + inline)
    "Q_INVOKABLE",
    "Q_SCRIPTABLE",
    "Q_ALWAYS_INLINE",
    "Q_SLOT",
    "Q_SIGNAL",
    // Folly / Abseil / LLVM / V8 / Eigen / rapidjson
    "FOLLY_ALWAYS_INLINE",
    "FOLLY_NOINLINE",
    "ABSL_ATTRIBUTE_ALWAYS_INLINE",
    "ABSL_ATTRIBUTE_NOINLINE",
    "LLVM_ATTRIBUTE_ALWAYS_INLINE",
    "LLVM_ATTRIBUTE_NOINLINE",
    "V8_INLINE",
    "V8_NOINLINE",
    "EIGEN_STRONG_INLINE",
    "EIGEN_ALWAYS_INLINE",
    "EIGEN_DEVICE_FUNC",
    "RAPIDJSON_FORCEINLINE",
    // Mozilla / SpiderMonkey
    "MOZ_ALWAYS_INLINE",
    "MOZ_NEVER_INLINE",
    // Protocol Buffers
    "PROTOBUF_ALWAYS_INLINE",
    "PROTOBUF_NOINLINE",
    // {fmt} / spdlog
    "FMT_CONSTEXPR20",
    "FMT_CONSTEXPR",
    "FMT_INLINE",
    // Hedley + nlohmann/json (bundles Hedley)
    "JSON_HEDLEY_ALWAYS_INLINE",
    "JSON_HEDLEY_NEVER_INLINE",
    "HEDLEY_ALWAYS_INLINE",
    "HEDLEY_NEVER_INLINE",
    // GLM (graphics math — pervasive in games/rendering)
    "GLM_FUNC_QUALIFIER",
    "GLM_FUNC_DECL",
    "GLM_CONSTEXPR",
    "GLM_INLINE",
    // Bullet Physics / Skia / OpenCV / EASTL / Cocos2d-x / Chromium-WebKit
    "SIMD_FORCE_INLINE",
    "SK_ALWAYS_INLINE",
    "CV_ALWAYS_INLINE",
    "CV_INLINE",
    "EA_FORCE_INLINE",
    "EA_NOINLINE",
    "CC_INLINE",
    "NEVER_INLINE",
    // C libraries: GLib, SQLite (internal linkage)
    "G_INLINE_FUNC",
    "SQLITE_PRIVATE",
    "SQLITE_API",
    // Windows calling conventions (linkage position — recover the return
    // type; the name is salvaged regardless). Only the unambiguous,
    // non-word-like ones.
    "STDMETHODCALLTYPE",
    "WINAPIV",
    "WINAPI",
    "APIENTRY",
    // Common cross-ecosystem inline/attribute hints
    "ALWAYS_INLINE",
    "FORCE_INLINE",
    "NOINLINE",
];

/// One alternation, longest token first so a longer macro wins over a prefix
/// (the TS `sort by length desc` spelling; both sorts are stable).
static CPP_INLINE_MACRO_RE: LazyLock<Regex> = LazyLock::new(|| {
    let mut tokens = CPP_INLINE_MACROS;
    tokens.sort_by_key(|t| std::cmp::Reverse(t.len()));
    #[allow(clippy::unwrap_used)] // identifiers joined by `|` — known good
    Regex::new(&format!(r"\b(?:{})\b", tokens.join("|"))).unwrap()
});
static AFTER_IDENT_RE: LazyLock<Regex> = literal_regex!(r"^\s+[A-Za-z_]");

/// Blank a known inline-specifier macro sitting in front of a function's
/// return type (`FORCEINLINE FString GetName(…)`) — pervasive in Unreal
/// Engine and vendored libraries; without the blank the macro becomes the
/// return type and the real return type is glued onto the name. Curated
/// exact tokens only (a real return type like `HRESULT DoIt()` is never
/// touched), specifier position only (the `\s+[A-Za-z_]` after-check).
pub(crate) fn blank_cpp_inline_macros(source: &str) -> Cow<'_, str> {
    if !CPP_INLINE_MACROS.iter().any(|m| source.contains(m)) {
        return Cow::Borrowed(source);
    }
    let mut bytes: Option<Vec<u8>> = None;
    let mut at = 0;
    while let Some(m) = CPP_INLINE_MACRO_RE.find_at(source, at) {
        at = m.end();
        if !AFTER_IDENT_RE.is_match(&source[m.end()..]) {
            continue;
        }
        blank_bytes(
            bytes.get_or_insert_with(|| source.as_bytes().to_vec()),
            m.range(),
            true,
        );
    }
    match bytes {
        Some(b) => into_string(b, source),
        None => Cow::Borrowed(source),
    }
}

static CPP_API_PREFIX_RE: LazyLock<Regex> =
    literal_regex!(r"\b[A-Z][A-Z0-9_]*(?:_API|_EXPORT|_ABI)\b");
static CPP_API_GATE_RE: LazyLock<Regex> = literal_regex!(r"_(?:API|EXPORT|ABI)\b");

/// Blank an export/visibility macro in front of a *member*/method
/// declaration (`ENGINE_API virtual void Tick(…)`): ALL-CAPS with the
/// conventional `_API`/`_EXPORT`/`_ABI` suffix, followed by whitespace and a
/// declaration token. Value uses (`x == FOO_API`) fail the after-check.
/// C++-only (wired by Task 13).
pub(crate) fn blank_cpp_api_prefix_macros(source: &str) -> Cow<'_, str> {
    if !CPP_API_GATE_RE.is_match(source) {
        return Cow::Borrowed(source);
    }
    let mut bytes: Option<Vec<u8>> = None;
    let mut at = 0;
    while let Some(m) = CPP_API_PREFIX_RE.find_at(source, at) {
        at = m.end();
        if !AFTER_IDENT_RE.is_match(&source[m.end()..]) {
            continue;
        }
        blank_bytes(
            bytes.get_or_insert_with(|| source.as_bytes().to_vec()),
            m.range(),
            true,
        );
    }
    match bytes {
        Some(b) => into_string(b, source),
        None => Cow::Borrowed(source),
    }
}

// =============================================================================
// Balanced-paren annotation blankers (string-aware byte scans)
// =============================================================================

/// From `open_paren` (the byte index of `(`), scan to the matching close
/// paren, skipping string/char literals (`\` escapes honored) so an embedded
/// `)` can't mis-close the balance. Returns the index one PAST the closing
/// paren, or `None` when unbalanced. Byte-driven: quote/paren bytes are
/// ASCII, and UTF-8 continuation bytes can never equal them.
fn balanced_paren_end(bytes: &[u8], open_paren: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut i = open_paren;
    while i < bytes.len() {
        match bytes[i] {
            b'"' | b'\'' => {
                let quote = bytes[i];
                i += 1;
                while i < bytes.len() && bytes[i] != quote {
                    if bytes[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            b'(' => depth += 1,
            b')' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(i + 1);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

static CPP_INLINE_ANNOTATION_RE: LazyLock<Regex> =
    literal_regex!(r"\b(?:UMETA|UPARAM|UE_DEPRECATED\w*)\s*\(");
static CPP_INLINE_ANNOTATION_GATE_RE: LazyLock<Regex> =
    literal_regex!(r"\b(?:UMETA|UPARAM|UE_DEPRECATED)");

/// Blank an Unreal-Engine annotation macro appearing MID-LINE inside a
/// declaration — an enum value's `UMETA(…)`, a parameter's `UPARAM(ref)`, a
/// deprecation tag inside a `using` (`World.h`'s `UWorld` collapse). Keyed
/// on the UE-exclusive name list, balanced parens, string-aware. The
/// line-LEADING forms are [`blank_cpp_annotation_macro_calls`]' job.
pub(crate) fn blank_cpp_inline_annotation_macros(source: &str) -> Cow<'_, str> {
    if !CPP_INLINE_ANNOTATION_GATE_RE.is_match(source) {
        return Cow::Borrowed(source);
    }
    let src_bytes = source.as_bytes();
    let mut bytes: Option<Vec<u8>> = None;
    let mut at = 0;
    while let Some(m) = CPP_INLINE_ANNOTATION_RE.find_at(source, at) {
        let Some(end) = balanced_paren_end(src_bytes, m.end() - 1) else {
            at = m.end(); // unbalanced — TS leaves lastIndex after the match
            continue;
        };
        blank_bytes(
            bytes.get_or_insert_with(|| source.as_bytes().to_vec()),
            m.start()..end,
            true,
        );
        at = end;
    }
    match bytes {
        Some(b) => into_string(b, source),
        None => Cow::Borrowed(source),
    }
}

static CPP_ANNOTATION_CALL_RE: LazyLock<Regex> =
    literal_regex!(r"(?m)^([ \t]*)[A-Z][A-Z0-9_]{2,}\s*\(");
static CPP_ANNOTATION_CALL_GATE_RE: LazyLock<Regex> =
    literal_regex!(r"(?m)^[ \t]*[A-Z][A-Z0-9_]{2,}\s*\(");

/// Blank annotation-style macro invocations that decorate a declaration but
/// carry NO terminating semicolon — Unreal reflection markup (`UPROPERTY`,
/// `UFUNCTION`, `GENERATED_BODY`, …), whose accumulated parse errors can
/// collapse a whole class (#946 territory). Name-list-FREE, keyed on
/// structure: line-leading ALL-CAPS call whose balanced `(...)` is followed
/// by a declaration starter (`[A-Za-z_~#]`) — statement calls (`;`),
/// init-list items (`,`/`{`) and expression fragments (operators) are all
/// rejected. C++-only (wired by Task 13).
pub(crate) fn blank_cpp_annotation_macro_calls(source: &str) -> Cow<'_, str> {
    if !CPP_ANNOTATION_CALL_GATE_RE.is_match(source) {
        return Cow::Borrowed(source);
    }
    let src_bytes = source.as_bytes();
    let mut bytes: Option<Vec<u8>> = None;
    let mut at = 0;
    while let Some(caps) = CPP_ANNOTATION_CALL_RE.captures_at(source, at) {
        let Some(full) = caps.get(0) else { break };
        let indent_len = caps.get(1).map_or(0, |g| g.len());
        let macro_start = full.start() + indent_len;
        let Some(end) = balanced_paren_end(src_bytes, full.end() - 1) else {
            at = full.end();
            continue;
        };
        // The char after the balanced parens (whitespace skipped) must START
        // A DECLARATION — a letter, `_`, `~` (destructor), or `#`.
        let mut j = end;
        while j < src_bytes.len() && src_bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        let starts_declaration = src_bytes
            .get(j)
            .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_' || *b == b'~' || *b == b'#');
        if !starts_declaration {
            at = full.end();
            continue;
        }
        blank_bytes(
            bytes.get_or_insert_with(|| source.as_bytes().to_vec()),
            macro_start..end,
            true,
        );
        at = end;
    }
    match bytes {
        Some(b) => into_string(b, source),
        None => Cow::Borrowed(source),
    }
}

// =============================================================================
// Metal / CUDA dialect blankers
// =============================================================================

/// `[[attribute]]`, `[[ident(args)]]`, and comma-separated lists — after
/// `[[` a lambda subscript continues with `]`, never an identifier followed
/// by `]]`, so `arr[[]{ … }()]` can never match. `.metal`-only (wired by
/// Task 13): in regular C++ the pre-declarator attribute position is legal
/// syntax the grammar parses natively. (#1121)
static METAL_ATTRIBUTE_RE: LazyLock<Regex> = literal_regex!(
    r"\[\[\s*[A-Za-z_]\w*(?:\s*\([^()\n]*\))?(?:\s*,\s*[A-Za-z_]\w*(?:\s*\([^()\n]*\))?)*\s*\]\]"
);

/// Blank Metal Shading Language `[[attribute]]` annotations (post-declarator
/// positions tree-sitter-cpp can't reconcile — a misparsed struct field
/// emits a spurious `extends` to the field's type). See
/// [`METAL_ATTRIBUTE_RE`] for the tight shape.
pub(crate) fn blank_metal_attributes(source: &str) -> Cow<'_, str> {
    if !source.contains("[[") {
        return Cow::Borrowed(source);
    }
    let mut bytes: Option<Vec<u8>> = None;
    for m in METAL_ATTRIBUTE_RE.find_iter(source) {
        blank_bytes(
            bytes.get_or_insert_with(|| source.as_bytes().to_vec()),
            m.range(),
            true,
        );
    }
    match bytes {
        Some(b) => into_string(b, source),
        None => Cow::Borrowed(source),
    }
}

static CUDA_LAUNCH_BOUNDS_RE: LazyLock<Regex> =
    literal_regex!(r"\b__launch_bounds__\s*\([^()\n]*\)");
/// `__restrict__` is deliberately absent: the grammar parses it natively as
/// a type_qualifier.
static CUDA_SPECIFIER_RE: LazyLock<Regex> = literal_regex!(
    r"\b__(?:global|device|host|constant|shared|managed|grid_constant|forceinline|noinline|launch_bounds)__\b"
);
/// `;` stays excluded (launch configs are expressions; a stray `<<<`
/// spanning real statements always crosses one) and the span is capped.
/// Braces pass the regex — `k<<<dim3{1,1,1}, …>>>` is a real launch shape —
/// but the replacer only blanks a BALANCED match, so a merge conflict's
/// `<<<<<<< … >>>>>>>` region stays untouched.
static CUDA_LAUNCH_CONFIG_RE: LazyLock<Regex> = literal_regex!(r"<<<[^;]{0,400}?>>>");

/// Blank CUDA constructs before parsing with the C++ grammar (#387):
/// execution-space/storage dunder specifiers, `__launch_bounds__(…)`, and
/// kernel-launch configs `k<<<grid, block>>>(args)` (whose chevrons
/// otherwise lex as shifts and lose the host→kernel call edge — the main
/// reason to index CUDA at all). Wired by Task 13 for `.cu`/`.cuh` and for
/// any C/C++-family file [`looks_like_cuda_source`] flags.
pub(crate) fn blank_cuda_constructs(source: &str) -> Cow<'_, str> {
    let mut bytes: Option<Vec<u8>> = None;
    if source.contains("__") {
        for re in [&CUDA_LAUNCH_BOUNDS_RE, &CUDA_SPECIFIER_RE] {
            for m in re.find_iter(source) {
                blank_bytes(
                    bytes.get_or_insert_with(|| source.as_bytes().to_vec()),
                    m.range(),
                    true,
                );
            }
        }
    }
    if source.contains("<<<") {
        for m in CUDA_LAUNCH_CONFIG_RE.find_iter(source) {
            // Balance check: blank only when every `{` closes (the TS
            // replacer's guard).
            let mut depth = 0i64;
            let mut balanced = true;
            for b in source[m.range()].bytes() {
                if b == b'{' {
                    depth += 1;
                } else if b == b'}' {
                    depth -= 1;
                    if depth < 0 {
                        balanced = false;
                        break;
                    }
                }
            }
            if balanced && depth == 0 {
                // TS spelling here: only `\n` survives (`\r` becomes a space).
                blank_bytes(
                    bytes.get_or_insert_with(|| source.as_bytes().to_vec()),
                    m.range(),
                    false,
                );
            }
        }
    }
    match bytes {
        Some(b) => into_string(b, source),
        None => Cow::Borrowed(source),
    }
}

/// Strong content markers for CUDA source in files without a CUDA extension
/// (headers — cutlass/flash-attention/llm.c keep kernels and launchers in
/// `.h`). Deliberately excludes weak markers (`dim3`, `<<<`) that could
/// plausibly appear in non-CUDA text.
pub(crate) fn looks_like_cuda_source(source: &str) -> bool {
    source.contains("__global__")
        || source.contains("__device__")
        || source.contains("__constant__")
        || source.contains("cudaStream_t")
}

// =============================================================================
// C++ name/type recovery
// =============================================================================

/// Bare C/C++ type/qualifier tokens that must never be taken as a recovered
/// function name (guards [`recover_mangled_cpp_name`] against the
/// `Ret (name)` idiom). Copied verbatim from `CPP_PRIMITIVE_NAMES` (23).
const CPP_PRIMITIVE_NAMES: [&str; 23] = [
    "bool", "void", "int", "char", "short", "long", "float", "double", "unsigned", "signed",
    "wchar_t", "char8_t", "char16_t", "char32_t", "char_t", "size_t", "auto", "const", "struct",
    "class", "enum", "union", "typename",
];

static RET_PAREN_NAME_RE: LazyLock<Regex> = literal_regex!(r"^\S+\s+\([A-Za-z_]\w*\)");
static IDENT_RE: LazyLock<Regex> = literal_regex!(r"^[A-Za-z_]\w*$");

/// Universal fallback (any macro, no list) for a C/C++ function name still
/// mangled because an unblanked macro sat before the return type (`Ret
/// name`, `char_t* to_str(double v)`): recover the token immediately before
/// the parameter list. Only touches an ALREADY-mangled name (internal
/// whitespace, not `operator …`/destructor), leaves the ambiguous
/// `Ret (name)` idiom alone, and rejects bare primitives/keywords.
pub(crate) fn recover_mangled_cpp_name(name: String) -> String {
    if !name.contains(char::is_whitespace) || name.starts_with("operator") || name.starts_with('~')
    {
        return name;
    }
    if RET_PAREN_NAME_RE.is_match(&name) {
        return name; // `Ret (name)` idiom — leave alone
    }
    let before_params = match name.find('(') {
        Some(i) => &name[..i],
        None => &name,
    };
    let Some(candidate) = before_params.split_whitespace().last() else {
        return name;
    };
    if !IDENT_RE.is_match(candidate) || CPP_PRIMITIVE_NAMES.contains(&candidate) {
        return name;
    }
    candidate.to_string()
}

/// Built-in return types that can't be a method receiver — copied verbatim
/// from `CPP_NON_CLASS_RETURN` (28).
const CPP_NON_CLASS_RETURN: [&str; 28] = [
    "void",
    "bool",
    "char",
    "short",
    "int",
    "long",
    "float",
    "double",
    "unsigned",
    "signed",
    "size_t",
    "ssize_t",
    "auto",
    "wchar_t",
    "char8_t",
    "char16_t",
    "char32_t",
    "int8_t",
    "int16_t",
    "int32_t",
    "int64_t",
    "uint8_t",
    "uint16_t",
    "uint32_t",
    "uint64_t",
    "intptr_t",
    "uintptr_t",
    "nullptr_t",
];

static CPP_SMART_PTR_RE: LazyLock<Regex> = literal_regex!(
    r"\b(?:std\s*::\s*)?(?:unique_ptr|shared_ptr|weak_ptr|optional)\s*<\s*([^,>]+?)\s*>"
);
static CPP_CV_RE: LazyLock<Regex> =
    literal_regex!(r"\b(?:const|volatile|typename|struct|class|enum)\b");
static CPP_TEMPLATE_ARGS_RE: LazyLock<Regex> = literal_regex!(r"<[^>]*>");
static CPP_REF_PTR_RE: LazyLock<Regex> = literal_regex!(r"[*&]+");
static WS_RUN_RE: LazyLock<Regex> = literal_regex!(r"\s+");

/// Normalize a C++ return type to the bare class name a chained `->method()`
/// could be called on (#645/#608 mechanism): unwrap smart-pointer/optional
/// wrappers to the pointee, strip cv-qualifiers / template args / `*&` /
/// namespace qualifiers; `None` for primitives, `void`, `auto`, empty.
pub(crate) fn normalize_cpp_return_type(raw: &str) -> Option<String> {
    let mut t = raw.trim().to_string();
    if t.is_empty() {
        return None;
    }
    if let Some(caps) = CPP_SMART_PTR_RE.captures(&t)
        && let Some(inner) = caps.get(1)
    {
        t = inner.as_str().to_string();
    }
    let t = CPP_CV_RE.replace_all(&t, " ");
    let t = CPP_TEMPLATE_ARGS_RE.replace_all(&t, " ");
    let t = CPP_REF_PTR_RE.replace_all(&t, " ");
    let t = WS_RUN_RE.replace_all(&t, " ");
    let t = t.trim();
    if t.is_empty() {
        return None;
    }
    let last = t.rsplit("::").find(|s| !s.is_empty())?;
    let last = last.trim();
    if CPP_NON_CLASS_RETURN.contains(&last) || !IDENT_RE.is_match(last) {
        return None;
    }
    Some(last.to_string())
}

/// Strip every balanced `<…>` group from a base-type reference name so it
/// matches the bare class the template was DEFINED as (#1043):
/// `Base<int>` → `Base`, `ns::Tpl<Foo<int>>` → `ns::Tpl`,
/// `Outer<int>::Inner` → `Outer::Inner`.
pub(crate) fn strip_cpp_template_args(name: &str) -> Cow<'_, str> {
    if !name.contains('<') {
        return Cow::Borrowed(name);
    }
    let mut out = String::with_capacity(name.len());
    let mut depth = 0usize;
    for ch in name.chars() {
        match ch {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(ch),
            _ => {}
        }
    }
    Cow::Owned(out.trim().to_string())
}

// =============================================================================
// C# / VB.NET
// =============================================================================

static CSHARP_DIRECTIVE_RE: LazyLock<Regex> =
    literal_regex!(r"(?m)^([ \t]*)#[ \t]*(?:if|elif|else|endif)\b[^\n]*");

/// Blank C# conditional-compilation directive lines (`#if`/`#elif`/`#else`/
/// `#endif`) before parsing (#237): the grammar misparses a `#if` inside an
/// enum member list, detaching the enclosing class's members. Both branches
/// are kept (the right default for a code graph — index every symbol
/// regardless of build flags). `#region`/`#pragma`/`#nullable` parse fine
/// and are left alone. Indentation survives; byte offsets exact.
pub(crate) fn blank_csharp_preprocessor_directives(source: &str) -> Cow<'_, str> {
    if !source.contains('#') {
        return Cow::Borrowed(source);
    }
    let mut bytes: Option<Vec<u8>> = None;
    for caps in CSHARP_DIRECTIVE_RE.captures_iter(source) {
        let Some(full) = caps.get(0) else { continue };
        let indent_len = caps.get(1).map_or(0, |g| g.len());
        blank_bytes(
            bytes.get_or_insert_with(|| source.as_bytes().to_vec()),
            full.start() + indent_len..full.end(),
            true,
        );
    }
    match bytes {
        Some(b) => into_string(b, source),
        None => Cow::Borrowed(source),
    }
}

/// Append a trailing newline when absent (VB.NET, wave 2 — its grammar
/// drops a final statement without one). The one length-CHANGING transform:
/// safe because the byte is appended at EOF, after every position that
/// feeds a node id.
pub(crate) fn ensure_trailing_newline(source: &str) -> Cow<'_, str> {
    if source.ends_with('\n') {
        Cow::Borrowed(source)
    } else {
        Cow::Owned(format!("{source}\n"))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    /// Every blanker's hard invariant: byte length identical, newline bytes
    /// (`\n`, and `\r` unless the function documents otherwise) in identical
    /// positions, and every non-blanked byte identical to the input.
    fn assert_byte_preserving(before: &str, after: &str) {
        assert_eq!(before.len(), after.len(), "byte length must not change");
        for (i, (b, a)) in before.bytes().zip(after.bytes()).enumerate() {
            if b == b'\n' {
                assert_eq!(a, b'\n', "newline at byte {i} must survive");
            }
            if a != b {
                assert_eq!(a, b' ', "byte {i} may only change into a space");
            }
        }
    }

    // ---- blank_cpp_export_macros -------------------------------------------

    #[test]
    fn export_macro_blanked_in_class_definition_header() {
        let src = "class MYMODULE_API Widget : public Base {\n};\n";
        let out = blank_cpp_export_macros(src);
        assert_byte_preserving(src, &out);
        assert!(out.contains("class"));
        assert!(out.contains("Widget : public Base"));
        assert!(!out.contains("MYMODULE_API"));
        // `struct MACRO Name {` form too.
        let s2 = "struct CORE_EXPORT Vec final {\n};\n";
        let o2 = blank_cpp_export_macros(s2);
        assert!(!o2.contains("CORE_EXPORT"));
        assert!(o2.contains("Vec final {"));
    }

    #[test]
    fn export_macro_leaves_variable_declarations_and_values_alone() {
        // Elaborated-type declarations end in `;`/`=`, never `:`/`{`.
        for src in [
            "struct FOO var;\n",
            "class FOO obj = make();\n",
            "int x = SOME_API;\n",
        ] {
            let out = blank_cpp_export_macros(src);
            assert_eq!(&*out, src, "must not touch: {src}");
        }
    }

    // ---- blank_cpp_inline_macros -------------------------------------------

    #[test]
    fn inline_macro_blanked_before_return_type() {
        let src = "FORCEINLINE FString GetName() { return N; }\n";
        let out = blank_cpp_inline_macros(src);
        assert_byte_preserving(src, &out);
        assert!(!out.contains("FORCEINLINE"));
        assert!(out.contains("FString GetName()"));
    }

    #[test]
    fn adjacent_inline_macros_are_both_blanked() {
        // The TS lookahead form blanks both; a consuming port would miss the
        // second — this pins the lookahead emulation.
        let src = "Q_INVOKABLE FORCEINLINE FString Get();\n";
        let out = blank_cpp_inline_macros(src);
        assert!(!out.contains("Q_INVOKABLE"));
        assert!(!out.contains("FORCEINLINE"));
        assert!(out.contains("FString Get();"));
    }

    #[test]
    fn inline_macro_respects_word_boundary_and_specifier_position() {
        // Longer word: untouched (word boundary).
        let src = "FORCEINLINE_SOMETHINGELSE int f();\n";
        assert_eq!(&*blank_cpp_inline_macros(src), src);
        // Value/expression position (`? a : b` follows): untouched.
        let src2 = "x = FORCEINLINE ? a : b;\n";
        assert_eq!(&*blank_cpp_inline_macros(src2), src2);
        // Longest token wins over a prefix (FORCEINLINE_DEBUGGABLE vs FORCEINLINE).
        let src3 = "FORCEINLINE_DEBUGGABLE void Tick();\n";
        let out3 = blank_cpp_inline_macros(src3);
        assert!(!out3.contains("FORCEINLINE_DEBUGGABLE"));
        assert!(out3.contains("void Tick();"));
    }

    // ---- blank_cpp_api_prefix_macros ---------------------------------------

    #[test]
    fn api_prefix_macro_blanked_before_member_declarations() {
        let src = "ENGINE_API virtual void Tick(float Dt);\nstatic CORE_EXPORT void Add();\n";
        let out = blank_cpp_api_prefix_macros(src);
        assert_byte_preserving(src, &out);
        assert!(!out.contains("ENGINE_API"));
        assert!(!out.contains("CORE_EXPORT"));
        assert!(out.contains("virtual void Tick"));
    }

    #[test]
    fn api_prefix_macro_value_uses_untouched() {
        for src in ["if (mode == FOO_API) {}\n", "int x = SOME_API;\n"] {
            let out = blank_cpp_api_prefix_macros(src);
            assert_eq!(&*out, src, "must not touch: {src}");
        }
    }

    // ---- blank_cpp_inline_annotation_macros --------------------------------

    #[test]
    fn inline_annotation_macros_blanked_string_aware() {
        // An embedded `)` inside the string must not close the balance early.
        let src = "enum E { A UMETA(DisplayName=\"a)b\"), B };\n";
        let out = blank_cpp_inline_annotation_macros(src);
        assert_byte_preserving(src, &out);
        assert!(!out.contains("UMETA"));
        assert!(!out.contains("DisplayName"));
        assert!(out.contains(", B };"));

        let src2 = "using FOn UE_DEPRECATED(5.5, \"msg\") = TDelegate<void(float)>;\n";
        let out2 = blank_cpp_inline_annotation_macros(src2);
        assert!(!out2.contains("UE_DEPRECATED"));
        assert!(out2.contains("= TDelegate<void(float)>;"));
    }

    #[test]
    fn inline_annotation_multibyte_args_stay_byte_exact() {
        // Multi-byte UTF-8 inside the blanked args: every BYTE becomes one
        // space byte, so all downstream offsets survive.
        let src = "enum E { A UMETA(DisplayName=\"日本語の名前\"), B };\n";
        let out = blank_cpp_inline_annotation_macros(src);
        assert_byte_preserving(src, &out);
        assert!(!out.contains("日本語"));
        assert!(out.contains(", B };"));
    }

    // ---- blank_cpp_annotation_macro_calls ----------------------------------

    #[test]
    fn annotation_macro_calls_blanked_when_followed_by_declaration() {
        let src = "\
class ACharacter {
    UPROPERTY(EditAnywhere, Category=\"Move (x)\")
    float MaxSpeed;
    UFUNCTION(BlueprintCallable)
    void Jump();
};
";
        let out = blank_cpp_annotation_macro_calls(src);
        assert_byte_preserving(src, &out);
        assert!(!out.contains("UPROPERTY"));
        assert!(!out.contains("UFUNCTION"));
        assert!(out.contains("float MaxSpeed;"));
        assert!(out.contains("void Jump();"));
    }

    #[test]
    fn annotation_macro_statement_calls_untouched() {
        // A statement call is followed by `;`, an init-list item by `,`/`{`,
        // an expression fragment by an operator — all rejected.
        for src in [
            "    CHECK(x);\n    int y = 1;\n",
            "    MAKE(a) + 1;\n",
            "    FOO(x), BAR(y);\n",
        ] {
            let out = blank_cpp_annotation_macro_calls(src);
            assert_eq!(&*out, src, "must not touch: {src}");
        }
        // Mid-line (not line-leading) is never matched by THIS blanker.
        let mid = "int x = UPROPERTY(1);\n";
        assert_eq!(&*blank_cpp_annotation_macro_calls(mid), mid);
    }

    #[test]
    fn annotation_macro_stacked_markup_blanked_including_hash_follow() {
        // Markup followed by another directive (`#`) or more markup.
        let src = "UPROPERTY(A)\n#if WITH_EDITOR\nUFUNCTION(B)\nvoid F();\n#endif\n";
        let out = blank_cpp_annotation_macro_calls(src);
        assert!(!out.contains("UPROPERTY"));
        assert!(!out.contains("UFUNCTION"));
        assert!(out.contains("#if WITH_EDITOR"));
        assert!(out.contains("void F();"));
    }

    // ---- blank_metal_attributes --------------------------------------------

    #[test]
    fn metal_attributes_blanked() {
        let src = "struct VertexIn {\n  float3 pos [[attribute(0)]];\n  float4 col [[position]];\n};\nfragment float4 f(VertexIn in [[stage_in]], constant U &u [[buffer(0), raster_order_group(0)]]) {}\n";
        let out = blank_metal_attributes(src);
        assert_byte_preserving(src, &out);
        assert!(!out.contains("[["));
        assert!(out.contains("float3 pos"));
        assert!(out.contains("constant U &u"));
    }

    #[test]
    fn metal_lambda_subscript_untouched() {
        // `arr[[]{ … }()]` — after `[[` a lambda continues with `]`, never an
        // identifier followed by `]]`.
        let src = "int v = arr[[]{ return 1; }()];\n";
        assert_eq!(&*blank_metal_attributes(src), src);
    }

    // ---- blank_cuda_constructs ---------------------------------------------

    #[test]
    fn cuda_specifiers_launch_bounds_and_launch_config_blanked() {
        let src = "__global__ void __launch_bounds__(256) step(float* p) {}\nvoid host() { step<<<grid, block, 0, s>>>(p); }\n__shared__ float tile[256];\n";
        let out = blank_cuda_constructs(src);
        assert_byte_preserving(src, &out);
        assert!(!out.contains("__global__"));
        assert!(!out.contains("__launch_bounds__"));
        assert!(!out.contains("__shared__"));
        assert!(!out.contains("<<<"));
        assert!(out.contains("void host() { step"));
        assert!(out.contains("(p); }"));
    }

    #[test]
    fn cuda_launch_config_balance_and_bounds_guards() {
        // Braced dims are a real launch shape — balanced, blanked.
        let src = "k<<<dim3{1,1,1}, dim3{256,1,1}>>>(x);\n";
        let out = blank_cuda_constructs(src);
        assert!(!out.contains("<<<"));
        // A merge-conflict-like region opening a brace it never closes fails
        // the balance check and stays untouched.
        let src2 = "a <<<one {open\nnever closed>>> b;\n";
        let out2 = blank_cuda_constructs(src2);
        assert!(out2.contains("<<<"));
        // `__restrict__` is grammar-native and deliberately NOT blanked.
        let src3 = "void f(float* __restrict__ p);\n";
        assert_eq!(&*blank_cuda_constructs(src3), src3);
    }

    #[test]
    fn cuda_content_sniff() {
        assert!(looks_like_cuda_source("__global__ void k();"));
        assert!(looks_like_cuda_source("void launch(cudaStream_t s);"));
        assert!(!looks_like_cuda_source("int main() { return 0; }"));
        // Weak markers deliberately excluded.
        assert!(!looks_like_cuda_source("dim3 grid; // <<< in a comment"));
    }

    // ---- recover_mangled_cpp_name ------------------------------------------

    #[test]
    fn recover_mangled_name_cases() {
        // Return type glued onto the name → the token before the params.
        assert_eq!(
            recover_mangled_cpp_name("FString GetName".to_string()),
            "GetName"
        );
        assert_eq!(
            recover_mangled_cpp_name("char_t* to_str(double v)".to_string()),
            "to_str"
        );
        // Clean names, operators, destructors: unchanged.
        assert_eq!(recover_mangled_cpp_name("GetName".to_string()), "GetName");
        assert_eq!(
            recover_mangled_cpp_name("operator ==".to_string()),
            "operator =="
        );
        assert_eq!(recover_mangled_cpp_name("~Widget".to_string()), "~Widget");
        // `Ret (name)` idiom: ambiguous, left alone.
        assert_eq!(
            recover_mangled_cpp_name("int (getter)".to_string()),
            "int (getter)"
        );
        // Candidate is a bare primitive/keyword: unchanged.
        assert_eq!(
            recover_mangled_cpp_name("unsigned int".to_string()),
            "unsigned int"
        );
    }

    // ---- normalize_cpp_return_type -----------------------------------------

    #[test]
    fn normalize_return_type_cases() {
        assert_eq!(
            normalize_cpp_return_type("std::unique_ptr<Widget>").as_deref(),
            Some("Widget")
        );
        assert_eq!(
            normalize_cpp_return_type("shared_ptr<Model>").as_deref(),
            Some("Model")
        );
        assert_eq!(
            normalize_cpp_return_type("const Foo&").as_deref(),
            Some("Foo")
        );
        assert_eq!(
            normalize_cpp_return_type("a::b::Foo").as_deref(),
            Some("Foo")
        );
        assert_eq!(
            normalize_cpp_return_type("Vec<Foo>").as_deref(),
            Some("Vec")
        );
        for prim in ["int", "void", "auto", "size_t", "uint64_t", ""] {
            assert_eq!(normalize_cpp_return_type(prim), None, "primitive: {prim}");
        }
    }

    // ---- strip_cpp_template_args -------------------------------------------

    #[test]
    fn strip_template_args_cases() {
        assert_eq!(&*strip_cpp_template_args("Base<int>"), "Base");
        assert_eq!(&*strip_cpp_template_args("ns::Tpl<Foo<int>>"), "ns::Tpl");
        assert_eq!(
            &*strip_cpp_template_args("Outer<int>::Inner"),
            "Outer::Inner"
        );
        assert_eq!(&*strip_cpp_template_args("Plain"), "Plain");
    }

    // ---- blank_csharp_preprocessor_directives -------------------------------

    #[test]
    fn csharp_conditional_directives_blanked_both_branches_kept() {
        let src = "\
enum ReadType {
#if HAVE_DTO
    ReadAsDateTimeOffset,
#endif
    ReadAsDouble,
}
";
        let out = blank_csharp_preprocessor_directives(src);
        assert_byte_preserving(src, &out);
        assert!(!out.contains("#if"));
        assert!(!out.contains("#endif"));
        assert!(out.contains("ReadAsDateTimeOffset,"));
        assert!(out.contains("ReadAsDouble,"));
        // Indentation before the `#` survives (only the directive is blanked).
        let src2 = "    #if DEBUG\nint x;\n    #endif\n";
        let out2 = blank_csharp_preprocessor_directives(src2);
        assert_byte_preserving(src2, &out2);
        assert!(out2.starts_with("    "));
    }

    #[test]
    fn csharp_non_conditional_directives_untouched() {
        for src in [
            "#region Helpers\nint x;\n#endregion\n",
            "#pragma warning disable 1591\n",
            "#nullable enable\n",
        ] {
            let out = blank_csharp_preprocessor_directives(src);
            assert_eq!(&*out, src, "must not touch: {src}");
        }
    }

    // ---- ensure_trailing_newline -------------------------------------------

    #[test]
    fn ensure_trailing_newline_cases() {
        assert_eq!(&*ensure_trailing_newline("x"), "x\n");
        assert_eq!(&*ensure_trailing_newline("x\n"), "x\n");
        assert_eq!(&*ensure_trailing_newline(""), "\n");
    }
}
