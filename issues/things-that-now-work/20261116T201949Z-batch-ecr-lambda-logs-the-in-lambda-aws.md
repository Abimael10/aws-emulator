Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: batch__ecr__lambda__logs.e2e-diagnose-batch-job-no-logs-lifecycle
Issue-Key: batch__ecr__lambda__logs.e2e-diagnose-batch-job-no-logs-lifecycle|phase#6|AssertionError|raise AssertionError(f'[phase {index}/{len(self.phases)} {name}] {exc}') from exc
Issue-Commit: a8ac3c69a763146fdc582441249cf5b2157d2212

# batch__ecr__lambda__logs: the in-Lambda AWS SDK has no 'batch' client, so a handler calling SubmitJob dies with UnknownServiceError and Invoke answers 200 with FunctionError='Unhandled'

> _Note: this issue was identified against the code base at commit `a8ac3c69a763146fdc582441249cf5b2157d2212`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
Invoke

## Description
One net-new root cause, grounded in the single surviving failing check of this service (every other failure listed under `### batch__ecr__lambda__logs` in this run is OWNED by an already-filed issue and is not repeated here).

Cause 1 of 1: the AWS SDK that the emulator's Lambda runtime makes available to a managed Python function has no `batch` client at all. A handler that does `boto3.client('batch').submit_job(...)` raises botocore's UnknownServiceError at client construction, so the invocation is reported as an unhandled handler failure (HTTP 200 with the X-Amz-Function-Error header set to 'Unhandled') instead of returning the handler's own document. The error payload the emulator produces enumerates the SDK's whole service catalogue, and `batch` is absent from it, so this is a gap in the in-Lambda SDK's service catalogue, not a defect of the handler, the role, or Batch itself.

AWS: Smithy (com.amazonaws.lambda) models Invoke as POST /2015-03-31/functions/{FunctionName}/invocations, code 200, output InvocationResponse{StatusCode (@httpResponseCode), FunctionError (@httpHeader X-Amz-Function-Error), LogResult, Payload (@httpPayload), ExecutedVersion}. FunctionError is not a modeled error shape: it is set in-band on an otherwise successful 200 only when the function's own code raises. AWS's managed Python runtimes ship the full AWS SDK for Python (boto3/botocore) on the runtime import path, and that SDK carries a client for every GA AWS service, `batch` among them (botocore data directory `batch/2016-08-10/service-2.json`; `boto3.client('batch')` is the documented way to call Batch from Lambda). On AWS this handler therefore constructs a Batch client, calls SubmitJob, and the invocation is answered 200 with NO X-Amz-Function-Error header and Payload equal to the handler's own return document — for this handler a JSON object carrying a `jobId` key whose value is the id Batch minted in that run (the keys are the contract; the id value is per-run state and is not part of the requirement).

Emulator: Invoke answers HTTP 200 but with X-Amz-Function-Error='Unhandled' and an error document whose errorMessage is botocore's UnknownServiceError text, verbatim from the replay: "Unknown service: 'batch'. Valid service names are: sqs, sns, sts, events, eventbridge, pipes, dynamodb, logs, cloudwatchlogs, cloudwatch, apigateway, apigatewaymanagementapi, secretsmanager, kms, ssm, opensearch, es, stepfunctions, sfn, sesv2, s3, xray, rds, rds-data, appconfigdata, appconfig, config, configservice, codedeploy, codepipeline, cloudfront, rekognition, lambda, glue, …". In that message the fixed text is botocore's own "Unknown service: '<name>'. Valid service names are: <comma-separated catalogue>"; the `<name>` comes from the request (the service string the handler passed to boto3.client) and the catalogue comes from the emulator's bundled SDK state — the specific list in any one run is not part of any requirement, its MISSING member `batch` is. The check fails in phase 2/6 (pre_invoke) at the point where the submit Lambda is invoked once to obtain a jobId, so nothing downstream of the invocation (the job's SUBMITTED→RUNNABLE→STARTING→FAILED trajectory, the image-pull classification of its statusReason, the attempt's taskArn and logStreamName, the solve/post_invoke/cleanup phases) is ever exercised.

Why: invoking a thin Lambda wrapper that calls another AWS service with the runtime-provided SDK is the most common Lambda pattern, and the Batch reference architecture replayed here depends on it. A client gets a 200 whose FunctionError header says the handler blew up and whose payload is a botocore error document instead of the handler's JSON, so it cannot read back a jobId, cannot poll DescribeJobs, and cannot tell 'submission accepted' from 'handler crashed'. Any Lambda-driven Batch control flow — submit-and-report, submit-and-poll, scheduled submitters — is unreachable, and the diagnostic property the scenario exists to prove (a 200 with a jobId is compatible with a job that later FAILS for an image-pull reason) can never be demonstrated because no successful 200 is ever produced.

