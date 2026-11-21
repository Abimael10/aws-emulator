Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation.stack-deploys-real-fifo-queue-that-deduplicates-and-orders cloudformation.update-stack-replaces-a-renamed-queue-and-updates-visibility-in-place cloudformation.logstream-replacement-orphans-old-stream-in-former-group
Issue-Shape-Id: com.amazonaws.cloudformation#CloudFormation
Issue-Key: cloudformation.stack-deploys-real-fifo-queue-that-deduplicates-and-orders|run#7|AssertionError|assert outputs["QueueUrl"] == queue_url, f"Ref of the queue is {outputs['QueueUrl']!r}, SQS reports {queue_url!r}"
Issue-Key: cloudformation.update-stack-replaces-a-renamed-queue-and-updates-visibility-in-place|run#8|AssertionError|assert in_place == first_url, f"a VisibilityTimeout change must keep the queue: Ref {in_place!r} vs {first_url!r}"
Issue-Key: cloudformation.logstream-replacement-orphans-old-stream-in-former-group|run#26|AssertionError|assert orphan_present == {_STREAM_NAME, "unrelated"}, (
Issue-Commit: 7feef7c3a108bb63e3851ac69c48ec064beaa56b

# cloudformation: 2 net-new causes — the Ref/Output of an AWS::SQS::Queue is a real-AWS-host queue URL instead of the URL SQS itself serves, and a replaced AWS::Logs::LogStream is really deleted from the former log group instead of being orphaned

