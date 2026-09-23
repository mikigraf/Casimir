//! Verified content-addressed repository snapshots. No timestamp-based reconstruction.
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, fs, io::{Read, Write}, path::{Component, Path, PathBuf}, process::Command, time::Duration};
use crate::{model::Harness, util::{atomic_write, private_dir, RunLock}};

pub const DEFAULT_LIMIT: u64 = 2 * 1024 * 1024 * 1024;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub path: String,
    pub kind: String,
    pub blob: Option<String>,
    pub mode: u32,
    pub target: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Checkpoint {
    pub schema_version: u32,
    pub repository: PathBuf,
    pub base: String,
    pub subdir: PathBuf,
    pub index: Option<String>,
    pub entries: Vec<Entry>,
    pub conversation: Option<String>,
    pub harness: Harness,
    pub harness_version: Option<String>,
    pub turn: u32,
    pub conversation_turns: u32,
    pub conversation_complete: bool,
    pub pending_prompt: String,
    pub configuration_hash: String,
    pub coverage: String,
}

pub fn hash(bytes: &[u8]) -> String { format!("{:x}", Sha256::digest(bytes)) }
fn valid_hash(id: &str) -> bool { id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) }
fn root() -> PathBuf { crate::util::casimir_home().join("checkpoints") }
fn blob_path(id: &str) -> Result<PathBuf> {
    if !valid_hash(id) { bail!("invalid checkpoint object identifier"); }
    Ok(root().join("blobs").join(id))
}
fn git(cwd: &Path, args: &[&str]) -> Result<String> {
    let temp = tempfile::tempdir()?;
    let out = crate::process::capture(Command::new("git").args(args).current_dir(cwd), b"", Duration::from_secs(30), Some(&temp.path().join("git")))?;
    if !out.status.success() { bail!("checkpoint git command failed"); }
    Ok(String::from_utf8(out.stdout)?.trim().to_string())
}
fn storage_size(path: &Path) -> Result<u64> {
    let mut total = 0u64;
    if path.exists() {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let meta = fs::symlink_metadata(entry.path())?;
            if meta.is_dir() { total = total.saturating_add(storage_size(&entry.path())?); }
            else { total = total.saturating_add(meta.len()); }
        }
    }
    Ok(total)
}
fn put_file(path: &Path, used: &mut u64, limit: u64) -> Result<String> {
    let dir = root().join("blobs");
    private_dir(&dir)?;
    let mut temp = tempfile::NamedTempFile::new_in(&dir)?;
    let mut source = fs::File::open(path)?;
    let mut digest = Sha256::new();
    let mut size = 0;
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = source.read(&mut buf)?;
        if n == 0 { break; }
        size += n as u64;
        if size > limit { bail!("checkpoint file exceeds storage limit: {}", path.display()); }
        digest.update(&buf[..n]);
        temp.write_all(&buf[..n])?;
    }
    let id = format!("{:x}", digest.finalize());
    let destination = dir.join(&id);
    if !destination.exists() {
        if used.saturating_add(size) > limit { bail!("checkpoint storage limit exceeded; increase --checkpoint-limit or clean owned artifacts"); }
        temp.as_file().sync_all()?;
        temp.persist_noclobber(&destination)?;
        *used += size;
    } else { verify_blob(&id)?; }
    Ok(id)
}
fn verify_blob(id: &str) -> Result<()> {
    let path = blob_path(id)?;
    if !fs::symlink_metadata(&path).with_context(|| format!("missing checkpoint object {id}"))?.is_file() { bail!("checkpoint object is not a regular file"); }
    let mut file = fs::File::open(path)?;
    let mut digest = Sha256::new();
    std::io::copy(&mut file, &mut digest)?;
    if format!("{:x}", digest.finalize()) != id { bail!("corrupt checkpoint object {id}"); }
    Ok(())
}
fn safe_relative(path: &Path) -> bool {
    !path.as_os_str().is_empty() && path.components().all(|c| matches!(c, Component::Normal(_)))
        && !path.components().any(|c| c.as_os_str().to_string_lossy().eq_ignore_ascii_case(".git"))
}
fn collect(dir: &Path, base: &Path, excluded: &[PathBuf], entries: &mut Vec<Entry>, used: &mut u64, limit: u64) -> Result<()> {
    let mut children: Vec<_> = fs::read_dir(dir)?.collect::<std::io::Result<_>>()?;
    children.sort_by_key(|e| e.file_name());
    for child in children {
        let path = child.path();
        if child.file_name() == ".casimir-owned-worktree" || child.file_name().to_string_lossy().eq_ignore_ascii_case(".git") || excluded.contains(&path) { continue; }
        let meta = fs::symlink_metadata(&path)?;
        let relative = path.strip_prefix(base)?;
        let name = relative.to_str().context("checkpoint paths must be valid Unicode")?.replace(std::path::MAIN_SEPARATOR, "/");
        #[cfg(unix)] let mode = { use std::os::unix::fs::PermissionsExt; meta.permissions().mode() & 0o777 };
        #[cfg(not(unix))] let mode = if meta.permissions().readonly() { 0o444 } else { 0o644 };
        let (kind, blob, target) = if meta.file_type().is_symlink() {
            let target = fs::read_link(&path)?.into_os_string().into_string().map_err(|_| anyhow::anyhow!("symlink target is not Unicode"))?;
            ("symlink", None, Some(target))
        } else if meta.is_dir() { ("directory", None, None) }
        else if meta.is_file() { ("file", Some(put_file(&path, used, limit)?), None) }
        else { bail!("unsupported special file in checkpoint: {}", path.display()); };
        entries.push(Entry { path: name, kind: kind.into(), blob, mode, target });
        if meta.is_dir() { collect(&path, base, excluded, entries, used, limit)?; }
    }
    Ok(())
}

