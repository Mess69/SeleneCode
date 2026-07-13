//! Task 8 conformance: TS/JS core machinery — HOC components, store
//! collections (Zustand/RTK/Pinia/Vuex), import-binding + re-export refs,
//! and type-annotation references. Ported from the "Exported Variable"
//! block of `extraction.test.ts` and the v0-relevant cases of
//! `vue-store-extraction.test.ts` (extraction-level: function nodes +
//! unresolved refs; the DB-level joins of the TS test become ref asserts).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use selene_core::NodeKind;
use selene_extract::{ExtractionResult, Language, extract_from_source};

fn extract(path: &str, code: &str) -> ExtractionResult {
    extract_from_source(path, code, Language::Typescript)
}

fn find<'r>(r: &'r ExtractionResult, kind: NodeKind, name: &str) -> Option<&'r selene_core::Node> {
    r.nodes.iter().find(|n| n.kind == kind && n.name == name)
}

fn fn_named(r: &ExtractionResult, name: &str) -> bool {
    find(r, NodeKind::Function, name).is_some()
}

// =============================================================================
// Exported Variable block (Zustand / Zod / XState / #425)
// =============================================================================

#[test]
fn zod_schema_export_is_an_exported_constant() {
    let code = "\nexport const userSchema = z.object({\n  id: z.string(),\n  name: z.string(),\n  email: z.string().email(),\n});\n";
    let r = extract("schemas.ts", code);
    let v = find(&r, NodeKind::Constant, "userSchema").unwrap();
    assert_eq!(v.is_exported, Some(true));
}

#[test]
fn xstate_machine_export_is_an_exported_constant() {
    let code = "\nexport const authMachine = createMachine({\n  id: \"auth\",\n  initial: \"idle\",\n  states: {\n    idle: {},\n    authenticated: {},\n  },\n});\n";
    let r = extract("machine.ts", code);
    let v = find(&r, NodeKind::Constant, "authMachine").unwrap();
    assert_eq!(v.is_exported, Some(true));
}

#[test]
fn top_level_initializer_calls_are_captured_425() {
    let code = "\nimport { getTokenMp } from './api/upload';\n\nconst token = getTokenMp();\n";
    let r = extract("app.ts", code);
    assert!(
        r.unresolved
            .iter()
            .any(|u| u.reference_kind == "calls" && u.reference_name == "getTokenMp")
    );
}

#[test]
fn zustand_store_actions_become_function_nodes() {
    let code = "\nexport const useStore = create((set, get) => ({\n  user: null,\n  fetchUser: async (id) => {\n    const data = await api.load(id);\n    set({ user: data });\n  },\n}));\n";
    let r = extract("store.ts", code);
    let fetch_user = find(&r, NodeKind::Function, "fetchUser").unwrap();
    // The action's body calls attribute to it.
    assert!(r.unresolved.iter().any(|u| u.reference_kind == "calls"
        && u.reference_name == "api.load"
        && u.from_node_id == fetch_user.id));
}

// =============================================================================
// vue-store-extraction.test.ts — v0 cases
// =============================================================================

#[test]
fn vuex_module_collections_extract_with_bodies() {
    let code = "import { persistToken } from './auth-utils';\nconst state = { token: '' };\nconst mutations = {\n  SET_TOKEN: (state, token) => { state.token = token; },\n};\nconst actions = {\n  login({ commit }, info) {\n    persistToken(info.token);\n  },\n  async logout({ commit }) {\n    commit('SET_TOKEN', '');\n  },\n};\nexport default { namespaced: true, state, mutations, actions };\n";
    let r = extract_from_source("userModule.js", code, Language::Javascript);
    assert!(fn_named(&r, "login"), "vuex action login");
    assert!(fn_named(&r, "logout"), "vuex action logout");
    assert!(fn_named(&r, "SET_TOKEN"), "vuex mutation");
    // login's body call attributes to it (extraction-level assert of the
    // TS test's DB join).
    let login = find(&r, NodeKind::Function, "login").unwrap();
    assert!(r.unresolved.iter().any(|u| u.reference_kind == "calls"
        && u.reference_name == "persistToken"
        && u.from_node_id == login.id));
}

