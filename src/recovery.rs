//! A single atomic document is the commit record for completed turn boundaries.
use crate::{
    model::Session,
    rerun::{RerunOpts, RerunOutcome},
    simulate::SimState,
};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Recovery {
    pub schema_version: u32,
    pub state: String,
    pub options: RerunOpts,
    pub original: Session,
    pub session: Session,
    pub simulator: SimState,
    pub next_turn: u32,
    pub checkpoint: Option<String>,
    pub active: bool,
    pub source: PathBuf,
    #[serde(default)]
    pub resumed_as: Option<PathBuf>,
}
impl Recovery {
    pub fn save(&self, run_dir: &Path) -> Result<()> {
        crate::util::write_json(&run_dir.join("recovery.json"), self)
    }
}

pub fn resume(
    run_dir: &Path,
    retry: bool,
    log: &mut dyn FnMut(&str),
    out: &mut dyn FnMut(&str),
) -> Result<Option<RerunOutcome>> {
    let _lock = crate::util::RunLock::acquire(run_dir)?;
    let mut recovery: Recovery = crate::util::read_json(&run_dir.join("recovery.json"))
        .context("run has no durable recovery record (legacy runs cannot be resumed safely)")?;
    if recovery.schema_version != 1 {
        bail!("unsupported recovery schema");
    }
    if recovery.state == "completed" {
        log("Run is already complete; no prompts were sent.");
        return Ok(None);
    }
    if let Some(attempt) = &recovery.resumed_as {
        bail!(
            "this run already has a recovery attempt; resume {} instead",
            attempt.display()
        );
    }
    if recovery.active && !retry {
        bail!("turn {} may already have executed; use --retry-interrupted to create a new attempt from its verified checkpoint", recovery.next_turn);
    }
    let checkpoint_id = recovery
        .checkpoint
        .as_deref()
        .context("no verified boundary checkpoint is available; cannot safely resume")?;
    let checkpoint = crate::checkpoint::load(checkpoint_id)?;
    if recovery.next_turn as usize
        <= recovery
            .options
            .turns
            .unwrap_or(crate::model::user_turns(&recovery.original).len())
    {
        crate::checkpoint::require_compatible(&checkpoint)?;
    }
    let mut options = recovery.options.clone();
    let id = format!(
        "{}-attempt-{}",
        crate::util::now_stamp(),
        uuid::Uuid::new_v4()
    );
    let output = crate::util::casimir_home().join("runs").join(&id);
    options.run_id = Some(id);
    options.out_dir = Some(output.clone());
    options.from_turn = None;
    options.intervention = None;
    options.dry_run = false;
    // Reserve the attempt before any model call. A second caller is directed to this attempt.
    crate::util::private_dir(&output)?;
    recovery.source = run_dir.to_path_buf();
    recovery.resumed_as = None;
    recovery.save(&output)?;
    let original = recovery.original.clone();
    let mut old = recovery.clone();
    old.resumed_as = Some(output);
    old.save(run_dir)?;
    Ok(Some(crate::rerun::rerun_recovered(
        &original, &options, recovery, log, out,
    )?))
}
