//! Git helpers: base-commit inference, worktrees, and diff capture.
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::model::Session;

fn git(args: &[&str], cwd: &Path) -> Result<String> {
    let out = Command::new("git").args(args).current_dir(cwd).output()?;
    if !out.status.success() {
        bail!("git {} failed: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn git_lenient(args: &[&str], cwd: &Path) -> String {
    Command::new("git").args(args).current_dir(cwd).output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default()
}

pub fn is_git_repo(dir: &Path) -> bool {
    dir.exists() && Command::new("git").args(["rev-parse", "--is-inside-work-tree"]).current_dir(dir).output().is_ok_and(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == "true")
}

pub fn repo_root(dir: &Path) -> Result<PathBuf> {
    Ok(PathBuf::from(git(&["rev-parse", "--show-toplevel"], dir)?.trim()))
}

pub fn head_commit(dir: &Path) -> Result<String> {
    Ok(git(&["rev-parse", "HEAD"], dir)?.trim().to_string())
}

pub fn commit_exists(sha: &str, dir: &Path) -> bool {
    Command::new("git").args(["cat-file", "-e", &format!("{sha}^{{commit}}")]).current_dir(dir).output().is_ok_and(|o| o.status.success())
}

/// Best-effort commit the original session started from: the one recorded by the harness,
/// else the last commit on the recorded branch (or HEAD) before the session started.
pub fn base_commit(session: &Session, dir: &Path) -> Result<(String, String)> {
    if let Some(c) = &session.git_commit {
        if commit_exists(c, dir) {
            return Ok((c.clone(), "recorded in session log".into()));
        }
    }
    let mut refs: Vec<&str> = Vec::new();
    if let Some(b) = session.git_branch.as_deref() {
        refs.push(b);
    }
    refs.push("HEAD");
    if let Some(start) = &session.started_at {
        for r in refs {
            let out = Command::new("git").args(["rev-list", "-1", &format!("--before={start}"), r]).current_dir(dir).output()?;
            let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if out.status.success() && !sha.is_empty() {
                return Ok((sha, format!("last commit on {r} before session start")));
            }
        }
    }
    Ok((head_commit(dir)?, "current HEAD (no better information)".into()))
}

/// Create a detached worktree at `commit` under `dest`.
pub fn create_worktree(repo: &Path, commit: &str, dest: &Path) -> Result<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    git(&["worktree", "add", "--detach", &dest.display().to_string(), commit], repo)?;
    Ok(())
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct ChangedFile {
    pub status: String,
    pub path: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Diff {
    pub files: Vec<ChangedFile>,
    pub stat: String,
    #[serde(default)]
    pub patch: String,
}

/// Changed files and a unified patch, including untracked files.
pub fn capture_diff(dir: &Path) -> Diff {
    if !is_git_repo(dir) {
        return Diff::default();
    }
    let status = git_lenient(&["status", "--porcelain", "--untracked-files=all"], dir);
    let files = status
        .lines()
        .filter(|l| l.len() > 3)
        .map(|l| ChangedFile { status: l[..2].trim().to_string(), path: l[3..].trim().to_string() })
        .collect();
    let mut patch = git_lenient(&["diff", "HEAD", "--no-color"], dir);
    let untracked: Vec<String> = git_lenient(&["ls-files", "--others", "--exclude-standard"], dir).lines().map(String::from).collect();
    for f in &untracked {
        patch.push_str(&git_lenient(&["diff", "--no-index", "--no-color", "--", "/dev/null", f], dir));
    }
    let mut stat = git_lenient(&["diff", "HEAD", "--stat", "--no-color"], dir);
    for f in &untracked {
        stat.push_str(&format!(" {f} | (new file)\n"));
    }
    Diff { files, stat: stat.trim().to_string(), patch }
}
