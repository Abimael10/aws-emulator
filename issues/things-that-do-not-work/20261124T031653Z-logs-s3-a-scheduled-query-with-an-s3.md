Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: logs__s3.xsvc-logs-3-scheduled-query-s3-object-rows-equal-seeded-events logs__s3.xsvc-logs-3-scheduled-query-s3-put-denied-records-destination-error
Issue-Key: logs__s3.xsvc-logs-3-scheduled-query-s3-object-rows-equal-seeded-events|_poll_until#27|AssertionError|raise AssertionError(f"{description} not met within {timeout_s}s. Last result: {last!r}")
Issue-Key: logs__s3.xsvc-logs-3-scheduled-query-s3-put-denied-records-destination-error|_poll_until#28|AssertionError|raise AssertionError(f"{description} not met within {timeout_s}s. Last result: {last!r}")
Issue-Commit: 02e30cec75724d494cfd983590fb8cff440247b8

# logs__s3: a scheduled query with an S3 destinationConfiguration never settles an execution, so no ScheduledQueryDestination status, processedIdentifier or errorMessage is ever reported (1 cause)

> _Note: this issue was identified against the code base at commit `02e30cec75724d494cfd983590fb8cff440247b8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetScheduledQueryHistory

## Description
One net-new root cause, grounded in both failing behavioral checks the suite lists under `### logs__s3` and marks neither ALREADY FILED nor OWNED. No open issue carries these checks' replay keys (a keyword search for 'scheduled-query' over the open issues returns nothing). This is the S3-destination face of the same missing execution path reported separately for the `logs` service token; it is filed separately because an issue carries a single service token.

=== Cause 1 of 1: a scheduled query configured with DestinationConfiguration.s3Configuration never produces a settled TriggerHistoryRecord, so neither successful S3 delivery nor a denied S3 PUT is ever recorded on the execution's destinations ===

AWS: Smithy: `CreateScheduledQuery.destinationConfiguration` is a `DestinationConfiguration` ("Configuration for where to deliver scheduled query results") whose `s3Configuration` is an `S3Configuration` with a required `destinationIdentifier` (S3Uri, @length 0..1024, @pattern `^s3://[a-z0-9][\.\-a-z0-9]{1,61}[a-z0-9](/.*)?$`), a required `roleArn`, and optional `ownerAccountId` (@pattern `^\d{12}$`) and `kmsKeyId`. `GetScheduledQueryHistory` returns `triggerHistory`, a list of `TriggerHistoryRecord`; each record's `destinations` is a `ScheduledQueryDestinationList` of `ScheduledQueryDestination` carrying `destinationType`, `destinationIdentifier` ("The identifier for the destination where results are delivered"), `status` (an `ActionStatus` enum — "The processing status of the destination delivery"), `processedIdentifier` ("The identifier of the processed result at the destination") and `errorMessage` ("Error message if destination processing failed"). The record's own `executionStatus` is the `ExecutionStatus` enum [Running, InvalidQuery, Complete, Failed, Timeout]. In AWS each firing of such a query runs, writes the result rows to the configured `s3://bucket/prefix`, and settles a TriggerHistoryRecord whose `destinations` entry names that S3 URI, a terminal `status`, and a `processedIdentifier` for the written object — one row per matched log event. When the `roleArn` is not permitted to PutObject at that destination, the execution still settles and the destination entry reports the failure through `status` plus a populated `errorMessage` instead of a `processedIdentifier`.

Emulator: the scheduled query is created with the S3 destinationConfiguration accepted, but no execution of it is ever triggered or settled. Polling GetScheduledQueryHistory for a settled execution carrying an S3 destination returns an empty list for the entire poll window — `Last result: []` in both checks. Consequently: no object is ever written to the configured bucket/prefix, so the row-for-row comparison against the seeded events cannot be made; and on the denied-PUT path no destination `status`/`errorMessage` is ever recorded, so a delivery failure is indistinguishable from 'never ran'.

