//! Ported Kotlin conformance tests: the Kotlin describe block of
//! `extraction.test.ts` (classes, functions, suspend, `fun interface` ×4,
//! packages ×3) + the Kotlin imports block + the extraction-level half of
//! the Multiplatform expect/actual suite (decorator capture; the resolver
//! linking stays with its phase), plus property-scope classification,
//! extension receivers, enum classify, `Unit` rejection, and one insta
//! snapshot.
//!
//! kotlin-ng note: the grammar parses `fun interface` CLEANLY (no ERROR
//! recovery — dropped, spike-pinned), so the TS tests' "misparse pattern"
//! comments no longer apply; the assertions themselves are unchanged.
//! Fixtures use multiline bodies (kotlin-ng wants a newline/semicolon after
//! the last member of a single-line body).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use selene_core::{EdgeKind, NodeKind};
use selene_extract::{ExtractionResult, Language, extract_from_source};

fn extract(path: &str, code: &str) -> ExtractionResult {
    extract_from_source(path, code, Language::Kotlin)
}

fn find<'r>(r: &'r ExtractionResult, kind: NodeKind, name: &str) -> Option<&'r selene_core::Node> {
    r.nodes.iter().find(|n| n.kind == kind && n.name == name)
}

// =============================================================================
// extraction.test.ts — describe('Kotlin Extraction')
// =============================================================================

#[test]
fn extracts_class_declarations() {
    let code = "\nclass UserRepository(private val database: Database) {\n    fun findById(id: String): User? {\n        return database.query(\"SELECT * FROM users WHERE id = ?\", id)\n    }\n\n    suspend fun save(user: User) {\n        database.insert(user)\n    }\n}\n";
    let r = extract("UserRepository.kt", code);

    assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
    let class = find(&r, NodeKind::Class, "UserRepository").unwrap();
    assert_eq!(class.language, "kotlin");

    let find_by_id = find(&r, NodeKind::Method, "findById").unwrap();
    assert_eq!(find_by_id.qualified_name, "UserRepository::findById");
    assert_eq!(find_by_id.signature.as_deref(), Some("(id: String): User?"));
    // Nullable return unwraps to the bare class name.
    assert_eq!(find_by_id.return_type.as_deref(), Some("User"));
}

#[test]
fn extracts_function_declarations() {
    let code = "\nfun calculateTotal(items: List<Item>): Double {\n    return items.sumOf { it.price }\n}\n\nsuspend fun fetchUserData(userId: String): User {\n    return api.getUser(userId)\n}\n";
    let r = extract("utils.kt", code);
    let functions: Vec<_> = r
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Function)
        .collect();
    assert!(!functions.is_empty());
    assert!(find(&r, NodeKind::Function, "calculateTotal").is_some());
}

#[test]
fn detects_suspend_functions_as_async() {
    let code = "\nsuspend fun loadData(): List<String> {\n    delay(1000)\n    return listOf(\"a\", \"b\", \"c\")\n}\n";
    let r = extract("loader.kt", code);
    let f = find(&r, NodeKind::Function, "loadData").unwrap();
    assert_eq!(f.is_async, Some(true));
}

#[test]
fn extracts_fun_interface_declarations() {
    let code = "\nfun interface OnObjectRetainedListener {\n  fun onObjectRetained()\n}\n";
    let r = extract("listener.kt", code);

    let iface = find(&r, NodeKind::Interface, "OnObjectRetainedListener").unwrap();
    let method = find(&r, NodeKind::Method, "onObjectRetained").unwrap();
    assert_eq!(
        method.qualified_name,
        "OnObjectRetainedListener::onObjectRetained"
    );
    assert!(
        r.edges
            .iter()
            .any(|e| e.kind == EdgeKind::Contains && e.source == iface.id && e.target == method.id)
    );
}