pub struct Capture<'a> {
    pub cwd: &'a Path,
    pub run_dir: &'a Path,
    pub native: Option<&'a Path>,
    pub harness: Harness,
    pub version: Option<String>,
    pub turn: u32,
    pub expected_conversation_turns: u32,
    pub prompt: &'a str,
    pub configuration_hash: &'a str,
    pub limit: u64,
}
pub fn capture(options: Capture<'_>) -> Result<String> {
    let _lock = RunLock::acquire_wait(&root())?;
    let repository = fs::canonicalize(git(options.cwd, &["rev-parse", "--show-toplevel"])?)?;
    let storage = fs::canonicalize(root())?;
    if storage == repository { bail!("checkpoint storage cannot be the repository root"); }
    let subdir = fs::canonicalize(options.cwd)?.strip_prefix(&repository)?.to_path_buf();
    let base = git(options.cwd, &["rev-parse", "HEAD"])?;
    let index_path = PathBuf::from(git(options.cwd, &["rev-parse", "--path-format=absolute", "--git-path", "index"])?);
    // Split indexes reference files outside the workspace. Refuse until those dependencies
    // are explicitly modeled; silently copying only the index would break the guarantee.
    let shared = git(options.cwd, &["rev-parse", "--shared-index-path"])?;
    if !shared.is_empty() { bail!("checkpoint capture requires a non-split Git index"); }
    let mut used = storage_size(&root())?;
    let index = if index_path.is_file() { Some(put_file(&index_path, &mut used, options.limit)?) } else { None };
    let mut entries = Vec::new();
    let excluded = [fs::canonicalize(crate::util::casimir_home())?, fs::canonicalize(options.run_dir)?];
    collect(&repository, &repository, &excluded, &mut entries, &mut used, options.limit)?;
    let conversation_turns = options.native.and_then(|p| crate::adapters::parse_file(options.harness, p).ok()).map(|s| crate::model::user_turns(&s).len() as u32);
    let conversation_complete = if options.expected_conversation_turns == 0 { options.native.is_none() } else { conversation_turns == Some(options.expected_conversation_turns) };
    let conversation = options.native.map(|p| put_file(p, &mut used, options.limit)).transpose()?;
    let checkpoint = Checkpoint { schema_version: 1, repository, base, subdir, index, entries, conversation,
        harness: options.harness, harness_version: options.version, turn: options.turn, conversation_turns: conversation_turns.unwrap_or(0), conversation_complete, pending_prompt: options.prompt.into(),
        configuration_hash: options.configuration_hash.into(), coverage: "Recorded repository workspace, Git index, and native conversation only. External files, services, and process memory are not captured.".into() };
    let bytes = serde_json::to_vec(&checkpoint)?;
    if used.saturating_add(bytes.len() as u64) > options.limit { bail!("checkpoint storage limit exceeded"); }
    let id = hash(&bytes);
    atomic_write(&root().join("manifests").join(format!("{id}.json")), &bytes)?;
    Ok(id)
}

