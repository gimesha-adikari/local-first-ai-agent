#[test]
fn reports_that_the_core_is_ready() {
    assert_eq!(agent_core::status(), "Agent core is ready.");
}
