# Checkpoint guarantee and limitations

Before user turns, Casimir records the repository HEAD and its Git bundle, raw Git index and
staged blob objects, workspace contents,
pending prompt, configuration fingerprint, and available native conversation boundary.
Content-addressed objects live in `$CASIMIR_HOME/checkpoints`, outside the agent worktree.
The default content-addressed storage limit is 2 GiB; configure `--checkpoint-limit BYTES`.
Temporary capture files and managed bare Git restore caches need additional free space.

Snapshots include tracked, untracked, ignored and binary files, directories, executable modes,
and symlink targets. Symlinks are recorded without following external targets. Git internals
and Casimir storage are excluded. Split Git indexes and unsupported special files are refused
rather than incompletely captured. Paths must be Unicode. Empty directories are preserved.
Native Windows symlink restoration requires the relevant OS capability.

Restoration checks manifest and object hashes before creating a fresh worktree. It never resets
or cleans the source checkout. The stored Git bundle and staged objects allow restoration
even after the original checkout is deleted. Missing/corrupt objects fail before model execution. Native
conversation completeness and the installed harness version are checked separately. An
unvalidated transcript version or missing completed turns prevents continuation.

The guarantee covers the recorded repository workspace, index, and conversation. It does not
capture external files, external services, network state, installed dependencies outside the
repository, running processes, or process memory. Submodule Git metadata and repositories outside the
recorded root are also outside this guarantee. Retrying cannot undo a previously sent email,
network request, or external file write. A worktree is not OS isolation.

A capture can fail on storage limits, permissions, concurrent file changes, or disk exhaustion.
Keep raw evidence and the recovery journal until the interruption is understood. Never edit a
manifest or fabricate a historical checkpoint to make a fork proceed.

Preview checkpoint reclamation with `casimir cleanup RUN --checkpoints`; add `--apply` to
remove the selected run, its owned worktree, and checkpoint objects unreferenced by other
registered runs/restores. Ordinary cleanup retains checkpoints; `cleanup RUN --checkpoints` can reclaim them later
even after the run directory has been removed. Manually copying a session
JSON file does not register a checkpoint reference and is not a complete backup. Managed
bare Git restore caches are retained; the checkpoint purge does not remove them.
