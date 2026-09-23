//! User simulator: when a rerun diverges from the original session, later user messages may no
//! longer make sense verbatim (they reacted to what the original agent did). The simulator plays
//! the human, keeping the original intent and voice.
//!
//! Guardrails (after SWE-Together and tau2-bench): the simulator carries memory across turns,
//! discloses information progressively, may not introduce facts beyond the recorded session,
//! must cite which original turns a non-verbatim message is grounded in (ungrounded replies are
//! retried a bounded number of times, then the verbatim original is sent), and distinguishes
//! "goals already met" from "out of scope / cannot adapt" when stopping.
use anyhow::Result;
use serde_json::Value;

use crate::llm::{complete_json, LlmOpts};
use crate::model::{
    events_for_turn, files_touched, final_assistant_text, tool_one_liner, user_turns, EventKind,
    Session,
};

pub const MAX_RETRIES: usize = 3;

const SYSTEM: &str = "You are standing in for the human user of a recorded coding-agent session that is being replayed
against a different model or harness. You will see every message the real user sent in the original session,
what the original agent had done right before the user's next message, what the NEW agent has done so far
in the replay (all turns, with the latest in detail), and your own notes from earlier replay turns.
Your job is to send the message this user would send now.

Rules:
- Preserve the user's intent, priorities, and voice. Never add requirements, facts, file names, error
  messages or preferences that do not appear in the recorded session. Everything you say must be grounded
  in the original user's messages or in what the new agent just did; cite the original turn numbers.
- Disclose information progressively, the way the original user did: do not front-load later requests
  or details the user only revealed in later turns unless the new agent asks for them.
- If the original next message still makes sense given what the new agent did, send it verbatim.
- If it reacted to something that did not happen in the replay (a bug the new agent didn't introduce,
  a question it didn't ask, a file it didn't create), adapt it so it fits the replay while pursuing the
  same underlying goal. If the original message answered a question, answer the new agent's question
  instead, using only information from the recorded session.
- If the new agent asked for a decision the original user never faced, decide the way this user most likely
  would, based on their other messages.
