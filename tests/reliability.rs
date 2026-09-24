mod fixture;
use casimir::{
    adapters, checkpoint,
    model::{Event, EventKind, Harness, Session},
    process,
    rerun::{rerun, RerunOpts},
    util,
};
use serde_json::json;
use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::OnceLock,
    time::{Duration, Instant},
};
fn setup() {
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(|| {
        let home = std::env::temp_dir().join(format!("casimir-reliability-{}", std::process::id()));
        std::env::set_var("CASIMIR_HOME", &home);
        std::env::set_var("CASIMIR_TEST_PASSWORD", "known \"quoted\" credential\\path");
        std::env::set_var("CASIMIR_CLAUDE_BIN", fixture::executable("fake-claude"));
        std::env::set_var("CASIMIR_LLM_CMD", fixture::executable("fake-llm"));
        std::env::set_var("CLAUDE_CONFIG_DIR", home.join("claude"));
    });
}

#[test]
fn subscription_subprocesses_discard_api_overrides_but_keep_subscription_login() {
    setup();
    let fixture = fixture::executable("supervisor");
    for (harness, forbidden, retained) in [
        (Harness::ClaudeCode, "anthropicApiKey", "claudeOauth"),
        (Harness::Codex, "codexApiKey", "codexAccessToken"),
    ] {
        let mut command = Command::new(&fixture);
        command
            .args(["--supervisor", "subscription-env"])
            .env("ANTHROPIC_API_KEY", "invalid-api-key")
            .env("CODEX_API_KEY", "invalid-api-key")
            .env("CLAUDE_CODE_OAUTH_TOKEN", "subscription-token")
            .env("CODEX_ACCESS_TOKEN", "subscription-token");
        util::subscription_command(&mut command, harness);
        let dir = tempfile::tempdir().unwrap();
        let output =
            process::capture(&mut command, b"", Duration::from_secs(5), Some(dir.path())).unwrap();
        let environment: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(environment[forbidden], false);
        assert_eq!(environment[retained], true);
    }
}
fn repo() -> tempfile::TempDir {
    setup();
    let dir = tempfile::tempdir().unwrap();
    for args in [
        vec!["init", "-q"],
        vec![
            "-c",
            "user.name=fixture",
            "-c",
            "user.email=f@f",
            "commit",
            "--allow-empty",
            "-qm",
            "initial",
        ],
    ] {
        assert!(Command::new("git")
            .args(args)
            .current_dir(dir.path())
            .status()
            .unwrap()
            .success());
    }
    dir
}
fn original(repo: &Path, turns: u32) -> Session {
    let mut session = Session::new(Harness::ClaudeCode);
    session.cwd = Some(repo.display().to_string());
    for turn in 1..=turns {
        session.events.push(Event::text(
            turn,
            "",
            EventKind::User,
            format!("task {turn}"),
        ));
    }
    session
}
fn capture(repo: &Path, run: &Path, limit: u64) -> anyhow::Result<String> {
    checkpoint::capture(checkpoint::Capture {
        cwd: repo,
        run_dir: run,
        native: None,
        harness: Harness::ClaudeCode,
        version: None,
        turn: 1,
        expected_conversation_turns: 0,
        prompt: "recorded prompt",
        configuration_hash: "configuration",
        limit,
    })
}
#[test]
fn checkpoint_preserves_index_ignored_binary_unicode_modes_and_symlinks() {
    let repo = repo();
    let run = tempfile::tempdir().unwrap();
    std::fs::write(repo.path().join(".gitignore"), "ignored.bin\n").unwrap();
    std::fs::write(repo.path().join("ignored.bin"), [0, 255, 128, 0]).unwrap();
    std::fs::write(repo.path().join("café.txt"), "staged\n").unwrap();
    assert!(Command::new("git")
        .args(["add", "café.txt"])
        .current_dir(repo.path())
        .status()
        .unwrap()
        .success());
    std::fs::write(repo.path().join("café.txt"), "unstaged\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            repo.path().join("café.txt"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        std::os::unix::fs::symlink("/outside/never-follow", repo.path().join("external-link"))
            .unwrap();
    }
    let id = capture(repo.path(), run.path(), checkpoint::DEFAULT_LIMIT).unwrap();
    let destination = run.path().join("restored");
    checkpoint::restore(&id, &destination).unwrap();
    assert_eq!(
        std::fs::read(destination.join("ignored.bin")).unwrap(),
        [0, 255, 128, 0]
    );
    assert_eq!(
        std::fs::read_to_string(destination.join("café.txt")).unwrap(),
        "unstaged\n"
    );
    let staged = Command::new("git")
        .args(["show", ":café.txt"])
        .current_dir(&destination)
        .output()
        .unwrap();
    assert_eq!(staged.stdout, b"staged\n");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(destination.join("café.txt"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
        assert_eq!(
            std::fs::read_link(destination.join("external-link")).unwrap(),
            PathBuf::from("/outside/never-follow")
        );
    }
    assert!(
        checkpoint::restore(&id, repo.path()).is_err(),
        "never reset original checkout"
    );
}
static CHECKPOINT_INTEGRITY_TEST: std::sync::Mutex<()> = std::sync::Mutex::new(());
#[test]
fn corrupt_checkpoint_fails_before_creating_worktree() {
    let _guard = CHECKPOINT_INTEGRITY_TEST.lock().unwrap();
    let repo = repo();
    let run = tempfile::tempdir().unwrap();
    std::fs::write(
        repo.path().join("unique-corrupt-me"),
        uuid::Uuid::new_v4().to_string(),
    )
    .unwrap();
    let id = capture(repo.path(), run.path(), checkpoint::DEFAULT_LIMIT).unwrap();
    let cp = checkpoint::load(&id).unwrap();
    let blob = cp
        .entries
        .iter()
        .find(|e| e.path == "unique-corrupt-me")
        .unwrap()
        .blob
        .as_ref()
        .unwrap();
    let path = util::casimir_home().join("checkpoints/blobs").join(blob);
    let bytes = std::fs::read(&path).unwrap();
    std::fs::write(&path, "corrupted").unwrap();
    assert!(checkpoint::restore(&id, &run.path().join("never-created")).is_err());
    assert!(!run.path().join("never-created").exists());
    std::fs::write(path, bytes).unwrap();
}
#[test]
fn storage_limit_and_exclusive_locks_are_enforced() {
    let repo = repo();
    let run = tempfile::tempdir().unwrap();
    assert!(capture(repo.path(), run.path(), 1).is_err());
    let first = util::RunLock::acquire(run.path()).unwrap();
    assert!(util::RunLock::acquire(run.path()).is_err());
    drop(first);
    assert!(util::RunLock::acquire(run.path()).is_ok());
}
#[test]
fn atomic_metadata_replacement_is_complete_under_readers() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.json");
    util::write_json(&path, &json!({"value":"a".repeat(32000)})).unwrap();
    let reader = {
        let path = path.clone();
        std::thread::spawn(move || {
            for _ in 0..100 {
                let value: serde_json::Value = util::read_json(&path).unwrap();
                assert_eq!(value["value"].as_str().unwrap().len(), 32000);
            }
        })
    };
    for _ in 0..20 {
        util::write_json(&path, &json!({"value":"b".repeat(32000)})).unwrap();
    }
    reader.join().unwrap();
}
#[test]
fn supervisor_times_out_and_bounds_response_memory() {
    setup();
    let start = Instant::now();
    let temp = tempfile::tempdir().unwrap();
    let result = process::capture(
        Command::new(fixture::executable("supervisor")).args(["--supervisor", "hang"]),
        b"",
        Duration::from_millis(120),
        Some(&temp.path().join("hung")),
    );
    assert!(result.is_err());
    assert!(start.elapsed() < Duration::from_secs(3));
    let result = process::capture(
        Command::new(fixture::executable("supervisor")).args(["--supervisor", "oversize"]),
        b"",
        Duration::from_secs(30),
        Some(&temp.path().join("oversize")),
    );
    assert!(result.err().unwrap().to_string().contains("exceeds 4 MiB"));
    assert!(
        std::fs::metadata(temp.path().join("oversize/stdout.log"))
            .unwrap()
            .len()
            >= 4 * 1024 * 1024
    );
}
#[test]
fn supervisor_terminates_descendants_on_timeout_and_parent_exit() {
    setup();
    for mode in ["child", "orphan", "separate-group"] {
        let temp = tempfile::tempdir().unwrap();
        let spool = temp.path().join("run");
        let result = process::capture(
            Command::new(fixture::executable("supervisor")).args(["--supervisor", mode]),
            b"",
            // A cold Windows runner may need longer to start the orphan's child.
            // Other modes still exercise prompt timeout cancellation.
            if mode == "orphan" {
                Duration::from_secs(3)
            } else {
                Duration::from_millis(250)
            },
            Some(&spool),
        );
        if mode != "orphan" {
            assert!(result.is_err());
        } else {
            assert!(result.is_ok(), "orphan cleanup failed: {:?}", result.err());
        }
        let pid = std::fs::read_to_string(spool.join("stdout.log"))
            .unwrap()
            .trim()
            .parse::<i32>()
            .unwrap();
        // Linux may retain a killed orphan as a zombie until PID 1 reaps it.
        #[cfg(unix)]
        let alive = unsafe { libc::kill(pid, 0) } == 0;
        #[cfg(unix)]
        if alive {
            #[cfg(target_os = "linux")]
            assert!(std::fs::read_to_string(format!("/proc/{pid}/stat"))
                .unwrap_or_default()
                .contains(") Z "));
            #[cfg(not(target_os = "linux"))]
            panic!("descendant survived cancellation");
        }
        #[cfg(windows)]
        unsafe {
            use windows_sys::Win32::{
                Foundation::{CloseHandle, WAIT_OBJECT_0},
                System::Threading::{OpenProcess, WaitForSingleObject},
            };
            let process = OpenProcess(0x00100000, 0, pid as u32);
            if !process.is_null() {
                assert_eq!(WaitForSingleObject(process, 2000), WAIT_OBJECT_0);
                CloseHandle(process);
            }
        }
    }
}
#[test]
fn permissions_default_to_preserve_and_passthrough_bypass_needs_consent() {
    let default = adapters::RunOpts::default();
    assert!(adapters::validate_permissions(&default).is_ok());
    for flag in [
        "--dangerously-skip-permissions",
        "--sandbox=danger-full-access",
        "--allow-all-tools",
        "--approval-mode=yolo",
    ] {
        let mut opts = default.clone();
        opts.extra_args = vec![flag.into()];
        assert!(adapters::validate_permissions(&opts).is_err(), "{flag}");
        opts.allow_unrestricted = true;
        assert!(adapters::validate_permissions(&opts).is_ok());
    }
    let opts = adapters::RunOpts {
        extra_args: vec!["--api-key=secret".into()],
        allow_unrestricted: true,
        ..Default::default()
    };
    assert!(adapters::validate_permissions(&opts).is_err());
    let opts = adapters::RunOpts {
        extra_args: vec!["forced_login_method=api".into()],
        ..Default::default()
    };
    assert!(adapters::run_turn(Harness::Codex, &opts, &mut |_| {}).is_err());
}
#[test]
fn failed_checks_cannot_be_overridden_by_judge_and_are_frozen() {
    let repo = repo();
    let run = tempfile::tempdir().unwrap();
    let source = run.path().join("source.json");
    util::write_json(&source,&json!({"schemaVersion":1,"checks":[{"executable":fixture::executable("supervisor"),"args":["--supervisor","fail"],"timeoutSecs":3,"expectedExitStatus":0}]})).unwrap();
    let (definition, hash) = casimir::checks::freeze(&source, run.path(), repo.path()).unwrap();
    std::fs::write(&source, "edited original").unwrap();
    let results = casimir::checks::execute(&definition, &hash, repo.path(), run.path()).unwrap();
    assert_eq!(results.outcome, "failed");
    let mut report = casimir::compare::Report {
        execution_status: "completed".into(),
        checks: Some(results),
        judge: Some(casimir::compare::Judgement {
            score_b: 10.0,
            evidence_a: vec!["evidence".into()],
            evidence_b: vec!["evidence".into()],
            ..Default::default()
        }),
        ..Default::default()
    };
    report.update_outcome(7.0);
    assert_eq!(report.overall_outcome, "failed");
    let mut saved = original(repo.path(), 1);
    saved.execution = Some(casimir::model::Execution {
        requested_turns: 1,
        completed_turns: 1,
        ..Default::default()
    });
    saved.evaluation = Some(json!({"checks":report.checks,"passThreshold":7.0}));
    let compared =
        casimir::compare::compare_sessions(&saved, &saved, None, None, report.judge.clone());
    assert_eq!(
        compared.overall_outcome, "failed",
        "standalone comparisons retain required check failures"
    );
    saved.evaluation = Some(json!({"checks":{"broken":"metadata"}}));
    assert_eq!(
        casimir::compare::compare_sessions(&saved, &saved, None, None, report.judge.clone())
            .overall_outcome,
        "inconclusive"
    );
    std::fs::write(run.path().join("checks.definition.json"), "tampered").unwrap();
    assert!(casimir::checks::execute(&definition, &hash, repo.path(), run.path()).is_err());
}
#[test]
fn ambiguous_turn_requires_explicit_retry_and_completed_run_is_noop() {
    let repo = repo();
    let output = tempfile::tempdir().unwrap();
    let external = tempfile::tempdir().unwrap();
    let opts = RerunOpts {
        workspace: repo.path().display().to_string(),
        out_dir: Some(output.path().to_path_buf()),
        quiet: true,
        extra_args: vec![
            "--fake-crash-once".into(),
            "--marker".into(),
            external.path().join("once").display().to_string(),
        ],
        ..Default::default()
    };
    let run = rerun(&original(repo.path(), 1), &opts, &mut |_| {}, &mut |_| {}).unwrap();
    assert_eq!(run.session.unwrap().execution.unwrap().failed_turns, 1);
    assert!(casimir::recovery::resume(output.path(), false, &mut |_| {}, &mut |_| {}).is_err());
    let recovered = casimir::recovery::resume(output.path(), true, &mut |_| {}, &mut |_| {})
        .unwrap()
        .unwrap();
    assert_ne!(recovered.run_dir, output.path());
    let session = recovered.session.unwrap();
    assert_eq!(session.execution.unwrap().completed_turns, 1);
    assert_eq!(
        session
            .events
            .iter()
            .filter(|e| e.kind == EventKind::User)
            .count(),
        1
    );
    assert!(!repo.path().join("out.txt").exists());
    assert!(
        casimir::recovery::resume(&recovered.run_dir, false, &mut |_| {}, &mut |_| {})
            .unwrap()
            .is_none()
    );
    assert!(
        casimir::recovery::resume(output.path(), true, &mut |_| {}, &mut |_| {}).is_err(),
        "duplicate recovery attempts are refused"
    );
}
#[test]
fn cleanup_previews_and_preserves_original_and_unowned_directories() {
    let repo = repo();
    let output = tempfile::tempdir().unwrap();
    let run = rerun(
        &original(repo.path(), 1),
        &RerunOpts {
            workspace: repo.path().display().to_string(),
            out_dir: Some(output.path().to_path_buf()),
            quiet: true,
            ..Default::default()
        },
        &mut |_| {},
        &mut |_| {},
    )
    .unwrap();
    let preview = casimir::artifacts::cleanup(output.path(), false).unwrap();
    assert_eq!(preview["preview"], true);
    assert!(run.workspace.dir.exists());
    assert!(casimir::artifacts::cleanup(repo.path(), true).is_err());
    assert!(
        !repo.path().join(".lock").exists(),
        "unowned cleanup must not modify the source"
    );
    casimir::artifacts::cleanup(output.path(), true).unwrap();
    assert!(!output.path().exists());
    assert!(!run.workspace.dir.exists());
    assert!(repo.path().join(".git").exists());
    let _guard = CHECKPOINT_INTEGRITY_TEST.lock().unwrap();
    let preview = casimir::artifacts::cleanup_with_checkpoints(output.path(), false, true).unwrap();
    assert!(preview["checkpoints"]["manifests"].as_u64().unwrap() > 0);
    casimir::artifacts::cleanup_with_checkpoints(output.path(), true, true).unwrap();
}
#[test]
fn shared_exports_redact_credentials_and_mark_redaction() {
    setup();
    let value = json!({"api_key":"very-secret","text":"Authorization: Bearer abcdefghijklmnopqrstuvwxyz and sk-ant-12345678901234567890","checkpoints":{"1":"private"}});
    let shared = casimir::privacy::share(&value);
    let text = shared.to_string();
    assert_eq!(shared["redacted"], true);
    assert!(!text.contains("very-secret"));
    assert!(!text.contains("abcdefghijklmnopqrstuvwxyz"));
    assert!(shared["data"].get("checkpoints").is_none());
    let secret = std::env::var("CASIMIR_TEST_PASSWORD").unwrap();
    let shared = casimir::privacy::share(&json!({"text":format!("password=\"{secret}\"")}));
    let encoded = serde_json::to_string(&shared["data"]).unwrap();
    assert!(!encoded.contains("quoted"));
    assert!(!encoded.contains("credential"));
    assert!(serde_json::from_str::<serde_json::Value>(&encoded).is_ok());
}

