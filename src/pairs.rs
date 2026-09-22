//! Blinded original-vs-simulated message pairs for occasional human 2AFC spot checks of the user
//! simulator (SWE-Together's Turing protocol, arXiv 2606.29957), and scoring of the answers.
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::adapters::load_session_file;
use crate::model::{final_assistant_text, EventKind, Session};
use crate::rerun::wilson;
use crate::util::{read_json, write_json};

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PairCandidate {
    pub label: String,
    /// What the agent had just said when this message was sent.
    pub preceding_agent_message: String,
    pub message: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Pair {
    pub pair_id: String,
    pub task_summary: String,
    pub candidates: Vec<PairCandidate>,
    pub instruction: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct PairKey {
    pub real: String,
    pub run: PathBuf,
    pub turn: u32,
    pub session: String,
}

pub struct PairsExport {
    pub pairs_path: PathBuf,
    pub key_path: PathBuf,
    pub n: usize,
}

fn clip(s: &str, n: usize) -> String {
    let c = s.chars().count();
    if c > n {
        format!("{}…", s.chars().take(n).collect::<String>())
    } else {
        s.to_string()
    }
}

/// Deterministic but unpredictable-looking side assignment from the pair id.
fn side_for(pair_id: &str) -> bool {
    use sha2::{Digest, Sha256};
    Sha256::digest(pair_id.as_bytes())[0] & 1 == 0
}

/// Export blinded pairs from rerun directories that contain simulated (adapted) turns.
pub fn export_pairs(runs: &[PathBuf], out_dir: &Path) -> Result<PairsExport> {
    std::fs::create_dir_all(out_dir)?;
    let mut pairs: Vec<Pair> = Vec::new();
    let mut key: BTreeMap<String, PairKey> = BTreeMap::new();
    for run in runs {
        let rerun: Session = load_session_file(run).with_context(|| format!("loading {}", run.display()))?;
        let original_path = run.join("original.json");
        if !original_path.exists() {
            continue;
        }
        let original: Session = read_json(&original_path)?;
        for e in rerun.events.iter().filter(|e| e.kind == EventKind::User && !e.sidechain) {
            let Some(sim) = &e.simulated else { continue };
            if sim.verbatim || sim.action.as_deref() == Some("intervention") {
                continue;
            }
            let Some(orig) = original.events.iter().find(|o| o.kind == EventKind::User && !o.sidechain && o.turn == e.turn) else { continue };
            let pair_id = format!("{}-{}", &rerun.id.chars().take(8).collect::<String>(), e.turn);
            let real_first = side_for(&pair_id);
            let human = PairCandidate { label: String::new(), preceding_agent_message: clip(&final_assistant_text(&original, Some(e.turn - 1)), 1500), message: orig.text_str().to_string() };
            let simulated = PairCandidate { label: String::new(), preceding_agent_message: clip(&final_assistant_text(&rerun, Some(e.turn - 1)), 1500), message: e.text_str().to_string() };
            let (mut x, mut y) = if real_first { (human, simulated) } else { (simulated, human) };
            x.label = "X".into();
            y.label = "Y".into();
            key.insert(pair_id.clone(), PairKey { real: if real_first { "X".into() } else { "Y".into() }, run: run.clone(), turn: e.turn, session: original.id.clone() });
            pairs.push(Pair {
                pair_id,
                task_summary: clip(original.title.as_deref().unwrap_or(""), 200),
                candidates: vec![x, y],
                instruction: "One message was written by the real user, the other by a simulator reacting to a replay. Each candidate shows the agent message it was replying to. Answer with the label (X or Y) of the message you believe the real human wrote.".into(),
            });
        }
    }
    let pairs_path = out_dir.join("pairs.jsonl");
    let mut text = String::new();
    for p in &pairs {
        text.push_str(&serde_json::to_string(p)?);
        text.push('\n');
    }
    std::fs::write(&pairs_path, text)?;
    let key_path = out_dir.join("pairs.key.json");
    write_json(&key_path, &key)?;
    let template: BTreeMap<String, String> = key.keys().map(|k| (k.clone(), String::new())).collect();
    write_json(&out_dir.join("answers.template.json"), &template)?;
    Ok(PairsExport { pairs_path, key_path, n: pairs.len() })
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct PairsScore {
    pub answered: usize,
    /// Pairs where the annotator picked the simulated message as the human one.
    pub simulator_passed: usize,
    /// Turing pass rate: share of pairs where the simulator was mistaken for the human (0.5 = indistinguishable).
    pub pass_rate: f64,
    pub ci_low: f64,
    pub ci_high: f64,
    pub sessions: usize,
}

/// Score annotator answers (`{pair_id: "X"|"Y"}`, the label believed to be the real human) against the key.
pub fn score_pairs(key_path: &Path, answers_path: &Path) -> Result<PairsScore> {
    let key: BTreeMap<String, PairKey> = read_json(key_path)?;
    let answers: BTreeMap<String, Value> = read_json(answers_path)?;
    let mut answered = 0;
    let mut passed = 0;
    let mut sessions: Vec<String> = Vec::new();
    for (id, k) in &key {
        let Some(a) = answers.get(id).and_then(Value::as_str).map(|s| s.trim().to_ascii_uppercase()).filter(|s| s == "X" || s == "Y") else { continue };
        answered += 1;
        if a != k.real {
            passed += 1;
        }
        if !sessions.contains(&k.session) {
            sessions.push(k.session.clone());
        }
    }
    let (lo, hi) = wilson(passed, answered);
    Ok(PairsScore { answered, simulator_passed: passed, pass_rate: if answered == 0 { 0.0 } else { passed as f64 / answered as f64 }, ci_low: lo, ci_high: hi, sessions: sessions.len() })
}