> _Note: this issue was identified against the code base at commit `7feef7c3a108bb63e3851ac69c48ec064beaa56b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateStack

## Description
Two net-new root causes in the CloudFormation provisioning engine, found by replaying the three failing checks in trace_ids. Both are reached through CreateStack/UpdateStack on stacks the emulator itself created, so both are the emulator's own behaviour.

Cause 1 of 2: CloudFormation publishes a synthesized real-AWS-host queue URL for AWS::SQS::Queue, so the stack's Ref/Output does not match the QueueUrl SQS reports for the very queue the stack created. Failing checks: cloudformation.stack-deploys-real-fifo-queue-that-deduplicates-and-orders, cloudformation.update-stack-replaces-a-renamed-queue-and-updates-visibility-in-place.
AWS:      Ref of an AWS::SQS::Queue returns the queue's URL — the same value the SQS API returns for that queue. Smithy (sqs): CreateQueue output member QueueUrl: String and GetQueueUrl output member QueueUrl: String are the single authority for a queue's URL, and every SQS call a client makes afterwards takes that URL as QueueUrl. So the requirement is an identity: the value CloudFormation publishes for Ref (and for an Output built from it) is byte-for-byte the value SQS's CreateQueue/GetQueueUrl serves for the same queue, on an authority the emulator actually answers on; AWS satisfies it because both are https://sqs.<region>.amazonaws.com/<account-id>/<queue-name>.
Emulator: the two values disagree in their authority. The stack Output/Ref is 'https://sqs.us-west-2.amazonaws.com/<account-id>/<queue-name>' (a hard-coded real-AWS host the emulator does not serve), while SQS's own GetQueueUrl/CreateQueue for the same queue reports 'https://<emulator-host>:<port>/<account-id>/<queue-name>'. The account id, queue name and path are identical; only the announced endpoint differs, and the one CloudFormation announces is not the one SQS is reached on.
Why:      a client that does the normal thing — read the queue URL out of the stack (Ref, GetAtt, or an Output) and then call SendMessage/ReceiveMessage/GetQueueAttributes with it — is sent to an address this deployment does not serve, so the call hangs, fails to resolve, or is refused; comparing the stack's Ref with GetQueueUrl (a standard post-deploy assertion, and how a client decides whether an update replaced the queue) reports a difference that does not exist in AWS.
Fix:      the CloudFormation AWS::SQS::Queue resource handler's Ref/GetAtt (QueueUrl) construction and the Outputs serializer — publish the queue URL SQS's own state holds instead of composing sqs.<region>.amazonaws.com. Open issues name the same hard-coded host in other surfaces but carry neither of these replay keys: 20261111T110051Z-sqs-2-net-new-causes-createqueue-publishes-a.md and 20261111T055733Z-sqs-queue-urls-returned-by-the-read-operations.md (SQS's own reads), 20261121T121249Z-cloudformation-s3-sqs-the-queueurl-output-of-an.md (the Output of an AWS::Include-transformed stack) and 20261121T121249Z-cloudcontrol-sqs-the-aws-sqs-queue-handler-keys.md (Cloud Control).
Traces:   cloudformation.stack-deploys-real-fifo-queue-that-deduplicates-and-orders (assertion "Ref of the queue is ... SQS reports ..." at run() line 200) and cloudformation.update-stack-replaces-a-renamed-queue-and-updates-visibility-in-place (assertion "a VisibilityTimeout change must keep the queue: Ref ... vs ..." at run() line 215).

Cause 2 of 2: when an UpdateStack replaces an AWS::Logs::LogStream because its LogGroupName changed, the emulator really deletes the superseded stream from the former log group, instead of leaving it behind as AWS does. Failing check: cloudformation.logstream-replacement-orphans-old-stream-in-former-group.
AWS:      recorded behavior only for the data-plane effect. LogGroupName and LogStreamName are create-only properties of AWS::Logs::LogStream, so changing LogGroupName forces a replacement, and CloudFormation reports the replaced stream's DELETE_IN_PROGRESS/DELETE_COMPLETE — yet the stream itself stays in the former log group with the events written to it before the move: DescribeLogStreams on the first group still lists the moved stream's name alongside any unrelated streams, and GetLogEvents still returns its content. The delete CloudFormation announces is bookkeeping about the logical resource, not a CloudWatch Logs DeleteLogStream on the old group. DeleteStack then removes only the stream CloudFormation currently tracks (the one in the new group), and unrelated sibling streams in both groups are untouched throughout.
Emulator: after the move update, DescribeLogStreams on the first log group lists only the unrelated sibling stream — the superseded stream named by the template is gone, so the replacement path issues a real delete against the old group. The check's set comparison reports {'unrelated'} where AWS has {'<stream-name>', 'unrelated'}.
Why:      a client that relies on AWS's orphaning — reading the pre-move events out of the former group after the replacement, or running a cleanup/drift pass that expects to find and remove the orphan — finds the data destroyed instead of retained, and the log content written before the move is lost without any delete having been requested. A check or operator script that reconciles orphans concludes there is nothing to reconcile.
Fix:      the CloudFormation replacement/cleanup path for AWS::Logs::LogStream — the replaced physical resource must be released, not deleted, when its identity (LogGroupName/LogStreamName) changes; only the stream the stack currently tracks is deleted by DeleteStack. The open issue 20261112T113237Z-cloudformation-2-net-new-causes-changing-aws-logs.md covers the earlier, different cause on this same check (the LogGroupName change being classified as an in-place update with no DELETE event at all) and carries that replay key (run#21, "assert old_deletes"); the failure now surfaces later in the same check, at the orphan assertion (run#97), and is a distinct cause.
Traces:   cloudformation.logstream-replacement-orphans-old-stream-in-former-group (AssertionError at run() line 97: "first group streams after the move are {'unrelated'}").

## Evidence
Cause 1 — check cloudformation.stack-deploys-real-fifo-queue-that-deduplicates-and-orders [mismatch], AssertionError at run() line 200:
  AssertionError: Ref of the queue is 'https://sqs.us-west-2.amazonaws.com/<account-id>/<queue-name>.fifo', SQS reports 'https://<emulator-host>:<port>/<account-id>/<queue-name>.fifo'
Cause 1 — check cloudformation.update-stack-replaces-a-renamed-queue-and-updates-visibility-in-place [mismatch], AssertionError at run() line 215:
  AssertionError: a VisibilityTimeout change must keep the queue: Ref 'https://sqs.us-west-2.amazonaws.com/<account-id>/<queue-name>' vs 'https://<emulator-host>:<port>/<account-id>/<queue-name>'
Expected (AWS): the stack's Ref/Output value and SQS's CreateQueue/GetQueueUrl value for the same queue are the identical string, on an authority the emulator serves. The host, port, account id and generated queue name are per-run values; the identity of the two values is the requirement.

Cause 2 — check cloudformation.logstream-replacement-orphans-old-stream-in-former-group [mismatch], AssertionError at run() line 97:
  AssertionError: first group streams after the move are {'unrelated'}; real AWS leaves the superseded stream orphaned here despite CloudFormation's own DELETE_COMPLETE event
Expected (AWS): DescribeLogStreams on the first log group lists both the template's stream name and the unrelated sibling ({'<stream-name>', 'unrelated'}), with the pre-move events still readable; only the tracked stream in the new group is removed by DeleteStack.

## Steps to Reproduce
Cause 1:
1. CreateStack a template with a single AWS::SQS::Queue (e.g. a FIFO queue) and an Output whose Value is {"Ref": "<queue>"}; wait for CREATE_COMPLETE.
2. Read the Output (or Ref/GetAtt QueueUrl) and call sqs GetQueueUrl (or read CreateQueue's QueueUrl) for the same queue name.
3. Compare: CloudFormation reports https://sqs.<region>.amazonaws.com/<account-id>/<name> while SQS reports https://<emulator-host>:<port>/<account-id>/<name>; using the CloudFormation value as QueueUrl does not reach the emulator.
4. Same divergence on an UpdateStack that changes only VisibilityTimeout: the in-place Ref cannot be compared with the queue URL SQS serves.

Cause 2:
1. Create two CloudWatch Logs log groups and give each an unrelated log stream.
2. CreateStack a template with one AWS::Logs::LogStream whose LogStreamName is fixed and whose LogGroupName comes from a parameter set to the first group; wait for CREATE_COMPLETE and PutLogEvents into the stream.
3. UpdateStack with UsePreviousTemplate=true, changing only the parameter to the second group (LogGroupName is create-only, so the stream is replaced); wait for UPDATE_COMPLETE.
4. DescribeLogStreams on the FIRST log group: the superseded stream is absent (only 'unrelated' remains), where AWS still lists it with its pre-move events.
