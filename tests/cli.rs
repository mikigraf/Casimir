mod fixture;
use casimir::adapters::{claude_code, codex, copilot, gemini, load_session_file, resolve_session};
use casimir::brief::{draft_brief, intent_coverage, Brief};
use casimir::compare::{compare_sessions, end_state_similarity, first_divergent_turn, judge_sessions, judge_sessions_with, relativize, render_compare_markdown, render_compare_text, sequence_similarity, JudgeOpts};
use casimir::pairs::{export_pairs, score_pairs};
use casimir::llm::LlmOpts;
use casimir::model::{actions, anti_patterns, classify_shell, files_touched, final_assistant_text, is_validation_command, lexicon_of, model_family, simulator_drift, stats, tool_one_liner, user_turns, ActionKind, Event, EventKind, Harness, Session, Usage};
use casimir::render::{render_markdown, render_transcript, RenderOpts};
use casimir::rerun::{attribute, rerun, rerun_matrix, verdict, wilson, RerunOpts};
use casimir::simulate::{simulate_user_turn, SimState};
use casimir::util::extract_json;
use casimir::workspace::{parse_patch, Diff};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Once;

fn fx(name: &str) -> PathBuf {
    if name.starts_with("fake-") { return fixture::executable(name); }
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
}

static COUNTER: AtomicUsize = AtomicUsize::new(0);
static INIT: Once = Once::new();

