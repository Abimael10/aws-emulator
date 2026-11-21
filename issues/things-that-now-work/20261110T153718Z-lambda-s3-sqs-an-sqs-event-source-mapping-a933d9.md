Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda__s3__sqs.sqs-esm-disabled-mapping-no-invocation-negative
Issue-Key: lambda__s3__sqs.sqs-esm-disabled-mapping-no-invocation-negative|run#18|Timeout|time.sleep(5)
Issue-Commit: 76dff93be94f91562bba6a25a3aecfc1e2578190

# lambda__s3__sqs: an SQS event source mapping never delivers, so the disabled-then-re-enabled mapping contract cannot be measured

> _Note: this issue was identified against the code base at commit `76dff93be94f91562bba6a25a3aecfc1e2578190`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateEventSourceMapping / UpdateEventSourceMapping (SQS poller)

## Description
One net-new root cause in the lambda__s3__sqs suite. It is NOT covered by the open issues for this heading (20261107T081525Z-lambda-s3-sqs-2-net-new-causes-a.md, 20261109T185037Z-lambda-s3-sqs-2-net-new-causes-s3-presigned-urls-are.md, 20261109T023059Z-lambda-s3-sqs-createeventsourcemapping-refuses-a-foreign-account.md, 20261105T094842Z-lambda-s3-sqs-the-python-lambda-runtime-provides.md, 20261107T145219Z-lambda-s3-sqs-a-bucket-notification-configuration-that.md); this check carries none of their replay keys.

Cause 1 of 1: an enabled SQS event source mapping never invokes its function, so the check cannot even measure the mapping's delivery latency — the baseline it needs before it can assert that a DISABLED mapping withholds a message and that re-enabling delivers the one queued while it was off.
AWS: recorded behavior only for the poller timing; the surface is modelled. CreateEventSourceMapping with an SQS EventSourceArn and Enabled=true reaches State "Enabled" and the poller invokes the function for each queued message (BatchSize=1 here), with the message removed from the queue on success. UpdateEventSourceMapping with Enabled=false moves it to "Disabled" and stops delivery: a message sent while disabled stays in the queue and the function is not invoked for a window sized from the mapping's own measured delivery latency. Re-enabling it delivers exactly that queued message.
Emulator: the check is reported "Error: Check timed out after 60.0s" with no assertion reached — the first leg, measuring one delivery through the enabled mapping, never completes.
Why: a client's SQS-triggered Lambda never runs, so every queue-driven workload is dead. And because nothing is delivered in the enabled state either, the negative half of the contract is vacuous: a mapping that ignores Enabled=false entirely cannot be distinguished from one that honours it.
Fix: the Lambda event-source-mapping poller for SQS — an Enabled mapping must receive from the queue and invoke the function (honouring BatchSize), deleting the message on success; Enabled=false must suspend receiving without losing queued messages; re-enabling must resume and deliver them.
Traces: lambda__s3__sqs.sqs-esm-disabled-mapping-no-invocation-negative.

## Evidence
Check lambda__s3__sqs.sqs-esm-disabled-mapping-no-invocation-negative, verdict [mismatch]:
  Error: Check timed out after 60.0s
with no assertion reached.

The awaited requirement, from the check's own docstring: "A disabled event source mapping does not deliver a queued message for a window sized from the mapping's own measured delivery latency, and re-enabling it delivers the message that was queued while it was disabled." The check creates a bucket, a role, a queue and a function, then creates the mapping with batch_size=1, enabled=True, and measures one delivery before disabling.

Expected: the enabled mapping invokes the function for the first queued message (observable as the function's report object in the bucket) within the measured latency; the disabled mapping invokes nothing for the absence window; re-enabling delivers the withheld message. Actual: the first delivery never happens, so the check is killed with nothing compared. Queue URLs, function names, mapping UUIDs and bucket names are per-run values.

## Steps to Reproduce
1. Create a bucket, an execution role, an SQS queue and a Lambda function that writes a report object per received record.
2. CreateEventSourceMapping(FunctionName=..., EventSourceArn=<queue arn>, BatchSize=1, Enabled=True) and wait for State "Enabled"; SendMessage and time how long the invocation takes to appear.
3. UpdateEventSourceMapping(UUID=..., Enabled=False), wait for State "Disabled", SendMessage again and confirm no invocation for a window sized from step 2's latency, with the message still in the queue.
4. UpdateEventSourceMapping(Enabled=True) and confirm the withheld message is delivered.
5. The emulator never completes step 2. Re-replay lambda__s3__sqs.sqs-esm-disabled-mapping-no-invocation-negative to confirm the fix.

## Resolution

The reported behaviour — an `Enabled` queue mapping that never invokes its function — is served
by the current tree, and the whole contract this check measures completes in about ten seconds
on the emulator a client STARTS (the built binary, its own port, request authentication on,
every call SigV4-signed over TLS): the mapping settles into `Enabled`, the message sent to its
queue reaches the running handler, which writes its report object into Amazon S3; the mapping
moved to `Disabled` takes nothing out of the queue, so the message sent then stays waiting
there (`ApproximateNumberOfMessages` 1, none in flight) with nothing reported for it over a
quiet window, and a direct `ReceiveMessage` is still handed it; enabled again, the mapping
delivers exactly that withheld message. Pinned end to end by
core/router/tests/all/lambda_sqs_mapping_enabled_gate_on_a_started_emulator.rs, beside the
owning crate's tests/all/event_source_mapping_pause_and_resume_delivery.rs (the same gate on a
router assembled in the test process) and tests/all/event_source_mapping_sqs_delivery.rs. The
poller itself lives in
services/service-com-amazonaws-lambda-awsgirapiservice/src/state/sqs_poller.rs, armed by
src/ops/create_event_source_mapping.rs and src/ops/update_event_source_mapping.rs.

What WAS broken in that path is the execution role's published grant: AWS's own
`arn:aws:iam::aws:policy/service-role/AWSLambdaSQSQueueExecutionRole` — the policy a client
attaches to the role an SQS-triggered function polls as, and the policy this emulator's own
CloudFormation SAM transform puts on the roles it creates
(services/service-com-amazonaws-cloudformation-cloudformation/src/state/sam.rs) — was not in
the IAM emulator's catalog of AWS-managed policies, so `GetPolicy`, `GetPolicyVersion` and
`ListPolicies(Scope=AWS)` reported it absent and a role granted the queue's three reads only
through it was judged on no document at all: Lambda then accepted that role for a mapping on
ANY event source, where AWS refuses a Kinesis or DynamoDB stream mapping for a role carrying
only the queue policy. The same held for `AWSLambdaMSKExecutionRole`. Both are now carried in
services/service-com-amazonaws-iam-awsidentitymanagementv20100508/src/state/managed_policy_catalog.rs,
pinned by that crate's tests/all/published_lambda_event_source_policies.rs and by
services/service-com-amazonaws-lambda-awsgirapiservice/tests/all/event_source_mapping_published_grant.rs,
whose table drives each published policy against each source and consumes a queue through a
role granted by nothing else.

The replay key `|run#18|Timeout|time.sleep(5)` is the harness's own 60 s per-check cap, reached
inside the check's own sleep; no step of the contract stalls in the emulator, which serves all
of it in about ten seconds.
