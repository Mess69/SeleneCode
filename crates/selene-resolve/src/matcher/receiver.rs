//! Receiver-type inference (#1108) — recovering a local variable's type from its
//! declaration, so `lg.log()` can resolve when `lg` is not a node at all.
//!
//! Local variables are deliberately **not indexed** (node explosion), so the only
//! way to learn a receiver's type is to read the enclosing function's source and
//! match its declaration. That is what this module does, per language.
//!
//! # The patterns are allowed to be loose, because inference is VALIDATED
//!
//! Every type this module returns is handed to
//! [`crate::matcher::method::resolve_method_on_type`], which requires the method
//! to actually exist on that type. **A mis-inference therefore yields no edge,
//! never a wrong one** — and that safety net is exactly what lets the patterns
//! below stay simple regexes instead of a type checker.
//!
//! # Bounds that are not optional
//!
//! - The scan runs **backward from the call line to the enclosing function's
//!   start**, so a same-named variable in another function cannot leak in.
//!   *Nearest declaration wins.*
//! - Lines longer than **10 000 chars** are skipped: a minified/generated line is
//!   not where a human-written local declaration lives, and regexing it per
//!   reference is pure waste (#1122).
//! - Patterns are **compiled once and cached** — the spike (F6) measured ~10 ms
//!   per compile, and this runs for every `receiver.method()` reference in the
//!   repo.

use std::sync::{Arc, LazyLock};

use fancy_regex::Regex;
use selene_core::{Language, Node, NodeKind, UnresolvedRef};

use crate::cache::SyncLru;
use crate::context::ResolutionContext;

/// Tokens a loose pattern might capture that are never a user-defined type.
const NON_TYPE_RECEIVER_TOKENS: [&str; 16] = [
    "this",
    "self",
    "super",
    "new",
    "return",
    "await",
    "yield",
    "typeof",
    "null",
    "nil",
    "None",
    "true",
    "false",
    "True",
    "False",
    "undefined",
];

/// C++ keywords that can sit right before a receiver (`return ptr->m()`) and must
/// never be read as its type.
const CPP_NON_TYPE_TOKENS: [&str; 23] = [
    "return",
    "if",
    "else",
    "for",
    "while",
    "do",
    "switch",
    "case",
    "default",
    "break",
    "continue",
    "goto",
    "throw",
    "new",
    "delete",
    "co_await",
    "co_yield",
    "co_return",
    "static_cast",
    "const_cast",
    "dynamic_cast",
    "reinterpret_cast",
    "sizeof",
];

/// Compiled receiver patterns, keyed by the pattern string.
///
/// The spike (F6) measured a single receiver pattern at **~10 ms to compile**, and
/// these are built **per reference** from an escaped receiver name (the TS `new
/// RegExp` shape). Naive compilation would dominate resolution wall-clock outright
/// on any real repo — 400 references cost 4.6 s naive vs 0.22 s cached.
static PATTERN_CACHE: LazyLock<SyncLru<String, Option<Arc<Regex>>>> =
    LazyLock::new(|| SyncLru::new(2_048));

/// A compiled pattern, from the cache when possible.
///
/// `None` for a pattern that fails to compile — a receiver name that escapes into
/// something unparseable degrades that one lookup, it never fails a run.
fn compiled(pattern: &str) -> Option<Arc<Regex>> {
    PATTERN_CACHE.get_or_insert_with(pattern.to_string(), || {
        Regex::new(pattern).ok().map(Arc::new)
    })
}

/// Capture group 1 of the first pattern that matches `line`.
fn capture(patterns: &[String], line: &str) -> Option<String> {
    for p in patterns {
        let Some(re) = compiled(p) else { continue };
        if let Ok(Some(caps)) = re.captures(line)
            && let Some(m) = caps.get(1)
        {
            return Some(m.as_str().to_string());
        }
    }
    None
}

/// Regex-escape a receiver name before it is spliced into a pattern.
fn escape(receiver: &str) -> String {
    fancy_regex::escape(receiver).into_owned()
}

