//! Ported TypeScript conformance tests: the TypeScript / Arrow Function
//! Export / Type Alias (#634) / File Node describe blocks of
//! `extraction.test.ts`, the call-shape tests deferred from Task 6 (the TS
//! grammar drives them), the multi-declarator docstring rider (Task 4
//! review), and insta snapshots for one ts + one tsx fixture.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use selene_core::{EdgeKind, NodeKind};
use selene_extract::{ExtractionResult, Language, extract_from_source};

fn extract(path: &str, code: &str) -> ExtractionResult {
    extract_from_source(path, code, Language::Typescript)
}

fn find<'r>(r: &'r ExtractionResult, kind: NodeKind, name: &str) -> Option<&'r selene_core::Node> {
    r.nodes.iter().find(|n| n.kind == kind && n.name == name)
}

// =============================================================================
// describe('TypeScript Extraction')
// =============================================================================

#[test]
fn extracts_function_declarations() {
    let code = "\nexport function processPayment(amount: number): Promise<Receipt> {\n  return stripe.charge(amount);\n}\n";
    let r = extract("payment.ts", code);

    let file = r.nodes.iter().find(|n| n.kind == NodeKind::File).unwrap();
    assert_eq!(file.name, "payment.ts");

    let f = find(&r, NodeKind::Function, "processPayment").unwrap();
    assert_eq!(f.language, "typescript");
    assert_eq!(f.is_exported, Some(true));
    assert!(f.signature.as_deref().unwrap().contains("amount: number"));
}

#[test]
fn extracts_class_declarations_with_members() {
    let code = "\nexport class PaymentService {\n  private stripe: StripeClient;\n\n  constructor(apiKey: string) {\n    this.stripe = new StripeClient(apiKey);\n  }\n\n  async charge(amount: number): Promise<Receipt> {\n    return this.stripe.charge(amount);\n  }\n}\n";
    let r = extract("service.ts", code);

    let class = find(&r, NodeKind::Class, "PaymentService").unwrap();
    assert_eq!(class.is_exported, Some(true));

    let charge = find(&r, NodeKind::Method, "charge").unwrap();
    assert_eq!(charge.qualified_name, "PaymentService::charge");
    assert_eq!(charge.is_async, Some(true));

    // #808: a plain typed field is a PROPERTY, not a method.
    let stripe = find(&r, NodeKind::Property, "stripe").unwrap();
    assert_eq!(stripe.visibility, Some(selene_core::Visibility::Private));
    assert!(find(&r, NodeKind::Method, "stripe").is_none());
}

#[test]
fn field_valued_functions_are_methods_hof_included() {
    // #808 the other direction: arrow fields and HOF-wrapped arrow fields
    // ARE methods.
    let code = "class Scroller {\n  onScroll = throttle((e: Event) => {\n    this.track(e);\n  });\n  handle = (e: Event) => this.track(e);\n  plain = 0;\n}\n";
    let r = extract("scroll.ts", code);
    assert!(find(&r, NodeKind::Method, "onScroll").is_some());
    assert!(find(&r, NodeKind::Method, "handle").is_some());
    assert!(find(&r, NodeKind::Property, "plain").is_some());
    // The HOF-wrapped field's body calls attribute to the method (#808
    // resolveBody digs through the wrapper).
    let on_scroll = find(&r, NodeKind::Method, "onScroll").unwrap();
    assert!(r.unresolved.iter().any(|u| u.from_node_id == on_scroll.id
        && u.reference_kind == "calls"
        && u.reference_name == "track"));
}

// -----------------------------------------------------------------------------
// Class-field initializers are walked (TS tree-sitter.ts:996-1006). A #808
// property demotion consumes the field subtree, so its initializer's calls /
// instantiations must be walked explicitly — and attribute to the PROPERTY
// node (TS pushes `propNode.id` before `visitFunctionBody`).
// -----------------------------------------------------------------------------

#[test]
fn class_field_initializer_emits_instantiates_from_the_property() {
    let code = "class Svc {\n  client = new HttpClient();\n}\n";
    let r = extract("svc.ts", code);

    let client = find(&r, NodeKind::Property, "client").unwrap();
    let inst: Vec<_> = r
        .unresolved
        .iter()
        .filter(|u| u.reference_kind == EdgeKind::Instantiates.as_str())
        .collect();
    assert_eq!(inst.len(), 1, "exactly one instantiates ref: {inst:?}");
    assert_eq!(inst[0].reference_name, "HttpClient");
    assert_eq!(inst[0].from_node_id, client.id);
    assert_eq!(inst[0].line, Some(2));
}