#[test]
fn pinia_options_store_collections_extract() {
    let code = "import { defineStore } from 'pinia';\nexport const useAuthStore = defineStore({\n  id: 'auth',\n  state: () => ({ name: '' }),\n  getters: {\n    upperName: state => state.name.toUpperCase(),\n  },\n  actions: {\n    async fetchMenu() { return loadMenu(); },\n    setName(n: string) { this.name = n; },\n  },\n});\n";
    let r = extract("authStore.ts", code);
    assert!(fn_named(&r, "fetchMenu"));
    assert!(fn_named(&r, "setName"));
    assert!(fn_named(&r, "upperName"));
}

#[test]
fn pinia_setup_store_local_actions_extract() {
    let code = "import { defineStore } from 'pinia';\nexport const useChatStore = defineStore('chat', () => {\n  const list = reactive([]);\n  const getList = async () => { return fetchList(); };\n  function pushItem(x) { list.push(x); }\n  return { list, getList, pushItem };\n});\n";
    let r = extract("chatStore.ts", code);
    assert!(fn_named(&r, "getList"));
    assert!(fn_named(&r, "pushItem"));
}

#[test]
fn non_store_file_const_actions_not_extracted() {
    // A plain module with a non-exported `const actions` object: no store
    // signals (need ≥2) → its members must NOT become function nodes.
    let code = "const actions = {\n  run: () => { work(); },\n};\nexport function use() { return actions; }\n";
    let r = extract("plain.ts", code);
    assert!(!fn_named(&r, "run"), "non-store actions stay unextracted");
}

// =============================================================================
// React HOC components (#841)
// =============================================================================

#[test]
fn hoc_wrapped_components_and_memo_util_guard() {
    let code = "export const Button = forwardRef((props, ref) => {\n  useTheme();\n  return null;\n});\nexport const Card = React.memo(() => null);\nexport const Fancy = styled.button`color: red;`;\nconst cache = memo(fn);\n";
    let r = extract("Button.tsx", code);
    let button = find(&r, NodeKind::Component, "Button").unwrap();
    assert_eq!(button.is_exported, Some(true));
    // The inline render fn's body calls attribute to the component.
    assert!(r.unresolved.iter().any(|u| u.reference_kind == "calls"
        && u.reference_name == "useTheme"
        && u.from_node_id == button.id));
    assert!(find(&r, NodeKind::Component, "Card").is_some());
    assert!(
        find(&r, NodeKind::Component, "Fancy").is_some(),
        "styled tag"
    );
    // Lowercase memoization util stays a constant (PascalCase gate).
    assert!(find(&r, NodeKind::Constant, "cache").is_some());
    assert!(find(&r, NodeKind::Component, "cache").is_none());
}

// =============================================================================
// RTK Query
// =============================================================================

#[test]
fn rtk_endpoints_and_hook_bindings() {
    let code = "export const api = createApi({\n  reducerPath: 'api',\n  endpoints: (build) => ({\n    getUser: build.query({\n      query: (id) => fetchUser(id),\n    }),\n    saveUser: build.mutation({\n      queryFn: (u) => persist(u),\n    }),\n  }),\n});\n\nexport const { useGetUserQuery, useSaveUserMutation, notAHook } = api;\n";
    let r = extract("api.ts", code);
    let get_user = find(&r, NodeKind::Function, "getUser").unwrap();
    assert!(r.unresolved.iter().any(|u| u.reference_kind == "calls"
        && u.reference_name == "fetchUser"
        && u.from_node_id == get_user.id));
    assert!(fn_named(&r, "saveUser"));
    // Generated hook destructures matching the convention become nodes.
    assert!(fn_named(&r, "useGetUserQuery"));
    assert!(fn_named(&r, "useSaveUserMutation"));
    assert!(!fn_named(&r, "notAHook"));
}

