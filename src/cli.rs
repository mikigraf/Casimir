//! Command-line interface.
use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use std::fs;
use std::path::{Path, PathBuf};

use crate::adapters::{list_all_sessions, resolve_session};
use crate::compare::{compare_sessions, judge_sessions, render_compare_markdown, render_compare_text};
use crate::llm::LlmOpts;
use crate::model::{stats, Harness};
use crate::play::{play, PlayOpts};
use crate::render::{render_markdown, render_session_list, render_stats, render_transcript, RenderOpts};
use crate::rerun::{render_matrix_text, rerun_matrix, RerunOpts};
use crate::util::{casimir_home, colors, read_json};
use crate::workspace::{reconstruct_original_diff, Diff};

const ABOUT: &str = "Replay, rerun and compare coding-agent sessions (Claude Code, Codex, Copilot CLI, Gemini CLI).

<SESSION> is a log path, a casimir run dir, \"last\", \"claude:last\", \"codex:last\", \"copilot:last\",
\"gemini:last\", a session id, or a unique id prefix (optionally \"codex:<prefix>\").";

#[derive(Parser, Debug)]
#[command(name = "casimir", version, about = ABOUT)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Cmd,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum Format {
    Text,
    Md,
    Json,
}

#[derive(clap::Args, Debug, Clone)]
pub struct ShowArgs {
    /// Include reasoning blocks
    #[arg(long)]
    pub thinking: bool,
    /// Do not truncate tool output
    #[arg(long)]
    pub full: bool,
    /// Include subagent traffic
    #[arg(long)]
    pub sidechains: bool,
    /// Only one user turn
    #[arg(long)]
    pub turn: Option<u32>,
    /// Lines of tool output to keep per result
    #[arg(long, default_value_t = 12)]
    pub max_lines: usize,
}

impl ShowArgs {
    fn render(&self) -> RenderOpts {
        RenderOpts { thinking: self.thinking, full: self.full, sidechains: self.sidechains, turn: self.turn, max_lines: self.max_lines, ..Default::default() }
    }
}

#[derive(clap::Args, Debug, Clone)]
pub struct LlmArgs {
    /// Default backend for the user simulator and judge
    #[arg(long, default_value = "auto", value_parser = ["auto", "api", "claude-cli", "cmd"])]
    pub llm: String,
    /// Default model for the user simulator and judge (default claude-opus-5)
    #[arg(long)]
    pub llm_model: Option<String>,
    /// Judge model (overrides --llm-model for the judge only)
    #[arg(long)]
    pub judge_model: Option<String>,
    /// Judge backend (overrides --llm for the judge only)
    #[arg(long, value_parser = ["auto", "api", "claude-cli", "cmd"])]
    pub judge_llm: Option<String>,
}

impl LlmArgs {
    fn opts(&self) -> LlmOpts {
        LlmOpts { model: self.llm_model.clone(), backend: self.llm.clone(), ..Default::default() }
    }
    fn judge_opts(&self) -> LlmOpts {
        LlmOpts { model: self.judge_model.clone().or_else(|| self.llm_model.clone()), backend: self.judge_llm.clone().unwrap_or_else(|| self.llm.clone()), ..Default::default() }
    }
}

