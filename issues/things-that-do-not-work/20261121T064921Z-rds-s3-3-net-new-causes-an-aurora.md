Issue-Severity: HIGH
Issue-Commit: bab32ab3ecd51bb55a27809c2e18220867c710f4

# rds__s3: 3 net-new causes — an Aurora cluster's published writer Endpoint is an unserved '.rds.localhost' host, nothing answers MySQL at the published host:port, and the snapshot-export lifecycle never settles inside the run budget

> _Note: this issue was identified against the code base at commit `bab32ab3ecd51bb55a27809c2e18220867c710f4`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeDBClusters (also CreateDBSnapshot, StartExportTask)

## Description
3 net-new root causes, grounded in 8 failing behavioral checks of the rds__s3 group. None of these 8 checks was reported as OWNED by an open issue, and none was reported as blocked by an unimplemented scaffold stub; their 60-odd sibling failures in the same group were all reported as OWNED. A fixer must close all three causes before submitting.

=== Cause 1 of 3: the writer endpoint an available Aurora DB cluster publishes is not an address the emulator serves ===
Failing check ids for this cause: rds__s3.cli-rds-import-from-s3-aurora-workflow, rds__s3.rds-load-from-s3-missing-role-negative, rds__s3.rds-mysql-load-data-from-s3-missing-role-negative, rds__s3.rds-mysql-load-data-from-s3-positive.

AWS: Smithy (rds model, com-amazonaws-rds-amazonrdsv19): DBCluster.Endpoint is documented as "The connection endpoint for the primary instance of the DB cluster", and DBCluster.Port is the port a client uses with it. On AWS that value is a resolvable DNS name of the form <cluster-id>.cluster-<resource-hash>.<region>.rds.amazonaws.com, and a client inside the cluster's VPC opens a TCP connection to it on the cluster's engine port and completes a MySQL handshake with the master credentials. The contract a client depends on is that the address the control plane publishes is the address the database answers on; the concrete name, hash and port of one cluster are per-run values and are not part of the requirement.

