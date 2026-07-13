#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Task 8 — method-call matching: receiver-type inference + the **validated**
//! `resolve_method_on_type`.
//!
//! # Every language block carries the same safety test
//!
//! *"Creates NO edge when the type lacks the method."* That assertion is what
//! makes loose receiver regexes safe to ship: the inference is allowed to be
//! wrong, because `resolve_method_on_type` refuses to invent an edge for a method
//! the type does not declare. Delete those tests and the patterns become a
//! wrong-edge generator.

mod common;

use common::{FakeContext, node};
use selene_core::{Language, Node, NodeKind, RefStatus, UnresolvedRef};
use selene_resolve::{ImportMapping, ResolvedBy, match_method_call, resolve_method_on_type};

fn call(name: &str, file: &str, line: u32, lang: Language) -> UnresolvedRef {
    UnresolvedRef {
        from_node_id: "function:caller".into(),
        reference_name: name.into(),
        reference_kind: "calls".into(),
        line: Some(line),
        column: Some(0),
        candidates: vec![],
        file_path: file.into(),
        language: lang.as_str().into(),
        status: RefStatus::Pending,
        name_tail: name.rsplit(['.', ':']).next().unwrap_or(name).into(),
    }
}

/// A method node whose qualified name is `Type::method` — the shape
/// `resolve_method_on_type` validates against.
fn method(id: &str, ty: &str, name: &str, file: &str, lang: Language) -> Node {
    node(
        id,
        NodeKind::Method,
        name,
        &format!("{ty}::{name}"),
        file,
        lang,
    )
}

fn class(id: &str, name: &str, file: &str, lang: Language) -> Node {
    node(id, NodeKind::Class, name, name, file, lang)
}

/// A function/method node that spans the whole file, so it is the enclosing scope
/// for every reference in it (the backward scan is bounded by it).
fn enclosing_fn(id: &str, name: &str, file: &str, lang: Language) -> Node {
    let mut n = node(id, NodeKind::Function, name, name, file, lang);
    n.start_line = 1;
    n.end_line = 100;
    n
}

// =============================================================================
// resolve_method_on_type — THE safety mechanism
// =============================================================================

/// The one assertion this whole module exists to make good on.
#[test]
fn a_type_that_lacks_the_method_yields_no_edge() {
    let ctx = FakeContext::new()
        .with_node(class(
            "class:Logger",
            "Logger",
            "src/log.ts",
            Language::Typescript,
        ))
        .with_node(method(
            "method:log",
            "Logger",
            "log",
            "src/log.ts",
            Language::Typescript,
        ))
        // A same-named method on an UNRELATED type — the decoy a name matcher grabs.
        .with_node(method(
            "method:decoy",
            "Decoy",
            "flush",
            "src/decoy.ts",
            Language::Typescript,
        ));

    let r = call("lg.flush", "src/a.ts", 5, Language::Typescript);

    assert!(
        resolve_method_on_type(
            "Logger",
            "flush",
            &r,
            &ctx,
            0.9,
            ResolvedBy::InstanceMethod,
            None,
            0
        )
        .is_none(),
        "`Logger` does not declare `flush` — NO EDGE. Not the same-named `flush` \
         on `Decoy`, not a guess. This is what makes a wrong inference harmless."
    );

    // …and the method it DOES declare resolves.
    let hit = resolve_method_on_type(
        "Logger",
        "log",
        &r,
        &ctx,
        0.9,
        ResolvedBy::InstanceMethod,
        None,
        0,
    )
    .unwrap();
    assert_eq!(hit.target_node_id, "method:log");
    assert_eq!(hit.confidence, 0.9);
}

