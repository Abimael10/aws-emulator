Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch__ecr__ecs.ecr-scan-finding-without-rule-target-not-delivered
Issue-Key: cloudwatch__ecr__ecs.ecr-scan-finding-without-rule-target-not-delivered|_poll_for_event#15|Timeout|resp = sqs.receive_message(QueueUrl=queue_url, MaxNumberOfMessages=10, WaitTimeSeconds=remaining)
Issue-Commit: 4cf7211f237a5c930178a3131e9243c3c3eb5dd6

# ECR scan-on-push emits no "ECR Image Scan" EventBridge event, so the no-target / with-target delivery differential never reaches a verdict (check cut off at its 60 s cap)

> _Note: this issue was identified against the code base at commit `4cf7211f237a5c930178a3131e9243c3c3eb5dd6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutImage

## Description
Cause 1 of 1: pushing an image to a repository with scanOnPush enabled never results in an "ECR Image Scan" event on the default event bus, so the check that proves a targetless EventBridge rule delivers nothing and the SAME rule delivers once a target is wired cannot complete — it only ever exhausts its clock. Failing check: cloudwatch__ecr__ecs.ecr-scan-finding-without-rule-target-not-delivered.

AWS: with imageScanningConfiguration.scanOnPush=true, PutImage starts a basic scan, and when the scan reaches a terminal status ECR publishes an event to the account's default event bus with source `aws.ecr`, detail-type `ECR Image Scan`, and a detail carrying `repository-name`, `image-digest` and `scan-status`. A rule whose EventPattern matches it but which has no targets delivers nothing; after PutTargets wires an SQS queue (and the queue policy allows events.amazonaws.com for that rule ARN), the next matching scan's event is delivered to that queue. Recorded behavior only for the terminal status of the synthetic single-layer image this check pushes through the raw layer-upload API: the recording shows `scan-status` FAILED, because such an image has no analyzable filesystem. Nothing here is expressible as a Smithy constraint — the event shape and the delivery rule are not in the API model.

Emulator: the check never reaches any assertion verdict — the replay reports only `Check timed out after 60.0s`. Its absence leg polls the queue for 200 s and its positive-control leg for 240 s, and neither produced a failed assertion before the cap, so no `ECR Image Scan` delivery was ever observed in either leg. Hypothesis (the only attribution the recording supports): ECR's scan-on-push path publishes no `ECR Image Scan` event at all, so the positive control can never satisfy its predicate and the check can only ever run out its budget; the alternative — an event delivered despite the rule having no target — would have failed fast with the negative leg's assertion instead of timing out.

Why: a client that drives image-scan automation off EventBridge (quarantine a repository, open a ticket, gate a deploy on scan-status) gets no event and no error — the pipeline silently never fires. The targetless-rule guarantee, the queue-policy condition and the scan-status in the detail are all unobservable, so neither the positive nor the negative behavior can be relied on.

Fix: ECR's scan-on-push completion path — when a scan started by PutImage reaches a terminal status, publish the `aws.ecr` / `ECR Image Scan` event (with repository-name, image-digest and scan-status in its detail) to the default event bus so EventBridge rule matching and target delivery apply to it.

Traces: cloudwatch__ecr__ecs.ecr-scan-finding-without-rule-target-not-delivered (both legs: the targetless-rule absence poll and the with-target positive control).

## Evidence
Expected: after PutImage into a scanOnPush repository, an event with detail-type `ECR Image Scan`, detail.repository-name = the repository and detail.image-digest = the pushed digest reaches the SQS queue once (and only once) the matching rule has a target; with no target attached, nothing is delivered. For the synthetic image this check pushes, detail.scan-status is `FAILED`.

Actual (replayed check):
  Error: Check timed out after 60.0s
No assertion in the check failed — neither `scan-complete event delivered to the queue even though the rule has no target` nor `scan-complete event never delivered after wiring the target` was reached, so no delivery was observed in either direction before the budget expired. The 60.0 s figure is this run's cap, not a requirement; the requirement is that the scan event is published and delivered at all.

## Steps to Reproduce
1. CreateRepository with imageScanningConfiguration={"scanOnPush": true}.
2. CreateQueue, read its QueueArn, and PutRule with EventPattern {"source":["aws.ecr"],"detail-type":["ECR Image Scan"],"detail":{"repository-name":[<repo>]}}, State ENABLED. Confirm ListTargetsByRule returns [].
3. Push a minimal single-layer image via InitiateLayerUpload / UploadLayerPart / CompleteLayerUpload / PutImage. Poll the queue: nothing should arrive (correct).
4. SetQueueAttributes with a policy allowing events.amazonaws.com sqs:SendMessage for the rule ARN, then PutTargets wiring the queue to the SAME rule.
5. Push a second, distinct image and poll the queue for an `ECR Image Scan` event whose detail.image-digest is the new digest. Observe that no event ever arrives, so the check can only run to its cap.

## Resolution

The cause as stated — "pushing an image to a repository with scanOnPush enabled never results
in an `ECR Image Scan` event on the default event bus" — is not what this code base does. The
scan-on-push completion path is `state/scan_events.rs`: a push into a repository that scanning
is configured for on push starts the same basic scan `StartImageScan` starts, and `PutImage`
arms the publication of that run's completion, which is published once onto the default event
bus as source `aws.ecr` / detail-type `ECR Image Scan`, read back out of the stored run. It
landed with `issues/things-that-now-work/20261029T174654Z-cloudwatch-ecr-ecs-an-ecr-image-push-and.md`,
after this check's key had already been filed against it once.

What this change adds is the coverage of the two axes the recorded check drives that no suite
reached, over the whole assembled emulator:
`core/router/tests/all/ecr_scan_on_push_event_reaches_a_rule_scoped_queue_target.rs` pushes its
images through the raw layer-upload sequence (`InitiateLayerUpload` -> `UploadLayerPart` ->
`CompleteLayerUpload` -> `PutImage`), so the scanned image is a single-layer one basic scanning
cannot examine and the run ends `FAILED` as the recording shows; and it decides the delivery
against a queue grant SCOPED to the invoking rule (`aws:SourceArn`, a pattern covering it,
`aws:SourceAccount`, and — the boundary — another rule's ARN), with the rule first holding no
target and then holding one. Its second test drives the same row with every signature verified,
because the announcement is published from a task of the registry's that outlives the push that
armed it.

REMAINING DIVERGENCE, unchanged from the earlier resolution: the recorded unit still ends
"Check timed out after 60.0s" and no wire call of it diverges. The check's own absence leg polls
the queue for a span longer than the replay harness's whole budget, so it is cut off inside its
`sqs.receive_message` before either of its assertions is reached — which is what the replay
reports — whatever the emulator answers. The behaviour the check would assert is the one the two
router suites and the ECR crate's own `scan_on_push.rs` hold the emulator to.

