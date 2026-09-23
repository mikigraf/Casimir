# Frozen calibration corpus

`corpus.json` is a separate synthetic, **unreviewed** 40-pair candidate corpus. Its byte hash in
`corpus.sha256` freezes the inputs for review. It is not a passed evaluation gate and is not a
claim that these examples represent all coding work. Do not tune prompts against adjudicated
labels and then reuse this same corpus as independent validation.

Two humans independently review all pairs, without seeing each other's labels or model
predictions. Give reviewers only the case ID, task, traces and check evidence; hide
`samplingStratum`, which describes corpus construction rather than ground truth. Reviewers
must determine outcomes from the actual evidence. An adjudicator resolves disagreements and
records the final labels. Mark ambiguous or insufficient evidence inconclusive.

Each review/prediction file has this structure, with all 40 case IDs:

```json
{
  "schemaVersion": 1,
  "corpusHash": "SHA256_OF_EXACT_CORPUS_BYTES",
  "reviewerId": "independent-human-identity",
  "humanReviewed": true,
  "labels": {
    "pair-01": {"winner":"tie", "outcomeA":"passed", "outcomeB":"passed"}
  }
}
```

Predictions use the same label schema but need no human attestation. `winner` accepts A, B,
tie or inconclusive; outcomes accept passed, failed or inconclusive. Do not populate human
review files from generated expectations. Preserve each review and adjudication separately.

Run `casimir calibrate --corpus corpus.json --predictions predictions.json --reviewer-a a.json
--reviewer-b b.json --adjudication adjudicated.json -o calibration.json`. At least 90% of
adjudicated decisive cases must agree on the winner and both outcomes. Abstentions count as
non-agreement on decisive cases. The report publishes abstentions, false positives, reviewer
disagreements and required-check violations. A required-check failure labeled passed blocks
the gate regardless of aggregate agreement. Human review is still an external prerequisite.

Simulator and attribution acceptance must additionally use the maintained cases in
`review-cases.json`, with independently recorded human decisions. Their mere presence is
not validation.