#[test]
fn extracts_complex_fun_interface_with_nested_classes() {
    let code = "\nfun interface EventListener {\n  fun onEvent(event: Event)\n\n  sealed class Event {\n    class DumpingHeap : Event()\n  }\n}\n";
    let r = extract("events.kt", code);

    assert!(find(&r, NodeKind::Interface, "EventListener").is_some());
    // kotlin-ng parses the nested classes properly (the TS-era grammar
    // surfaced them as siblings "due to grammar limitations") — presence and
    // kinds are what the TS test asserted.
    assert!(find(&r, NodeKind::Class, "Event").is_some());
    assert!(find(&r, NodeKind::Class, "DumpingHeap").is_some());
}

#[test]
fn fun_interface_does_not_affect_regular_functions() {
    let code = "\nfun interface MyCallback {\n  fun invoke(value: Int)\n}\n\nfun regularFunction(): String {\n  return \"hello\"\n}\n";
    let r = extract("mixed.kt", code);
    assert!(find(&r, NodeKind::Interface, "MyCallback").is_some());
    assert!(find(&r, NodeKind::Function, "regularFunction").is_some());
}

#[test]
fn extracts_fun_interface_with_annotated_method() {
    // The OkHttp Interceptor pattern (TS "Pattern 2b" — a misparse under the
    // WASM grammar; kotlin-ng parses it clean, same assertion).
    let code = "\nimport java.io.IOException\n\nfun interface Interceptor {\n  @Throws(IOException::class)\n  fun intercept(chain: Chain): Response\n}\n";
    let r = extract("interceptor.kt", code);
    assert!(find(&r, NodeKind::Interface, "Interceptor").is_some());
    assert!(find(&r, NodeKind::Method, "intercept").is_some());
}

#[test]
fn extracts_methods_from_interface_with_nested_fun_interface() {
    let code = "\ninterface WebSocket {\n  fun request(): Request\n  fun send(text: String): Boolean\n  fun cancel()\n  fun interface Factory {\n    fun newWebSocket(request: Request): WebSocket\n  }\n}\n";
    let r = extract("websocket.kt", code);

    assert!(find(&r, NodeKind::Interface, "WebSocket").is_some());
    let method_names: Vec<&str> = r
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Method && n.qualified_name.starts_with("WebSocket::"))
        .map(|n| n.name.as_str())
        .collect();
    for want in ["request", "send", "cancel"] {
        assert!(
            method_names.contains(&want),
            "missing {want}: {method_names:?}"
        );
    }
    assert!(find(&r, NodeKind::Interface, "Factory").is_some());
}

// =============================================================================
// Package namespaces
// =============================================================================

#[test]
fn wraps_top_level_declarations_in_package_namespace() {
    let code = "\npackage com.example.foo\n\nclass Bar {\n  fun greet(): String = \"hi\"\n}\n\nfun util(): Int = 42\n";
    let r = extract("Bar.kt", code);

    let ns = find(&r, NodeKind::Namespace, "com.example.foo").unwrap();
    assert_eq!(ns.name, "com.example.foo");
    assert_eq!(
        find(&r, NodeKind::Class, "Bar").unwrap().qualified_name,
        "com.example.foo::Bar"
    );
    assert_eq!(
        find(&r, NodeKind::Method, "greet").unwrap().qualified_name,
        "com.example.foo::Bar::greet"
    );
    assert_eq!(
        find(&r, NodeKind::Function, "util").unwrap().qualified_name,
        "com.example.foo::util"
    );
}

#[test]
fn handles_a_single_segment_package() {
    let code = "\npackage foo\n\nclass Bar\n";
    let r = extract("Bar.kt", code);
    assert_eq!(
        find(&r, NodeKind::Class, "Bar").unwrap().qualified_name,
        "foo::Bar"
    );
}