#[test]
fn class_field_initializer_emits_calls_from_the_property() {
    let code = "class Svc {\n  handler = makeHandler();\n}\n";
    let r = extract("svc.ts", code);

    let handler = find(&r, NodeKind::Property, "handler").unwrap();
    let calls: Vec<_> = r
        .unresolved
        .iter()
        .filter(|u| u.reference_kind == EdgeKind::Calls.as_str())
        .collect();
    assert_eq!(calls.len(), 1, "exactly one calls ref: {calls:?}");
    assert_eq!(calls[0].reference_name, "makeHandler");
    assert_eq!(calls[0].from_node_id, handler.id);
    assert_eq!(calls[0].line, Some(2));
}

#[test]
fn class_field_initializer_walk_leaves_property_nodes_unchanged() {
    // Existing-behavior guard: walking the value must not add/alter nodes.
    let code = "class Svc {\n  private static client = new HttpClient();\n}\n";
    let r = extract("svc.ts", code);

    let client = find(&r, NodeKind::Property, "client").unwrap();
    assert_eq!(client.qualified_name, "Svc::client");
    assert_eq!(client.visibility, Some(selene_core::Visibility::Private));
    assert_eq!(client.is_static, Some(true));
    assert!(find(&r, NodeKind::Method, "client").is_none());
    // The initializer creates no nodes of its own — File + Class + Property.
    assert_eq!(r.nodes.len(), 3, "{:?}", r.nodes);
}

// =============================================================================
// describe('Arrow Function Export Extraction')
// =============================================================================

#[test]
fn exported_arrow_const_extracts_as_function() {
    let r = extract(
        "hooks.ts",
        "\nexport const useAuth = (): AuthContextValue => {\n  return useContext(AuthContext);\n};\n",
    );
    let f = find(&r, NodeKind::Function, "useAuth").unwrap();
    assert_eq!(f.is_exported, Some(true));
    // NOT duplicated as a variable.
    assert!(find(&r, NodeKind::Variable, "useAuth").is_none());
    assert!(find(&r, NodeKind::Constant, "useAuth").is_none());
}

#[test]
fn exported_function_expression_extracts_as_function() {
    let r = extract(
        "utils.ts",
        "\nexport const processData = function(input: string): string {\n  return input.trim();\n};\n",
    );
    let f = find(&r, NodeKind::Function, "processData").unwrap();
    assert_eq!(f.is_exported, Some(true));
}

#[test]
fn non_exported_arrow_is_not_exported() {
    let r = extract(
        "internal.ts",
        "\nconst internalHelper = () => {\n  return 42;\n};\n",
    );
    let f = find(&r, NodeKind::Function, "internalHelper").unwrap();
    assert_ne!(f.is_exported, Some(true));
}

#[test]
fn truly_anonymous_arrows_stay_skipped() {
    let r = extract("anon.ts", "\nconst items = [1, 2, 3].map((x) => x * 2);\n");
    assert!(
        !r.nodes
            .iter()
            .any(|n| n.kind == NodeKind::Function && n.name == "<anonymous>")
    );
}

#[test]
fn multiple_exported_arrows() {
    let code = "\nexport const add = (a: number, b: number): number => a + b;\n\nexport const subtract = (a: number, b: number): number => a - b;\n\nconst internal = () => 'not exported';\n";
    let r = extract("math.ts", code);
    let exported: Vec<&str> = r
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Function && n.is_exported == Some(true))
        .map(|n| n.name.as_str())
        .collect();
    assert_eq!(exported.len(), 2);
    assert!(exported.contains(&"add") && exported.contains(&"subtract"));
    let internal = find(&r, NodeKind::Function, "internal").unwrap();
    assert_ne!(internal.is_exported, Some(true));
}

// =============================================================================
// describe('Type Alias Extraction') incl. #634
// =============================================================================

#[test]
fn type_aliases_exported_and_not() {
    let r = extract(
        "types.ts",
        "\nexport type AuthContextValue = {\n  user: User | null;\n  login: () => void;\n  logout: () => void;\n};\n",
    );
    let alias = find(&r, NodeKind::TypeAlias, "AuthContextValue").unwrap();
    assert_eq!(alias.is_exported, Some(true));
    // #359: function-typed members surface as methods under the alias.
    let login = find(&r, NodeKind::Method, "login").unwrap();
    assert_eq!(login.qualified_name, "AuthContextValue::login");
    let user = find(&r, NodeKind::Property, "user").unwrap();
    assert_eq!(user.qualified_name, "AuthContextValue::user");

    let r = extract(
        "internal.ts",
        "\ntype InternalState = {\n  loading: boolean;\n  error: string | null;\n};\n",
    );
    let alias = find(&r, NodeKind::TypeAlias, "InternalState").unwrap();
    assert_eq!(alias.is_exported, Some(false));
}

