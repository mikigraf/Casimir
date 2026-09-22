import { spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { exists } from "./util.js";

function git(args, cwd, { check = true } = {}) {
  const r = spawnSync("git", args, { cwd, encoding: "utf8", maxBuffer: 64 * 1024 * 1024 });
  if (check && r.status !== 0) throw new Error(`git ${args.join(" ")} failed: ${(r.stderr || r.stdout || "").trim()}`);
  return r.stdout ?? "";
}

export function isGitRepo(dir) {
  if (!dir || !exists(dir)) return false;
  const r = spawnSync("git", ["rev-parse", "--is-inside-work-tree"], { cwd: dir, encoding: "utf8" });
  return r.status === 0 && r.stdout.trim() === "true";
}

export function repoRoot(dir) {
  return git(["rev-parse", "--show-toplevel"], dir).trim();
}

export function headCommit(dir) {
  return git(["rev-parse", "HEAD"], dir).trim();
}

/**
 * Best-effort commit the original session started from:
 * the recorded commit if the harness logged one, otherwise the last commit on the
 * recorded branch (or HEAD) that predates the session start.
 */
export function baseCommit(session, dir) {
  if (session.gitCommit && commitExists(session.gitCommit, dir)) return { commit: session.gitCommit, how: "recorded in session log" };
  const refs = [session.gitBranch, "HEAD"].filter(Boolean);
  for (const ref of refs) {
    if (session.startedAt) {
      const r = spawnSync("git", ["rev-list", "-1", `--before=${session.startedAt}`, ref], { cwd: dir, encoding: "utf8" });
      const sha = r.stdout?.trim();
      if (r.status === 0 && sha) return { commit: sha, how: `last commit on ${ref} before session start` };
    }
  }
  return { commit: headCommit(dir), how: "current HEAD (no better information)" };
}

export function commitExists(sha, dir) {
  const r = spawnSync("git", ["cat-file", "-e", `${sha}^{commit}`], { cwd: dir, encoding: "utf8" });
  return r.status === 0;
}

/** Create a detached worktree at `commit` under `dest`. */
export function createWorktree(repoDir, commit, dest) {
  fs.mkdirSync(path.dirname(dest), { recursive: true });
  git(["worktree", "add", "--detach", dest, commit], repoDir);
  return dest;
}

export function removeWorktree(repoDir, dest) {
  spawnSync("git", ["worktree", "remove", "--force", dest], { cwd: repoDir, encoding: "utf8" });
}

/** Changed files (status --porcelain) and a unified patch including untracked files. */
export function captureDiff(dir) {
  if (!isGitRepo(dir)) return { files: [], patch: "", stat: "" };
  const status = git(["status", "--porcelain", "--untracked-files=all"], dir);
  const files = status
    .split("\n")
    .filter(Boolean)
    .map((l) => ({ status: l.slice(0, 2).trim(), path: l.slice(3).trim() }));
  let patch = git(["diff", "HEAD", "--no-color"], dir, { check: false });
  const untracked = git(["ls-files", "--others", "--exclude-standard"], dir).split("\n").filter(Boolean);
  for (const f of untracked) {
    const r = spawnSync("git", ["diff", "--no-index", "--no-color", "--", "/dev/null", f], { cwd: dir, encoding: "utf8", maxBuffer: 64 * 1024 * 1024 });
    if (r.stdout) patch += r.stdout;
  }
  const stat = git(["diff", "HEAD", "--stat", "--no-color"], dir, { check: false }) + (untracked.length ? untracked.map((f) => ` ${f} | (new file)`).join("\n") + "\n" : "");
  return { files, patch, stat: stat.trim() };
}
