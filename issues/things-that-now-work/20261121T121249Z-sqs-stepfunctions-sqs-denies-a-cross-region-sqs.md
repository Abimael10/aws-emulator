Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: sqs__stepfunctions.cross-region-sqs-sendmessage-optimized-integration-succeeds sqs__stepfunctions.cross-region-sqs-sendmessage-without-permission-on-far-queue-denied
Issue-Key: sqs__stepfunctions.cross-region-sqs-sendmessage-optimized-integration-succeeds|run#7|AssertionError|assert resp["status"] == "SUCCEEDED", (
Issue-Key: sqs__stepfunctions.cross-region-sqs-sendmessage-without-permission-on-far-queue-denied|run#9|AssertionError|assert good["status"] == "SUCCEEDED", (
Issue-Commit: a3e7427fc33f71baf84855dcb1036df3c2b77cc6

# sqs__stepfunctions: SQS denies a cross-region sqs:sendMessage from a Step Functions role that holds sqs:SendMessage on the far-region queue ARN

> _Note: this issue was identified against the code base at commit `a3e7427fc33f71baf84855dcb1036df3c2b77cc6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SendMessage

## Description
Cause 1 of 1: a states:::sqs:sendMessage task whose QueueUrl names a queue in another region of the same account is refused AccessDenied even though the execution role's inline policy allows Action sqs:SendMessage on exactly that queue's ARN (the ARN GetQueueAttributes returned for it). The authorization decision is made against a resource ARN in the CALLER's region, so a policy scoped to the far-region ARN never matches, and the task fails instead of delivering. Both the positive case and the positive control of the negative case fail this way, so no cross-region optimized SQS integration can succeed.

AWS: Smithy (sqs) SendMessage takes the queue's absolute QueueUrl, which carries its own region; the queue referenced is the one that URL names, and IAM evaluates the request against THAT queue's ARN (arn:aws:sqs:<queue-region>:<account>:<name>). An identity policy allowing sqs:SendMessage on that ARN authorizes the call, so Step Functions reports SUCCEEDED with the SendMessage result (MD5OfMessageBody, MessageId) and the far region's queue holds the message. When a call genuinely is denied, AWS's message is 'User: <principal-arn> is not authorized to perform: sqs:SendMessage on resource: <queue-arn> because no identity-based policy allows the sqs:SendMessage action' — the action appears with its modeled casing, sqs:SendMessage, and the resource is the queue the URL names. Recorded behavior only for the rest of the envelope.

Emulator: StartSyncExecution reports status FAILED, error 'SQS.AmazonSQSException', cause 'User: arn:aws:sts::<account>:assumed-role/<role>/<session> is not authorized to perform: sqs:sendmessage on resource: arn:aws:sqs:us-west-2:<account>:<far-queue-name> because no identity-based policy allows the sqs:sendmessage action (Service: AmazonSQS; Status Code: 403; Error Code: AccessDenied; ...)'. Two things diverge in that text: the resource ARN carries the caller's region (us-west-2) although the QueueUrl and the granted ARN name the far region, and the action is lower-cased ('sqs:sendmessage') where AWS writes 'sqs:SendMessage'. Hypothesis: the authorizer builds the resource ARN from its own default/region context rather than from the region in the QueueUrl, and compares case-folded action strings, so the statement's Resource never matches.

Why: a client that grants exactly the documented permission on the queue it targets is still denied, so cross-region SQS delivery from Step Functions (and any SQS call addressed by an absolute URL in another region) is impossible; a negative test for the same path cannot distinguish a real denial from this one, and the denial message's action casing does not match what a client parses or logs.

Fix: SQS's authorization path — resolve the request's resource ARN from the region in the supplied QueueUrl before evaluating identity policies, and render the action in the refusal message with its modeled casing.

Traces: the two failing checks in trace_ids.

## Evidence
- cross-region-sqs-sendmessage-optimized-integration-succeeds [mismatch]:
  AssertionError: a cross-region sqs:sendMessage task must SUCCEED; got 'FAILED' error='SQS.AmazonSQSException' cause='User: arn:aws:sts::000000000000:assumed-role/<role>/<session> is not authorized to perform: sqs:sendmessage on resource: arn:aws:sqs:us-west-2:000000000000:<name>-far-<id> because no identity-based policy allows the sqs:sendmessage action (Service: AmazonSQS; Status Code: 403; Error Code: AccessDenied; Request ID: <id>; Proxy: null)'
- cross-region-sqs-sendmessage-without-permission-on-far-queue-denied [mismatch] (its positive control):
  AssertionError: control: the role holding sqs:SendMessage on the far queue must SUCCEED; got 'FAILED' ... same cause text.

Setup in both checks: the queue is created through the far region's client; its ARN is read with GetQueueAttributes(QueueArn); the role's inline policy is {"Effect":"Allow","Action":"sqs:SendMessage","Resource":<that far-region ARN>}; the state machine's task Parameters.QueueUrl is the far queue's URL.
Expected (AWS): status SUCCEEDED, output carrying MD5OfMessageBody/MessageId, and the message receivable from the far region's endpoint.
Actual (emulator): AccessDenied naming the caller's region in the resource ARN and a lower-cased action. (Role names, session ids, request ids and queue names are per-run values.)

## Steps to Reproduce
1. In region B (!= the caller's region A) create a queue; read its QueueArn with GetQueueAttributes.
2. Create an IAM role trusted by states.amazonaws.com with an inline policy allowing sqs:SendMessage on that far-region queue ARN.
3. In region A create an EXPRESS state machine with one Task, Resource arn:aws:states:::sqs:sendMessage, Parameters {QueueUrl: <far queue URL>, MessageBody.$: $.body}, roleArn = that role.
4. StartSyncExecution. Expected SUCCEEDED with the SendMessage result and the message present in region B. Observed FAILED / SQS.AmazonSQSException with AccessDenied whose resource ARN carries region A and whose action is 'sqs:sendmessage'.
