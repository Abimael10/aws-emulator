Issue-Source: trace-sweep-divergence
Issue-Trace: batch-scheduling-policy-standalone-lifecycle
Issue-Step: task#9
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: batch-scheduling-policy-standalone-lifecycle
Issue-Shape-Id: com.amazonaws.batch#AWSBatchV20160810
Issue-Key: batch|DescribeSchedulingPolicies|4xx->4xx|$.message
Issue-Commit: 2f6ea84c563fd33f00bbd79776ed9e614362531a

# batch DescribeSchedulingPolicies malformed-ARN ClientException message is still emulator-authored prose

> _Note: this issue was identified against the code base at commit `2f6ea84c563fd33f00bbd79776ed9e614362531a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeSchedulingPolicies

## Description
One net-new root cause on the AWS Batch scheduling-policy read plane. It is the ONLY divergence cluster in this replay whose owned_by is EMPTY; every other cluster in the report is owned by an open issue that already carries its replay key (20261102T113649Z-batch-job-api-refusal-contracts-diverge-two-wrong.md, 20261109T161717Z-batch-3-net-new-causes-tag-untag-204.md, 20261029T234420Z-batch-refusal-contracts-diverge-not-found-arn-wording.md, 20261115T005730Z-batch-describejobdefinitions-omits-networkconfiguration-members-aws-returns-for.md) or is an unimplemented-operation stub whose issue is filed already, and none of those is re-filed here. Three issues exist on THIS exact step and all three are RESOLVED (20261030T031148Z-batch-describeschedulingpolicies-malformed-arn-refusal-message-is-emulator.md, 20261030T204009Z-batch-describeschedulingpolicies-malformed-arn-refusal-is-not-wrapped.md, 20261113T130550Z-batch-describeschedulingpolicies-malformed-arn-refusal-message-still-diverges.md, all listed [fixed]); a resolved issue never covers a failure that still replays, and none of them carries this step's replay key any more, so this is filed net-new. The prior fixes clearly landed in part - the message now carries Batch's request envelope and no longer interpolates the rejected ARN string into the sentence - but the message as a whole still fails the recording's assertion, so this operation's refusal contract is still not the recorded one.

Cause 1 of 1: DescribeSchedulingPolicies' refusal for a malformed scheduling-policy ARN in `arns` carries a message whose text is not the one AWS sends for that refusal class.

AWS: Smithy (services/service-com-amazonaws-batch-awsbatchv20160810/smithy-model/com-amazonaws-batch-awsbatchv20160810.json) models DescribeSchedulingPolicies as POST /v1/describeschedulingpolicies, success code 200; input member `arns` -> StringList (@required, clientOptional); output `schedulingPolicies` -> SchedulingPolicyDetailList; errors exactly ClientException (errorKind client, httpError 400) and ServerException (500). The model states no message text, so the wording is recorded behavior only. The recording refuses an `arns` entry that is not a well-formed scheduling-policy ARN with HTTP 400 / ClientException, and the comparator asserts that refusal's message by SHAPE rather than literally (the replay reports expected '<shaped-not-asserted>') because the recorded text ends in a per-call request id. The recorded shape is Batch's input-validation envelope - fixed text "Error executing request, Exception : ", then the validator's own sentence, then fixed text ", RequestId: ", then this call's request id - whose only variable parts are values taken from the request (the offending member/value) and the per-call request id. The emulator's message still fails that shape assertion even though the envelope's fixed text is now present, so what diverges is the sentence the envelope wraps, not the envelope and not the per-run request id (a difference confined to a request id would satisfy the shape assertion and is explicitly NOT what is filed here).
Hypothesis: AWS's inner sentence for a malformed entry in `arns` is Batch's standard input-constraint sentence naming the member and the rejected value - the "1 validation error detected: Value '<rejected arns entry>' at 'arns' failed to satisfy constraint: ..." form, whose fixed text is the constraint sentence and whose variable parts are the member path 'arns' and the rejected value taken from the request - rather than a hand-written arn-pattern sentence. This is labelled Hypothesis because neither the Smithy model nor the replay detail states the recorded sentence; only the shape mismatch is asserted.

Emulator: HTTP 400 with error class ClientException (status and class are both correct) and $.message = "Error executing request, Exception : Scheduling policy arn should match a valid Scheduling policy arn pattern, RequestId: <uuid>". In that string the fixed text is "Error executing request, Exception : " + "Scheduling policy arn should match a valid Scheduling policy arn pattern" + ", RequestId: "; the only variable part is the per-call request id (this run's value is not the defect). Note that, unlike the state the last resolved issue recorded, the sentence no longer echoes the rejected `arns` entry at all, so the refusal now names neither the member nor the value it rejected.

Why: Batch answers every client mistake with the single ClientException shape, so the message text is the only machine-readable detail a caller gets. A client that distinguishes "the ARN I passed is syntactically wrong" from "this scheduling policy does not exist" or from an arity/quota refusal by matching AWS's sentence gets no match against the emulator, so a correctable input mistake falls into a generic error path; conversely a suite written against the emulator's sentence passes here and fails against real AWS. Because the sentence names neither the member nor the rejected value, a caller passing several ARNs cannot tell which entry was refused.

Fix: the inner refusal text produced by DescribeSchedulingPolicies' input validation in the batch crate - the arn-pattern branch of that operation's validation layer. The envelope/renderer it now goes through is correct and must be kept; only the wrapped sentence has to become the recorded one, and it must carry the member path and the rejected value from the request. The status and the error class already match and must not change.

Traces: batch-scheduling-policy-standalone-lifecycle#task#9.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout: emulator_up true ("/__admin/health answered 200 and the :33181 data listener accepts"), bootstrap_signal false, 21 fixtures replayed, 177 steps total, 157 passed, 19 mismatched, 1 errored, 0 blocked, score 88.7%, 12 divergence clusters, not_implemented 1.

This cluster, verbatim from the replay:
  divergence_key: "service=batch|DescribeSchedulingPolicies|status=400->400|keys=$.message"
  fixture_id: batch-scheduling-policy-standalone-lifecycle
  step_key: task#9
  operation: DescribeSchedulingPolicies
  service: batch
  verdict: mismatch
  expected_status 400, actual_status 400 (status and error class agree)
  affected_count: 1
  stub_path: EMPTY, owned_by: EMPTY (no open issue carries this replay key)
  detail: $.message: value_mismatch (expected '<shaped-not-asserted>', got 'Error executing request, Exception : Scheduling policy arn should match a valid Scheduling policy arn pattern, RequestId: 57f699d9-b511-4de5-b77b-ad7cdc7437de')

Reading of that detail: the expected side is a shape assertion, not a literal, because the recorded AWS message terminates in a per-call request id. The actual message carries the envelope fixed text ("Error executing request, Exception : " ... ", RequestId: ") that the earlier resolved issues asked for and STILL fails the shape assertion, so the residual divergence is in the sentence the envelope wraps. The uuid 57f699d9-b511-4de5-b77b-ad7cdc7437de is this run's request id and is not part of the requirement; a divergence confined to it would not be filed.

Smithy evidence (services/service-com-amazonaws-batch-awsbatchv20160810/smithy-model/com-amazonaws-batch-awsbatchv20160810.json), from smithy_query query="operation": DescribeSchedulingPolicies - http POST /v1/describeschedulingpolicies, code 200; input {arns: StringList, required, clientOptional}; output {schedulingPolicies: SchedulingPolicyDetailList}; errors ClientException (client, httpError 400) and ServerException (500). No message text is modeled, hence "recorded behavior only" for the wording.

Prior-art status: list_existing_issues(status="all", query="describeschedulingpolicies") returns exactly three issues and all three are [fixed]: 20261030T031148Z-..., 20261030T204009Z-... and 20261113T130550Z-.... No OPEN issue mentions this operation, and the replay reports owned_by EMPTY for this cluster.

## Steps to Reproduce
1. Clone this checkout, boot the emulator from it and replay the wire corpus simparity-wire-aws-v1.
2. Fixture batch-scheduling-policy-standalone-lifecycle, step task#9: POST /v1/describeschedulingpolicies with `arns` containing one entry that is not a well-formed scheduling-policy ARN. By hand: aws batch describe-scheduling-policies --arns not-a-valid-arn
3. Observed: HTTP 400, error class ClientException, $.message = "Error executing request, Exception : Scheduling policy arn should match a valid Scheduling policy arn pattern, RequestId: <uuid>" - reported by the replay as value_mismatch against the recording's shaped message assertion.
4. Expected: HTTP 400 / ClientException whose message is the recorded AWS text for a malformed `arns` entry, rendered through Batch's input-validation envelope ("Error executing request, Exception : " + AWS's constraint sentence naming the member 'arns' and the rejected value from the request + ", RequestId: " + this call's request id). The envelope is already right; the wrapped sentence is not.
5. Cross-check the sibling validators on this operation (arns count out of range; an `arns` entry that is well-formed but names no policy) so one client-side matcher handles every refusal this operation emits, as it does against AWS.
6. Re-replay to confirm: batch-scheduling-policy-standalone-lifecycle#task#9.