#[test]
fn does_not_wrap_without_package_declaration() {
    let code = "\nclass Bar {\n  fun greet() = \"hi\"\n}\n";
    let r = extract("Bar.kt", code);
    assert!(r.nodes.iter().all(|n| n.kind != NodeKind::Namespace));
    assert_eq!(
        find(&r, NodeKind::Class, "Bar").unwrap().qualified_name,
        "Bar"
    );
}

// =============================================================================
// extraction.test.ts — describe('Kotlin imports')
// =============================================================================

#[test]
fn simple_import() {
    let r = extract("Main.kt", "import java.io.IOException");
    let imp = find(&r, NodeKind::Import, "java.io.IOException").unwrap();
    assert_eq!(imp.signature.as_deref(), Some("import java.io.IOException"));
}

#[test]
fn aliased_import() {
    let r = extract(
        "Utils.kt",
        "import okhttp3.Request.Builder as RequestBuilder",
    );
    let imp = find(&r, NodeKind::Import, "okhttp3.Request.Builder").unwrap();
    assert!(
        imp.signature
            .as_deref()
            .unwrap()
            .contains("as RequestBuilder")
    );
}

#[test]
fn wildcard_import() {
    let r = extract("Time.kt", "import java.util.concurrent.TimeUnit.*");
    let imp = find(&r, NodeKind::Import, "java.util.concurrent.TimeUnit").unwrap();
    assert!(imp.signature.as_deref().unwrap().contains(".*"));
}

#[test]
fn multiple_imports() {
    let code = "\nimport java.io.IOException\nimport kotlin.test.assertFailsWith\nimport okhttp3.OkHttpClient\n";
    let r = extract("Test.kt", code);
    let names: Vec<&str> = r
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Import)
        .map(|n| n.name.as_str())
        .collect();
    assert_eq!(names.len(), 3);
    for want in [
        "java.io.IOException",
        "kotlin.test.assertFailsWith",
        "okhttp3.OkHttpClient",
    ] {
        assert!(names.contains(&want), "missing {want}: {names:?}");
    }
}

// =============================================================================
// Multiplatform expect/actual — extraction-level half (decorator capture)
// =============================================================================

#[test]
fn expect_and_actual_modifiers_are_captured_as_decorators() {
    let common = "\npackage demo.internal\n\nexpect fun systemProp(name: String): String?\n\nexpect class Platform {\n    fun describe(): String\n}\n";
    let r = extract("SystemProps.kt", common);
    let f = find(&r, NodeKind::Function, "systemProp").unwrap();
    assert!(
        f.decorators.iter().any(|d| d == "expect"),
        "{:?}",
        f.decorators
    );
    let c = find(&r, NodeKind::Class, "Platform").unwrap();
    assert!(
        c.decorators.iter().any(|d| d == "expect"),
        "{:?}",
        c.decorators
    );

    let jvm = "\npackage demo.internal\n\nactual fun systemProp(name: String): String? = System.getProperty(name)\n\nactual class Platform {\n    actual fun describe(): String = \"JVM\"\n}\n";
    let r2 = extract("SystemPropsJvm.kt", jvm);
    let af = find(&r2, NodeKind::Function, "systemProp").unwrap();
    assert!(af.decorators.iter().any(|d| d == "actual"));
}

// =============================================================================
// Property scope classification (val/var/const val)
// =============================================================================