#[test]
fn interrupted_second_turn_retry_preserves_completed_first_turn() {
    let repo = repo();
    let output = tempfile::tempdir().unwrap();
    let external = tempfile::tempdir().unwrap();
    let options = RerunOpts {
        workspace: repo.path().display().to_string(),
        out_dir: Some(output.path().to_path_buf()),
        quiet: true,
        extra_args: vec![
            "--fake-crash-turn2-once".into(),
            "--marker".into(),
            external.path().join("once").display().to_string(),
        ],
        ..Default::default()
    };
    let first = rerun(
        &original(repo.path(), 2),
        &options,
        &mut |_| {},
        &mut |_| {},
    )
    .unwrap();
    let first_session = first.session.unwrap();
    assert!(
        first_session.cost_usd.is_none(),
        "an interrupted unmeasured turn makes total cost unknown"
    );
    let execution = first_session.execution.unwrap();
    assert_eq!(execution.completed_turns, 1);
    assert_eq!(execution.failed_turns, 1);
    let second = casimir::recovery::resume(output.path(), true, &mut |_| {}, &mut |_| {})
        .unwrap()
        .unwrap();
    let session = second.session.unwrap();
    assert_eq!(session.execution.unwrap().completed_turns, 2);
    assert_eq!(
        session
            .events
            .iter()
            .filter(|e| e.kind == EventKind::User && e.text_str() == "task 1")
            .count(),
        1
    );
    assert!(!second.run_dir.join("turns/1").exists());
    assert!(second.run_dir.join("turns/2/stdout.log").exists());
}

