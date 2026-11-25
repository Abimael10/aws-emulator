Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda__s3.armed-notification-converts-csv-and-suppresses-uppercase-suffix
Issue-Key: lambda__s3.armed-notification-converts-csv-and-suppresses-uppercase-suffix|_watch_absent#26|Timeout|return found
Issue-Commit: 2c87622153d52c1735efdd41ae2a08ec1437d782

# lambda__s3: an armed S3 ObjectCreated -> Lambda notification never delivers, so the CSV converter never runs and the case-sensitive suffix filter is unobservable

> _Note: this issue was identified against the code base at commit `2c87622153d52c1735efdd41ae2a08ec1437d782`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutBucketNotificationConfiguration

## Description
Cause 1 of 1: a bucket notification configuration naming a Lambda function is accepted, but no object creation ever invokes the function, so the whole event-driven leg of the scenario produces nothing and the check runs out its deadline without a verdict.

Related but NOT covering (name them so the fix lands once): 20261029T180026Z-dynamodb-lambda-s3-s3-notifications-never-invoke-lambda-are-accepted-without.md, 20261123T111903Z-iam-lambda-s3-the-csv-to-xlsx-s3-notification-pipeline-lifecycle.md, 20261118T064915Z-lambda-s3-stepfunctions-a-bucket-s-lambdafunctionconfigurations-notification.md and 20261116T230421Z-lambda-s3-stepfunctions-a-cloudformation-deployed-s3-objectcreated.md all record undelivered S3->Lambda notifications on other checks; none carries this check's replay key, and none covers the key-filter half of this cause.

AWS: after PutBucketNotificationConfiguration (Smithy, services/service-com-amazonaws-s3-amazons3: PUT /{Bucket}?notification, success 200, no modeled errors; NotificationConfiguration.LambdaFunctionConfigurations -> LambdaFunctionConfiguration with required CloudFunction (LambdaFunctionArn) and required Event list, optional Filter -> NotificationConfigurationFilter -> FilterRule{Name: prefix|suffix, Value}) S3 invokes the named function asynchronously for every object creation matching the event type and the key filter, provided the function has a lambda:InvokeFunction resource policy for Principal s3.amazonaws.com with the bucket SourceArn. AWS documents that a notification-configuration change takes effect in "usually about five minutes"; events that fire before it arms are dropped and not redelivered, which is why the check re-uploads a NEW key every 15 s round. Filter rules are literal string prefix/suffix matches on the key name and are case-SENSITIVE (Smithy FilterRule doc: "The value that the filter searches for in object key names"; prefix is a string match, not a directory boundary), so suffix `.csv` must NOT match a key ending `.CSV`, and prefix `raw/` MUST match `raw/nested/x.csv`.

Emulator: PutBucketNotificationConfiguration is accepted, AddPermission for s3.amazonaws.com is accepted, the function reaches Active — and then no PutObject under `raw/` ever produces any object under the handler's `converted/` output prefix. The check uploads a fresh `raw/<token>-fresh<n>.csv` and `raw/nested/<token>-deep<n>.csv` each round and polls ListObjectsV2 on `converted/`; the prefix stays empty, so the loop never exits and the run ends with `Check timed out after 60.0s` with no assertion reached. Because the positive control never arrives, the negative control is unobservable too: the emulator is never shown to apply the literal, case-sensitive suffix rule that must suppress `raw/<token>-case<n>.CSV` (the handler's own key.lower().endswith('.csv') guard would accept it, which is exactly what isolates S3's filter from the handler).

Why: a client that wires the canonical S3 -> Lambda ETL (upload under a prefix, function writes a derived object) gets a 200 from every setup call and a pipeline that silently does nothing — no invocation, no output object, no error to diagnose. Code that waits for the derived object hangs forever rather than failing. Because the filter is never exercised, a client also cannot discover whether case-sensitive suffix matching holds, so a `.CSV` upload may later be processed where AWS would suppress it.

Fix: the S3 notification dispatcher that must fire on object creation and call Lambda Invoke (event type match for s3:ObjectCreated:*, async invoke against the configured LambdaFunctionArn, authorised by the function's s3.amazonaws.com resource policy), plus the notification key-filter matcher in that dispatch path, which must apply prefix/suffix FilterRules as literal, case-sensitive string matches on the full key.

Traces: lambda__s3.armed-notification-converts-csv-and-suppresses-uppercase-suffix (mismatch, timed out with no verdict).

## Evidence
Expected (AWS): within the notification's arming window, a PutObject of `raw/<token>-fresh1.csv` into the configured bucket invokes the converter function, which writes `converted/<token>-fresh1.xlsx` (an OPC zip containing [Content_Types].xml and xl/worksheets/sheet1.xml whose sheet rows equal the probe CSV's rows). `raw/nested/<token>-deep1.csv` also matches prefix `raw/` and produces `converted/<token>-deep1.xlsx`. `raw/<token>-case1.CSV` is SUPPRESSED by the literal, case-sensitive suffix rule `.csv` and produces nothing.

Actual (emulator): PutBucketNotificationConfiguration accepted (printed "[validation] configuration accepted on try 1"); ListObjectsV2 on prefix `converted/` returns no keys for the whole run; no invocation of the function is observable. The check's own propagation loop (uploading a new key every 15 s, polling every 5 s) never sees any new key, so the run ends as:
  Error: Check timed out after 60.0s
and neither of these assertions is ever reached:
  assert observed is not None, "no CSV uploaded under raw/ produced any object under 'converted/' ... the notification never armed"
  assert leaked == [], "keys the notification filter must SUPPRESS produced output"

## Steps to Reproduce
1. CreateBucket <bucket>.
2. CreateRole trusted by lambda.amazonaws.com with s3:GetObject/s3:PutObject on arn:aws:s3:::<bucket>/*; CreateFunction <fn> (python3.12) whose handler reads each Records[].s3.object.key, and for keys under `raw/` ending `.csv` writes `converted/<stem>.xlsx`; wait for State=Active.
3. lambda AddPermission FunctionName=<fn> StatementId=s3invoke Action=lambda:InvokeFunction Principal=s3.amazonaws.com SourceArn=arn:aws:s3:::<bucket>.
4. PutBucketNotificationConfiguration Bucket=<bucket> with LambdaFunctionConfigurations=[{LambdaFunctionArn:<fn arn>, Events:["s3:ObjectCreated:*"], Filter:{Key:{FilterRules:[{Name:prefix,Value:"raw/"},{Name:suffix,Value:".csv"}]}}}] — accepted.
5. PutObject <bucket>/raw/probe-fresh1.csv (body "id,name,value\n1,alpha,100\n2,beta,200\n") and <bucket>/raw/nested/probe-deep1.csv; also PutObject <bucket>/raw/probe-case1.CSV.
6. Poll ListObjectsV2 Bucket=<bucket> Prefix=converted/ — it stays empty: no invocation happened. Re-upload new keys over several minutes; still empty.
7. (Blocked by 6) Once delivery works, confirm `converted/probe-case1.xlsx` is NEVER created, i.e. suffix `.csv` does not match `.CSV`.
