use casimir::adapters::{claude_code, codex, load_session_file, resolve_session};
use casimir::compare::{compare_sessions, relativize, render_compare_markdown};
use casimir::model::{files_touched, final_assistant_text, stats, tool_one_liner, user_turns, Event, EventKind, Harness, Session, Usage};
use casimir::render::{render_markdown, render_transcript, RenderOpts};
use casimir::rerun::{rerun, RerunOpts};
use casimir::util::extract_json;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Once;

fn fx(name: &str) -> PathBuf {
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
        std::env::set_var("CASIMIR_HOME", tmp("home"));
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