Why: a client that points a scheduled Insights query at an S3 destination relies on it both to deliver the result object and to report per-destination delivery outcome in the execution history. Here it gets neither: the configured bucket stays empty forever with no error anywhere, and the IAM-denial case — the one a client most needs surfaced — produces no `errorMessage`, so a mis-scoped delivery role looks identical to a query that simply has not fired yet. Pipelines that consume the delivered object block indefinitely with no diagnosable cause.

Fix: the scheduled-query execution and destination-delivery path behind the CloudWatch Logs scheduled-query state — once a firing runs, write the result rows to `s3Configuration.destinationIdentifier` using `roleArn` (honouring `ownerAccountId`/`kmsKeyId`), and settle the TriggerHistoryRecord that the `GetScheduledQueryHistory` handler/serializer returns with a `destinations` entry carrying `destinationType`, `destinationIdentifier`, a terminal `status`, and either `processedIdentifier` on success or `errorMessage` describing the refusal when the delivery role is denied the PUT.

Traces: logs__s3.xsvc-logs-3-scheduled-query-s3-object-rows-equal-seeded-events, logs__s3.xsvc-logs-3-scheduled-query-s3-put-denied-records-destination-error.

## Evidence
Suite: 919/1114 passed (82.5%); service `logs__s3` 5/7 (71%), 2 failed. Both failures were marked neither ALREADY FILED nor OWNED, and both fail on the same missing behaviour.

[mismatch] logs__s3.xsvc-logs-3-scheduled-query-s3-object-rows-equal-seeded-events
  Expected (AWS): a firing of the scheduled query settles a TriggerHistoryRecord whose destinations entry names the configured s3:// URI with a terminal status and a processedIdentifier, and the written object's rows equal the seeded log events.
  Actual: `AssertionError: a settled execution of arn:aws:logs:<region>:<account>:scheduled-query:<name> with an S3 destination not met within <budget>s. Last result: []` — GetScheduledQueryHistory returned no settled execution at all, so no object and no destination record exist.

[mismatch] logs__s3.xsvc-logs-3-scheduled-query-s3-put-denied-records-destination-error
  Expected (AWS): with a delivery roleArn denied PutObject at the destination, the execution still settles and the ScheduledQueryDestination reports the failure via status plus a populated errorMessage (and no processedIdentifier).
  Actual: `AssertionError: a settled execution of arn:aws:logs:<region>:<account>:scheduled-query:<name> with an S3 destination not met within <budget>s. Last result: []` — no execution settles, so the denial is never reported.

The ARNs, generated query/bucket names and the 240s/300s poll budgets are per-run values, not requirements; the requirement is that a scheduled query with an s3Configuration settles an execution and reports its destination outcome at all.

## Steps to Reproduce
1. CreateBucket in S3 and CreateLogGroup in CloudWatch Logs; PutLogEvents a known set of events into the log group.
2. Create an IAM role for query execution, and a delivery role with s3:PutObject on `arn:aws:s3:::<bucket>/<prefix>/*` (for the negative leg, create a second delivery role WITHOUT that permission, or with an explicit Deny on it).
3. CreateScheduledQuery with queryLanguage=CWLI, a queryString selecting the seeded events, logGroupIdentifiers=[<log group ARN>], a scheduleExpression firing every minute, executionRoleArn=<execution role>, and destinationConfiguration={s3Configuration:{destinationIdentifier:'s3://<bucket>/<prefix>', roleArn:<delivery role>}}.
4. Poll GetScheduledQueryHistory(identifier=<scheduledQueryArn>, startTime=..., endTime=...) every 10s for several minutes, looking for a record whose executionStatus is terminal and whose `destinations` list is non-empty.
5. Observe: `triggerHistory` never contains such a record (`[]` throughout). ListObjectsV2 on the bucket/prefix returns no object. On the denied-PUT variant, no ScheduledQueryDestination `status` or `errorMessage` is ever recorded either.
6. In AWS, step 4 yields a settled record within a couple of schedule periods whose destinations entry names the s3:// URI with a terminal status — a processedIdentifier for the written object on the permitted role, or a populated errorMessage describing the refused PUT on the denied role.