fn tmp(prefix: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let p = std::env::temp_dir().join(format!("casimir-{prefix}-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&p).unwrap();
    p
}

/// Point the harness executables at the fake scripts (process-wide, set once).
fn setup_env() {
    INIT.call_once(|| {
        std::env::set_var("CASIMIR_CLAUDE_BIN", fx("fake-claude.sh"));
        std::env::set_var("CASIMIR_CODEX_BIN", fx("fake-codex.sh"));
        std::env::set_var("CASIMIR_COPILOT_BIN", fx("fake-stream.py"));
        std::env::set_var("CASIMIR_GEMINI_BIN", fx("fake-stream.py"));
        std::env::set_var("CASIMIR_HOME", tmp("home"));
        std::env::set_var("CASIMIR_LLM_CMD", fx("fake-llm.py"));
        std::env::set_var("COPILOT_HOME", fx("copilot"));
        std::env::set_var("GEMINI_CLI_HOME", fx("gemini"));
        std::env::set_var("CODEX_HOME", tmp("codexhome"));
        std::env::set_var("CLAUDE_CONFIG_DIR", tmp("claudehome"));
    });
}

fn tmp_repo() -> PathBuf {
    let dir = tmp("repo");
    let git = |args: &[&str]| {
        let st = Command::new("git").args(["-c", "user.email=t@t", "-c", "user.name=t"]).args(args).current_dir(&dir).status().unwrap();
        assert!(st.success(), "git {args:?}");
    };
    git(&["init", "-q"]);
    std::fs::write(dir.join("README.md"), "seed\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "seed"]);
    dir
}

fn no_log(_: &str) {}

fn checkpointed_original(repo: &Path) -> Session {
    let mut original = claude_code::parse_file(&fx("claude-code.jsonl")).unwrap();
    original.model = Some("fake-model".into());
    original.cwd = Some(repo.display().to_string());
    let native = claude_code::prepare_fork(&original, 2, &uuid::Uuid::new_v4().to_string(), repo).unwrap();
    let configuration_hash = casimir::checkpoint::hash(&serde_json::to_vec(&json!({"harness":Harness::ClaudeCode,"permissionMode":"preserve","sandbox":"preserve","extraArgs":[],"checksHash":null})).unwrap());
    let id = casimir::checkpoint::capture(casimir::checkpoint::Capture { cwd: repo, run_dir: &tmp("checkpoint-source"), native: Some(&native), harness: Harness::ClaudeCode, version: Some("casimir-fixture 1.0.0".into()), turn: 2, expected_conversation_turns: 1, prompt: &user_turns(&original)[1].text, configuration_hash: &configuration_hash, limit: casimir::checkpoint::DEFAULT_LIMIT }).unwrap();
    original.checkpoints.insert(2, id);
    original.configuration_hash = Some(configuration_hash);
    original.evaluation = Some(json!({"outcome":"failed","execution":"completed","judgeModel":"fake:judge","passThreshold":7.0}));
    original
}

#[test]
fn nested_harness_cleanup_preserves_authentication_and_configuration() {
    for key in ["CLAUDECODE", "CLAUDE_PID", "CLAUDE_AGENT_SDK_VERSION", "CLAUDE_CODE_ENTRYPOINT"] {
        assert!(casimir::util::is_nested_harness_var(key), "{key}");
    }
    for key in ["CLAUDE_CODE_OAUTH_TOKEN", "CLAUDE_CODE_USE_BEDROCK", "CLAUDE_CODE_USE_VERTEX", "CLAUDE_CODE_SANDBOXED", "ANTHROPIC_API_KEY", "CLAUDE_CONFIG_DIR", "CODEX_HOME"] {
        assert!(!casimir::util::is_nested_harness_var(key), "{key}");
    }
}

#[test]
fn simulator_cannot_claim_verbatim_or_goals_met_with_invalid_output() {
    setup_env();
    let original = claude_code::parse_file(&fx("claude-code.jsonl")).unwrap();
    for mode in ["sim-malformed", "sim-false-verbatim"] {
        let mut state = SimState::default();
        let result = simulate_user_turn(&original, &original, 2, &fake_llm(mode), &mut state).unwrap();
        assert_eq!(result.message, Some(user_turns(&original)[1].text.clone()));
        assert!(result.verbatim);
        assert_eq!(result.retries, 3);
        assert!(result.stop_reason.is_none());
        assert!(state.memory.is_empty(), "rejected replies must not poison simulator memory");
    }
}

#[test]
fn skipped_turns_keep_original_alignment_in_pairs_and_exports() {
    setup_env();
    let mut original = claude_code::parse_file(&fx("claude-code.jsonl")).unwrap();
    original.events.push(Event::text(3, "2026-09-21T10:10:00Z", EventKind::User, "third original request"));
    let repo = tmp_repo();
    let output = tmp("skip-alignment");
    let opts = RerunOpts { workspace: repo.display().to_string(), out_dir: Some(output.clone()), user_mode: "simulate".into(), sim_llm: fake_llm("sim-skip2"), judge_llm: fake_llm("brief"), quiet: true, ..Default::default() };
    let result = rerun(&original, &opts, &mut no_log, &mut no_log).unwrap();
    let s = result.session.unwrap();
    let adapted = s.events.iter().find(|e| e.kind == EventKind::User && e.turn == 2).unwrap();
    assert_eq!(adapted.source_turn, Some(3));
    assert_eq!(adapted.text_str(), "adapted third request");
    let pairs = export_pairs(std::slice::from_ref(&output), &tmp("skip-pairs")).unwrap();
    let row: serde_json::Value = serde_json::from_str(std::fs::read_to_string(pairs.pairs_path).unwrap().trim()).unwrap();
    assert!(row["candidates"].as_array().unwrap().iter().any(|c| c["message"] == "third original request"));
    let loaded = load_session_file(&output).unwrap();
    assert!(loaded.events.iter().any(|e| e.source_turn == Some(3)));
}

#[test]
fn simulator_transport_failures_save_reports_and_failed_execution() {
    setup_env();
    let original = claude_code::parse_file(&fx("claude-code.jsonl")).unwrap();
    let repo = tmp_repo();
    let output = tmp("sim-failure");
    let opts = RerunOpts { workspace: repo.display().to_string(), out_dir: Some(output.clone()), user_mode: "simulate".into(), sim_llm: fake_llm("sim-fail"), judge_llm: fake_llm("brief"), quiet: true, ..Default::default() };
    let result = rerun(&original, &opts, &mut no_log, &mut no_log).unwrap();
    let execution = result.session.unwrap().execution.unwrap();
    assert_eq!(execution.completed_turns, 1);
    assert_eq!(execution.failed_turns, 1);
    assert_eq!(execution.stop_reason.as_deref(), Some("simulator_error"));
    for name in ["session.json", "record.jsonl", "report.json", "diff.patch"] { assert!(output.join(name).exists()); }
}

#[test]
fn fork_uses_native_log_from_saved_run_and_rejects_nonexistent_turn() {
    setup_env();
    let repo = tmp_repo();
    for (fixture, harness) in [("claude-code.jsonl", Harness::ClaudeCode), ("codex.jsonl", Harness::Codex)] {
        let mut original = casimir::adapters::parse_file(harness, &fx(fixture)).unwrap();
        original.harness_log_path = original.path.take();
        original.path = Some(repo.join("normalized-session.json").display().to_string());
        let id = uuid::Uuid::new_v4().to_string();
        let fork = casimir::adapters::prepare_fork(harness, &original, 2, &id, &repo).unwrap();
        assert_eq!(user_turns(&casimir::adapters::parse_file(harness, &fork).unwrap()).len(), 1);
        assert!(casimir::adapters::prepare_fork(harness, &original, 3, &uuid::Uuid::new_v4().to_string(), &repo).is_err());
    }
}

#[test]
fn codex_live_usage_accumulates_across_resumed_turns() {
    setup_env();
    let original = claude_code::parse_file(&fx("claude-code.jsonl")).unwrap();
    let repo = tmp_repo();
    let opts = RerunOpts { harness: Some(Harness::Codex), workspace: repo.display().to_string(), out_dir: Some(tmp("codex-usage")), quiet: true, ..Default::default() };
    let result = rerun(&original, &opts, &mut no_log, &mut no_log).unwrap();
    let usage = result.session.unwrap().usage_total.unwrap();
    assert_eq!(usage.output, 60);
    assert_eq!(usage.input, 200);
}

#[test]
fn rerun_retains_commits_and_reports_failed_execution() {
    setup_env();
    let original = claude_code::parse_file(&fx("claude-code.jsonl")).unwrap();
    let repo = tmp_repo();
    let base = RerunOpts { workspace: repo.display().to_string(), quiet: true, turns: Some(1), ..Default::default() };
    let committed = rerun(&original, &RerunOpts { out_dir: Some(tmp("committed")), extra_args: vec!["--fake-commit".into()], ..base.clone() }, &mut no_log, &mut no_log).unwrap();
    assert!(committed.diff.unwrap().patch.contains("+hi"), "committed edits remain part of the outcome");
    assert!(Command::new("git").args(["diff", "HEAD", "--exit-code"]).current_dir(&repo).status().unwrap().success());
    for flag in ["--fake-error", "--fake-crash", "--fake-empty"] {
        let out_dir = tmp("failed-run");
        let options = RerunOpts { out_dir: Some(out_dir.clone()), replicates: 2, extra_args: vec![flag.into()], continue_on_error: true, ..base.clone() };
        let (_, matrix) = rerun_matrix(&original, &options, &mut no_log, &mut no_log).unwrap();
        let matrix = matrix.unwrap();
        assert!(matrix.entries.iter().all(|e| !e.pass && e.completed_turns == 0 && e.errors > 0), "{flag}: failed prompts must not count as completed turns");
        assert_eq!(matrix.groups[0].verdict, "inconclusive");
        let session = load_session_file(&out_dir.join("r1")).unwrap();
        assert_eq!(session.execution.unwrap().failed_turns, 1);
        assert!(out_dir.join("r1/record.jsonl").exists());
        if flag == "--fake-crash" {
            assert!(std::fs::read_to_string(out_dir.join("r1/raw.jsonl")).unwrap().contains("init"));
        }
    }
}

#[test]
fn matrix_freezes_rubric_and_isolates_workspaces() {
    setup_env();
    let original = claude_code::parse_file(&fx("claude-code.jsonl")).unwrap();
    let repo = tmp_repo();
    let out_dir = tmp("frozen");
    let options = RerunOpts { workspace: repo.display().to_string(), out_dir: Some(out_dir.clone()), quiet: true, replicates: 2, judge: true, judge_llm: fake_llm("judge"), ..Default::default() };
    let mut logs = Vec::new();
    let (_, matrix) = rerun_matrix(&original, &options, &mut |s| logs.push(s.to_string()), &mut no_log).unwrap();
    assert_eq!(logs.iter().filter(|s| s.contains("drafting a per-session brief")).count(), 1);
    assert!(!repo.join("out.txt").exists(), "replicates must not mutate the source repository");
    let first = load_session_file(&out_dir.join("r1")).unwrap();
    let second = load_session_file(&out_dir.join("r2")).unwrap();
    assert_ne!(first.cwd, second.cwd);
    assert_eq!(std::fs::read(out_dir.join("r1/brief.json")).unwrap(), std::fs::read(out_dir.join("r2/brief.json")).unwrap());
    assert!(matrix.unwrap().entries.iter().all(|e| e.pass));
}

#[test]
fn attribution_requires_outcomes_and_dry_runs_leave_no_artifacts() {
    setup_env();
    let original = claude_code::parse_file(&fx("claude-code.jsonl")).unwrap();
    let repo = tmp_repo();
    let output = tmp("plan-parent").join("not-created");
    let mut options = RerunOpts { workspace: repo.display().to_string(), out_dir: Some(output.clone()), quiet: true, replicates: 2, dry_run: true, judge_llm: fake_llm("judge-both-pass"), ..Default::default() };
    assert!(attribute(&original, &options, &[2], &mut no_log, &mut no_log).unwrap_err().to_string().contains("requires --judge"));
    options.judge = true;
    assert!(attribute(&original, &options, &[2], &mut no_log, &mut no_log).unwrap_err().to_string().contains("withheld"));
    assert!(!output.exists());
    assert!(attribute(&original, &options, &[2, 2], &mut no_log, &mut no_log).is_err());
    assert!(attribute(&original, &options, &[1], &mut no_log, &mut no_log).is_err());
    options.dry_run = false;
    options.replicates = 1;
    assert!(attribute(&original, &options, &[2], &mut no_log, &mut no_log).is_err(), "missing original evidence cannot establish a rescued failure");
    assert_eq!(wilson(0, 7).0, 0.0, "floating point residue must not look like a positive effect");
}

#[test]
fn judge_schema_and_invalidity_are_enforced() {
    setup_env();
    let original = claude_code::parse_file(&fx("claude-code.jsonl")).unwrap();
    assert!(judge_sessions(&original, &original, None, None, &fake_llm("judge-malformed")).is_err());
    let repo = tmp_repo();
    let options = RerunOpts { workspace: repo.display().to_string(), out_dir: Some(tmp("invalid-patch")), quiet: true, replicates: 2, judge: true, judge_llm: fake_llm("judge-invalid-patch"), ..Default::default() };
    let (_, matrix) = rerun_matrix(&original, &options, &mut no_log, &mut no_log).unwrap();
    assert!(matrix.unwrap().entries.iter().all(|e| !e.pass), "a high score cannot override an explicit invalidity finding");
}

#[test]
fn record_envelopes_are_ordered_unique_and_preserve_unanswered_calls() {
    let events = vec![
        call(1, "a", "Bash", json!({"command": "one"})),
        call(1, "b", "Bash", json!({"command": "two"})),
        Event::text(1, "2026-01-01T00:00:00Z", EventKind::Assistant, "reply"),
        Event::tool_result(1, "2026-01-01T00:00:01Z", "b", Some("Bash".into()), "second finishes first", false),
        Event::tool_result(1, "2026-01-01T00:00:02Z", "orphan", Some("Bash".into()), "missing call", false),
        Event::tool_result(1, "2026-01-01T00:00:03Z", "other-orphan", Some("Bash".into()), "also missing", false),
    ];
    let session = Session { events, ..Default::default() };
    let path = tmp("records").join("record.jsonl");
    casimir::rerun::write_record(&path, &session, "test", "test").unwrap();
    let bytes = std::fs::read_to_string(&path).unwrap();
    casimir::rerun::write_record(&path, &session, "test", "test").unwrap();
    assert_eq!(bytes, std::fs::read_to_string(path).unwrap());
    let rows: Vec<serde_json::Value> = bytes.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(rows[0]["unanswered"], true);
    assert_eq!(rows[1]["output"], "second finishes first");
    assert_eq!(rows[2]["inputAvailable"], false);
    let addresses: std::collections::BTreeSet<_> = rows.iter().map(|r| r["address"].as_str().unwrap()).collect();
    assert_eq!(addresses.len(), rows.len());
    assert!(rows.windows(2).all(|r| r[0]["eventIndex"].as_u64() < r[1]["eventIndex"].as_u64()));
}

#[test]
fn cli_accepts_standalone_patches_and_returns_failure_for_harness_errors() {
    setup_env();
    let repo = tmp_repo();
    let output = tmp("cli-failure");
    let patch = output.join("reference.patch");
    std::fs::write(&patch, "diff --git a/out.txt b/out.txt\n--- /dev/null\n+++ b/out.txt\n@@ -0,0 +1 @@\n+hi\n").unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_casimir"))
        .args(["rerun", fx("claude-code.jsonl").to_str().unwrap(), "--workspace", repo.to_str().unwrap(), "--turns", "1", "--quiet", "--original-diff", patch.to_str().unwrap(), "-o", output.join("run").to_str().unwrap(), "--", "--fake-crash"])
        .output().unwrap();
    assert_eq!(result.status.code(), Some(1));
    assert_eq!(std::fs::read(output.join("run/original.patch")).unwrap(), std::fs::read(patch).unwrap());
    assert!(output.join("run/report.json").exists(), "a failed CLI run still saves its report");
}

#[test]
fn cli_rerun_uses_saved_reference_patch_after_workspace_changes() {
    setup_env();
    let original = claude_code::parse_file(&fx("claude-code.jsonl")).unwrap();
    let repo = tmp_repo();
    let saved = tmp("saved-reference");
    let opts = RerunOpts { workspace: repo.display().to_string(), out_dir: Some(saved.clone()), quiet: true, ..Default::default() };
    rerun(&original, &opts, &mut no_log, &mut no_log).unwrap();
    let expected = std::fs::read(saved.join("diff.patch")).unwrap();
    std::fs::write(repo.join("out.txt"), "later unrelated edit\n").unwrap();
    let output = tmp("frozen-reference");
    let result = Command::new(env!("CARGO_BIN_EXE_casimir"))
        .args(["rerun", saved.to_str().unwrap(), "--workspace", repo.to_str().unwrap(), "--quiet", "-o", output.to_str().unwrap()])
        .output().unwrap();
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    assert_eq!(std::fs::read(output.join("original.patch")).unwrap(), expected);
}

#[test]
fn judge_receives_tool_inputs_and_results_in_both_orders() {
    setup_env();
    let mut a = claude_code::parse_file(&fx("claude-code.jsonl")).unwrap();
    a.events.push(Event::tool_call(2, "", "verify", "Bash", json!({"command": "verification_evidence_marker"})));
    a.events.push(Event::tool_result(2, "", "verify", Some("Bash".into()), "verified content from actual tool result", false));
    let judgement = judge_sessions(&a, &a, None, None, &fake_llm("judge-evidence")).unwrap();
    assert_eq!(judgement.passes.len(), 2);
    assert_eq!(judgement.score_b, 9.0);
}

#[test]
fn attribution_returns_nonzero_and_keeps_reports_for_harness_failures() {
    setup_env();
    let repo = tmp_repo();
    let output = tmp("attribute-failure");
    let result = Command::new(env!("CARGO_BIN_EXE_casimir"))
        .args(["attribute", fx("claude-code.jsonl").to_str().unwrap(), "--workspace", repo.to_str().unwrap(),
            "--turns-at", "2", "--replicates", "1", "--judge", "--judge-llm", "cmd", "--judge-model", "fake:judge",
            "--quiet", "-o", output.to_str().unwrap(), "--", "--fake-error"])
        .output().unwrap();
    assert_eq!(result.status.code(), Some(1), "{}", String::from_utf8_lossy(&result.stderr));
    assert!(String::from_utf8_lossy(&result.stderr).contains("attribution withheld"));
    assert!(!output.join("attribution.json").exists(), "missing original evidence fails before model execution");
}

#[test]
fn simulator_noops_and_goals_met_have_explicit_completion_semantics() {
    setup_env();
    let original = claude_code::parse_file(&fx("claude-code.jsonl")).unwrap();
    let repo = tmp_repo();
    for (mode, judged, expected_pass) in [("sim-noop", true, true), ("sim-goals-met", true, false), ("sim-goals-met", false, false), ("sim-stop", true, false)] {
        let opts = RerunOpts { workspace: repo.display().to_string(), out_dir: Some(tmp("sim-completion")), quiet: true, replicates: 2, user_mode: "simulate".into(), sim_llm: fake_llm(mode), judge: judged, judge_llm: fake_llm("judge"), ..Default::default() };
        let (_, matrix) = rerun_matrix(&original, &opts, &mut no_log, &mut no_log).unwrap();
        assert!(matrix.unwrap().entries.iter().all(|e| e.pass == expected_pass), "{mode}, judge={judged}");
    }
}

#[test]
fn invalid_plans_fail_before_creating_artifacts() {
    setup_env();
    let original = claude_code::parse_file(&fx("claude-code.jsonl")).unwrap();
    let repo = tmp_repo();
    let output = tmp("invalid-plan").join("not-created");
    let base = RerunOpts { workspace: repo.display().to_string(), out_dir: Some(output.clone()), quiet: true, ..Default::default() };
    for opts in [RerunOpts { replicates: 0, ..base.clone() }, RerunOpts { turns: Some(0), ..base.clone() }, RerunOpts { pass_threshold: f64::NAN, ..base.clone() }, RerunOpts { from_turn: Some(2), turns: Some(1), ..base.clone() }] {
        assert!(rerun_matrix(&original, &opts, &mut no_log, &mut no_log).is_err());
        assert!(!output.exists());
    }
    let mut unknown = original.clone();
    unknown.model = None;
    assert!(rerun_matrix(&unknown, &RerunOpts { control: true, ..base }, &mut no_log, &mut no_log).is_err());
    assert!(!output.exists());
}

#[test]
fn diff_capture_from_subdirectory_keeps_staged_and_untracked_paths() {
    let repo = tmp_repo();
    let sub = repo.join("src");
    std::fs::create_dir(&sub).unwrap();
    std::fs::write(sub.join("with space.txt"), "untracked\n").unwrap();
    std::fs::write(repo.join("README.md"), "staged\n").unwrap();
    assert!(Command::new("git").args(["add", "README.md"]).current_dir(&repo).status().unwrap().success());
    let diff = casimir::workspace::capture_diff_against(&sub, "HEAD");
    assert!(diff.patch.contains("+staged"));
    assert!(diff.patch.contains("+untracked"));
    assert!(diff.files.iter().any(|f| f.path == "src/with space.txt"));
    let empty = end_state_similarity(&Diff::default(), &Diff::default());
    assert!(!empty.informative, "empty reference patches cannot support an agreement score");
}

#[test]
fn mismatched_or_failed_controls_cannot_establish_a_noise_floor() {
    setup_env();
    let mut original = claude_code::parse_file(&fx("claude-code.jsonl")).unwrap();
    let repo = tmp_repo();
    let options = RerunOpts { workspace: repo.display().to_string(), out_dir: Some(tmp("model-mismatch")), quiet: true, control: true, ..Default::default() };
    let (_, matrix) = rerun_matrix(&original, &options, &mut no_log, &mut no_log).unwrap();
    let matrix = matrix.unwrap();
    assert!(matrix.entries.iter().filter(|e| e.control).all(|e| !e.control_model_verified));
    assert!(matrix.groups.iter().all(|g| g.exceeds_control.is_none()));
    original.model = Some("fake-model".into());
    let options = RerunOpts { out_dir: Some(tmp("failed-control")), extra_args: vec!["--fake-error".into()], ..options };
    let (_, matrix) = rerun_matrix(&original, &options, &mut no_log, &mut no_log).unwrap();
    assert!(matrix.unwrap().groups.iter().all(|g| g.exceeds_control.is_none()));
}

#[test]
fn claude_code_parses_log_into_normalized_session() {
    let s = claude_code::parse_file(&fx("claude-code.jsonl")).unwrap();
    assert_eq!(s.harness, Some(Harness::ClaudeCode));
    assert_eq!(s.id, "11111111-2222-4333-8444-555555555555");
    assert_eq!(s.title.as_deref(), Some("Add greet function"));
    assert_eq!(s.model.as_deref(), Some("claude-opus-5"));
    assert_eq!(s.cwd.as_deref(), Some("/work/demo"));
    assert_eq!(s.git_branch.as_deref(), Some("main"));
    assert_eq!(s.permission_mode.as_deref(), Some("bypassPermissions"));
    let turns = user_turns(&s);
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[0].text, "Add a greet(name) function to lib.py and a test for it", "system-reminder stripped");
    assert_eq!(turns[1].turn, 2);
    let kinds: Vec<EventKind> = s.events.iter().filter(|e| !e.sidechain).map(|e| e.kind).take(5).collect();
    assert_eq!(kinds, [EventKind::User, EventKind::Thinking, EventKind::Assistant, EventKind::ToolCall, EventKind::ToolResult]);
    let sys: Vec<&str> = s.events.iter().filter(|e| e.kind == EventKind::System).map(|e| e.subtype.as_deref().unwrap()).collect();
    assert_eq!(sys, ["command", "command-output"]);
    assert_eq!(s.events.iter().filter(|e| e.sidechain).count(), 2);
    let err = s.events.iter().find(|e| e.kind == EventKind::ToolResult && e.result.as_ref().unwrap().is_error).unwrap();
    assert_eq!(err.result.as_ref().unwrap().name.as_deref(), Some("Bash"));
}

#[test]
fn claude_code_stats_dedupe_usage_and_exclude_subagents() {
    let st = stats(&claude_code::parse_file(&fx("claude-code.jsonl")).unwrap());
    assert_eq!(st.turns, 2);
    assert_eq!(st.assistant_messages, 3);
    assert_eq!(st.thinking_blocks, 1);
    assert_eq!(st.tool_calls, 5);
    assert_eq!(st.tool_errors, 1);
    assert_eq!(st.files_touched, 2);
    assert_eq!(st.sidechain_events, 2);
    assert_eq!(st.tools_by_name.get("Bash"), Some(&2));
    assert_eq!(st.tools_by_name.get("Edit"), Some(&2));
    assert_eq!(st.tools_by_name.get("Write"), Some(&1));
    assert_eq!(st.tools_by_name.get("Glob"), None);
    assert_eq!(st.usage.output, 120 + 6 * 300);
    assert_eq!(st.usage.input, 5 + 6 * 3);
    assert_eq!(st.usage.cache_read, 2000 + 6 * 3000);
    assert_eq!(st.usage.cache_write, 1000 + 6 * 200);
    assert_eq!(st.usage.reasoning, 40);
    assert_eq!(st.duration_ms, 2 * 60_000 + 7_000);
}

#[test]
fn codex_parses_rollout_skips_injected_context_maps_tools_and_usage() {
    let s = codex::parse_file(&fx("codex.jsonl")).unwrap();
    assert_eq!(s.harness, Some(Harness::Codex));
    assert_eq!(s.id, "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee");
    assert_eq!(s.model.as_deref(), Some("gpt-5-codex"));
    assert_eq!(s.git_commit.as_deref(), Some("0123456789abcdef0123456789abcdef01234567"));
    assert_eq!(s.git_branch.as_deref(), Some("main"));
    assert_eq!(s.sandbox.as_deref(), Some("workspace-write"));
    assert_eq!(s.effort.as_deref(), Some("medium"));
    let turns = user_turns(&s);
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[0].text, "Add a greet(name) function to lib.py and a test for it");
    let calls: Vec<&Event> = s.events.iter().filter(|e| e.kind == EventKind::ToolCall).collect();
    let names: Vec<&str> = calls.iter().map(|e| e.tool.as_ref().unwrap().name.as_str()).collect();
    assert_eq!(names, ["shell", "apply_patch", "shell", "apply_patch"]);
    assert_eq!(calls[0].tool.as_ref().unwrap().input["command"], json!(["bash", "-lc", "cat lib.py"]));
    let results: Vec<&Event> = s.events.iter().filter(|e| e.kind == EventKind::ToolResult).collect();
    assert_eq!(results[0].result.as_ref().unwrap().output, "def add(a, b):\n    return a + b\n", "JSON-wrapped output unwrapped");
    assert!(!results[0].result.as_ref().unwrap().is_error);
    assert!(results[2].result.as_ref().unwrap().is_error, "Exit code: 127 detected");
    assert_eq!(results[2].result.as_ref().unwrap().name.as_deref(), Some("shell"));
    let st = stats(&s);
    assert_eq!(st.assistant_messages, 2, "event_msg duplicates ignored");
    assert_eq!(st.thinking_blocks, 1);
    assert_eq!(st.usage, Usage { input: 15000, output: 800, cache_read: 9000, cache_write: 0, reasoning: 250 });
    let files: Vec<(String, Vec<String>)> = files_touched(&s).into_iter().map(|f| (f.path, f.ops)).collect();
    assert_eq!(files, [("lib.py".to_string(), vec!["update".to_string()]), ("test_lib.py".to_string(), vec!["add".to_string()])]);
    assert_eq!(final_assistant_text(&s, None), "Done: greet now defaults to \"World\".");
    assert_eq!(final_assistant_text(&s, Some(1)), "Added `greet` to lib.py and a test in test_lib.py. pytest isn't installed so the test was not run.");
}

#[test]
fn format_detection_and_session_resolution() {
    setup_env();
    assert_eq!(load_session_file(&fx("claude-code.jsonl")).unwrap().harness, Some(Harness::ClaudeCode));
    assert_eq!(load_session_file(&fx("codex.jsonl")).unwrap().harness, Some(Harness::Codex));
    assert_eq!(resolve_session(fx("codex.jsonl").to_str().unwrap()).unwrap().id, "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee");
    let dir = tmp("export");
    let exported = dir.join("s.json");
    let orig = codex::parse_file(&fx("codex.jsonl")).unwrap();
    std::fs::write(&exported, serde_json::to_string(&orig).unwrap()).unwrap();
    assert_eq!(load_session_file(&exported).unwrap().events.len(), orig.events.len());
    let err = resolve_session("definitely-not-a-session-xyz").unwrap_err().to_string();
    assert!(err.contains("not found"), "{err}");
}

#[test]
fn tool_one_liners_and_file_inference_from_shell() {
    let ev = Event::tool_call(1, "2026-01-01T00:00:00Z", "x", "Bash", json!({ "command": "cat > a/b.txt <<'EOF'\nhi\nEOF\n; tee out.log" }));
    assert_eq!(tool_one_liner(&ev, 40), "Bash: cat > a/b.txt <<'EOF' hi EOF ; te…");
    let s = Session { events: vec![ev], ..Default::default() };
    let files: Vec<String> = files_touched(&s).into_iter().map(|f| f.path).collect();
    assert_eq!(files, ["a/b.txt", "out.log"]);
    let patch = Event::tool_call(1, "2026-01-01T00:00:00Z", "y", "apply_patch", json!({ "patch": "*** Begin Patch\n*** Delete File: gone.py\n*** End Patch" }));
    assert_eq!(tool_one_liner(&patch, 200), "apply_patch: gone.py");
}

#[test]
fn renderers_produce_transcript_and_markdown() {
    let s = claude_code::parse_file(&fx("claude-code.jsonl")).unwrap();
    let text = render_transcript(&s, &RenderOpts { thinking: true, ..Default::default() });
    assert!(text.contains("user (turn 2)"));
    assert!(text.contains("I should look at lib.py first"));
    assert!(!text.contains("Explore the repo"), "sidechains hidden by default");
    assert!(render_transcript(&s, &RenderOpts { sidechains: true, ..Default::default() }).contains("[subagent]"));
    let md = render_markdown(&codex::parse_file(&fx("codex.jsonl")).unwrap(), &RenderOpts::default());
    assert!(md.contains("## Turn 2 — user"));
    assert!(md.contains("apply_patch: lib.py, test_lib.py"));
}

#[test]
fn compare_relativizes_paths_across_harnesses() {
    let a = claude_code::parse_file(&fx("claude-code.jsonl")).unwrap();
    let b = codex::parse_file(&fx("codex.jsonl")).unwrap();
    let r = compare_sessions(&a, &b, None, None, None);
    assert_eq!(r.files.both, ["lib.py", "test_lib.py"]);
    assert!(r.files.only_a.is_empty());
    assert_eq!(relativize("/work/demo/x/y.py", Some("/work/demo")), "x/y.py");
    assert_eq!(relativize("/elsewhere/y.py", Some("/work/demo")), "/elsewhere/y.py");
    assert!(render_compare_markdown(&r, "original", "rerun").contains("| tool calls | 5 | 4 |"));
}

#[test]
fn extract_json_tolerates_prose_and_fences() {
    assert_eq!(extract_json("Sure:\n```json\n{\"a\": 1, \"b\": \"x}y\"}\n```\nthanks"), Some(json!({ "a": 1, "b": "x}y" })));
    assert_eq!(extract_json("prefix {\"action\":\"stop\",\"message\":\"\"} suffix"), Some(json!({ "action": "stop", "message": "" })));
    assert_eq!(extract_json("no json here"), None);
}

#[test]
fn claude_code_stream_json_mapping() {
    let rec = json!({ "type": "assistant", "timestamp": "2026-01-01T00:00:00Z", "message": { "id": "m", "model": "claude-sonnet-5",
        "content": [{ "type": "text", "text": "hi" }, { "type": "tool_use", "id": "t", "name": "Bash", "input": { "command": "ls" } }],
        "usage": { "input_tokens": 1, "output_tokens": 2 } } });
    let evs = claude_code::record_to_events(&rec, 3);
    assert_eq!(evs.len(), 2);
    assert_eq!(evs[0].turn, 3);
    assert_eq!(evs[0].usage.as_ref().unwrap().output, 2);
    assert!(evs[1].usage.is_none(), "usage attached once per message");
    assert_eq!(evs[1].tool.as_ref().unwrap().name, "Bash");
    let synthetic = json!({ "type": "assistant", "message": { "model": "<synthetic>", "content": [{ "type": "text", "text": "Not logged in" }] } });
    assert_eq!(claude_code::record_to_events(&synthetic, 1)[0].kind, EventKind::Error);
}

#[test]
fn codex_exec_json_event_mapping() {
    let mut state = codex::ExecState::default();
    assert!(codex::exec_event_to_events(&json!({ "type": "thread.started", "thread_id": "T" }), 1, &mut state).is_empty());
    assert_eq!(state.thread_id.as_deref(), Some("T"));
    let evs = codex::exec_event_to_events(&json!({ "type": "item.completed", "item": { "id": "i", "type": "command_execution", "command": "ls", "aggregated_output": "a\n", "exit_code": 2 } }), 1, &mut state);
    assert_eq!(evs[0].kind, EventKind::ToolCall);
    assert!(evs[1].result.as_ref().unwrap().is_error);
    codex::exec_event_to_events(&json!({ "type": "turn.completed", "usage": { "input_tokens": 3, "cached_input_tokens": 1, "output_tokens": 2 } }), 1, &mut state);
    assert_eq!(state.usage, Some(Usage { input: 3, output: 2, cache_read: 1, cache_write: 0, reasoning: 0 }));
}

#[test]
fn rerun_drives_fake_claude_harness_end_to_end() {
    setup_env();
    let original = claude_code::parse_file(&fx("claude-code.jsonl")).unwrap();
    let repo = tmp_repo();
    let out_dir = tmp("run");
    let opts = RerunOpts { workspace: repo.display().to_string(), out_dir: Some(out_dir.clone()), quiet: true, user_mode: "verbatim".into(), run_id: Some("t-claude".into()), ..Default::default() };
    let res = rerun(&original, &opts, &mut no_log, &mut no_log).unwrap();
    let session = res.session.unwrap();
    assert_eq!(session.harness, Some(Harness::ClaudeCode));
    assert_eq!(session.model.as_deref(), Some("fake-model"));
    assert_eq!(user_turns(&session).len(), 2);
    assert_eq!(session.events.iter().filter(|e| e.kind == EventKind::ToolCall).count(), 2);
    assert_eq!(session.cost_usd, Some(0.02));
    let diff = res.diff.unwrap();
    assert!(diff.files.iter().any(|f| f.path == "out.txt"), "workspace diff captured");
    assert!(diff.patch.contains("+hi"));
    for f in ["session.json", "original.json", "meta.json", "raw.jsonl", "diff.patch", "report.md", "report.json"] {
        assert!(out_dir.join(f).exists(), "{f} written");
    }
    let reloaded = load_session_file(&out_dir).unwrap();
    assert_eq!(stats(&reloaded).tool_calls, 2);
    assert_eq!(res.report.unwrap().b.turns, 2);
    let first = session.events.iter().find(|e| e.kind == EventKind::Assistant).unwrap();
    assert!(first.text_str().contains("Working on: Add a greet"));
}

#[test]
fn rerun_drives_fake_codex_harness_cross_harness() {
    setup_env();
    let original = claude_code::parse_file(&fx("claude-code.jsonl")).unwrap();
    let repo = tmp_repo();
    let out_dir = tmp("run");
    let opts = RerunOpts { harness: Some(Harness::Codex), model: Some("gpt-5-codex".into()), workspace: repo.display().to_string(), out_dir: Some(out_dir), turns: Some(1), quiet: true, user_mode: "verbatim".into(), run_id: Some("t-codex".into()), ..Default::default() };
    let res = rerun(&original, &opts, &mut no_log, &mut no_log).unwrap();
    let session = res.session.unwrap();
    assert_eq!(session.harness, Some(Harness::Codex));
    assert_eq!(user_turns(&session).len(), 1);
    let names: Vec<&str> = session.events.iter().filter(|e| e.kind == EventKind::ToolCall).map(|e| e.tool.as_ref().unwrap().name.as_str()).collect();
    assert_eq!(names, ["shell", "apply_patch"]);
    assert_eq!(session.usage_total, Some(Usage { input: 100, output: 30, cache_read: 40, cache_write: 0, reasoning: 0 }));
    let report = res.report.unwrap();
    assert!(report.files.both.is_empty());
    assert!(report.files.only_b.iter().any(|f| f == "out.txt"));
}

#[test]
fn rerun_creates_worktree_at_base_commit() {
    setup_env();
    let repo = tmp_repo();
    let mut original = claude_code::parse_file(&fx("claude-code.jsonl")).unwrap();
    original.cwd = Some(repo.display().to_string());
    original.git_branch = None;
    original.started_at = Some((chrono::Utc::now() + chrono::Duration::seconds(60)).to_rfc3339());
    let out_dir = tmp("run");
    let opts = RerunOpts { workspace: "auto".into(), out_dir: Some(out_dir), turns: Some(1), quiet: true, user_mode: "verbatim".into(), run_id: Some("t-worktree".into()), ..Default::default() };
    let res = rerun(&original, &opts, &mut no_log, &mut no_log).unwrap();
    assert_eq!(res.workspace.mode, "worktree");
    let root = res.workspace.root.clone().unwrap();
    assert!(root.starts_with(std::env::var("CASIMIR_HOME").unwrap()));
    assert!(res.workspace.dir.join("out.txt").exists(), "fake harness wrote into the worktree");
    assert!(!repo.join("out.txt").exists(), "original checkout untouched");
}

fn fake_llm(mode: &str) -> LlmOpts {
    LlmOpts { model: Some(format!("fake:{mode}")), backend: "cmd".into(), ..Default::default() }
}

#[test]
fn copilot_parses_session_dir_and_lists() {
    setup_env();
    let dir = fx("copilot/session-state/cccccccc-1111-4222-8333-444444444444");
    let s = copilot::parse_dir(&dir).unwrap();
    assert_eq!(s.harness, Some(Harness::Copilot));
    assert_eq!(s.id, "cccccccc-1111-4222-8333-444444444444");
    assert_eq!(s.cwd.as_deref(), Some("/work/demo"));
    assert_eq!(s.git_branch.as_deref(), Some("main"));
    assert_eq!(s.title.as_deref(), Some("Add greet function to lib.py"), "block-scalar summary parsed");
    assert_eq!(s.model.as_deref(), Some("gpt-5"));
    assert_eq!(user_turns(&s).len(), 1);
    let names: Vec<&str> = s.events.iter().filter(|e| e.kind == EventKind::ToolCall).map(|e| e.tool.as_ref().unwrap().name.as_str()).collect();
    assert_eq!(names, ["bash", "edit", "create"], "toolRequest and execution_start for the same call are not duplicated");
    let st = stats(&s);
    assert_eq!(st.assistant_messages, 2);
    assert_eq!(st.tool_errors, 1);
    assert_eq!(st.usage, Usage { input: 600, output: 200, cache_read: 300, cache_write: 100, reasoning: 50 }, "shutdown modelMetrics, uncached input derived");
    let files: Vec<String> = files_touched(&s).into_iter().map(|f| f.path).collect();
    assert_eq!(files, ["/work/demo/lib.py", "/work/demo/test_lib.py"]);
    assert_eq!(load_session_file(&dir).unwrap().harness, Some(Harness::Copilot));
    assert_eq!(load_session_file(&dir.join("events.jsonl")).unwrap().events.len(), s.events.len());
    let listed = copilot::list_sessions();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].cwd.as_deref(), Some("/work/demo"));
    assert_eq!(resolve_session("copilot:last").unwrap().id, s.id);
}

