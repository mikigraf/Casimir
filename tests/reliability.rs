mod fixture;
use casimir::{adapters, checkpoint, model::{Event, EventKind, Harness, Session}, process, rerun::{rerun, RerunOpts}, util};
use serde_json::json;
use std::{path::{Path, PathBuf}, process::Command, sync::OnceLock, time::{Duration, Instant}};
fn setup() {
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(|| {
        let home = std::env::temp_dir().join(format!("casimir-reliability-{}",std::process::id()));
        std::env::set_var("CASIMIR_HOME", &home);
        std::env::set_var("CASIMIR_CLAUDE_BIN", fixture::executable("fake-claude"));
        std::env::set_var("CLAUDE_CONFIG_DIR", home.join("claude"));
    });
}
fn repo() -> tempfile::TempDir {
    setup(); let dir = tempfile::tempdir().unwrap();
    for args in [vec!["init","-q"],vec!["-c","user.name=fixture","-c","user.email=f@f","commit","--allow-empty","-qm","initial"]] {
        assert!(Command::new("git").args(args).current_dir(dir.path()).status().unwrap().success());
    }
    dir
}
fn original(repo: &Path, turns: u32) -> Session {
    let mut session = Session::new(Harness::ClaudeCode); session.cwd = Some(repo.display().to_string());
    for turn in 1..=turns { session.events.push(Event::text(turn,"",EventKind::User,format!("task {turn}"))); }
    session
}
fn capture(repo: &Path, run: &Path, limit: u64) -> anyhow::Result<String> {
    checkpoint::capture(checkpoint::Capture { cwd: repo, run_dir: run, native: None, harness: Harness::ClaudeCode, version: None, turn: 1, expected_conversation_turns: 0, prompt:"recorded prompt",configuration_hash:"configuration",limit })
}
#[test]
fn checkpoint_preserves_index_ignored_binary_unicode_modes_and_symlinks() {
    let repo = repo(); let run = tempfile::tempdir().unwrap();
    std::fs::write(repo.path().join(".gitignore"),"ignored.bin\n").unwrap();
    std::fs::write(repo.path().join("ignored.bin"),[0,255,128,0]).unwrap();
    std::fs::write(repo.path().join("café.txt"),"staged\n").unwrap();
    assert!(Command::new("git").args(["add","café.txt"]).current_dir(repo.path()).status().unwrap().success());
    std::fs::write(repo.path().join("café.txt"),"unstaged\n").unwrap();
    #[cfg(unix)] {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(repo.path().join("café.txt"),std::fs::Permissions::from_mode(0o755)).unwrap();
        std::os::unix::fs::symlink("/outside/never-follow",repo.path().join("external-link")).unwrap();
    }
    let id = capture(repo.path(),run.path(),checkpoint::DEFAULT_LIMIT).unwrap();
    let destination = run.path().join("restored"); checkpoint::restore(&id,&destination).unwrap();
    assert_eq!(std::fs::read(destination.join("ignored.bin")).unwrap(),[0,255,128,0]);
    assert_eq!(std::fs::read_to_string(destination.join("café.txt")).unwrap(),"unstaged\n");
    let staged = Command::new("git").args(["show",":café.txt"]).current_dir(&destination).output().unwrap();
    assert_eq!(staged.stdout,b"staged\n");
    #[cfg(unix)] {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(destination.join("café.txt")).unwrap().permissions().mode() & 0o777,0o755);
        assert_eq!(std::fs::read_link(destination.join("external-link")).unwrap(),PathBuf::from("/outside/never-follow"));
    }
    assert!(checkpoint::restore(&id,repo.path()).is_err(),"never reset original checkout");
}
#[test]
fn corrupt_checkpoint_fails_before_creating_worktree() {
    let repo = repo(); let run = tempfile::tempdir().unwrap();
    std::fs::write(repo.path().join("unique-corrupt-me"),uuid::Uuid::new_v4().to_string()).unwrap();
    let id = capture(repo.path(),run.path(),checkpoint::DEFAULT_LIMIT).unwrap();
    let cp = checkpoint::load(&id).unwrap(); let blob = cp.entries.iter().find(|e| e.path=="unique-corrupt-me").unwrap().blob.as_ref().unwrap();
    let path = util::casimir_home().join("checkpoints/blobs").join(blob); let bytes = std::fs::read(&path).unwrap();
    std::fs::write(&path,"corrupted").unwrap();
    assert!(checkpoint::restore(&id,&run.path().join("never-created")).is_err());
    assert!(!run.path().join("never-created").exists());
    std::fs::write(path,bytes).unwrap();
}
#[test]
fn storage_limit_and_exclusive_locks_are_enforced() {
    let repo = repo(); let run = tempfile::tempdir().unwrap();
    assert!(capture(repo.path(),run.path(),1).is_err());
    let first = util::RunLock::acquire(run.path()).unwrap();
    assert!(util::RunLock::acquire(run.path()).is_err()); drop(first);
    assert!(util::RunLock::acquire(run.path()).is_ok());
}
#[test]
fn atomic_metadata_replacement_is_complete_under_readers() {
    let dir = tempfile::tempdir().unwrap(); let path = dir.path().join("state.json");
    util::write_json(&path,&json!({"value":"a".repeat(32000)})).unwrap();
    let reader = {let path=path.clone(); std::thread::spawn(move || {for _ in 0..100 { let value: serde_json::Value = util::read_json(&path).unwrap(); assert_eq!(value["value"].as_str().unwrap().len(),32000); }})};
    for _ in 0..20 {util::write_json(&path,&json!({"value":"b".repeat(32000)})).unwrap();} reader.join().unwrap();
}
#[test]
fn supervisor_times_out_and_bounds_response_memory() {
    setup(); let start = Instant::now();
    let temp = tempfile::tempdir().unwrap();
    let result = process::capture(Command::new(fixture::executable("supervisor")).args(["--supervisor","hang"]),b"",Duration::from_millis(120),Some(&temp.path().join("hung")));
    assert!(result.is_err()); assert!(start.elapsed()<Duration::from_secs(3));
    let result = process::capture(Command::new(fixture::executable("supervisor")).args(["--supervisor","oversize"]),b"",Duration::from_secs(5),Some(&temp.path().join("oversize")));
    assert!(result.is_err()); assert!(std::fs::metadata(temp.path().join("oversize/stdout.log")).unwrap().len() >= 4*1024*1024);
}
#[cfg(unix)]
#[test]
fn supervisor_terminates_descendants_on_timeout_and_parent_exit() {
    setup();
    for mode in ["child","orphan"] {
        let temp = tempfile::tempdir().unwrap(); let spool=temp.path().join("run");
        let result=process::capture(Command::new(fixture::executable("supervisor")).args(["--supervisor",mode]),b"",Duration::from_millis(250),Some(&spool));
        if mode=="child" {assert!(result.is_err());} else {assert!(result.is_ok());}
        let pid=std::fs::read_to_string(spool.join("stdout.log")).unwrap().trim().parse::<i32>().unwrap();
        // Linux may retain a killed orphan as a zombie until PID 1 reaps it.
        let alive = unsafe {libc::kill(pid,0)} == 0;
        if alive {
            #[cfg(target_os="linux")] assert!(std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default().contains(") Z "));
            #[cfg(not(target_os="linux"))] panic!("descendant survived cancellation");
        }
    }
}
#[test]
fn permissions_default_to_preserve_and_passthrough_bypass_needs_consent() {
    let default=adapters::RunOpts::default(); assert!(adapters::validate_permissions(&default).is_ok());
    for flag in ["--dangerously-skip-permissions","--sandbox=danger-full-access","--allow-all-tools","--approval-mode=yolo"] {
        let mut opts=default.clone(); opts.extra_args=vec![flag.into()]; assert!(adapters::validate_permissions(&opts).is_err(),"{flag}");
        opts.allow_unrestricted=true; assert!(adapters::validate_permissions(&opts).is_ok());
    }
    let opts=adapters::RunOpts {extra_args:vec!["--api-key=secret".into()],allow_unrestricted:true,..Default::default()}; assert!(adapters::validate_permissions(&opts).is_err());
}
#[test]
fn failed_checks_cannot_be_overridden_by_judge_and_are_frozen() {
    let repo=repo(); let run=tempfile::tempdir().unwrap(); let source=run.path().join("source.json");
    util::write_json(&source,&json!({"schemaVersion":1,"checks":[{"executable":fixture::executable("supervisor"),"args":["--supervisor","fail"],"timeoutSecs":3,"expectedExitStatus":0}]})).unwrap();
    let (definition,hash)=casimir::checks::freeze(&source,run.path(),repo.path()).unwrap();
    std::fs::write(&source,"edited original").unwrap();
    let results=casimir::checks::execute(&definition,&hash,repo.path(),run.path()).unwrap(); assert_eq!(results.outcome,"failed");
    let mut report=casimir::compare::Report {execution_status:"completed".into(),checks:Some(results),judge:Some(casimir::compare::Judgement {score_b:10.0,evidence_a:vec!["evidence".into()],evidence_b:vec!["evidence".into()],..Default::default()}),..Default::default()}; report.update_outcome(7.0);assert_eq!(report.overall_outcome,"failed");
    std::fs::write(run.path().join("checks.definition.json"),"tampered").unwrap(); assert!(casimir::checks::execute(&definition,&hash,repo.path(),run.path()).is_err());
}
#[test]
fn ambiguous_turn_requires_explicit_retry_and_completed_run_is_noop() {
    let repo=repo(); let output=tempfile::tempdir().unwrap(); let external=tempfile::tempdir().unwrap();
    let opts=RerunOpts {workspace:repo.path().display().to_string(),out_dir:Some(output.path().to_path_buf()),quiet:true,
        extra_args:vec!["--fake-crash-once".into(),"--marker".into(),external.path().join("once").display().to_string()],..Default::default()};
    let run=rerun(&original(repo.path(),1),&opts,&mut |_|{},&mut |_|{}).unwrap(); assert_eq!(run.session.unwrap().execution.unwrap().failed_turns,1);
    assert!(casimir::recovery::resume(output.path(),false,&mut |_|{},&mut |_|{}).is_err());
    let recovered=casimir::recovery::resume(output.path(),true,&mut |_|{},&mut |_|{}).unwrap().unwrap();
    assert_ne!(recovered.run_dir,output.path()); let session=recovered.session.unwrap();assert_eq!(session.execution.unwrap().completed_turns,1);
    assert_eq!(session.events.iter().filter(|e|e.kind==EventKind::User).count(),1);
    assert!(!repo.path().join("out.txt").exists());
    assert!(casimir::recovery::resume(&recovered.run_dir,false,&mut |_|{},&mut |_|{}).unwrap().is_none());
    assert!(casimir::recovery::resume(output.path(),true,&mut |_|{},&mut |_|{}).is_err(),"duplicate recovery attempts are refused");
}
#[test]
fn cleanup_previews_and_preserves_original_and_unowned_directories() {
    let repo=repo();let output=tempfile::tempdir().unwrap();
    let run=rerun(&original(repo.path(),1),&RerunOpts {workspace:repo.path().display().to_string(),out_dir:Some(output.path().to_path_buf()),quiet:true,..Default::default()},&mut |_|{},&mut |_|{}).unwrap();
    let preview=casimir::artifacts::cleanup(output.path(),false).unwrap();assert_eq!(preview["preview"],true);assert!(run.workspace.dir.exists());
    assert!(casimir::artifacts::cleanup(repo.path(),true).is_err());
    casimir::artifacts::cleanup(output.path(),true).unwrap();assert!(!output.path().exists());assert!(!run.workspace.dir.exists());assert!(repo.path().join(".git").exists());
}
#[test]
fn shared_exports_redact_credentials_and_mark_redaction() {
    let value=json!({"api_key":"very-secret","text":"Authorization: Bearer abcdefghijklmnopqrstuvwxyz and sk-ant-12345678901234567890","checkpoints":{"1":"private"}});
    let shared=casimir::privacy::share(&value);let text=shared.to_string();assert_eq!(shared["redacted"],true);assert!(!text.contains("very-secret"));assert!(!text.contains("abcdefghijklmnopqrstuvwxyz"));assert!(shared["data"].get("checkpoints").is_none());
}

#[test]
fn interrupted_second_turn_retry_preserves_completed_first_turn() {
    let repo=repo();let output=tempfile::tempdir().unwrap();let external=tempfile::tempdir().unwrap();
    let options=RerunOpts {workspace:repo.path().display().to_string(),out_dir:Some(output.path().to_path_buf()),quiet:true,
        extra_args:vec!["--fake-crash-turn2-once".into(),"--marker".into(),external.path().join("once").display().to_string()],..Default::default()};
    let first=rerun(&original(repo.path(),2),&options,&mut |_|{},&mut |_|{}).unwrap();
    let execution=first.session.unwrap().execution.unwrap();assert_eq!(execution.completed_turns,1);assert_eq!(execution.failed_turns,1);
    let second=casimir::recovery::resume(output.path(),true,&mut |_|{},&mut |_|{}).unwrap().unwrap();
    let session=second.session.unwrap();assert_eq!(session.execution.unwrap().completed_turns,2);
    assert_eq!(session.events.iter().filter(|e|e.kind==EventKind::User && e.text_str()=="task 1").count(),1);
    assert!(!second.run_dir.join("turns/1").exists());assert!(second.run_dir.join("turns/2/stdout.log").exists());
}
