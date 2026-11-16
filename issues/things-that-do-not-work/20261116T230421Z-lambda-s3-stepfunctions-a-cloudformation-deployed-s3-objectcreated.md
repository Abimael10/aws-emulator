Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda__s3__stepfunctions.cfn-deploy-s3-notification-to-lambda-positive
Issue-Key: lambda__s3__stepfunctions.cfn-deploy-s3-notification-to-lambda-positive|_poll_until#13|Timeout|time.sleep(interval_s)
Issue-Commit: 058978cf46c9798d44feed3f38358d3fb4a774eb

# lambda__s3__stepfunctions: a CloudFormation-deployed S3 ObjectCreated notification never produces the Lambda's processed/ marker, so the check never reaches a verdict

> _Note: this issue was identified against the code base at commit `058978cf46c9798d44feed3f38358d3fb4a774eb`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutBucketNotificationConfiguration

## Description
Cause 1 of 1: after a CloudFormation stack deploys a bucket, a function and the bucket's ObjectCreated notification, a PutObject under uploads/ never results in the function's output object, so the check makes no progress and is cut off at the harness budget.

AWS: a stack that creates an S3 bucket with a NotificationConfiguration LambdaConfiguration for s3:ObjectCreated:* (plus the AWS::Lambda::Permission with Principal s3.amazonaws.com) reaches CREATE_COMPLETE with the notification live. A subsequent PutObject(Bucket=<stack bucket>, Key='uploads/<name>') is delivered to the function as an s3 event record, and the handler writes its derived object — here 'processed/<name>' whose body is 'processed:uploads/<name>' — which ListObjectsV2(Prefix='processed/') then reports. (recorded behavior: the delivery and the handler's write are the check's subject; the model's PutBucketNotificationConfiguration carries no message literal here.)
Emulator: nothing ever appears under processed/. The check polls ListObjectsV2(Prefix='processed/') every 5 s for up to 120 s and makes no progress, so the harness ends it: 'Check timed out after 60.0s'. No refusal, no error body — the notification simply produces no invocation output.
Why: a client that wires an S3 ObjectCreated notification to a Lambda through CloudFormation sees the stack succeed and then nothing happen: the event-driven leg of the deployment is silently dead, which is worse than a refusal because the deployment reports healthy.
Fix: the S3 bucket-notification dispatch to Lambda and the Lambda invocation path it drives (the notification registered by the stack must fire on ObjectCreated and the invoked function's writes must land), and whatever in the CloudFormation provisioner registers that notification on the created bucket.
Note on neighbouring work: the open issue 20261110T140310Z-lambda-s3-stepfunctions-an-s3-objectcreated-bucket-notification.md owns the sibling check s3-notification-triggers-lambda-positive (the same service, the notification configured directly rather than by a stack). This check is not carried by that issue's replay keys and is reported here so its CloudFormation-deployed variant is re-replayed too; if the fixer finds one dispatch defect behind both, closing it closes this one.
Traces: lambda__s3__stepfunctions.cfn-deploy-s3-notification-to-lambda-positive

## Evidence
lambda__s3__stepfunctions.cfn-deploy-s3-notification-to-lambda-positive -> Error: Check timed out after 60.0s
The check's shape: _deploy_cfn(template 's3-notification-cfn.yaml') -> read Outputs['BucketName'] -> PutObject(Key='uploads/cfn-test.txt') -> _poll_until(ListObjectsV2(Prefix='processed/'), len>0, timeout_s=120, interval_s=5) -> assert 'processed/cfn-test.txt' present with body b'processed:uploads/cfn-test.txt'.
Expected: the processed/ object exists shortly after the upload. Actual: the listing stays empty for the whole run, so the check is still polling when the harness budget expires. (The 60 s budget is the harness's; the observable is that no object ever lands under processed/ — not the particular deadline.)

## Steps to Reproduce
1. Replay the behavioral suite and run lambda__s3__stepfunctions.cfn-deploy-s3-notification-to-lambda-positive.
2. Minimal reproduction: CreateStack with a template that creates an S3 bucket, a Lambda function that writes 'processed/<basename>' with body 'processed:<key>' for each s3 event record, the AWS::Lambda::Permission for Principal s3.amazonaws.com, and the bucket's NotificationConfiguration LambdaConfiguration on s3:ObjectCreated:*.
3. After CREATE_COMPLETE, PutObject(Bucket=<stack bucket>, Key='uploads/cfn-test.txt') and poll ListObjectsV2(Bucket=<bucket>, Prefix='processed/'): the prefix stays empty, where AWS lists the marker object written by the invoked function.