/// Normalize a captured type expression to a simple type name: drop generic args
/// and pointer/ref markers, take the last `.`/`::` segment, reject obvious
/// non-types.
pub fn normalize_inferred_type_name(raw: &str) -> Option<String> {
    let no_generics = strip_angle_brackets(raw);
    let cleaned: String = no_generics
        .chars()
        .filter(|c| *c != '&' && *c != '*')
        .collect();
    let seg = cleaned
        .split(['.', ':'])
        .rfind(|s| !s.is_empty())?
        .trim()
        .to_string();
    if seg.is_empty() || NON_TYPE_RECEIVER_TOKENS.contains(&seg.as_str()) {
        return None;
    }
    Some(seg)
}

/// Remove `<...>` spans (generics) from a type expression.
fn strip_angle_brackets(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut depth = 0usize;
    for ch in s.chars() {
        match ch {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(ch),
            _ => {}
        }
    }
    out
}

// =============================================================================
// The per-language pattern tables (ported verbatim, name-matcher.ts:1092–1237)
// =============================================================================

/// Per-language patterns that recover a local variable's (or typed parameter's)
/// type from its declaration. Group 1 captures the type; `r` is the **already
/// escaped** receiver name. Ordered most-specific first.
///
/// PascalCase is required in the capture wherever the language's convention
/// allows it — a cheap false-positive guard *on top of* `resolve_method_on_type`'s
/// validation, not instead of it.
///
/// Wave-2 languages (Swift, Scala, Dart, R, Pascal, CFML) have their rows in the
/// map and land with their extractors in Phase 8 — a reference in them cannot
/// exist yet, so porting their patterns now would be untestable dead code.
fn local_receiver_patterns(language: Language, r: &str) -> Vec<String> {
    match language {
        Language::Typescript
        | Language::Javascript
        | Language::Tsx
        | Language::Jsx
        | Language::Arkts => vec![
            // `lg = new Logger()`
            format!(r"\b{r}\b\s*=\s*new\s+([A-Za-z_$][\w.$]*)"),
            // `lg: Logger` — an annotation OR a typed parameter (#1125). No
            // keyword prefix, so `function use(lg: Logger)` and `(lg: Logger) =>`
            // match too. The capture stops at `<`, so `repo: Repository<User>`
            // still yields `Repository`.
            format!(r"\b{r}\b\s*:\s*([A-Z][\w.$]*)"),
        ],
        Language::Python => vec![
            format!(r"\b{r}\b\s*=\s*([A-Z][\w.]*)\s*\("), // lg = Logger(...)
            format!(r"\b{r}\b\s*:\s*([A-Z][\w.]*)"),      // lg: Logger  (PEP 526)
        ],
        Language::Java => vec![
            format!(r"\b{r}\b\s*=\s*new\s+([A-Za-z_][\w.]*)"), // = new Logger()
            format!(r"\b([A-Z][\w.]*)\s+{r}\b\s*[=;,)]"),      // Logger lg;  / param
        ],
        Language::Kotlin => vec![
            format!(r"\b{r}\b\s*=\s*([A-Z][\w.]*)\s*\("), // val lg = Logger(...)
            format!(r"\b{r}\b\s*:\s*([A-Z][\w.]*)"),      // val lg: Logger / param
        ],
        Language::CSharp => vec![
            format!(r"\b{r}\b\s*=\s*new\s+([A-Za-z_][\w.]*)"),
            format!(r"\b([A-Z][\w.]*)\s+{r}\b\s*[=;,)]"),
        ],
        Language::Rust => vec![
            // `let lg: Logger = …` / `let lg=Logger::new()`.
            //
            // ⚠ KNOWN GAP, ported VERBATIM from the TS source (name-matcher.ts:1135):
            // there is no `\s*` before the `=`, so the single most common Rust idiom
            // — `let lg = Logger::new();` **with a space** — does NOT match this
            // pattern, and (having no `:` annotation) does not match the second one
            // either. Such a receiver falls through to the weaker name strategies.
            //
            // This is a recall bug in the TS build, not a design decision, and it is
            // carried deliberately: adding the `\s*` would emit Rust edges the TS
            // build does not have, which the Part C parity gate would then have to
            // explain away. Fixing it is a one-character change plus a real-repo A/B —
            // a Phase 9 parity decision, made with evidence, not a silent divergence
            // here. Pinned by `the_rust_let_pattern_gap_is_carried_deliberately`.
            format!(r"\blet\s+(?:mut\s+)?{r}\b(?:\s*:[^=]+)?=\s*&?(?:mut\s+)?([A-Z][\w]*)"),
            // No `let`, so a typed parameter (`fn use(lg: &Logger)`, `|lg: Logger|`)
            // matches too (#1125).
            format!(r"\b{r}\s*:\s*&?(?:mut\s+)?([A-Z][\w]*)"),
        ],
        Language::Go => vec![
            format!(r"\b{r}\b\s*:=\s*&?([A-Za-z_][\w.]*)\s*\{{"), // lg := Logger{} / &Logger{}
            format!(r"\bvar\s+{r}\s+\*?([A-Za-z_][\w.]*)"),       // var lg Logger / *Logger
            // A typed parameter or method receiver (`func use(lg Logger)`,
            // `func (l Logger) M()`) — name-before-type with no `var`/`:=` (#1125).
            // PascalCase-guarded, unlike the anchored patterns above, to keep the
            // keyword-free `ident Type` shape from matching unrelated pairs.
            format!(r"\b{r}\s+\*?([A-Z][\w.]*)"),
        ],
        Language::Ruby => vec![
            format!(r"\b{r}\b\s*=\s*([A-Z][\w:]*)\.new\b"), // lg = Logger.new
        ],
        Language::Php => vec![
            format!(r"\$?{r}\b\s*=\s*new\s+([A-Za-z_\\][\w\\]*)"), // $lg = new Logger()
            // A typed parameter (`function use(Logger $lg)`, `?Logger $lg`,
            // `\App\Logger $lg`, by-ref `&$lg`) and a typed `catch (E $e)` — the
            // type sits before the `$` variable (#1125).
            format!(r"\b([A-Za-z_\\][\w\\]*)\s+&?\${r}\b"),
        ],
        Language::Lua | Language::Luau => vec![
            format!(r"\b{r}\b\s*=\s*([A-Z][\w]*)\.new\b"), // local lg = Logger.new()
            format!(r"\b{r}\b\s*=\s*([A-Z][\w]*)\s*\("),   // local lg = Logger(...)
            // ⚠ The one pattern in the codebase that needs LOOKAHEAD — and the
            // entire reason `fancy-regex` is a dependency (spike F5c).
            //
            // Lua's method-call syntax is the IDENTICAL `receiver:Name` shape as a
            // type annotation, and the backward scan starts on the call's own
            // line — so without a gate, any PascalCase method call (`lg:Log()`,
            // the Roblox convention) self-matches as "type = Log" before the scan
            // ever reaches the real declaration (#1124). The lookahead rejects a
            // capture followed by any of Lua's three call forms — `(args)`,
            // `"s"`/`'s'`/`[[s]]`, `{t}` — and its leading `[\w.]` alternative
            // stops backtracking from shrinking the capture to dodge the gate
            // (`lg:Log()` would otherwise still match, as `Lo`).
            format!(r#"\b{r}\b\s*:\s*([A-Z][\w.]*)(?![\w.]|\s*[({{"'\[])"#),
        ],
        _ => Vec::new(),
    }
}

/// The patterns that recover a **PHP class property's** declared type for a
/// `$this->prop` receiver.
///
/// Deliberately **not** the local patterns: only property-shaped declarations
/// qualify. A bare `X $prop` parameter, or a `$prop = new X()` local in some other
/// method, can never alias `$this->prop` — typing the property from those would be
/// a *wrong* 0.9-confidence edge, not a missing one. A union-typed property
/// (`Foo|Bar $prop`) matches nothing and therefore produces no edge: silent beats
/// wrong.
fn php_property_patterns(r: &str) -> Vec<String> {
    vec![
        // `private readonly ?Foo $prop` — a typed property or a promoted ctor param.
        format!(
            r"\b(?:(?:private|protected|public|readonly|static|final)(?:\(set\))?\s+)+\??([A-Za-z_\\][\w\\]*)\s+&?\${r}\b"
        ),
        // The pseudoconstructor assignment.
        format!(r"\$this->{r}\b\s*=\s*new\s+([A-Za-z_\\][\w\\]*)"),
    ]
}

// =============================================================================
// The shared inferrer
// =============================================================================

/// The 1-based start line of the **tightest** function/method enclosing the call.
fn enclosing_scope_start_line<C: ResolutionContext>(r: &UnresolvedRef, ctx: &C) -> u32 {
    let Some(line) = r.line else { return 1 };
    let mut start = 1u32;
    for n in ctx.nodes_in_file(&r.file_path) {
        if !matches!(n.kind, NodeKind::Function | NodeKind::Method) || n.language != r.language {
            continue;
        }
        let end = n.end_line.max(n.start_line);
        if n.start_line <= line && end >= line && n.start_line >= start {
            start = n.start_line;
        }
    }
    start
}

/// Infer a receiver's type from its declaration in the enclosing scope.
///
/// `None` for a language with no patterns, or when no declaration is found — and
/// `None` means *no edge*, which is the correct outcome.
pub fn infer_local_receiver_type<C: ResolutionContext>(
    receiver: &str,
    r: &UnresolvedRef,
    ctx: &C,
) -> Option<String> {
    let lang = Language::from_wire(&r.language)?;

    // A PHP `$this->prop` receiver: the property's declaration lives OUTSIDE the
    // calling method (a promoted ctor param, a typed property, or a classic ctor
    // assignment), so strip the prefix, widen the scan to the whole file — and
    // switch to PROPERTY-shaped patterns, because a plain `$prop` local lives in a
    // different namespace and can never shadow `$this->prop`.
    let (scan_receiver, whole_file, php_property) = if lang == Language::Php
        && let Some(prop) = receiver.strip_prefix("this->")
    {
        (prop.to_string(), true, true)
    } else {
        (receiver.to_string(), false, false)
    };
    // (Wave 2: CFML's `variables.`/`this.`/`local.` scope prefixes strip the same
    // way and set `whole_file`.)

    let escaped = escape(&scan_receiver);
    let patterns = if php_property {
        php_property_patterns(&escaped)
    } else {
        local_receiver_patterns(lang, &escaped)
    };
    if patterns.is_empty() {
        return None;
    }

    let lines = ctx.file_lines(&r.file_path)?;
    if lines.is_empty() {
        return None;
    }

    let call_idx = (r.line.unwrap_or(1).saturating_sub(1) as usize).min(lines.len() - 1);
    let start_idx = if whole_file {
        0
    } else {
        (enclosing_scope_start_line(r, ctx).saturating_sub(1)) as usize
    };

    let match_line = |i: usize| -> Option<String> {
        let line = lines.get(i)?;
        // A generated/minified line (one multi-KB statement) is not where a
        // human-written declaration lives, and regexing it per reference is pure
        // waste (#1122).
        if line.len() > 10_000 {
            return None;
        }
        capture(&patterns, line).and_then(|raw| normalize_inferred_type_name(&raw))
    };

    // NEAREST DECLARATION WINS: backward, from the call to the scope's start.
    let mut i = call_idx as isize;
    while i >= start_idx as isize {
        if let Some(t) = match_line(i as usize) {
            return Some(t);
        }
        i -= 1;
    }

    // A field's declaration is position-independent (a constructor may sit *below*
    // the calling method), so a whole-file scan sweeps forward too.
    if whole_file {
        for i in (call_idx + 1)..lines.len() {
            if let Some(t) = match_line(i) {
                return Some(t);
            }
        }
    }

    // Second chance for an untyped PHP property (classic pre-7.4 style): follow
    // `$this->prop = $var` to `$var`'s own typed declaration.
    if php_property {
        return infer_php_assigned_property_type(&escaped, &lines, call_idx);
    }

    None
}

/// A PHP property with no static type may still be typed by what is **assigned**
/// to it: find `$this->prop = $var`, then recover `$var`'s type from its own
/// declaration **within the assigning function**.
///
/// The backward scan stops at the enclosing `function` line (which is itself
/// checked — a single-line `__construct(Foo $var) { … }` carries the typed
/// parameter), so a same-named variable in another method can never type the
/// property.
fn infer_php_assigned_property_type(
    escaped_prop: &str,
    lines: &[String],
    call_idx: usize,
) -> Option<String> {
    let assign = compiled(&format!(r"\$this->{escaped_prop}\b\s*=\s*\$(\w+)"))?;

    // Find the assignment, anywhere in the file.
    let mut assign_idx = None;
    let mut var = String::new();
    for (i, line) in lines.iter().enumerate() {
        if let Ok(Some(caps)) = assign.captures(line)
            && let Some(m) = caps.get(1)
        {
            assign_idx = Some(i);
            var = m.as_str().to_string();
            break;
        }
    }
    let assign_idx = assign_idx?;
    let _ = call_idx; // the assignment's own scope is what bounds the scan

    // Type `$var` from its declaration, bounded by the enclosing `function` line.
    let escaped_var = escape(&var);
    let decl = vec![
        format!(r"\b([A-Za-z_\\][\w\\]*)\s+&?\${escaped_var}\b"), // a typed parameter
        format!(r"\${escaped_var}\b\s*=\s*new\s+([A-Za-z_\\][\w\\]*)"), // a local `new`
    ];
    let function_line = compiled(r"\bfunction\b")?;

    let mut i = assign_idx as isize;
    while i >= 0 {
        let line = &lines[i as usize];
        if let Some(t) = capture(&decl, line).and_then(|raw| normalize_inferred_type_name(&raw)) {
            return Some(t);
        }
        // The enclosing `function` line is CHECKED (above) and then STOPS the scan.
        if function_line.is_match(line).unwrap_or(false) {
            break;
        }
        i -= 1;
    }
    None
}

// =============================================================================
// Java/Kotlin field receivers
// =============================================================================

/// Infer a Java/Kotlin receiver's type from a **field declaration** in the
/// enclosing class.
///
/// A field name often does not match its type by convention (`userbo` → class
/// `UserBO`), so the local patterns miss it. This covers Spring
/// `@Resource`/`@Autowired` field injection, where the field's type is the
/// concrete bean class.
pub fn infer_java_field_receiver_type<C: ResolutionContext>(
    receiver: &str,
    r: &UnresolvedRef,
    ctx: &C,
) -> Option<String> {
    let line = r.line?;
    let in_file = ctx.nodes_in_file(&r.file_path);
    if in_file.is_empty() {
        return None;
    }

    // The class enclosing the call — the TIGHTEST one (the latest start).
    let mut enclosing: Option<&Node> = None;
    for n in &in_file {
        if !matches!(n.kind, NodeKind::Class | NodeKind::Interface) || n.language != r.language {
            continue;
        }
        let end = n.end_line.max(n.start_line);
        if n.start_line <= line
            && end >= line
            && enclosing.is_none_or(|e| n.start_line >= e.start_line)
        {
            enclosing = Some(n);
        }
    }
    let enclosing = enclosing?;
    let enclosing_end = enclosing.end_line.max(enclosing.start_line);

    let field = in_file.iter().find(|n| {
        n.kind == NodeKind::Field
            && n.name == receiver
            && n.language == r.language
            && n.start_line >= enclosing.start_line
            && n.end_line.max(n.start_line) <= enclosing_end
    })?;

    // The signature's shape is `"<TypeName> <fieldName>"` (the extractor's
    // `extract_field`). Pull the type, strip generics, arrays and varargs.
    let signature = field.signature.as_ref()?;
    let name_at = signature.rfind(&field.name)?;
    let type_raw = signature[..name_at].trim();
    if type_raw.is_empty() {
        return None;
    }

    let no_generics = strip_angle_brackets(type_raw);
    let no_array = no_generics.replace("[]", "");
    let no_varargs = no_array.trim_end_matches("...").trim();
    let last = no_varargs.split(['.', ' ']).rfind(|s| !s.is_empty())?;

    // A primitive / lowercase type is not a class we can resolve a method on.
    if !last.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
        return None;
    }
    Some(last.to_string())
}

