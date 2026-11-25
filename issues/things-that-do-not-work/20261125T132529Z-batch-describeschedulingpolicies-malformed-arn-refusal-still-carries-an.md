Issue-Source: trace-sweep-divergence
Issue-Trace: batch-scheduling-policy-standalone-lifecycle
Issue-Step: task#9
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: batch-scheduling-policy-standalone-lifecycle
Issue-Shape-Id: com.amazonaws.batch#AWSBatchV20160810
Issue-Key: batch|DescribeSchedulingPolicies|4xx->4xx|$.message
Issue-Commit: 9836cb9fc16422860d5f8603511ba419094de73f

# batch DescribeSchedulingPolicies malformed-ARN refusal still carries an emulator-authored sentence inside Batch's envelope

> _Note: this issue was identified against the code base at commit `9836cb9fc16422860d5f8603511ba419094de73f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeSchedulingPolicies

## Description
One net-new root cause on the AWS Batch scheduling-policy read plane. It is the ONLY divergence cluster in this replay whose owned_by is EMPTY: the other nine clusters are owned by open issues that already carry their replay keys (20261102T113649Z-batch-job-api-refusal-contracts-diverge-two-wrong.md for the DescribeJobs/ListJobs/RegisterJobDefinition/SubmitJob clusters, 20261029T234420Z-batch-refusal-contracts-diverge-not-found-arn-wording.md for ListJobsByConsumableResource and SubmitServiceJob, 20261109T161717Z-batch-3-net-new-causes-tag-untag-204.md for the TagResource/UntagResource 204 Content-Length clusters) and are not re-filed here.

Cause 1 of 1: DescribeSchedulingPolicies' refusal for a malformed scheduling-policy ARN in `arns` carries an inner sentence that is not the one AWS sends for that refusal class, so the recording's message assertion still fails even though the status, the error class and Batch's request envelope are now all correct.

AWS: Smithy (services/service-com-amazonaws-batch-awsbatchv20160810/smithy-model/com-amazonaws-batch-awsbatchv20160810.json) models DescribeSchedulingPolicies as POST /v1/describeschedulingpolicies, success code 200, input member `arns` -> StringList (@required, clientOptional), output `schedulingPolicies` -> SchedulingPolicyDetailList, and exactly two errors: ClientException (errorKind client, httpError 400) and ServerException (500). The model states no message text, so the wording is recorded behavior only. The recording refuses an `arns` entry that is not a well-formed scheduling-policy ARN with HTTP 400 / ClientException, and asserts its message by SHAPE rather than literally (the replay reports expected '<shaped-not-asserted>') because the recorded text terminates in a per-call request id. The recorded shape is Batch's input-validation envelope: fixed text "Error executing request, Exception : ", then AWS's own validator sentence for the malformed-ARN class, then fixed text ", RequestId: ", then the call's request id. The only variable parts of that template are the values the sentence interpolates from the request (the rejected `arns` entry and/or the member name) and the request id taken from the call; those are never themselves the requirement.
Hypothesis: AWS's inner sentence for a malformed entry in `arns` is Batch's standard input-validation wording that names the member and the constraint (the "1 validation error detected: Value '<rejected value>' at 'arns' failed to satisfy constraint: ..." form that this same service emits for a null `jobs` member on DescribeJobs), not a hand-written "Scheduling policy arn should match a valid Scheduling policy arn pattern" sentence. This explanation is labelled Hypothesis because neither the model nor the replay detail states the recorded sentence - what IS asserted is only that the emulator's message does not satisfy the recorded shape.

Emulator: HTTP 400 with x-amzn-errortype ClientException (status and error class both correct) and $.message = "Error executing request, Exception : Scheduling policy arn should match a valid Scheduling policy arn pattern, RequestId: <request id>". In that string the fixed text is "Error executing request, Exception : " + "Scheduling policy arn should match a valid Scheduling policy arn pattern" + ", RequestId: "; the only variable part is the per-call request id, which is not the defect. Note the history: two earlier filings on this exact step are RESOLVED (20261030T031148Z-batch-describeschedulingpolicies-malformed-arn-refusal-message-is-emulator.md, 20261030T204009Z-batch-describeschedulingpolicies-malformed-arn-refusal-is-not-wrapped.md) and a third, 20261113T130550Z-batch-describeschedulingpolicies-malformed-arn-refusal-message-still-diverges.md, is also marked [fixed]. Those fixes landed in part - the envelope is present now, and the ": <rejected value>" interpolation that the last report quoted has since been dropped - but the wrapped sentence is still the emulator's own, so the step still replays as a mismatch. A resolved issue does not cover a failure that still replays and none of the three carries this step's replay key any more, hence this net-new filing.

Why: Batch answers every client mistake with the single ClientException shape at HTTP 400, so the message text is the only machine-readable detail a caller gets. A client that tells "the ARN I passed is syntactically wrong" apart from "the scheduling policy does not exist" or from an arity/page-size refusal by matching AWS's sentence gets no match against the emulator and drops a correctable input mistake into a generic error path; conversely a suite written against the emulator's sentence passes here and fails against AWS. Because sibling validators on this operation render through the same envelope, a caller cannot cover all of this operation's refusals with the one matcher AWS's uniform wording allows.

Fix: the inner refusal text produced by DescribeSchedulingPolicies' input validation in the batch crate - the arn-pattern branch of that operation's validation layer. The envelope/renderer it goes through is correct and must be kept; only the sentence it wraps has to become AWS's. Not the status and not the error class, which already match.

Traces: batch-scheduling-policy-standalone-lifecycle#task#9.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout: emulator_up true (/__admin/health answered 200 and the :44401 data listener accepts), bootstrap_signal false, 21 fixtures replayed, 177 steps total, 158 passed, 17 mismatched, 2 errored, 0 blocked, score 89.3%, 10 divergence clusters.

This cluster: divergence_key "service=batch|DescribeSchedulingPolicies|status=400->400|keys=$.message", fixture_id batch-scheduling-policy-standalone-lifecycle, step_key task#9, operation DescribeSchedulingPolicies, verdict mismatch, affected_count 1 (affected: batch-scheduling-policy-standalone-lifecycle#task#9), stub_path EMPTY, owned_by EMPTY - no open issue carries this replay key.

expected_status 400, actual_status 400 - HTTP status and error class (ClientException) agree.
detail, verbatim:
  $.message: value_mismatch (expected '<shaped-not-asserted>', got 'Error executing request, Exception : Scheduling policy arn should match a valid Scheduling policy arn pattern, RequestId: a724a0be-77b6-4f4b-92a0-dee9a2c4b628')

Reading of that detail: the expected side is a shape assertion (not a literal) because the recorded AWS message ends in a per-call request id. The actual message carries the envelope's fixed text ("Error executing request, Exception : " ... ", RequestId: ") that the earlier filings asked for, and no longer echoes the rejected value, yet it still fails the shape assertion - so the residual divergence is the sentence the envelope wraps. The uuid a724a0be-77b6-4f4b-92a0-dee9a2c4b628 is this run's request id; a difference confined to a per-run request id would satisfy the shape assertion and is not what is filed here.

Smithy evidence (smithy_query on services/service-com-amazonaws-batch-awsbatchv20160810/smithy-model/com-amazonaws-batch-awsbatchv20160810.json): DescribeSchedulingPolicies - http POST /v1/describeschedulingpolicies, code 200; input {arns: StringList, required, clientOptional}; output {schedulingPolicies: SchedulingPolicyDetailList}; errors ClientException (errorKind client, httpError 400) and ServerException (errorKind server, httpError 500). No message text is modeled, hence "recorded behavior only" for the wording.

Prior-art status: list_existing_issues(status="all", query="describeschedulingpolicies") returns exactly three issues, all [fixed]: 20261030T031148Z-..., 20261030T204009Z-..., 20261113T130550Z-.... No OPEN issue mentions this operation, which matches the replay's empty owned_by for this cluster.

## Steps to Reproduce
1. Clone this checkout, boot the emulator from it and replay the wire corpus simparity-wire-aws-v1.
2. Fixture batch-scheduling-policy-standalone-lifecycle, step task#9: POST /v1/describeschedulingpolicies with `arns` containing one entry that is not a well-formed scheduling-policy ARN (the recorded probe uses the literal "not-a-valid-arn"). By hand: aws batch describe-scheduling-policies --arns not-a-valid-arn
3. Observed: HTTP 400, x-amzn-errortype ClientException, $.message = "Error executing request, Exception : Scheduling policy arn should match a valid Scheduling policy arn pattern, RequestId: <request id>" - which the replay reports as value_mismatch against the recording's shaped message assertion.
4. Expected: HTTP 400 / ClientException whose message is AWS's recorded sentence for a malformed `arns` entry, rendered through Batch's input-validation envelope ("Error executing request, Exception : " + <AWS's sentence> + ", RequestId: " + this call's request id). The envelope is already correct; the wrapped sentence is not.
5. Cross-check the sibling validators on the same operation (arns arity, and the ListQuotaShares/ListSchedulingPolicies page-size refusals) so every refusal this operation emits matches on one client-side matcher.
6. Re-replay to confirm: batch-scheduling-policy-standalone-lifecycle#task#9.
