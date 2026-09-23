//! Ownership records are created when an empty run directory is claimed. Cleanup is preview-first.
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ownership {
    pub schema_version: u32,
    pub id: String,
    pub run: PathBuf,
    pub worktree: Option<PathBuf>,
    pub repository: Option<PathBuf>,
}
fn worktree_marker(path: &Path) -> Result<PathBuf> {
    let output = std::process::Command::new("git").args(["rev-parse", "--absolute-git-dir"]).current_dir(path).output()?;
    if !output.status.success() { bail!("owned worktree Git metadata is missing"); }
    Ok(PathBuf::from(String::from_utf8(output.stdout)?.trim()).join("casimir-owner"))
}
fn registry() -> PathBuf { crate::util::casimir_home().join("ownership") }
pub fn register(run: &Path, workspace: &crate::rerun::WorkspacePlan) -> Result<()> {
    let run = std::fs::canonicalize(run)?;
    let id = uuid::Uuid::new_v4().to_string();
    let worktree = workspace.root.as_ref().filter(|_| workspace.mode == "worktree").map(std::fs::canonicalize).transpose()?;
    let ownership = Ownership { schema_version: 1, id: id.clone(), run: run.clone(), worktree, repository: workspace.repo.clone() };
    crate::util::atomic_write(&run.join(".casimir-owned"), id.as_bytes())?;
    if let Some(path) = &ownership.worktree { crate::util::atomic_write(&worktree_marker(path)?, id.as_bytes())?; }
    crate::util::private_dir(&registry())?;
    crate::util::write_json(&registry().join(format!("{id}.json")), &ownership)
}
fn verify(record: &Ownership) -> Result<()> {
    if record.schema_version != 1 || uuid::Uuid::parse_str(&record.id).is_err() { bail!("invalid ownership record"); }
    if std::fs::canonicalize(&record.run)? != record.run || record.run.parent().is_none() { bail!("run ownership path changed"); }
    if std::fs::read_to_string(record.run.join(".casimir-owned"))? != record.id { bail!("run ownership marker does not match"); }
    if let Some(path) = &record.worktree {
        let allowed = std::fs::canonicalize(crate::util::casimir_home().join("worktrees"))?;
        if !path.starts_with(&allowed) || path == &allowed { bail!("refusing worktree outside Casimir-owned storage"); }
        // A previous cleanup can have removed the worktree before interruption.
        if std::fs::symlink_metadata(path).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound) { return Ok(()); }
        if std::fs::canonicalize(path)? != *path { bail!("worktree ownership path changed"); }
        if std::fs::read_to_string(worktree_marker(path)?)? != record.id { bail!("worktree ownership marker does not match"); }
        let repo = record.repository.as_ref().context("owned worktree has no repository")?;
        if std::fs::canonicalize(repo)? == *path { bail!("refusing to remove the original repository"); }
    }
    Ok(())
}
pub fn cleanup(run: &Path, apply: bool) -> Result<serde_json::Value> {
    cleanup_with_checkpoints(run, apply, false)
}
pub fn cleanup_with_checkpoints(run: &Path, apply: bool, checkpoints: bool) -> Result<serde_json::Value> {
    let canonical = std::fs::canonicalize(run)?;
    let mut found = None;
    for entry in std::fs::read_dir(registry()).context("no owned artifact registry")? {
        let entry = entry?;
        if entry.path().extension().is_none_or(|e| e != "json") { continue; }
        let record: Ownership = crate::util::read_json(&entry.path())?;
        if record.run == canonical { found = Some((entry.path(), record)); break; }
    }
    let (manifest, record) = found.context("run is not in the Casimir ownership registry; cleanup refused")?;
    let _cleanup_lock = crate::util::RunLock::acquire(&registry())?;
    verify(&record)?;
    let _lock = crate::util::RunLock::acquire_cleanup(&canonical)?;
    verify(&record)?;
    let checkpoint_preview = if checkpoints { Some(crate::checkpoint::cleanup_owner(&canonical, false)?) } else { None };
    let result = serde_json::json!({"schemaVersion":1,"preview":!apply,"run":record.run,"worktree":record.worktree,
        "checkpoints":checkpoint_preview,"checkpointBlobs":if checkpoints {"unreferenced objects selected by manifest"} else {"retained; use --checkpoints to include unreferenced snapshot objects"}});
    if apply {
        if checkpoints { crate::checkpoint::cleanup_owner(&canonical, true)?; }
        if let Some(path) = record.worktree.as_ref().filter(|p| p.exists()) {
            let temp = tempfile::tempdir()?;
            let out = crate::process::capture(std::process::Command::new("git").args(["worktree", "remove", "--force"]).arg(path).current_dir(record.repository.as_ref().unwrap()), b"", std::time::Duration::from_secs(60), Some(&temp.path().join("cleanup")))?;
            if !out.status.success() { bail!("Git refused removal of the owned worktree; run artifacts retained"); }
        }
        // Windows cannot delete an open lock handle. Keep an external cleanup lock while
        // releasing the run lock, then recheck ownership before removing the directory.
        crate::util::atomic_write(&record.run.join(".deleting"), record.id.as_bytes())?;
        drop(_lock);
        std::fs::remove_dir_all(&record.run)?;
        std::fs::remove_file(manifest)?;
    }
    Ok(result)
}
