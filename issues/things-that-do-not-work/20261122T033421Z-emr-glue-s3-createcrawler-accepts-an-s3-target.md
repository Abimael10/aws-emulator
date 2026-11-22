Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: emr__glue__s3.cfn-deploy-glue-crawler-invalid-s3-path-negative
Issue-Key: emr__glue__s3.cfn-deploy-glue-crawler-invalid-s3-path-negative|run#23|AssertionError|assert invalid_deploy["status"] in {"ROLLBACK_COMPLETE", "CREATE_FAILED"}, invalid_deploy
Issue-Commit: aa87d0058386f57e2d9444188ae656a8c51d6f38

# emr__glue__s3: CreateCrawler accepts an S3 target inside a bucket that exists nowhere, so a CloudFormation AWS::Glue::Crawler stack AWS rolls back reaches CREATE_COMPLETE with no failures

> _Note: this issue was identified against the code base at commit `aa87d0058386f57e2d9444188ae656a8c51d6f38`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateCrawler

## Description
One net-new cause (Cause 1 of 1), reached through the emr__glue__s3 CloudFormation crawler check. The emulator ACCEPTS a call the recording shows AWS refusing, so an invalid-input deployment is reported as a healthy one.

Cause 1 of 1: CreateCrawler performs no create-time validation of its S3Targets paths, so a crawler over a bucket that does not exist is created and the CloudFormation stack that owns it settles CREATE_COMPLETE. Failing check: emr__glue__s3.cfn-deploy-glue-crawler-invalid-s3-path-negative.
AWS: Smithy (com.amazonaws.glue#CreateCrawler) models InvalidInputException as a client error, HTTP 400, alongside AlreadyExistsException / OperationTimeoutException / ResourceNumberLimitExceededException; the model does not state the validation rule or the text, so the rule itself is recorded behavior only. The recording shows AWS resolving every S3Targets[].Path at CreateCrawler time and refusing a path whose bucket exists nowhere, with a 400 whose failure reason names the S3 target and the underlying 404 from S3 (the check matches case-insensitively on the reason containing both 's3 target' and '404'): the fixed text is that S3-target-validation wording, and the variable parts are the offending s3:// path and the S3 status code. Consequently the AWS::Glue::Crawler resource ends CREATE_FAILED and the stack ends ROLLBACK_COMPLETE (or CREATE_FAILED), and the AWS::Glue::Database the same stack would have owned is rolled back too — GetDatabase then answers EntityNotFoundException with message 'Database <DatabaseName> not found.' ('Database ' + the requested name + ' not found.').
Emulator: CreateCrawler stores the target as written with no validation, the crawler resource creates successfully, and the stack settles `{'status': 'CREATE_COMPLETE', 'failures': []}`; the database the stack created therefore also still exists. Replay: `AssertionError: {'stack_name': 'sim2real-xsvc-stk-3e99d0f00411', 'status': 'CREATE_COMPLETE', 'failures': []}`.
Why: a typo'd or not-yet-created bucket in a crawler target is reported to the deployer as a successful deployment — no refusal anywhere — so the client discovers it much later or never, and templates written to be refused at deploy time are accepted instead; rollback-on-bad-input behavior cannot be exercised at all. The positive control in the same check (a same-shape stack over a real, populated prefix) deploys, crawls to SUCCEEDED and catalogues exactly one table, so the account, role and CloudFormation wiring are healthy and the divergence is isolated to this validation.
Fix: CreateCrawler's input validation in the Glue crate — resolve each S3Targets[].Path against the object store and refuse a path whose bucket does not exist with a 400 InvalidInputException naming the S3 target and the 404, which is what lets CloudFormation mark the crawler resource CREATE_FAILED and roll the stack (including its AWS::Glue::Database) back.
Prior art a fixer must read first: resolved issue 20261111T052620Z-emr-glue-s3-3-net-new-causes-the.md filed this same check as its Cause 2, and its closing annotation deliberately WITHDREW the create-time validation, on the ground that other recordings show AWS SERVING CreateCrawler over a bucket owned by a different account and the emulator's object store answers 404 for every bucket it does not hold, so it cannot tell 'nonexistent anywhere' (NoSuchBucket) from 'owned elsewhere' (AccessDenied). A resolved issue does not cover a live failure, so the check is refiled here with that conflict named: the two recordings are distinguished by the S3 answer, not by the bucket name, so the fix is to make the emulator's bucket probe distinguish a bucket that exists in no account (404 -> refuse CreateCrawler) from one that exists but is not the caller's (403 -> accept), and only then re-apply the validation. If that distinction genuinely cannot be represented, the resolution is to say so against THIS issue rather than to leave the check failing silently.
Traces: emr__glue__s3.cfn-deploy-glue-crawler-invalid-s3-path-negative; services/service-com-amazonaws-glue-awsglue/src/ops/create_crawler.rs and the CloudFormation AWS::Glue::Crawler resource provider.

## Evidence
Stack with Resources = AWS::Glue::Database(DatabaseInput.Name=<db_invalid>) + AWS::Glue::Crawler(Role=<a role trusted by glue.amazonaws.com>, DatabaseName=<db_invalid>, Targets={'S3Targets':[{'Path':'s3://<bucket-never-created-anywhere>/data/'}]}):
  Expected (AWS): stack status in {ROLLBACK_COMPLETE, CREATE_FAILED}; a stack-event failure reason on the crawler resource naming the S3 target and a 404 (reason contains 's3 target' and '404'); afterwards GetDatabase(Name=<db_invalid>) -> HTTP 400 EntityNotFoundException, message 'Database <db_invalid> not found.'
  Actual (emulator): {'stack_name': 'sim2real-xsvc-stk-<id>', 'status': 'CREATE_COMPLETE', 'failures': []} — CreateCrawler returned 200 for the nonexistent-bucket target, so no resource failed and nothing rolled back.

Replay line:
  [mismatch] emr__glue__s3.cfn-deploy-glue-crawler-invalid-s3-path-negative
  check_emr__glue__s3_cfn_deploy_glue_crawler_invalid_s3_path_negative.py line 157:
  AssertionError: {'stack_name': 'sim2real-xsvc-stk-3e99d0f00411', 'status': 'CREATE_COMPLETE', 'failures': []}
  (stack name and bucket name are per-run generated values; the requirement is the terminal status, the 404/'S3 target' reason, and the rolled-back database.)

Positive control in the same check that PASSES: the same-shape stack over s3://<a bucket the stack creates>/data/ with one CSV object deploys, StartCrawler reaches State=READY with LastCrawl.Status=SUCCEEDED, and GetTables returns exactly 1 table.

## Steps to Reproduce
1. Deploy a stack creating an S3 bucket, a role trusted by glue.amazonaws.com (AWSGlueServiceRole + s3:GetObject/s3:ListBucket on that bucket), an AWS::Glue::Database and an AWS::Glue::Crawler over s3://<that bucket>/data/; put one CSV object under data/; StartCrawler and poll GetCrawler to READY -> LastCrawl.Status=SUCCEEDED and GetTables == 1 table (passes).
2. Deploy a second stack with only an AWS::Glue::Database(<db_invalid>) and an AWS::Glue::Crawler(Role=<the role arn from step 1>, DatabaseName=<db_invalid>, Targets={'S3Targets':[{'Path':'s3://<a bucket name never created>/data/'}]}).
   Observed: the stack reaches CREATE_COMPLETE with an empty failure list; CreateCrawler accepted the target; GetDatabase(<db_invalid>) still succeeds.
   Expected: the crawler resource fails with a reason naming the S3 target and its 404, the stack ends ROLLBACK_COMPLETE/CREATE_FAILED, and GetDatabase(<db_invalid>) -> EntityNotFoundException 'Database <db_invalid> not found.'
3. Equivalently, call glue.create_crawler directly with Targets={'S3Targets':[{'Path':'s3://<a bucket name never created>/data/'}]} — observe 200 instead of 400 InvalidInputException.
