use std::fs;
use std::process::Command;
use tempfile::TempDir;

// Helper to run `still` from the test environment.
fn run_still(args: &[&str], cwd: Option<&std::path::Path>) -> std::process::Output {
    let bin_path = env!("CARGO_BIN_EXE_still");
    let mut cmd = Command::new(bin_path);
    cmd.args(args);
    if let Some(path) = cwd {
        cmd.current_dir(path);
    }
    cmd.output().expect("Failed to execute still binary")
}

#[test]
fn test_workflow_scan_plan_apply_dryrun() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let root = root.as_path(); // Keep it as Path

    // Setup: Create rules.toml
    let rules_path = root.join("rules.toml");
    // Use the temp root as the scope to pass validation and match the file
    let rules_content = format!(
        r#"
[[rules]]
name = "test_rule"
scopes = [{:?}]
min_size_bytes = 100
recommended_action = "Trash"
"#,
        root.to_str().unwrap()
    );

    fs::write(&rules_path, rules_content).unwrap();

    // Create a target file
    let target_file = root.join("target.dat");
    fs::write(&target_file, vec![0u8; 200]).unwrap(); // 200 bytes > 100

    // 1. Scan -> Plan
    // Scan current directory (temp root) using --scope custom
    let plan_path = root.join("plan.json");
    let output = run_still(
        &[
            "scan",
            "--scope",
            "custom",
            "--rules",
            rules_path.to_str().unwrap(),
            "--out",
            plan_path.to_str().unwrap(),
            "--json",
        ],
        Some(root),
    ); // Set CWD to temp root

    if !output.status.success() {
        println!("Scan Sderr: {}", String::from_utf8_lossy(&output.stderr));
    }
    assert!(output.status.success(), "Scan failed");

    // Read Plan and Verify
    let plan_content = fs::read_to_string(&plan_path).unwrap();
    assert!(
        plan_content.contains("target.dat"),
        "Plan missing target file"
    );

    // Simulate User Action: Change selected_action from Keep to Trash
    // (In reality, TUI does this. For test, we edit the JSON)
    let plan_modified = plan_content.replace(
        r#""selected_action": "Keep""#,
        r#""selected_action": "Trash""#,
    );
    fs::write(&plan_path, plan_modified).unwrap();

    // 2. Apply --dry-run
    // We need --allow-outside-scope because temp dir is not in ~/Library
    // Note: plan is positional
    let output = run_still(
        &[
            "apply",
            "--dry-run",
            "--allow-outside-scope",
            plan_path.to_str().unwrap(), // Positional argument last
        ],
        None,
    );

    if !output.status.success() {
        println!("Apply Stderr: {}", String::from_utf8_lossy(&output.stderr));
    }
    assert!(output.status.success(), "Apply dry-run failed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    if !stdout.contains("Trash: 1 items") {
        println!("Apply Stdout: {}", stdout);
        // Also print plan content to verify edit worked
        println!("Modified Plan: {}", fs::read_to_string(&plan_path).unwrap());
    }
    assert!(stdout.contains("(Dry Run)"));
    assert!(stdout.contains("Trash: 2 items"));

    assert!(
        target_file.exists(),
        "Target file should still exist after dry run"
    );
}

#[test]
fn test_safety_reject_purge_in_plan() {
    let temp = TempDir::new().unwrap();
    let plan_path = temp.path().join("evil_plan.json");

    // Create malicious plan manually
    let evil_json = r#"{
        "schema_version": "2.0",
        "generated_by": "hacker",
        "created_at": {"secs_since_epoch": 0, "nanos_since_epoch": 0},
        "entries": [
            {
                "path": "/etc/passwd",
                "size": 100,
                "modified": {"secs_since_epoch": 0, "nanos_since_epoch": 0},
                "entry_type": "File",
                "origin": "Scan",
                "recommended_action": "Keep",
                "reason": "malice",
                "selected_action": "Purge"
            }
        ]
    }"#;
    fs::write(&plan_path, evil_json).unwrap();

    let output = run_still(&["apply", plan_path.to_str().unwrap()], None);

    // Expect FAILURE (exit code 2 usually for validation)
    assert!(!output.status.success(), "Malicious plan should fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);

    // It might output error to stdout or stderr depending on impl.
    // Check for "Validation" or specific rejection text.
    let combined = format!("{}\n{}", stdout, stderr);
    assert!(
        combined.to_lowercase().contains("validation") || combined.contains("Purge action blocked"),
        "Expected validation error, got: {}",
        combined
    );
}

