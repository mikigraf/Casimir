//! Offline scoring against a frozen, independently reviewed corpus. No labels are invented.
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::path::Path;

/// Run the production AB/BA judge on frozen trace evidence. Construction strata and human
/// labels are deliberately excluded from its inputs. These are predictions, never reviews.
pub fn predict(corpus_path: &Path, output: &Path, llm: &crate::llm::LlmOpts) -> Result<Value> {
    predict_limit(corpus_path, output, llm, 40)
}
pub fn predict_limit(
    corpus_path: &Path,
    output: &Path,
    llm: &crate::llm::LlmOpts,
    limit: usize,
) -> Result<Value> {
    if !(1..=40).contains(&limit) {
        bail!("prediction limit must be between 1 and 40; release calibration requires all 40");
    }
    use crate::{
        compare::{compare_sessions, judge_sessions_with, JudgeOpts},
        model::{Event, EventKind, Execution, Harness, Session},
    };
    let bytes = std::fs::read(corpus_path)?;
    let corpus: Value = serde_json::from_slice(&bytes)?;
    let cases = corpus["cases"].as_array().context("corpus cases missing")?;
    let ids: std::collections::BTreeSet<_> =
        cases.iter().filter_map(|c| c["id"].as_str()).collect();
    if corpus["schemaVersion"] != 1
        || corpus["frozen"] != true
        || cases.len() != 40
        || ids.len() != 40
    {
        bail!("prediction requires a frozen schemaVersion 1 corpus with 40 unique cases");
    }
    if ids.iter().any(|id| {
        id.is_empty()
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    }) {
        bail!("unsafe corpus case ID");
    }
    let _lock = crate::util::RunLock::acquire(output)?;
    if std::fs::read_dir(output)?
        .any(|entry| entry.map_or(true, |entry| entry.file_name() != ".lock"))
    {
        bail!("prediction output must be empty");
    }
    let hash = crate::checkpoint::hash(&bytes);
    crate::util::atomic_write(&output.join("corpus.json"), &bytes)?;
    let mut result = json!({"schemaVersion":1,"corpusHash":hash,"humanReviewed":false,"judgeModel":crate::llm::effective_model(llm),"casesPlanned":limit,"labels":{},"failures":{}});
    for case in cases.iter().take(limit) {
        let id = case["id"].as_str().unwrap();
        let task = case["task"].as_str().context("case task missing")?;
        let mut sessions = Vec::new();
        let mut snapshots = Vec::new();
        for (trace_name, failed_key) in [
            ("traceA", "requiredCheckFailedA"),
            ("traceB", "requiredCheckFailedB"),
        ] {
            let trace = &case[trace_name];
            let mut session = Session::new(Harness::ClaudeCode);
            session
                .events
                .push(Event::text(1, "", EventKind::User, task));
            let snapshot = trace["files"].as_object().map(|files| {
                let mut patch = String::from("Recorded final workspace snapshot (synthetic corpus evidence; no Git commit is claimed):\n");
                for (name, content) in files { patch.push_str(&format!("\n=== {name} ===\n{}\n",content.as_str().unwrap_or(""))); }
                if let Some(definition) = trace.get("checkDefinition") {
                    patch.push_str(&format!("\nFrozen external validation definition (executed independently of the candidate):\n{}\nObserved results:\n{}\n",definition,trace["toolEvidence"]));
                }
                crate::workspace::Diff { files: files.keys().map(|path| crate::workspace::ChangedFile { status:"snapshot".into(),path:path.clone() }).collect(),patch,source:Some("frozen synthetic final snapshot; not an inferred Git diff".into()),..Default::default() }
            });
            snapshots.push(snapshot);
            if let Some(evidence) = trace["toolEvidence"].as_array() {
                for (index, record) in evidence.iter().enumerate() {
                    let tool_id = format!("check-{index}");
                    session.events.push(Event::tool_call(
                        1,
                        "",
                        &tool_id,
                        "Bash",
                        json!({"command":record["command"]}),
                    ));
                    session.events.push(Event::tool_result(
                        1,
                        "",
                        &tool_id,
                        Some("Bash".into()),
                        record["output"].as_str().unwrap_or(""),
                        record["exitStatus"].as_i64().is_some_and(|n| n != 0),
                    ));
                }
            }
            session.events.push(Event::text(
                1,
                "",
                EventKind::Assistant,
                trace["finalMessage"].as_str().unwrap_or(""),
            ));
            session.execution = Some(Execution {
                requested_turns: 1,
                completed_turns: 1,
                ..Default::default()
            });
            if case[failed_key] == true
                || (trace.get("checkDefinition").is_some()
                    && trace["toolEvidence"]
                        .as_array()
                        .is_some_and(|e| !e.is_empty() && e.iter().all(|r| r["exitStatus"] == 0)))
            {
                session.evaluation = Some(
                    json!({"checks":{"schemaVersion":1,"definitionHash":hash,"outcome":if case[failed_key]==true {"failed"} else {"passed"},"results":[]}}),
                );
            }
            sessions.push(session);
        }
        let directory = output.join(id);
        crate::util::private_dir(&directory)?;
        let mut options = llm.clone();
        options.recording_dir = Some(directory.join("llm"));
        let judgement = judge_sessions_with(
            &sessions[0],
            &sessions[1],
            snapshots[0].as_ref(),
            snapshots[1].as_ref(),
            &JudgeOpts::new(options),
        );
        let label = match judgement {
            Ok(judge) => {
                let report_b = compare_sessions(
                    &sessions[0],
                    &sessions[1],
                    snapshots[0].clone(),
                    snapshots[1].clone(),
                    Some(judge.clone()),
                );
                let mut swapped = judge.clone();
                std::mem::swap(&mut swapped.score_a, &mut swapped.score_b);
                std::mem::swap(&mut swapped.invalid_a, &mut swapped.invalid_b);
                std::mem::swap(&mut swapped.evidence_a, &mut swapped.evidence_b);
                let report_a = compare_sessions(
                    &sessions[1],
                    &sessions[0],
                    snapshots[1].clone(),
                    snapshots[0].clone(),
                    Some(swapped),
                );
                crate::util::write_json(&directory.join("report.json"), &report_b)?;
                let winner = if report_a.overall_outcome == "inconclusive"
                    || report_b.overall_outcome == "inconclusive"
                {
                    "inconclusive"
                } else {
                    &judge.winner
                };
                json!({"winner":winner,"outcomeA":report_a.overall_outcome,"outcomeB":report_b.overall_outcome})
            }
            Err(error) => {
                result["failures"][id] = json!(crate::privacy::redact(&error.to_string()));
                json!({"winner":"inconclusive","outcomeA":if case["requiredCheckFailedA"]==true {"failed"} else {"inconclusive"},"outcomeB":if case["requiredCheckFailedB"]==true {"failed"} else {"inconclusive"}})
            }
        };
        result["labels"][id] = label;
        crate::util::write_json(&output.join("predictions.json"), &result)?;
    }
    Ok(result)
}