- Stop instead of inventing work when: the goals behind the remaining original messages are already met
  (stop_reason \"goals_met\"); the replay has drifted so far that the remaining messages cannot be adapted
  without inventing facts (stop_reason \"cannot_adapt\"); or the recorded session lacks the information
  needed to answer what the new agent is asking (stop_reason \"out_of_scope\").
- If the intent behind this particular original message is already satisfied in the replay but later
  original messages may still apply, choose \"no_op\": say nothing at this point and let the session
  move on to the next original message. Use \"stop\" only when nothing that remains applies.
- Keep a short private note (memory) of what you have already disclosed or decided, for later turns.
- Label what kind of message you send: verbatim (the original text), answer (answering the agent's
  question), question (asking the agent something), redirect (steering it back to the goal), or
  new_requirement (a requirement the original user raised at this point).
Reply with a JSON object only:
{\"action\": \"send\" | \"no_op\" | \"stop\", \"kind\": \"verbatim\" | \"answer\" | \"question\" | \"redirect\" | \"new_requirement\" | null,
 \"message\": \"...\", \"verbatim\": true|false, \"grounded_in\": [original turn numbers],
 \"reason\": \"...\", \"stop_reason\": \"goals_met\" | \"cannot_adapt\" | \"out_of_scope\" | null, \"memory\": \"...\"}";

fn clip(s: &str, n: usize) -> String {
    let count = s.chars().count();
    if count > n {
        format!(
            "{} … [{} more chars]",
            s.chars().take(n).collect::<String>(),
            count - n
        )
    } else {
        s.to_string()
    }
}

fn turn_activity(session: &Session, turn: u32, max_tools: usize, final_chars: usize) -> String {
    let evs: Vec<_> = events_for_turn(session, turn)
        .into_iter()
        .filter(|e| !e.sidechain)
        .collect();
    let tools: Vec<String> = evs
        .iter()
        .filter(|e| e.kind == EventKind::ToolCall)
        .map(|e| format!("  - {}", tool_one_liner(e, 140)))
        .collect();
    let mut shown: Vec<String> = tools.iter().take(max_tools).cloned().collect();
    if tools.len() > max_tools {
        shown.push(format!("  … {} more tool calls", tools.len() - max_tools));
    }
    if shown.is_empty() {
        shown.push("  (none)".into());
    }
    let errors: Vec<String> = evs
        .iter()
        .filter(|e| e.kind == EventKind::Error)
        .map(|e| format!("  ! {}", clip(e.text_str(), 300)))
        .collect();
    let fin = final_assistant_text(session, Some(turn));
    let mut out = vec![format!("tool calls ({}):", tools.len())];
    out.extend(shown);
    out.extend(errors);
    out.push("final assistant message:".into());
    out.push(if fin.is_empty() {
        "(none)".into()
    } else {
        clip(&fin, final_chars)
    });
    out.join("\n")
}

/// State the simulator carries across turns of one rerun.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct SimState {
    /// One note per simulated turn, in order.
    pub memory: Vec<String>,
    /// One-time session analysis (objective, constraints, intervention conditions) from the brief.
    pub analysis: Option<String>,
}

#[derive(Clone, Debug)]
pub struct SimResult {
    pub message: Option<String>,
    pub verbatim: bool,
    pub reason: String,
    /// goals_met | cannot_adapt | out_of_scope when `message` is None and the session stops.
    pub stop_reason: Option<String>,
    /// The simulator chose to say nothing at this turn and continue with the next original message.
    pub no_op: bool,
    /// verbatim | answer | question | redirect | new_requirement.
    pub kind: Option<String>,
    pub grounded_in: Vec<u32>,
    /// Number of ungrounded replies discarded before this result.
    pub retries: usize,
}

fn build_prompt(
    original: &Session,
    rerun: &Session,
    turn_index: u32,
    state: &SimState,
    target_text: &str,
) -> String {
    let turns = user_turns(original);
    let prev = turn_index.saturating_sub(1).max(1);
    let mut p: Vec<String> = Vec::new();
    if let Some(a) = &state.analysis {
        p.push("# Session analysis (what this user wanted, their constraints, and when they intervened)".into());
        p.push(a.clone());
        p.push(String::new());
    }
    p.push("# All user messages in the ORIGINAL session".into());
    for t in &turns {
        p.push(format!(
            "[turn {}]{}\n{}",
            t.turn,
            if t.turn == turn_index {
                " (the one to send now)"
            } else {
                ""
            },
            clip(&t.text, 3000)
        ));
    }
    p.push(String::new());
    p.push(format!("# What the ORIGINAL agent did in turn {prev}, right before the user sent turn {turn_index}"));
    p.push(turn_activity(original, prev, 30, 5000));
    p.push(String::new());
    p.push("# What the NEW agent did in the replay so far".into());
    for t in 1..prev {
        p.push(format!("## replay turn {t} (summary)"));
        let sent = rerun
            .events
            .iter()
            .find(|e| e.kind == EventKind::User && e.turn == t)
            .map(|e| clip(e.text_str(), 400))
            .unwrap_or_default();
        p.push(format!("user sent: {sent}"));
        p.push(turn_activity(rerun, t, 8, 1200));
    }
    p.push(format!("## replay turn {prev} (latest, in detail)"));
    p.push(turn_activity(rerun, prev, 30, 5000));
    let files = files_touched(rerun)
        .iter()
        .map(|f| f.path.clone())
        .collect::<Vec<_>>()
        .join(", ");
    p.push(format!(
        "files touched so far in the replay: {}",
        if files.is_empty() {
            "(none)".into()
        } else {
            files
        }
    ));
    if !state.memory.is_empty() {
        p.push(String::new());
        p.push("# Your notes from earlier replay turns".into());
        for (i, m) in state.memory.iter().enumerate() {
            p.push(format!("{}. {}", i + 1, clip(m, 600)));
        }
    }
    p.push(String::new());
    p.push("# Task".into());
    p.push(format!("Produce the user's message for turn {turn_index} of the replay (or stop). Original text of that message:"));
    p.push(clip(target_text, 6000));
    p.join("\n")
}

fn parse_turns(v: Option<&Value>) -> Vec<u32> {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| {
                    x.as_u64()
                        .or_else(|| x.as_str().and_then(|s| s.parse().ok()))
                })
                .filter_map(|n| u32::try_from(n).ok())
                .collect()
        })
        .unwrap_or_default()
}

