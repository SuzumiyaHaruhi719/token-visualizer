use core::paths::{app_data_dir, default_db_path, project_name_from_cwd};

#[test]
fn basename_of_cwd_is_project() {
    assert_eq!(
        project_name_from_cwd(r"C:\Users\Thomas\Documents\8111Reader"),
        "8111Reader"
    );
    assert_eq!(
        project_name_from_cwd("/c/Users/Thomas/Documents/Projects/CorePilot"),
        "CorePilot"
    );
}

#[test]
fn empty_cwd_falls_back() {
    assert_eq!(project_name_from_cwd(""), "unknown");
}

#[test]
fn trailing_separators_are_ignored() {
    assert_eq!(
        project_name_from_cwd(r"C:\Users\Thomas\CorePilot\"),
        "CorePilot"
    );
    assert_eq!(project_name_from_cwd("/home/user/proj//"), "proj");
}

#[test]
fn mixed_separators_take_last_segment() {
    assert_eq!(
        project_name_from_cwd(r"/c/Users\Thomas/Documents\Mixed"),
        "Mixed"
    );
}

#[test]
fn whitespace_only_falls_back() {
    assert_eq!(project_name_from_cwd("   "), "unknown");
}

/// Read-only invariant: the default DB must live under an app-data dir that
/// contains "claude-monitor", and must NOT live under a `.claude` tree.
#[test]
fn store_path_not_under_dotclaude() {
    let db = default_db_path().expect("db path");
    let s = db.to_string_lossy().replace('\\', "/").to_lowercase();
    assert!(
        s.contains("claude-monitor"),
        "db path should contain 'claude-monitor': {s}"
    );
    assert!(
        !s.contains("/.claude/"),
        "db path must not be under a .claude dir: {s}"
    );
    assert!(s.ends_with("db.sqlite"), "db file should be db.sqlite: {s}");

    let dir = app_data_dir().expect("app data dir");
    let ds = dir.to_string_lossy().replace('\\', "/").to_lowercase();
    assert!(ds.contains("claude-monitor"));
    assert!(!ds.contains("/.claude/"));
}