#[test]
fn native_executable_paths_and_arguments_preserve_unicode_and_metacharacters() {
    setup();
    let temp = tempfile::tempdir().unwrap();
    let directory = temp.path().join("space café");
    std::fs::create_dir(&directory).unwrap();
    let executable = directory.join(format!("fixture name{}", std::env::consts::EXE_SUFFIX));
    std::fs::copy(fixture::executable("supervisor"), &executable).unwrap();
    let output = process::capture(
        Command::new(&executable).args([
            "--supervisor",
            "args",
            "é & $(no-shell) \"quoted\"",
            r"C:\path with spaces\",
        ]),
        b"",
        Duration::from_secs(5),
        Some(&temp.path().join("output")),
    )
    .unwrap();
    let args: Vec<String> = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(args[2], "é & $(no-shell) \"quoted\"");
    assert_eq!(args[3], r"C:\path with spaces\");
    assert_eq!(
        casimir::compare::relativize(r"C:\Work\café.txt", Some(r"c:\work")),
        "café.txt"
    );
}

#[test]
fn malformed_internal_jsonl_is_not_silently_discarded() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("partial.jsonl");
    std::fs::write(&file, "{\"ok\":true}\n{\"partial\":").unwrap();
    assert_eq!(util::read_jsonl(&file).unwrap().len(), 1);
    std::fs::write(&file, "{\"ok\":true}\nnot json\n{\"ok\":true}\n").unwrap();
    assert!(util::read_jsonl(&file).is_err());
}

