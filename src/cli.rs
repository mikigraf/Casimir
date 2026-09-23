//! Command-line interface.
use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use std::fs;
use std::path::{Path, PathBuf};

use crate::adapters::{list_all_sessions, resolve_session};
use crate::brief::{draft_brief, render_brief_markdown, Brief};
use crate::compare::{compare_sessions, judge_sessions_with, render_compare_markdown, render_compare_text, JudgeOpts};
use crate::pairs::{export_pairs, score_pairs};
use crate::llm::LlmOpts;
use crate::model::{stats, user_turns, Harness};
use crate::play::{play, PlayOpts};
use crate::render::{render_markdown, render_session_list, render_stats, render_transcript, RenderOpts};
use crate::rerun::{attribute, render_attribution_text, render_matrix_text, rerun_matrix, RerunOpts};
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
    /// Maximum seconds per judge or simulator call
    #[arg(long, default_value_t = 300)]
    pub llm_timeout: u64,
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
        LlmOpts { timeout_secs: self.llm_timeout, model: self.llm_model.clone(), backend: self.llm.clone(), ..Default::default() }
    }
    fn judge_opts(&self) -> LlmOpts {
        LlmOpts { timeout_secs: self.llm_timeout, model: self.judge_model.clone().or_else(|| self.llm_model.clone()), backend: self.judge_llm.clone().unwrap_or_else(|| self.llm.clone()), ..Default::default() }
    }
}

#[derive(clap::Args, Debug, Clone)]
pub struct RunArgs {
    /// JSON executable checks, frozen outside the agent worktree
    #[arg(long)]
    pub checks: Option<PathBuf>,
    /// Maximum bytes in checkpoint storage (default 2 GiB)
    #[arg(long, default_value_t = crate::checkpoint::DEFAULT_LIMIT)]
    pub checkpoint_limit: u64,
    /// Maximum seconds for each harness turn
    #[arg(long, default_value_t = 900)]
    pub turn_timeout: u64,
    /// Explicitly permit unrestricted harness configuration (including passthrough flags)
    #[arg(long)]
    pub allow_unrestricted: bool,
    /// Target harness (default: same as original)
    #[arg(long, value_parser = Harness::parse)]
    pub harness: Option<Harness>,
    /// Target model (default: original model when same harness)
    #[arg(long)]
    pub model: Option<String>,
    /// How later user turns are produced
    #[arg(long = "user", default_value = "verbatim", value_parser = ["verbatim", "simulate"])]
    pub user_mode: String,
    /// auto = fresh git worktree at the session's base commit; worktree | same | <dir>
    #[arg(long, default_value = "auto")]
    pub workspace: String,
    /// Replay only the first N user turns
    #[arg(long)]
    pub turns: Option<usize>,
    /// Claude Code permission mode (default: preserve harness configuration)
    #[arg(long)]
    pub permission_mode: Option<String>,
    /// Codex sandbox (default: preserve harness configuration)
    #[arg(long)]
    pub sandbox: Option<String>,
    /// Ask an LLM to score original vs rerun (runs in both candidate orders)
    #[arg(long)]
    pub judge: bool,
    #[command(flatten)]
    pub llm: LlmArgs,
    /// Simulator model(s); repeat to bound simulator-induced variance (overrides --llm-model for the simulator)
    #[arg(long = "sim-model")]
    pub sim_model: Vec<String>,
    /// Simulator backend (overrides --llm for the simulator only)
    #[arg(long, value_parser = ["auto", "api", "claude-cli", "cmd"])]
    pub sim_llm: Option<String>,
    /// Replicate reruns per group (default: 3 with --judge, --control, fork or attribute; else 1)
    #[arg(long)]
    pub replicates: Option<usize>,
    /// Also rerun with the original harness and model as a control group (same-model noise floor)
    #[arg(long)]
    pub control: bool,
    /// Judge score (0-10) at or above which a replicate counts as a pass
    #[arg(long, default_value_t = 7.0)]
    pub pass_threshold: f64,
    /// Same-order judge repeats per ordering (>= 2 reports judge test-retest separately from agent variance)
    #[arg(long, default_value_t = 1)]
    pub judge_repeats: usize,
    /// Per-session brief (rubric, session analysis, intents) to use; default: draft one into the run directory
    #[arg(long)]
    pub brief: Option<PathBuf>,
    /// Run directory (or diff.patch) holding the original session's workspace diff; default: reconstruct from git
    #[arg(long)]
    pub original_diff: Option<PathBuf>,
    /// Show reasoning while running
    #[arg(long)]
    pub thinking: bool,
    /// Run directory (default ~/.casimir/runs/<id>)
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    #[arg(short, long)]
    pub quiet: bool,
    /// Keep replaying turns after a harness error
    #[arg(long)]
    pub continue_on_error: bool,
    /// Print the plan only
    #[arg(long)]
    pub dry_run: bool,
    /// Extra arguments passed to the harness CLI (after --)
    #[arg(last = true)]
    pub extra: Vec<String>,
}

