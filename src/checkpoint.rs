//! Verified content-addressed repository snapshots. No timestamp-based reconstruction.
use crate::{
    model::Harness,
    util::{atomic_write, private_dir, RunLock},
};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    process::Command,
    time::Duration,
};

pub const DEFAULT_LIMIT: u64 = 2 * 1024 * 1024 * 1024;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub path: String,
    pub kind: String,
    pub blob: Option<String>,
    pub mode: u32,
    pub target: Option<String>,
    #[serde(default)]
    pub symlink_directory: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Checkpoint {
    pub schema_version: u32,
    pub repository: PathBuf,
    pub base: String,
    pub base_bundle: String,
    pub index_objects: BTreeMap<String, String>,
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

pub fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn valid_hash(id: &str) -> bool {
    id.len() == 64
        && id
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}
fn root() -> PathBuf {
    crate::util::casimir_home().join("checkpoints")
}
fn blob_path(id: &str) -> Result<PathBuf> {
    if !valid_hash(id) {
        bail!("invalid checkpoint object identifier");
    }
    Ok(root().join("blobs").join(id))
}
fn git(cwd: &Path, args: &[&str]) -> Result<String> {
    let temp = tempfile::tempdir()?;
    let out = crate::process::capture(
        Command::new("git").args(args).current_dir(cwd),
        b"",
        Duration::from_secs(30),
        Some(&temp.path().join("git")),
    )?;
    if !out.status.success() {
        bail!("checkpoint git command failed");
    }
    Ok(String::from_utf8(out.stdout)?.trim().to_string())
}
fn storage_size(path: &Path) -> Result<u64> {
    let mut total = 0u64;
    if path.exists() {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let meta = fs::symlink_metadata(entry.path())?;
            if meta.is_dir() {
                total = total.saturating_add(storage_size(&entry.path())?);
            } else {
                total = total.saturating_add(meta.len());
            }
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
        if n == 0 {
            break;
        }
        size += n as u64;
        if size > limit {
            bail!("checkpoint file exceeds storage limit: {}", path.display());
        }
        digest.update(&buf[..n]);
        temp.write_all(&buf[..n])?;
    }
    let id = format!("{:x}", digest.finalize());
    let destination = dir.join(&id);
    if !destination.exists() {
        if used.saturating_add(size) > limit {
            bail!("checkpoint storage limit exceeded; increase --checkpoint-limit or clean owned artifacts");
        }
        temp.as_file().sync_all()?;
        temp.persist_noclobber(&destination)?;
        *used += size;
    } else {
        verify_blob(&id)?;
    }
    Ok(id)
}
fn verify_blob(id: &str) -> Result<()> {
    let path = blob_path(id)?;
    if !fs::symlink_metadata(&path)
        .with_context(|| format!("missing checkpoint object {id}"))?
        .is_file()
    {
        bail!("checkpoint object is not a regular file");
    }
    let mut file = fs::File::open(path)?;
    let mut digest = Sha256::new();
    std::io::copy(&mut file, &mut digest)?;
    if format!("{:x}", digest.finalize()) != id {
        bail!("corrupt checkpoint object {id}");
    }
    Ok(())
}
fn safe_relative(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path.components().all(|c| matches!(c, Component::Normal(_)))
        && !path
            .components()
            .any(|c| c.as_os_str().to_string_lossy().eq_ignore_ascii_case(".git"))
}
fn collect(
    dir: &Path,
    base: &Path,
    excluded: &[PathBuf],
    entries: &mut Vec<Entry>,
    used: &mut u64,
    limit: u64,
) -> Result<()> {
    let mut children: Vec<_> = fs::read_dir(dir)?.collect::<std::io::Result<_>>()?;
    children.sort_by_key(|e| e.file_name());
    for child in children {
        let path = child.path();
        if child.file_name() == ".casimir-owned-worktree"
            || child
                .file_name()
                .to_string_lossy()
                .eq_ignore_ascii_case(".git")
            || excluded.contains(&path)
        {
            continue;
        }
        let meta = fs::symlink_metadata(&path)?;
        let relative = path.strip_prefix(base)?;
        let name = relative
            .to_str()
            .context("checkpoint paths must be valid Unicode")?
            .replace(std::path::MAIN_SEPARATOR, "/");
        #[cfg(unix)]
        let mode = {
            use std::os::unix::fs::PermissionsExt;
            meta.permissions().mode() & 0o777
        };
        #[cfg(not(unix))]
        let mode = if meta.permissions().readonly() {
            0o444
        } else {
            0o644
        };
        let (kind, blob, target) = if meta.file_type().is_symlink() {
            let target = fs::read_link(&path)?
                .into_os_string()
                .into_string()
                .map_err(|_| anyhow::anyhow!("symlink target is not Unicode"))?;
            ("symlink", None, Some(target))
        } else if meta.is_dir() {
            ("directory", None, None)
        } else if meta.is_file() {
            ("file", Some(put_file(&path, used, limit)?), None)
        } else {
            bail!("unsupported special file in checkpoint: {}", path.display());
        };
        #[cfg(windows)]
        let symlink_directory = {
            use std::os::windows::fs::FileTypeExt;
            meta.file_type().is_symlink_dir()
        };
        #[cfg(not(windows))]
        let symlink_directory = false;
        entries.push(Entry {
            path: name,
            kind: kind.into(),
            blob,
            mode,
            target,
            symlink_directory,
        });
        if meta.is_dir() {
            collect(&path, base, excluded, entries, used, limit)?;
        }
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
    if storage.starts_with(&repository) {
        bail!("checkpoint storage must be outside the agent repository workspace");
    }
    let subdir = fs::canonicalize(options.cwd)?
        .strip_prefix(&repository)?
        .to_path_buf();
    let base = git(options.cwd, &["rev-parse", "HEAD"])?;
    let index_path = PathBuf::from(git(
        options.cwd,
        &["rev-parse", "--path-format=absolute", "--git-path", "index"],
    )?);
    // Split indexes reference files outside the workspace. Refuse until those dependencies
    // are explicitly modeled; silently copying only the index would break the guarantee.
    let shared = git(options.cwd, &["rev-parse", "--shared-index-path"])?;
    if !shared.is_empty() {
        bail!("checkpoint capture requires a non-split Git index");
    }
    let mut used = storage_size(&root())?;
    if used > options.limit {
        bail!("checkpoint storage already exceeds configured limit");
    }
    let base_bundle = capture_base(options.cwd, &base, &mut used, options.limit)?;
    let index_objects = capture_index_objects(options.cwd, &mut used, options.limit)?;
    let index = if index_path.is_file() {
        Some(put_file(&index_path, &mut used, options.limit)?)
    } else {
        None
    };
    let mut entries = Vec::new();
    let excluded = [
        fs::canonicalize(crate::util::casimir_home())?,
        fs::canonicalize(options.run_dir)?,
    ];
    collect(
        &repository,
        &repository,
        &excluded,
        &mut entries,
        &mut used,
        options.limit,
    )?;
    let conversation = options
        .native
        .map(|p| put_file(p, &mut used, options.limit))
        .transpose()?;
    let native_session = conversation
        .as_ref()
        .and_then(|id| blob_path(id).ok())
        .and_then(|p| {
            use std::io::{Seek, SeekFrom};
            let mut file = fs::File::open(&p).ok()?;
            if file.seek(SeekFrom::End(-1)).is_err() {
                return None;
            }
            let mut last = [0];
            file.read_exact(&mut last).ok()?;
            if last[0] != b'\n' {
                return None;
            }
            crate::adapters::parse_file(options.harness, &p).ok()
        });
    let conversation_turns = native_session
        .as_ref()
        .map(|s| crate::model::user_turns(s).len() as u32);
    let has_final_assistant = native_session.as_ref().is_some_and(|s| {
        let last_user = s
            .events
            .iter()
            .rposition(|e| e.kind == crate::model::EventKind::User && !e.sidechain);
        let last_assistant = s
            .events
            .iter()
            .rposition(|e| e.kind == crate::model::EventKind::Assistant && !e.sidechain);
        last_user
            .zip(last_assistant)
            .is_some_and(|(user, assistant)| assistant > user)
    });
    let conversation_complete = if options.expected_conversation_turns == 0 {
        options.native.is_none()
    } else {
        conversation_turns == Some(options.expected_conversation_turns) && has_final_assistant
    };
    let checkpoint = Checkpoint { schema_version: 1, repository, base, base_bundle, index_objects, subdir, index, entries, conversation,
        harness: options.harness, harness_version: options.version, turn: options.turn, conversation_turns: conversation_turns.unwrap_or(0), conversation_complete, pending_prompt: options.prompt.into(),
        configuration_hash: options.configuration_hash.into(), coverage: "Recorded repository workspace, Git index, and native conversation only. External files, services, and process memory are not captured.".into() };
    let bytes = serde_json::to_vec(&checkpoint)?;
    if used.saturating_add(bytes.len() as u64) > options.limit {
        bail!("checkpoint storage limit exceeded");
    }
    let id = hash(&bytes);
    atomic_write(&root().join("manifests").join(format!("{id}.json")), &bytes)?;
    pin_unlocked(options.run_dir, &id)?;
    Ok(id)
}

pub fn load(id: &str) -> Result<Checkpoint> {
    if !valid_hash(id) {
        bail!("invalid checkpoint identifier");
    }
    let bytes = fs::read(root().join("manifests").join(format!("{id}.json"))).context("checkpoint missing; historical sessions may be inspected or rerun, but cannot be forked without a compatible checkpoint")?;
    if hash(&bytes) != id {
        bail!("corrupt checkpoint manifest");
    }
    let checkpoint: Checkpoint = serde_json::from_slice(&bytes)?;
    if checkpoint.schema_version != 1 {
        bail!("unsupported checkpoint schema");
    }
    let mut paths = BTreeSet::new();
    for entry in &checkpoint.entries {
        let path = Path::new(&entry.path);
        if !safe_relative(path) || !paths.insert(path.to_path_buf()) {
            bail!("unsafe or duplicate checkpoint path");
        }
        for ancestor in path
            .ancestors()
            .skip(1)
            .filter(|p| !p.as_os_str().is_empty())
        {
            if !checkpoint
                .entries
                .iter()
                .any(|e| Path::new(&e.path) == ancestor && e.kind == "directory")
            {
                bail!("checkpoint entry has a non-directory ancestor");
            }
        }
        match entry.kind.as_str() {
            "file" => verify_blob(entry.blob.as_deref().context("file blob missing")?)?,
            "symlink" => {
                entry.target.as_ref().context("symlink target missing")?;
            }
            "directory" => {}
            _ => bail!("unknown checkpoint entry kind"),
        }
    }
    for blob in checkpoint
        .index
        .iter()
        .chain(checkpoint.conversation.iter())
        .chain(std::iter::once(&checkpoint.base_bundle))
        .chain(checkpoint.index_objects.values())
    {
        verify_blob(blob)?;
    }
    if !checkpoint.subdir.as_os_str().is_empty() && !safe_relative(&checkpoint.subdir) {
        bail!("invalid checkpoint working directory");
    }
    Ok(checkpoint)
}

/// Only a fresh destination may be restored. The original checkout is never cleaned or reset.
pub fn restore(id: &str, destination: &Path) -> Result<Checkpoint> {
    restore_for_run(id, destination, destination)
}
pub fn restore_for_run(id: &str, destination: &Path, owner: &Path) -> Result<Checkpoint> {
    let checkpoint = {
        let _lock = RunLock::acquire_wait(&root())?;
        let checkpoint = load(id)?;
        pin_unlocked(owner, id)?;
        checkpoint
    };
    if destination.exists() {
        bail!("checkpoint restore requires a fresh worktree destination");
    }
    let repository = ensure_repository(&checkpoint)?;
    let temp = tempfile::tempdir()?;
    let output = crate::process::capture(
        Command::new("git")
            .args(["worktree", "add", "--no-checkout", "--detach"])
            .arg(destination)
            .arg(&checkpoint.base)
            .current_dir(&repository),
        b"",
        Duration::from_secs(60),
        Some(&temp.path().join("worktree")),
    )?;
    if !output.status.success() {
        bail!("creating checkpoint worktree failed");
    }
    for (object, blob) in &checkpoint.index_objects {
        let actual = git(
            &repository,
            &[
                "hash-object",
                "-w",
                blob_path(blob)?
                    .to_str()
                    .context("non-Unicode object path")?,
            ],
        )?;
        if &actual != object {
            bail!("checkpoint Git index object identity mismatch");
        }
    }
    for entry in fs::read_dir(destination)? {
        let entry = entry?;
        if entry.file_name() == ".git" {
            continue;
        }
        if entry.file_type()?.is_dir() {
            fs::remove_dir_all(entry.path())?;
        } else {
            fs::remove_file(entry.path())?;
        }
    }
    for entry in &checkpoint.entries {
        let path = destination.join(&entry.path);
        match entry.kind.as_str() {
            "directory" => fs::create_dir_all(&path)?,
            "file" => {
                fs::copy(blob_path(entry.blob.as_ref().unwrap())?, &path)?;
            }
            "symlink" => {
                #[cfg(unix)]
                std::os::unix::fs::symlink(entry.target.as_ref().unwrap(), &path)?;
                #[cfg(windows)]
                {
                    let target = entry.target.as_ref().unwrap();
                    if entry.symlink_directory {
                        std::os::windows::fs::symlink_dir(target, &path)
                    } else {
                        std::os::windows::fs::symlink_file(target, &path)
                    }
                    .context("restoring symlink requires Windows symlink capability")?;
                }
            }
            _ => unreachable!(),
        }
    }
    // Directory modes are applied last so read-only directories can be populated first.
    #[cfg(unix)]
    for entry in checkpoint
        .entries
        .iter()
        .rev()
        .filter(|e| e.kind != "symlink")
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            destination.join(&entry.path),
            fs::Permissions::from_mode(entry.mode),
        )?;
    }
    #[cfg(windows)]
    for entry in checkpoint.entries.iter().filter(|e| e.kind == "file") {
        let path = destination.join(&entry.path);
        let mut permissions = fs::metadata(&path)?.permissions();
        permissions.set_readonly(entry.mode & 0o222 == 0);
        fs::set_permissions(path, permissions)?;
    }
    if let Some(index) = &checkpoint.index {
        let target = PathBuf::from(git(
            destination,
            &["rev-parse", "--path-format=absolute", "--git-path", "index"],
        )?);
        atomic_write(&target, &fs::read(blob_path(index)?)?)?;
    }
    Ok(checkpoint)
}