#[test]
fn calibration_requires_independent_reviews_and_check_failures_never_pass() {
    let directory = tempfile::tempdir().unwrap();
    let corpus = directory.path().join("corpus.json");
    let cases: Vec<_> = (0..40)
        .map(|i| json!({"id":format!("case-{i}"),"requiredCheckFailedB":i==0}))
        .collect();
    util::write_json(
        &corpus,
        &json!({"schemaVersion":1,"frozen":true,"cases":cases}),
    )
    .unwrap();
    let hash = checkpoint::hash(&std::fs::read(&corpus).unwrap());
    let labels:serde_json::Map<String,serde_json::Value>=(0..40).map(|i|(format!("case-{i}"),json!({"winner":"tie","outcomeA":"passed","outcomeB":if i==0 {"failed"} else {"passed"}}))).collect();
    let mut paths = Vec::new();
    for name in ["prediction", "reviewer-a", "reviewer-b", "adjudicator"] {
        let path = directory.path().join(format!("{name}.json"));
        let mut value = json!({"schemaVersion":1,"corpusHash":hash,"reviewerId":name,"humanReviewed":true,"labels":labels});
        if name == "prediction" {
            value["labels"]["case-0"]["outcomeB"] = json!("passed");
        }
        util::write_json(&path, &value).unwrap();
        paths.push(path);
    }
    let report =
        casimir::calibration::score(&corpus, &paths[0], &paths[1], &paths[2], &paths[3]).unwrap();
    assert!(report["agreement"].as_f64().unwrap() > 0.9);
    assert_eq!(report["falsePositives"], 1);
    assert_eq!(report["requiredCheckViolations"], 1);
    assert_eq!(report["passed"], false);
    assert!(
        casimir::calibration::score(&corpus, &paths[0], &paths[1], &paths[1], &paths[3]).is_err()
    );
}

