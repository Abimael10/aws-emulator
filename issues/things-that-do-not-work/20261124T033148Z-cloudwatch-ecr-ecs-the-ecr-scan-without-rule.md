Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch__ecr__ecs.ecr-scan-finding-without-rule-target-not-delivered
Issue-Key: cloudwatch__ecr__ecs.ecr-scan-finding-without-rule-target-not-delivered|_poll_for_event#15|Timeout|resp = sqs.receive_message(QueueUrl=queue_url, MaxNumberOfMessages=10, WaitTimeSeconds=remaining)
Issue-Commit: 02e30cec75724d494cfd983590fb8cff440247b8

# cloudwatch__ecr__ecs: the ECR-scan-without-rule-target delivery scenario reaches no verdict — the check is cut at the 60 s cap before the scan event is ever observable

> _Note: this issue was identified against the code base at commit `02e30cec75724d494cfd983590fb8cff440247b8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutRule

## Description
One net-new root cause under the `### cloudwatch__ecr__ecs` heading (Cause 1 of 1). Five checks fail under that heading; four are marked OWNED by open filings and are skipped — this id is the survivor, and no open issue carries its replay key.

Cause 1 of 1 — the negative/positive delivery pair for an ECR image-scan event never reaches a verdict. Failing check: cloudwatch__ecr__ecs.ecr-scan-finding-without-rule-target-not-delivered.
AWS: recorded behavior only (no model statement covers event delivery). The scenario is: create an ECR repository with imageScanningConfiguration.scanOnPush = true; create an SQS queue; events PutRule(State='ENABLED') with pattern {'source': ['aws.ecr'], 'detail-type': ['ECR Image Scan'], 'detail': {'repository-name': [<repo>]}}; ListTargetsByRule must report [] for the fresh rule; push an image, and NO 'ECR Image Scan' event for that image digest may arrive at the queue because the matching rule has no target; then wire the SAME rule to the queue, push a second image, and the scan-complete event for the new digest MUST arrive. The requirement is the two-sided outcome (absence while untargeted, delivery once targeted), not any timing of one run.
Emulator: the check produced 'Check timed out after 60.0s' — no assertion was reached, no response, status or message was observed, so neither the absence leg nor the positive control was graded. The check's own absence poll alone is budgeted at 200 s, so the suite's per-check cap may be the limiting factor rather than the emulator; it is filed because it is the only surviving failure under this heading and the same budget-exhaustion shape has been filed before for other services (20261116T122519Z-codebuild-ecr-ecs-elbv2-logs-both-container-ci.md, 20261120T073735Z-lambda-rds-secretsmanager-the-iam-auth-rds-proxy.md). Hypothesis: the image push and the scanOnPush finding do not settle quickly enough for the first leg to complete inside the cap.
Why: EventBridge's most important safety property here — that a matching rule with no target delivers nothing, and that attaching a target makes the identical event arrive — is ungraded for the ECR scan source, so a divergence in either direction (phantom delivery to an untargeted rule, or no delivery at all once targeted) is invisible.
Fix: whichever leg consumes the budget — ECR's push/scanOnPush finding pipeline and the aws.ecr 'ECR Image Scan' event emission into EventBridge, plus the rule-with-no-target delivery decision; confirm first by timing the create-repository/push/scan legs alone.
Traces: cloudwatch__ecr__ecs.ecr-scan-finding-without-rule-target-not-delivered.

## Evidence
Replay of suite simparity-behavioral-aws-v1 against the booted emulator. Service heading `### cloudwatch__ecr__ecs` 17/27 (63%), 5 failed — 4 marked OWNED and skipped; this id survives.

[mismatch] cloudwatch__ecr__ecs.ecr-scan-finding-without-rule-target-not-delivered
  Error: Check timed out after 60.0s

Nothing from the emulator was observed: the first graded step (ListTargetsByRule on the fresh rule returning []) and the absence poll (_poll_for_event(..., timeout_s=200) for detail-type 'ECR Image Scan' with the pushed image's digest) were never reported. The check creates the repository with imageScanningConfiguration={'scanOnPush': True}, creates the queue, PutRule with the aws.ecr/'ECR Image Scan' pattern bound to the repository name, pushes a minimal image, asserts no delivery, then attaches the queue target (with a queue policy allowing events.amazonaws.com) and pushes a second image expecting delivery.

Expected: both legs graded — absence for the untargeted rule, delivery of the scan-complete event for the second digest once the target is attached.
Actual: no verdict; the run was ended by the 60 s per-check cap.

## Steps to Reproduce
1. Replay cloudwatch__ecr__ecs.ecr-scan-finding-without-rule-target-not-delivered; it is cut at 60 s with no assertion.
2. Time the setup legs individually against the booted emulator: ecr CreateRepository(imageScanningConfiguration={'scanOnPush': True}); sqs CreateQueue; events PutRule(Name=<rule>, EventPattern={'source':['aws.ecr'],'detail-type':['ECR Image Scan'],'detail':{'repository-name':[<repo>]}}, State='ENABLED'); events ListTargetsByRule(Rule=<rule>) must return Targets == [].
3. Push a minimal image to the repository and poll the queue: no message whose body has detail-type 'ECR Image Scan' and detail.image-digest == the pushed digest may arrive.
4. events PutTargets attaching the queue to the SAME rule (with an SQS policy allowing events.amazonaws.com Publish/SendMessage), push a second image, and poll again: the scan-complete event for the new digest MUST arrive.
5. Re-replay the id to confirm the scenario reaches a verdict inside the suite's cap.
