//! Per-session brief: a one-time, human-editable artifact drafted by an LLM from the original
//! session and consumed by the judge (rubric + invalid-reason taxonomy, after arXiv 2511.10865),
//! the user simulator (session analysis: objective, constraints, intervention conditions, after
//! SWE-Together arXiv 2606.29957), and the intent-coverage check (atomic intents).
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;

use crate::llm::{complete_json, effective_model, LlmOpts};
use crate::model::{final_assistant_text, user_turns, Session};
use crate::util::{now_iso, read_json, write_json};
use crate::workspace::Diff;

pub const INVALID_REASONS: &[&str] = &[
    "requirement_violation",
    "root_cause_not_addressed",
    "incomplete_implementation",
    "new_issues_introduced",
];

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Criterion {
    pub id: String,
    pub text: String,
    /// A must-have: violating it makes the result invalid regardless of other merits.
    #[serde(default)]
    pub must: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Intent {
    pub id: String,
    pub text: String,
    /// Original user turn the intent was expressed in.
    pub turn: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Brief {
    #[serde(default)]
    pub schema_version: u32,
    pub session_id: String,
    pub drafted_by: String,
    pub drafted_at: String,
    /// Set to true once a human has reviewed and edited the draft.
    #[serde(default)]
    pub human_reviewed: bool,
    /// What the user was ultimately trying to achieve.
    pub objective: String,
    /// Constraints the user stated or clearly implied (style, scope, tools, files not to touch…).
    #[serde(default)]
    pub constraints: Vec<String>,
    /// When the original user intervened and why (grounded in their follow-up messages).
    #[serde(default)]
    pub intervention_conditions: Vec<String>,
    /// Task-specific rubric the judge scores against.
    #[serde(default)]
    pub criteria: Vec<Criterion>,
    /// Atomic intents expressed across the user's turns.
    #[serde(default)]
    pub intents: Vec<Intent>,
}

impl Brief {
    pub fn load(path: &Path) -> Result<Brief> {
        read_json(path).with_context(|| format!("loading brief {}", path.display()))
    }
    pub fn save(&self, path: &Path) -> Result<()> {
        let mut brief = self.clone();
        brief.schema_version = 1;
        write_json(path, &brief)
    }
    /// Rubric text for the judge prompt.
    pub fn rubric_text(&self) -> String {
        let mut out = vec![format!("Objective: {}", self.objective)];
        if !self.constraints.is_empty() {
            out.push("Constraints:".into());
            out.extend(self.constraints.iter().map(|c| format!("- {c}")));
        }
        if !self.criteria.is_empty() {
            out.push(
                "Criteria (score against these; MUST items make a result invalid when violated):"
                    .into(),
            );
            out.extend(self.criteria.iter().map(|c| {
                format!(
                    "- [{}]{} {}",
                    c.id,
                    if c.must { " MUST" } else { "" },
                    c.text
                )
            }));
        }
        out.join("\n")
    }
    /// Session analysis text for the simulator prompt.
    pub fn analysis_text(&self) -> String {
        let mut out = vec![format!("Objective: {}", self.objective)];
        if !self.constraints.is_empty() {
            out.push("Constraints:".into());
            out.extend(self.constraints.iter().map(|c| format!("- {c}")));
        }
        if !self.intervention_conditions.is_empty() {
            out.push("When and why the original user intervened:".into());
            out.extend(
                self.intervention_conditions
                    .iter()
                    .map(|c| format!("- {c}")),
            );
        }
        out.join("\n")
    }
}

const BRIEF_SYSTEM: &str = "You analyse a recorded session between a human developer and a coding agent and produce a brief
that other tools will use to judge reruns of the same task and to simulate the same user.
Ground everything in the user's own messages; do not invent requirements. The original agent's wording
is context, not a requirement. Asking to verify an action does not require describing that verification
in the final reply unless the user explicitly asks for such a description. Do not turn optional
presentation preferences into implementation requirements. Reply with a JSON object only:
{\"objective\": \"...\",
 \"constraints\": [\"...\"],
 \"intervention_conditions\": [\"when the agent did X, the user asked for Y (turn N)\", ...],
 \"criteria\": [{\"id\": \"C1\", \"text\": \"...\", \"must\": true|false}, ...],
 \"intents\": [{\"id\": \"I1\", \"text\": \"one atomic thing the user asked for\", \"turn\": N}, ...]}
Criteria must be concrete and checkable against the diff, recorded tool calls/results, and final message.
Use only as many criteria as the task warrants; do not invent extra requirements to fill a quota.
Intents are atomic: split compound requests, keep each tied to the turn it was expressed in.";

fn clip(s: &str, n: usize) -> String {
    let c = s.chars().count();
    if c > n {
        format!(
            "{}\n… [truncated {} chars]",
            s.chars().take(n).collect::<String>(),
            c - n
        )
    } else {
        s.to_string()
    }
}

/// Draft a brief from the original session (and its workspace diff when known).
pub fn draft_brief(original: &Session, diff: Option<&Diff>, llm: &LlmOpts) -> Result<Brief> {
    let mut p: Vec<String> = vec!["# User messages (in order)".into()];
    for t in user_turns(original) {
        p.push(format!("[turn {}]\n{}", t.turn, clip(&t.text, 4000)));
    }
    p.push(String::new());
    p.push("# Agent's final message in the original session".into());
    p.push(clip(&final_assistant_text(original, None), 4000));
    if let Some(d) = diff.filter(|d| !d.patch.is_empty()) {
        p.push(String::new());
        p.push("# Workspace diff produced in the original session (for context; the rubric judges the task, not this diff)".into());
        p.push(clip(&d.patch, 20000));
    }
    let obj = complete_json(BRIEF_SYSTEM, &p.join("\n"), llm)?;
    let arr = |k: &str| {
        obj.get(k)
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(String::from)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };
    let criteria = obj
        .get("criteria")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .enumerate()
                .filter_map(|(i, c)| {
                    let text = c.get("text").and_then(Value::as_str)?.to_string();
                    Some(Criterion {
                        id: c
                            .get("id")
                            .and_then(Value::as_str)
                            .map(String::from)
                            .unwrap_or_else(|| format!("C{}", i + 1)),
                        text,
                        must: c.get("must").and_then(Value::as_bool).unwrap_or(false),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let intents = obj
        .get("intents")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .enumerate()
                .filter_map(|(i, c)| {
                    let text = c.get("text").and_then(Value::as_str)?.to_string();
                    Some(Intent {
                        id: c
                            .get("id")
                            .and_then(Value::as_str)
                            .map(String::from)
                            .unwrap_or_else(|| format!("I{}", i + 1)),
                        text,
                        turn: c.get("turn").and_then(Value::as_u64).unwrap_or(1) as u32,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(Brief {
        schema_version: 1,
        session_id: original.id.clone(),
        drafted_by: effective_model(llm),
        drafted_at: now_iso(),
        human_reviewed: false,
        objective: obj
            .get("objective")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        constraints: arr("constraints"),
        intervention_conditions: arr("intervention_conditions"),
        criteria,
        intents,
    })
}

pub fn render_brief_markdown(b: &Brief) -> String {
    let mut md = vec![
        format!("# Brief for session {}", b.session_id),
        String::new(),
        format!(
            "_drafted by {} at {}; human reviewed: {}_",
            b.drafted_by,
            b.drafted_at,
            if b.human_reviewed {
                "yes"
            } else {
                "no — edit brief.json and set humanReviewed to true"
            }
        ),
        String::new(),
    ];
    md.push(format!("## Objective\n\n{}", b.objective));
    md.push("\n## Constraints\n".into());
    md.extend(b.constraints.iter().map(|c| format!("- {c}")));
    md.push("\n## Intervention conditions\n".into());
    md.extend(b.intervention_conditions.iter().map(|c| format!("- {c}")));
    md.push("\n## Rubric\n".into());
    md.extend(b.criteria.iter().map(|c| {
        format!(
            "- **{}**{}: {}",
            c.id,
            if c.must { " (must)" } else { "" },
            c.text
        )
    }));
    md.push("\n## Intents\n".into());
    md.extend(
        b.intents
            .iter()
            .map(|i| format!("- **{}** (turn {}): {}", i.id, i.turn, i.text)),
    );
    md.push(String::new());
    md.push(format!(
        "Invalid-reason taxonomy used by the judge: {}",
        INVALID_REASONS.join(", ")
    ));
    md.join("\n")
}

// ---------------------------------------------------------------------------------------------
// Intent coverage (SWE-Together): recall of original intents re-expressed by the simulated user,
// precision of simulated messages that stay within the original scope.

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct IntentCoverage {
    pub recall: f64,
    pub precision: f64,
    /// round(0.70 × recall + 0.30 × precision, 2)
    pub score: f64,
    pub intents: usize,
    pub covered: Vec<String>,
    pub simulated_messages: usize,
    pub in_scope: usize,
    pub model: String,
}

const INTENT_SYSTEM: &str = "You compare a list of atomic user intents from an original coding session with the messages a simulated
user sent in a replay. Decide which intents are re-expressed (covered) by the simulated messages, and which
simulated messages stay within the scope of the original intents. Reply with a JSON object only:
{\"covered\": [\"I1\", ...], \"in_scope\": [message indices, 0-based]}";

/// Compute intent coverage for a rerun. Verbatim turns cover their own turn's intents by construction;
/// adapted messages are matched by the LLM.
pub fn intent_coverage(
    brief: &Brief,
    rerun: &Session,
    llm: &LlmOpts,
) -> Result<Option<IntentCoverage>> {
    if brief.intents.is_empty() {
        return Ok(None);
    }
    let sim_msgs: Vec<(u32, bool, String)> = rerun
        .events
        .iter()
        .filter(|e| e.kind == crate::model::EventKind::User && !e.sidechain)
        .filter_map(|e| {
            e.simulated.as_ref().map(|s| {
                (
                    e.source_turn.unwrap_or(e.turn),
                    s.verbatim,
                    e.text_str().to_string(),
                )
            })
        })
        .collect();
    if sim_msgs.is_empty() {
        return Ok(None);
    }
    let mut covered: Vec<String> = Vec::new();
    // turns sent verbatim (and the never-simulated turn 1) cover their intents
    let verbatim_turns: Vec<u32> = std::iter::once(1u32)
        .chain(sim_msgs.iter().filter(|(_, v, _)| *v).map(|(t, _, _)| *t))
        .collect();
    for i in &brief.intents {
        if verbatim_turns.contains(&i.turn) {
            covered.push(i.id.clone());
        }
    }
    let adapted: Vec<(usize, &(u32, bool, String))> = sim_msgs
        .iter()
        .enumerate()
        .filter(|(_, (_, v, _))| !*v)
        .collect();
    let mut in_scope = sim_msgs.iter().filter(|(_, v, _)| *v).count();
    if !adapted.is_empty() {
        let mut p: Vec<String> = vec!["# Original intents".into()];
        p.extend(
            brief
                .intents
                .iter()
                .map(|i| format!("{} (turn {}): {}", i.id, i.turn, i.text)),
        );
        p.push(String::new());
        p.push("# Simulated messages (index: text)".into());
        for (idx, (_, _, text)) in &adapted {
            p.push(format!("{idx}: {}", clip(text, 2000)));
        }
        let obj = complete_json(INTENT_SYSTEM, &p.join("\n"), llm)?;
        for id in obj
            .get("covered")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            if brief.intents.iter().any(|i| i.id == id) && !covered.iter().any(|c| c == id) {
                covered.push(id.to_string());
            }
        }
        let scoped: Vec<usize> = obj
            .get("in_scope")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_u64)
            .map(|n| n as usize)
            .collect();
        in_scope += adapted
            .iter()
            .filter(|(idx, _)| scoped.contains(idx))
            .count();
    }
    let recall = covered.len() as f64 / brief.intents.len() as f64;
    let precision = in_scope as f64 / sim_msgs.len() as f64;
    Ok(Some(IntentCoverage {
        recall,
        precision,
        score: ((0.70 * recall + 0.30 * precision) * 100.0).round() / 100.0,
        intents: brief.intents.len(),
        covered,
        simulated_messages: sim_msgs.len(),
        in_scope,
        model: effective_model(llm),
    }))
}