#[test]
fn checkpoint_restores_after_source_checkout_and_git_objects_are_removed() {
    let repo = repo();
    let run = tempfile::tempdir().unwrap();
    std::fs::write(repo.path().join("staged.txt"), "index-only contents\n").unwrap();
    assert!(Command::new("git")
        .args(["add", "staged.txt"])
        .current_dir(repo.path())
        .status()
        .unwrap()
        .success());
    std::fs::write(repo.path().join("staged.txt"), "working contents\n").unwrap();
    let id = capture(repo.path(), run.path(), checkpoint::DEFAULT_LIMIT).unwrap();
    drop(repo);
    let destination = run.path().join("restored");
    checkpoint::restore(&id, &destination).unwrap();
    let staged = Command::new("git")
        .args(["show", ":staged.txt"])
        .current_dir(&destination)
        .output()
        .unwrap();
    assert!(staged.status.success());
    assert_eq!(staged.stdout, b"index-only contents\n");
    assert_eq!(
        std::fs::read(destination.join("staged.txt")).unwrap(),
        b"working contents\n"
    );
}

#[test]
fn checkpoint_cleanup_preserves_objects_shared_with_other_runs() {
    let _guard = CHECKPOINT_INTEGRITY_TEST.lock().unwrap();
    let repo = repo();
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    // A unique prompt/configuration prevents matching unrelated concurrent fixture captures.
    let config = uuid::Uuid::new_v4().to_string();
    let make = |owner: &Path| {
        checkpoint::capture(checkpoint::Capture {
            cwd: repo.path(),
            run_dir: owner,
            native: None,
            harness: Harness::ClaudeCode,
            version: None,
            turn: 1,
            expected_conversation_turns: 0,
            prompt: &config,
            configuration_hash: &config,
            limit: checkpoint::DEFAULT_LIMIT,
        })
        .unwrap()
    };
    let id = make(first.path());
    assert_eq!(id, make(second.path()));
    let preview = checkpoint::cleanup_owner(first.path(), false).unwrap();
    assert_eq!(preview["manifests"], 0);
    checkpoint::cleanup_owner(first.path(), true).unwrap();
    assert!(checkpoint::load(&id).is_ok());
    let preview = checkpoint::cleanup_owner(second.path(), false).unwrap();
    assert_eq!(preview["manifests"], 1);
    assert!(checkpoint::load(&id).is_ok());
    checkpoint::cleanup_owner(second.path(), true).unwrap();
    assert!(checkpoint::load(&id).is_err());
}