pub fn native_copy(checkpoint: &Checkpoint, path: &Path) -> Result<()> {
    let blob = checkpoint
        .conversation
        .as_deref()
        .context("checkpoint has no native conversation boundary")?;
    verify_blob(blob)?;
    atomic_write(path, &fs::read(blob_path(blob)?)?)
}

pub fn require_compatible(checkpoint: &Checkpoint) -> Result<()> {
    if !checkpoint.conversation_complete {
        bail!("checkpoint native conversation does not cover every completed turn; refusing approximate continuation");
    }
    if checkpoint.conversation.is_none() && checkpoint.turn == 1 {
        return Ok(());
    }
    let manifest: serde_json::Value =
        serde_json::from_str(include_str!("../compatibility/harnesses.json"))?;
    let validated = manifest["harnesses"].as_array().unwrap().iter().any(|h| {
        h["id"] == checkpoint.harness.as_str()
            && h["version"].as_str() == checkpoint.harness_version.as_deref()
            && h["checkpointValidated"] == true
            && h["checkpointPlatforms"]
                .as_array()
                .is_some_and(|platforms| platforms.iter().any(|p| p == std::env::consts::OS))
    });
    if validated && crate::doctor::harness_version(checkpoint.harness) != checkpoint.harness_version
    {
        bail!("installed harness version does not match checkpoint compatibility version");
    }
    if !validated {
        bail!("native transcript format/version has not passed checkpoint compatibility validation; checkpoint operations are refused");
    }
    Ok(())
}