#[test]
fn gemini_parses_jsonl_with_upserts_injected_context_and_project_root() {
    setup_env();
    let file = fx("gemini/tmp/demo/chats/session-2026-09-21T10-00-dddddddd.jsonl");
    let s = gemini::parse_file(&file).unwrap();
    assert_eq!(s.harness, Some(Harness::Gemini));
    assert_eq!(s.id, "dddddddd-1111-4222-8333-555555555555");
    assert_eq!(s.cwd.as_deref(), Some("/work/demo"), "cwd from .project_root");
    assert_eq!(s.model.as_deref(), Some("gemini-2.5-pro"));
    let turns = user_turns(&s);
    assert_eq!(turns.len(), 1, "<session_context> message filtered");
    assert_eq!(turns[0].text, "Add a greet(name) function to lib.py and a test for it");
    let st = stats(&s);
    assert_eq!(st.thinking_blocks, 1);
    assert_eq!(st.assistant_messages, 2);
    assert_eq!(st.tool_calls, 2, "$set re-sending g2 does not duplicate it");
    assert_eq!(st.tool_errors, 0);
    assert_eq!(st.usage, Usage { input: 1800, output: 170, cache_read: 600, cache_write: 0, reasoning: 20 });
    let results: Vec<&Event> = s.events.iter().filter(|e| e.kind == EventKind::ToolResult).collect();
    assert_eq!(results[0].result.as_ref().unwrap().output, "def add(a, b):\n    return a + b\n");
    assert!(results[1].result.as_ref().unwrap().output.contains("Successfully wrote"), "upserted record wins");
    assert_eq!(files_touched(&s).into_iter().map(|f| f.path).collect::<Vec<_>>(), ["/work/demo/test_lib.py"]);
    assert_eq!(s.events.iter().find(|e| e.kind == EventKind::Thinking).unwrap().text_str(), "**Inspecting lib.py** I should read the file first.");
    assert_eq!(load_session_file(&file).unwrap().harness, Some(Harness::Gemini));
    assert_eq!(gemini::list_sessions().len(), 1);
    assert_eq!(resolve_session("gemini:last").unwrap().id, s.id);
    // $rewindTo drops later messages
    let recs = vec![
        json!({"sessionId": "x", "projectHash": "h", "startTime": "2026-01-01T00:00:00Z"}),
        json!({"id": "u1", "timestamp": "2026-01-01T00:00:01Z", "type": "user", "content": "first"}),
        json!({"id": "g1", "timestamp": "2026-01-01T00:00:02Z", "type": "gemini", "content": "reply one"}),
        json!({"id": "u2", "timestamp": "2026-01-01T00:00:03Z", "type": "user", "content": "second"}),
        json!({"$rewindTo": "g1"}),
        json!({"id": "u3", "timestamp": "2026-01-01T00:00:04Z", "type": "user", "content": "second, again"}),
    ];
    let r = gemini::parse_records(&recs, None);
    assert_eq!(user_turns(&r).iter().map(|t| t.text.clone()).collect::<Vec<_>>(), ["first", "second, again"]);
}