#[test]
fn interrupted_cleanup_can_resume_after_worktree_removal() {
    let repo = repo();
    let output = tempfile::tempdir().unwrap();
    let run = rerun(
        &original(repo.path(), 1),
        &RerunOpts {
            workspace: repo.path().display().to_string(),
            out_dir: Some(output.path().to_path_buf()),
            quiet: true,
            ..Default::default()
        },
        &mut |_| {},
        &mut |_| {},
    )
    .unwrap();
    assert!(Command::new("git")
        .args(["worktree", "remove", "--force"])
        .arg(&run.workspace.dir)
        .current_dir(repo.path())
        .status()
        .unwrap()
        .success());
    util::atomic_write(&output.path().join(".deleting"), b"interrupted cleanup").unwrap();
    assert!(util::RunLock::acquire(output.path()).is_err());
    casimir::artifacts::cleanup(output.path(), true).unwrap();
    assert!(!output.path().exists());
    assert!(repo.path().join(".git").exists());
}

#[cfg(unix)]
#[test]
fn storage_write_failure_keeps_atomic_metadata_and_aborts_streaming() {
    use std::os::unix::process::CommandExt;
    setup();
    let directory = tempfile::tempdir().unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "storage_fault_child", "--nocapture"])
        .env("CASIMIR_STORAGE_FAULT", directory.path())
        .env("CASIMIR_FAULT_FIXTURE", fixture::executable("supervisor"));
    unsafe {
        command.pre_exec(|| {
            libc::signal(libc::SIGXFSZ, libc::SIG_IGN);
            let limit = libc::rlimit {
                rlim_cur: 4096,
                rlim_max: 4096,
            };
            if libc::setrlimit(libc::RLIMIT_FSIZE, &limit) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}
#[cfg(unix)]
#[test]
fn storage_fault_child() {
    let Some(directory) = std::env::var_os("CASIMIR_STORAGE_FAULT").map(PathBuf::from) else {
        return;
    };
    let metadata = directory.join("metadata.json");
    util::write_json(&metadata, &json!({"state":"committed"})).unwrap();
    assert!(util::atomic_write(&metadata, &vec![b'x'; 8192]).is_err());
    assert_eq!(
        util::read_json::<serde_json::Value>(&metadata).unwrap()["state"],
        "committed"
    );
    let mut command = Command::new(std::env::var_os("CASIMIR_FAULT_FIXTURE").unwrap());
    command.args(["--supervisor", "flood"]);
    assert!(process::Process::spawn(
        &mut command,
        b"",
        Duration::from_secs(3),
        Some(&directory.join("spool"))
    )
    .and_then(process::Process::finish)
    .is_err());
    assert!(
        std::fs::metadata(directory.join("spool/stdout.log"))
            .unwrap()
            .len()
            <= 4096
    );
}

#[cfg(unix)]
#[test]
fn signal_cancellation_preserves_ambiguous_journal_and_reaps_children() {
    let repo = repo();
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("input.json");
    let run = directory.path().join("run");
    util::write_json(&input, &original(repo.path(), 1)).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_casimir"))
        .args(["rerun"])
        .arg(&input)
        .args(["--workspace"])
        .arg(repo.path())
        .args(["-o"])
        .arg(&run)
        .args(["--quiet", "--", "--supervisor", "child"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let started = Instant::now();
    let path = run.join("turns/1/stdout.log");
    let descendant = loop {
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Ok(pid) = text.trim().parse::<i32>() {
                break pid;
            }
        }
        if started.elapsed() > Duration::from_secs(10) {
            let _ = child.kill();
            panic!("fixture child did not start");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    unsafe {
        libc::kill(child.id() as i32, libc::SIGTERM);
    }
    while child.try_wait().unwrap().is_none() {
        if started.elapsed() > Duration::from_secs(15) {
            let _ = child.kill();
            panic!("cancellation did not complete");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_ne!(
        unsafe { libc::kill(descendant, 0) },
        0,
        "cancelled descendant remained alive"
    );
    let journal: serde_json::Value = util::read_json(&run.join("recovery.json")).unwrap();
    assert_eq!(journal["active"], true);
    assert!(casimir::recovery::resume(&run, false, &mut |_| {}, &mut |_| {}).is_err());
    assert!(!repo.path().join("out.txt").exists());
}

#[test]
fn frozen_predictions_are_not_human_reviews_and_preserve_required_failures() {
    setup();
    let output = tempfile::tempdir().unwrap();
    let corpus = Path::new(env!("CARGO_MANIFEST_DIR")).join("acceptance/evaluation/corpus.json");
    let options = casimir::llm::LlmOpts {
        backend: "cmd".into(),
        model: Some("judge-consistent".into()),
        ..Default::default()
    };
    let result = casimir::calibration::predict(&corpus, output.path(), &options).unwrap();
    assert_eq!(result["humanReviewed"], false);
    assert_eq!(result["labels"].as_object().unwrap().len(), 40);
    let original: serde_json::Value = util::read_json(&corpus).unwrap();
    for case in original["cases"].as_array().unwrap() {
        for suffix in ["A", "B"] {
            if case[format!("requiredCheckFailed{suffix}")] == true {
                assert_eq!(
                    result["labels"][case["id"].as_str().unwrap()][format!("outcome{suffix}")],
                    "failed"
                );
            }
        }
    }
    assert_eq!(
        result["corpusHash"],
        checkpoint::hash(&std::fs::read(&corpus).unwrap())
    );
    assert!(
        casimir::calibration::predict(&corpus, output.path(), &options).is_err(),
        "existing evidence cannot be overwritten"
    );
}

#[test]
fn failed_judge_keeps_successful_checks_inconclusive() {
    let checks = casimir::checks::Results {
        schema_version: 1,
        definition_hash: "frozen".into(),
        outcome: "passed".into(),
        results: vec![],
    };
    let mut session = Session::new(Harness::ClaudeCode);
    session.execution = Some(casimir::model::Execution {
        requested_turns: 1,
        completed_turns: 1,
        ..Default::default()
    });
    session.evaluation = Some(json!({"checks":checks,"judgeError":"HTTP 429"}));
    let report = casimir::compare::compare_sessions(&session, &session, None, None, None);
    assert_eq!(report.judge_assessment, "inconclusive");
    assert_eq!(report.overall_outcome, "inconclusive");
}

#[test]
fn cli_judges_disable_ambient_context_and_keep_system_text_off_arguments() {
    setup();
    let output = tempfile::tempdir().unwrap();
    let options = casimir::llm::LlmOpts {
        backend: "claude-cli".into(),
        model: Some("safe-context".into()),
        recording_dir: Some(output.path().to_path_buf()),
        ..Default::default()
    };
    let answer =
        casimir::llm::complete("fixture-private-system", "only supplied evidence", &options)
            .unwrap();
    let value: serde_json::Value = serde_json::from_str(&answer).unwrap();
    assert_eq!(value["system"], "fixture-private-system");
    assert_eq!(value["prompt"], "only supplied evidence");
    let cwd = PathBuf::from(value["cwd"].as_str().unwrap());
    assert_ne!(cwd, std::env::current_dir().unwrap());
    assert!(!cwd.exists(), "temporary helper workspace is removed");
}
