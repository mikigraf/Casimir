# Checkpoint guarantee and limitations

Before user turns, Casimir records the repository HEAD, raw Git index, workspace contents,
pending prompt, configuration fingerprint, and available native conversation boundary.
Content-addressed objects live in `$CASIMIR_HOME/checkpoints`, outside the agent worktree.
Default total storage limit is 2 GiB; configure `--checkpoint-limit BYTES`.

Snapshots include tracked, untracked, ignored and binary files, directories, executable modes,
and symlink targets. Symlinks are recorded without following external targets. Git internals
and Casimir storage are excluded. Split Git indexes and unsupported special files are refused
rather than incompletely captured. Paths must be Unicode. Empty directories are preserved.
Native Windows symlink restoration requires the relevant OS capability.

Restoration checks manifest and object hashes before creating a fresh worktree. It never resets
or cleans the source checkout. Missing/corrupt objects fail before model execution. Native
conversation completeness and the installed harness version are checked separately. An
unvalidated transcript version or missing completed turns prevents continuation.

The guarantee covers the recorded repository workspace, index, and conversation. It does not
capture external files, external services, network state, installed dependencies outside the
repository, running processes, or process memory. Retrying cannot undo a previously sent email,
network request, or external file write. A worktree is not OS isolation.

A capture can fail on storage limits, permissions, concurrent file changes, or disk exhaustion.
Keep raw evidence and the recovery journal until the interruption is understood. Never edit a
manifest or fabricate a historical checkpoint to make a fork proceed.
