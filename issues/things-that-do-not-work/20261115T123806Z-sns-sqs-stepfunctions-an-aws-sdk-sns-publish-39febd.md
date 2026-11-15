Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: sns__sqs__stepfunctions.aws-sdk-sns-publish-task-delivers-and-missing-topic-is-caught
Issue-Key: sns__sqs__stepfunctions.aws-sdk-sns-publish-task-delivers-and-missing-topic-is-caught|run#21|AssertionError|assert got == _MISSING_TOPIC, f"the caught error was {err!r}, expected {_MISSING_TOPIC!r}"
Issue-Commit: f4e3930dabb7420d5dbe8abed2b59ac3a4a8acd2

# sns__sqs__stepfunctions: an aws-sdk:sns:publish task failure carries the legacy SDK v1 Cause envelope (semicolon fields, service 'AmazonSNS', 'Error Code'/'Proxy', no 'SDK Attempt Count') instead of AWS's modern SDK envelope

> _Note: this issue was identified against the code base at commit `f4e3930dabb7420d5dbe8abed2b59ac3a4a8acd2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartExecution

## Description
ONE net-new root cause, grounded in the single failing check of this suite's `sns__sqs__stepfunctions` service that the replay report marks neither ALREADY FILED nor OWNED.

Context — related OPEN issue named for context only (the report does not mark this check as OWNED by it, so it does not cover it): 20261111T104243Z-sns-sqs-stepfunctions-an-aws-sdk-sns-publish.md. That issue described TWO causes for this same check: the ASL error NAME and the Cause STRING. The name half no longer diverges — the emulator now reports `Sns.NotFoundException`, the Catch matches, and the execution ends SUCCEEDED with output.handled == "missing-topic". The Cause half is still wrong, and that is what this issue is about. A fixer closing this must make only the Cause envelope match.

Cause 1 of 1: the Cause string a failed `arn:aws:states:::aws-sdk:<service>:<op>` Task writes into the caught error object.
Check id: sns__sqs__stepfunctions.aws-sdk-sns-publish-task-delivers-and-missing-topic-is-caught

AWS: Step Functions' AWS SDK service integrations are driven by the modern (v2) AWS SDK, so the Cause it reports for a service-side refusal is that SDK's error envelope: the service's message text, then a single parenthesised group of COMMA-separated fields `(Service: <PascalCaseSdkName>, Status Code: <http status>, Request ID: <id>)`, followed by a second group `(SDK Attempt Count: <n>)`. There is no `Error Code:` field and no `Proxy:` field. For SNS Publish against a well-formed topic ARN naming no topic the refusal is SNS's modelled NotFoundException (Smithy: Publish errors include `NotFoundException`, errorKind client, httpError 404, awsQueryError code `NotFound`), whose message is "Topic does not exist", and the service name in the envelope is `Sns`. The fixed text of the Cause is therefore `<service message> (Service: <SdkName>, Status Code: <status>, Request ID: <id>) (SDK Attempt Count: <n>)`; the VARIABLE parts are the service's own message, the HTTP status of the refusal, the per-call request id and the attempt count — those come from the refused call, not from any recording. Status/code for this class: HTTP 404, SNS error code `NotFound`, ASL error name `Sns.NotFoundException` (recorded behavior for the error-name derivation; the status and code are the Smithy-modelled ones above).

Emulator: the Cause is the legacy AWS SDK v1 envelope — SEMICOLON-separated fields, the v1 service label `AmazonSNS`, an extra `Error Code: <code>` field, a trailing `Proxy: null`, and no `(SDK Attempt Count: n)` group: `Topic does not exist (Service: AmazonSNS; Status Code: 404; Error Code: NotFound; Request ID: <per-run id>; Proxy: null)`. The error name and the execution outcome are now correct; only this string diverges.

Why: the caught error object a Catch writes to its ResultPath is what a state machine logs, pattern-matches on and forwards downstream. A client that parses Cause for the service name, the status code or the attempt count reads fields AWS never emits (`Error Code`, `Proxy`) under a separator AWS does not use, and finds no `SDK Attempt Count` — so error-reporting states, log scrapers and failure-classification branches written against AWS produce the wrong result, and anything that compares the Cause against AWS's documented shape mismatches.

Fix: the Step Functions task-failure error/cause mapper for `aws-sdk:*` resource integrations in the sfn service crate (the component that turns a downstream service refusal into the Task's {Error, Cause} pair) — build the Cause with the modern SDK envelope (comma-separated Service/Status Code/Request ID, PascalCase SDK service name, no Error Code or Proxy fields, trailing SDK Attempt Count group) rather than the v1 envelope.

Traces: sns__sqs__stepfunctions.aws-sdk-sns-publish-task-delivers-and-missing-topic-is-caught. The positive leg of the same check (publish to a real topic, MessageId returned, message delivered to the subscribed queue) passes; only the Cause text of the failure leg diverges.

## Evidence
Failing check (suite service heading `### sns__sqs__stepfunctions`):
sns__sqs__stepfunctions.aws-sdk-sns-publish-task-delivers-and-missing-topic-is-caught

