Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: logs__s3.xsvc-logs-3-scheduled-query-s3-put-denied-records-destination-error
Issue-Key: logs__s3.xsvc-logs-3-scheduled-query-s3-put-denied-records-destination-error|run#42|AssertionError|assert bdest.get("status") == "ERROR", f"denied destination status was {bdest!r}"
Issue-Commit: ffc87a9d44db1f32843bf66b14ed7266d71ed3fd

# CloudWatch Logs scheduled-query S3 delivery never authorizes its destination role: a role without s3:PutObject is reported COMPLETE with a written object instead of status ERROR

> _Note: this issue was identified against the code base at commit `ffc87a9d44db1f32843bf66b14ed7266d71ed3fd`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetScheduledQueryHistory

## Description
Cause 1 of 1: the scheduled-query execution writes its results to the configured S3 destination without ever authorizing the write against the destination's roleArn, so a destination whose role is missing s3:PutObject is recorded as a successful delivery. Failing check ids for this cause: logs__s3.xsvc-logs-3-scheduled-query-s3-put-denied-records-destination-error.

AWS: recorded behavior only for the refusal value itself — Smithy (com.amazonaws.cloudwatchlogs#ScheduledQueryDestination) models the destination entry's members status (target ActionStatus, an enum declaring only IN_PROGRESS | CLIENT_ERROR | FAILED | COMPLETE), destinationType, destinationIdentifier, processedIdentifier and errorMessage; the model does NOT declare the value AWS actually returns here, so the enum is cited only to show that errorMessage and the absence of processedIdentifier are modelled, wire-legal outcomes. What the recording shows: when a scheduled query's s3Configuration.roleArn lacks s3:PutObject on the destination bucket, the query itself still completes (executionStatus "Complete") and GetScheduledQueryHistory returns exactly ONE destination entry for it with destinationType "S3", destinationIdentifier equal to the s3:// URI that was configured, status "ERROR", an errorMessage, and NO processedIdentifier member at all; nothing is written under that prefix. The errorMessage's fixed part is the leading bracketed error class "[AccessDenied]"; its variable parts are the denied action name (s3:PutObject) and the name of the role taken from the request's destinationConfiguration.s3Configuration.roleArn. Repointing only that roleArn to a role that does hold s3:PutObject makes the next scheduled run report status "COMPLETE" and land its object under the SAME prefix — i.e. the refusal is an authorization decision per delivery, not a property of the prefix. A control query of identical shape whose role does hold s3:PutObject delivers normally, which is how the absence above is bounded.

Emulator: for the denied destination, GetScheduledQueryHistory reports the delivery as a success. The single destination entry comes back as destinationType "S3", destinationIdentifier the configured s3:// URI, status "COMPLETE", and a processedIdentifier naming an object the emulator wrote under the denied prefix (shape: s3://<bucket>/<configured-prefix>/<scheduled-query-name>/<uuid>.json). There is no errorMessage, the status is never ERROR, and the object exists — so the destination role's identity policy is never consulted before the PutObject. (Bucket, prefix, query name and the uuid in that key are per-run values, not requirements; the requirement is status/errorMessage/processedIdentifier.)

Why: a client relying on AWS's behavior reads the destination entry to decide whether its results actually landed and, when they did not, which grant is missing. Here it is told COMPLETE and handed a processedIdentifier for an object that AWS would never have written, so (a) a mis-scoped delivery role is invisible — monitoring and negative-path tests keyed on status != COMPLETE never fire; (b) the "[AccessDenied] ... s3:PutObject ... <role>" diagnostic that names the exact missing permission never appears, so an operator has nothing to act on; and (c) query results are written into a bucket/prefix the role is not permitted to write, which inverts the authorization boundary the destination role exists to enforce (a least-privilege delivery role becomes decorative).

Fix: the scheduled-query execution's destination-delivery step in the logs service — the component that, after the query settles, assumes destinationConfiguration.s3Configuration.roleArn and performs the PutObject. It must evaluate that role's policy for s3:PutObject on the destination object before writing; on denial it must write nothing, record status ERROR on that destination entry, populate errorMessage with the "[AccessDenied]" class plus the denied action and the role name, and omit processedIdentifier entirely. The success path (status COMPLETE plus processedIdentifier) must stay reachable for a role that does hold the grant, including after an UpdateScheduledQuery that only repoints roleArn.

Traces: logs__s3.xsvc-logs-3-scheduled-query-s3-put-denied-records-destination-error (verdict mismatch; assertion "denied destination status was {'destinationType': 'S3', 'destinationIdentifier': 's3://<bucket>/bad/', 'status': 'COMPLETE', 'processedIdentifier': 's3://<bucket>/bad/<qname>/<uuid>.json'}"). No open issue covers this: the two prior logs scheduled-query issues (20261025T032458Z-logs-s3-cloudwatch-logs-startquery-is-an-unimplemented.md and 20261029T222444Z-logs-scheduled-query-operations-accept-invalid-input-and.md) are both resolved and concern StartQuery being a stub and scheduled-query input validation / not-found wording, not destination-role authorization.

## Evidence
Expected (AWS), for a scheduled query whose s3Configuration.roleArn is allowed only s3:GetObject on the destination bucket:
  executionStatus = "Complete"
  destinations = [ { destinationType: "S3",
                     destinationIdentifier: "s3://<bucket>/<prefix>/",
                     status: "ERROR",
                     errorMessage: "[AccessDenied] ... s3:PutObject ... <role-name> ..." } ]
  (no processedIdentifier member)
and ListObjectsV2 under <prefix>/ returns no keys. Repointing roleArn to a role holding s3:PutObject then yields status "COMPLETE" with a processedIdentifier whose key is under the same <prefix>/.

Actual (emulator):
  executionStatus = "Complete"
  destinations = [ { 'destinationType': 'S3',
                     'destinationIdentifier': 's3://sim2real-xsvc-sqdeny-53b204237542/bad/',
                     'status': 'COMPLETE',
                     'processedIdentifier': 's3://sim2real-xsvc-sqdeny-53b204237542/bad/sim2real-xsvc-sqdeny-qno-796bc03a3b78/00000000-0000-4000-8000-000000000003.json' } ]
  (no errorMessage; the named object was written under the denied prefix)

Raised as: AssertionError: denied destination status was {'destinationType': 'S3', 'destinationIdentifier': 's3://sim2real-xsvc-sqdeny-53b204237542/bad/', 'status': 'COMPLETE', 'processedIdentifier': 's3://.../00000000-0000-4000-8000-000000000003.json'} — at the assertion bdest.get("status") == "ERROR".

The bucket name, the role names, the scheduled-query name and the uuid in the object key above are per-run values and are not part of the requirement. The requirements are: status ERROR, an errorMessage leading with "[AccessDenied]" that names s3:PutObject and the denied role, the absence of processedIdentifier, and the absence of any object under the denied prefix.

The positive control in the same run (identical query shape, same bucket, role holding s3:PutObject) delivered successfully, so the emulator's delivery path works — it simply skips the authorization check.

## Steps to Reproduce
1. Boot the emulator from this checkout.
2. Create a bucket B and a log group G; put two log events into a stream of G and wait until a Logs Insights query over G returns them.
3. Create two IAM roles trusted by logs.amazonaws.com, both allowed logs:StartQuery/StopQuery/GetQueryResults/DescribeLogGroups on "*":
   - role OK: also allowed s3:PutObject on arn:aws:s3:::B/*
   - role NO: allowed only s3:GetObject on arn:aws:s3:::B/* (the single broken thing)
4. CreateScheduledQuery twice with queryLanguage=CWLI, scheduleExpression="cron(* * * * ? *)", logGroupIdentifiers=[G], startTimeOffset=3600, endTimeOffset=0, state=ENABLED:
   - query QNO: executionRoleArn=role NO, destinationConfiguration={"s3Configuration":{"destinationIdentifier":"s3://B/bad/","roleArn":role NO}}
   - query QOK: executionRoleArn=role OK, destinationConfiguration={"s3Configuration":{"destinationIdentifier":"s3://B/good/","roleArn":role OK}}
5. Poll GetScheduledQueryHistory(identifier=QOK) until a record's destination leaves IN_PROGRESS; confirm status COMPLETE and that the object exists under good/ (positive control, bounds the absence below).
6. Poll GetScheduledQueryHistory(identifier=QNO) until its destination settles. Observe status "COMPLETE" with a processedIdentifier under bad/ instead of status "ERROR" with an "[AccessDenied]" errorMessage naming s3:PutObject and role NO and no processedIdentifier; ListObjectsV2(Bucket=B, Prefix="bad/") returns the written object instead of nothing.
7. UpdateScheduledQuery(identifier=QNO) repointing only s3Configuration.roleArn to role OK and confirm the next run reports COMPLETE with an object under the same bad/ prefix (the repair path must remain correct once the denial is implemented).
8. Re-replay check: logs__s3.xsvc-logs-3-scheduled-query-s3-put-denied-records-destination-error
