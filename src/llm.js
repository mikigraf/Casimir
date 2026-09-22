/**
 * Small LLM client used by the user simulator and the judge.
 *
 * Backends:
 *   api        - Anthropic SDK (ANTHROPIC_API_KEY / ANTHROPIC_AUTH_TOKEN / `ant auth login` profile)
 *   claude-cli - `claude -p` with tools disabled; reuses the local Claude Code login
 *   auto       - api when Anthropic credentials are visible, otherwise claude-cli
 */
import { spawn } from "node:child_process";
import path from "node:path";
import { cleanEnv, extractJson, homeDir, exists } from "./util.js";

export const DEFAULT_MODEL = "claude-opus-5";

export function hasApiCredentials() {
  if (process.env.ANTHROPIC_API_KEY || process.env.ANTHROPIC_AUTH_TOKEN) return true;
  return exists(path.join(homeDir(), ".config", "anthropic"));
}

export function pickBackend(requested = "auto") {
  if (requested && requested !== "auto") return requested;
  return hasApiCredentials() ? "api" : "claude-cli";
}

async function completeApi({ system, prompt, model, maxTokens }) {
  let Anthropic;
  try {
    ({ default: Anthropic } = await import("@anthropic-ai/sdk"));
  } catch {
    throw new Error("the api backend needs @anthropic-ai/sdk (run `npm install`), or use --llm claude-cli");
  }
  const client = new Anthropic();
  const response = await client.beta.messages.create({
    model,
    max_tokens: maxTokens,
    system,
    messages: [{ role: "user", content: prompt }],
    betas: ["server-side-fallback-2026-07-01"],
    fallbacks: "default",
  });
  if (response.stop_reason === "refusal") {
    throw new Error(`model refused (${response.stop_details?.category || "unknown"}): ${response.stop_details?.explanation || ""}`);
  }
  return response.content
    .filter((b) => b.type === "text")
    .map((b) => b.text)
    .join("\n");
}

function completeCli({ system, prompt, model }) {
  const bin = process.env.CASIMIR_CLAUDE_BIN || "claude";
  const args = ["-p", "--output-format", "json", "--tools", "", "--no-session-persistence"];
  if (model) args.push("--model", model);
  if (system) args.push("--system-prompt", system);
  return new Promise((resolve, reject) => {
    const child = spawn(bin, args, { env: cleanEnv(), stdio: ["pipe", "pipe", "pipe"] });
    let out = "";
    let err = "";
    child.stdout.on("data", (d) => (out += d));
    child.stderr.on("data", (d) => (err += d));
    child.on("error", reject);
    child.on("close", (code) => {
      let parsed;
      try {
        parsed = JSON.parse(out.trim().split("\n").pop());
      } catch {
        return reject(new Error(`claude -p returned no JSON (exit ${code}): ${err.trim() || out.trim()}`));
      }
      if (parsed.is_error) return reject(new Error(`claude -p error: ${parsed.result}`));
      resolve(String(parsed.result ?? ""));
    });
    child.stdin.end(prompt);
  });
}

/** Returns the text completion. */
export async function complete({ system, prompt, model = DEFAULT_MODEL, backend = "auto", maxTokens = 16000 }) {
  const b = pickBackend(backend);
  if (b === "api") return completeApi({ system, prompt, model, maxTokens });
  if (b === "claude-cli") return completeCli({ system, prompt, model });
  throw new Error(`unknown llm backend ${b}`);
}

/** Like complete(), but parses a JSON object out of the reply (retries once on parse failure). */
export async function completeJson(opts) {
  let text = await complete(opts);
  let obj = extractJson(text);
  if (!obj) {
    text = await complete({ ...opts, prompt: opts.prompt + "\n\nRespond with a single JSON object and nothing else." });
    obj = extractJson(text);
  }
  if (!obj) throw new Error(`LLM did not return JSON: ${text.slice(0, 300)}`);
  return obj;
}
