Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: redshift__sqs.cfn-deploy-redshift-sqs-queue-policy-negative redshift__sqs.sqs-dead-letter-queue-captures-failed-eventbridge-delivery-positive
Issue-Key: redshift__sqs.cfn-deploy-redshift-sqs-queue-policy-negative|run#13|AssertionError|assert error_code == "NO_PERMISSIONS", f"unexpected DLQ error code: {error_code!r}"
Issue-Key: redshift__sqs.sqs-dead-letter-queue-captures-failed-eventbridge-delivery-positive|run#17|AssertionError|assert error_code == "NO_PERMISSIONS", (
Issue-Commit: 852fb652ec4af2dc3d2dc139523083e06c8afdc9

# redshift__sqs: EventBridge dead-letters a delivery to an SQS target the queue policy does not authorize as ERROR_CODE='NO_RESOURCE' instead of 'NO_PERMISSIONS' (1 net-new cause, 2 failing checks)

> _Note: this issue was identified against the code base at commit `852fb652ec4af2dc3d2dc139523083e06c8afdc9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutEvents

## Description
One net-new root cause, grounded in two failing checks the suite lists under `redshift__sqs`. Both must be re-replayed before this issue is submitted.

Cause 1 of 1: when an EventBridge rule's SQS target is an EXISTING queue whose resource policy does not authorize the sending rule (either no policy at all, or an Allow for events.amazonaws.com whose Condition ArnEquals aws:SourceArn names a DIFFERENT rule's ARN), the dead-letter copy EventBridge writes to that target's DeadLetterConfig queue is stamped with the SQS message attribute ERROR_CODE='NO_RESOURCE'. The recorded behavior for this class is the fixed token ERROR_CODE='NO_PERMISSIONS'. Failing checks: redshift__sqs.cfn-deploy-redshift-sqs-queue-policy-negative (assertion at check line 55/55-ish: `assert error_code == "NO_PERMISSIONS"`) and redshift__sqs.sqs-dead-letter-queue-captures-failed-eventbridge-delivery-positive (assertion at check line 61).

AWS: recorded behavior only for the attribute values — EventBridge's dead-letter message attributes are not modeled. Smithy (eventbridge): DeadLetterConfig is documented only as "Configuration details of the Amazon SQS queue for EventBridge to use as a dead-letter queue (DLQ)"; no shape enumerates ERROR_CODE's token set, so the tokens come from the recordings. Both recordings behind these two checks show the same thing: the dead letter arrives on the DeadLetterConfig queue with RULE_ARN and TARGET_ARN naming the rule and the unauthorized target of the failed delivery (values taken from state), a Body that is the original event verbatim (same `source`, `detail-type` and `detail`, so the token/clusterIdentifier in `detail` round-trips), and ERROR_CODE='NO_PERMISSIONS' — a fixed enumerated token with nothing in it taken from the request. The pairing rule the recordings follow is that ERROR_CODE and the sibling ERROR_MESSAGE name the SAME class: 'NO_PERMISSIONS' goes with the authorization-refusal sentence (HTTP 403, error code AccessDenied) and 'NO_RESOURCE' goes with SQS's own not-found refusal, 'The specified queue does not exist or you do not have access to it. (Service: AmazonSQS; Status Code: 400; Error Code: AWS.SimpleQueueService.NonExistentQueue; Request ID: <id>; Proxy: null)' — all fixed text, with <id> the only per-call part and therefore not part of the requirement. Smithy (sqs) corroborates which refusal is which: SendMessage models InvalidAddress(404), InvalidMessageContents(400), InvalidSecurity(403), the KMS.* family, QueueDoesNotExist(400, awsQueryError AWS.SimpleQueueService.NonExistentQueue), RequestThrottled(403) and UnsupportedOperation(400) — the not-found class is 400/AWS.SimpleQueueService.NonExistentQueue, and no AccessDenied is modeled on SendMessage at all.

Emulator: the dead letter does reach the DLQ and its Body and RULE_ARN match (both checks got past those assertions — the second asserts RULE_ARN == the rule ARN immediately before failing), but the ERROR_CODE attribute read back by ReceiveMessage(MessageAttributeNames=['All']) is the string 'NO_RESOURCE' in both scenarios. The emulator therefore classifies an authorization failure against a queue that demonstrably exists as a missing-resource failure: in the second check the same queue URL receives the next event successfully as soon as SetQueueAttributes grants the policy, and in the first check the queue was created by the same CloudFormation stack and is readable by ReceiveMessage throughout. Neither check asserts ERROR_MESSAGE, so this run does not establish which sentence the emulator pairs with the code; if it is the 403 AccessDenied sentence, the code and message name two different classes, which is unproducible by AWS under either class.

Why: ERROR_CODE is the only field that tells an operator or a redrive tool why a delivery failed. A client branching on ERROR_CODE='NO_PERMISSIONS' to point at the missing AWS::SQS::QueuePolicy or at a SourceArn condition scoped to the wrong rule — the commonest EventBridge→SQS misconfiguration, and exactly the fault these two scenarios inject — never fires. A client branching on 'NO_RESOURCE' to delete a stale target, stop re-driving, or page about a destroyed queue acts destructively on a queue that exists and is one SetQueueAttributes away from working.

Fix: the EventBridge target-delivery failure classifier that fills the dead-letter ERROR_CODE/ERROR_MESSAGE message attributes, together with the SQS SendMessage authorization refusal it consumes — the two attributes must be derived from one refusal so they can never name different classes, and an authorization denial on an existing queue must not be reported as a missing resource. CAUTION for the fixer: this classifier is contested across the corpus and must NOT be flipped globally. Open issue 20261115T153528Z-elbv2-events-sqs-an-eventbridge-sqs-target-with.md reports the same NO_RESOURCE-for-an-unauthorized-queue direction from an elbv2 CloudTrail scenario (it does not carry either of these two checks' replay keys), while open issue 20261123T070333Z-events-3-net-new-causes-an-unauthorized-sqs.md and open issue 20261115T165311Z-events-the-eventbridge-sqs-target-dead-letter-error.md record the OPPOSITE requirement (NO_RESOURCE + SQS's 400 NonExistentQueue sentence) for other `events` checks. None of those cover these two checks. The discriminator must be which refusal the target service actually returns for the send, not a hard-coded token.

Traces: redshift__sqs.cfn-deploy-redshift-sqs-queue-policy-negative; redshift__sqs.sqs-dead-letter-queue-captures-failed-eventbridge-delivery-positive.

## Evidence
Check 1 — redshift__sqs.cfn-deploy-redshift-sqs-queue-policy-negative (CloudFormation-deployed stack: QueueGood/QueueBad/Dlq + RuleGood/RuleBad, QueueBadPolicy deliberately scoped with Condition ArnEquals aws:SourceArn = RuleGood's ARN instead of RuleBad's):
  assert error_code == "NO_PERMISSIONS", f"unexpected DLQ error code: {error_code!r}"
  AssertionError: unexpected DLQ error code: 'NO_RESOURCE'
Everything up to that point matched: CreateStack reached CREATE_COMPLETE, PutEvents returned FailedEntryCount == 0, and exactly one dead letter carrying the original event (detail.requestParameters.clusterIdentifier == the bad cluster id) arrived on the Dlq.

Check 2 — redshift__sqs.sqs-dead-letter-queue-captures-failed-eventbridge-delivery-positive (hand-built: target queue intentionally left with NO policy, DLQ policy granted for the rule, target put with DeadLetterConfig + RetryPolicy MaximumRetryAttempts=0):
  assert error_code == "NO_PERMISSIONS", (
  AssertionError: expected ERROR_CODE=NO_PERMISSIONS (missing SQS resource policy), got 'NO_RESOURCE'
The two assertions immediately before it matched: the DLQ body carried the per-run token from the PutEvents Detail, and the RULE_ARN attribute equaled the PutRule RuleArn.

Expected vs actual, per dead-letter message attribute (ReceiveMessage with MessageAttributeNames=['All']):
  expected ERROR_CODE = "NO_PERMISSIONS"  — fixed enumerated token; no part of it comes from the request or state
  actual   ERROR_CODE = "NO_RESOURCE"
  RULE_ARN / TARGET_ARN / Body: matched (per-run ARNs and tokens; the requirement is only that they name the rule and the unauthorized target of the failed delivery and reproduce the original event).
  ERROR_MESSAGE: not asserted by either check, so not observed in this run; whatever it is, it must name the same class as ERROR_CODE. The 'NO_RESOURCE' class message is 'The specified queue does not exist or you do not have access to it. (Service: AmazonSQS; Status Code: 400; Error Code: AWS.SimpleQueueService.NonExistentQueue; Request ID: <id>; Proxy: null)' (all fixed text, <id> per-call).

Smithy (sqs) SendMessage modeled errors: InvalidAddress(404), InvalidMessageContents(400), InvalidSecurity(403), KMS.AccessDeniedException/DisabledException/InvalidKeyUsageException/InvalidStateException/NotFoundException/OptInRequired/ThrottlingException, QueueDoesNotExist(400, awsQueryError AWS.SimpleQueueService.NonExistentQueue), RequestThrottled(403), UnsupportedOperation(400). No AccessDenied is modeled on SendMessage.
Smithy (eventbridge) DeadLetterConfig: documented only as "Configuration details of the Amazon SQS queue for EventBridge to use as a dead-letter queue (DLQ)" — the dead-letter attribute names and tokens are recorded behavior only.

Proof the queue exists in both scenarios (so 'NO_RESOURCE' is the wrong class): check 2 grants the target queue the identical policy with SetQueueAttributes and the very next PutEvents is delivered to that same queue URL; check 1's QueueBad is created by the stack and is polled successfully by ReceiveMessage (it correctly holds nothing).

## Steps to Reproduce
Scenario A (check 2, minimal):
1. CreateQueue twice: a DLQ and a target queue. SetQueueAttributes on the DLQ only, with Policy {Version 2012-10-17, Statement[{Sid AllowEventBridgeRule, Effect Allow, Principal {Service: events.amazonaws.com}, Action sqs:SendMessage, Resource <dlq arn>, Condition {ArnEquals {aws:SourceArn: <rule arn>}}}]}. Leave the TARGET queue with no policy at all.
2. PutRule with EventPattern {"source":["sim2real.dlqtest"],"detail-type":["DLQ Capture Test"]}, State ENABLED.
3. PutTargets on that rule: {Id:"1", Arn: <target queue arn>, DeadLetterConfig {Arn: <dlq arn>}, RetryPolicy {MaximumRetryAttempts: 0, MaximumEventAgeInSeconds: 60}}.
4. PutEvents one entry {Source: "sim2real.dlqtest", DetailType: "DLQ Capture Test", Detail: {"token": "<unique>"}}.
5. ReceiveMessage on the DLQ with MessageAttributeNames=['All'].
   Expected: one dead letter whose Body is the original event (detail.token == the unique token), RULE_ARN == the rule ARN, TARGET_ARN == the target queue ARN, and ERROR_CODE == 'NO_PERMISSIONS' with an ERROR_MESSAGE in that same (403 / AccessDenied authorization-refusal) class.
   Emulator: ERROR_CODE == 'NO_RESOURCE'.
6. ReceiveMessage on the policy-less target queue — it must hold nothing.
7. SetQueueAttributes to grant the target queue the same events.amazonaws.com sqs:SendMessage policy for this rule, PutEvents an identical-shaped event with a new token, and ReceiveMessage the target — it must now arrive, proving the queue existed all along and only the permission suppressed delivery (so the dead letter's class had to be NO_PERMISSIONS, not NO_RESOURCE).

Scenario B (check 1, via CloudFormation):
1. CreateStack with a template holding three AWS::SQS::Queue resources (QueueGood, QueueBad, Dlq), two AWS::Events::Rule resources whose EventPatterns differ only in detail.requestParameters.clusterIdentifier (RuleGood → QueueGood; RuleBad → QueueBad with DeadLetterConfig {Arn: Fn::GetAtt Dlq.Arn}), and three AWS::SQS::QueuePolicy resources. Scope QueueBadPolicy's Condition ArnEquals aws:SourceArn to RuleGood's ARN (the wrong rule) while DlqPolicy is scoped to RuleBad's ARN. Outputs: QueueGoodUrl, QueueBadUrl, DlqUrl. Wait for CREATE_COMPLETE.
2. PutEvents one entry {Source: "sim2real.redshift", DetailType: "AWS API Call via CloudTrail", Detail: {eventSource: "redshift.amazonaws.com", eventName: "PauseCluster", requestParameters: {clusterIdentifier: <bad cluster>}}}; FailedEntryCount must be 0.
3. ReceiveMessage on DlqUrl with MessageAttributeNames=['All'].
   Expected: exactly one dead letter for the bad cluster with ERROR_CODE == 'NO_PERMISSIONS'.
   Emulator: ERROR_CODE == 'NO_RESOURCE'.
4. ReceiveMessage on QueueBadUrl — nothing may have reached it; then PutEvents the same envelope for the good cluster and confirm QueueGoodUrl receives it (the correctly scoped pair in the same stack still delivers).