#[test]
fn judge_runs_both_orders_and_flags_order_sensitivity() {
    setup_env();
    let a = claude_code::parse_file(&fx("claude-code.jsonl")).unwrap();
    let b = codex::parse_file(&fx("codex.jsonl")).unwrap();
    let j = judge_sessions(&a, &b, None, None, &fake_llm("judge-flip")).unwrap();
    assert!(j.order_sensitive, "a judge that always prefers the first candidate must be caught");
    assert_eq!(j.winner, "tie");
    assert_eq!(j.passes.len(), 2);
    assert_eq!(j.passes[0].order, "AB");
    assert_eq!(j.passes[1].order, "BA");
    assert_eq!(j.passes[1].winner, "B", "second pass winner mapped back to real labels");
    assert!((j.score_a - 7.0).abs() < 1e-9 && (j.score_b - 7.0).abs() < 1e-9, "scores averaged across orders");
    assert!(j.close);
    assert_eq!(j.model, "fake:judge-flip");
    // a consistent judge keeps its verdict
    let mut b2 = b.clone();
    b2.events.push(Event::text(2, "2026-09-20T12:02:00.000Z", EventKind::Assistant, "Working on it: done"));
    let j2 = judge_sessions(&a, &b2, None, None, &fake_llm("judge-consistent")).unwrap();
    assert!(!j2.order_sensitive);
    assert_eq!(j2.winner, "B");
    assert!(!j2.close);
}