/// An out-of-line definition (`int Foo::bar()` in `foo.cpp`, `class Foo` in
/// `foo.hpp`) — the typical C++ layout, which a same-file-only lookup misses.
#[test]
fn an_out_of_line_definition_resolves_through_the_qualified_name_suffix() {
    let ctx = FakeContext::new().with_node(node(
        "method:bar",
        NodeKind::Method,
        "bar",
        "ns::Foo::bar", // namespace-qualified — the reference only knows `Foo`
        "src/foo.cpp",
        Language::Cpp,
    ));

    let r = call("f.bar", "src/main.cpp", 5, Language::Cpp);
    let hit = resolve_method_on_type(
        "Foo",
        "bar",
        &r,
        &ctx,
        0.9,
        ResolvedBy::InstanceMethod,
        None,
        0,
    )
    .expect("the `::Foo::bar` suffix matches");
    assert_eq!(hit.target_node_id, "method:bar");
}

/// The conformance fallback: the method lives on a SUPERTYPE. This is what the
/// deferral (Task 9) exists to make possible — during the first pass the
/// implements/extends edges do not exist yet.
#[test]
fn a_method_on_a_supertype_resolves_through_the_conformance_walk() {
    let ctx = FakeContext::new()
        .with_node(class(
            "class:Dog",
            "Dog",
            "src/dog.ts",
            Language::Typescript,
        ))
        .with_node(class(
            "class:Animal",
            "Animal",
            "src/animal.ts",
            Language::Typescript,
        ))
        .with_node(method(
            "method:speak",
            "Animal",
            "speak",
            "src/animal.ts",
            Language::Typescript,
        ))
        .with_supertype("class:Dog", "class:Animal");

    let r = call("d.speak", "src/a.ts", 5, Language::Typescript);
    let hit = resolve_method_on_type(
        "Dog",
        "speak",
        &r,
        &ctx,
        0.9,
        ResolvedBy::InstanceMethod,
        None,
        0,
    )
    .expect("`Dog` inherits `speak` from `Animal`");
    assert_eq!(hit.target_node_id, "method:speak");

    // …and the walk is still VALIDATED: a method neither declares yields nothing.
    assert!(
        resolve_method_on_type(
            "Dog",
            "fly",
            &r,
            &ctx,
            0.9,
            ResolvedBy::InstanceMethod,
            None,
            0
        )
        .is_none()
    );
}

/// #314 vs #1079 — the tie-break ORDER. A Java import pins which same-named class
/// the caller means, and its target is deliberately in ANOTHER file, so the FQN
/// preference must run BEFORE the same-file preference.
#[test]
fn the_preferred_fqn_beats_the_call_sites_own_file() {
    let ctx = FakeContext::new()
        .with_node(method(
            "method:dao",
            "FooConverter",
            "convert",
            "src/main/java/com/example/dao/FooConverter.java",
            Language::Java,
        ))
        // A same-named class+method in the CALLER's own file.
        .with_node(method(
            "method:local",
            "FooConverter",
            "convert",
            "src/main/java/com/example/Main.java",
            Language::Java,
        ));

    let r = call(
        "conv.convert",
        "src/main/java/com/example/Main.java",
        20,
        Language::Java,
    );

    // With the import FQN: the DAO one wins, even though a same-named method sits
    // in the call site's own file.
    let hit = resolve_method_on_type(
        "FooConverter",
        "convert",
        &r,
        &ctx,
        0.9,
        ResolvedBy::InstanceMethod,
        Some("com.example.dao.FooConverter"),
        0,
    )
    .unwrap();
    assert_eq!(
        hit.target_node_id, "method:dao",
        "the import is the ONLY signal that names which class (#314) — it must \
         outrank the same-file preference"
    );

    // Without it, the call site's own file wins (#1079).
    let hit = resolve_method_on_type(
        "FooConverter",
        "convert",
        &r,
        &ctx,
        0.9,
        ResolvedBy::InstanceMethod,
        None,
        0,
    )
    .unwrap();
    assert_eq!(hit.target_node_id, "method:local");
}

// =============================================================================
// Receiver inference — one block per v0 language, each with its safety test
// =============================================================================