#[test]
fn multiple_type_aliases() {
    let code = "\nexport type UnitSystem = 'metric' | 'imperial';\nexport type DateFormat = 'ISO' | 'US' | 'EU';\ntype Internal = string;\n";
    let r = extract("config.ts", code);
    let aliases: Vec<&selene_core::Node> = r
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::TypeAlias)
        .collect();
    assert_eq!(aliases.len(), 3);
    let mut exported: Vec<&str> = aliases
        .iter()
        .filter(|n| n.is_exported == Some(true))
        .map(|n| n.name.as_str())
        .collect();
    exported.sort_unstable();
    assert_eq!(exported, vec!["DateFormat", "UnitSystem"]);
}

#[test]
fn tuple_contract_names_634() {
    let code = "\ninterface Service<Name extends string, Req, Resp> { name: Name; }\nexport type MyServiceList = [\n  Service<'query_apply_record', { pageNo: number }, { ok: boolean }>,\n  Service<'apply_confirm', { code: string }, { ok: boolean }>\n];\n";
    let r = extract("services/api.ts", code);

    let mut names: Vec<&str> = r
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Method && n.qualified_name.starts_with("MyServiceList::"))
        .map(|n| n.name.as_str())
        .collect();
    names.sort_unstable();
    assert_eq!(names, vec!["apply_confirm", "query_apply_record"]);

    let query = find(&r, NodeKind::Method, "query_apply_record").unwrap();
    assert_eq!(query.qualified_name, "MyServiceList::query_apply_record");
    assert!(
        query
            .signature
            .as_deref()
            .unwrap()
            .contains("Service<'query_apply_record'")
    );

    // Contained by the alias.
    let alias = find(&r, NodeKind::TypeAlias, "MyServiceList").unwrap();
    assert!(
        r.edges
            .iter()
            .any(|e| e.kind == EdgeKind::Contains && e.source == alias.id && e.target == query.id)
    );
}

#[test]
fn tuple_contract_noise_guard_634() {
    let code = "\ninterface User { id: string; name: string; }\ninterface Service<Name extends string, Req, Resp> { name: Name; }\nexport type Picked = Pick<User, 'id' | 'name'>;\nexport type Rec = Record<'foo' | 'bar', number>;\nexport type Routes = [Service<'/api/users', Pick<User, 'id'>, {}>];\nexport type Names = ['alpha', 'beta'];\n";
    let r = extract("noise.ts", code);
    // No method/property nodes leak from utility types, route paths,
    // nested generics, or bare string tuples.
    let leaked: Vec<&str> = r
        .nodes
        .iter()
        .filter(|n| {
            (n.kind == NodeKind::Method || n.kind == NodeKind::Property)
                && n.qualified_name.contains("::")
                && !n.qualified_name.starts_with("User::")
                && !n.qualified_name.starts_with("Service::")
        })
        .map(|n| n.name.as_str())
        .collect();
    assert!(leaked.is_empty(), "leaked contract names: {leaked:?}");
}

// =============================================================================
// Call shapes deferred from Task 6 (TS grammar drives them)
// =============================================================================

#[test]
fn call_shapes_receiver_this_and_instantiation() {
    let code = "class Svc {\n  run() {\n    this.helper();\n    other.method();\n    plain();\n    const c = new StripeClient('key');\n  }\n  helper() {}\n}\n";
    let r = extract("svc.ts", code);
    let run_id = &find(&r, NodeKind::Method, "run").unwrap().id;
    let calls: Vec<&str> = r
        .unresolved
        .iter()
        .filter(|u| u.reference_kind == "calls" && &u.from_node_id == run_id)
        .map(|u| u.reference_name.as_str())
        .collect();
    assert!(calls.contains(&"helper"), "this.x() → bare: {calls:?}");
    assert!(calls.contains(&"other.method"));
    assert!(calls.contains(&"plain"));
    // new_expression → instantiates, generic args stripped elsewhere.
    assert!(
        r.unresolved
            .iter()
            .any(|u| u.reference_kind == "instantiates"
                && u.reference_name == "StripeClient"
                && &u.from_node_id == run_id)
    );
}

