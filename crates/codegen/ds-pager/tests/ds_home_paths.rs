//! `DS_HOME` override tests in an isolated binary so `ds_home()`'s
//! process-wide `OnceLock` initializes from the overridden env var.

use std::path::PathBuf;

#[test]
fn ds_home_override_path_helpers() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let ds_home = tmp.path().to_path_buf();
    unsafe {
        std::env::set_var("DS_HOME", &ds_home);
    }

    assert_eq!(
        ds_pager::util::pager_toml_path(),
        ds_home.join("pager.toml")
    );
    assert_eq!(
        ds_pager::util::display_ds_home_prefix(),
        "$DS_HOME"
    );
    assert_eq!(
        ds_pager::util::display_user_ds_path("config.toml"),
        "$DS_HOME/config.toml"
    );

    let memory_path = ds_home.join("memory/MEMORY.md");
    assert_eq!(
        ds_pager::util::abbreviate_path(&memory_path.display().to_string()),
        "$DS_HOME/memory/MEMORY.md"
    );

    assert!(ds_pager::util::is_under_user_ds_home(&memory_path));
    assert!(!ds_pager::util::is_under_user_ds_home(
        PathBuf::from("/tmp/other").as_path()
    ));
}
