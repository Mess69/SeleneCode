#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Ported C# conformance tests: the C# describe blocks of
//! `extraction.test.ts` (class declarations, record forms #831,
//! primary constructors #237, `#if`-guarded enum members #237) plus the
//! C#-imports block, namespace scoping, and one insta snapshot.

use selene_core::NodeKind;
use selene_extract::{ExtractionResult, Language, extract_from_source};

fn extract(path: &str, code: &str) -> ExtractionResult {
    extract_from_source(path, code, Language::CSharp)
}

fn find<'r>(r: &'r ExtractionResult, kind: NodeKind, name: &str) -> Option<&'r selene_core::Node> {
    r.nodes.iter().find(|n| n.kind == kind && n.name == name)
}

fn kind_of(r: &ExtractionResult, name: &str) -> Option<NodeKind> {
    r.nodes.iter().find(|n| n.name == name).map(|n| n.kind)
}

#[test]
fn extracts_class_declarations() {
    let code = "\npublic class OrderService\n{\n    private readonly IOrderRepository _repository;\n\n    public OrderService(IOrderRepository repository)\n    {\n        _repository = repository;\n    }\n\n    public async Task<Order> GetOrderAsync(string id)\n    {\n        return await _repository.FindByIdAsync(id);\n    }\n}\n";
    let r = extract("OrderService.cs", code);
    assert!(r.errors.is_empty(), "errors: {:?}", r.errors);

    let class = find(&r, NodeKind::Class, "OrderService").unwrap();
    assert_eq!(class.visibility, Some(selene_core::Visibility::Public));

    // Constructor + method are methods; async modifier captured; the
    // return type normalizes to the bare chained-call receiver (#645/#608).
    assert!(find(&r, NodeKind::Method, "OrderService").is_some());
    let get_order = find(&r, NodeKind::Method, "GetOrderAsync").unwrap();
    assert_eq!(get_order.is_async, Some(true));
    assert_eq!(get_order.qualified_name, "OrderService::GetOrderAsync");
}

/// #831: the grammar parses EVERY record form as record_declaration; the
/// value-type forms are told apart by their `struct` keyword child.
#[test]
fn indexes_every_record_form_with_the_right_kind() {
    let code = "\nnamespace Fixture;\n\npublic record SimplePositional(int A);\npublic record WithBody(int A) { public int DoubleIt() => A * 2; }\npublic record class ExplicitClassRec(string Name);\npublic record struct ValueRec(int X);\npublic readonly record struct ReadonlyRec(int X, int Y);\npublic record DerivedRec(int A, string B) : SimplePositional(A);\npublic record GenericRec<T>(T Value);\npublic partial record PartialRec(int A);\n";
    let r = extract("Records.cs", code);

    for class_rec in [
        "SimplePositional",
        "WithBody",
        "ExplicitClassRec",
        "DerivedRec",
        "GenericRec",
        "PartialRec",
    ] {
        assert_eq!(kind_of(&r, class_rec), Some(NodeKind::Class), "{class_rec}");
    }
    // Value-type records are structs, not classes.
    assert_eq!(kind_of(&r, "ValueRec"), Some(NodeKind::Struct));
    assert_eq!(kind_of(&r, "ReadonlyRec"), Some(NodeKind::Struct));
    // Members of a bodied record still extract.
    assert_eq!(kind_of(&r, "DoubleIt"), Some(NodeKind::Method));
    // File-scoped namespace scopes qualified names.
    assert_eq!(
        find(&r, NodeKind::Class, "WithBody")
            .unwrap()
            .qualified_name,
        "Fixture::WithBody"
    );
}

/// #237: C# 12 primary constructors, including attribute-with-args ctor
/// params (the ASP.NET keyed-DI pattern that used to swallow whole classes).
#[test]
fn indexes_primary_constructor_classes() {
    let code = "\npublic class DataService(IMemoryCache cache)\n{\n    public void Warm() { }\n}\n\npublic class InstanceService(InstanceManager m, ProfileManager p)\n{\n    public void DeployAndLaunchAsync() { }\n    public void Deploy() { }\n}\n\npublic partial class UpdateService(int x) : ILifetimeService\n{\n    public void Run() { }\n}\n\npublic class K1KeyedDi([FromKeyedServices(\"primary\")] IMemoryCache cache)\n{\n    public void Warm() { }\n}\n\npublic record CatalogBrand(int Id, string Name);\n";
    let r = extract("Services.cs", code);

    for class in [
        "DataService",
        "InstanceService",
        "UpdateService",
        "K1KeyedDi",
        "CatalogBrand",
    ] {
        assert_eq!(kind_of(&r, class), Some(NodeKind::Class), "{class}");
    }
    for method in ["DeployAndLaunchAsync", "Deploy", "Run"] {
        assert_eq!(kind_of(&r, method), Some(NodeKind::Method), "{method}");
    }
}