#[derive(Subcommand, Debug)]
pub enum Cmd {
    /// List recorded sessions from all harnesses
    List {
        #[arg(long, value_parser = Harness::parse)]
        harness: Option<Harness>,
        /// Only sessions recorded in this directory
        #[arg(long)]
        cwd: Option<PathBuf>,
        #[arg(long, default_value_t = 30)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    /// Print a transcript
    Show {
        session: String,
        #[arg(long, value_enum, default_value_t = Format::Text)]
        format: Format,
        #[command(flatten)]
        show: ShowArgs,
    },
    /// Replay with the original pacing
    Play {
        session: String,
        /// Playback speed multiplier
        #[arg(long, default_value_t = 5.0)]
        speed: f64,
        /// Longest pause between events, in milliseconds
        #[arg(long, default_value_t = 2000)]
        max_delay: u64,
        #[command(flatten)]
        show: ShowArgs,
    },
    /// Aggregate numbers for a session
    Stats {
        session: String,
        #[arg(long)]
        json: bool,
    },
    /// Write normalized JSON or markdown
    Export {
        session: String,
        #[arg(short, long)]
        output: Option<PathBuf>,
        #[arg(long, value_enum)]
        format: Option<Format>,
        #[command(flatten)]
        show: ShowArgs,
    },
    /// Replay the user's turns against a harness/model
    Rerun {
        session: String,
        /// Target harness (default: same as original)
        #[arg(long, value_parser = Harness::parse)]
        harness: Option<Harness>,
        /// Target model (default: original model when same harness)
        #[arg(long)]
        model: Option<String>,
        /// How later user turns are produced
        #[arg(long = "user", default_value = "verbatim", value_parser = ["verbatim", "simulate"])]
        user_mode: String,
        /// auto = fresh git worktree at the session's base commit; worktree | same | <dir>
        #[arg(long, default_value = "auto")]
        workspace: String,
        /// Replay only the first N user turns
        #[arg(long)]
        turns: Option<usize>,
        /// Claude Code permission mode (default: bypass in isolated workspaces, else acceptEdits)
        #[arg(long)]
        permission_mode: Option<String>,
        /// Codex sandbox (default: bypass in isolated workspaces, else workspace-write)
        #[arg(long)]
        sandbox: Option<String>,
        /// Ask an LLM to score original vs rerun (runs in both candidate orders)
        #[arg(long)]
        judge: bool,
        #[command(flatten)]
        llm: LlmArgs,
        /// Simulator model(s); repeat to bound simulator-induced variance (overrides --llm-model for the simulator)
        #[arg(long = "sim-model")]
        sim_model: Vec<String>,
        /// Simulator backend (overrides --llm for the simulator only)
        #[arg(long, value_parser = ["auto", "api", "claude-cli", "cmd"])]
        sim_llm: Option<String>,
        /// Number of replicate reruns per simulator model
        #[arg(long, default_value_t = 1)]
        replicates: usize,
        /// Judge score (0-10) at or above which a replicate counts as a pass
        #[arg(long, default_value_t = 7.0)]
        pass_threshold: f64,
        /// Run directory (or diff.patch) holding the original session's workspace diff; default: reconstruct from git
        #[arg(long)]
        original_diff: Option<PathBuf>,
        /// Show reasoning while running
        #[arg(long)]
        thinking: bool,
        /// Run directory (default ~/.casimir/runs/<id>)
        #[arg(short, long)]
        output: Option<PathBuf>,
        #[arg(short, long)]
        quiet: bool,
        /// Keep replaying turns after a harness error
        #[arg(long)]
        continue_on_error: bool,
        /// Print the plan only
        #[arg(long)]
        dry_run: bool,
        /// Extra arguments passed to the harness CLI (after --)
        #[arg(last = true)]
        extra: Vec<String>,
    },
    /// Compare two sessions / run directories
    Compare {
        a: String,
        b: String,
        #[arg(long)]
        judge: bool,
        #[command(flatten)]
        llm: LlmArgs,
        #[arg(long, value_enum, default_value_t = Format::Text)]
        format: Format,
    },
    /// List rerun directories under ~/.casimir/runs
    Runs,
}

fn load_diff(reference: &str) -> Option<Diff> {
    let p = Path::new(reference);
    if !p.exists() {
        return None;
    }
    let dir = if p.is_dir() { p.to_path_buf() } else { p.parent()?.to_path_buf() };
    let dj = dir.join("diff.json");
    if !dj.exists() {
        return None;
    }
    let mut d: Diff = read_json(&dj).ok()?;
    if let Ok(patch) = fs::read_to_string(dir.join("diff.patch")) {
        d.patch = patch;
    }
    if d.source.is_none() {
        d.source = Some(format!("captured in run {}", dir.display()));
    }
    Some(d)
}

pub fn run() -> Result<i32> {
    let cli = Cli::parse();
    let c = colors();
    match cli.command {
        Cmd::List { harness, cwd, limit, json } => {
            let mut items = list_all_sessions(harness);
            if let Some(want) = cwd {
                let want = fs::canonicalize(&want).unwrap_or(want);
                items.retain(|s| s.cwd.as_deref().map(Path::new).and_then(|p| fs::canonicalize(p).ok()).is_some_and(|p| p == want));
            }
            items.truncate(limit);
            if json {
                println!("{}", serde_json::to_string_pretty(&items)?);
            } else {
                println!("{}", render_session_list(&items));
            }
        }
        Cmd::Show { session, format, show } => {
            let s = resolve_session(&session)?;
            match format {
                Format::Json => println!("{}", serde_json::to_string_pretty(&s)?),
                Format::Md => println!("{}", render_markdown(&s, &show.render())),
                Format::Text => println!("{}", render_transcript(&s, &show.render())),
            }
        }
        Cmd::Play { session, speed, max_delay, show } => {
            let s = resolve_session(&session)?;
            let mut stdout = std::io::stdout().lock();
            play(&s, &PlayOpts { speed, max_delay_ms: max_delay, render: show.render() }, &mut stdout)?;
        }
        Cmd::Stats { session, json } => {
            let s = resolve_session(&session)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&stats(&s))?);
            } else {
                println!("{}", render_stats(&s));
            }
        }
        Cmd::Export { session, output, format, show } => {
            let s = resolve_session(&session)?;
            let fmt = format.unwrap_or(if output.as_ref().is_some_and(|o| o.extension().is_some_and(|e| e == "md")) { Format::Md } else { Format::Json });
            let body = match fmt {
                Format::Md => render_markdown(&s, &RenderOpts { thinking: true, ..show.render() }),
                _ => serde_json::to_string_pretty(&s)?,
            };
            match output {
                Some(o) => {
                    fs::write(&o, body)?;
                    eprintln!("wrote {}", o.display());
                }
                None => println!("{body}"),
            }
        }
        Cmd::Rerun { session, harness, model, user_mode, workspace, turns, permission_mode, sandbox, judge, llm, sim_model, sim_llm, replicates, pass_threshold, original_diff, thinking, output, quiet, continue_on_error, dry_run, extra } => {
            let original = resolve_session(&session)?;
            let mut sim = llm.opts();
            if let Some(b) = sim_llm {
                sim.backend = b;
            }
            let opts = RerunOpts {
                harness,
                model,
                user_mode,
                workspace,
                turns,
                permission_mode,
                sandbox,
                judge,
                sim_llm: sim,
                judge_llm: llm.judge_opts(),
                out_dir: output,
                quiet,
                thinking,
                dry_run,
                continue_on_error,
                extra_args: extra,
                run_id: None,
                replicates,
                sim_models: sim_model,
                pass_threshold,
                original_diff: original_diff.as_deref().and_then(|p| load_diff(&p.display().to_string())),
            };
            let (single, matrix) = rerun_matrix(&original, &opts, &mut |s| eprintln!("{s}"), &mut |s| println!("{s}"))?;
            if let Some(res) = single {
                if res.dry_run {
                    return Ok(0);
                }
                println!();
                println!("{}", render_compare_text(res.report.as_ref().unwrap(), "original", "rerun"));
                println!();
                println!("{}run saved:{} {}", c.bold, c.reset, res.run_dir.display());
                if res.workspace.mode == "worktree" {
                    let root = res.workspace.root.as_ref().unwrap().display();
                    println!("{}worktree kept at {root} (remove with: git worktree remove --force {root}){}", c.dim, c.reset);
                }
                println!("{}casimir show {}   |   casimir compare {} {}{}", c.dim, res.run_dir.display(), original.path.clone().unwrap_or(original.id.clone()), res.run_dir.display(), c.reset);
            }
            if let Some(m) = matrix {
                println!();
                println!("{}", render_matrix_text(&m));
                println!();
                println!("{}runs saved under:{} {}", c.bold, c.reset, m.run_dir.display());
                println!("{}worktrees are kept under {} (remove with: git worktree remove --force <dir>){}", c.dim, casimir_home().join("worktrees").display(), c.reset);
            }
        }
        Cmd::Compare { a, b, judge, llm, format } => {
            let sa = resolve_session(&a)?;
            let sb = resolve_session(&b)?;
            // run dirs carry a captured diff; for raw logs fall back to reconstructing from git history
            let da = load_diff(&a).or_else(|| reconstruct_original_diff(&sa));
            let db = load_diff(&b).or_else(|| reconstruct_original_diff(&sb));
            let j = if judge { Some(judge_sessions(&sa, &sb, da.as_ref(), db.as_ref(), &llm.judge_opts())?) } else { None };
            let report = compare_sessions(&sa, &sb, da, db, j);
            match format {
                Format::Json => println!("{}", serde_json::to_string_pretty(&report)?),
                Format::Md => println!("{}", render_compare_markdown(&report, "A", "B")),
                Format::Text => println!("{}", render_compare_text(&report, &format!("A: {}", sa.harness()), &format!("B: {}", sb.harness()))),
            }
        }
        Cmd::Runs => {
            let root = casimir_home().join("runs");
            if !root.exists() {
                println!("(no runs yet)");
                return Ok(0);
            }
            let mut dirs: Vec<PathBuf> = fs::read_dir(&root)?.flatten().map(|e| e.path()).filter(|p| p.join("meta.json").exists()).collect();
            dirs.sort();
            dirs.reverse();
            if dirs.is_empty() {
                println!("(no runs yet)");
            }
            for d in dirs {
                let meta: serde_json::Value = read_json(&d.join("meta.json"))?;
                let name = d.file_name().and_then(|n| n.to_str()).unwrap_or("");
                let model = meta.get("model").and_then(|m| m.as_str()).map(|m| format!("/{m}")).unwrap_or_default();
                let orig = meta.get("original").cloned().unwrap_or_default();
                println!("{name}  {}{model}  ← {}:{}", meta.get("harness").and_then(|h| h.as_str()).unwrap_or("?"), orig.get("harness").and_then(|h| h.as_str()).unwrap_or("?"), orig.get("id").and_then(|h| h.as_str()).unwrap_or("?"));
            }
        }
    }
    Ok(0)
}
