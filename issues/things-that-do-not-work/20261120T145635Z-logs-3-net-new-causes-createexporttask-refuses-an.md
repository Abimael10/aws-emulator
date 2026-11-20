Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: logs.create-export-task-lifecycle logs.export-task-cancel-ordering logs.limit-exceeded-triggers logs.put-bearer-token-authentication-enforced-server-side
Issue-Shape-Id: com.amazonaws.cloudwatchlogs#Logs_20140328
Issue-Key: cloudwatch-logs|CreateExportTask|AccessDeniedException
Issue-Key: logs.limit-exceeded-triggers|_error_code#31|AssertionError|raise AssertionError(f"the call was expected to be refused but it succeeded, returning {result!r}")
Issue-Key: logs.put-bearer-token-authentication-enforced-server-side|run#22|AssertionError|assert _rpc_reply(pre_body) == (7, _AUTH_FAILED), (
Issue-Commit: f422e37ca967a244b72f2f746d1d1ab79e5db12d

# logs: 3 net-new causes — CreateExportTask refuses an authorized destination bucket with an unmodelled AccessDeniedException, AssociateSourceToS3TableIntegration accepts a nonexistent integration ARN, and the OTLP endpoint's 403 carries a JSON body instead of google.rpc.Status

> _Note: this issue was identified against the code base at commit `f422e37ca967a244b72f2f746d1d1ab79e5db12d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateExportTask

## Description
Three independent root causes in CloudWatch Logs, all grounded in failing checks of this suite.

Cause 1 of 3: CreateExportTask refuses a destination bucket whose bucket policy DOES grant CloudWatch Logs the required permissions.
AWS: Smithy CreateExportTask takes logGroupName/from/to/destination (+optional taskName, logStreamNamePrefix, destinationPrefix) and returns taskId; its modelled errors are InvalidParameterException, LimitExceededException, OperationAbortedException, ResourceAlreadyExistsException, ResourceNotFoundException, ServiceUnavailableException (all client errors httpError 400 except ServiceUnavailableException 500) — AccessDeniedException is NOT a modelled error of this operation. When the destination bucket carries a policy allowing Principal {"Service":"logs.<region>.amazonaws.com"} to s3:GetBucketAcl on arn:aws:s3:::<bucket> and s3:PutObject on arn:aws:s3:::<bucket>/*, conditioned on aws:SourceAccount = the caller's account and aws:SourceArn = the log group ARN, the call SUCCEEDS and returns a taskId; the message 'GetBucketAcl call on the given bucket failed. Please check if CloudWatch Logs has been granted permission to perform this operation.' is AWS's fixed text for the genuinely-unauthorized case only (and AWS raises it as InvalidParameterException, not AccessDeniedException).
Emulator: every CreateExportTask against such a bucket is refused with error code AccessDeniedException and exactly that message, so no export task is ever created. The bucket policy the check installed is evidently not consulted (or its service-principal/condition form is not understood), and the refusal is classified as an error the operation does not model.
Why: export-to-S3 is unusable — a client that follows AWS's documented bucket-policy setup still cannot start an export, and the error class it gets (AccessDeniedException) is not in the operation's modelled error set, so SDK error handling written against the model never matches it.
Fix: the CreateExportTask handler's destination-bucket authorization step (its evaluation of the S3 bucket policy for the logs service principal) and its error classification; the whole downstream export-task lifecycle (DescribeExportTasks/CancelExportTask, terminal status CANCELLED 'Export task cancelled by user' vs COMPLETED 'Completed successfully') is unreachable behind it.
Failing checks: logs.create-export-task-lifecycle, logs.export-task-cancel-ordering.

Cause 2 of 3: AssociateSourceToS3TableIntegration accepts an integration ARN that does not exist.
AWS: Smithy AssociateSourceToS3TableIntegration requires integrationArn (Arn) and dataSource, returns an optional identifier, and models ResourceNotFoundException (errorKind client, httpError 400) among AccessDeniedException/ThrottlingException/ValidationException/InternalServerException. Addressing an integration that was never created is the ResourceNotFoundException case — the message's fixed text is the service's not-found wording with the requested integration identifier from the request as its variable part; it must not be answered 200.
Emulator: the call returns HTTP 200 with a minted body {'identifier': '00000000-0000-4000-8000-000000000001'} for an integrationArn of the form arn:aws:logs:<region>:<account>:integration:<name> that was never created, so a caller is told it associated a log group with a nonexistent S3-table integration.
Why: a client's error path never runs: a typo'd or deleted integration silently 'succeeds', the returned identifier refers to nothing, and the log group is believed to be wired to a table integration it is not.
Fix: existence validation in the AssociateSourceToS3TableIntegration handler (look the integration up in the service's integration state and refuse with ResourceNotFoundException before minting a source identifier). The open issue 20261029T182816Z-logs-startquery-and-eight-other-logs-operations-accept.md covers the same accept-anything shape for a different set of operations and does not carry this check.
Failing check: logs.limit-exceeded-triggers.

Cause 3 of 3: the OTLP /v1/logs endpoint's rejection body is JSON, not the protobuf google.rpc.Status AWS returns.
AWS: recorded behavior only (the OTLP data-plane endpoint is not in the Smithy model). A bearer-token POST to <logs endpoint>/v1/logs for a log group that has not had PutBearerTokenAuthentication enabled is answered 403 whose body is a protobuf-encoded google.rpc.Status: field 1 (varint) code = 7 (PERMISSION_DENIED) and field 2 (length-delimited) message = the fixed text 'Authentication failed: Please make sure your API Key is valid.'. The same body is returned for an invalid token; once auth is explicitly disabled again the endpoint answers 400 with code 3 and the message 'This operation is not supported for log group: <log group name>', whose only variable part is the log group name from the request.
Emulator: the status is 403 but the body is the JSON document {"message":"Forbidden"} — no google.rpc.Status, no code 7, and none of AWS's message text; the check cannot even reach the enable/valid-token/invalid-token/disable legs.
Why: an OTLP client (or the OpenTelemetry exporter) decodes the response body as google.rpc.Status to learn whether to retry or re-authenticate; a JSON 'Forbidden' body decodes to nothing, so every rejection looks like an unknown transport failure and the bearer-token lifecycle is unobservable.
Fix: the OTLP ingest endpoint's rejection serializer and its bearer-token authorization path (emit the protobuf google.rpc.Status with the code and the fixed message text for each refusal class), alongside the PutBearerTokenAuthentication state it must consult.
Failing check: logs.put-bearer-token-authentication-enforced-server-side.

## Evidence
Cause 1 — Expected: create_export_task(logGroupName=<group>, from=0, to=<now_ms>, destination=<bucket>) returns a taskId after the bucket policy granting logs.<region>.amazonaws.com s3:GetBucketAcl + s3:PutObject (with aws:SourceAccount/aws:SourceArn conditions) is installed.
Actual: botocore.errorfactory.AccessDeniedException: An error occurred (AccessDeniedException) when calling the CreateExportTask operation: GetBucketAcl call on the given bucket failed. Please check if CloudWatch Logs has been granted permission to perform this operation. (identical in both failing checks). The message's text is fixed; bucket, group and account are per-run values. AccessDeniedException is not among CreateExportTask's modelled errors.

Cause 2 — Expected: associate_source_to_s3_table_integration(integrationArn=arn:aws:logs:<region>:<account>:integration:<never-created>, dataSource={'name': <log group>, 'type': 'LOG_GROUP'}) is refused with ResourceNotFoundException (400).
Actual: AssertionError: the call was expected to be refused but it succeeded, returning {'identifier': '00000000-0000-4000-8000-000000000001', 'ResponseMetadata': {'HTTPStatusCode': 200, ...}}. The identifier value is a per-run/placeholder value; the divergence is the 200 instead of a refusal.

Cause 3 — Expected: POST <logs endpoint>/v1/logs with a valid service-specific bearer token, before PutBearerTokenAuthentication enables auth for the group, answers 403 with a protobuf google.rpc.Status decoding to (7, 'Authentication failed: Please make sure your API Key is valid.').
Actual: AssertionError: unexpected rejection body before auth is enabled: '{"message":"Forbidden"}' — status 403 but a JSON body carrying neither the code nor AWS's message text.

## Steps to Reproduce
Cause 1: create a log group + stream, PutLogEvents one event; create an S3 bucket in the same region and PutBucketPolicy granting Principal {"Service":"logs.<region>.amazonaws.com"} s3:GetBucketAcl on the bucket and s3:PutObject on bucket/*, conditioned on aws:SourceAccount=<account> and aws:SourceArn=<log group arn>; call CreateExportTask(logGroupName, from=0, to=now_ms, destination=bucket). Expected a taskId; observed AccessDeniedException with the GetBucketAcl message. Then DescribeExportTasks/CancelExportTask on the returned taskId (unreachable today).
Cause 2: call AssociateSourceToS3TableIntegration with integrationArn=arn:aws:logs:<region>:<account>:integration:<name-never-created> and dataSource={'name': <an existing log group>, 'type': 'LOG_GROUP'}. Expected ResourceNotFoundException; observed 200 with an identifier.
Cause 3: create a log group and stream; create an IAM user with logs:CallWithBearerToken + logs:PutLogEvents and CreateServiceSpecificCredential for ServiceName=logs.amazonaws.com; POST an OTLP logs payload to <logs endpoint>/v1/logs with Authorization: Bearer <token> and the x-aws-log-group/x-aws-log-stream routing headers BEFORE calling PutBearerTokenAuthentication. Expected 403 with a protobuf google.rpc.Status (code 7, 'Authentication failed: Please make sure your API Key is valid.'); observed 403 with body {"message":"Forbidden"}. Then enable auth and repeat (valid token -> 200, wrong token -> 403 same status body), then disable and repeat (-> 400, code 3, 'This operation is not supported for log group: <group>').
