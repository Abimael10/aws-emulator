Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: rds__s3.cli-rds-import-from-s3-aurora-workflow rds__s3.rds-load-from-s3-missing-role-negative rds__s3.rds-mysql-load-data-from-s3-missing-role-negative rds__s3.rds-mysql-load-data-from-s3-positive rds__s3.rds-load-from-s3-role-association-positive rds__s3.cli-rds-create-snapshot-export-to-s3-workflow rds__s3.cli-rds-start-export-task-workflow rds__s3.rds-export-task-wrong-bucket-negative
Issue-Key: rds__s3.cli-rds-import-from-s3-aurora-workflow|run#25|PublishedUrlRejected|endpoint, port = ctx.endpoint_host(cluster["Endpoint"], cluster["Port"])
Issue-Key: rds__s3.rds-load-from-s3-missing-role-negative|run#12|PublishedUrlRejected|endpoint_dial, port_dial = ctx.endpoint_host(endpoint, int(port))
Issue-Key: rds__s3.rds-mysql-load-data-from-s3-missing-role-negative|_mysql_load_from_s3#15|PublishedUrlRejected|host, port = ctx.endpoint_host(host, port)
Issue-Key: rds__s3.rds-mysql-load-data-from-s3-positive|_mysql_load_from_s3#15|PublishedUrlRejected|host_dial, port_dial = ctx.endpoint_host(host, int(port))
Issue-Key: rds__s3.rds-load-from-s3-role-association-positive|_load_from_s3_and_read_back#13|TimeoutError|raise TimeoutError(f"MySQL connection to {endpoint}:{port}: the run budget ended first; not graded")
Issue-Key: rds__s3.cli-rds-create-snapshot-export-to-s3-workflow|_poll_until#16|Timeout|time.sleep(interval_s)
Issue-Key: rds__s3.cli-rds-start-export-task-workflow|_wait_for#13|Timeout|time.sleep(interval_s)
Issue-Key: rds__s3.rds-export-task-wrong-bucket-negative|_start_export_task_expect_error#11|Timeout|time.sleep(interval_s)
Issue-Commit: bccbad003e7099c3c9072bea417f676e8df40816

# rds__s3: 3 net-new causes — an available Aurora cluster publishes a writer Endpoint on a host the emulator does not serve, no engine answers at that host:port, and the snapshot/export lifecycle never settles

> _Note: this issue was identified against the code base at commit `bccbad003e7099c3c9072bea417f676e8df40816`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeDBClusters

## Description
3 net-new root causes, grounded in the 8 failing behavioral checks of the `### rds__s3` section that the replay report marked NEITHER "OWNED by" an open issue NOR "ALREADY FILED" (blocked by an unimplemented scaffold stub). All ~60 sibling failures in that section were reported as OWNED and are skipped here. A fixer must close all three causes before submitting.

Prior filing, named because the report does not report these checks as owned by it: 20261121T064921Z-rds-s3-3-net-new-causes-an-aurora.md describes the same three causes and lists the same eight check ids in its prose, but it carries no replay key for them (the suite still reports them unowned), so they resurfaced on this pass. Treat that file and this one as the same work item if the triager prefers; this filing carries the eight check ids as trace_ids so the ownership index can bind them.

=== Cause 1 of 3: the writer endpoint an available Aurora DB cluster publishes is not an address the emulator serves ===
Failing check ids for this cause: rds__s3.cli-rds-import-from-s3-aurora-workflow, rds__s3.rds-load-from-s3-missing-role-negative, rds__s3.rds-mysql-load-data-from-s3-missing-role-negative, rds__s3.rds-mysql-load-data-from-s3-positive.

AWS: Smithy (rds model, services/service-com-amazonaws-rds-amazonrdsv19/smithy-model/com-amazonaws-rds-amazonrdsv19.json): DBCluster.Endpoint is documented "The connection endpoint for the primary instance of the DB cluster", used together with DBCluster.Port. On AWS that value is a resolvable DNS name of the shape <cluster-id>.cluster-<resource-hash>.<region>.rds.amazonaws.com, and a client in the cluster's VPC opens a TCP connection to it on the cluster's engine port (3306 aurora-mysql, 5432 aurora-postgresql) and completes the engine handshake with the master credentials. The requirement is that the address the control plane publishes is the address the database answers on; the concrete cluster id, hash and port of any one run are per-run values and are not part of the requirement.