pub fn load(id: &str) -> Result<Checkpoint> {
    if !valid_hash(id) { bail!("invalid checkpoint identifier"); }
    let bytes = fs::read(root().join("manifests").join(format!("{id}.json"))).context("checkpoint missing; historical sessions may be inspected or rerun, but cannot be forked without a compatible checkpoint")?;
    if hash(&bytes) != id { bail!("corrupt checkpoint manifest"); }
    let checkpoint: Checkpoint = serde_json::from_slice(&bytes)?;
    if checkpoint.schema_version != 1 { bail!("unsupported checkpoint schema"); }
    let mut paths = BTreeSet::new();
    for entry in &checkpoint.entries {
        let path = Path::new(&entry.path);
        if !safe_relative(path) || !paths.insert(path.to_path_buf()) { bail!("unsafe or duplicate checkpoint path"); }
        for ancestor in path.ancestors().skip(1).filter(|p| !p.as_os_str().is_empty()) {
            if !checkpoint.entries.iter().any(|e| Path::new(&e.path) == ancestor && e.kind == "directory") { bail!("checkpoint entry has a non-directory ancestor"); }
        }
        match entry.kind.as_str() {
            "file" => verify_blob(entry.blob.as_deref().context("file blob missing")?)?,
            "symlink" => { entry.target.as_ref().context("symlink target missing")?; },
            "directory" => {},
            _ => bail!("unknown checkpoint entry kind"),
        }
    }
    for blob in checkpoint.index.iter().chain(checkpoint.conversation.iter()) { verify_blob(blob)?; }
    if !checkpoint.subdir.as_os_str().is_empty() && !safe_relative(&checkpoint.subdir) { bail!("invalid checkpoint working directory"); }
    Ok(checkpoint)
}

/// Only a fresh destination may be restored. The original checkout is never cleaned or reset.
pub fn restore(id: &str, destination: &Path) -> Result<Checkpoint> {
    let checkpoint = load(id)?;
    if destination.exists() { bail!("checkpoint restore requires a fresh worktree destination"); }
    crate::workspace::create_worktree(&checkpoint.repository, &checkpoint.base, destination)?;
    for entry in fs::read_dir(destination)? {
        let entry = entry?;
        if entry.file_name() == ".git" { continue; }
        if entry.file_type()?.is_dir() { fs::remove_dir_all(entry.path())?; } else { fs::remove_file(entry.path())?; }
    }
    for entry in &checkpoint.entries {
        let path = destination.join(&entry.path);
        match entry.kind.as_str() {
            "directory" => fs::create_dir_all(&path)?,
            "file" => { fs::copy(blob_path(entry.blob.as_ref().unwrap())?, &path)?; },
            "symlink" => {
                #[cfg(unix)] std::os::unix::fs::symlink(entry.target.as_ref().unwrap(), &path)?;
                #[cfg(windows)] std::os::windows::fs::symlink_file(entry.target.as_ref().unwrap(), &path).context("restoring symlink requires Windows symlink capability")?;
            },
            _ => unreachable!(),
        }
    }
    // Directory modes are applied last so read-only directories can be populated first.
    #[cfg(unix)] for entry in checkpoint.entries.iter().rev().filter(|e| e.kind != "symlink") {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(destination.join(&entry.path), fs::Permissions::from_mode(entry.mode))?;
    }
    if let Some(index) = &checkpoint.index {
        let target = PathBuf::from(git(destination, &["rev-parse", "--path-format=absolute", "--git-path", "index"])?);
        atomic_write(&target, &fs::read(blob_path(index)?)?)?;
    }
    Ok(checkpoint)
}

pub fn native_copy(checkpoint: &Checkpoint, path: &Path) -> Result<()> {
    let blob = checkpoint.conversation.as_deref().context("checkpoint has no native conversation boundary")?;
    verify_blob(blob)?;
    atomic_write(path, &fs::read(blob_path(blob)?)?)
}

pub fn require_compatible(checkpoint: &Checkpoint) -> Result<()> {
    if !checkpoint.conversation_complete { bail!("checkpoint native conversation does not cover every completed turn; refusing approximate continuation"); }
    if checkpoint.conversation.is_none() && checkpoint.turn == 1 { return Ok(()); }
    let manifest: serde_json::Value = serde_json::from_str(include_str!("../compatibility/harnesses.json"))?;
    let validated = manifest["harnesses"].as_array().unwrap().iter().any(|h|
        h["id"] == checkpoint.harness.as_str() && h["version"].as_str() == checkpoint.harness_version.as_deref() && h["checkpointValidated"] == true);
    if validated && crate::doctor::harness_version(checkpoint.harness) != checkpoint.harness_version { bail!("installed harness version does not match checkpoint compatibility version"); }
    if !validated { bail!("native transcript format/version has not passed checkpoint compatibility validation; checkpoint operations are refused"); }
    Ok(())
}