impl RunArgs {
    fn opts(&self, forcing_replicates: bool) -> Result<RerunOpts> {
        let mut sim = self.llm.opts();
        if let Some(b) = &self.sim_llm {
            sim.backend = b.clone();
        }
        Ok(RerunOpts {
            checks: self.checks.clone(),
            checkpoint_limit: self.checkpoint_limit,
            turn_timeout_secs: self.turn_timeout,
            allow_unrestricted: self.allow_unrestricted,
            harness: self.harness,
            model: self.model.clone(),
            user_mode: self.user_mode.clone(),
            workspace: self.workspace.clone(),
            turns: self.turns,
            permission_mode: self.permission_mode.clone(),
            sandbox: self.sandbox.clone(),
            judge: self.judge,
            sim_llm: sim,
            judge_llm: self.llm.judge_opts(),
            out_dir: self.output.clone(),
            quiet: self.quiet,
            thinking: self.thinking,
            dry_run: self.dry_run,
            continue_on_error: self.continue_on_error,
            extra_args: self.extra.clone(),
            run_id: None,
            replicates: self.replicates.unwrap_or(if self.judge || self.control || forcing_replicates { 3 } else { 1 }),
            sim_models: self.sim_model.clone(),
            pass_threshold: self.pass_threshold,
            original_diff: match &self.original_diff {
                Some(p) => Some(load_diff(&p.display().to_string()).ok_or_else(|| anyhow::anyhow!("cannot read original diff from {}", p.display()))?),
                None => None,
            },
            control: self.control,
            from_turn: None,
            intervention: None,
            judge_repeats: self.judge_repeats,
            brief: self.brief.clone(),
            freeze_inputs: false,
            workspace_repo: None,
        })
    }
}

