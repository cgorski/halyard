#[cfg(not(feature = "__internal_erase_components"))]
#[test]
fn ui() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/server.rs");
}
