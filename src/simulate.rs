//! User simulator: when a rerun diverges from the original session, later user messages may no
//! longer make sense verbatim (they reacted to what the original agent did). The simulator plays
//! the human, keeping the original intent and voice.
use anyhow::Result;
use serde_json::Value;

use crate::llm::{complete_json, LlmOpts};
use crate::model::{events_for_turn, files_touched, final_assistant_text, tool_one_liner, user_turns, EventKind, Session};

const SYSTEM: &str = "You are standing in for the human user of a recorded coding-agent session that is being replayed
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
Reply with a JSON object only: {\"action\": \"send\" | \"stop\", \"message\": \"...\", \"verbatim\": true|false, \"reason\": \"...\"}";

fn clip(s: &str, n: usize) -> String {
    let count = s.chars().count();
    if count > n {
        format!("{} … [{} more chars]", s.chars().take(n).collect::<String>(), count - n)
    } else {
        s.to_string()
    }
}

fn turn_activity(session: &Session, turn: u32) -> String {
    let evs: Vec<_> = events_for_turn(session, turn).into_iter().filter(|e| !e.sidechain).collect();
    let tools: Vec<String> = evs.iter().filter(|e| e.kind == EventKind::ToolCall).map(|e| format!("  - {}", tool_one_liner(e, 140))).collect();
    let mut shown: Vec<String> = tools.iter().take(30).cloned().collect();
    if tools.len() > 30 {
        shown.push(format!("  … {} more tool calls", tools.len() - 30));
    }
    if shown.is_empty() {
        shown.push("  (none)".into());
    }
    let errors: Vec<String> = evs.iter().filter(|e| e.kind == EventKind::Error).map(|e| format!("  ! {}", clip(e.text_str(), 300))).collect();
    let fin = final_assistant_text(session, Some(turn));
    let mut out = vec![format!("tool calls ({}):", tools.len())];
    out.extend(shown);
    out.extend(errors);
    out.push("final assistant message:".into());
    out.push(if fin.is_empty() { "(none)".into() } else { clip(&fin, 5000) });
    out.join("\n")
}

#[derive(Clone, Debug)]
pub struct SimResult {
    pub message: Option<String>,
    pub verbatim: bool,
    pub reason: String,
}

/// Decide the user message for `turn_index` (1-based, >= 2) of the rerun.
pub fn simulate_user_turn(original: &Session, rerun: &Session, turn_index: u32, llm: &LlmOpts) -> Result<SimResult> {
    let turns = user_turns(original);
    let Some(target) = turns.iter().find(|t| t.turn == turn_index) else {
        return Ok(SimResult { message: None, verbatim: false, reason: "no such turn in original".into() });
    };
    let prev = turn_index.saturating_sub(1).max(1);
    let mut p: Vec<String> = vec!["# All user messages in the ORIGINAL session".into()];
    for t in &turns {
        p.push(format!("[turn {}]{}\n{}", t.turn, if t.turn == turn_index { " (the one to send now)" } else { "" }, clip(&t.text, 3000)));
    }
    p.push(String::new());
    p.push(format!("# What the ORIGINAL agent did in turn {prev}, right before the user sent turn {turn_index}"));
    p.push(turn_activity(original, prev));
    p.push(String::new());
    p.push(format!("# What the NEW agent did in turn {prev} of the replay"));
    p.push(turn_activity(rerun, prev));
    let files = files_touched(rerun).iter().map(|f| f.path.clone()).collect::<Vec<_>>().join(", ");
    p.push(format!("files touched so far in the replay: {}", if files.is_empty() { "(none)".into() } else { files }));
    p.push(String::new());
    p.push("# Task".into());
    p.push(format!("Produce the user's message for turn {turn_index} of the replay (or stop). Original text of that message:"));
    p.push(clip(&target.text, 6000));
    let obj = complete_json(SYSTEM, &p.join("\n"), llm)?;
    let message = obj.get("message").and_then(Value::as_str).unwrap_or("").to_string();
    let reason = obj.get("reason").and_then(Value::as_str).unwrap_or("").to_string();
    if obj.get("action").and_then(Value::as_str) == Some("stop") || message.trim().is_empty() {
        return Ok(SimResult { message: None, verbatim: false, reason: if reason.is_empty() { "simulator stopped the session".into() } else { reason } });
    }
    let verbatim = obj.get("verbatim").and_then(Value::as_bool).unwrap_or(false) || message.trim() == target.text.trim();
    Ok(SimResult { message: Some(message), verbatim, reason })
}