// =============================================================================
// C++ — its own inferrer (declarators, `auto`, sibling headers)
// =============================================================================

/// Normalize a C++ type expression: drop cv-qualifiers, `*`/`&`, generics; take
/// the last `::` segment; reject the keywords that can precede a receiver.
pub fn normalize_cpp_type_name(raw: &str) -> Option<String> {
    let mut s = strip_angle_brackets(raw);
    for kw in [
        "const", "volatile", "mutable", "typename", "class", "struct",
    ] {
        s = s.replace(kw, " ");
    }
    let s: String = s
        .chars()
        .map(|c| if c == '&' || c == '*' { ' ' } else { c })
        .collect();
    let segments: Vec<&str> = s.split("::").filter(|p| !p.trim().is_empty()).collect();
    let last = segments.last()?;
    let last = last.split_whitespace().next_back()?;
    if last.is_empty() || CPP_NON_TYPE_TOKENS.contains(&last) {
        return None;
    }
    Some(last.to_string())
}

/// The last `::` segment of a C++ name.
pub fn cpp_last_segment(name: &str) -> String {
    let segments: Vec<&str> = name.split("::").filter(|s| !s.is_empty()).collect();
    segments.last().copied().unwrap_or(name).to_string()
}

/// `Type receiver`, `Type* receiver`, `Type<X> receiver`, … — **requiring a
/// declarator terminator** (`;`, `=`, `,`, `)`, `[`, `{`, `(`, or end-of-line)
/// after the receiver.
///
/// The terminator is what rules out a *use* like `return receiver->m()`, where the
/// preceding token is a keyword and not a type.
fn cpp_declarator_pattern(escaped_receiver: &str) -> String {
    format!(
        r"([A-Za-z_][\w:]*(?:\s*<[^;=(){{}}]+>)?(?:\s*[*&]+)?)\s*\b{escaped_receiver}\b\s*(?=[;=,)\[{{(]|$)"
    )
}