/// #237: a `#if` inside a nested enum's member list detaches the enclosing
/// class's members without the pre-parse blank; both branches are kept.
#[test]
fn class_with_if_guarded_nested_enum_members_stays_indexed() {
    let code = "\npublic class Reader\n{\n    private enum ReadType\n    {\n#if HAVE_DATE_TIME_OFFSET\n        ReadAsDateTimeOffset,\n#endif\n        ReadAsDouble,\n        ReadAsString,\n    }\n\n    public void Open() { }\n    public void Close() { }\n    public int ReadInt() { return 0; }\n}\n";
    let r = extract("Reader.cs", code);

    for method in ["Open", "Close", "ReadInt"] {
        assert_eq!(kind_of(&r, method), Some(NodeKind::Method), "{method}");
    }
    for member in ["ReadAsDateTimeOffset", "ReadAsDouble"] {
        assert_eq!(kind_of(&r, member), Some(NodeKind::EnumMember), "{member}");
    }
}

#[test]
fn block_namespace_scopes_qualified_names() {
    let code = "\nnamespace App.Core\n{\n    public interface IWidget { }\n    public class Widget\n    {\n        public void Render() { }\n    }\n}\n";
    let r = extract("Widget.cs", code);

    let ns = find(&r, NodeKind::Namespace, "App.Core").unwrap();
    assert_eq!(ns.qualified_name, "App.Core");
    assert_eq!(
        find(&r, NodeKind::Class, "Widget").unwrap().qualified_name,
        "App.Core::Widget"
    );
    assert_eq!(
        find(&r, NodeKind::Method, "Render").unwrap().qualified_name,
        "App.Core::Widget::Render"
    );
    assert_eq!(
        find(&r, NodeKind::Interface, "IWidget")
            .unwrap()
            .qualified_name,
        "App.Core::IWidget"
    );
}

#[test]
fn extracts_using_directives_and_member_flags() {
    let code = "\nusing System;\nusing System.Collections.Generic;\n\npublic class Config\n{\n    public const int MaxItems = 5;\n    private static readonly string[] Names = { \"a\" };\n    private readonly int _count;\n    public static void Load() { }\n}\n";
    let r = extract("Config.cs", code);

    let imports: Vec<&str> = r
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Import)
        .map(|n| n.name.as_str())
        .collect();
    assert!(imports.contains(&"System"), "{imports:?}");
    assert!(
        imports.contains(&"System.Collections.Generic"),
        "{imports:?}"
    );

    // `const` and `static readonly` fields are constants (#value-ref
    // targets); instance `readonly` stays a field.
    assert_eq!(kind_of(&r, "MaxItems"), Some(NodeKind::Constant));
    assert_eq!(kind_of(&r, "Names"), Some(NodeKind::Constant));
    assert_eq!(kind_of(&r, "_count"), Some(NodeKind::Field));
    let load = find(&r, NodeKind::Method, "Load").unwrap();
    assert_eq!(load.is_static, Some(true));
    // C# visibility defaults to private when no modifier is present.
    assert_eq!(
        find(&r, NodeKind::Field, "_count").unwrap().visibility,
        Some(selene_core::Visibility::Private)
    );
}

#[test]
fn representative_fixture_snapshot() {
    let code = "\nnamespace Shop;\n\npublic record Item(int Id, string Name);\n\npublic class Cart\n{\n    public const int Limit = 10;\n\n    public void Add(Item item) { }\n}\n";
    let mut r = extract("Cart.cs", code);
    for n in &mut r.nodes {
        n.updated_at = 0; // the one wall-clock value
    }
    r.duration_ms = 0;
    insta::assert_yaml_snapshot!("csharp_representative_fixture", r);
}