Emulator: once the cluster reports Status 'available', DescribeDBClusters publishes a writer Endpoint whose host ends in '.rds.localhost' together with an ephemeral per-run port — of the shape '<cluster-id>.cluster-<hash>.<region>.rds.localhost:<ephemeral-port>'. That host is not one of the emulator's own hosts and resolves to nothing a customer of the emulator can dial, so the client refuses the published target before dialling: simparity.published.PublishedUrlRejected "endpoint_host was given <host>:<port> (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs an own host[:port]: the endpoint host, a documented own host or, for a loopback endpoint, a loopback address" (context.py line 665, endpoint_host -> _published_target). The emulator announced an address other than the one it is reached on.

Why: DBCluster.Endpoint is the only thing AWS hands a client to reach its database. A client that reads the endpoint from DescribeDBClusters and dials it can never connect against this emulator, so every native-protocol scenario — LOAD DATA FROM S3, aws_s3.table_import_from_s3, any SQL session, any s3Import authorization test — is unreachable and silently ungraded. A user cannot distinguish "my IAM/parameter-group wiring is wrong" from "no database is served at the address I was handed".

Fix: the rds crate's cluster-endpoint publication and the listener behind it — whatever fills DBCluster.Endpoint/Port (and the identically shaped ReaderEndpoint) must publish an address the emulator accepts engine connections on (its own endpoint host, a documented own host, or a loopback address with the port it listens on), and the engine data-plane listener must be bound there while the cluster is 'available'.

Traces: the four check ids above; all four die with the identical PublishedUrlRejected, differing only in the per-run cluster id/hash/port.

=== Cause 2 of 3: when the published cluster endpoint IS dialled, no engine answers it ===
Failing check id for this cause: rds__s3.rds-load-from-s3-role-association-positive.

AWS: recorded behavior only for the wire handshake (not modeled). Once DescribeDBClusters reports Status 'available', a TCP connect to DBCluster.Endpoint on DBCluster.Port completes a MySQL handshake with the master user, and `LOAD DATA FROM S3 's3://<bucket>/<key>' INTO TABLE <db>.<table>` executed on a cluster whose associated s3Import role (AddRoleToDBCluster, FeatureName s3Import) grants s3:GetObject + s3:ListBucket on that bucket loads the object's rows, which a following SELECT reads back.

Emulator: the check reaches the connection attempt and nothing answers; it ends in TimeoutError "MySQL connection to <cluster-id>.cluster-<hash>.<region>.rds.localhost:<port>: the run budget ended first; not graded". So beyond the address being un-dialable (cause 1) there is no engine listener serving the MySQL wire protocol for an 'available' cluster, and the s3Import role association can never be exercised.

Why: the s3Import grant is the only control separating an authorized bulk import from an unauthorized one. With no engine on the wire, neither the success path nor any refusal is observable, so an import-authorization test written against this emulator always reaches no verdict.

Fix: the rds crate's engine data plane for clusters — an 'available' cluster must accept engine connections at the address it publishes, authenticate the master credentials, and execute LOAD DATA FROM S3 under the associated s3Import role.

Traces: rds__s3.rds-load-from-s3-role-association-positive (check file line 51 -> line 354, _load_from_s3_and_read_back).

=== Cause 3 of 3: the snapshot/export lifecycle makes no observable progress, so the export workflow checks are killed by the replay budget ===
Failing check ids for this cause: rds__s3.cli-rds-create-snapshot-export-to-s3-workflow, rds__s3.cli-rds-start-export-task-workflow, rds__s3.rds-export-task-wrong-bucket-negative.

AWS: Smithy (rds model): CreateDBSnapshot answers a DBSnapshot that transitions to Status 'available'; StartExportTask answers an ExportTask whose Status member is documented with exactly CANCELED | CANCELING | COMPLETE | FAILED | IN_PROGRESS | STARTING, starting at STARTING/IN_PROGRESS and settling COMPLETE, with the exported objects written under s3://<S3Bucket>/<ExportTaskIdentifier>/; DescribeExportTasks reports that settled task. For a role holding no grant on the bucket named in the request, StartExportTask refuses with HTTP 400 and awsQueryError code IamRoleMissingPermissions, message "The IAM role <role-arn> isn't authorized to call s3:GetBucketLocation on the S3 bucket <bucket>." — that sentence is fixed text apart from the role ARN and the bucket name, both taken verbatim from the request (never from one recording).