// =============================================================================
// Import-binding + re-export refs
// =============================================================================

#[test]
fn import_binding_refs_default_named_alias_namespace() {
    let code = "import Foo from './foo';\nimport { A, B as C } from './ab';\nimport * as NS from './ns';\nimport './side-effect';\n";
    let r = extract("imports.ts", code);
    let imports: Vec<&str> = r
        .unresolved
        .iter()
        .filter(|u| u.reference_kind == "imports")
        .map(|u| u.reference_name.as_str())
        .collect();
    assert!(imports.contains(&"Foo"), "default binding: {imports:?}");
    assert!(imports.contains(&"A"));
    assert!(imports.contains(&"C"), "LOCAL alias, not source name");
    assert!(imports.contains(&"NS"));
    // Module refs from the import nodes themselves are also present.
    assert!(imports.contains(&"./foo") && imports.contains(&"./side-effect"));
}

#[test]
fn re_export_refs_source_side_names() {
    let code = "export { helper, inner as outer } from './impl';\nexport * from './everything';\n";
    let r = extract("barrel.ts", code);
    let imports: Vec<&str> = r
        .unresolved
        .iter()
        .filter(|u| u.reference_kind == "imports")
        .map(|u| u.reference_name.as_str())
        .collect();
    assert!(imports.contains(&"helper"));
    assert!(
        imports.contains(&"inner"),
        "SOURCE-side name, not the alias"
    );
    assert!(!imports.contains(&"outer"));
}

// =============================================================================
// Type-annotation references
// =============================================================================

#[test]
fn type_refs_params_return_builtins_filtered() {
    let code = "export function render(model: ITextModel, count: number): RenderResult {\n  return draw(model);\n}\n";
    let r = extract("render.ts", code);
    let f_id = &find(&r, NodeKind::Function, "render").unwrap().id;
    let refs: Vec<&str> = r
        .unresolved
        .iter()
        .filter(|u| u.reference_kind == "references" && &u.from_node_id == f_id)
        .map(|u| u.reference_name.as_str())
        .collect();
    assert!(refs.contains(&"ITextModel"), "param type: {refs:?}");
    assert!(refs.contains(&"RenderResult"), "return type: {refs:?}");
    assert!(!refs.contains(&"number"), "builtins filtered: {refs:?}");
}

#[test]
fn interface_member_type_refs_attach_to_interface() {
    let code = "import type { IPage } from '../PromoterList';\nimport type { IOrderField } from '../types';\n\ninterface Hprops {\n  value?: Partial<IPage> & Partial<IOrderField>;\n}\n";
    let r = extract("HeaderFilter.ts", code);
    let refs: Vec<&str> = r
        .unresolved
        .iter()
        .filter(|u| u.reference_kind == "references")
        .map(|u| u.reference_name.as_str())
        .collect();
    assert!(refs.contains(&"IPage"), "refs: {refs:?}");
    assert!(refs.contains(&"IOrderField"));
}

#[test]
fn local_variable_type_annotation_refs_enclosing_function() {
    let code = "function build() {\n  const nodes: GraphNode[] = [];\n  return nodes;\n}\n";
    let r = extract("build.ts", code);
    let f_id = &find(&r, NodeKind::Function, "build").unwrap().id;
    assert!(r.unresolved.iter().any(|u| u.reference_kind == "references"
        && u.reference_name == "GraphNode"
        && &u.from_node_id == f_id));
}

#[test]
fn type_alias_value_refs() {
    let code = "type Editor = ITextModel | null;\n";
    let r = extract("alias.ts", code);
    let alias_id = &find(&r, NodeKind::TypeAlias, "Editor").unwrap().id;
    assert!(r.unresolved.iter().any(|u| u.reference_kind == "references"
        && u.reference_name == "ITextModel"
        && &u.from_node_id == alias_id));
}
