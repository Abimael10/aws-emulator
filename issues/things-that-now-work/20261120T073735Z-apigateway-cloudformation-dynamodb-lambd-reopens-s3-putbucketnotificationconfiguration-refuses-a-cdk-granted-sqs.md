Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway__cloudformation__dynamodb__lambda__s3__ssm__stepfunctions.cdk-reference-estate-deploys-serves-and-destroys
Issue-Key: apigateway__cloudformation__dynamodb__lambda__s3__ssm__stepfunctions.cdk-reference-estate-deploys-serves-and-destroys|_run_cli#13|AssertionError|assert result.returncode == 0, (
Issue-Commit: 27aae757db5fa764a3702a0ead4409204ce3a274

# REOPENS: S3 PutBucketNotificationConfiguration refuses a CDK-granted SQS/SNS destination, so the CDK reference estate's S3EventNotif stack rolls back and `cdk deploy --all` fails

> _Note: this issue was identified against the code base at commit `27aae757db5fa764a3702a0ead4409204ce3a274`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutBucketNotificationConfiguration

## Description
REOPENS 20261029T185330Z-s3-putbucketnotificationconfiguration-accepts-an-sqs-destination-whose-queue.md: its fix left the call refused.

1 cause, grounded in one failing behavioral check.

Cause 1 of 1: PutBucketNotificationConfiguration refuses a notification configuration whose SQS and SNS destinations DO grant the s3.amazonaws.com service principal, so CDK's BucketNotifications custom resource reports FAILED and the stack rolls back.
Failing check ids for this cause: apigateway__cloudformation__dynamodb__lambda__s3__ssm__stepfunctions.cdk-reference-estate-deploys-serves-and-destroys.

AWS: Smithy (s3 model, com-amazonaws-s3-amazons3): PutBucketNotificationConfiguration is PUT /{Bucket}?notification with required members Bucket (httpLabel) and NotificationConfiguration (httpPayload), the optional header member SkipDestinationValidation (x-amz-skip-destination-validation), http success code 200, and an EMPTY modeled error list — so a destination-validation failure is carried by the S3 REST error envelope: HTTP 400, Error.Code `InvalidArgument`, Error.Message `Unable to validate the following destination configurations` with ArgumentName/ArgumentValue naming the destination it could not validate (fixed text: the sentence and the code; variable parts: the destination ARN(s) taken from the request). AWS emits that refusal ONLY when the destination's resource policy does not let S3 publish to it. When the queue policy allows Principal {"Service":"s3.amazonaws.com"} to sqs:SendMessage on the queue ARN and the topic policy allows SNS:Publish on the topic ARN — in either case optionally narrowed by a Condition on aws:SourceAccount / aws:SourceArn, which is exactly the shape CDK's SqsDestination and SnsDestination synthesize next to the notification — the call is accepted with HTTP 200 and an empty body, and a following GetBucketNotificationConfiguration reports the merged configuration (here one QueueConfiguration keyed `SampleQueueNotification` pointing at the queue ARN and one TopicConfiguration keyed `SampleSnsNotification` pointing at the topic ARN). Recorded behavior for the scenario as a whole: `cdk deploy --all` of this reference estate exits 0, every custom resource reports SUCCESS, and a `CategoryA/<key>` put is then delivered to the queue as an s3:ObjectCreated record.

Emulator: the call is refused. The CDK BucketNotificationsHandler function (stack-generated name `*-NotificationManagerFuncti-*`, per-run) raises inside its handler at index.js:28 and reports response status [FAILED] to CloudFormation with `Message returned: Unable to validate the following destination configurations`; the custom resource `SampleBucketNotificationA` (AWS::CloudFormation::CustomResource) ends FAILED, the `*-S3EventNotif` stack rolls back, and `cdk deploy --all` exits 1 where 0 is required. Because the write is refused, nothing downstream is observable: GetBucketNotificationConfiguration cannot report the two merged configurations, and no ObjectCreated record for the `CategoryA/` key ever reaches the queue.

Why: a client that follows AWS's contract — grant the destination policy, then write the notification configuration — is told its valid configuration cannot be validated. The refusal is indistinguishable from a genuinely missing queue/topic policy, so the operator is sent to debug a policy that is in fact correct. Every CDK or CloudFormation stack that wires a bucket notification through CDK's BucketNotifications custom resource fails to deploy and rolls back, which takes out the entire multi-stack estate, not just the notification.