fn capture_base(cwd: &Path, base: &str, used: &mut u64, limit: u64) -> Result<String> {
    if !matches!(base.len(), 40 | 64) || !base.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("invalid Git base object identifier");
    }
    let cache = root().join("bases").join(format!("{base}.json"));
    if cache.exists() {
        let id: String = crate::util::read_json(&cache)?;
        verify_blob(&id)?;
        return Ok(id);
    }
    let temporary = tempfile::tempdir_in(root())?;
    let bundle = temporary.path().join("base.bundle");
    git(
        cwd,
        &[
            "bundle",
            "create",
            bundle.to_str().context("non-Unicode bundle path")?,
            "HEAD",
        ],
    )?;
    let id = put_file(&bundle, used, limit)?;
    crate::util::write_json(&cache, &id)?;
    Ok(id)
}

fn capture_index_objects(
    cwd: &Path,
    used: &mut u64,
    limit: u64,
) -> Result<BTreeMap<String, String>> {
    use std::io::{BufRead, BufReader};
    let index = git(cwd, &["ls-files", "--stage", "-z"])?;
    let mut objects = BTreeMap::new();
    let mut missing = BTreeSet::new();
    for record in index.split('\0').filter(|r| !r.is_empty()) {
        let metadata = record
            .split('\t')
            .next()
            .context("invalid Git index record")?;
        let mut fields = metadata.split_whitespace();
        let mode = fields.next().context("index mode missing")?;
        let object = fields.next().context("index object missing")?;
        if mode == "160000" || objects.contains_key(object) {
            continue;
        }
        if !matches!(object.len(), 40 | 64) || !object.bytes().all(|b| b.is_ascii_hexdigit()) {
            bail!("invalid index object identity");
        }
        let cache = root().join("git-objects").join(format!("{object}.json"));
        if cache.exists() {
            let blob: String = crate::util::read_json(&cache)?;
            if blob_path(&blob)?.exists() {
                verify_blob(&blob)?;
                objects.insert(object.into(), blob);
                continue;
            }
        }
        missing.insert(object.to_string());
    }
    if missing.is_empty() {
        return Ok(objects);
    }
    let temp = tempfile::tempdir_in(root())?;
    let spool = temp.path().join("objects");
    let input = format!(
        "{}\n",
        missing.iter().cloned().collect::<Vec<_>>().join("\n")
    );
    let output = crate::process::Process::spawn(
        Command::new("git")
            .args(["cat-file", "--batch"])
            .current_dir(cwd),
        input.as_bytes(),
        Duration::from_secs(60),
        Some(&spool),
    )?
    .finish()?;
    if !output.status.success() {
        bail!("reading Git index objects failed");
    }
    let mut reader = BufReader::new(fs::File::open(spool.join("stdout.log"))?);
    for expected in missing {
        let mut header = String::new();
        reader.by_ref().take(1024).read_line(&mut header)?;
        let fields: Vec<_> = header.split_whitespace().collect();
        if fields.len() != 3 || fields[0] != expected || fields[1] != "blob" {
            bail!("Git index references a missing or invalid object");
        }
        let size: u64 = fields[2].parse()?;
        if size > limit {
            bail!("Git index object exceeds checkpoint storage limit");
        }
        let mut object = tempfile::NamedTempFile::new_in(temp.path())?;
        if std::io::copy(&mut reader.by_ref().take(size), object.as_file_mut())? != size {
            bail!("truncated Git index object");
        }
        let mut separator = [0];
        reader.read_exact(&mut separator)?;
        if separator[0] != b'\n' {
            bail!("invalid Git object boundary");
        }
        let blob = put_file(object.path(), used, limit)?;
        crate::util::write_json(
            &root().join("git-objects").join(format!("{expected}.json")),
            &blob,
        )?;
        objects.insert(expected, blob);
    }
    Ok(objects)
}

