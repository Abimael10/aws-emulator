Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: logs__s3__sts.create-export-task-bucket-missing-permission-rejected
Issue-Key: logs__s3__sts.create-export-task-bucket-missing-permission-rejected|run#11|AssertionError|assert code == "AccessDeniedException", (
Issue-Commit: 76a27d9bdd16e996b43ee8be0e1161c3fa717030

# logs__s3__sts: CreateExportTask against a bucket that grants CloudWatch Logs nothing is refused InvalidParameterException instead of AWS's AccessDeniedException with the GetBucketAcl/PutObject sentence

> _Note: this issue was identified against the code base at commit `76a27d9bdd16e996b43ee8be0e1161c3fa717030`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateExportTask

## Description
REOPENS 20261109T143515Z-logs-s3-sts-createexporttask-is-accepted-although-the-destination-bucket-does.md: its fix left the call refused. The destination-permission validation that issue asked for now exists — the call is no longer accepted — but it is refused in the wrong error class, so the very check that issue was filed from still fails. (The later fixed issue 20261120T145635Z-logs-3-net-new-causes-createexporttask-refuses-an.md asserts the opposite classification for this condition; the recording is authoritative here.)

Cause 1 of 1: the unauthorized-destination refusal carries error code InvalidParameterException instead of AccessDeniedException.
AWS: Smithy logs#CreateExportTask takes logGroupName/from/to/destination (+ optional taskName, logStreamNamePrefix, destinationPrefix) and models InvalidParameterException, LimitExceededException, OperationAbortedException, ResourceAlreadyExistsException, ResourceNotFoundException (client, httpError 400) and ServiceUnavailableException (server, 500). Recorded behavior for this condition: a destination bucket with NO policy granting the CloudWatch Logs service principal is refused with HTTP 400 and error code `AccessDeniedException` (a code outside that modelled set — AWS returns it anyway) whose message is the wholly fixed sentence "GetBucketAcl call on the given bucket failed. Please check if CloudWatch Logs has been granted permission to perform this operation." — it interpolates nothing from the request, no bucket, group or account. Once the bucket policy grants only s3:GetBucketAcl, the same call is refused the same way with the fixed sentence "PutObject call on the given bucket failed. Please check if CloudWatch Logs has been granted permission to perform this operation.", naming the next missing action. In both cases no export task is created, so DescribeExportTasks lists none for that log group; with both s3:GetBucketAcl and s3:PutObject granted the identical call succeeds and the task reaches status code COMPLETED.
Emulator: the first call is refused with error code `InvalidParameterException`, so a client branching on the refusal class never recognises the missing-bucket-permission case, and the check stops before the GetBucketAcl-only leg, the no-task-created leg and the fully-permissioned positive control can be graded.
Why: export-to-S3 setup is diagnosed by this refusal — an SDK or IaC caller distinguishes "the bucket policy is missing a grant" (AccessDeniedException, naming GetBucketAcl then PutObject in turn) from a malformed parameter; classified as InvalidParameterException it reads as a bad request, the two-stage grant guidance is lost, and error handling written against AWS's code never matches.
Fix: the error classification (and message text) of the destination-bucket authorization step in the CloudWatch Logs CreateExportTask handler: emit AccessDeniedException with the GetBucketAcl sentence when the bucket policy grants the logs service principal nothing, and the PutObject sentence when only s3:GetBucketAcl is granted, creating no export task either way.
Traces: logs__s3__sts.create-export-task-bucket-missing-permission-rejected.

## Evidence
Expected (AWS): create_export_task(logGroupName=<group>, destination=<bucket with no bucket policy>, from=<now-60s ms>, to=<now ms>) is refused HTTP 400 with Error.Code 'AccessDeniedException' and Error.Message 'GetBucketAcl call on the given bucket failed. Please check if CloudWatch Logs has been granted permission to perform this operation.' (entire sentence fixed text), and DescribeExportTasks lists no task for that log group.

Actual (emulator): AssertionError: CreateExportTask against an unpermissioned bucket returned 'InvalidParameterException', expected AccessDeniedException. The refusal is correct in kind (400, no task created) but wrong in class, so the message, the GetBucketAcl-only leg ('PutObject call on the given bucket failed. ...') and the fully-permissioned positive control (export reaching status COMPLETED) are never graded. Bucket and log-group names are per-run values.

## Steps to Reproduce
1. Create an S3 bucket in the region and a log group; install NO bucket policy.
2. Call logs.CreateExportTask(logGroupName=<group>, destination=<bucket>, from=<now-60000 ms>, to=<now ms>). Expected 400 AccessDeniedException with 'GetBucketAcl call on the given bucket failed. Please check if CloudWatch Logs has been granted permission to perform this operation.'; observed error code InvalidParameterException.
3. DescribeExportTasks() — no task for that log group (as AWS).
4. PutBucketPolicy granting Principal {"Service":"logs.<region>.amazonaws.com"} only s3:GetBucketAcl on the bucket and repeat step 2: expected 400 AccessDeniedException with 'PutObject call on the given bucket failed. Please check if CloudWatch Logs has been granted permission to perform this operation.'
5. PutBucketPolicy granting both s3:GetBucketAcl on the bucket and s3:PutObject on bucket/* and repeat: expected a taskId whose DescribeExportTasks status code reaches COMPLETED.
