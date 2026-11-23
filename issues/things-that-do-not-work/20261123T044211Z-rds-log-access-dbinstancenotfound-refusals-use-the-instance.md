Issue-Source: trace-sweep-divergence
Issue-Trace: rds-db-instance-core-lifecycle-and-features
Issue-Step: task#11
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: rds-db-instance-core-lifecycle-and-features rds-db-instance-target-error-paths
Issue-Shape-Id: com.amazonaws.rds#AmazonRDSv19
Issue-Key: rds|DescribeDBLogFiles|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Key: rds|DownloadDBLogFilePortion|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Commit: 04bfb1df2ab3a04d24aee0f88623716656b604a8

# rds log-access DBInstanceNotFound refusals use the instance-scoped sentence for an identifier that never existed (2 causes)

> _Note: this issue was identified against the code base at commit `04bfb1df2ab3a04d24aee0f88623716656b604a8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeDBLogFiles

## Description
Two net-new wire causes in the rds emulator, both on the DB-log-access surface, both the same fault seen through a different operation: a DBInstanceIdentifier that resolves to NO instance is refused with the instance-scoped 'not ready' sentence instead of the control-plane not-found sentence. Status (404) and error code (DBInstanceNotFound) are correct in both; only the Message text diverges. There are 2 causes; BOTH must be closed before this issue is resubmitted.

Neither divergence is marked ALREADY FILED or OWNED in the replay report. The nearest open issue is 20261122T222647Z-rds-3-net-new-behavioral-causes-the-db.md, which describes the same collapse from the BEHAVIORAL corpus (simparity-behavioral-aws-v1, checks rds.describe-db-log-files / rds.log-file-access-on-not-ready-instance-error) — it carries no wire replay key for these two fixture steps, so these wire bindings are filed here; a fixer closing one should re-replay both.

=== Cause 1 of 2: DescribeDBLogFiles on an identifier that never existed ===
AWS: Smithy (services/service-com-amazonaws-rds-amazonrdsv19/smithy-model/com-amazonaws-rds-amazonrdsv19.json): DescribeDBLogFiles declares error DBInstanceNotFoundFault, errorKind client, httpError 404, awsQueryError code DBInstanceNotFound, documented "DBInstanceIdentifier doesn't refer to an existing DB instance.". The model does not state the message text, so the wording is recorded behavior only: for an identifier that resolves to no DB instance in the request's region, the recorded 404 body carries Message 'DBInstance not found: <DBInstanceIdentifier>' — fixed text 'DBInstance not found: ' followed by the DBInstanceIdentifier taken verbatim from the request, with NO trailing period. (The instance-scoped sentence 'DBInstance <DBInstanceIdentifier> not found.' is the one RDS words the *exists-but-still-creating* case with; code and status are identical, only the sentence differs.)
Emulator: answers 404 / DBInstanceNotFound with the instance-scoped sentence instead — Message 'DBInstance <DBInstanceIdentifier> not found.' (fixed text 'DBInstance ' + the requested identifier + ' not found.'). The handler states the collapse in its own comment: "Either miss is ONE error class, so both carry the single sentence this service words that class with".
Why: on these log-access operations the Message is the ONLY signal that distinguishes 'this identifier does not exist, stop' from 'this instance is still being created, retry' — status and code are the same for both. A client or log-collection tool that matches AWS's sentence sees an unrecognised string, and a wait/retry loop keyed on it spins forever on a typo'd identifier.
Fix: the DBInstanceNotFoundFault construction in the DescribeDBLogFiles handler must select the sentence by cause — the shared control-plane not-found wording when the identifier resolves to no instance in the region, and the instance-scoped wording only when a stored instance has not yet left 'creating' — instead of calling the single state helper for both arms.
Traces: rds-db-instance-core-lifecycle-and-features#task#11 (also rds-db-instance-target-error-paths#task#15).

=== Cause 2 of 2: DownloadDBLogFilePortion on an identifier that never existed ===
AWS: Smithy: DownloadDBLogFilePortion declares DBInstanceNotFoundFault, errorKind client, httpError 404, awsQueryError code DBInstanceNotFound (same documentation), alongside DBInstanceNotReadyFault (400) and DBLogFileNotFoundFault (404). Message wording is recorded behavior only and is the same control-plane sentence as Cause 1: 'DBInstance not found: <DBInstanceIdentifier>', fixed text plus the identifier from the request, no trailing period.
Emulator: same substitution as Cause 1 — 404 / DBInstanceNotFound with Message 'DBInstance <DBInstanceIdentifier> not found.'.
Why: identical client impact for the log-download path: a caller cannot tell a non-existent instance from one that is merely provisioning, and the sentence it does receive matches neither AWS form for this cause.
Fix: the same cause-dependent selection in the DownloadDBLogFilePortion handler's DBInstanceNotFoundFault arm.
Traces: rds-db-instance-core-lifecycle-and-features#task#12 (also rds-db-instance-target-error-paths#task#16). Code: services/service-com-amazonaws-rds-amazonrdsv19/src/ops/describe_db_log_files.rs lines 186-204 (the `match` whose `_` arm covers both misses) and the equivalent arm in src/ops/download_db_log_file_portion.rs; the shared sentence is crate::state::db_instance_not_found_message.

## Evidence
Cause 1 — fixture rds-db-instance-core-lifecycle-and-features step task#11, operation DescribeDBLogFiles, expected_status 404, actual_status 404, verdict mismatch. $.ErrorResponse.Error.Message: value_mismatch — expected 'DBInstance not found: <missing-identifier>', got 'DBInstance <missing-identifier> not found.' (the replayed identifier was sim2real-probe-rds-dbinst-missing-c6a7d4cb; the identifier is a per-run value, the two differing fixed texts are the requirement). Cluster also covers rds-db-instance-target-error-paths#task#15.

Cause 2 — fixture rds-db-instance-core-lifecycle-and-features step task#12, operation DownloadDBLogFilePortion, expected_status 404, actual_status 404, verdict mismatch. $.ErrorResponse.Error.Message: value_mismatch — expected 'DBInstance not found: <missing-identifier>', got 'DBInstance <missing-identifier> not found.'. Cluster also covers rds-db-instance-target-error-paths#task#16.

In both causes the HTTP status (404), Error.Type (Sender) and Error.Code (DBInstanceNotFound) matched the recording; the sole diverging key is $.ErrorResponse.Error.Message.

## Steps to Reproduce
Cause 1: against the emulator's rds endpoint, call DescribeDBLogFiles(DBInstanceIdentifier=<an identifier that was never created in this region>) -> expect HTTP 404, ErrorResponse.Error.Code 'DBInstanceNotFound' and Error.Message 'DBInstance not found: <that identifier>' (no trailing period); the emulator answers 'DBInstance <that identifier> not found.'. Control: create a db.t3.micro instance and, while DescribeDBInstances still reports DBInstanceStatus 'creating', call DescribeDBLogFiles on it -> that case must keep 'DBInstance <identifier> not found.' (it already matches, so the fix must not flip it).

Cause 2: call DownloadDBLogFilePortion(DBInstanceIdentifier=<an identifier that was never created>, LogFileName=<any>) -> expect HTTP 404, Code 'DBInstanceNotFound', Message 'DBInstance not found: <that identifier>'; the emulator answers 'DBInstance <that identifier> not found.'.

Replay to confirm: rds-db-instance-core-lifecycle-and-features#task#11 and #task#12 (and the clustered rds-db-instance-target-error-paths#task#15 / #task#16).