/// Build a context whose file `src/a.<ext>` holds `source`, with `Logger::log`
/// declared elsewhere.
fn ctx_for(lang: Language, file: &str, source: &str) -> FakeContext {
    FakeContext::new()
        .with_file(file, source)
        .with_node(enclosing_fn("function:main", "main", file, lang))
        .with_node(class("class:Logger", "Logger", "src/log.x", lang))
        .with_node(method("method:log", "Logger", "log", "src/log.x", lang))
}

#[test]
fn typescript_receiver_inference() {
    // `= new Logger()`
    let ctx = ctx_for(
        Language::Typescript,
        "src/a.ts",
        "function main() {\n  const lg = new Logger();\n  lg.log('x');\n}\n",
    );
    let hit =
        match_method_call(&call("lg.log", "src/a.ts", 3, Language::Typescript), &ctx).unwrap();
    assert_eq!(hit.target_node_id, "method:log");
    assert_eq!(hit.confidence, 0.9);
    assert_eq!(hit.resolved_by, ResolvedBy::InstanceMethod);

    // A TYPED PARAMETER, with no keyword prefix (#1125).
    let ctx = ctx_for(
        Language::Typescript,
        "src/a.ts",
        "function main(lg: Logger) {\n  lg.log('x');\n}\n",
    );
    let hit =
        match_method_call(&call("lg.log", "src/a.ts", 2, Language::Typescript), &ctx).unwrap();
    assert_eq!(hit.target_node_id, "method:log");
    assert_eq!(hit.confidence, 0.9, "INFERRED, not name-matched");

    // SAFETY: the type is inferred, but it does not declare the method.
    let ctx = ctx_for(
        Language::Typescript,
        "src/a.ts",
        "function main() {\n  const lg = new Logger();\n  lg.flush();\n}\n",
    );
    assert!(
        match_method_call(&call("lg.flush", "src/a.ts", 3, Language::Typescript), &ctx).is_none(),
        "NO EDGE — `Logger` has no `flush`"
    );
}

#[test]
fn python_receiver_inference() {
    let ctx = ctx_for(
        Language::Python,
        "src/a.py",
        "def main():\n    lg = Logger()\n    lg.log('x')\n",
    );
    let hit = match_method_call(&call("lg.log", "src/a.py", 3, Language::Python), &ctx).unwrap();
    assert_eq!(hit.target_node_id, "method:log");
    assert_eq!(
        hit.confidence, 0.9,
        "0.9 means the receiver's type was INFERRED and validated — a 0.7 here \
         would mean the weaker name fallback resolved it and the inference \
         silently did nothing"
    );

    // PEP 526 annotation.
    let ctx = ctx_for(
        Language::Python,
        "src/a.py",
        "def main():\n    lg: Logger = get()\n    lg.log('x')\n",
    );
    let hit = match_method_call(&call("lg.log", "src/a.py", 3, Language::Python), &ctx).unwrap();
    assert_eq!(hit.target_node_id, "method:log");
    assert_eq!(
        hit.confidence, 0.9,
        "0.9 means the receiver's type was INFERRED and validated — a 0.7 here \
         would mean the weaker name fallback resolved it and the inference \
         silently did nothing"
    );

    let ctx = ctx_for(
        Language::Python,
        "src/a.py",
        "def main():\n    lg = Logger()\n    lg.flush()\n",
    );
    assert!(match_method_call(&call("lg.flush", "src/a.py", 3, Language::Python), &ctx).is_none());
}