#[test]
fn simulator_retries_ungrounded_replies_and_reports_stop_reasons() {
    setup_env();
    let original = claude_code::parse_file(&fx("claude-code.jsonl")).unwrap();
    let mut rerun_s = Session::new(Harness::ClaudeCode);
    rerun_s.events.push(Event::text(1, "2026-09-22T00:00:00Z", EventKind::User, "Add a greet(name) function to lib.py and a test for it"));
    rerun_s.events.push(Event::text(1, "2026-09-22T00:00:05Z", EventKind::Assistant, "Working on it"));
    let mut state = SimState::default();
    let r = simulate_user_turn(&original, &rerun_s, 2, &fake_llm("sim-retry"), &mut state).unwrap();
    assert_eq!(r.retries, 2, "two ungrounded replies discarded");
    assert!(!r.verbatim);
    assert_eq!(r.grounded_in, [2]);
    assert!(r.message.unwrap().starts_with("adapted: Also make greet"));
    assert_eq!(state.memory.len(), 1, "only the accepted reply's note is kept");
    let stop = simulate_user_turn(&original, &rerun_s, 2, &fake_llm("sim-stop"), &mut state).unwrap();
    assert!(stop.message.is_none());
    assert_eq!(stop.stop_reason.as_deref(), Some("out_of_scope"));
    let vb = simulate_user_turn(&original, &rerun_s, 2, &fake_llm("sim-verbatim"), &mut state).unwrap();
    assert!(vb.verbatim);
    assert_eq!(vb.message.as_deref(), Some("Also make greet default to 'World' when no name is given"));
}

#[test]
fn end_state_and_tool_sequence_similarity() {
    let pa = "diff --git a/lib.py b/lib.py\n--- a/lib.py\n+++ b/lib.py\n@@ -1,2 +1,4 @@\n def add(a, b):\n     return a + b\n+def greet(name):\n+    return f\"Hello, {name}!\"\ndiff --git a/test_lib.py b/test_lib.py\nnew file mode 100644\n--- /dev/null\n+++ b/test_lib.py\n@@ -0,0 +1 @@\n+from lib import greet\n";
    let pb = "diff --git a/lib.py b/lib.py\n--- a/lib.py\n+++ b/lib.py\n@@ -1,2 +1,4 @@\n def add(a, b):\n     return a + b\n+def greet(name):\n+    return f\"Hello, {name}!\"\n";
    let parsed = parse_patch(pa);
    assert_eq!(parsed["lib.py"].added.len(), 2);
    assert_eq!(parsed["test_lib.py"].added, ["from lib import greet"]);
    let da = Diff { patch: pa.into(), source: Some("A".into()), ..Default::default() };
    let db = Diff { patch: pb.into(), ..Default::default() };
    let es = end_state_similarity(&da, &db);
    assert!((es.files_jaccard - 0.5).abs() < 1e-9);
    assert!((es.content_similarity - 0.5).abs() < 1e-9, "lib.py identical (1.0), test_lib.py missing (0.0)");
    assert!((es.score - 0.25).abs() < 1e-9);
    assert_eq!(es.source_a.as_deref(), Some("A"));
    let same = end_state_similarity(&da, &da);
    assert!((same.score - 1.0).abs() < 1e-9);
    let v = |xs: &[&str]| xs.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    assert!((sequence_similarity(&v(&["Bash", "Edit", "Write"]), &v(&["Bash", "Write"])) - 0.8).abs() < 1e-9);
    assert_eq!(sequence_similarity(&[], &[]), 1.0);
    let a = claude_code::parse_file(&fx("claude-code.jsonl")).unwrap();
    let r = compare_sessions(&a, &a, Some(da.clone()), Some(da), None);
    assert!((r.tool_sequence_similarity - 1.0).abs() < 1e-9);
    assert!((r.end_state.unwrap().score - 1.0).abs() < 1e-9);
}

#[test]
fn simulated_rerun_records_simulator_and_marks_turns() {
    setup_env();
    let original = claude_code::parse_file(&fx("claude-code.jsonl")).unwrap();
    let repo = tmp_repo();
    let out_dir = tmp("run");
    let original_diff = Diff { patch: "diff --git a/out.txt b/out.txt\nnew file mode 100644\n--- /dev/null\n+++ b/out.txt\n@@ -0,0 +1 @@\n+hi\n".into(), source: Some("test".into()), ..Default::default() };
    let opts = RerunOpts { workspace: repo.display().to_string(), out_dir: Some(out_dir.clone()), quiet: true, user_mode: "simulate".into(), sim_llm: fake_llm("sim-verbatim"), judge: true, judge_llm: fake_llm("judge-consistent"), run_id: Some("t-sim".into()), original_diff: Some(original_diff), ..Default::default() };
    let res = rerun(&original, &opts, &mut no_log, &mut no_log).unwrap();
    let session = res.session.unwrap();
    assert_eq!(session.simulator.as_ref().map(|s| s.model.as_str()), Some("fake:sim-verbatim"));
    assert_eq!(session.simulator.as_ref().map(|s| s.backend.as_str()), Some("cmd"));
    let second = session.events.iter().find(|e| e.kind == EventKind::User && e.turn == 2).unwrap();
    let sim = second.simulated.as_ref().expect("second turn marked as simulated");
    assert!(sim.verbatim);
    assert_eq!(sim.grounded_in, [2]);
    let text = render_transcript(&session, &RenderOpts::default());
    assert!(text.contains("[simulated user: verbatim]"));
    let report = res.report.unwrap();
    assert_eq!(report.b.simulator_model.as_deref(), Some("fake:sim-verbatim"));
    let j = report.judge.unwrap();
    assert_eq!(j.passes.len(), 2);
    assert_eq!(j.winner, "B", "fake harness output mentions 'Working on', which the consistent fake judge prefers");
    let es = report.end_state.expect("end state computed from the supplied original diff and the captured rerun diff");
    assert!((es.score - 1.0).abs() < 1e-9, "fake harness wrote the same out.txt as the supplied original diff");
    assert_eq!(es.source_a.as_deref(), Some("test"));
    assert!(out_dir.join("original.patch").exists());
}