/// Infer a C++ receiver's type: a backward declarator scan, `auto` initializer
/// recovery, then the sibling header.
pub fn infer_cpp_receiver_type<C: ResolutionContext>(
    receiver: &str,
    r: &UnresolvedRef,
    ctx: &C,
    depth: u8,
) -> Option<String> {
    let lines = ctx.file_lines(&r.file_path)?;
    if lines.is_empty() {
        return None;
    }

    let escaped = escape(receiver);
    let receiver_re = compiled(&format!(r"\b{escaped}\b"))?;
    let declarator = cpp_declarator_pattern(&escaped);
    let call_idx = (r.line.unwrap_or(1).saturating_sub(1) as usize).min(lines.len() - 1);

    let mut i = call_idx as isize;
    while i >= 0 {
        let line = &lines[i as usize];
        if line.len() <= 10_000
            && receiver_re.is_match(line).unwrap_or(false)
            && let Some(raw) = capture(std::slice::from_ref(&declarator), line)
        {
            match normalize_cpp_type_name(&raw).as_deref() {
                // `auto x = Foo::instance();` — the type is deduced, so recover it
                // from the initializer (#645). If this line has no usable one, keep
                // scanning earlier lines.
                Some("auto") => {
                    if let Some(t) = infer_cpp_auto_initializer_type(line, receiver, r, ctx, depth)
                    {
                        return Some(t);
                    }
                }
                Some(t) => return Some(t.to_string()),
                None => {}
            }
        }
        i -= 1;
    }

    // The declaration may live in the sibling header (the typical C++ layout).
    for ext in [".h", ".hpp", ".hxx"] {
        let header = match r.file_path.rsplit_once('.') {
            Some((stem, _)) => format!("{stem}{ext}"),
            None => continue,
        };
        if header == r.file_path || !ctx.file_exists(&header) {
            continue;
        }
        let Some(header_lines) = ctx.file_lines(&header) else {
            continue;
        };
        for line in header_lines.iter() {
            if line.len() > 10_000 || !receiver_re.is_match(line).unwrap_or(false) {
                continue;
            }
            if let Some(raw) = capture(std::slice::from_ref(&declarator), line)
                && let Some(t) = normalize_cpp_type_name(&raw)
                && t != "auto"
            {
                return Some(t);
            }
        }
    }

    None
}