#[test]
fn java_and_kotlin_receiver_inference() {
    let ctx = ctx_for(
        Language::Java,
        "src/A.java",
        "void main() {\n  Logger lg = new Logger();\n  lg.log();\n}\n",
    );
    let hit = match_method_call(&call("lg.log", "src/A.java", 3, Language::Java), &ctx).unwrap();
    assert_eq!(hit.target_node_id, "method:log");
    assert_eq!(
        hit.confidence, 0.9,
        "0.9 means the receiver's type was INFERRED and validated — a 0.7 here \
         would mean the weaker name fallback resolved it and the inference \
         silently did nothing"
    );

    let ctx = ctx_for(
        Language::Kotlin,
        "src/A.kt",
        "fun main() {\n  val lg = Logger()\n  lg.log()\n}\n",
    );
    let hit = match_method_call(&call("lg.log", "src/A.kt", 3, Language::Kotlin), &ctx).unwrap();
    assert_eq!(hit.target_node_id, "method:log");
    assert_eq!(
        hit.confidence, 0.9,
        "0.9 means the receiver's type was INFERRED and validated — a 0.7 here \
         would mean the weaker name fallback resolved it and the inference \
         silently did nothing"
    );

    let ctx = ctx_for(
        Language::Java,
        "src/A.java",
        "void main() {\n  Logger lg = new Logger();\n  lg.flush();\n}\n",
    );
    assert!(match_method_call(&call("lg.flush", "src/A.java", 3, Language::Java), &ctx).is_none());
}

/// ⚠ Rust's `let` pattern carries a KNOWN GAP, ported verbatim from the TS source:
/// it has no `\s*` before the `=`, so `let lg = Logger::new();` — **the** common
/// idiom — does not match it, and with no `:` annotation it matches nothing.
///
/// A first version of this test asserted only the target node id, and it PASSED —
/// through the weaker 0.7 name fallback, which happened to find the unique `log`.
/// That is exactly the trap: a test that checks the destination and not the road
/// enshrines whatever the code does. Confidence is the claim (0.9 = "I know the
/// receiver's type"), so confidence is what these assertions check.
#[test]
fn rust_receiver_inference_and_its_known_gap() {
    // The ANNOTATED form infers properly.
    let ctx = ctx_for(
        Language::Rust,
        "src/a.rs",
        "fn main() {\n    let lg: Logger = Logger::new();\n    lg.log();\n}\n",
    );
    let hit = match_method_call(&call("lg.log", "src/a.rs", 3, Language::Rust), &ctx).unwrap();
    assert_eq!(hit.target_node_id, "method:log");
    assert_eq!(hit.confidence, 0.9, "the type was INFERRED and validated");

    // So does a typed parameter (#1125).
    let ctx = ctx_for(
        Language::Rust,
        "src/a.rs",
        "fn main(lg: &Logger) {\n    lg.log();\n}\n",
    );
    assert_eq!(
        match_method_call(&call("lg.log", "src/a.rs", 2, Language::Rust), &ctx)
            .unwrap()
            .confidence,
        0.9
    );

    // THE GAP: `let lg = Logger::new();` (a space before `=`) infers NOTHING. The
    // reference still resolves, but through the weaker 0.7 name fallback — the
    // resolver correctly declines to claim it knows the receiver's type.
    let ctx = ctx_for(
        Language::Rust,
        "src/a.rs",
        "fn main() {\n    let lg = Logger::new();\n    lg.log();\n}\n",
    );
    let hit = match_method_call(&call("lg.log", "src/a.rs", 3, Language::Rust), &ctx);
    assert!(
        hit.as_ref().is_none_or(|h| h.confidence != 0.9),
        "the TS regex has no `\\s*` before its `=`, so the most common Rust idiom \
         gets no receiver inference. This is a recall bug in the TS build, carried \
         deliberately for parity (see receiver.rs's KNOWN GAP note) — pinned here so \
         it is visible, and so fixing it is a decision with an A/B behind it rather \
         than a silent divergence."
    );

    // SAFETY, on the path that DOES infer: the type is known and lacks the method.
    let ctx = ctx_for(
        Language::Rust,
        "src/a.rs",
        "fn main() {\n    let lg: Logger = Logger::new();\n    lg.flush();\n}\n",
    );
    assert!(match_method_call(&call("lg.flush", "src/a.rs", 3, Language::Rust), &ctx).is_none());
}