#[test]
fn replicated_rerun_aggregates_pass_rates() {
    setup_env();
    let original = claude_code::parse_file(&fx("claude-code.jsonl")).unwrap();
    let repo = tmp_repo();
    let out_dir = tmp("matrix");
    let opts = RerunOpts { workspace: repo.display().to_string(), out_dir: Some(out_dir.clone()), quiet: true, replicates: 2, run_id: Some("t-matrix".into()), ..Default::default() };
    let (single, matrix) = rerun_matrix(&original, &opts, &mut no_log, &mut no_log).unwrap();
    assert!(single.is_none());
    let m = matrix.unwrap();
    assert_eq!(m.entries.len(), 2);
    assert_eq!(m.entries[0].label, "r1");
    assert!(m.entries.iter().all(|e| !e.pass && e.errors == 0), "clean execution without evaluation remains inconclusive");
    assert_eq!(m.groups.len(), 1);
    assert_eq!(m.groups[0].pass_at_1, 0.0);
    assert!(!m.groups[0].pass_pow_k);
    assert!(!m.groups[0].disagree);
    assert!(out_dir.join("replicates.json").exists());
    assert!(out_dir.join("report.md").exists());
    assert!(out_dir.join("r1/session.json").exists() && out_dir.join("r2/session.json").exists());
    // two simulator models × 1 replicate → two groups
    let out2 = tmp("matrix");
    let opts2 = RerunOpts { workspace: repo.display().to_string(), out_dir: Some(out2), quiet: true, user_mode: "simulate".into(), sim_llm: fake_llm("sim-verbatim"), sim_models: vec!["fake:sim-verbatim".into(), "fake:sim-stop".into()], run_id: Some("t-matrix2".into()), ..Default::default() };
    let (_, m2) = rerun_matrix(&original, &opts2, &mut no_log, &mut no_log).unwrap();
    let m2 = m2.unwrap();
    assert_eq!(m2.groups.len(), 2);
    let stop_group = m2.groups.iter().find(|g| g.simulator_model.as_deref() == Some("fake:sim-stop")).unwrap();
    assert!(!stop_group.pass_pow_k, "an out_of_scope stop before the last turn is not a pass");
    let ok_group = m2.groups.iter().find(|g| g.simulator_model.as_deref() == Some("fake:sim-verbatim")).unwrap();
    assert!(!ok_group.pass_pow_k, "execution alone is not task success");
}

fn call(turn: u32, id: &str, name: &str, input: serde_json::Value) -> Event {
    Event::tool_call(turn, "2026-01-01T00:00:00Z", id, name, input)
}

#[test]
fn action_taxonomy_maps_tools_and_shell_commands() {
    assert_eq!(classify_shell("grep -rn foo src").0, ActionKind::Search);
    assert_eq!(classify_shell("cat src/lib.py"), (ActionKind::FileRead, false, Some("src/lib.py".into())));
    assert_eq!(classify_shell("cargo test -q"), (ActionKind::Command, true, None));
    assert_eq!(classify_shell("cd src && ls").0, ActionKind::Navigate);
    assert_eq!(classify_shell("cat > out.txt <<'EOF'\nhi\nEOF").0, ActionKind::FileWrite);
    assert!(is_validation_command("python -m pytest tests/"));
    assert!(!is_validation_command("python setup.py --version"));
    let s = Session {
        events: vec![
            call(1, "a", "Read", json!({ "file_path": "/w/a.py" })),
            call(1, "b", "Grep", json!({ "pattern": "x" })),
            call(1, "c", "apply_patch", json!({ "patch": "*** Update File: a.py" })),
            call(1, "d", "run_shell_command", json!({ "command": "npm test" })),
            call(1, "e", "shell", json!({ "command": ["bash", "-lc", "rg TODO"] })),
            call(1, "f", "create", json!({ "path": "/w/b.py" })),
            call(1, "g", "Task", json!({ "prompt": "explore" })),
            Event::text(1, "2026-01-01T00:00:00Z", EventKind::Thinking, "hmm"),
        ],
        ..Default::default()
    };
    let kinds: Vec<ActionKind> = actions(&s).iter().map(|a| a.kind).collect();
    assert_eq!(kinds, [ActionKind::FileRead, ActionKind::Search, ActionKind::FileWrite, ActionKind::Command, ActionKind::Search, ActionKind::FileWrite, ActionKind::AgentSpawn, ActionKind::Reason]);
    assert!(actions(&s)[3].validation, "npm test is a validation command even through a generic shell tool");
    let st = stats(&s);
    assert_eq!(st.actions.get("file_write"), Some(&2));
    assert_eq!(st.actions.get("reason"), Some(&1));
}

#[test]
fn anti_patterns_follow_the_published_rules() {
    // search loop: 10 reads/searches with no write and no validation
    let mut events: Vec<Event> = (0..10).map(|i| call(1, &format!("r{i}"), "Grep", json!({ "pattern": "x" }))).collect();
    events.push(call(1, "w", "Write", json!({ "file_path": "/w/a.py" })));
    let ap = anti_patterns(&Session { events: events.clone(), ..Default::default() });
    assert_eq!(ap.search_loops, 1);
    assert!(ap.verification_skip, "write with no test afterwards");
    // nine reads is not a loop; a test after the write clears the skip
    let mut nine: Vec<Event> = (0..9).map(|i| call(1, &format!("r{i}"), "Grep", json!({ "pattern": "x" }))).collect();
    nine.push(call(1, "w", "Write", json!({ "file_path": "/w/a.py" })));
    nine.push(call(1, "t", "Bash", json!({ "command": "pytest -q" })));
    let ap2 = anti_patterns(&Session { events: nine, ..Default::default() });
    assert_eq!(ap2.search_loops, 0);
    assert!(!ap2.verification_skip);
    assert_eq!(ap2.tool_actions, 11);
    // re-read churn: same file read three times within ten actions without a write to it
    let churn = Session {
        events: vec![
            call(1, "1", "Read", json!({ "file_path": "/w/a.py" })),
            call(1, "2", "Read", json!({ "file_path": "/w/b.py" })),
            call(1, "3", "Read", json!({ "file_path": "/w/a.py" })),
            call(1, "4", "Read", json!({ "file_path": "/w/a.py" })),
        ],
        ..Default::default()
    };
    assert_eq!(anti_patterns(&churn).reread_churn_files, ["/w/a.py"]);
    let no_churn = Session {
        events: vec![
            call(1, "1", "Read", json!({ "file_path": "/w/a.py" })),
            call(1, "2", "Edit", json!({ "file_path": "/w/a.py" })),
            call(1, "3", "Read", json!({ "file_path": "/w/a.py" })),
            call(1, "4", "Read", json!({ "file_path": "/w/a.py" })),
        ],
        ..Default::default()
    };
    assert!(anti_patterns(&no_churn).reread_churn_files.is_empty(), "a write in between resets the window");
    // failed-action share from tool results
    let mut failing = Session { events: vec![call(1, "x", "Bash", json!({ "command": "ls" })), call(1, "y", "Bash", json!({ "command": "ls" }))], ..Default::default() };
    failing.events.push(Event::tool_result(1, "2026-01-01T00:00:01Z", "x", Some("Bash".into()), "boom", true));
    assert!((anti_patterns(&failing).failed_action_share - 0.5).abs() < 1e-9);
    // the fixtures: the Claude fixture has a verification attempt (pytest) after its last write
    let fx_s = claude_code::parse_file(&fx("claude-code.jsonl")).unwrap();
    let ap3 = stats(&fx_s).anti_patterns;
    assert_eq!(ap3.search_loops, 0);
    assert!(ap3.verification_skip, "the last write (turn 2 Edit) is not followed by a test run");
}

#[test]
fn divergence_recall_verdict_and_wilson() {
    let a = claude_code::parse_file(&fx("claude-code.jsonl")).unwrap();
    let mut b = a.clone();
    assert_eq!(first_divergent_turn(&a, &b), None);
    // change turn 2's action sequence
    let idx = b.events.iter().position(|e| e.turn == 2 && e.kind == EventKind::ToolCall).unwrap();
    b.events[idx] = call(2, "z", "Bash", json!({ "command": "grep x" }));
    assert_eq!(first_divergent_turn(&a, &b), Some(2));
    let r = compare_sessions(&a, &b, None, None, None);
    assert_eq!(r.first_divergent_turn, Some(2));
    assert!(r.action_sequence_similarity < 1.0 && r.action_sequence_similarity > 0.5);
    let mut c = a.clone();
    c.events.retain(|e| e.turn < 2);
    assert_eq!(first_divergent_turn(&a, &c), Some(2), "missing turn counts as divergence");
    let mut suffix_a = a.clone();
    let mut suffix_b = b.clone();
    suffix_a.events.retain(|e| e.turn >= 2);
    suffix_b.events.retain(|e| e.turn >= 2);
    assert_eq!(first_divergent_turn(&suffix_a, &suffix_b), Some(2), "fork suffixes retain original turn numbers");
    // recall: B reproduces half of A's lines plus extra lines of its own
    let pa = "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@\n+one\n+two\n";
    let pb = "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@\n+one\n+extra\n+more\n";
    let es = end_state_similarity(&Diff { patch: pa.into(), ..Default::default() }, &Diff { patch: pb.into(), ..Default::default() });
    assert!((es.recall - 0.5).abs() < 1e-9);
    assert!(es.content_similarity < es.recall, "symmetric Jaccard penalizes the extra edits, recall does not");
    assert_eq!(verdict(2, 3, 3), "validated");
    assert_eq!(verdict(3, 3, 3), "validated");
    assert_eq!(verdict(1, 3, 3), "partial");
    assert_eq!(verdict(0, 3, 3), "refuted");
    assert_eq!(verdict(0, 1, 3), "inconclusive");
    let (lo, hi) = wilson(1, 3);
    assert!(lo > 0.0 && lo < 0.1 && hi > 0.7);
    assert_eq!(wilson(0, 3).0, 0.0);
}