pub fn score(
    corpus: &Path,
    predictions: &Path,
    reviewer_a: &Path,
    reviewer_b: &Path,
    adjudication: &Path,
) -> Result<Value> {
    let bytes = std::fs::read(corpus)?;
    let hash = crate::checkpoint::hash(&bytes);
    let corpus: Value = serde_json::from_slice(&bytes)?;
    if corpus["schemaVersion"] != 1 || corpus["frozen"] != true {
        bail!("calibration requires a frozen schemaVersion 1 corpus");
    }
    let cases = corpus["cases"].as_array().context("corpus cases missing")?;
    if cases.len() != 40 {
        bail!("release calibration requires exactly 40 frozen trace pairs");
    }
    let ids: std::collections::BTreeSet<_> =
        cases.iter().filter_map(|c| c["id"].as_str()).collect();
    if ids.len() != 40 {
        bail!("case IDs must be present and unique");
    }
    let documents: Vec<Value> = [predictions, reviewer_a, reviewer_b, adjudication]
        .iter()
        .map(|p| crate::util::read_json(p))
        .collect::<Result<_>>()?;
    for document in &documents {
        if document["schemaVersion"] != 1 || document["corpusHash"].as_str() != Some(&hash) {
            bail!("review/prediction does not match the frozen corpus hash");
        }
        let labels = document["labels"].as_object().context("labels missing")?;
        if labels.len() != 40 || labels.keys().any(|id| !ids.contains(id.as_str())) {
            bail!("every document must cover exactly the frozen 40 case IDs");
        }
        for label in labels.values() {
            if !matches!(
                label["winner"].as_str(),
                Some("A" | "B" | "tie" | "inconclusive")
            ) || !matches!(
                label["outcomeA"].as_str(),
                Some("passed" | "failed" | "inconclusive")
            ) || !matches!(
                label["outcomeB"].as_str(),
                Some("passed" | "failed" | "inconclusive")
            ) {
                bail!("incomplete or invalid calibration label");
            }
        }
    }
    let reviewers: Vec<&str> = documents[1..]
        .iter()
        .map(|d| {
            d["reviewerId"]
                .as_str()
                .filter(|s| !s.trim().is_empty())
                .context("reviewer/adjudicator identity missing")
        })
        .collect::<Result<_>>()?;
    if reviewers[0] == reviewers[1] {
        bail!("two independent reviewer identities are required");
    }
    if documents[1..].iter().any(|d| d["humanReviewed"] != true) {
        bail!("human review attestations are required; fixture labels cannot substitute");
    }
    let mut decisive = 0;
    let mut agreements = 0;
    let mut abstentions = 0;
    let mut false_positives = 0;
    let mut unsafe_passes = 0;
    let mut disagreements = 0;
    for case in cases {
        let id = case["id"].as_str().unwrap();
        let prediction = &documents[0]["labels"][id];
        let gold = &documents[3]["labels"][id];
        if documents[1]["labels"][id] != documents[2]["labels"][id] {
            disagreements += 1;
        }
        let decisive_gold = gold["winner"] != "inconclusive"
            && gold["outcomeA"] != "inconclusive"
            && gold["outcomeB"] != "inconclusive";
        if decisive_gold {
            decisive += 1;
            if ["winner", "outcomeA", "outcomeB"]
                .iter()
                .all(|k| prediction[k] == gold[k])
            {
                agreements += 1;
            }
        }
        if prediction["winner"] == "inconclusive"
            || prediction["outcomeA"] == "inconclusive"
            || prediction["outcomeB"] == "inconclusive"
        {
            abstentions += 1;
        }
        for (suffix, key) in [("A", "requiredCheckFailedA"), ("B", "requiredCheckFailedB")] {
            let outcome = format!("outcome{suffix}");
            if prediction[&outcome] == "passed" && gold[&outcome] == "failed" {
                false_positives += 1;
            }
            if case[key] == true && prediction[&outcome] == "passed" {
                unsafe_passes += 1;
            }
        }
    }
    let agreement = if decisive == 0 {
        0.0
    } else {
        agreements as f64 / decisive as f64
    };
    Ok(
        json!({"schemaVersion":1,"corpusHash":hash,"cases":40,"decisiveCases":decisive,"agreements":agreements,"agreement":agreement,
        "abstentions":abstentions,"falsePositives":false_positives,"requiredCheckViolations":unsafe_passes,"reviewerDisagreements":disagreements,
        "reviewers":reviewers,"passed":decisive>0 && agreement>=0.90 && unsafe_passes==0,
        "claim":"Thresholds apply only to this reviewed corpus; they are not universal accuracy or causal claims."}),
    )
}