#[test]
fn go_receiver_inference() {
    let ctx = ctx_for(
        Language::Go,
        "src/a.go",
        "func main() {\n\tlg := Logger{}\n\tlg.log()\n}\n",
    );
    let hit = match_method_call(&call("lg.log", "src/a.go", 3, Language::Go), &ctx).unwrap();
    assert_eq!(hit.target_node_id, "method:log");
    assert_eq!(hit.confidence, 0.9);

    // `var lg *Logger`
    let ctx = ctx_for(
        Language::Go,
        "src/a.go",
        "func main() {\n\tvar lg *Logger\n\tlg.log()\n}\n",
    );
    assert_eq!(
        match_method_call(&call("lg.log", "src/a.go", 3, Language::Go), &ctx)
            .unwrap()
            .confidence,
        0.9
    );

    let ctx = ctx_for(
        Language::Go,
        "src/a.go",
        "func main() {\n\tlg := Logger{}\n\tlg.flush()\n}\n",
    );
    assert!(match_method_call(&call("lg.flush", "src/a.go", 3, Language::Go), &ctx).is_none());
}

#[test]
fn csharp_and_ruby_receiver_inference() {
    let ctx = ctx_for(
        Language::CSharp,
        "src/A.cs",
        "void Main() {\n  Logger lg = new Logger();\n  lg.log();\n}\n",
    );
    let hit = match_method_call(&call("lg.log", "src/A.cs", 3, Language::CSharp), &ctx).unwrap();
    assert_eq!(hit.target_node_id, "method:log");
    assert_eq!(
        hit.confidence, 0.9,
        "0.9 means the receiver's type was INFERRED and validated — a 0.7 here \
         would mean the weaker name fallback resolved it and the inference \
         silently did nothing"
    );

    let ctx = ctx_for(
        Language::Ruby,
        "src/a.rb",
        "def main\n  lg = Logger.new\n  lg.log\nend\n",
    );
    let hit = match_method_call(&call("lg.log", "src/a.rb", 3, Language::Ruby), &ctx).unwrap();
    assert_eq!(hit.target_node_id, "method:log");
    assert_eq!(
        hit.confidence, 0.9,
        "0.9 means the receiver's type was INFERRED and validated — a 0.7 here \
         would mean the weaker name fallback resolved it and the inference \
         silently did nothing"
    );

    let ctx = ctx_for(
        Language::Ruby,
        "src/a.rb",
        "def main\n  lg = Logger.new\n  lg.flush\nend\n",
    );
    assert!(match_method_call(&call("lg.flush", "src/a.rb", 3, Language::Ruby), &ctx).is_none());
}

/// The C++ inferrer: a declarator, then `auto` recovered from its initializer.
#[test]
fn cpp_receiver_inference_including_auto() {
    let ctx = ctx_for(
        Language::Cpp,
        "src/a.cpp",
        "void main() {\n  Logger* lg = getLogger();\n  lg->log();\n}\n",
    );
    let hit = match_method_call(&call("lg.log", "src/a.cpp", 3, Language::Cpp), &ctx).unwrap();
    assert_eq!(hit.target_node_id, "method:log");
    assert_eq!(
        hit.confidence, 0.9,
        "0.9 means the receiver's type was INFERRED and validated — a 0.7 here \
         would mean the weaker name fallback resolved it and the inference \
         silently did nothing"
    );

    // `auto lg = new Logger();`
    let ctx = ctx_for(
        Language::Cpp,
        "src/a.cpp",
        "void main() {\n  auto lg = new Logger();\n  lg->log();\n}\n",
    );
    assert_eq!(
        match_method_call(&call("lg.log", "src/a.cpp", 3, Language::Cpp), &ctx)
            .unwrap()
            .target_node_id,
        "method:log",
        "`auto` is recovered from the initializer (#645)"
    );

    // `return lg->log()` must NOT type `lg` as `return` — that is what the
    // non-type-token list is for.
    let ctx = ctx_for(
        Language::Cpp,
        "src/a.cpp",
        "void main() {\n  return lg->flush();\n}\n",
    );
    assert!(match_method_call(&call("lg.flush", "src/a.cpp", 2, Language::Cpp), &ctx).is_none());
}