Emulator: none of the three scenarios produces a graded comparison at all; each is terminated by the harness with "Error: Check timed out after 60.0s" before reaching its first assertion, so neither the COMPLETE export with objects under the export prefix nor the IamRoleMissingPermissions refusal above is ever observed. Nothing in these three checks mandates a wait longer than the cap: there is no unconditional sleep, every wait is a poll on a status the service is supposed to move (CreateDBSnapshot -> DescribeDBSnapshots 'available', StartExportTask -> DescribeExportTasks terminal), and the refusal case should answer immediately, before any polling. The only local cause of non-convergence is an RDS lifecycle (DB instance 'available', snapshot 'available', export task terminal) that does not settle, or a StartExportTask validation that neither accepts nor refuses promptly.

Why: a client that polls DescribeDBSnapshots/DescribeExportTasks the way AWS's own CLI waiters do never terminates, and a negative test that should get an immediate 400 IamRoleMissingPermissions gets no answer at all — the whole snapshot-export surface is unusable and any later regression in it stays hidden behind the timeout.

Fix: the rds crate's snapshot and export-task state machines (CreateDBSnapshot/DescribeDBSnapshots transitions, StartExportTask/DescribeExportTasks progression and its S3 object delivery) plus StartExportTask's role/bucket validation path, which must refuse before any polling begins.

Traces: the three check ids above. Related open issues, named because the report does not report these three as owned by them: 20261120T150929Z-rds-s3-the-snapshot-export-lifecycle-never-settles.md, 20261119T132710Z-rds-s3-the-aurora-seeded-snapshot-export-manifest.md, 20261120T082455Z-rds-s3-the-addroletodbcluster-s3import-trust-scenario-reaches.md — same non-settling family, other check ids.

Caveat for the fixer: for causes 2 and 3 the evidence is non-convergence rather than a captured wrong response. They are filed because what fails to converge is the emulator's own RDS control plane and engine data plane — every API call in these checks is served by the emulator, no tool was missing, and no check in this bundle sleeps unconditionally past the cap.

## Evidence
Behavioral suite (simparity-behavioral-aws-v1) replayed against the emulator built from this checkout. Overall 2193/2681 passed (81.8%); 398 mismatched, 90 errored. In the `### rds__s3` section these 8 failures were reported as neither OWNED by an open issue nor blocked by an unimplemented scaffold stub.

