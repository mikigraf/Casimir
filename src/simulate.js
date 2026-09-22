/**
 * User simulator: when a rerun diverges from the original session, later user messages
 * may no longer make sense verbatim (they reacted to what the original agent did).
 * The simulator plays the human, keeping the original intent and voice.
 */
import { userTurns, eventsForTurn, finalAssistantText, filesTouched, toolOneLiner } from "./model.js";
import { completeJson } from "./llm.js";

const SYSTEM = `You are standing in for the human user of a recorded coding-agent session that is being replayed
against a different model or harness. You will see every message the real user sent in the original session,
what the original agent had done right before the user's next message, and what the NEW agent has done so far
in the replay. Your job is to send the message this user would send now.

Rules:
- Preserve the user's intent, priorities, and voice. Do not add requirements the user never expressed.
- If the original next message still makes sense given what the new agent did, send it verbatim.
- If it reacted to something that did not happen in the replay (a bug the new agent didn't introduce,
  a question it didn't ask, a file it didn't create), adapt it so it fits the replay while pursuing the
  same underlying goal. If the original message answered a question, answer the new agent's question instead.
- If the new agent asked for a decision the original user never faced, decide the way this user most likely
  would, based on their other messages.
- If the goals behind the remaining original messages are already met, or the original message can't be
  meaningfully adapted, stop the session instead of inventing work.
Reply with a JSON object only: {"action": "send" | "stop", "message": "...", "verbatim": true|false, "reason": "..."}`;

function clip(s, n) {
  s = String(s ?? "");
  return s.length > n ? s.slice(0, n) + ` … [${s.length - n} more chars]` : s;
}

function turnActivity(session, turn, maxTools = 30) {
  const evs = eventsForTurn(session, turn).filter((e) => !e.sidechain);
  const tools = evs.filter((e) => e.kind === "tool_call").map((e) => "  - " + toolOneLiner(e, 140));
  const shown = tools.length > maxTools ? [...tools.slice(0, maxTools), `  … ${tools.length - maxTools} more tool calls`] : tools;
  const errors = evs.filter((e) => e.kind === "error").map((e) => "  ! " + clip(e.text, 300));
  const final = finalAssistantText(session, turn);
  return [`tool calls (${tools.length}):`, ...(shown.length ? shown : ["  (none)"]), ...errors, `final assistant message:`, clip(final, 5000) || "(none)"].join("\n");
}

/**
 * Decide the user message for `turnIndex` (1-based, >= 2) of the rerun.
 * Returns {message: string|null, verbatim: boolean, reason: string}.
 */
export async function simulateUserTurn({ original, rerun, turnIndex, model, backend }) {
  const turns = userTurns(original);
  const target = turns[turnIndex - 1];
  if (!target) return { message: null, verbatim: false, reason: "no such turn in original" };
  const prevTurn = turnIndex - 1;
  const prompt = [
    `# All user messages in the ORIGINAL session`,
    ...turns.map((t) => `[turn ${t.turn}]${t.turn === turnIndex ? " (the one to send now)" : ""}\n${clip(t.text, 3000)}`),
    "",
    `# What the ORIGINAL agent did in turn ${prevTurn}, right before the user sent turn ${turnIndex}`,
    turnActivity(original, prevTurn),
    "",
    `# What the NEW agent did in turn ${prevTurn} of the replay`,
    turnActivity(rerun, prevTurn),
    `files touched so far in the replay: ${filesTouched(rerun).map((f) => f.path).join(", ") || "(none)"}`,
    "",
    `# Task`,
    `Produce the user's message for turn ${turnIndex} of the replay (or stop). Original text of that message:`,
    clip(target.text, 6000),
  ].join("\n");
  const obj = await completeJson({ system: SYSTEM, prompt, model, backend });
  if (obj.action === "stop" || !obj.message?.trim()) {
    return { message: null, verbatim: false, reason: obj.reason || "simulator stopped the session" };
  }
  return { message: String(obj.message), verbatim: !!obj.verbatim || obj.message.trim() === target.text.trim(), reason: obj.reason || "" };
}
