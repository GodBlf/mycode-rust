use mycode_core::plan::PlanFileManager;

#[test]
fn plan_manager_creates_saves_loads_and_resets_plans() {
    let work = tempfile::tempdir().expect("work tempdir");
    let mut manager = PlanFileManager::new(work.path());

    assert_eq!(manager.load().expect("load with no plan"), None);

    let first_path = manager.create().expect("create first plan");
    assert!(first_path.starts_with(work.path().join(".mycode/plans")));
    assert!(
        first_path
            .extension()
            .is_some_and(|extension| extension == "md")
    );

    manager.save("# First plan").expect("save first plan");
    assert_eq!(
        manager.load().expect("load first plan"),
        Some("# First plan".to_string())
    );

    manager.reset();
    assert_eq!(manager.load().expect("load after reset"), None);

    let second_path = manager.create().expect("create second plan");
    manager.save("# Second plan").expect("save second plan");

    assert_ne!(first_path, second_path);
    assert_eq!(
        manager.load().expect("load second plan"),
        Some("# Second plan".to_string())
    );
}

#[test]
fn plan_state_does_not_leak_between_workspaces() {
    let first_work = tempfile::tempdir().expect("first work tempdir");
    let second_work = tempfile::tempdir().expect("second work tempdir");
    let mut first = PlanFileManager::new(first_work.path());
    let mut second = PlanFileManager::new(second_work.path());

    first.create().expect("create first workspace plan");
    first.save("# First").expect("save first workspace plan");
    second.create().expect("create second workspace plan");
    second.save("# Second").expect("save second workspace plan");

    assert_eq!(
        first.load().expect("load first workspace plan"),
        Some("# First".to_string())
    );
    assert_eq!(
        second.load().expect("load second workspace plan"),
        Some("# Second".to_string())
    );
}