/// PHP `$this->prop->method()` — the EXCLUSIVE typed path. A property whose type
/// cannot be recovered stays unlinked rather than guessed.
#[test]
fn php_this_property_receiver_inference() {
    let source = "class Svc {\n  private Logger $lg;\n  public function run() {\n    $this->lg->log();\n  }\n}\n";
    let ctx = FakeContext::new()
        .with_file("src/Svc.php", source)
        .with_node(enclosing_fn(
            "function:run",
            "run",
            "src/Svc.php",
            Language::Php,
        ))
        .with_node(class(
            "class:Logger",
            "Logger",
            "src/Logger.php",
            Language::Php,
        ))
        .with_node(method(
            "method:log",
            "Logger",
            "log",
            "src/Logger.php",
            Language::Php,
        ))
        // The decoy a name matcher would grab.
        .with_node(method(
            "method:decoy",
            "Other",
            "log",
            "src/Other.php",
            Language::Php,
        ));

    let hit = match_method_call(&call("this->lg.log", "src/Svc.php", 4, Language::Php), &ctx)
        .expect("the typed property resolves");
    assert_eq!(hit.target_node_id, "method:log");
    assert_eq!(hit.confidence, 0.9);

    // An UNTYPED property with no recoverable type: unlinked, NOT guessed. The
    // name-similarity strategies never see this shape.
    let untyped =
        "class Svc {\n  private $lg;\n  public function run() {\n    $this->lg->log();\n  }\n}\n";
    let ctx = FakeContext::new()
        .with_file("src/Svc.php", untyped)
        .with_node(enclosing_fn(
            "function:run",
            "run",
            "src/Svc.php",
            Language::Php,
        ))
        .with_node(method(
            "method:decoy",
            "Other",
            "log",
            "src/Other.php",
            Language::Php,
        ));
    assert!(
        match_method_call(&call("this->lg.log", "src/Svc.php", 4, Language::Php), &ctx).is_none(),
        "no recoverable type ⇒ no edge — and crucially NOT a fall-through to the \
         same-named `Other::log`"
    );
}

/// #1108's bound: a same-named variable in ANOTHER function must not leak in. The
/// backward scan stops at the enclosing function's start.
///
/// Note what this does NOT claim: that the reference resolves to nothing. The
/// weaker strategies still run, and a *unique* same-named method legitimately
/// resolves through the 0.7 name fallback. What must never happen is the **0.9
/// instance-method** binding, because that one asserts "we know the receiver's
/// type" — and here we do not. The confidence is the claim, so the confidence is
/// what the test checks.
#[test]
fn the_backward_scan_is_bounded_by_the_enclosing_function() {
    let source =
        "function other() {\n  const lg = new Logger();\n}\n\nfunction main() {\n  lg.log();\n}\n";
    let mut other = enclosing_fn("function:other", "other", "src/a.ts", Language::Typescript);
    other.start_line = 1;
    other.end_line = 3;
    let mut main = enclosing_fn("function:main", "main", "src/a.ts", Language::Typescript);
    main.start_line = 5;
    main.end_line = 7;

    let ctx = FakeContext::new()
        .with_file("src/a.ts", source)
        .with_node(other)
        .with_node(main)
        .with_node(class(
            "class:Logger",
            "Logger",
            "src/log.ts",
            Language::Typescript,
        ))
        .with_node(method(
            "method:log",
            "Logger",
            "log",
            "src/log.ts",
            Language::Typescript,
        ));

    let hit = match_method_call(&call("lg.log", "src/a.ts", 6, Language::Typescript), &ctx);
    assert!(
        hit.as_ref().is_none_or(|h| h.confidence != 0.9),
        "`lg` is declared in `other()`, not in `main()`. The scan stops at the \
         enclosing function's start, so the receiver must NOT be typed from there \
         — a 0.9 instance-method binding would be the resolver claiming knowledge \
         it does not have. (A weaker 0.7 name fallback is fine and is not what \
         this test is about.)"
    );

    // Make the fallback decline too, and the leak becomes visible as a clean None:
    // two same-named methods, and `lg` shares no words with either class.
    let ctx = ctx.with_node(method(
        "method:other",
        "Printer",
        "log",
        "src/print.ts",
        Language::Typescript,
    ));
    assert!(
        match_method_call(&call("lg.log", "src/a.ts", 6, Language::Typescript), &ctx).is_none(),
        "with nothing to disambiguate on, an out-of-scope declaration must leave \
         the reference unresolved rather than guess"
    );
}

