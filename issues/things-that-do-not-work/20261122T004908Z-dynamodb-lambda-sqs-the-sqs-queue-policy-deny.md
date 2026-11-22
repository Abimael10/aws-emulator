Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: dynamodb__lambda__sqs.queue-policy-deny-suppresses-esm-delivery
Issue-Key: dynamodb__lambda__sqs.queue-policy-deny-suppresses-esm-delivery|_assert_absent_for#30|Timeout|time.sleep(interval_s)
Issue-Commit: 29c63349654daecf518dc89fab6ca09926c7725f

# dynamodb__lambda__sqs: the SQS queue-policy Deny / ESM suppression scenario makes no observable progress — the SQS event source mapping never delivers its calibration message, so the check is cut off at the harness cap with no verdict

> _Note: this issue was identified against the code base at commit `29c63349654daecf518dc89fab6ca09926c7725f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateEventSourceMapping

## Description
Cause 1 of 1: an Enabled Lambda event source mapping over an SQS queue never delivers the message that is sent to prove the pipe works, so the whole queue-policy-Deny suppression differential never reaches a verdict and the check is terminated at the suite's 60 s budget with `Error: Check timed out after 60.0s`.

AWS: CreateEventSourceMapping(EventSourceArn=<queue arn>, FunctionName=<fn>, BatchSize=1, Enabled=True) reaches GetEventSourceMapping State "Enabled" (Smithy: lambda EventSourceMappingConfiguration$State), and Lambda's poller then receives from the queue and invokes the function once per message, with the record's eventSourceARN equal to the resolved queue ARN. On top of that working pipe, AWS behaves as follows and none of it is observable here: (a) a queue Policy whose statement Denies sqs:ReceiveMessage for Principal "*" silently stops the poller — the message stays in the queue (ApproximateNumberOfMessages >= 1), the mapping stays State "Enabled" and reports no LastProcessingResult, so a client watching the ESM gets no signal its consumer stopped; (b) the SAME explicit Deny refuses the caller's OWN sqs:ReceiveMessage with HTTP 403 and error code AccessDenied and the message `User: <caller arn> is not authorized to perform: sqs:receivemessage on resource: <queue arn> with an explicit deny in a resource-based policy` — fixed text "User: ", " is not authorized to perform: ", " on resource: ", " with an explicit deny in a resource-based policy"; the caller ARN (from GetCallerIdentity), the lower-cased action name and the queue ARN are values from the request and the emulator's state (Smithy: sqs ReceiveMessage models no AccessDenied shape, so this refusal is recorded behavior only — authorization is applied ahead of the modeled errors); (c) a Deny scoped to the Lambda execution role's ARN does NOT suppress the poller, because the poller does not receive as that principal; (d) clearing the Policy resumes delivery of that same still-queued message.

Emulator: the check produces no observable step at all within the harness budget. Its internal waits are far longer than that budget (180 s for the ESM to reach Enabled and 180 s for the first delivery, 300 s for the resume leg), and the suite kills it at 60 s, so the emulator never got past the calibration leg: the first message sent to the queue after the mapping is created is never turned into a DynamoDB item by the handler. Replay detail: `Error: Check timed out after 60.0s`. None of the suppression, 403/AccessDenied refusal, queue-depth, mapping-State/LastProcessingResult or resume assertions is reached.

Why: a client that uses an SQS-triggered Lambda gets no invocations at all, and a client that relies on the documented asymmetry — resource-policy Deny silently starves the poller while the mapping keeps reporting Enabled, versus a role-scoped Deny that does not — cannot observe either side, so a working configuration and a broken one are indistinguishable. The negative control is also indistinguishable from the positive one, which is the worst case for a test suite written against AWS.

Fix: the lambda service's SQS event-source poller (the component that drives an Enabled EventSourceMapping over an SQS EventSourceArn: receive → invoke → delete) must actually poll and invoke, and must consult the queue's resource policy for sqs:ReceiveMessage as the SQS poller principal so an explicit Deny suppresses delivery without changing the mapping's State or setting LastProcessingResult; the SQS service's ReceiveMessage authorization must refuse an explicit resource-policy Deny with 403/AccessDenied and the message wording above.

Note: open issue 20261024T102105Z-kms-lambda-sqs-lambda-event-source-mappings-never-deliver-sqs-messages.md records the same silence for a different scenario, and 20261029T183635Z-lambda-sns-sqs-sqs-event-source-mappings-never.md likewise; neither carries this check's replay key and the suite reports this failure as unowned, so it is filed here.

Traces: dynamodb__lambda__sqs.queue-policy-deny-suppresses-esm-delivery.

## Evidence
Expected (AWS): (1) GetEventSourceMapping reaches State "Enabled"; (2) SendMessage of a calibration body is delivered — the handler writes a DynamoDB item whose `source` attribute equals the queue ARN; (3) with a role-scoped Deny of sqs:ReceiveMessage in the queue Policy, a second message is still delivered; (4) with an unscoped `Principal: "*"` Deny of sqs:ReceiveMessage, the caller's own ReceiveMessage is refused with HTTP 403, Error.Code "AccessDenied" and Error.Message `User: <caller arn> is not authorized to perform: sqs:receivemessage on resource: <queue arn> with an explicit deny in a resource-based policy`, the third message is NOT delivered, ApproximateNumberOfMessages stays >= 1, and GetEventSourceMapping still reports State "Enabled" with LastProcessingResult absent/empty; (5) clearing the Policy delivers that same third message.

Actual (emulator): no step is observable. The check is terminated by the harness: `Error: Check timed out after 60.0s`, before the calibration delivery in (2) completes — the first message sent after the mapping is created never produces the DynamoDB item, so none of (3), (4) or (5) is evaluated.

## Steps to Reproduce
1. CreateTable (single `pk` string key); CreateFunction with an execution role granting the SQS receive/delete and DynamoDB put actions, and env TABLE=<table>.
2. CreateQueue; CreateEventSourceMapping(EventSourceArn=<queue arn>, FunctionName=<fn>, BatchSize=1, Enabled=True); poll GetEventSourceMapping until State == "Enabled".
3. SendMessage a unique calibration body; poll GetItem on the table for that body.
4. Observe no item ever appears; the check exhausts its budget here and is killed at the 60 s harness cap, so the remaining legs (role-scoped Deny still delivers; unscoped `Principal:"*"` Deny of sqs:ReceiveMessage refuses the caller with 403/AccessDenied and silently suppresses delivery while the mapping stays Enabled with no LastProcessingResult; clearing the Policy resumes delivery of the same message) are never reached.