Hypothesis: the destination validation introduced when the fixed issue named above was closed does not recognise a grant whose statement carries a Condition block (aws:SourceAccount equality / ArnLike aws:SourceArn) or one created by a sibling resource in the same stack, so every CDK-shaped grant reads as absent; the same handler also appears not to honour x-amz-skip-destination-validation. Note also fixed issue 20261109T185037Z-cloudformation-lambda-s3-sqs-the-in-lambda-aws-sdk-binds-no-s3.md, which made the in-Lambda SDK bind this operation at all: the call now reaches S3 and is refused there.

Fix: the s3 crate's PutBucketNotificationConfiguration handler and its destination-validation step — it must accept an SQS/SNS/Lambda destination whose resource policy allows the s3.amazonaws.com service principal, including statements narrowed by aws:SourceAccount / aws:SourceArn conditions, must honour the SkipDestinationValidation request header, and must emit the 400 InvalidArgument `Unable to validate the following destination configurations` refusal only when no such grant exists; on acceptance it must store and merge the configuration so GetBucketNotificationConfiguration reports it.

Traces: apigateway__cloudformation__dynamodb__lambda__s3__ssm__stepfunctions.cdk-reference-estate-deploys-serves-and-destroys (the estate's S3EventNotif stack; assertion helper _assert_custom_bucket_notifications).

## Evidence
Behavioral suite replayed against the emulator built from this checkout. apigateway__cloudformation__dynamodb__lambda__s3__ssm__stepfunctions scored 0/1 with 1 failure, reported as neither OWNED by an open issue nor blocked by an unimplemented stub.

[mismatch] apigateway__cloudformation__dynamodb__lambda__s3__ssm__stepfunctions.cdk-reference-estate-deploys-serves-and-destroys
  File "check_...cdk_reference_estate_deploys_serves_and_destroys.py", line 1141, in _cdk -> _run_cli(...)
  line 1152, in _run_cli: assert result.returncode == 0
  AssertionError: `cdk deploy --all` exited 1, expected 0; output tail:
    "...rce updates failed:\n<stack>-S3EventNotif/SampleBucketNotificationA/Default (AWS::CloudFormation::CustomResource SampleBucketNotificationA)\n  Received response status [FAILED] from custom resource. Message returned: Unable to validate the following destination configurations\n ... at exports.handler (/var/task/index.js:28:87) ..."

Expected (AWS): `cdk deploy --all` exits 0; the bucket's notification configuration reads back as exactly {"SampleQueueNotification": <queue ARN>, "SampleSnsNotification": <topic ARN>} and a `CategoryA/<key>` put is delivered to the queue.
Actual (emulator): PutBucketNotificationConfiguration is refused with the destination-validation refusal (`Unable to validate the following destination configurations`) although the CDK-synthesized queue policy grants s3.amazonaws.com sqs:SendMessage and the topic policy grants SNS:Publish; the custom resource ends FAILED and the stack rolls back.

The fixed text of the refusal is the sentence `Unable to validate the following destination configurations` and the code `InvalidArgument`; the destination ARNs are values from the request. The stack/function/log-stream names (`sim2real-ref-<hex>-S3EventNotif`, `*-NotificationManagerFuncti-*`, the RequestId and the log stream date) are per-run values and are not part of the requirement.

## Steps to Reproduce
1. Build and boot the emulator from this checkout.
2. Replay the behavioral suite (simparity-behavioral-aws-v1) and run apigateway__cloudformation__dynamodb__lambda__s3__ssm__stepfunctions.cdk-reference-estate-deploys-serves-and-destroys.
3. Minimal repro of the cause without CDK: sqs:CreateQueue(Q); sqs:SetQueueAttributes(Q, Policy = Allow Principal {"Service":"s3.amazonaws.com"} Action sqs:SendMessage Resource <Q arn> Condition {"StringEquals":{"aws:SourceAccount":"<account>"},"ArnLike":{"aws:SourceArn":"arn:aws:s3:::B"}}); sns:CreateTopic(T) with an equivalent SNS:Publish policy; s3:CreateBucket(B).
4. s3:PutBucketNotificationConfiguration(Bucket=B, NotificationConfiguration={"QueueConfigurations":[{"Id":"SampleQueueNotification","QueueArn":<Q arn>,"Events":["s3:ObjectCreated:*"]}],"TopicConfigurations":[{"Id":"SampleSnsNotification","TopicArn":<T arn>,"Events":["s3:ObjectCreated:*"]}]}).
   Expected: HTTP 200, empty body; GetBucketNotificationConfiguration(B) then reports both configurations.
   Observed: HTTP 400 InvalidArgument `Unable to validate the following destination configurations`, nothing stored.
5. Repeat step 4 with x-amz-skip-destination-validation=true: it must be accepted unconditionally.