#[test]
fn property_scope_classification() {
    let code = "\nclass Repo {\n    companion object {\n        val DEFAULT = 1\n        var counter = 0\n    }\n    val instanceProp: String = \"iv\"\n    var mutable = 1\n    fun work() {\n        val local = 2\n    }\n}\n\nobject Singleton {\n    val X = 1\n}\n\nval topLevel: Int = 3\nconst val MAX = 10\n";
    let r = extract("props.kt", code);

    // companion object: val → constant, var → variable.
    assert!(find(&r, NodeKind::Constant, "DEFAULT").is_some());
    assert!(find(&r, NodeKind::Variable, "counter").is_some());
    // class instance properties → field.
    let inst = find(&r, NodeKind::Field, "instanceProp").unwrap();
    assert_eq!(inst.signature.as_deref(), Some("val instanceProp: String"));
    assert!(find(&r, NodeKind::Field, "mutable").is_some());
    // locals inside a function body are skipped entirely.
    assert!(
        r.nodes.iter().all(|n| n.name != "local"),
        "function-body locals must not be extracted"
    );
    // object declarations are constant scopes.
    assert!(find(&r, NodeKind::Constant, "X").is_some());
    // top level: val → constant (a `const val` is a `val`).
    assert!(find(&r, NodeKind::Constant, "topLevel").is_some());
    assert!(find(&r, NodeKind::Constant, "MAX").is_some());
    // object_declaration itself is an extra class node.
    assert!(find(&r, NodeKind::Class, "Singleton").is_some());
}

// =============================================================================
// Extension receivers, enum classify, Unit rejection, typealias
// =============================================================================

#[test]
fn extension_function_gets_receiver_qualified_name() {
    let code = "\nfun String.shout(): String = this + \"!\"\n";
    let r = extract("ext.kt", code);
    let m = find(&r, NodeKind::Method, "shout").unwrap();
    assert_eq!(m.qualified_name, "String::shout");
}

#[test]
fn classifies_interface_and_enum_class_declarations() {
    let code =
        "\ninterface Face {\n    fun m(): Int\n}\n\nenum class Level {\n    LOW,\n    HIGH\n}\n";
    let r = extract("kinds.kt", code);

    assert!(find(&r, NodeKind::Interface, "Face").is_some());
    let level = find(&r, NodeKind::Enum, "Level").unwrap();
    let low = find(&r, NodeKind::EnumMember, "LOW").unwrap();
    assert!(find(&r, NodeKind::EnumMember, "HIGH").is_some());
    assert!(
        r.edges
            .iter()
            .any(|e| e.kind == EdgeKind::Contains && e.source == level.id && e.target == low.id)
    );
}

#[test]
fn unit_and_nothing_returns_are_rejected() {
    let code = "\nfun log(msg: String): Unit {\n    println(msg)\n}\n\nfun fail(): Nothing {\n    throw IllegalStateException()\n}\n\nfun name(): String = \"x\"\n";
    let r = extract("returns.kt", code);
    assert_eq!(
        find(&r, NodeKind::Function, "log").unwrap().return_type,
        None
    );
    assert_eq!(
        find(&r, NodeKind::Function, "fail").unwrap().return_type,
        None
    );
    assert_eq!(
        find(&r, NodeKind::Function, "name")
            .unwrap()
            .return_type
            .as_deref(),
        Some("String")
    );
}

#[test]
fn typealias_name_comes_from_the_type_field() {
    // kotlin-ng: `(type_alias type: (identifier) …)` — the name sits in the
    // `type` field, resolved by the resolve_name hook.
    let r = extract("alias.kt", "typealias Alias = Map<String, Int>\n");
    assert!(find(&r, NodeKind::TypeAlias, "Alias").is_some());
}

// =============================================================================
// Snapshot: full ExtractionResult for a representative Kotlin fixture.
// =============================================================================

#[test]
fn representative_fixture_snapshot() {
    let code = "package demo.repo\n\nimport kotlin.collections.List\n\n// Repo doc\nclass Repo(val name: String) {\n    companion object {\n        val DEFAULT = 1\n    }\n\n    val label: String = \"r\"\n\n    suspend fun fetch(id: Int): Repo? {\n        return null\n    }\n}\n\nfun interface Transformer {\n    fun transform(x: Int): Int\n}\n\nexpect fun platform(): String\n";
    let r = extract("Repo.kt", code);
    insta::assert_yaml_snapshot!(r, {
        ".nodes[].updatedAt" => "[ts]",
        ".durationMs" => "[ms]",
    });
}
