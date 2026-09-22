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
    /// Where this diff came from (captured worktree, commit range, working tree heuristic, run dir).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// Added and removed lines of one file in a unified diff.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileChange {
    pub added: Vec<String>,
    pub removed: Vec<String>,
}

/// Parse a unified diff into per-file added/removed lines (hunk headers and context ignored).
pub fn parse_patch(patch: &str) -> std::collections::BTreeMap<String, FileChange> {
    let mut out: std::collections::BTreeMap<String, FileChange> = Default::default();
    let mut current: Option<String> = None;
    for line in patch.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            // "a/path b/path" — take the b/ side
            let b = rest.rsplit(" b/").next().unwrap_or(rest).to_string();
            current = Some(b.clone());
            out.entry(b).or_default();
            continue;
        }
        if line.starts_with("+++ ") || line.starts_with("--- ") || line.starts_with("@@") || line.starts_with("index ") || line.starts_with("new file") || line.starts_with("deleted file") || line.starts_with("similarity") || line.starts_with("rename ") || line.starts_with("old mode") || line.starts_with("new mode") || line.starts_with("Binary files") {
            continue;
        }
        let Some(cur) = current.as_ref() else { continue };
        if let Some(a) = line.strip_prefix('+') {
            out.get_mut(cur).unwrap().added.push(a.to_string());
        } else if let Some(r) = line.strip_prefix('-') {
            out.get_mut(cur).unwrap().removed.push(r.to_string());
        }
    }
    out
}

fn last_commit_before(ts: &str, refname: &str, dir: &Path) -> Option<String> {
    let out = Command::new("git").args(["rev-list", "-1", &format!("--before={ts}"), refname]).current_dir(dir).output().ok()?;
    let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if out.status.success() && !sha.is_empty() {
        Some(sha)
    } else {
        None
    }
}

/// Best-effort reconstruction of the workspace changes an original session produced, from git history:
/// the commits between the base commit and the last commit before the session ended, or, when there
/// are none, the working tree against the base commit (a heuristic, and labelled as such).
pub fn reconstruct_original_diff(session: &Session) -> Option<Diff> {
    let dir = Path::new(session.cwd.as_deref()?);
    if !is_git_repo(dir) {
        return None;
    }
    let (base, _) = base_commit(session, dir).ok()?;
    let end = session.ended_at.as_deref().and_then(|ts| {
        let mut refs: Vec<&str> = Vec::new();
        if let Some(b) = session.git_branch.as_deref() {
            refs.push(b);
        }
        refs.push("HEAD");
        refs.into_iter().find_map(|r| last_commit_before(ts, r, dir))
    });
    match end {
        Some(end) if end != base => {
            let range = format!("{base}..{end}");
            let patch = git_lenient(&["diff", "--no-color", &range], dir);
            let stat = git_lenient(&["diff", "--stat", "--no-color", &range], dir);
            let files = git_lenient(&["diff", "--name-status", &range], dir)
                .lines()
                .filter_map(|l| {
                    let (st, p) = l.split_once('\t')?;
                    Some(ChangedFile { status: st.trim().to_string(), path: p.trim().to_string() })
                })
                .collect();
            Some(Diff { files, stat: stat.trim().to_string(), patch, source: Some(format!("commits {}..{} in {}", &base[..base.len().min(8)], &end[..end.len().min(8)], dir.display())) })
        }
        _ => {
            let mut d = capture_diff_against(dir, &base);
            d.source = Some(format!("working tree of {} vs base {} (heuristic: no commits recorded after the session)", dir.display(), &base[..base.len().min(8)]));
            Some(d)
        }
    }
}

/// Like `capture_diff`, but relative to an arbitrary commit instead of HEAD.
pub fn capture_diff_against(dir: &Path, commit: &str) -> Diff {
    if !is_git_repo(dir) {
        return Diff::default();
    }
    let status = git_lenient(&["status", "--porcelain", "--untracked-files=all"], dir);
    let mut files: Vec<ChangedFile> = status
        .lines()
        .filter(|l| l.len() > 3)
        .map(|l| ChangedFile { status: l[..2].trim().to_string(), path: l[3..].trim().to_string() })
        .collect();
    for l in git_lenient(&["diff", "--name-status", commit], dir).lines() {
        if let Some((st, p)) = l.split_once('\t') {
            if !files.iter().any(|f| f.path == p.trim()) {
                files.push(ChangedFile { status: st.trim().to_string(), path: p.trim().to_string() });
            }
        }
    }
    let mut patch = git_lenient(&["diff", commit, "--no-color"], dir);
    let untracked: Vec<String> = git_lenient(&["ls-files", "--others", "--exclude-standard"], dir).lines().map(String::from).collect();
    for f in &untracked {
        patch.push_str(&git_lenient(&["diff", "--no-index", "--no-color", "--", "/dev/null", f], dir));
    }
    let mut stat = git_lenient(&["diff", commit, "--stat", "--no-color"], dir);
    for f in &untracked {
        stat.push_str(&format!(" {f} | (new file)\n"));
    }
    Diff { files, stat: stat.trim().to_string(), patch, source: None }
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
    Diff { files, stat: stat.trim().to_string(), patch, source: Some(format!("captured from {}", dir.display())) }
}
