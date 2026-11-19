Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda__s3__stepfunctions.s3-notification-triggers-lambda-positive lambda__s3__stepfunctions.s3-notification-payload-format-preserved lambda__s3__stepfunctions.s3-event-filter-prefix-negative lambda__s3__stepfunctions.s3-notification-missing-invoke-permission-negative lambda__s3__stepfunctions.cfn-deploy-s3-notification-to-lambda-positive lambda__s3__stepfunctions.s3-trigger-lambda-starts-sfn-pipeline lambda__s3__stepfunctions.lambda-starts-sfn-missing-permission-negative lambda__s3__stepfunctions.sfn-pipeline-lambda-broken-s3-permission-negative
Issue-Key: lambda__s3__stepfunctions.s3-notification-triggers-lambda-positive|_poll_until#12|AssertionError|raise AssertionError(
Issue-Key: lambda__s3__stepfunctions.s3-notification-payload-format-preserved|_poll_until#13|AssertionError|raise AssertionError(
Issue-Key: lambda__s3__stepfunctions.s3-event-filter-prefix-negative|_poll_until#15|AssertionError|raise AssertionError(
Issue-Key: lambda__s3__stepfunctions.s3-notification-missing-invoke-permission-negative|_poll_until#22|AssertionError|raise AssertionError(
Issue-Key: lambda__s3__stepfunctions.cfn-deploy-s3-notification-to-lambda-positive|_poll_until#13|AssertionError|raise AssertionError(
Issue-Key: lambda__s3__stepfunctions.s3-trigger-lambda-starts-sfn-pipeline|_poll_until#23|AssertionError|raise AssertionError(
Issue-Key: lambda__s3__stepfunctions.lambda-starts-sfn-missing-permission-negative|_poll_until#17|AssertionError|raise AssertionError(
Issue-Key: lambda__s3__stepfunctions.sfn-pipeline-lambda-broken-s3-permission-negative|_run_until#26|AssertionError|raise AssertionError(
Issue-Commit: d6cb6c274eaec5e2dce099ad9d2ddf00c4faf8ea

# 2 net-new causes: S3 ObjectCreated notifications never invoke the function, and the Lambda runtime's bundled SDK has no s3/stepfunctions client

> _Note: this issue was identified against the code base at commit `d6cb6c274eaec5e2dce099ad9d2ddf00c4faf8ea`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutBucketNotificationConfiguration

## Description
Two root causes, both grounded in failing checks this report does NOT mark ALREADY FILED or OWNED. Open issues 20261105T081644Z-lambda-s3-stepfunctions-the-lambda-python-runtime-ships.md and 20261029T183117Z-lambda-s3-sfn-four-net-new-causes-raw.md own OTHER checks of this service and do not carry the replay keys below.

Cause 1 of 2: a bucket's LambdaFunctionConfigurations notification never invokes the function.
AWS: with a LambdaFunctionConfigurations entry for s3:ObjectCreated:* (optionally with a prefix/suffix FilterRule) and lambda:InvokeFunction granted to s3.amazonaws.com with the bucket as SourceArn, PutObject invokes the function asynchronously within seconds with the standard notification envelope (Records[0].eventSource 'aws:s3', eventName 'ObjectCreated:Put', s3.bucket.name, s3.object.key URL-encoded, size, eTag, sequencer). A key outside the configured prefix is not delivered, and a function without the permission is not invoked.
Emulator: nothing is delivered on any path, including the explicitly calibrated positive ones: "condition not met within 90s. Last result: []" / "... Last result: None", "Lambda triggered for matching uploads/ prefix not met within 90s. Last result: []", "Lambda invocation marker after granting invoke permission not met within 90s. Last result: []", "condition not met within 120s. Last result: []" (CloudFormation-deployed wiring).
Why: the most common serverless trigger does nothing, the notification payload shape cannot be verified, and both negatives (prefix filter, missing permission) are unfalsifiable because the matching case never fires either.
Fix: the S3 notification dispatcher's Lambda destination — event matching against the bucket's configurations and FilterRules, the function resource-policy check against the bucket SourceArn, the notification-envelope serializer, and the asynchronous Invoke; plus the CloudFormation path that installs a bucket NotificationConfiguration.
Traces: lambda__s3__stepfunctions.s3-notification-triggers-lambda-positive, .s3-notification-payload-format-preserved, .s3-event-filter-prefix-negative, .s3-notification-missing-invoke-permission-negative, .cfn-deploy-s3-notification-to-lambda-positive, .s3-trigger-lambda-starts-sfn-pipeline.