// =============================================================================
// The class-name and fallback strategies
// =============================================================================

#[test]
fn a_receiver_naming_a_class_resolves_at_0_85() {
    let ctx = FakeContext::new()
        .with_node(class(
            "class:Logger",
            "Logger",
            "src/log.ts",
            Language::Typescript,
        ))
        .with_node(method(
            "method:log",
            "Logger",
            "log",
            "src/log.ts",
            Language::Typescript,
        ));

    let hit = match_method_call(
        &call("Logger.log", "src/a.ts", 5, Language::Typescript),
        &ctx,
    )
    .expect("a static call on a class name");
    assert_eq!(hit.target_node_id, "method:log");
    assert_eq!(hit.confidence, 0.85);
    assert_eq!(hit.resolved_by, ResolvedBy::QualifiedName);
}

/// The capitalized-receiver strategy: `permissionEngine.check()` → class
/// `PermissionEngine`.
#[test]
fn a_capitalized_receiver_finds_its_class_at_0_8() {
    let ctx = FakeContext::new()
        .with_node(class(
            "class:PermissionEngine",
            "PermissionEngine",
            "src/perm.ts",
            Language::Typescript,
        ))
        .with_node(method(
            "method:check",
            "PermissionEngine",
            "check",
            "src/perm.ts",
            Language::Typescript,
        ));

    let hit = match_method_call(
        &call(
            "permissionEngine.check",
            "src/a.ts",
            5,
            Language::Typescript,
        ),
        &ctx,
    )
    .unwrap();
    assert_eq!(hit.target_node_id, "method:check");
    assert_eq!(hit.confidence, 0.8);
    assert_eq!(hit.resolved_by, ResolvedBy::InstanceMethod);
}

/// The last-resort fallback: a UNIQUE same-language method by name (0.7), and a
/// word-overlap score of ≥ 2 otherwise (0.65). Below that it is a coin flip, and a
/// coin flip is a wrong edge waiting to happen.
#[test]
fn the_method_name_fallback_requires_a_unique_match_or_real_overlap() {
    // Unique.
    let ctx = FakeContext::new().with_node(method(
        "method:only",
        "Whatever",
        "veryUniqueName",
        "src/w.ts",
        Language::Typescript,
    ));
    let hit = match_method_call(
        &call("thing.veryUniqueName", "src/a.ts", 5, Language::Typescript),
        &ctx,
    )
    .unwrap();
    assert_eq!(hit.confidence, 0.7);

    // Two candidates, and the receiver's words overlap one of them.
    let ctx = FakeContext::new()
        .with_node(method(
            "method:right",
            "PermissionRuleEngine",
            "check",
            "src/p.ts",
            Language::Typescript,
        ))
        .with_node(method(
            "method:wrong",
            "SomethingElse",
            "check",
            "src/s.ts",
            Language::Typescript,
        ));
    let hit = match_method_call(
        &call(
            "permissionEngine.check",
            "src/a.ts",
            5,
            Language::Typescript,
        ),
        &ctx,
    )
    .unwrap();
    assert_eq!(
        hit.target_node_id, "method:right",
        "`permission` + `Engine` overlap `PermissionRuleEngine`"
    );
    assert_eq!(hit.confidence, 0.65);

    // No overlap at all ⇒ NO edge (score 1 from the language bonus is below 2).
    let ctx = FakeContext::new()
        .with_node(method(
            "method:a",
            "Alpha",
            "run",
            "src/a.ts",
            Language::Typescript,
        ))
        .with_node(method(
            "method:b",
            "Beta",
            "run",
            "src/b.ts",
            Language::Typescript,
        ));
    assert!(
        match_method_call(&call("zzz.run", "src/c.ts", 5, Language::Typescript), &ctx).is_none(),
        "two same-named methods and nothing to choose between them ⇒ decline"
    );
}

