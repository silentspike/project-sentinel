use std::collections::BTreeMap;

use sentinel_common::nano_runtime::{
    NanoRuntimeRegistry, NanoWorkloadSpec, RUNTIME_BWRAP_LANDLOCK, RUNTIME_ECS_NATIVE,
};
use sentinel_common::AgentId;
use sentinel_runtime::EcsNativeRuntime;
use sentinel_sandbox::BwrapNanoRuntime;

#[cfg(feature = "wasm")]
use sentinel_common::nano_runtime::RUNTIME_WASM_WASMTIME;
#[cfg(feature = "wasm")]
use sentinel_wasm::WasmtimeNanoRuntime;

fn workload(id: &str, runtime_key: Option<&str>, agent_id: u16) -> NanoWorkloadSpec {
    NanoWorkloadSpec {
        workload_id: id.to_string(),
        runtime_key: runtime_key.map(str::to_string),
        agent_id: Some(AgentId(agent_id)),
        agent_name: format!("registry-agent-{agent_id}"),
        role: "Registry Tester".to_string(),
        room_id: "empfang".to_string(),
        shift_set: 1,
        command: Vec::new(),
        capabilities: Vec::new(),
        metadata: BTreeMap::new(),
        ecs_snapshot: None,
    }
}

#[test]
fn runtime_registry_routes_explicit_workload_keys() {
    let mut registry = NanoRuntimeRegistry::new(Some(RUNTIME_ECS_NATIVE.to_string()));
    registry.register(EcsNativeRuntime::new(8)).unwrap();
    registry.register(BwrapNanoRuntime::detect()).unwrap();
    #[cfg(feature = "wasm")]
    registry.register(WasmtimeNanoRuntime::new()).unwrap();

    assert!(registry.contains(RUNTIME_ECS_NATIVE));
    assert!(registry.contains(RUNTIME_BWRAP_LANDLOCK));
    #[cfg(feature = "wasm")]
    assert!(registry.contains(RUNTIME_WASM_WASMTIME));

    assert_eq!(
        registry
            .select_key(&workload("workload-a", Some(RUNTIME_ECS_NATIVE), 1))
            .unwrap(),
        RUNTIME_ECS_NATIVE
    );
    #[cfg(feature = "wasm")]
    assert_eq!(
        registry
            .select_key(&workload("workload-b", Some(RUNTIME_WASM_WASMTIME), 2))
            .unwrap(),
        RUNTIME_WASM_WASMTIME
    );
    assert_eq!(
        registry
            .select_key(&workload("workload-c", Some(RUNTIME_BWRAP_LANDLOCK), 3))
            .unwrap(),
        RUNTIME_BWRAP_LANDLOCK
    );
    assert_eq!(
        registry.select_key(&workload("fallback", None, 4)).unwrap(),
        RUNTIME_ECS_NATIVE
    );
}

struct OwnershipEscapeVisitor {
    file: String,
    violations: Vec<String>,
}

impl<'ast> syn::visit::Visit<'ast> for OwnershipEscapeVisitor {
    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        let test_only = node.attrs.iter().any(|attr| {
            attr.path().is_ident("cfg")
                && attr
                    .meta
                    .require_list()
                    .is_ok_and(|list| list.tokens.to_string().contains("test"))
        });
        if !test_only {
            syn::visit::visit_item_mod(self, node);
        }
    }

    fn visit_path(&mut self, node: &'ast syn::Path) {
        const RAW_OWNERS: &[&str] = &[
            "NanoRuntimeRegistry",
            "BwrapNanoRuntime",
            "EcsNativeRuntime",
            "MicrovmNanoRuntime",
            "WasmtimeNanoRuntime",
        ];
        for segment in &node.segments {
            if RAW_OWNERS.contains(&segment.ident.to_string().as_str()) {
                self.violations
                    .push(format!("{}:{}", self.file, segment.ident));
            }
        }
        syn::visit::visit_path(self, node);
    }

    fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
        const RAW_LIFECYCLE_METHODS: &[&str] = &[
            "setup_agent",
            "start_agent_process",
            "teardown_agent",
            "terminate_checked",
        ];
        if RAW_LIFECYCLE_METHODS.contains(&node.method.to_string().as_str()) {
            self.violations
                .push(format!("{}:.{}()", self.file, node.method));
        }
        syn::visit::visit_expr_method_call(self, node);
    }
}

fn rust_sources(root: &std::path::Path, files: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            // Standalone diagnostic/benchmark binaries do not participate in
            // the daemon's serving lifecycle. Their direct adapter use is
            // intentional and cannot mutate the live daemon registry.
            if path.file_name().is_some_and(|name| name == "bin") {
                continue;
            }
            rust_sources(&path, files);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
}

#[test]
fn ast_inventory_finds_no_raw_adapter_owner_outside_lifecycle_boundary() {
    // This is a static architecture inventory, not a compile-time proof. The
    // actual enforcement is Rust visibility: raw adapters and their registry
    // are private to the orchestrator's runtime_lifecycle submodule.
    let source_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let owner = source_root.join("orchestrator/runtime_lifecycle.rs");
    let mut files = Vec::new();
    rust_sources(&source_root, &mut files);
    let mut violations = Vec::new();
    for path in files {
        if path == owner {
            continue;
        }
        let source = std::fs::read_to_string(&path).unwrap();
        let parsed = syn::parse_file(&source).unwrap();
        let mut visitor = OwnershipEscapeVisitor {
            file: path.display().to_string(),
            violations: Vec::new(),
        };
        syn::visit::Visit::visit_file(&mut visitor, &parsed);
        violations.extend(visitor.violations);
    }
    assert!(
        violations.is_empty(),
        "raw adapter ownership escaped orchestrator/runtime_lifecycle.rs: {violations:?}"
    );
}
