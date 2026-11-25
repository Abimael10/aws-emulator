Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudtrail__events__iam__sqs.xsvc-iam-1-iam-role-events-route-by-event-name-and-only-in-us-east-1
Issue-Key: cloudtrail__events__iam__sqs.xsvc-iam-1-iam-role-events-route-by-event-name-and-only-in-us-east-1|_collect#86|Timeout|msgs = sqs.receive_message(
Issue-Commit: f4f620b4bc8cb3b1a7871cc60a92566045773536

# cloudtrail__events__iam__sqs: no 'AWS API Call via CloudTrail' event is published for global IAM calls, so an eventName-routed rule in us-east-1 never delivers and the check burns its whole budget

> _Note: this issue was identified against the code base at commit `f4f620b4bc8cb3b1a7871cc60a92566045773536`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutEvents

## Description
Cause 1 of 1: management API calls recorded by a logging CloudTrail trail are never republished onto the EventBridge default bus, so a rule whose pattern matches source=aws.iam / detail-type='AWS API Call via CloudTrail' / detail.eventName=CreateRole never fires. Failing check: cloudtrail__events__iam__sqs.xsvc-iam-1-iam-role-events-route-by-event-name-and-only-in-us-east-1.

AWS: with a multi-region trail that is logging write management events (GetTrailStatus.IsLogging=true), CloudTrail republishes each recorded management call onto the account's EventBridge default bus as an event with source '<aws.service>' (here 'aws.iam'), detail-type 'AWS API Call via CloudTrail', and a detail object that is the CloudTrail record — including detail.eventSource ('iam.amazonaws.com'), detail.eventName ('CreateRole'), detail.requestParameters and detail.responseElements. Because IAM is a global service, those records are recorded and delivered in us-east-1 only, so a rule in another region must NOT receive them, while a regional call such as sqs:TagQueue is delivered in the region it was made. Recorded behavior only for the exact delivery latency; the model states the envelope members (cloudwatchevents/eventbridge PutEvents entry: Source, DetailType, Detail) that such an event occupies.

Emulator: no such event is ever produced. The check creates a logging trail, puts two queue-targeted rules in us-east-1 (one on eventName=CreateRole, one on DeleteRole) plus a comparison rule in a second region, then calls iam:CreateRole and sqs:TagQueue and long-polls both queues, re-emitting the subject calls every 120 s. Neither the IAM CreateRole event nor the SQS TagQueue event is ever delivered, so the loop spins to its 1000 s cold-delivery budget and the check is cut off by the harness cap first: 'Error: Check timed out after 60.0s'. The trail accepts creation and reports IsLogging=true, so the gap is in the trail -> default-bus publication, not in trail admission.

Why: a client that wires audit-driven automation the documented way — a trail for write management events plus an EventBridge rule on 'AWS API Call via CloudTrail' filtered by detail.eventName — receives nothing. Role-creation/deletion alarms, compliance reactions and remediation Lambdas never run, and because the absence manifests as silence rather than an error the client blocks on its delivery wait instead of failing. The us-east-1-only routing contract for global services is likewise unverifiable: a rule that should receive and a rule that should not both receive nothing, so a consumer cannot tell correct scoping from total non-delivery.

Fix: the CloudTrail -> EventBridge publication path: after a recorded management call is written for an enabled trail, the trail's delivery component must put a corresponding entry on the default event bus of the recording region (us-east-1 for global services such as IAM, the call's own region for regional services such as SQS) with source 'aws.<service>', detail-type 'AWS API Call via CloudTrail' and the CloudTrail record as detail, so EventBridge's existing rule matching and SQS target delivery carry it the rest of the way.

NOTE: open issues 20261119T154334Z-cloudtrail-dynamodb-events-sqs-no-aws-api-call.md (aws.dynamodb) and 20261029T173328Z-cloudtrail-records-and-delivers-no-audit-record-for.md describe the same family for other sources, but neither carries this check's replay key, so the IAM/global-scoping scenario is filed here.

Traces: cloudtrail__events__iam__sqs.xsvc-iam-1-iam-role-events-route-by-event-name-and-only-in-us-east-1.

## Evidence
Expected: within the check's cold-delivery budget, the us-east-1 rule whose pattern is {"source":["aws.iam"],"detail-type":["AWS API Call via CloudTrail"],"detail":{"eventSource":["iam.amazonaws.com"],"eventName":["CreateRole"]}} delivers one message to its SQS target carrying the CreateRole record (source='aws.iam', detail-type='AWS API Call via CloudTrail', detail.eventSource='iam.amazonaws.com', detail.eventName='CreateRole'), the sibling DeleteRole rule receives nothing for that call, and the comparison-region rule receives the regional sqs:TagQueue event but no IAM event.

Actual: zero messages reach any of the three queues. The delivery loop never satisfies its predicate and the check is terminated by the runner: 'Error: Check timed out after 60.0s'. The trail itself reports IsLogging=true with no LatestDeliveryError, so the calls are accepted but no 'AWS API Call via CloudTrail' entry is ever placed on the default bus.

## Steps to Reproduce
1. In us-east-1 create an S3 bucket and a multi-region CloudTrail trail over it with an event selector for write-only management events; StartLogging and confirm GetTrailStatus reports IsLogging=true.
2. Create an SQS queue in us-east-1 and two EventBridge rules on the default bus targeting it: pattern A {"source":["aws.iam"],"detail-type":["AWS API Call via CloudTrail"],"detail":{"eventSource":["iam.amazonaws.com"],"eventName":["CreateRole"]}}, pattern B identical but eventName=DeleteRole. Grant both rules sqs:SendMessage on the queue.
3. Create a second queue and rule in another region (e.g. us-west-2) matching aws.iam/aws.sqs with eventName in [CreateRole, DeleteRole, TagQueue].
4. Call iam:CreateRole, then sqs:TagQueue on the comparison-region queue.
5. Long-poll both queues: no 'AWS API Call via CloudTrail' message ever arrives for either call.
6. Replay check cloudtrail__events__iam__sqs.xsvc-iam-1-iam-role-events-route-by-event-name-and-only-in-us-east-1 to confirm.