AssertionError (request id masked by the check):
  the caught error was {'Error': 'Sns.NotFoundException', 'Cause': 'Topic does not exist (Service: AmazonSNS; Status Code: 404; Error Code: NotFound; Request ID: <per-run id>; Proxy: null)'},
  expected ('Sns.NotFoundException', 'Topic does not exist (Service: Sns, Status Code: 404, Request ID: <id>) (SDK Attempt Count: 1)')

Expected (AWS): err.Error == "Sns.NotFoundException" AND err.Cause == "<service message> (Service: Sns, Status Code: 404, Request ID: <id>) (SDK Attempt Count: 1)" — fixed text: " (Service: ", ", Status Code: ", ", Request ID: ", ") (SDK Attempt Count: ", ")"; variable text: the SNS message ("Topic does not exist"), the status (404), the per-run request id, the attempt count.
Actual (emulator): err.Error == "Sns.NotFoundException" (correct) but err.Cause == "Topic does not exist (Service: AmazonSNS; Status Code: 404; Error Code: NotFound; Request ID: <per-run id>; Proxy: null)" — semicolon separators, v1 service label, extra Error Code and Proxy fields, no SDK Attempt Count group.

State machine under test: single Task, Resource `arn:aws:states:::aws-sdk:sns:publish`, Parameters {"TopicArn.$": "$.topic", "Message.$": "$.message"}, Catch [{"ErrorEquals": ["Sns.NotFoundException"], "ResultPath": "$.err", "Next": "Missing"}]. Execution status and output.handled are correct today; only err.Cause diverges.

## Steps to Reproduce
1. sns:CreateTopic; sqs:CreateQueue with a policy admitting the topic; sns:Subscribe(Protocol="sqs").
2. iam:CreateRole trusting states.amazonaws.com, allowing sns:Publish on both <topic arn> and <topic arn>-absent.
3. states:CreateStateMachine: StartAt "Publish"; "Publish" = {Type: Task, Resource: "arn:aws:states:::aws-sdk:sns:publish", Parameters: {"TopicArn.$": "$.topic", "Message.$": "$.message"}, Catch: [{ErrorEquals: ["Sns.NotFoundException"], ResultPath: "$.err", Next: "Missing"}], End: true}; "Missing" = Pass emitting {"handled": "missing-topic"}.
4. states:StartExecution with {"topic": "<real topic arn>", "message": "<marker>"} — SUCCEEDED, MessageId returned, message delivered (passes today).
5. states:StartExecution with {"topic": "<topic arn>-absent", "message": "never"} — the execution ends SUCCEEDED via the Catch and output.handled == "missing-topic" (both correct today). Read output.err.Cause: EXPECT "Topic does not exist (Service: Sns, Status Code: 404, Request ID: <id>) (SDK Attempt Count: 1)"; OBSERVE "Topic does not exist (Service: AmazonSNS; Status Code: 404; Error Code: NotFound; Request ID: <id>; Proxy: null)".
Replay: sns__sqs__stepfunctions.aws-sdk-sns-publish-task-delivers-and-missing-topic-is-caught
