Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: rds__rds-data__secretsmanager.aurora-mysql-backtrack-removes-a-later-data-api-write-from-both-surfaces
Issue-Key: rds__rds-data__secretsmanager.aurora-mysql-backtrack-removes-a-later-data-api-write-from-both-surfaces|_poll#35|Timeout|val = fn()
Issue-Commit: bf5aedd86bc1c51d8b010649bf7e2337ac48d430

# rds__rds-data__secretsmanager: the Aurora MySQL backtrack scenario reaches no verdict — the backtrack's terminal status is never observed, so the check is killed by the 60 s replay cap

> _Note: this issue was identified against the code base at commit `bf5aedd86bc1c51d8b010649bf7e2337ac48d430`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeDBClusterBacktracks

## Description
Cause 1 of 1: the backtrack lifecycle of an Aurora MySQL cluster never presents a terminal backtrack status to a client that polls DescribeDBClusterBacktracks for one, so the whole 'a backtrack removes a later Data API write from both surfaces' scenario consumes the entire replay budget without reaching a single post-backtrack assertion. The check is killed at the 60 s cap with no exception of its own; no control-plane call returned an error code, and the pre-backtrack assertions (HttpEndpointEnabled=True, BacktrackWindow as created, MasterUserSecret.SecretStatus 'active', the two Data API INSERTs and the two-row SELECT) are the ones that can be reached at all.

AWS: Smithy (services/service-com-amazonaws-rds-amazonrdsv19/smithy-model/com-amazonaws-rds-amazonrdsv19.json) BacktrackDBCluster returns BacktrackIdentifier, BacktrackRequestCreationTime, BacktrackTo, BacktrackedFrom, DBClusterIdentifier and Status, documented as one of 'applying' (being applied), 'completed' (successfully applied), 'failed' and 'pending'; DescribeDBClusterBacktracks reports the backtracks performed on a cluster, narrowable by BacktrackIdentifier, and is how a client learns the rewind finished. Recorded behavior: the check — recorded against AWS — polls DescribeDBClusterBacktracks(DBClusterIdentifier, BacktrackIdentifier) until Status is one of the UPPER-CASE terminal values 'COMPLETED' / 'FAILED', asserts Status == 'COMPLETED', then asserts BacktrackTo equals the accepted BacktrackTo truncated to the second, and finally that the row inserted after the recorded mark is gone from BOTH the Data API and a native MySQL session while the earlier row survives. AWS therefore reports a terminal backtrack status in that vocabulary and the rewind is observable on both surfaces.

Emulator: BacktrackDBCluster is implemented and completes the rewind synchronously (services/service-com-amazonaws-rds-amazonrdsv19/src/ops/backtrack_db_cluster.rs records a BacktrackRecord with status crate::state::BACKTRACK_STATUS_COMPLETED), and DescribeDBClusterBacktracks reports exactly the records it wrote. Hypothesis (the timeout carries no traceback line of its own, so the failing poll is identified from the code, not from the replay output): the reported Status is lower case — services/service-com-amazonaws-rds-amazonrdsv19/src/ops/describe_db_cluster_backtracks.rs states it outright in the filter-matching helper, "The status values the filter publishes are written in lower case, as the `Status` member reports them" — so a client polling for the terminal values AWS reports ('COMPLETED'/'FAILED') never sees one, and the poll (interval 10 s, budget 900 s) spins until the harness kills the check. Whatever the precise stalled step, the observable defect is the same: no terminal backtrack status is ever presented and the scenario reaches no verdict.

Why: a client doing the documented thing — request a backtrack, poll DescribeDBClusterBacktracks until the backtrack settles, then read the rewound data — never leaves the poll. Backtracking appears to hang forever even though the emulator has already rewound the storage, so no caller can sequence anything after a backtrack, and every post-backtrack behavior (the later write being gone from the Data API and from a native MySQL session, the earlier write surviving, backtracking again to fall back) is unreachable. A status vocabulary that differs in case from the one AWS reports also silently breaks any client that compares the status exactly.

