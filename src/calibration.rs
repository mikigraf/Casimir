//! Offline scoring against a frozen, independently reviewed corpus. No labels are invented.
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::path::Path;

pub fn score(corpus: &Path, predictions: &Path, reviewer_a: &Path, reviewer_b: &Path, adjudication: &Path) -> Result<Value> {
    let bytes = std::fs::read(corpus)?;
    let hash = crate::checkpoint::hash(&bytes);
    let corpus: Value = serde_json::from_slice(&bytes)?;
    if corpus["schemaVersion"] != 1 || corpus["frozen"] != true { bail!("calibration requires a frozen schemaVersion 1 corpus"); }
    let cases = corpus["cases"].as_array().context("corpus cases missing")?;
    if cases.len() != 40 { bail!("release calibration requires exactly 40 frozen trace pairs"); }
    let ids: std::collections::BTreeSet<_> = cases.iter().filter_map(|c| c["id"].as_str()).collect();
    if ids.len() != 40 { bail!("case IDs must be present and unique"); }
    let documents: Vec<Value> = [predictions,reviewer_a,reviewer_b,adjudication].iter().map(|p| crate::util::read_json(p)).collect::<Result<_>>()?;
    for document in &documents {
        if document["schemaVersion"] != 1 || document["corpusHash"].as_str() != Some(&hash) { bail!("review/prediction does not match the frozen corpus hash"); }
        let labels = document["labels"].as_object().context("labels missing")?;
        if labels.len() != 40 || labels.keys().any(|id| !ids.contains(id.as_str())) { bail!("every document must cover exactly the frozen 40 case IDs"); }
        for label in labels.values() {
            if !matches!(label["winner"].as_str(), Some("A"|"B"|"tie"|"inconclusive"))
                || !matches!(label["outcomeA"].as_str(),Some("passed"|"failed"|"inconclusive"))
                || !matches!(label["outcomeB"].as_str(),Some("passed"|"failed"|"inconclusive")) { bail!("incomplete or invalid calibration label"); }
        }
    }
    let reviewers: Vec<&str> = documents[1..].iter().map(|d| d["reviewerId"].as_str().filter(|s| !s.trim().is_empty()).context("reviewer/adjudicator identity missing")).collect::<Result<_>>()?;
    if reviewers[0] == reviewers[1] { bail!("two independent reviewer identities are required"); }
    if documents[1..].iter().any(|d| d["humanReviewed"] != true) { bail!("human review attestations are required; fixture labels cannot substitute"); }
    let mut decisive = 0; let mut agreements = 0; let mut abstentions = 0; let mut false_positives = 0; let mut unsafe_passes = 0; let mut disagreements = 0;
    for case in cases {
        let id=case["id"].as_str().unwrap();
        let prediction=&documents[0]["labels"][id]; let gold=&documents[3]["labels"][id];
        if documents[1]["labels"][id] != documents[2]["labels"][id] { disagreements+=1; }
        let decisive_gold=gold["winner"]!="inconclusive" && gold["outcomeA"]!="inconclusive" && gold["outcomeB"]!="inconclusive";
        if decisive_gold { decisive+=1; if ["winner","outcomeA","outcomeB"].iter().all(|k|prediction[k]==gold[k]) {agreements+=1;} }
        if prediction["winner"]=="inconclusive" || prediction["outcomeA"]=="inconclusive" || prediction["outcomeB"]=="inconclusive" {abstentions+=1;}
        for (suffix, key) in [("A","requiredCheckFailedA"),("B","requiredCheckFailedB")] {
            let outcome=format!("outcome{suffix}");
            if prediction[&outcome]=="passed" && gold[&outcome]=="failed" {false_positives+=1;}
            if case[key]==true && prediction[&outcome]=="passed" {unsafe_passes+=1;}
        }
    }
    let agreement=if decisive==0 {0.0} else {agreements as f64 / decisive as f64};
    Ok(json!({"schemaVersion":1,"corpusHash":hash,"cases":40,"decisiveCases":decisive,"agreements":agreements,"agreement":agreement,
        "abstentions":abstentions,"falsePositives":false_positives,"requiredCheckViolations":unsafe_passes,"reviewerDisagreements":disagreements,
        "reviewers":reviewers,"passed":decisive>0 && agreement>=0.90 && unsafe_passes==0,
        "claim":"Thresholds apply only to this reviewed corpus; they are not universal accuracy or causal claims."}))
}