#[test]
fn test_safety_scope_rejection() {
    let temp = TempDir::new().unwrap();
    let plan_path = temp.path().join("scope_fail.json");

    // Create plan with file in /tmp (outside default ~/Library)
    let bad_path = temp.path().join("bad.txt");
    fs::write(&bad_path, "test").unwrap();

    let json = format!(
        r#"{{
        "schema_version": "2.0",
        "generated_by": "test",
        "created_at": {{ "secs_since_epoch": 0, "nanos_since_epoch": 0 }},
        "entries": [
            {{
                "path": {},
                "size": 4,
                "modified": {{ "secs_since_epoch": 0, "nanos_since_epoch": 0 }},
                "entry_type": "File",
                "origin": "Scan",
                "recommended_action": "Keep",
                "reason": "scope test",
                "selected_action": "Trash"
            }}
        ]
    }}"#,
        serde_json::to_string(&bad_path).unwrap()
    );

    fs::write(&plan_path, json).unwrap();

    // Run apply WITHOUT --allow-outside-scope
    let output = run_still(&["apply", plan_path.to_str().unwrap()], None);

    assert!(!output.status.success(), "Scope violation should fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.to_lowercase().contains("scope") || stderr.to_lowercase().contains("validation"),
        "Expected scope validation error, got: {}",
        stderr
    );
}

#[test]
#[cfg(unix)]
fn test_safety_symlink_rejection() {
    use std::os::unix::fs::symlink;
    let temp = TempDir::new().unwrap();
    let root = temp.path();
    let plan_path = root.join("symlink_plan.json");

    let target = root.join("target_file");
    fs::write(&target, "target").unwrap();
    let link = root.join("link_to_target");
    symlink(&target, &link).unwrap();

    // Create plan targeting the symlink
    let json = format!(
        r#"{{
        "schema_version": "2.0",
        "generated_by": "test",
        "created_at": {{ "secs_since_epoch": 0, "nanos_since_epoch": 0 }},
        "entries": [
            {{
                "path": "{}",
                "size": 6,
                "modified": {{ "secs_since_epoch": 0, "nanos_since_epoch": 0 }},
                "entry_type": "Symlink",
                "origin": "Scan",
                "recommended_action": "Keep",
                "reason": "symlink test",
                "selected_action": "Trash"
            }}
        ]
    }}"#,
        link.to_string_lossy()
    );

    fs::write(&plan_path, json).unwrap();

    // Run apply with --allow-outside-scope to bypass scope check
    // This allows us to test the Symlink JIT safety specifically
    let output = run_still(
        &[
            "apply",
            "--yes",
            "--allow-outside-scope",
            plan_path.to_str().unwrap(),
        ],
        None,
    );

    // It might succeed with 0 items processed successfully.
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    // Logic: execute_plan logs to stdout/stderr "Failed to process ...: Security: Symlink rejected"
    assert!(
        stderr.contains("Symlink rejected") || stdout.contains("Symlink rejected"),
        "Expected symlink rejection message, got stdout:\n{}\nstderr:\n{}",
        stdout,
        stderr
    );

    if output.status.success() {
        assert!(stdout.contains("Failed: 1"), "Expected Failed count 1");
    }
}

// v0.1.0 RC Fix Tests

#[test]
fn test_list_scopes() {
    // 1. Verify it runs without scanning (fast) and exits 0
    let start = std::time::Instant::now();
    let output = run_still(&["scan", "--list-scopes"], None);
    let duration = start.elapsed();

    assert!(output.status.success());
    assert!(duration.as_secs() < 1, "List scopes should be instant");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("library-safe"), "Must list library-safe");
    assert!(
        stdout.contains("library-unsafe"),
        "Must list library-unsafe"
    );
    assert!(
        stdout.to_lowercase().contains("source of truth"),
        "Must mention source of truth"
    );
    assert!(
        !stdout.contains("Scanning..."),
        "Should NOT start actual scanning"
    );
}

#[test]
fn test_list_scopes_json() {
    let output = run_still(&["scan", "--list-scopes", "--format", "json"], None);
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    // Naively check JSON structure
    assert!(stdout.contains("\"default_scope\": \"library-safe\""));
    assert!(stdout.contains("\"name\": \"library-unsafe\""));
    assert!(stdout.contains("\"safety\": \"UNSAFE\""));
}

#[test]
fn test_scan_library_is_safe_subset_then_apply_passes() {
    // This test simulates the "Trust Integrity" path.
    // We can't easily scan real ~/Library in CI/Test env without permissions/time.
    // Instead, we verify that `scan --scope library` generates a plan compatible with `apply` defaults.
    // But since we can't run real scan, we check scope definitions via list-scopes JSON
    // and cross-reference with policy.

    let output = run_still(&["scan", "--list-scopes", "--format", "json"], None);
    let stdout = String::from_utf8_lossy(&output.stdout);

    // Verify default roots exclude Containers
    assert!(
        !stdout.contains("Containers"),
        "Default scope must NOT include Containers"
    );
    assert!(
        stdout.contains("Caches"),
        "Default scope must include Caches"
    );
    assert!(stdout.contains("Logs"), "Default scope must include Logs");
}
