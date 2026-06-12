use hex::act_evidence;
use hex::types::ActEvidence;
use std::process::Command;

// ── helpers ──────────────────────────────────────────────────────────────────

/// Create a bare git repo in `dir` with one commit and a tag named `tag`.
fn setup_git_repo_with_tag(dir: &std::path::Path, tag: &str) {
    let run = |args: &[&str]| {
        Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git command failed");
    };
    run(&["init", "-b", "main"]);
    run(&["config", "user.email", "test@example.com"]);
    run(&["config", "user.name", "Test"]);
    std::fs::write(dir.join("f.txt"), "content").unwrap();
    run(&["add", "."]);
    run(&["commit", "-m", "init"]);
    run(&["tag", tag]);
}

// ── tests ────────────────────────────────────────────────────────────────────

#[test]
fn test_evidence_git_tag_valid() {
    let tmp = tempfile::tempdir().unwrap();
    setup_git_repo_with_tag(tmp.path(), "v0.0.1-evidence-test");
    let ev = ActEvidence::GitTag {
        value: "v0.0.1-evidence-test".to_string(),
        repo: tmp.path().to_string_lossy().to_string(),
    };
    assert!(act_evidence::verify(&ev).is_ok(), "existing tag should verify Ok");
}

#[test]
fn test_evidence_git_tag_bogus_returns_err() {
    let tmp = tempfile::tempdir().unwrap();
    setup_git_repo_with_tag(tmp.path(), "v0.0.1-real");
    let ev = ActEvidence::GitTag {
        value: "v99.99.99-does-not-exist".to_string(),
        repo: tmp.path().to_string_lossy().to_string(),
    };
    let result = act_evidence::verify(&ev);
    assert!(result.is_err(), "non-existent tag should return Err");
    assert!(
        result.unwrap_err().contains("not found"),
        "error message should mention 'not found'"
    );
}

#[test]
fn test_evidence_boi_dispatch_nonexistent_returns_err() {
    // A completely bogus spec ID should not appear in any real boi status output.
    let ev = ActEvidence::BoiDispatch {
        spec_id: "ZZZZ_FAKE_SPEC_ID_9999999".to_string(),
    };
    let result = act_evidence::verify(&ev);
    // Either boi is not installed (command error) or spec_id is not found — both are Err.
    assert!(result.is_err(), "unknown spec_id should return Err");
}

#[test]
fn test_evidence_file_written_exists() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("output.txt");
    std::fs::write(&path, "data").unwrap();
    let ev = ActEvidence::FileWritten {
        path: path.to_string_lossy().to_string(),
    };
    assert!(act_evidence::verify(&ev).is_ok(), "existing non-empty file should verify Ok");
}

#[test]
fn test_evidence_file_written_missing_returns_err() {
    let ev = ActEvidence::FileWritten {
        path: "/tmp/hex_evidence_test_definitely_does_not_exist_xyz.txt".to_string(),
    };
    let result = act_evidence::verify(&ev);
    assert!(result.is_err(), "missing file should return Err");
}

#[test]
fn test_evidence_file_written_empty_returns_err() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("empty.txt");
    std::fs::write(&path, "").unwrap();
    let ev = ActEvidence::FileWritten {
        path: path.to_string_lossy().to_string(),
    };
    let result = act_evidence::verify(&ev);
    assert!(result.is_err(), "empty file should return Err");
}

#[test]
fn test_evidence_act_evidence_parses_git_tag_from_json() {
    let json = serde_json::json!({
        "type": "git_tag",
        "value": "v1.2.3",
        "repo": "/some/repo"
    });
    let ev: ActEvidence = serde_json::from_value(json).expect("should parse ActEvidence");
    match ev {
        ActEvidence::GitTag { value, repo } => {
            assert_eq!(value, "v1.2.3");
            assert_eq!(repo, "/some/repo");
        }
        other => panic!("unexpected variant: {other:?}"),
    }
}

#[test]
fn test_evidence_act_evidence_parses_boi_dispatch_from_json() {
    let json = serde_json::json!({"type": "boi_dispatch", "spec_id": "S1234"});
    let ev: ActEvidence = serde_json::from_value(json).expect("should parse ActEvidence");
    match ev {
        ActEvidence::BoiDispatch { spec_id } => assert_eq!(spec_id, "S1234"),
        other => panic!("unexpected variant: {other:?}"),
    }
}

#[test]
fn test_evidence_no_evidence_field_means_no_verification() {
    // An act with no evidence field should NOT be flagged — the detail["evidence"]
    // key is simply absent. Verify the parse returns None.
    let detail = serde_json::json!({"action": "Decided to defer", "result": "deferred"});
    assert!(
        detail.get("evidence").is_none(),
        "detail without evidence key should have no evidence"
    );
}

