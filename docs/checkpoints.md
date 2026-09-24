# Checkpoints

Forking a session means going back to how things were at a given turn. To do that reliably,
Casimir saves a checkpoint before each user turn of a run. This page covers what a checkpoint
contains, what it doesn't, and how to manage the storage.

## What gets saved

Before each user turn, Casimir records:

- the repository `HEAD`, as a Git bundle
- the raw Git index and the blobs of any staged files
- the contents of the workspace
- the pending prompt
- a fingerprint of the configuration
- the agent's native conversation up to that point, where available

The workspace snapshot covers tracked, untracked, ignored and binary files, directories,
executable bits and symlinks. Symlinks are saved as links and their targets aren't followed.
Empty directories are kept. Git internals and Casimir's own storage are left out.

Casimir refuses to take a checkpoint rather than take an incomplete one. That happens with
split Git indexes, unsupported special files and non-Unicode paths. On Windows, restoring
symlinks needs the relevant OS permission.

## Storage

Checkpoints are content-addressed and stored in `$CASIMIR_HOME/checkpoints`, outside the agent's
worktree. The default limit is 2 GiB, which you can change with `--checkpoint-limit BYTES`.
Temporary capture files and the Git caches used for restores need extra free space on top of
that.

## Restoring

Before creating a worktree from a checkpoint, Casimir checks the manifest and every object hash.
Missing or corrupt objects stop the fork before any model is called. The source checkout is
never reset or cleaned. Because the Git bundle and staged objects are stored with the
checkpoint, you can still restore after the original checkout has been deleted.

The conversation is checked separately: the transcript must be complete, and the installed
agent version must be one that has been validated in the
[compatibility manifest](../compatibility/harnesses.json). If either check fails, the fork
doesn't start.

## What a checkpoint doesn't cover

A checkpoint covers the repository workspace, the Git index and the conversation. It doesn't
cover:

- files outside the repository
- external services and network state
- dependencies installed outside the repository
- running processes and their memory
- submodule Git metadata, or other repositories outside the recorded root

Retrying a turn can't take back an email that was sent, a network request that was made or a
file that was written outside the repository. A worktree is also not a sandbox.

## When capture fails

A checkpoint can fail because of the storage limit, permissions, files changing mid-capture or
a full disk. If that happens, keep the raw logs and the recovery journal until you understand
what went wrong. Don't edit a manifest or hand-craft a checkpoint to force a fork through.

## Cleaning up

```sh
casimir cleanup RUN --checkpoints          # preview
casimir cleanup RUN --checkpoints --apply  # delete
```

This removes the run, its worktree, and any checkpoint objects that no other run or restore
still uses. A plain `cleanup` without `--checkpoints` leaves checkpoints alone, and you can
reclaim them later even after the run directory is gone.

A few things to know:

- Copying a session JSON file by hand doesn't register a checkpoint reference, so it isn't a
  full backup.
- The Git caches used for restores are kept. Purging checkpoints doesn't remove them.
