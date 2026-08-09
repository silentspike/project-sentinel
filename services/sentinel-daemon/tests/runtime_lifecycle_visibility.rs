#[test]
fn productive_runtime_registry_owner_cannot_be_bypassed() {
    let cases = trybuild::TestCases::new();
    cases.compile_fail("tests/ui/runtime_registry_bypass.rs");
}
