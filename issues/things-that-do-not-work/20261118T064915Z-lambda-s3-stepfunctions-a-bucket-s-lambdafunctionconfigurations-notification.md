Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda__s3__stepfunctions.s3-notification-triggers-lambda-positive
Issue-Key: lambda__s3__stepfunctions.s3-notification-triggers-lambda-positive|_poll_until#12|Timeout|time.sleep(interval_s)
Issue-Commit: 1b8db06d77ea9b8946e423ddb778f8ee81626f53

# lambda__s3__stepfunctions: a bucket's LambdaFunctionConfigurations notification never invokes the function, so the handler's processed/ marker never appears (check killed at the 60s cap)

> _Note: this issue was identified against the code base at commit `1b8db06d77ea9b8946e423ddb778f8ee81626f53`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutBucketNotificationConfiguration

## Description
One net-new root cause, grounded in a failing check this replay marks neither ALREADY FILED nor OWNED. Open issue 20261107T185637Z-lambda-s3-stepfunctions-2-net-new-causes-s3-objectcreated-notifications-never.md describes the same area and even names this check, but it does NOT carry this check's current replay key (it was filed against an AssertionError raised inside the check's own _poll_until; the check now dies on the harness deadline before that assertion is ever reached), so this replay is not covered by it. Open issues 20261116T230421Z-lambda-s3-stepfunctions-a-cloudformation-deployed-s3-objectcreated.md and 20261117T134237Z-lambda-s3-the-armed-s3-objectcreated-notification-csv-conversion-scenario.md cover the CloudFormation-deployed and the armed-CSV variants, not this direct-API one.

Cause 1 of 1: PutBucketNotificationConfiguration accepts a LambdaFunctionConfigurations entry that is never dispatched.
AWS: Smithy (com.amazonaws.s3) LambdaFunctionConfiguration.LambdaFunctionArn is documented as "The Amazon Resource Name (ARN) of the Lambda function that Amazon S3 invokes when the specified event type occurs"; LambdaFunctionConfiguration.Events is required and accepts s3:ObjectCreated:*, and LambdaFunctionConfiguration.Filter -> NotificationConfigurationFilter -> S3KeyFilter -> FilterRule{Name=prefix,Value=<prefix>} narrows delivery to keys under that prefix. With lambda:InvokeFunction granted to Principal s3.amazonaws.com with the bucket ARN as SourceArn (Lambda AddPermission), a PutObject of a key under the configured prefix causes S3 to invoke the function asynchronously, within seconds, with the standard envelope (Records[0].eventSource 'aws:s3', eventName 'ObjectCreated:Put', s3.bucket.name, s3.object.key URL-encoded, size, eTag, sequencer). Side effects the handler performs — here a PutObject of processed/<filename> back into the same bucket — are therefore observable by ListObjectsV2 shortly after the upload. PutBucketNotificationConfiguration and AddPermission both answered 200 in this recording, so the configuration was accepted.
Emulator: the configuration and the invoke grant are accepted (HTTP 200 each) and the upload succeeds, but no invocation ever happens: ListObjectsV2 on prefix 'processed/' keeps returning an empty Contents for the whole run, and the check is killed by the harness with 'Check timed out after 60.0s' before its own 90s poll ("No processed/ object found — Lambda was not triggered") can even report. No error, no refusal, no partial delivery — the notification is silently dropped.
Why: a client wiring the single most common serverless trigger (S3 ObjectCreated -> Lambda) gets a 200 from every configuration call and then nothing ever runs. Because the accepted-and-silent path is indistinguishable from the correct one, no downstream consumer can detect the breakage, and every negative variant (prefix filter withholding a non-matching key, a function lacking the invoke grant) is unfalsifiable: the matching, permitted case produces exactly the same empty result.
Fix: the S3 notification dispatcher's Lambda destination — matching a stored event against the bucket's LambdaFunctionConfigurations and their S3Key FilterRules, evaluating the target function's resource policy against the bucket SourceArn, serializing the Records[] envelope, and issuing the asynchronous Invoke. The PutBucketNotificationConfiguration handler itself only stores the configuration; the gap is on the event-emission side of PutObject.
Traces: lambda__s3__stepfunctions.s3-notification-triggers-lambda-positive.

## Evidence
Check lambda__s3__stepfunctions.s3-notification-triggers-lambda-positive, verdict mismatch.

Expected (AWS): after PutBucketNotificationConfiguration with LambdaFunctionConfigurations=[{LambdaFunctionArn=<fn>, Events=['s3:ObjectCreated:*'], Filter.Key.FilterRules=[{Name:'prefix',Value:'uploads/'}]}] and AddPermission(Action='lambda:InvokeFunction', Principal='s3.amazonaws.com', SourceArn=<bucket arn>), a PutObject of 'uploads/test-file.txt' invokes the function, which writes 'processed/test-file.txt'; ListObjectsV2(Prefix='processed/') returns that key within seconds.

Actual (emulator): ListObjectsV2(Prefix='processed/') returns no Contents for the entire run; the check never observes an invocation and is terminated by the harness — 'Error: Check timed out after 60.0s'. The check's own assertion message for this state is "No processed/ object found — Lambda was not triggered". No error status or error code is returned anywhere in the flow: PutBucketNotificationConfiguration, AddPermission and PutObject all succeed, so the divergence is a silently dropped event rather than a refusal.

## Steps to Reproduce
1) CreateBucket.
2) Create an IAM role trusted by lambda.amazonaws.com with s3:GetObject on <bucket>/* and s3:PutObject on <bucket>/processed/*, and CreateFunction (python3.12) whose handler re-writes each record's key to processed/<filename> via boto3 s3.put_object; wait for State=Active.
3) lambda AddPermission(FunctionName=<fn>, StatementId='s3-invoke', Action='lambda:InvokeFunction', Principal='s3.amazonaws.com', SourceArn='arn:aws:s3:::<bucket>').
4) s3 PutBucketNotificationConfiguration(Bucket=<bucket>, NotificationConfiguration={LambdaFunctionConfigurations:[{LambdaFunctionArn:<fn arn>, Events:['s3:ObjectCreated:*'], Filter:{Key:{FilterRules:[{Name:'prefix',Value:'uploads/'}]}}}]}).
5) s3 PutObject(Bucket=<bucket>, Key='uploads/test-file.txt', Body='hello').
6) Poll s3 ListObjectsV2(Bucket=<bucket>, Prefix='processed/'). On AWS the processed/test-file.txt key appears within seconds; on the emulator Contents stays empty until the check is killed at the 60s harness cap.