pub fn repository_path(checkpoint: &Checkpoint) -> PathBuf {
    crate::util::casimir_home()
        .join("repositories")
        .join(&checkpoint.base_bundle)
}
fn ensure_repository(checkpoint: &Checkpoint) -> Result<PathBuf> {
    let _lock = RunLock::acquire_wait(&root())?;
    let destination = repository_path(checkpoint);
    if destination.exists() {
        if fs::read_to_string(destination.join("casimir-bundle"))? != checkpoint.base_bundle {
            bail!("Git checkpoint cache ownership mismatch");
        }
    } else {
        let parent = destination.parent().unwrap();
        private_dir(parent)?;
        let temporary = tempfile::tempdir_in(parent)?;
        let repository = temporary.path().join("repository.git");
        let output = crate::process::capture(
            Command::new("git")
                .args(["clone", "--bare", "--quiet"])
                .arg(blob_path(&checkpoint.base_bundle)?)
                .arg(&repository),
            b"",
            Duration::from_secs(60),
            Some(&temporary.path().join("clone")),
        )?;
        if !output.status.success() {
            bail!("checkpoint Git bundle cannot be restored");
        }
        atomic_write(
            &repository.join("casimir-bundle"),
            checkpoint.base_bundle.as_bytes(),
        )?;
        fs::rename(repository, &destination)?;
    }
    git(
        &destination,
        &["cat-file", "-e", &format!("{}^{{commit}}", checkpoint.base)],
    )?;
    Ok(destination)
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct References {
    schema_version: u32,
    owner: PathBuf,
    checkpoints: BTreeSet<String>,
}
fn owner_path(path: &Path) -> Result<PathBuf> {
    if let Ok(path) = fs::canonicalize(path) {
        return Ok(path);
    }
    if let Some((parent, name)) = path.parent().zip(path.file_name()) {
        if let Ok(parent) = fs::canonicalize(parent) {
            return Ok(parent.join(name));
        }
    }
    Ok(if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    })
}
fn reference_path(owner: &Path) -> Result<PathBuf> {
    Ok(root().join("references").join(format!(
        "{}.json",
        hash(&serde_json::to_vec(&owner_path(owner)?)?)
    )))
}
fn pin_unlocked(owner: &Path, id: &str) -> Result<()> {
    let path = reference_path(owner)?;
    let mut references: References = if path.exists() {
        crate::util::read_json(&path)?
    } else {
        References {
            schema_version: 1,
            owner: owner_path(owner)?,
            checkpoints: BTreeSet::new(),
        }
    };
    references.checkpoints.insert(id.into());
    crate::util::write_json(&path, &references)
}
fn objects(checkpoint: &Checkpoint) -> BTreeSet<String> {
    checkpoint
        .entries
        .iter()
        .filter_map(|e| e.blob.clone())
        .chain(checkpoint.index.iter().cloned())
        .chain(checkpoint.conversation.iter().cloned())
        .chain(std::iter::once(checkpoint.base_bundle.clone()))
        .chain(checkpoint.index_objects.values().cloned())
        .collect()
}