Related but NOT covering this: open issue 20261105T082918Z-batch-ecr-lambda-logs-lambda-invoke-of-a.md reports the same outward symptom (Invoke 200 with FunctionError='Unhandled' from a Batch-submitting python3.12 handler) for a different check, but attributes it hypothetically to boto3 being absent from the runtime import path. This check's captured payload shows boto3 IS importable and it is specifically the `batch` client that the bundled SDK does not have, which is a different component and a different fix. Open issues 20261113T195355Z-cloudwatch-events-lambda-logs-s3-ses-the-in-lambda-aws-sdk-has-no-ses.md, 20261106T134622Z-lambda-sns-sqs-the-in-lambda-sdk-has.md, 20261106T134622Z-lambda-secretsmanager-sqs-the-in-lambda-sdk-reports.md and 20261107T042814Z-cloudfront-events-the-in-lambda-aws-sdk-has.md report the same class of gap for 'ses', 'sns', 'secretsmanager' and 'cloudfront'; none names 'batch'.

Fix: the lambda crate's Invoke execution path — the component that builds the Python runtime sandbox and provisions the runtime-supplied AWS SDK for the handler. Its bundled SDK service catalogue must include `batch` (and the handler-facing client must be pointed at the emulator's own batch endpoint with the function's execution-role credentials), so that a handler which constructs a Batch client and returns normally is answered 200 with no X-Amz-Function-Error header and the handler's return value as Payload.

Traces: batch__ecr__lambda__logs.e2e-diagnose-batch-job-no-logs-lifecycle (phase 2/6 pre_invoke).

## Evidence
Suite run: 791/1049 passed (75.4%). Service batch__ecr__lambda__logs scored 11/15 (73%), 4 failed; three of those four are OWNED by already-filed issues, this is the fourth.

[mismatch] batch__ecr__lambda__logs.e2e-diagnose-batch-job-no-logs-lifecycle
  File "check_batch__ecr__lambda__logs_e2e_diagnose_batch_job_no_logs_lifecycle.py", line 595, in phase
    raise AssertionError(f'[phase {index}/{len(self.phases)} {name}] {exc}') from exc
  AssertionError: [phase 2/6 pre_invoke] <submit function name> returned FunctionError='Unhandled': {"errorMessage":"Unknown service: 'batch'. Valid service names are: sqs, sns, sts, events, eventbridge, pipes, dynamodb, logs, cloudwatchlogs, cloudwatch, apigateway, apigatewaymanagementapi, secretsmanager, kms, ssm, opensearch, es, stepfunctions, sfn, sesv2, s3, xray, rds, rds-data, appconfigdata, appconfig, config, configservice, codedeploy, codepipeline, cloudfront, rekognition, lambda, glue, …

Expected (AWS): lambda:Invoke with InvocationType=RequestResponse on the managed-Python submit function -> HTTP 200, NO X-Amz-Function-Error header, Payload = the handler's own document carrying the jobId returned by batch:SubmitJob. The runtime-provided boto3 resolves `boto3.client('batch')` because the managed Python runtime ships the full AWS SDK for Python, whose service catalogue includes batch (API version 2016-08-10).

Actual (emulator): HTTP 200 with X-Amz-Function-Error='Unhandled'; the payload is a botocore UnknownServiceError document — fixed text "Unknown service: '<name>'. Valid service names are: <catalogue>" with <name>='batch' taken from the handler's boto3.client(...) call and <catalogue> listing the emulator's bundled SDK services, which does not contain `batch`. No jobId is produced, so phases 3-6 of the check (solve / post_invoke / cleanup / destroy) are never reached. The function name, role ARN, queue/definition ARNs and the generated stack names in any one run are per-run values and are not part of the requirement; the absence of `batch` from the catalogue is.

## Steps to Reproduce
1. Deploy (or build by hand) the Batch estate: batch:CreateComputeEnvironment (FARGATE), batch:CreateJobQueue enabled and VALID, ecr:CreateRepository left EMPTY, and batch:RegisterJobDefinition type=container platformCapabilities=[FARGATE] with containerProperties.image set to the repository URI with NO tag and no logConfiguration.
2. iam:CreateRole trusting lambda.amazonaws.com; attach basic execution logging plus an inline policy granting batch:SubmitJob on the queue and job-definition ARNs.
3. lambda:CreateFunction with a managed Python runtime (e.g. python3.12), Handler='index.handler', Code.ZipFile containing only index.py — the archive bundles NO boto3, exactly as on AWS — whose handler does:
     import boto3, json, os
     def handler(event, context):
         r = boto3.client('batch').submit_job(jobName=..., jobQueue=os.environ['JOB_QUEUE'], jobDefinition=os.environ['JOB_DEFINITION'])
         return {"statusCode": 200, "body": json.dumps({"jobId": r["jobId"]})}
   Set Environment.Variables JOB_QUEUE and JOB_DEFINITION.
4. Poll GetFunctionConfiguration until State=Active (it does reach Active).
5. lambda:Invoke the function with InvocationType='RequestResponse'.
   -> AWS: 200, no X-Amz-Function-Error, Payload = the handler's document with a jobId.
   -> Emulator: 200 with X-Amz-Function-Error='Unhandled' and Payload = {"errorMessage":"Unknown service: 'batch'. Valid service names are: …"} with `batch` absent from the enumerated catalogue.
6. (Unreachable on the emulator) batch:DescribeJobs on the returned jobId and follow it to FAILED with an image-pull statusReason.