Fix: the backtrack status representation shared by the two BACKTRACK-api handlers and their record — crate::state's BACKTRACK_STATUS_* constants and BacktrackRecord::to_wire (the rds crate's cluster state), as read by src/ops/backtrack_db_cluster.rs and src/ops/describe_db_cluster_backtracks.rs. Both operations must report the status in the vocabulary AWS reports (and the describe's case-insensitive db-cluster-backtrack-status filter must keep matching), so a terminal status is observable; if any step of the lifecycle is instead left un-settling, that settle belongs in the same two handlers.

Related open issue that does NOT carry this check's replay key for this failure: 20261126T075329Z-rds-rds-data-secretsmanager-reopens-an-available-aurora-db-cluster-still-publishes.md lists this check's id, but its replay key binds it to a PublishedUrlRejected refusal at ctx.endpoint_host(...) — that wall is no longer where this check stops (the published cluster host is now a loopback address the guard accepts), and the replay report did not mark this check OWNED. The SQL/transaction-semantics issues 20261122T073021Z-rds-rds-data-secretsmanager-{2,4}-net-new-causes.md describe behavior behind this poll and cover none of this cause.

Traces: rds__rds-data__secretsmanager.aurora-mysql-backtrack-removes-a-later-data-api-write-from-both-surfaces.

## Evidence
Check rds__rds-data__secretsmanager.aurora-mysql-backtrack-removes-a-later-data-api-write-from-both-surfaces [mismatch]:
  Error: Check timed out after 60.0s

The check raises no assertion of its own, so no assertion after the backtrack request was ever evaluated. Its only unbounded waits are poll loops whose predicates the emulator must satisfy; the one that gates every remaining assertion is:

  requested = rds.backtrack_db_cluster(DBClusterIdentifier=cl_id, BacktrackTo=backtrack_to, Force=True)
  backtrack = _poll(lambda: rds.describe_db_cluster_backtracks(DBClusterIdentifier=cl_id, BacktrackIdentifier=requested['BacktrackIdentifier'])['DBClusterBacktracks'][0],
                    lambda b: b.get('Status') in ('COMPLETED', 'FAILED'), timeout_s=900, interval_s=10)
  assert backtrack['Status'] == 'COMPLETED'

Expected (AWS): after BacktrackDBCluster returns a BacktrackIdentifier, DescribeDBClusterBacktracks for that identifier reports a terminal Status ('COMPLETED', the success value; 'FAILED' otherwise) with BacktrackTo equal to the accepted BacktrackTo truncated to the second, and the rows a session reads afterwards are the rows the cluster held at that time on both the Data API and a native MySQL session.
Actual (emulator): no terminal status is ever observed by the poll and the check is killed at the suite's 60 s cap. The handler-side evidence for the vocabulary mismatch is services/service-com-amazonaws-rds-amazonrdsv19/src/ops/describe_db_cluster_backtracks.rs (doc comment on the filter-matching helper: the Status member reports the status values in lower case) together with src/ops/backtrack_db_cluster.rs writing crate::state::BACKTRACK_STATUS_COMPLETED into the record it reports. Cluster identifiers, backtrack identifiers and timestamps in any one run are per-run values, not part of the requirement; the requirement is that a terminal status in AWS's vocabulary becomes observable and that the rewind is visible on both surfaces.

## Steps to Reproduce
1. Boot the emulator; point rds, rds-data, secretsmanager and ec2 clients at it.
2. CreateDBCluster with Engine='aurora-mysql' (default engine version), ManageMasterUserPassword=True, DatabaseName set, EnableHttpEndpoint=True, BackupRetentionPeriod=1, BacktrackWindow=86400, ServerlessV2ScalingConfiguration, a subnet group and a security group; then CreateDBInstance with DBInstanceClass='db.serverless' in that cluster.
3. Poll until the instance and the cluster report 'available' and MasterUserSecret.SecretStatus == 'active'.
4. Through the RDS Data API: CREATE TABLE, INSERT row id=1, read the engine clock (SELECT UTC_TIMESTAMP(6)) to pick a target time, then INSERT row id=2 after it; confirm both rows read back.
5. Call rds.backtrack_db_cluster(DBClusterIdentifier=<cluster>, BacktrackTo=<that time>, Force=True) and poll rds.describe_db_cluster_backtracks(DBClusterIdentifier=<cluster>, BacktrackIdentifier=<returned id>) for a Status in ('COMPLETED','FAILED').
6. Observe that no terminal status in that vocabulary is ever reported (the describe answers 200 with a non-terminal/lower-case Status), so the poll never ends and the scenario reaches none of its post-backtrack assertions; under the suite it is killed at the 60 s cap.
7. Replay rds__rds-data__secretsmanager.aurora-mysql-backtrack-removes-a-later-data-api-write-from-both-surfaces to confirm a fix.