/// Recover an `auto`-declared local's type from its initializer:
/// `auto x = Foo::instance();`, `auto w = make_unique<W>();`, `auto p = new W();`.
fn infer_cpp_auto_initializer_type<C: ResolutionContext>(
    line: &str,
    receiver: &str,
    r: &UnresolvedRef,
    ctx: &C,
    depth: u8,
) -> Option<String> {
    let escaped = escape(receiver);
    let init_re = compiled(&format!(r"\b{escaped}\b\s*=\s*([^;]+)"))?;
    let caps = init_re.captures(line).ok()??;
    let init = caps.get(1)?.as_str().trim();

    // `new Foo(...)`
    if let Some(new_re) = compiled(r"^new\s+([A-Za-z_][\w:]*)")
        && let Ok(Some(c)) = new_re.captures(init)
        && let Some(m) = c.get(1)
    {
        return Some(cpp_last_segment(m.as_str()));
    }

    // A call or construction: `Foo(...)`, `A::b(...)`, `make_unique<T>(...)`.
    let call_re = compiled(r"^([A-Za-z_][\w:]*(?:\s*<[^>;]*>)?)\s*\(")?;
    let caps = call_re.captures(init).ok()??;
    let callee: String = caps.get(1)?.as_str().split_whitespace().collect();
    resolve_cpp_call_result_type(&callee, r, ctx, depth + 1)
}