/// Decide the user message for `turn_index` (1-based, >= 2) of the rerun.
pub fn simulate_user_turn(
    original: &Session,
    rerun: &Session,
    turn_index: u32,
    llm: &LlmOpts,
    state: &mut SimState,
) -> Result<SimResult> {
    let turns = user_turns(original);
    let Some(target) = turns.iter().find(|t| t.turn == turn_index) else {
        return Ok(SimResult {
            message: None,
            verbatim: false,
            reason: "no such turn in original".into(),
            stop_reason: Some("out_of_scope".into()),
            no_op: false,
            kind: None,
            grounded_in: vec![],
            retries: 0,
        });
    };
    let max_turn = turns.iter().map(|t| t.turn).max().unwrap_or(0);
    let base_prompt = build_prompt(original, rerun, turn_index, state, &target.text);
    let mut prompt = base_prompt.clone();
    let mut retries = 0;
    loop {
        let obj = complete_json(SYSTEM, &prompt, llm)?;
        let message = obj
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let reason = obj
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let memory = obj
            .get("memory")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        let grounded_in = parse_turns(obj.get("grounded_in"));
        let action = obj.get("action").and_then(Value::as_str).unwrap_or("");
        let kind = obj
            .get("kind")
            .and_then(Value::as_str)
            .filter(|k| {
                matches!(
                    *k,
                    "verbatim" | "answer" | "question" | "redirect" | "new_requirement"
                )
            })
            .map(String::from);
        if action == "no_op" {
            if !memory.is_empty() {
                state.memory.push(memory);
            }
            return Ok(SimResult {
                message: None,
                verbatim: false,
                reason: if reason.is_empty() {
                    "nothing to add at this turn".into()
                } else {
                    reason
                },
                stop_reason: None,
                no_op: true,
                kind: None,
                grounded_in,
                retries,
            });
        }
        let stop_reason = obj
            .get("stop_reason")
            .and_then(Value::as_str)
            .filter(|s| matches!(*s, "goals_met" | "cannot_adapt" | "out_of_scope"));
        if let ("stop", Some(stop_reason)) = (action, stop_reason) {
            let stop_reason = stop_reason.to_string();
            if !memory.is_empty() {
                state.memory.push(memory);
            }
            return Ok(SimResult {
                message: None,
                verbatim: false,
                reason: if reason.is_empty() {
                    "simulator stopped the session".into()
                } else {
                    reason
                },
                stop_reason: Some(stop_reason),
                no_op: false,
                kind: None,
                grounded_in,
                retries,
            });
        }
        let verbatim = message == target.text;
        let grounded = action == "send"
            && !message.trim().is_empty()
            && (verbatim
                || (!grounded_in.is_empty()
                    && grounded_in.iter().all(|t| *t >= 1 && *t <= max_turn)));
        if !grounded {
            if retries == MAX_RETRIES {
                // bounded: fall back to the recorded message rather than send an ungrounded one
                return Ok(SimResult {
                    message: Some(target.text.clone()),
                    verbatim: true,
                    reason: format!("simulator produced invalid or ungrounded replies after {MAX_RETRIES} retries; sent the original verbatim"),
                    stop_reason: None,
                    no_op: false,
                    kind: Some("verbatim".into()),
                    grounded_in: vec![turn_index],
                    retries,
                });
            }
            retries += 1;
            prompt = format!(
                "{base_prompt}\n\n# Correction\nYour previous reply was invalid or ungrounded: use action send with a nonempty message, no_op, or stop with a valid stop_reason. An adapted message must list the original turn numbers it draws from in \"grounded_in\" (1..={max_turn}), and must not introduce anything absent from the recorded session. Try again."
            );
            continue;
        }
        if !memory.is_empty() {
            state.memory.push(memory);
        }
        let grounded_in = if verbatim && grounded_in.is_empty() {
            vec![turn_index]
        } else {
            grounded_in
        };
        let kind = if verbatim {
            Some("verbatim".into())
        } else {
            kind.filter(|k| k != "verbatim")
                .or_else(|| Some("answer".into()))
        };
        return Ok(SimResult {
            message: Some(message),
            verbatim,
            reason,
            stop_reason: None,
            no_op: false,
            kind,
            grounded_in,
            retries,
        });
    }
}