Cause 1 — published cluster endpoint is not an emulator host (4 checks, identical exception):
[mismatch] rds__s3.cli-rds-import-from-s3-aurora-workflow
  File "context.py", line 665, in endpoint_host -> self._published_target(f"{published_host}:{port}", "endpoint_host")
  simparity.published.PublishedUrlRejected: endpoint_host was given sim2real-aurora-f37b2058883c.cluster-99ajp9b1nivx.us-west-2.rds.localhost:41941 (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs an own host[:port]: the endpoint host, a documented own host or, for a loopback endpoint, a loopback address
[mismatch] rds__s3.rds-load-from-s3-missing-role-negative
  ... endpoint_host was given sim2real-aur-norole-cluster-c1827092f80d.cluster-ngt0diuweypf.us-west-2.rds.localhost:33843 ...
[mismatch] rds__s3.rds-mysql-load-data-from-s3-positive
  ... endpoint_host was given sim2real-aurora-mysql-c78f42c0226a.cluster-3illkodzzg6d.us-west-2.rds.localhost:33265 ...
[mismatch] rds__s3.rds-mysql-load-data-from-s3-missing-role-negative
  ... same PublishedUrlRejected on its own per-run '<cluster-id>.cluster-<hash>.us-west-2.rds.localhost:<port>' ...
Expected (AWS): DescribeDBClusters publishes a writer Endpoint that resolves and accepts engine connections on DBCluster.Port (on AWS: <cluster-id>.cluster-<hash>.<region>.rds.amazonaws.com:3306); for an emulator, an address the emulator itself answers on.
Actual (emulator): a '<cluster-id>.cluster-<hash>.<region>.rds.localhost' host with an ephemeral port that is not one of the emulator's own hosts. The cluster ids, hashes and ports quoted above are per-run values and are NOT the requirement; the requirement is that the published host:port is one the emulator serves.

Cause 2 — nothing answers the engine protocol at the published endpoint (1 check):
[error] rds__s3.rds-load-from-s3-role-association-positive
  File "check_rds__s3_rds_load_from_s3_role_association_positive.py", line 51, in run -> line 354, in _load_from_s3_and_read_back
  TimeoutError: MySQL connection to sim2real-aur-cluster-4889a902d4cc.cluster-a3ms4xnv6zwu.us-west-2.rds.localhost:43493: the run budget ended first; not graded
Expected (AWS): the connection completes; LOAD DATA FROM S3 under the associated s3Import role loads the CSV rows and SELECT reads back the loaded row count (the check asserts 3 rows for its own 3-row CSV).
Actual (emulator): the connection never completes; no verdict reached.

Cause 3 — snapshot/export lifecycle never settles (3 checks):
[mismatch] rds__s3.cli-rds-create-snapshot-export-to-s3-workflow — Error: Check timed out after 60.0s
[mismatch] rds__s3.cli-rds-start-export-task-workflow — Error: Check timed out after 60.0s
[mismatch] rds__s3.rds-export-task-wrong-bucket-negative — Error: Check timed out after 60.0s
Expected (AWS): the first two reach ExportTask Status COMPLETE (Status values are modeled: CANCELED|CANCELING|COMPLETE|FAILED|IN_PROGRESS|STARTING) with at least one object, including a .parquet object, under s3://<bucket>/<ExportTaskIdentifier>/; the third gets HTTP 400 with code IamRoleMissingPermissions and message "The IAM role <role-arn> isn't authorized to call s3:GetBucketLocation on the S3 bucket <bucket>." (fixed sentence; the role ARN and bucket name are the request's own values), writes nothing under <task-id>/ in the unauthorized bucket, and then accepts the same role against the bucket it IS granted on, with DescribeExportTasks reporting STARTING/IN_PROGRESS/COMPLETE.
Actual (emulator): all three are killed at the 60 s cap with no assertion reached. None of the three contains an unconditional sleep; their waits are status polls (interval 2–30 s, caps 120–1800 s) that never observe a settled snapshot or export task, and the negative case never receives its immediate refusal.

## Steps to Reproduce
1. Build and boot the emulator from this checkout; replay the behavioral suite (simparity-behavioral-aws-v1) and read the `### rds__s3` section.
2. Cause 1 (4 checks): create an S3 bucket holding a CSV object, an s3Import IAM role granting s3:GetObject + s3:ListBucket on it, a DB subnet group and a security group opening the engine port, and a cluster parameter group naming the role; CreateDBCluster (engine aurora-mysql or aurora-postgresql) + CreateDBInstance and wait for Status 'available'; read DescribeDBClusters -> DBClusters[0].Endpoint and .Port. Observed: the host is '<cluster-id>.cluster-<hash>.<region>.rds.localhost' with an ephemeral port, not an address the emulator serves, so the client refuses the published target (PublishedUrlRejected at context.py endpoint_host) before dialling. Expected: a host:port the emulator accepts engine connections on.
3. Cause 2 (rds-load-from-s3-role-association-positive): same setup, then AddRoleToDBCluster with FeatureName s3Import, dial the published Endpoint:Port, run `LOAD DATA FROM S3 's3://<bucket>/<key>' INTO TABLE <db>.<table>` and SELECT the rows back. Observed: the TCP/MySQL connection never completes — TimeoutError naming the published endpoint; not graded.
4. Cause 3 (3 checks): CreateDBInstance in a private DB subnet group and poll DescribeDBInstances for 'available'; CreateDBSnapshot and poll DescribeDBSnapshots for Status 'available'; StartExportTask (SourceArn = the snapshot ARN, S3BucketName, IamRoleArn, KmsKeyId) and poll DescribeExportTasks for a terminal Status, then list s3://<bucket>/<ExportTaskIdentifier>/ and expect at least one .parquet object. Observed: the check is killed at the 60 s cap before any assertion. For rds-export-task-wrong-bucket-negative, call StartExportTask with a role granted only on bucket A while naming existing bucket B and expect an immediate HTTP 400 IamRoleMissingPermissions carrying the s3:GetBucketLocation sentence; observed: no answer inside the budget.