#[test]
fn every_adapter_rejects_empty_truncated_and_failed_streams() {
    for harness in Harness::all() {
        for mode in ["empty", "truncated", "failed", "bare-result"] {
            let opts = casimir::adapters::RunOpts {
                prompt: "hello".into(), bin: Some(fx("fake-stream.py").display().to_string()),
                extra_args: vec!["--fake-mode".into(), mode.into()], ..Default::default()
            };
            let result = casimir::adapters::run_turn(harness, &opts, &mut |_| {});
            assert!(result.as_ref().map_or(true, |r| r.is_error), "{harness}/{mode} must fail");
            if let Ok(res) = result {
                assert!(res.events.iter().any(|e| e.kind == EventKind::Error), "failure must be visible in the transcript");
                if mode != "empty" { assert!(!res.raw.is_empty(), "partial raw output must be saved"); }
            }
        }
    }
}

#[test]
fn copilot_and_gemini_reruns_resume_and_save_artifacts() {
    setup_env();
    let original = claude_code::parse_file(&fx("claude-code.jsonl")).unwrap();
    for harness in [Harness::Copilot, Harness::Gemini] {
        let out = tmp("stream-run");
        let opts = RerunOpts { harness: Some(harness), workspace: tmp_repo().display().to_string(), out_dir: Some(out.clone()), quiet: true, ..Default::default() };
        let result = rerun(&original, &opts, &mut no_log, &mut no_log).unwrap();
        let session = result.session.unwrap();
        let execution = session.execution.as_ref().unwrap();
        assert_eq!(execution.completed_turns, 2, "{harness}");
        assert_eq!(execution.failed_turns, 0, "{harness}");
        assert_eq!(stats(&session).assistant_messages, 2);
        assert!(out.join("session.json").exists() && out.join("report.md").exists() && out.join("raw.jsonl").exists());
        assert!(std::fs::read_to_string(out.join("diff.patch")).unwrap().contains("out.txt"));
        if harness == Harness::Gemini { assert_eq!(stats(&session).usage.output, 60); }
    }
}

#[test]
fn gemini_result_error_without_error_event_is_visible() {
    let opts = casimir::adapters::RunOpts {
        cwd: Some(tmp("gemini-error")), prompt: "hello".into(), bin: Some(fx("fake-stream.py").display().to_string()),
        extra_args: vec!["--fake-mode".into(), "result-error".into()], ..Default::default()
    };
    let result = gemini::run_turn(&opts, &mut |_| {}).unwrap();
    assert!(result.is_error);
    assert!(result.events.iter().any(|e| e.kind == EventKind::Error));
    assert_eq!(result.raw.len(), 3);
}

#[test]
fn control_group_measures_the_noise_floor() {
    setup_env();
    let mut original = claude_code::parse_file(&fx("claude-code.jsonl")).unwrap();
    original.model = Some("fake-model".into());
    let repo = tmp_repo();
    let out_dir = tmp("control");
    let opts = RerunOpts { workspace: repo.display().to_string(), out_dir: Some(out_dir.clone()), quiet: true, replicates: 2, control: true, model: Some("other-model".into()), run_id: Some("t-control".into()), ..Default::default() };
    let (_, m) = rerun_matrix(&original, &opts, &mut no_log, &mut no_log).unwrap();
    let m = m.unwrap();
    assert_eq!(m.entries.len(), 4);
    assert_eq!(m.entries.iter().filter(|e| e.control).count(), 2);
    assert!(m.entries[0].label.starts_with("control-"));
    assert_eq!(m.groups.len(), 2);
    let control = m.groups.iter().find(|g| g.control).unwrap();
    let target = m.groups.iter().find(|g| !g.control).unwrap();
    assert_eq!(control.verdict, "inconclusive");
    assert!(control.exceeds_control.is_none());
    // both groups run the same fake harness, so the target cannot exceed the control spread
    assert_eq!(target.exceeds_control, Some(false));
    assert!(target.mean_distance > 0.0, "fake harness trajectory differs from the original");
    assert!(m.entries.iter().all(|e| e.first_divergent_turn == Some(1)));
    assert!(out_dir.join("control-r1/record.jsonl").exists(), "record envelopes written");
    let rec = std::fs::read_to_string(out_dir.join("control-r1/record.jsonl")).unwrap();
    assert!(rec.contains("\"address\":\"tool:Write[1]\""));
    assert!(rec.contains("\"address\":\"model[1]\""));
}

#[test]
fn fork_at_turn_prepares_a_truncated_transcript_and_resumes() {
    setup_env();
    let repo = tmp_repo();
    let original = checkpointed_original(&repo);
    let out_dir = tmp("fork");
    let opts = RerunOpts { workspace: repo.display().to_string(), out_dir: Some(out_dir.clone()), quiet: true, from_turn: Some(2), intervention: Some("Make greet default to 'Earth' instead".into()), run_id: Some("t-fork".into()), ..Default::default() };
    let res = rerun(&original, &opts, &mut no_log, &mut no_log).unwrap();
    let session = res.session.unwrap();
    // prefix preserved verbatim from the original
    let prefix_tools: Vec<&str> = session.events.iter().filter(|e| e.turn == 1 && e.kind == EventKind::ToolCall && !e.sidechain).map(|e| e.tool.as_ref().unwrap().name.as_str()).collect();
    assert_eq!(prefix_tools, ["Bash", "Edit", "Write", "Bash"]);
    assert!(session.events.iter().any(|e| e.kind == EventKind::System && e.subtype.as_deref() == Some("fork")));
    let t2 = session.events.iter().find(|e| e.kind == EventKind::User && e.turn == 2).unwrap();
    assert_eq!(t2.text_str(), "Make greet default to 'Earth' instead");
    assert!(t2.simulated.as_ref().unwrap().reason.contains("intervention"));
    // the harness was resumed with the forked id and ran the intervention
    let reply = session.events.iter().find(|e| e.kind == EventKind::Assistant && e.turn == 2).unwrap();
    assert!(reply.text_str().contains("Working on: Make greet default to 'Earth'"));
    // the truncated transcript exists in the harness's project dir for the worktree/workspace
    let meta: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(out_dir.join("meta.json")).unwrap()).unwrap();
    assert_eq!(meta["forkedAtTurn"], json!(2));
    let transcript = PathBuf::from(meta["forkedTranscript"].as_str().unwrap());
    assert!(transcript.starts_with(std::env::var("CLAUDE_CONFIG_DIR").unwrap()));
    let forked = claude_code::parse_file(&transcript).unwrap();
    assert_eq!(user_turns(&forked).len(), 1, "only turn 1 kept");
    assert_eq!(forked.cwd.as_deref(), Some(res.workspace.dir.display().to_string().as_str()));
    assert_ne!(forked.id, original.id);
    // codex fork writes a truncated rollout too
    let codex_orig = codex::parse_file(&fx("codex.jsonl")).unwrap();
    let path = codex::prepare_fork(&codex_orig, 2, "eeeeeeee-1111-4222-8333-666666666666", &repo).unwrap();
    let forked_codex = codex::parse_file(&path).unwrap();
    assert_eq!(user_turns(&forked_codex).len(), 1);
    assert_eq!(forked_codex.id, "eeeeeeee-1111-4222-8333-666666666666");
    assert!(codex::prepare_fork(&codex_orig, 5, "x", &repo).is_err());
    assert!(casimir::adapters::prepare_fork(Harness::Gemini, &original, 2, "x", &repo).is_err());
}

#[test]
fn attribution_finds_the_point_of_commitment() {
    setup_env();
    let repo = tmp_repo();
    let original = checkpointed_original(&repo);
    let out_dir = tmp("attr");
    let opts = RerunOpts { workspace: repo.display().to_string(), out_dir: Some(out_dir.clone()), quiet: true, replicates: 2, judge: true, judge_llm: fake_llm("judge"), run_id: Some("t-attr".into()), ..Default::default() };
    let att = attribute(&original, &opts, &[2], &mut no_log, &mut no_log).unwrap();
    assert_eq!(att.effects.len(), 1);
    assert_eq!(att.effects[0].n, 2);
    assert_eq!(att.effects[0].passes, 2, "fake harness always completes cleanly");
    assert!(att.effects[0].ci_low > 0.0);
    assert_eq!(att.point_of_commitment, Some(2));
    assert!(out_dir.join("attribution.json").exists());
    assert!(out_dir.join("turn2/r1/session.json").exists());
}

#[test]
fn judge_repeats_measure_position_bias_and_test_retest() {
    setup_env();
    let a = claude_code::parse_file(&fx("claude-code.jsonl")).unwrap();
    let b = codex::parse_file(&fx("codex.jsonl")).unwrap();
    let jo = JudgeOpts { repeats: 2, ..JudgeOpts::new(fake_llm("judge-flip")) };
    let j = judge_sessions_with(&a, &b, None, None, &jo).unwrap();
    assert_eq!(j.repeats, 2);
    assert_eq!(j.passes.len(), 4);
    assert!((j.first_slot_win_rate - 1.0).abs() < 1e-9, "the biased fake judge always picks the first slot");
    assert!((j.position_bias - 0.5).abs() < 1e-9);
    assert_eq!(j.test_retest, Some(1.0), "…and does so repeatably");
    assert!(j.reliable_but_biased);
    assert!(j.order_sensitive);
    assert_eq!(j.winner, "tie");
    // family inference and the asymmetric-family warning
    assert_eq!(model_family("claude-opus-5"), "anthropic");
    assert_eq!(model_family("gpt-5-codex"), "openai");
    assert_eq!(model_family("gemini-2.5-pro"), "google");
    assert_eq!(model_family("fake:judge"), "unknown");
    let jo2 = JudgeOpts { llm: fake_llm("judge-consistent"), repeats: 1, brief: None, model_a: Some("claude-opus-5".into()), model_b: Some("gpt-5-codex".into()) };
    let mut anthropic_judge = jo2.clone();
    anthropic_judge.llm.model = Some("claude-sonnet-5".into());
    // the cmd backend ignores the model name except for the mode suffix, so this exercises the warning only
    let j2 = judge_sessions_with(&a, &b, None, None, &anthropic_judge).unwrap();
    assert_eq!(j2.judge_family, "anthropic");
    assert_eq!(j2.family_a, "anthropic");
    assert_eq!(j2.family_b, "openai");
    assert!(j2.family_warning.as_deref().unwrap().contains("candidate A only"));
    let same_both = JudgeOpts { model_a: Some("claude-opus-5".into()), model_b: Some("claude-sonnet-5".into()), ..anthropic_judge.clone() };
    assert!(judge_sessions_with(&a, &b, None, None, &same_both).unwrap().family_warning.is_none(), "symmetric families raise no warning");
    let text = render_compare_text(&compare_sessions(&a, &b, None, None, Some(j.clone())), "A", "B");
    assert!(text.contains("reliable-but-biased"));
    assert!(text.contains("position bias 0.50"));
}

