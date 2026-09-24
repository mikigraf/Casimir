# Evaluation corpus

This directory holds the corpus used to check Casimir's judge against human reviewers.

`corpus.json` has 40 synthetic candidate pairs that **have not been reviewed yet**. Its hash in
`corpus.sha256` freezes the inputs so reviewers all see the same thing. The corpus doesn't pass
the evaluation gate on its own, and it isn't meant to represent all coding work. Don't tune
prompts against the adjudicated labels and then reuse this corpus as independent validation.

## Human review

Two people review all 40 pairs independently. They shouldn't see each other's labels or the
model's predictions.

- Give reviewers only the case ID, the task, the traces and the check evidence.
- Hide `samplingStratum`. It describes how the corpus was built, not the right answer.
- Reviewers decide outcomes from the evidence. If it's ambiguous or insufficient, the answer is
  inconclusive.
- An adjudicator settles disagreements and records the final labels.

Each review file (and the prediction file) covers all 40 case IDs in this shape:

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

`winner` is `A`, `B`, `tie` or `inconclusive`. Outcomes are `passed`, `failed` or
`inconclusive`. Predictions use the same format but don't need a human attestation.

Never fill in human review files from generated expectations, and keep each review and the
adjudication in separate files.

## Generating predictions

Predictions come from the same AB/BA judge that Casimir uses in production:

```sh
casimir predict-evaluation --corpus acceptance/evaluation/corpus.json -o /private/predictions --judge-model MODEL --llm claude-cli
```

This makes 80 judge calls (up to 160 if JSON repair retries kick in). Try
`predict-evaluation --limit 1` first as a paid smoke test. The runner:

- keeps the construction strata and human labels out of the model's input;
- freezes the corpus bytes and keeps the evidence for each case;
- marks predictions `humanReviewed: false`;
- reports model or transport failures as inconclusive, and never turns a failed required check
  into a pass.

Use `predictions/predictions.json` for scoring. Generated predictions can't stand in for human
review, and a partial set of predictions can't pass the 40-case gate.

## Scoring

```sh
casimir calibrate --corpus corpus.json --predictions predictions.json \
    --reviewer-a a.json --reviewer-b b.json --adjudication adjudicated.json -o calibration.json
```

To pass, at least 90% of the adjudicated decisive cases must match on the winner and both
outcomes. An abstention on a decisive case counts as a mismatch. The report lists abstentions,
false positives, reviewer disagreements and required-check violations. If any case with a failed
required check is labelled as passed, the gate fails no matter how good the overall agreement
is.

Human review still has to happen outside this repository.

## Simulator and attribution cases

Simulator and attribution acceptance also use the cases in `review-cases.json`, with human
decisions recorded independently. Having the file here doesn't validate anything by itself.

## Corpus revisions

Revision 2 replaced placeholder test comments with executable check definitions and their
captured output. The prediction runner gives the judge the recorded final file snapshots and the
external check source, without pretending they're Git commits or agent trajectories. Snapshots
that are intentionally missing stay missing.

`scripts/refresh-evaluation-corpus.py --refresh` is a maintainer tool. It changes the frozen
hash, which invalidates every existing prediction and review, so never run it quietly against a
corpus that has already been reviewed for a release.
