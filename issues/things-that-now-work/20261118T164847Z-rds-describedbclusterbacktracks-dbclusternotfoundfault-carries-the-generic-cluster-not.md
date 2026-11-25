Issue-Source: trace-sweep-divergence
Issue-Trace: rds-db-cluster-state-roles-and-capacity-operations
Issue-Step: task#8
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: rds-db-cluster-state-roles-and-capacity-operations
Issue-Shape-Id: com.amazonaws.rds#AmazonRDSv19
Issue-Key: rds|DescribeDBClusterBacktracks|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Commit: b8754674c77ef7b2559f37a9d307034c37821ae3

# rds DescribeDBClusterBacktracks DBClusterNotFoundFault carries the generic cluster-not-found sentence instead of its own fixed wording

> _Note: this issue was identified against the code base at commit `b8754674c77ef7b2559f37a9d307034c37821ae3`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeDBClusterBacktracks

## Description
Cause 1 of 1: DescribeDBClusterBacktracks, called with a DBClusterIdentifier that names no DB cluster in the signed region, returns the right fault class and status but the wrong error message text.

AWS: the Smithy model declares DescribeDBClusterBacktracks with errors DBClusterBacktrackNotFoundFault and DBClusterNotFoundFault, both @httpError 404 with awsQueryError code DBClusterNotFoundFault / DBClusterBacktrackNotFoundFault (Smithy: DescribeDBClusterBacktracks.errors; DBClusterNotFoundFault doc: "DBClusterIdentifier doesn't refer to an existing DB cluster"). The model does not state message wording, so the message is recorded behavior only: the recording shows this front door answering HTTP 404, code DBClusterNotFoundFault, with the message 'Could not find specified DB Cluster'. That whole sentence is FIXED text for this operation's not-found class — it has NO variable part, and in particular it does not interpolate the requested identifier.

Emulator: same 404 and same error code, but the message body is 'DBCluster <db-cluster-identifier> not found.' — the generic per-identifier template this service's other cluster readers use, built from the shared helper crate::state::db_cluster_not_found_message(&named). Its fixed text ('DBCluster ' ... ' not found.') differs from AWS's fixed text for this operation, and it additionally splices in a request-supplied value (the DBClusterIdentifier the caller passed) where AWS splices in nothing. (In the replayed step that value happened to be the probe cluster id, which is a per-run name and not itself part of the requirement.)

Why: a client that keys off the error message of a backtrack lookup — error-message assertions in integration tests, log/alert matchers, SDK wrappers that re-wrap RDS faults by message, or UI text shown to a user — gets text no real RDS endpoint ever produces, so the branch that matches AWS's 'Could not find specified DB Cluster' never fires against the emulator and a message-equality check fails even though the status and code are right.

Fix: in the DescribeDBClusterBacktracks handler's not-found branch — it must raise DBClusterNotFoundFault with this operation's own fixed message rather than calling the shared generic cluster-not-found message helper in the service state module. The status (404) and the code (DBClusterNotFoundFault) are already correct and must not change.

Traces: rds-db-cluster-state-roles-and-capacity-operations#task#8 (the only affected step in the cluster). Code: services/service-com-amazonaws-rds-amazonrdsv19/src/ops/describe_db_cluster_backtracks.rs lines 191-197 construct DBClusterNotFoundFault with crate::state::db_cluster_not_found_message(&named).

Related open issue, which does NOT cover this: 20261104T053605Z-rds-describedbclusterbacktracks-stub.md ("Implement rds DescribeDBClusterBacktracks: unimplemented scaffold stub") describes the operation as a 501 scaffold. In this checkout the operation is implemented and answers 404 on the replayed step, and the replay did not attribute this divergence to that issue (no stub_path, no owner), so it is filed net-new.

## Evidence
Fixture rds-db-cluster-state-roles-and-capacity-operations, step task#8, operation DescribeDBClusterBacktracks on a non-existent DB cluster.
Expected (recorded AWS): HTTP 404, ErrorResponse.Error.Code = DBClusterNotFoundFault, ErrorResponse.Error.Message = 'Could not find specified DB Cluster' (entire sentence is fixed text; no request- or state-derived parts).
Actual (emulator): HTTP 404, same error code, ErrorResponse.Error.Message = 'DBCluster <requested DBClusterIdentifier> not found.' — fixed text 'DBCluster ' + ' not found.' with the request's DBClusterIdentifier interpolated. Replay detail: "$.ErrorResponse.Error.Message: value_mismatch (expected 'Could not find specified DB Cluster', got 'DBCluster sim2real-probe-rds-rdsclusterrolemissing-68cbf8da not found.')"; divergence_key service=rds|DescribeDBClusterBacktracks|status=404->404|keys=$.ErrorResponse.Error.Message; verdict mismatch; affected_count 1.

## Steps to Reproduce
1. Boot the rds emulator from this checkout.
2. Call DescribeDBClusterBacktracks with a DBClusterIdentifier that refers to no DB cluster in the signed region (no cluster created first).
3. Observe HTTP 404 with Error.Code DBClusterNotFoundFault and Error.Message 'DBCluster <that identifier> not found.' instead of AWS's fixed 'Could not find specified DB Cluster'.
4. Equivalently, replay fixture rds-db-cluster-state-roles-and-capacity-operations step task#8.
