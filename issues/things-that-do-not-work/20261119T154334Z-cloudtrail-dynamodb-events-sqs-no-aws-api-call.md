Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudtrail__dynamodb__events__sqs.xsvc-dynamodb-3-create-table-reaches-only-its-rule-not-the-delete-table-near-miss
Issue-Key: cloudtrail__dynamodb__events__sqs.xsvc-dynamodb-3-create-table-reaches-only-its-rule-not-the-delete-table-near-miss|_collect#66|Timeout|msgs = sqs.receive_message(
Issue-Commit: 95422a199f71de611c9b4568260a34e03a6bfa1c

# cloudtrail__dynamodb__events__sqs: no "AWS API Call via CloudTrail" event is ever published to the default bus, so a rule on aws.dynamodb CreateTable never delivers and the check cannot complete

> _Note: this issue was identified against the code base at commit `95422a199f71de611c9b4568260a34e03a6bfa1c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateTable

## Description
Cause 1 of 1: a logging CloudTrail trail with a WriteOnly / IncludeManagementEvents event selector produces no CloudTrail-derived EventBridge events at all, so no EventBridge rule matching detail-type "AWS API Call via CloudTrail" ever fires and the pipeline the check builds never produces a single message.

AWS: recorded behavior only — neither the CloudTrail nor the EventBridge Smithy model describes the fan-out, it is a documented platform behavior. With a trail that IsLogging=true and an event selector {ReadWriteType: WriteOnly, IncludeManagementEvents: true}, every write management API call in the account/region is additionally published to the region's DEFAULT EventBridge bus as an envelope whose `source` is "aws.<service>" (here "aws.dynamodb" for a DynamoDB call, "aws.sqs" for sqs:TagQueue), whose `detail-type` is the fixed string "AWS API Call via CloudTrail", whose `account` and `region` are the calling account and the call's own region, and whose `detail` is the CloudTrail record itself: detail.eventSource = "dynamodb.amazonaws.com", detail.eventName = the called operation ("CreateTable", "DeleteTable"), detail.recipientAccountId = the account, detail.awsRegion = the call's region, detail.readOnly = false and detail.eventCategory = "Management" for a write management call, and detail.requestParameters carrying the request's own members (requestParameters.tableName = the TableName sent on the call). DynamoDB publishes no native EventBridge events, so this CloudTrail fan-out is the ONLY path from a DynamoDB control-plane call to the default bus. A rule whose pattern names detail.eventName CreateTable receives the create and not the delete; a sibling rule naming DeleteTable receives the delete and not the create. In every value above the fixed part is the member name and the literal detail-type/eventSource/eventCategory strings; the table name, account id, region and queue names are taken from the request and the caller's own identity and are never fixed text.

Emulator: nothing is ever delivered. The check first proves the channel live with an unrelated control write (sqs:TagQueue on its own queue, matched by a rule on source aws.sqs / detail.eventName TagQueue) and polls that sink; no envelope arrives, so the check never gets past its control leg and is killed by the harness ceiling — the replay reports only `Error: Check timed out after 60.0s`, with no DynamoDB call having been made yet. The emulator accepts CreateTrail, PutEventSelectors and StartLogging and reports IsLogging=true, then publishes no CloudTrail-sourced event for any subsequent write call, so the CreateTable/DeleteTable routing and the near-miss isolation it asserts are all unreachable.

Why: a client that wires auditing or reactive automation onto control-plane activity — "run this when a table is created", "alarm when something is deleted" — sees its rules silently never fire against the emulator. The operations all succeed and the trail reports itself healthy, so the failure is invisible: there is no error to catch, only an event that never arrives, which makes every such pipeline appear broken in the user's own code rather than in the emulator. Absence assertions in the opposite direction also become vacuously true, hiding over-delivery bugs.

Fix: the CloudTrail management-event recorder must, for each recorded write management event on a logging trail whose selectors include it, additionally publish the record to the region's default EventBridge bus wrapped in the "AWS API Call via CloudTrail" envelope (source aws.<service>, detail = the CloudTrail record including eventSource/eventName/readOnly/eventCategory/recipientAccountId/awsRegion/requestParameters), from which the existing EventBridge rule-matching and target-delivery path takes over. This belongs in the CloudTrail trail-delivery/recording component and its hand-off into the EventBridge bus, not in the DynamoDB or SQS handlers.

Related open issues describe the same missing hop for other sources — 20261112T213312Z-cloudtrail-cognito-idp-events-sqs-no-cloudtrail-sourced.md (cognito-idp), 20261112T222923Z-cloudtrail-events-iam-sqs-an-eventbridge-rule-on.md (IAM), 20261114T042353Z-cloudfront-events-cloudfront-control-plane-calls-produce-no.md (CloudFront) and 20261024T114720Z-cloudtrail-event-history-is-empty-for-everything-but.md — none of them carries this check's replay key, so it is filed here; a fix that covers the fan-out generally should close them together.

Traces: cloudtrail__dynamodb__events__sqs.xsvc-dynamodb-3-create-table-reaches-only-its-rule-not-the-delete-table-near-miss.

## Evidence
Check: cloudtrail__dynamodb__events__sqs.xsvc-dynamodb-3-create-table-reaches-only-its-rule-not-the-delete-table-near-miss
Replay result: [mismatch] — `Error: Check timed out after 60.0s`

Expected (AWS): after CreateTrail + PutEventSelectors(ReadWriteType=WriteOnly, IncludeManagementEvents=true) + StartLogging (GetTrailStatus.IsLogging == true), an sqs:TagQueue write lands on the default bus as {source: "aws.sqs", detail-type: "AWS API Call via CloudTrail", detail.eventName: "TagQueue"} and reaches the rule scoped to it; then a dynamodb:CreateTable lands as {source: "aws.dynamodb", detail-type: "AWS API Call via CloudTrail", account: <caller account>, region: <call region>, detail: {eventSource: "dynamodb.amazonaws.com", eventName: "CreateTable", recipientAccountId: <caller account>, awsRegion: <call region>, readOnly: false, eventCategory: "Management", requestParameters: {tableName: <TableName sent>}}} and reaches only the CreateTable-scoped rule; a following dynamodb:DeleteTable reaches only the DeleteTable-scoped rule.

Actual (emulator): zero messages in any of the three sinks. The control leg (sqs:TagQueue, re-emitted every 120 s against an 1800 s budget) never observes a single envelope, so the check is cut off by the 60 s harness ceiling before any DynamoDB call is issued. The trail itself reports IsLogging=true, i.e. the emulator claims a healthy logging trail while publishing no CloudTrail-sourced event to the default bus.

## Steps to Reproduce
1. Create an S3 bucket with a bucket policy granting cloudtrail.amazonaws.com s3:GetBucketAcl on the bucket and s3:PutObject on AWSLogs/<account-id>/* (with the bucket-owner-full-control condition).
2. cloudtrail CreateTrail(Name=<trail>, S3BucketName=<bucket>, IsMultiRegionTrail=false, IncludeGlobalServiceEvents=false); PutEventSelectors(TrailName=<trail arn>, EventSelectors=[{ReadWriteType: "WriteOnly", IncludeManagementEvents: true}]); StartLogging; confirm GetTrailStatus reports IsLogging=true.
3. Create three SQS queues and three EventBridge rules on the DEFAULT bus, each with that queue as target and a queue policy allowing events.amazonaws.com to SendMessage:
   - {"source":["aws.sqs"],"detail-type":["AWS API Call via CloudTrail"],"detail":{"eventSource":["sqs.amazonaws.com"],"eventName":["TagQueue"]}}
   - {"source":["aws.dynamodb"],"detail-type":["AWS API Call via CloudTrail"],"detail":{"eventSource":["dynamodb.amazonaws.com"],"eventName":["CreateTable"]}}
   - the same with eventName DeleteTable.
4. Call sqs TagQueue on the first queue and poll its sink. On AWS an "AWS API Call via CloudTrail" envelope with source aws.sqs arrives; on the emulator nothing arrives and the poll runs until the harness kills the check.
5. (Unreachable on the emulator) dynamodb CreateTable, wait ACTIVE, poll the second sink for source aws.dynamodb / detail.eventName CreateTable with detail.requestParameters.tableName equal to the table created; then DeleteTable and poll the third sink; assert neither DynamoDB sink holds the other's eventName and the aws.sqs sink holds no aws.dynamodb event.