/// The class a C++ call/construction expression produces, from the `return_type`
/// captured at extraction (#645).
///
/// In order: `make_unique<T>`/`make_shared<T>` → `T`; a single-level `recv.method`
/// → the receiver's type, then that method's return type; a plain callee's return
/// type; a direct construction whose callee names a known class.
///
/// **Recursion is capped at depth 3** — mutual recursion between this and the
/// receiver inferrer is otherwise possible on pathological input.
pub fn resolve_cpp_call_result_type<C: ResolutionContext>(
    inner: &str,
    r: &UnresolvedRef,
    ctx: &C,
    depth: u8,
) -> Option<String> {
    if depth > 3 {
        return None;
    }
    let expr = inner.trim();

    if let Some(make_re) = compiled(r"(?:^|::)(?:make_unique|make_shared)\s*<\s*([A-Za-z_]\w*)")
        && let Ok(Some(c)) = make_re.captures(expr)
        && let Some(m) = c.get(1)
    {
        return Some(m.as_str().to_string());
    }

    // A single-level member call — the `manager.view().render()` shape.
    if let Some(dot) = expr.rfind('.')
        && dot > 0
    {
        let recv = &expr[..dot];
        let method = &expr[dot + 1..];
        // SINGLE level only: anything deeper is not something we can type.
        if recv.contains('.') || recv.contains('(') || recv.contains("::") {
            return None;
        }
        let recv_type = infer_cpp_receiver_type(recv, r, ctx, depth + 1)?;
        return lookup_callee_return_type(&format!("{recv_type}::{method}"), r, ctx);
    }

    if let Some(ret) = lookup_callee_return_type(expr, r, ctx) {
        return Some(ret);
    }

    // A direct construction: the callee itself names a class/struct.
    if cpp_class_exists(expr, r, ctx) {
        return Some(cpp_last_segment(expr));
    }
    None
}