#[test]
fn brief_rubric_invalid_reasons_and_intent_coverage() {
    setup_env();
    let a = claude_code::parse_file(&fx("claude-code.jsonl")).unwrap();
    let b = codex::parse_file(&fx("codex.jsonl")).unwrap();
    let brief = draft_brief(&a, None, &fake_llm("brief")).unwrap();
    assert_eq!(brief.criteria.len(), 3);
    assert!(brief.criteria[0].must);
    assert_eq!(brief.intents.len(), 3);
    assert!(!brief.human_reviewed);
    assert!(brief.rubric_text().contains("[C1] MUST"));
    assert!(brief.analysis_text().contains("intervened"));
    let dir = tmp("brief");
    brief.save(&dir.join("brief.json")).unwrap();
    let loaded = Brief::load(&dir.join("brief.json")).unwrap();
    assert_eq!(loaded, brief);
    let jo = JudgeOpts { brief: Some(loaded.clone()), ..JudgeOpts::new(fake_llm("judge-consistent")) };
    let j = judge_sessions_with(&a, &b, None, None, &jo).unwrap();
    assert!(j.rubric);
    // intent coverage: turn 1 verbatim by construction covers I1, I2; the fake matcher covers I3 and scopes message 0
    let mut rerun_s = a.clone();
    rerun_s.events.iter_mut().filter(|e| e.kind == EventKind::User && e.turn == 2).for_each(|e| {
        e.text = Some("please use World as the default".into());
        e.simulated = Some(casimir::model::Simulated { verbatim: false, reason: "adapted".into(), grounded_in: vec![2], action: Some("redirect".into()) });
    });
    let ic = intent_coverage(&loaded, &rerun_s, &fake_llm("intent")).unwrap().unwrap();
    assert_eq!(ic.intents, 3);
    assert_eq!(ic.covered.len(), 3);
    assert!((ic.recall - 1.0).abs() < 1e-9);
    assert!((ic.precision - 1.0).abs() < 1e-9);
    assert!((ic.score - 1.0).abs() < 1e-9);
    assert!(intent_coverage(&loaded, &a, &fake_llm("intent")).unwrap().is_none(), "no simulated turns → nothing to cover");
}

#[test]
fn lexicon_counters_and_simulator_drift() {
    let lx = lexicon_of(["ok", "Please fix lib.py, thanks", "Maybe use greet_name instead?", "Fix — now"]);
    assert_eq!(lx.turns, 4);
    assert!((lx.short_turn_rate - 0.5).abs() < 1e-9, "'ok' and 'Fix — now' are <= 3 words");
    assert!((lx.polite_rate - 0.25).abs() < 1e-9);
    assert!((lx.hedge_rate - 0.25).abs() < 1e-9);
    assert!((lx.pivot_rate - 0.25).abs() < 1e-9);
    assert!((lx.question_rate - 0.25).abs() < 1e-9);
    assert!((lx.em_dash_rate - 0.25).abs() < 1e-9);
    assert!((lx.identifier_tokens_per_turn - 0.5).abs() < 1e-9, "lib.py and greet_name");
    let original = claude_code::parse_file(&fx("claude-code.jsonl")).unwrap();
    assert!(simulator_drift(&original, &original).is_none());
    let mut rerun_s = original.clone();
    rerun_s.events.iter_mut().filter(|e| e.kind == EventKind::User && e.turn == 2).for_each(|e| {
        e.text = Some("Could you please also default it to World? Thanks!".into());
        e.simulated = Some(casimir::model::Simulated { verbatim: false, reason: "adapted".into(), grounded_in: vec![2], action: Some("answer".into()) });
    });
    let d = simulator_drift(&original, &rerun_s).unwrap();
    assert_eq!(d.human.turns, 2);
    assert_eq!(d.simulated.turns, 1);
    assert!((d.simulated.polite_rate - 1.0).abs() < 1e-9 && d.human.polite_rate == 0.0);
    let text = render_compare_text(&compare_sessions(&original, &rerun_s, None, None, None), "A", "B");
    assert!(text.contains("simulator drift"));
}

#[test]
fn simulated_rerun_with_brief_noop_and_pairs_export() {
    setup_env();
    let original = claude_code::parse_file(&fx("claude-code.jsonl")).unwrap();
    let repo = tmp_repo();
    let out_dir = tmp("noop");
    // the simulator says nothing at turn 2 → the run records a no-op and completes without that turn
    let opts = RerunOpts { workspace: repo.display().to_string(), out_dir: Some(out_dir.clone()), quiet: true, user_mode: "simulate".into(), sim_llm: fake_llm("sim-noop"), judge: true, judge_llm: fake_llm("brief"), run_id: Some("t-noop".into()), ..Default::default() };
    let res = rerun(&original, &opts, &mut no_log, &mut no_log).unwrap();
    let session = res.session.unwrap();
    assert!(session.events.iter().any(|e| e.kind == EventKind::System && e.subtype.as_deref() == Some("simulator-noop")));
    assert_eq!(user_turns(&session).len(), 1);
    assert!(out_dir.join("brief.json").exists(), "brief drafted into the run directory");
    let brief = Brief::load(&out_dir.join("brief.json")).unwrap();
    assert_eq!(brief.intents.len(), 3);
    let report = res.report.unwrap();
    assert!(report.judge.as_ref().unwrap().rubric, "judge used the drafted rubric");
    // an adapted turn: action label recorded, drift + intent coverage in the report, pairs exportable
    let out2 = tmp("adapt");
    let opts2 = RerunOpts { workspace: repo.display().to_string(), out_dir: Some(out2.clone()), quiet: true, user_mode: "simulate".into(), sim_llm: fake_llm("sim-adapt"), judge: true, judge_llm: fake_llm("brief"), brief: Some(out_dir.join("brief.json")), run_id: Some("t-adapt".into()), ..Default::default() };
    let res2 = rerun(&original, &opts2, &mut no_log, &mut no_log).unwrap();
    let s2 = res2.session.unwrap();
    let t2 = s2.events.iter().find(|e| e.kind == EventKind::User && e.turn == 2).unwrap();
    assert_eq!(t2.simulated.as_ref().unwrap().action.as_deref(), Some("redirect"));
    let r2 = res2.report.unwrap();
    assert!(r2.simulator_drift.is_some());
    let ic = r2.intent_coverage.expect("intent coverage computed when judged with a brief");
    assert!((ic.recall - 1.0).abs() < 1e-9);
    let pairs_dir = tmp("pairs");
    let exp = export_pairs(&[out2.clone(), out_dir.clone()], &pairs_dir).unwrap();
    assert_eq!(exp.n, 1, "only the adapted turn yields a pair; the no-op run has none");
    let key: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&exp.key_path).unwrap()).unwrap();
    let (pair_id, real) = key.as_object().unwrap().iter().next().map(|(k, v)| (k.clone(), v["real"].as_str().unwrap().to_string())).unwrap();
    let line = std::fs::read_to_string(&exp.pairs_path).unwrap();
    assert!(line.contains("please use World as the default") && line.contains("Also make greet default to 'World'"));
    assert!(!line.contains("\"real\""), "pairs file is blind");
    // an annotator who always picks the simulated message as human → pass rate 1.0
    let wrong = if real == "X" { "Y" } else { "X" };
    std::fs::write(pairs_dir.join("answers.json"), format!("{{\"{pair_id}\": \"{wrong}\"}}")).unwrap();
    let sc = score_pairs(&exp.key_path, &pairs_dir.join("answers.json")).unwrap();
    assert_eq!(sc.answered, 1);
    assert!((sc.pass_rate - 1.0).abs() < 1e-9);
    std::fs::write(pairs_dir.join("answers2.json"), format!("{{\"{pair_id}\": \"{real}\"}}")).unwrap();
    assert_eq!(score_pairs(&exp.key_path, &pairs_dir.join("answers2.json")).unwrap().simulator_passed, 0);
}

#[test]
fn matrix_reports_judge_quality_spread_and_notes() {
    setup_env();
    let mut original = claude_code::parse_file(&fx("claude-code.jsonl")).unwrap();
    original.model = Some("fake-model".into());
    let repo = tmp_repo();
    let out_dir = tmp("mx");
    let opts = RerunOpts { workspace: repo.display().to_string(), out_dir: Some(out_dir), quiet: true, replicates: 2, judge: true, judge_llm: fake_llm("judge-flip"), user_mode: "simulate".into(), sim_llm: fake_llm("sim-verbatim"), sim_models: vec!["fake:sim-verbatim".into(), "fake:sim-adapt".into()], control: true, run_id: Some("t-mx".into()), ..Default::default() };
    let (_, m) = rerun_matrix(&original, &opts, &mut no_log, &mut no_log).unwrap();
    let m = m.unwrap();
    assert_eq!(m.entries.len(), 8, "2 targets (control + target) × 2 simulators × 2 replicates");
    assert_eq!(m.groups.len(), 4);
    for g in &m.groups {
        assert_eq!(g.order_sensitive_rate, Some(1.0));
        assert!(g.judge_warning.as_deref().unwrap().contains("flipped on order swap"));
        assert!(g.pass_at_1_ci.0 <= g.pass_at_1 && g.pass_at_1 <= g.pass_at_1_ci.1);
    }
    // the control with the same simulator is the noise floor; targets with a different simulator are not compared
    for g in m.groups.iter().filter(|g| !g.control) {
        assert!(g.exceeds_control.is_some(), "each target has a control sharing its simulator");
    }
    assert_eq!(m.simulator_spread.len(), 2, "one spread per target (control and target)");
    assert!(m.notes.iter().any(|n| n.contains("relative comparisons")));
    assert!(m.notes.iter().any(|n| n.contains("coarse estimate")));
    let text = casimir::rerun::render_matrix_text(&m);
    assert!(text.contains("between-simulator spread"));
}