#[derive(Subcommand, Debug)]
pub enum Cmd {
    /// Generate AB/BA judge predictions for the frozen 40-pair corpus (makes model calls)
    PredictEvaluation {
        #[arg(long)] corpus: PathBuf,
        #[arg(short, long)] output: PathBuf,
        #[command(flatten)] llm: LlmArgs,
    },
    /// Score frozen evaluation predictions against independent reviews and adjudication
    Calibrate {
        #[arg(long)] corpus: PathBuf,
        #[arg(long)] predictions: PathBuf,
        #[arg(long)] reviewer_a: PathBuf,
        #[arg(long)] reviewer_b: PathBuf,
        #[arg(long)] adjudication: PathBuf,
        #[arg(short, long)] output: Option<PathBuf>,
    },
    /// Preview removal of manifest-owned run artifacts and worktrees
    Cleanup { run: PathBuf, #[arg(long)] apply: bool, #[arg(long)] checkpoints: bool },
    /// Continue a durable run; ambiguous prompts require an explicit new attempt
    Resume { run: PathBuf, #[arg(long)] retry_interrupted: bool },
    /// Diagnose local installation and login status without paid model calls
    Doctor { #[arg(long)] json: bool },
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
        /// Produce a redacted sharing export with an explicit redaction marker
        #[arg(long)]
        share: bool,
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
        #[command(flatten)]
        run: RunArgs,
    },
    /// Fork the original session at a turn and resume it (optionally with an edited message)
    Fork {
        session: String,
        /// User turn (1-based, >= 2) to fork at; turns before it are preserved verbatim
        #[arg(long)]
        at_turn: u32,
        /// Replacement user message for the forked turn (default: resample the original message)
        #[arg(long)]
        message: Option<String>,
        #[command(flatten)]
        run: RunArgs,
    },
    /// Resample the session at several turns to find the point of commitment of a failure
    Attribute {
        session: String,
        /// Turns to resample, e.g. 2,3,4 (default: every turn from 2)
        #[arg(long, value_delimiter = ',')]
        turns_at: Vec<u32>,
        #[command(flatten)]
        run: RunArgs,
    },
    /// Compare two sessions / run directories
    Compare {
        a: String,
        b: String,
        #[arg(long)]
        judge: bool,
        #[command(flatten)]
        llm: LlmArgs,
        /// Same-order judge repeats per ordering
        #[arg(long, default_value_t = 1)]
        judge_repeats: usize,
        /// Per-session brief whose rubric the judge scores against
        #[arg(long)]
        brief: Option<PathBuf>,
        #[arg(long, value_enum, default_value_t = Format::Text)]
        format: Format,
    },
    /// Draft a per-session brief (rubric, session analysis, intents) for humans to review and edit
    Brief {
        session: String,
        /// Where to write brief.json (default: ./brief-<id>.json); a .md summary is written next to it
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Run directory holding the session's workspace diff, for context
        #[arg(long)]
        original_diff: Option<PathBuf>,
        #[command(flatten)]
        llm: LlmArgs,
    },
    /// Export blinded original-vs-simulated message pairs from rerun directories for human 2AFC spot checks
    Pairs {
        /// Rerun directories (each with session.json and original.json)
        runs: Vec<PathBuf>,
        /// Output directory for pairs.jsonl, pairs.key.json and answers.template.json
        #[arg(short, long)]
        output: PathBuf,
    },
    /// Score 2AFC answers ({pair_id: "X"|"Y"} = the message believed to be the real human's) against the key
    PairsScore {
        key: PathBuf,
        answers: PathBuf,
    },
    /// List rerun directories under ~/.casimir/runs
    Runs,
}

fn load_diff(reference: &str) -> Option<Diff> {
    let p = Path::new(reference);
    if !p.exists() {
        return None;
    }
    if p.is_file() && p.extension().is_some_and(|e| e == "patch" || e == "diff") {
        let patch = fs::read_to_string(p).ok()?;
        let files = crate::workspace::parse_patch(&patch).keys().map(|p| crate::workspace::ChangedFile { status: "M".into(), path: p.clone() }).collect();
        return Some(Diff { patch, files, source: Some(format!("patch file {}", p.display())), ..Default::default() });
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
    let cli_fork_args: Option<(u32, Option<String>)> = match &cli.command {
        Cmd::Fork { at_turn, message, .. } => Some((*at_turn, message.clone())),
        _ => None,
    };
    match cli.command {
        Cmd::PredictEvaluation { corpus, output, llm } => {
            eprintln!("Preflight: 40 trace pairs, 80 ordered judge calls (up to 160 with JSON repair); no human labels are generated.");
            let result = crate::calibration::predict(&corpus, &output, &llm.judge_opts())?;
            println!("Predictions saved to {}", output.join("predictions.json").display());
            if result["failures"].as_object().is_some_and(|failures| !failures.is_empty()) { return Ok(1); }
        },
        Cmd::Calibrate { corpus, predictions, reviewer_a, reviewer_b, adjudication, output } => {
            let result = crate::calibration::score(&corpus, &predictions, &reviewer_a, &reviewer_b, &adjudication)?;
            if let Some(path) = output { crate::util::write_json(&path, &result)?; }
            println!("{}", serde_json::to_string_pretty(&result)?);
            if result["passed"] != true { return Ok(1); }
        },
        Cmd::Cleanup { run, apply, checkpoints } => println!("{}", serde_json::to_string_pretty(&crate::artifacts::cleanup_with_checkpoints(&run, apply, checkpoints)?)?),
        Cmd::Resume { run, retry_interrupted } => {
            let result = crate::recovery::resume(&run, retry_interrupted, &mut |s| eprintln!("{}", crate::privacy::redact(s)), &mut |s| println!("{s}"))?;
            if let Some(result) = result { println!("Recovery attempt: {}", result.run_dir.display());
                if result.session.as_ref().and_then(|s| s.execution.as_ref()).is_some_and(|e| e.failed_turns > 0) { return Ok(1); }
            }
        },
        Cmd::Doctor { json: as_json } => {
            let report = crate::doctor::report();
            if as_json { println!("{}", serde_json::to_string_pretty(&report)?); } else {
                println!("Casimir setup: {} {}", std::env::consts::OS, std::env::consts::ARCH);
                for harness in report["harnesses"].as_array().unwrap() { println!("{}: version={} auth={} executable={}", harness["id"], harness["version"], harness["authentication"], harness["executable"]); }
                println!("Storage writable: {}. Git: {}", report["storage"]["writable"], report["git"]);
                println!("Worktrees do not provide process isolation. No paid model calls made.");
            }
        },
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
        Cmd::Export { session, output, format, show, share } => {
            let s = resolve_session(&session)?;
            let fmt = format.unwrap_or(if output.as_ref().is_some_and(|o| o.extension().is_some_and(|e| e == "md")) { Format::Md } else { Format::Json });
            let body = match fmt {
                Format::Md => render_markdown(&s, &RenderOpts { thinking: true, ..show.render() }),
                _ => serde_json::to_string_pretty(&s)?,
            };
            let body = if share {
                match fmt {
                    Format::Md => format!("<!-- Casimir schemaVersion: 1; redacted: true -->\n\n{}", crate::privacy::redact(&body)),
                    _ => serde_json::to_string_pretty(&crate::privacy::share(&serde_json::to_value(&s)?))?,
                }
            } else { body };
            match output {
                Some(o) => {
                    crate::util::atomic_write(&o, body.as_bytes())?;
                    eprintln!("wrote {}", o.display());
                }
                None => println!("{body}"),
            }
        }
        Cmd::Attribute { session, turns_at, run } => {
            let original = resolve_session(&session)?;
            let mut opts = run.opts(true)?;
            if opts.original_diff.is_none() { opts.original_diff = load_diff(&session); }
            opts.judge = run.judge;
            let n = user_turns(&original).len() as u32;
            let turns: Vec<u32> = if turns_at.is_empty() { (2..=n).collect() } else { turns_at };
            if turns.is_empty() {
                anyhow::bail!("nothing to attribute: the session has a single turn (attribution resamples turns >= 2)");
            }
            let att = attribute(&original, &opts, &turns, &mut |s| eprintln!("{}", crate::privacy::redact(s)), &mut |s| println!("{s}"))?;
            println!();
            println!("{}", render_attribution_text(&att));
            if !att.dry_run { println!("{}saved under:{} {}", c.bold, c.reset, att.run_dir.display()); }
            if att.execution_failed { return Ok(1); }
        }
        Cmd::Rerun { session, run } | Cmd::Fork { session, run, .. } => {
            let original = resolve_session(&session)?;
            let (from_turn, intervention) = match &cli_fork_args {
                Some((t, m)) => (Some(*t), m.clone()),
                None => (None, None),
            };
            let mut opts = run.opts(from_turn.is_some())?;
            if opts.original_diff.is_none() { opts.original_diff = load_diff(&session); }
            opts.from_turn = from_turn;
            opts.intervention = intervention;
            let (single, matrix) = rerun_matrix(&original, &opts, &mut |s| eprintln!("{}", crate::privacy::redact(s)), &mut |s| println!("{s}"))?;
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
                    println!("{}worktree kept at {root} (preview removal with: casimir cleanup RUN){}", c.dim, c.reset);
                }
                println!("{}casimir show {}   |   casimir compare {} {}{}", c.dim, res.run_dir.display(), original.path.clone().unwrap_or(original.id.clone()), res.run_dir.display(), c.reset);
                if res.session.as_ref().unwrap().execution.as_ref().is_some_and(|e| e.failed_turns > 0) || (opts.judge && res.report.as_ref().unwrap().judge.is_none()) { return Ok(1); }
            }
            if let Some(m) = matrix {
                println!();
                println!("{}", render_matrix_text(&m));
                println!();
                println!("{}runs saved under:{} {}", c.bold, c.reset, m.run_dir.display());
                println!("{}worktrees are kept under {} (preview each run with: casimir cleanup RUN){}", c.dim, casimir_home().join("worktrees").display(), c.reset);
                if m.entries.iter().any(|e| e.errors > 0 || (m.judged && e.judge_score.is_none())) { return Ok(1); }
            }
        }
        Cmd::Compare { a, b, judge, llm, judge_repeats, brief, format } => {
            let sa = resolve_session(&a)?;
            let sb = resolve_session(&b)?;
            // run dirs carry a captured diff; for raw logs fall back to reconstructing from git history
            let da = load_diff(&a).or_else(|| reconstruct_original_diff(&sa));
            let db = load_diff(&b).or_else(|| reconstruct_original_diff(&sb));
            let j = if judge {
                let b = match brief {
                    Some(p) => Some(Brief::load(&p)?),
                    None => Path::new(&b).join("brief.json").exists().then(|| Brief::load(&Path::new(&b).join("brief.json"))).transpose()?,
                };
                let jo = JudgeOpts { llm: llm.judge_opts(), repeats: judge_repeats.max(1), brief: b, model_a: sa.model.clone(), model_b: sb.model.clone() };
                Some(judge_sessions_with(&sa, &sb, da.as_ref(), db.as_ref(), &jo)?)
            } else {
                None
            };
            let report = compare_sessions(&sa, &sb, da, db, j);
            match format {
                Format::Json => println!("{}", serde_json::to_string_pretty(&report)?),
                Format::Md => println!("{}", render_compare_markdown(&report, "A", "B")),
                Format::Text => println!("{}", render_compare_text(&report, &format!("A: {}", sa.harness()), &format!("B: {}", sb.harness()))),
            }
        }
        Cmd::Brief { session, output, original_diff, llm } => {
            let s = resolve_session(&session)?;
            let diff = match original_diff {
                Some(p) => Some(load_diff(&p.display().to_string()).ok_or_else(|| anyhow::anyhow!("cannot read original diff from {}", p.display()))?),
                None => load_diff(&session).or_else(|| reconstruct_original_diff(&s)),
            };
            let b = draft_brief(&s, diff.as_ref(), &llm.judge_opts())?;
            let out = output.unwrap_or_else(|| PathBuf::from(format!("brief-{}.json", s.id.chars().take(8).collect::<String>())));
            b.save(&out)?;
            let md = out.with_extension("md");
            fs::write(&md, render_brief_markdown(&b))?;
            println!("{}", render_brief_markdown(&b));
            eprintln!("wrote {} and {} — review, edit, set humanReviewed to true, then pass --brief {}", out.display(), md.display(), out.display());
        }
        Cmd::Pairs { runs, output } => {
            let exp = export_pairs(&runs, &output)?;
            println!("{} pair(s) written to {}", exp.n, exp.pairs_path.display());
            println!("key (keep it away from annotators): {}", exp.key_path.display());
            println!("fill answers.template.json with X or Y per pair, then: casimir pairs-score {} <answers.json>", exp.key_path.display());
        }
        Cmd::PairsScore { key, answers } => {
            let sc = score_pairs(&key, &answers)?;
            println!("answered: {}  simulator mistaken for the human: {}  Turing pass rate: {:.2}  95% CI [{:.2}, {:.2}]  sessions: {}", sc.answered, sc.simulator_passed, sc.pass_rate, sc.ci_low, sc.ci_high, sc.sessions);
            println!("0.5 = indistinguishable from the real user; the interval treats pairs as independent, which understates width when several pairs share a session");
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