/// The declared `return_type` of a callee (`Foo::instance`, or a free function).
pub(crate) fn lookup_callee_return_type<C: ResolutionContext>(
    callee: &str,
    r: &UnresolvedRef,
    ctx: &C,
) -> Option<String> {
    let (cls, method) = match callee.rfind("::") {
        Some(i) => (Some(&callee[..i]), &callee[i + 2..]),
        None => (None, callee),
    };

    let candidates: Vec<Node> = ctx
        .nodes_by_name(method)
        .into_iter()
        .filter(|n| {
            matches!(n.kind, NodeKind::Method | NodeKind::Function)
                && n.language == r.language
                && n.return_type.is_some()
        })
        .collect();

    match cls {
        Some(cls) => {
            let want = format!("{cls}::{method}");
            // The call site may qualify the class with MORE namespace than the
            // stored node (`details::registry::instance` vs `registry::instance`)
            // or LESS. Accept an exact match, or either being a namespace-suffix
            // of the other — the shared `::<class>::<method>` tail keeps it
            // specific.
            candidates
                .into_iter()
                .find(|n| {
                    n.qualified_name == want
                        || n.qualified_name.ends_with(&format!("::{want}"))
                        || want.ends_with(&format!("::{}", n.qualified_name))
                })
                .and_then(|n| n.return_type)
        }
        None => candidates
            .into_iter()
            .find(|n| n.kind == NodeKind::Function)
            .and_then(|n| n.return_type),
    }
}