#[test]
fn instantiation_strips_generics_and_qualifiers() {
    let code = "function make() {\n  const a = new Map<string, number>();\n  const b = new ns.Widget();\n}\n";
    let r = extract("mk.ts", code);
    let inst: Vec<&str> = r
        .unresolved
        .iter()
        .filter(|u| u.reference_kind == "instantiates")
        .map(|u| u.reference_name.as_str())
        .collect();
    assert!(inst.contains(&"Map"), "generic args stripped: {inst:?}");
    assert!(inst.contains(&"Widget"), "qualifier stripped: {inst:?}");
}

#[test]
fn anonymous_module_wrapper_body_still_walked_528() {
    // IIFE wrapper: no node for the anonymous fn, but its inner NAMED
    // function extracts and inner calls attribute through.
    let code = "(function () {\n  function setup() {\n    boot();\n  }\n  setup();\n})();\n";
    let r = extract("wrapper.ts", code);
    let setup = find(&r, NodeKind::Function, "setup").unwrap();
    assert!(r.unresolved.iter().any(|u| u.reference_kind == "calls"
        && u.reference_name == "boot"
        && u.from_node_id == setup.id));
    // The wrapper-level `setup()` call attributes to the file.
    assert!(r.unresolved.iter().any(|u| u.reference_kind == "calls"
        && u.reference_name == "setup"
        && u.from_node_id == "file:wrapper.ts"));
    assert!(
        !r.nodes
            .iter()
            .any(|n| n.kind == NodeKind::Function && n.name == "<anonymous>")
    );
}

// =============================================================================
// Multi-declarator docstring sharing (Task 4 review rider)
// =============================================================================

#[test]
fn multi_declarator_statement_shares_leading_comment() {
    let code = "// shared statement comment\nconst a = 1, b = 2;\n";
    let r = extract("multi.ts", code);
    let a = find(&r, NodeKind::Constant, "a").unwrap();
    let b = find(&r, NodeKind::Constant, "b").unwrap();
    assert_eq!(a.docstring.as_deref(), Some("shared statement comment"));
    assert_eq!(
        b.docstring.as_deref(),
        Some("shared statement comment"),
        "every declarator receives the wrapper's comment (TS-parity)"
    );
}

// =============================================================================
// Snapshots (ts + tsx)
// =============================================================================

#[test]
fn representative_ts_fixture_snapshot() {
    let code = "// payment module\nimport { Stripe } from 'stripe';\n\nexport interface Receipt {\n  id: string;\n}\n\nexport type Currency = 'usd' | 'eur';\n\nexport class PaymentService {\n  private client: Stripe;\n\n  async charge(amount: number): Promise<Receipt> {\n    return this.client.charge(amount);\n  }\n}\n\nexport const format = (r: Receipt): string => r.id;\n\nconst MAX_AMOUNT = 10000;\n";
    let r = extract("pay.ts", code);
    insta::assert_yaml_snapshot!(r, {
        ".nodes[].updatedAt" => "[ts]",
        ".durationMs" => "[ms]",
    });
}

#[test]
fn representative_tsx_fixture_snapshot() {
    let code = "import React from 'react';\n\n// login button\nexport const LoginButton = ({ onClick }: { onClick: () => void }) => {\n  return <button onClick={onClick}>Login</button>;\n};\n\nexport default function Page() {\n  return <LoginButton onClick={() => login()} />;\n}\n";
    let r = extract_from_source("Login.tsx", code, Language::Tsx);
    insta::assert_yaml_snapshot!(r, {
        ".nodes[].updatedAt" => "[ts]",
        ".durationMs" => "[ms]",
    });
}

/// Inheritance-gap closure — TS `class_heritage` wraps `extends_clause` +
/// `implements_clause`, so the pass recurses into it (tree-sitter.ts:5517).
/// `interface Repo extends Serializable` is a legitimate NO-OP: the TS grammar
/// spells it `extends_type_clause`, which TS's arm list does not cover, so TS
/// emits nothing for it either — verified against the real TS build.
#[test]
fn extracts_typescript_class_inheritance_refs() {
    let code = "export interface Serializable {\n  serialize(): string;\n}\n\nexport interface Repo extends Serializable {\n  find(id: string): void;\n}\n\nclass BaseController {\n  handle(): void {}\n}\n\nexport class ChildController extends BaseController implements Serializable {\n  serialize(): string {\n    return '';\n  }\n}\n";
    let r = extract("inherit.ts", code);
    assert!(r.errors.is_empty(), "errors: {:?}", r.errors);

    let refs = |kind: &str| -> Vec<&str> {
        r.unresolved
            .iter()
            .filter(|u| u.reference_kind == kind)
            .map(|u| u.reference_name.as_str())
            .collect()
    };
    assert_eq!(refs("extends"), vec!["BaseController"]);
    assert_eq!(refs("implements"), vec!["Serializable"]);
}