Emulator: DescribeDBClusters/CreateDBCluster publish a writer Endpoint whose host ends in '.rds.localhost' and a Port that is an ephemeral per-run port, e.g. host '<cluster-id>.cluster-<hash>.us-west-2.rds.localhost' with port 33423/40329/34723/36617. That host is not one of the emulator's own hosts and resolves to nothing a customer of the emulator can dial, so the replay client refuses the published target before it can even connect: 'endpoint_host was given <cluster-id>.cluster-<hash>.us-west-2.rds.localhost:<port> (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs an own host[:port]: the endpoint host, a documented own host or, for a loopback endpoint, a loopback address' (simparity.published.PublishedUrlRejected). The emulator has announced an address other than the one it is reached on.

Why: DBCluster.Endpoint is the only thing AWS gives a client to reach its database. A client relying on AWS's behavior reads the endpoint from DescribeDBClusters and dials it; against this emulator that dial can never succeed, so every native-protocol scenario (LOAD DATA FROM S3, any SQL session, any s3Import authorization test) is unreachable and silently ungraded — a user cannot distinguish "my IAM/parameter-group wiring is wrong" from "no database is served at the address I was handed".

Fix: the rds crate's cluster endpoint publication and the listener behind it — the component that fills DBCluster.Endpoint/Port (and the identically shaped reader endpoint) must publish an address the emulator actually accepts engine connections on (its own endpoint host, a documented own host, or a loopback address with the port it listens on), and the MySQL/Postgres data-plane listener must be bound there for an 'available' cluster.

Related open issues, named here because these checks are NOT reported as owned by them: 20261121T053640Z-logs-rds-an-available-aurora-postgresql-cluster-publishes.md (same unserved '.rds.localhost' host, aurora-postgresql, logs__rds checks), 20261120T153000Z-pi-rds-the-published-db-instance-endpoint-is-unreachable-from-the-replay-client.md and 20261118T181500Z-rds-the-published-endpoint-of-a-served-engine-is-the-core-listeners.md (DB *instance* endpoints). The four checks above are DB *cluster* endpoints in the rds__s3 group and carry replay keys none of those issues holds.

=== Cause 2 of 3: when the published cluster endpoint IS dialled, no engine answers it inside the budget ===
Failing check id for this cause: rds__s3.rds-load-from-s3-role-association-positive.

AWS: recorded behavior only for the handshake itself (not modeled). Once DescribeDBClusters reports Status 'available', a TCP connect to DBCluster.Endpoint on DBCluster.Port from inside the cluster's VPC completes a MySQL handshake with the master user, and `LOAD DATA FROM S3 's3://<bucket>/<key>' INTO TABLE ...` run under a cluster whose aws_default_s3_role / associated s3Import role grants s3:GetObject+s3:ListBucket loads the object's rows, which SELECT then reads back.

Emulator: the check reaches the connection attempt and nothing answers; it ends in `TimeoutError: MySQL connection to <cluster-id>.cluster-<hash>.us-west-2.rds.localhost:<port>: the run budget ended first; not graded`. So beyond the address being un-dialable (cause 1), there is no engine listener serving the MySQL wire protocol for an 'available' cluster, and the s3Import role association can never be exercised.

Why: the s3Import grant is the only control separating an authorized from an unauthorized bulk import. With no engine on the wire, neither the success path nor any refusal is observable, so an import-authorization test written against this emulator always reaches no verdict.

Fix: the rds crate's engine data plane for clusters — an 'available' cluster must accept MySQL connections at the address it publishes, authenticate the master credentials, and execute LOAD DATA FROM S3 against the associated s3Import role.

=== Cause 3 of 3: the snapshot/export lifecycle makes no observable progress, so export workflow checks are killed by the replay budget ===
Failing check ids for this cause: rds__s3.cli-rds-create-snapshot-export-to-s3-workflow, rds__s3.cli-rds-start-export-task-workflow, rds__s3.rds-export-task-wrong-bucket-negative.

AWS: Smithy (rds model): CreateDBSnapshot answers a DBSnapshot that transitions to Status 'available'; StartExportTask answers an ExportTask whose Status begins 'STARTING'/'IN_PROGRESS' and settles 'COMPLETE' (or 'FAILED'/'CANCELED'), with objects written under s3://<bucket>/<export-task-identifier>/; DescribeExportTasks reports that settled task. StartExportTask models IamRoleMissingPermissions (HTTP 400, awsQueryError code IamRoleMissingPermissions) for a role lacking a grant on the named bucket; for the probe the negative check makes, AWS's message is the fixed sentence "The IAM role <role-arn> isn't authorized to call s3:GetBucketLocation on the S3 bucket <bucket>." — fixed text apart from the role ARN and bucket name, both taken from the request. These three scenarios all reach a verdict promptly on AWS: none of them sleeps unconditionally, every wait is a poll on a status the service moves.

Emulator: none of the three produces a graded comparison at all; each is terminated by the harness with `Error: Check timed out after 60.0s` before reaching its first assertion, so neither the COMPLETE export with objects under the export prefix nor the IamRoleMissingPermissions refusal above is ever observed. Nothing in these checks mandates a wait longer than the cap — the only local cause of non-convergence is an RDS lifecycle (DB instance 'available', snapshot 'available', or export task terminal) that does not settle.

Why: a client that polls DescribeDBSnapshots/DescribeExportTasks the way AWS's own CLI and waiters do never terminates, and a negative test that should get an immediate 400 IamRoleMissingPermissions gets no answer — the whole snapshot-export surface is unusable and any later regression in it is hidden behind the timeout.

Fix: the rds crate's snapshot and export-task state machines (CreateDBSnapshot/DescribeDBSnapshots status transitions and StartExportTask/DescribeExportTasks progression plus its S3 object delivery), and StartExportTask's role/bucket validation path, which must refuse before any polling begins.

Related open issues, named here because these three checks are NOT reported as owned by them: 20261120T150929Z-rds-s3-the-snapshot-export-lifecycle-never-settles.md, 20261119T132710Z-rds-s3-the-aurora-seeded-snapshot-export-manifest.md and 20261120T082455Z-rds-s3-the-addroletodbcluster-s3import-trust-scenario-reaches.md describe the same non-settling family for OTHER check ids; these three carry replay keys none of them holds.

Caveat for the fixer: for causes 2 and 3 the evidence is non-convergence rather than a captured wrong response. They are filed because what fails to converge is the emulator's own RDS control plane and engine data plane, not a tool the runner lacked — every API call in these checks is served by the emulator, and no check in this bundle sleeps unconditionally past the cap.

## Evidence
Behavioral suite (simparity-behavioral-aws-v1) replayed against the emulator built from this checkout. Overall 2192/2681 passed; in the `### rds__s3` section these 8 failures were reported as neither OWNED by an open issue nor blocked by an unimplemented scaffold stub.

Cause 1 — published cluster endpoint is not an emulator host (4 checks, identical exception):
[mismatch] rds__s3.cli-rds-import-from-s3-aurora-workflow
  File "context.py", line 665, in endpoint_host -> _published_target(f"{published_host}:{port}", "endpoint_host")
  simparity.published.PublishedUrlRejected: endpoint_host was given sim2real-aurora-e92806e975a7.cluster-ts8r7vzyil6b.us-west-2.rds.localhost:36617 (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs an own host[:port]: the endpoint host, a documented own host or, for a loopback endpoint, a loopback address
[mismatch] rds__s3.rds-load-from-s3-missing-role-negative
  ... endpoint_host was given sim2real-aur-norole-cluster-c60d9f15c537.cluster-iriqvwh3s6qg.us-west-2.rds.localhost:40329 ...
[mismatch] rds__s3.rds-mysql-load-data-from-s3-missing-role-negative
  ... endpoint_host was given sim2real-aurora-mysql-0e005523bf91.cluster-xt5h98dfipvl.us-west-2.rds.localhost:34723 ...
[mismatch] rds__s3.rds-mysql-load-data-from-s3-positive
  ... endpoint_host was given sim2real-aurora-mysql-4968fac14720.cluster-3ht9jrokam2q.us-west-2.rds.localhost:33423 ...
Expected (AWS): DescribeDBClusters publishes a writer Endpoint that resolves and accepts engine connections on DBCluster.Port (on AWS: <cluster-id>.cluster-<hash>.<region>.rds.amazonaws.com:3306); for an emulator, an address the emulator itself serves.
Actual (emulator): a '<cluster-id>.cluster-<hash>.<region>.rds.localhost' host with an ephemeral port that is not one of the emulator's own hosts. The cluster ids, hashes and ports above are per-run values and are not requirements; the requirement is that the published host:port is one the emulator answers on.

Cause 2 — nothing answers MySQL at the published endpoint (1 check):
[error] rds__s3.rds-load-from-s3-role-association-positive
  File "check_rds__s3_rds_load_from_s3_role_association_positive.py", line 51, in run -> line 354, in _load_from_s3_and_read_back
  TimeoutError: MySQL connection to sim2real-aur-cluster-0ee86d9edfe1.cluster-3mjbt1iik85u.us-west-2.rds.localhost:42201: the run budget ended first; not graded
Expected (AWS): the connection completes, LOAD DATA FROM S3 under the associated s3Import role loads the CSV rows and SELECT reads them back.
Actual (emulator): the connection never completes; no verdict.

Cause 3 — export lifecycle never settles (3 checks):
[mismatch] rds__s3.cli-rds-create-snapshot-export-to-s3-workflow
  Error: Check timed out after 60.0s
[mismatch] rds__s3.cli-rds-start-export-task-workflow
  Error: Check timed out after 60.0s
[mismatch] rds__s3.rds-export-task-wrong-bucket-negative
  Error: Check timed out after 60.0s
Expected (AWS): the first two reach ExportTask Status COMPLETE with at least one object under s3://<bucket>/<export-task-identifier>/; the third gets HTTP 400 / code IamRoleMissingPermissions with message "The IAM role <role-arn> isn't authorized to call s3:GetBucketLocation on the S3 bucket <bucket>." (fixed text; role ARN and bucket name come from the request) and no object written under <task-id>/ in the unauthorized bucket, then accepts the same role against the bucket it IS granted on.
Actual (emulator): all three are terminated at the cap with no assertion reached. None of the three contains an unconditional sleep; their waits are status polls (interval 2-30 s) that never observe a settled snapshot or export task.

## Steps to Reproduce
1. Build and boot the emulator from this checkout.
2. Replay the behavioral suite (simparity-behavioral-aws-v1) and look at the `### rds__s3` section.

Cause 1 (4 checks): create an S3 bucket with a CSV object, an s3Import IAM role granting s3:GetObject+s3:ListBucket on it, a DB subnet group and security group opening 3306, and a cluster parameter group; CreateDBCluster (engine aurora-mysql) + CreateDBInstance and wait for Status 'available'; read DescribeDBClusters -> DBClusters[0].Endpoint and .Port. Observed: the host is '<cluster-id>.cluster-<hash>.<region>.rds.localhost', which is not an address the emulator serves, so the client refuses the published target (PublishedUrlRejected at context.py endpoint_host) before dialling. Expected: a host:port the emulator accepts engine connections on.
3. Cause 2 (rds-load-from-s3-role-association-positive): same setup, then AddRoleToDBCluster with FeatureName s3Import, dial the published Endpoint:Port and run `LOAD DATA FROM S3 's3://<bucket>/<key>' INTO TABLE <db>.<table>` and SELECT the rows back. Observed: the TCP/MySQL connection never completes — TimeoutError naming the published endpoint; not graded.
4. Cause 3 (3 checks): CreateDBInstance in a private subnet group and wait 'available'; CreateDBSnapshot and poll DescribeDBSnapshots for Status 'available'; StartExportTask (SourceArn = the snapshot ARN, S3BucketName, IamRoleArn, KmsKeyId) and poll DescribeExportTasks for a terminal Status, then list s3://<bucket>/<export-task-identifier>/. Observed: the check is killed at the 60 s cap before any assertion. For rds-export-task-wrong-bucket-negative, call StartExportTask with a role granted only on bucket A while naming existing bucket B and expect an immediate HTTP 400 IamRoleMissingPermissions with the s3:GetBucketLocation sentence; observed: no answer inside the budget.