/// Java's field-signature inference — a field name that does not match its type by
/// convention (`userbo` → `UserBO`), which the local patterns cannot see. Covers
/// Spring `@Autowired` injection.
#[test]
fn a_java_field_receiver_is_typed_from_its_signature() {
    let mut field = node(
        "field:userbo",
        NodeKind::Field,
        "userbo",
        "Svc::userbo",
        "src/Svc.java",
        Language::Java,
    );
    field.signature = Some("UserBO userbo".into());
    field.start_line = 3;
    field.end_line = 3;

    let mut svc = class("class:Svc", "Svc", "src/Svc.java", Language::Java);
    svc.start_line = 1;
    svc.end_line = 20;

    let ctx = FakeContext::new()
        .with_file("src/Svc.java", "class Svc {\n  @Autowired\n  private UserBO userbo;\n\n  void run() {\n    userbo.save();\n  }\n}\n")
        .with_node(svc)
        .with_node(field)
        .with_node(method("method:save", "UserBO", "save", "src/UserBO.java", Language::Java));

    let hit = match_method_call(
        &call("userbo.save", "src/Svc.java", 6, Language::Java),
        &ctx,
    )
    .expect("the field's declared type is the bean class");
    assert_eq!(hit.target_node_id, "method:save");
    assert_eq!(hit.confidence, 0.9);
}

/// A Java/Kotlin import disambiguates the inferred type when two classes share a
/// simple name (#314) — the inference names `FooConverter`, the import says WHICH.
#[test]
fn an_imported_fqn_disambiguates_the_inferred_type() {
    let ctx = FakeContext::new()
        .with_file(
            "src/main/java/com/example/Main.java",
            "class Main {\n  void run() {\n    FooConverter conv = new FooConverter();\n    conv.convert();\n  }\n}\n",
        )
        .with_node(enclosing_fn(
            "function:run",
            "run",
            "src/main/java/com/example/Main.java",
            Language::Java,
        ))
        .with_node(method(
            "method:dao",
            "FooConverter",
            "convert",
            "src/main/java/com/example/dao/FooConverter.java",
            Language::Java,
        ))
        .with_node(method(
            "method:web",
            "FooConverter",
            "convert",
            "src/main/java/com/example/web/FooConverter.java",
            Language::Java,
        ))
        .with_import_mapping(
            "src/main/java/com/example/Main.java",
            ImportMapping {
                local_name: "FooConverter".into(),
                exported_name: "FooConverter".into(),
                source: "com.example.dao.FooConverter".into(),
                is_default: false,
                is_namespace: false,
                resolved_path: None,
            },
        );

    let hit = match_method_call(
        &call(
            "conv.convert",
            "src/main/java/com/example/Main.java",
            4,
            Language::Java,
        ),
        &ctx,
    )
    .expect("the receiver types to `FooConverter`, and the import picks the dao one");
    assert_eq!(hit.target_node_id, "method:dao");
}