/// Optional checkpoint cleanup honors persistent references from other owned runs and restores.
/// Captures publish their references under the same lock, before a collector can observe them.
pub fn cleanup_owner(owner: &Path, apply: bool) -> Result<serde_json::Value> {
    let _lock = RunLock::acquire_wait(&root())?;
    let target_path = reference_path(owner)?;
    if !target_path.exists() {
        return Ok(serde_json::json!({"manifests":0,"objects":0,"bytes":0}));
    }
    let target: References = crate::util::read_json(&target_path)?;
    let deletion_path = target_path.with_extension("deletion");
    let pending: Option<(Vec<String>, Vec<String>)> = if deletion_path.exists() {
        Some(crate::util::read_json(&deletion_path)?)
    } else {
        None
    };
    let mut retained = BTreeSet::new();
    for entry in fs::read_dir(root().join("references"))? {
        let entry = entry?;
        if entry.path() == target_path || entry.path().extension().is_none_or(|e| e != "json") {
            continue;
        }
        if !entry.file_type()?.is_file() {
            bail!("unexpected reference storage entry");
        }
        let references: References = crate::util::read_json(&entry.path())?;
        if references.schema_version != 1 {
            bail!("unsupported checkpoint references");
        }
        retained.extend(references.checkpoints);
    }
    let removed: Vec<_> = target.checkpoints.difference(&retained).cloned().collect();
    let mut retained_objects = BTreeSet::new();
    for id in &retained {
        retained_objects.extend(objects(&load(id)?));
    }
    let mut removed_objects = BTreeSet::new();
    for id in &removed {
        if root().join("manifests").join(format!("{id}.json")).exists() {
            removed_objects.extend(objects(&load(id)?));
        } else if !pending.as_ref().is_some_and(|(ids, _)| ids.contains(id)) {
            bail!("missing checkpoint manifest during cleanup");
        }
    }
    if let Some((_, objects)) = &pending {
        removed_objects.extend(objects.iter().cloned());
    }
    let removed_objects: Vec<_> = removed_objects
        .difference(&retained_objects)
        .cloned()
        .collect();
    let mut bytes = 0;
    for id in &removed_objects {
        if let Ok(metadata) = fs::metadata(blob_path(id)?) {
            bytes += metadata.len();
        }
    }
    let preview = serde_json::json!({"schemaVersion":1,"preview":!apply,"manifests":removed.len(),"objects":removed_objects.len(),"bytes":bytes,
        "coverage":"References from owned runs and Casimir restores are retained. Unregistered manual copies are not checkpoint backups."});
    if apply {
        crate::util::write_json(&deletion_path, &(&removed, &removed_objects))?;
        for id in &removed {
            let path = root().join("manifests").join(format!("{id}.json"));
            if path.exists() {
                fs::remove_file(path)?;
            }
        }
        for id in &removed_objects {
            let path = blob_path(id)?;
            if path.exists() {
                fs::remove_file(path)?;
            }
        }
        for cache in ["bases", "git-objects"]
            .iter()
            .map(|name| root().join(name))
            .filter(|path| path.exists())
        {
            for entry in fs::read_dir(cache)? {
                let entry = entry?;
                if entry.path().extension().is_none_or(|e| e != "json") {
                    continue;
                }
                let bundle: String = crate::util::read_json(&entry.path())?;
                if removed_objects.contains(&bundle) {
                    fs::remove_file(entry.path())?;
                }
            }
        }
        fs::remove_file(target_path)?;
        fs::remove_file(deletion_path)?;
    }
    Ok(preview)
}
