//! Frozen executable validation, separate from process completion and model judgments.
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::{path::Path, process::Command, time::{Duration, Instant}};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Check {
    pub executable: String,
    #[serde(default)] pub args: Vec<String>,
    #[serde(default = "timeout")] pub timeout_secs: u64,
    #[serde(default)] pub expected_exit_status: i32,
}
fn timeout() -> u64 { 300 }
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Definition {
    pub schema_version: u32,
    pub checks: Vec<Check>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResultRecord {
    pub index: usize,
    pub outcome: String,
    pub exit_status: Option<i32>,
    pub expected_exit_status: i32,
    pub duration_ms: u128,
    pub stdout: String,
    pub stderr: String,
    pub error: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Results {
    pub schema_version: u32,
    pub definition_hash: String,
    pub outcome: String,
    pub results: Vec<ResultRecord>,
}

pub fn freeze(source: &Path, run_dir: &Path, workspace: &Path) -> Result<(Definition, String)> {
    let bytes = std::fs::read(source)?;
    let definition: Definition = serde_json::from_slice(&bytes)?;
    if definition.schema_version != 1 || definition.checks.is_empty() { bail!("checks require schemaVersion 1 and a nonempty checks array"); }
    if definition.checks.iter().any(|c| c.executable.trim().is_empty() || c.timeout_secs == 0) { bail!("each check requires an executable and positive timeoutSecs"); }
    let frozen = run_dir.join("checks.definition.json");
    let run = std::fs::canonicalize(run_dir)?;
    let workspace = std::fs::canonicalize(workspace)?;
    if run.starts_with(&workspace) { bail!("run output containing frozen checks must be outside the agent workspace"); }
    crate::util::atomic_write(&frozen, &bytes)?;
    Ok((definition, crate::checkpoint::hash(&bytes)))
}

pub fn execute(definition: &Definition, hash: &str, cwd: &Path, run_dir: &Path) -> Result<Results> {
    // Check the immutable input again after the agent has run; never trust a modified file.
    if crate::checkpoint::hash(&std::fs::read(run_dir.join("checks.definition.json"))?) != hash { bail!("frozen check definition was modified"); }
    let mut results = Results { schema_version: 1, definition_hash: hash.into(), outcome: "passed".into(), results: Vec::new() };
    for (index, check) in definition.checks.iter().enumerate() {
        let start = Instant::now();
        let spool = run_dir.join("checks").join(index.to_string());
        let mut command = Command::new(&check.executable);
        command.args(&check.args).current_dir(cwd);
        let output = crate::process::capture(&mut command, b"", Duration::from_secs(check.timeout_secs), Some(&spool));
        let (outcome, exit_status, error) = match output {
            Ok(output) => (if output.status.code() == Some(check.expected_exit_status) { "passed" } else { "failed" }, output.status.code(), None),
            Err(err) => ("error", None, Some(err.to_string())),
        };
        if outcome != "passed" { results.outcome = outcome.into(); }
        results.results.push(ResultRecord { index, outcome: outcome.into(), exit_status, expected_exit_status: check.expected_exit_status,
            duration_ms: start.elapsed().as_millis(), stdout: spool.join("stdout.log").display().to_string(), stderr: spool.join("stderr.log").display().to_string(), error });
        crate::util::write_json(&run_dir.join("checks.json"), &results)?;
    }
    Ok(results)
}
