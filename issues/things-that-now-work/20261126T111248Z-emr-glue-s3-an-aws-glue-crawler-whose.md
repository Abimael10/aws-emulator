Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: emr__glue__s3.cfn-deploy-glue-crawler-invalid-s3-path-negative
Issue-Key: emr__glue__s3.cfn-deploy-glue-crawler-invalid-s3-path-negative|run#23|AssertionError|assert invalid_deploy["status"] in {"ROLLBACK_COMPLETE", "CREATE_FAILED"}, invalid_deploy
Issue-Commit: 797b66678ccb4eaa2b7b0a5c198b3bd26a6c96cd

# emr__glue__s3: an AWS::Glue::Crawler whose S3 target is in a nonexistent bucket reaches CREATE_COMPLETE instead of failing creation on the S3-target 404 and rolling the stack back

> _Note: this issue was identified against the code base at commit `797b66678ccb4eaa2b7b0a5c198b3bd26a6c96cd`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
Cause 1 of 1: CloudFormation provisions an AWS::Glue::Crawler whose S3Targets[].Path points into a bucket that does not exist, and the stack settles CREATE_COMPLETE. The crawler's creation-time S3-target existence check is missing, so the resource never fails and the stack is never rolled back.

AWS: Glue validates a crawler's S3 targets when the crawler is created. Smithy (services/service-com-amazonaws-glue-awsglue/smithy-model/com-amazonaws-glue-awsglue.json): CreateCrawler takes Targets (CrawlerTargets, required) and models InvalidInputException (client, HTTP 400) as the refusal for an unusable target. When the target path's bucket does not exist, S3 answers the target probe 404 and Glue refuses the create; CloudFormation turns that refusal into a resource CREATE_FAILED whose ResourceStatusReason carries the wrapped S3 failure (its fixed text names the S3 target and the HTTP status 404; its variable parts are the target path/bucket taken from the template and the Glue/S3 error code and resource-handler wrapper), and the stack then goes to ROLLBACK_COMPLETE (or CREATE_FAILED when rollback is disabled). Recorded behavior only for the exact wrapper wording: the recording asserts only that a failure reason exists and that it contains both the HTTP status 404 and the phrase "S3 target" (case-insensitive) — do not hard-code the recording's bucket name, stack name or request id, which are per-run values. Because creation fails, the AWS::Glue::Database the same stack declares is rolled back too, and Glue GetDatabase on it answers EntityNotFoundException with message "Database <name> not found." (fixed text "Database " ... " not found.", variable part the requested database name).

Emulator: the same stack settles CREATE_COMPLETE with an empty failure-event list — DescribeStacks reports StackStatus=CREATE_COMPLETE and DescribeStackEvents contains no CREATE_FAILED/ROLLBACK_FAILED event at all ({'status': 'CREATE_COMPLETE', 'failures': []}). The crawler resource is created with a target in a bucket that was never created anywhere, and the stack's Glue database survives, so the effect-absent read at the destination cannot hold either. The positive leg of the same check passes (a crawler on a real, populated path deploys, crawls to SUCCEEDED and catalogues one table), so this is specifically the missing negative-path validation, not a broken crawler path.

Why: a client that relies on CloudFormation refusing a misconfigured crawler gets the opposite signal. A template with a typo'd or not-yet-created bucket deploys "successfully"; the deploy pipeline reports success and moves on, and the breakage only surfaces much later as a crawler that never catalogues anything. Negative-path tests and guardrails that assert ROLLBACK_COMPLETE plus a 404 S3-target reason pass against real AWS and fail against the emulator, and resources (the Glue database) that AWS would have rolled back are left behind as orphans.

Fix: the AWS::Glue::Crawler resource provisioner in the CloudFormation resource-handler layer must probe each S3Targets[].Path (bucket existence/HEAD) at create time and fail the resource when the probe 404s, surfacing the wrapped S3 404 in the resource's ResourceStatusReason; and Glue's CreateCrawler validation must refuse an S3 target whose bucket does not exist with InvalidInputException (HTTP 400) rather than accepting it. The stack machinery must then record the resource CREATE_FAILED event and roll the stack back so the sibling AWS::Glue::Database is removed.

Traces: emr__glue__s3.cfn-deploy-glue-crawler-invalid-s3-path-negative (assertion at check line 157: invalid_deploy["status"] in {"ROLLBACK_COMPLETE", "CREATE_FAILED"}).

Related but not covering this: open issue 20261125T184306Z-glue-createcrawler-refuses-a-valid-s3-target-create.md is about CreateCrawler refusing a VALID S3-target create — the opposite direction, and the valid leg of this check succeeds here.

## Evidence
Expected (AWS): the invalid stack ends in ROLLBACK_COMPLETE (or CREATE_FAILED), with at least one CREATE_FAILED stack event on the AWS::Glue::Crawler resource whose ResourceStatusReason names the S3 target and HTTP status 404 (the check requires both "404" and "s3 target" in the joined reasons), and Glue GetDatabase on the stack's database answers EntityNotFoundException "Database <db_invalid> not found."

Actual (emulator): AssertionError: {'stack_name': 'sim2real-xsvc-stk-<id>', 'status': 'CREATE_COMPLETE', 'failures': []} — the stack reached CREATE_COMPLETE and DescribeStackEvents reported no CREATE_FAILED/ROLLBACK_FAILED events, so the 404 / "S3 target" reason assertion and the rolled-back-database read are never reached.

Control in the same check: the same-shape stack targeting a real, populated s3://<ValidBucket>/data/ path deploys CREATE_COMPLETE, its crawler runs to LastCrawl.Status=SUCCEEDED and catalogues exactly one table — so the account, role and CFN wiring are healthy and only the invalid-target rejection is missing.

## Steps to Reproduce
1. CreateStack with a template containing an AWS::S3::Bucket, an AWS::IAM::Role trusted by glue.amazonaws.com, an AWS::Glue::Database and an AWS::Glue::Crawler whose S3Targets Path is s3://<that bucket>/data/; wait for CREATE_COMPLETE; PutObject a CSV at data/part-00000.csv; StartCrawler and poll GetCrawler until State=READY — LastCrawl.Status is SUCCEEDED and GetTables returns one table (positive control).
2. CreateStack a second, same-shape stack with only an AWS::Glue::Database and an AWS::Glue::Crawler, the crawler's S3Targets Path set to s3://<a bucket name never created>/data/ and its Role passed in as a parameter (the role ARN from step 1), Capabilities CAPABILITY_IAM/CAPABILITY_NAMED_IAM.
3. Poll DescribeStacks until the StackStatus is terminal, then call DescribeStackEvents and collect events with ResourceStatus in {CREATE_FAILED, ROLLBACK_FAILED}.
4. Observed: StackStatus=CREATE_COMPLETE and no such events. Expected: ROLLBACK_COMPLETE or CREATE_FAILED with a reason containing HTTP 404 and "S3 target".
5. Call Glue GetDatabase on the second stack's database name. Observed: it resolves. Expected: EntityNotFoundException "Database <name> not found." because the stack rolled back.