/// Does the graph hold a class/struct named `name`'s last segment?
fn cpp_class_exists<C: ResolutionContext>(name: &str, r: &UnresolvedRef, ctx: &C) -> bool {
    ctx.nodes_by_name(&cpp_last_segment(name))
        .iter()
        .any(|n| matches!(n.kind, NodeKind::Class | NodeKind::Struct) && n.language == r.language)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn normalize_strips_generics_pointers_and_qualifiers() {
        assert_eq!(normalize_inferred_type_name("Logger").unwrap(), "Logger");
        assert_eq!(
            normalize_inferred_type_name("Repository<User>").unwrap(),
            "Repository"
        );
        assert_eq!(
            normalize_inferred_type_name("app.models.User").unwrap(),
            "User"
        );
        assert_eq!(normalize_inferred_type_name("&Logger").unwrap(), "Logger");
        assert_eq!(
            normalize_inferred_type_name("ns::Inner::Type").unwrap(),
            "Type"
        );
    }

    /// Normalization strips `&`/`*` and generics — it does **not** strip keywords,
    /// and it does not need to. `"&mut Logger"` normalizes to `"mut Logger"`,
    /// exactly as the TS source does.
    ///
    /// That is harmless for two independent reasons, and both are worth knowing.
    /// The Rust receiver pattern's capture group is `([A-Z][\w]*)` and consumes
    /// `&?(?:mut\s+)?` **before** it, so `mut` can never reach normalization from a
    /// real declaration. And even if it somehow did, `resolve_method_on_type` would
    /// find no type named `mut Logger` and emit **no edge**. Belt and braces, on
    /// purpose — a first version of this test asserted the "tidier" behavior and was
    /// simply wrong about what the code (and the TS source) does.
    #[test]
    fn normalize_does_not_strip_keywords_and_does_not_need_to() {
        assert_eq!(
            normalize_inferred_type_name("&mut Logger").unwrap(),
            "mut Logger"
        );

        // What the Rust patterns actually capture from a real declaration: the
        // group starts AFTER `&`/`mut`, so neither ever reaches normalization.
        let patterns = local_receiver_patterns(Language::Rust, "lg");
        assert_eq!(
            capture(&patterns, "fn use(lg: &mut Logger) {").as_deref(),
            Some("Logger"),
            "a typed parameter: the capture skips `&mut`"
        );
        assert_eq!(
            capture(&patterns, "    let lg: Logger = Logger::new();").as_deref(),
            Some("Logger"),
            "an annotated binding"
        );
        // …and the KNOWN GAP, pinned here too: no `\s*` before the `=` in the TS
        // pattern, so the spaced, unannotated idiom captures nothing.
        assert_eq!(
            capture(&patterns, "    let lg = Logger::new();"),
            None,
            "carried verbatim from the TS source — see the KNOWN GAP note above"
        );
    }

    /// The non-type tokens are the difference between "no edge" and a wrong one:
    /// `return x->m()` would otherwise type `x` as `return`.
    #[test]
    fn normalize_rejects_non_types() {
        for token in [
            "this",
            "self",
            "super",
            "new",
            "return",
            "null",
            "undefined",
        ] {
            assert!(
                normalize_inferred_type_name(token).is_none(),
                "{token} is never a user-defined type"
            );
        }
    }

    #[test]
    fn cpp_normalize_drops_cv_qualifiers_and_keywords() {
        assert_eq!(normalize_cpp_type_name("const Logger&").unwrap(), "Logger");
        assert_eq!(
            normalize_cpp_type_name("std::shared_ptr").unwrap(),
            "shared_ptr"
        );
        assert_eq!(normalize_cpp_type_name("Foo<Bar>*").unwrap(), "Foo");
        assert_eq!(normalize_cpp_type_name("ns::Widget").unwrap(), "Widget");
        assert!(
            normalize_cpp_type_name("return").is_none(),
            "`return ptr->m()` must not type `ptr` as `return`"
        );
    }

    /// The pattern cache is what keeps per-reference compilation from dominating
    /// the run (spike F6: ~10 ms per compile).
    #[test]
    fn the_pattern_cache_returns_the_same_compiled_regex() {
        let a = compiled(r"\bfoo\b").unwrap();
        let b = compiled(r"\bfoo\b").unwrap();
        assert!(Arc::ptr_eq(&a, &b), "the second call is a cache hit");
        assert!(
            compiled("([unclosed").is_none(),
            "a bad pattern degrades, never panics"
        );
    }

    /// The Lua lookahead — the sole justification for the `fancy-regex` dependency.
    #[test]
    fn the_lua_annotation_pattern_rejects_a_method_call() {
        let patterns = local_receiver_patterns(Language::Lua, "lg");
        // A real annotation types the receiver.
        assert_eq!(
            capture(&patterns, "local lg: Logger").as_deref(),
            Some("Logger")
        );
        // A method call on the SAME shape must NOT self-match as a type (#1124).
        assert_eq!(
            capture(&patterns, "lg:Log()"),
            None,
            "without the lookahead, `lg:Log()` types `lg` as `Log` — and the \
             backward scan starts on the call's own line, so it would never reach \
             the real declaration"
        );
    }
}