Cause 2 of 2: function code cannot build clients for s3 or stepfunctions.
AWS: the managed python runtime's boto3 builds a client for any service; sfn.start_execution from a handler returns an executionArn (or raises ClientError AccessDeniedException when states:StartExecution is not granted), and s3.put_object writes the object (or raises AccessDenied).
Emulator: the handler faults before any call with errorType 'UnknownServiceError' and errorMessage "Unknown service: 'stepfunctions'. Valid service names are: sqs, events, eventbridge, dynamodb, logs, cloudwatchlogs, apigateway, secretsmanager" (and the same for 's3'), raised from the bundled client.py _raise_refusal; the state-machine execution consequently ends FAILED with error 'UnknownServiceError'. The enumerated service list is the emulator's own registry and is itself the defect.
Why: no function can start an execution or write an object, and every IAM negative is unfalsifiable because the granted and the denied case both produce the identical UnknownServiceError; a state machine also reports a failure cause AWS never emits.
Fix: the in-process Lambda runtime's bundled AWS SDK shim — its service registry and the client factory that refuses unknown services.
Traces: lambda__s3__stepfunctions.lambda-starts-sfn-missing-permission-negative, .sfn-pipeline-lambda-broken-s3-permission-negative.

## Evidence
Cause 1: expected the function invoked on PutObject with the standard Records[] envelope; got nothing — 'condition not met within 90s. Last result: []' (and []/None on the prefix-filter, permission and CloudFormation-deployed variants, 120s).
Cause 2: expected an executionArn (or AccessDeniedException) from the handler; got errorType 'UnknownServiceError', errorMessage "Unknown service: 'stepfunctions'. Valid service names are: sqs, events, eventbridge, dynamodb, logs, cloudwatchlogs, apigateway, secretsmanager", and the owning execution FAILED with error 'UnknownServiceError'.

## Steps to Reproduce
1) Cause 1: PutBucketNotificationConfiguration with a LambdaFunctionConfigurations entry (with and without a prefix FilterRule), AddPermission for s3.amazonaws.com with the bucket SourceArn, PutObject inside and outside the prefix, and poll for invocations and the record payload.
2) Cause 2: Invoke a handler calling boto3.client('stepfunctions').start_execution and boto3.client('s3').put_object, with the grant present and absent.

## What was fixed, and what is still open

Both causes are served now. The notification dispatcher's Lambda destination delivers — the
envelope, the key-name filter, the invoke grant and the handler's own calls to Step Functions and
Amazon S3 are all exercised by the recorded units that pass — and the bundled SDK's reachable-service
table carries `s3`, `stepfunctions` and `sfn`
(`services/service-com-amazonaws-lambda-awsgirapiservice/src/state/execution/aws_api.rs`), so a
handler builds a client for either and the two IAM negatives are falsifiable again.

One defect of that dispatcher was found, fixed and pinned in this change: a delivery carried NO
request chain, and neither did a handler's own call to Amazon S3 (or to CloudFront, Amazon SNS, the
token service, Amazon RDS, CloudFormation), because only the AWS-JSON and `restJson1` dispatch path
stamped the invocation's tracing header. Every hop of a Lambda-to-S3-to-Lambda loop therefore looked
like the start of a new chain, so AWS Lambda's recursive-loop guardrail — which this emulator already
implements — never saw one: a single `PutObject` into a bucket whose unfiltered configuration its own
handler writes back into produced fifty invocations and counting, each holding a run slot, a permit
of the one pool every bucket's deliveries share, and an interpreter, where AWS stops the chain at
sixteen. A delivery is now a hop of the chain the call that produced the event belongs to, so such a
loop is stopped at the published bound
(`services/service-com-amazonaws-s3-amazons3/src/state/notifications.rs`,
`services/common/src/xray_trace.rs`, pinned by
`core/router/tests/all/s3_notification_delivery_is_a_hop_of_its_chain.rs`).

STILL OPEN: two recorded checks of this fixture are NOT demonstrated passing —
`.s3-notification-triggers-lambda-positive` and `.cfn-deploy-s3-notification-to-lambda-positive`,
both of which poll `ListObjectsV2(Prefix='processed/')` for an object the invoked handler writes back
into the bucket the event came from. Their replay reports "Check timed out after 60.0s", and their
CURRENT replay keys are owned by the open issues
20261118T064915Z-lambda-s3-stepfunctions-a-bucket-s-lambdafunctionconfigurations-notification.md and
20261116T230421Z-lambda-s3-stepfunctions-a-cloudformation-deployed-s3-objectcreated.md. What was
driven and PASSES in process and on the started binary while tracing them: that exact pipeline with
the role scoped the way the recording scopes it (`s3:GetObject` on `<bucket>/*`, `s3:PutObject` on
`<bucket>/processed/*`), at the function's default `Timeout` and at the smallest the model allows,
with the client built inside the handler as well as at module scope, with the configuration filtered
and unfiltered, with the handler's marker read back through `ListObjectsV2` spelled as botocore
spells it (`prefix=processed%2F`, `encoding-type=url`) as well as with a bare slash, and with the
loop the unfiltered form produces. None of those reproduces the recording's outcome, so what the
recording does between its grant and its poll that this emulator answers differently is still not
identified; whoever picks those two up next should start from the client's own wire rather than from
the dispatcher.

