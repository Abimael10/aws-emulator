Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: redshift-data__redshift-serverless.geometric-and-money-literals-each-print-the-engines-normalized-form redshift-data__redshift-serverless.restore-into-same-namespace-erases-a-row-committed-after-the-snapshot
Issue-Key: redshift-data__redshift-serverless.geometric-and-money-literals-each-print-the-engines-normalized-form|_run_to_terminal#3|AssertionError|assert desc["Status"] == "FINISHED", (
Issue-Key: redshift-data__redshift-serverless|DeleteSnapshot|NotImplemented
Issue-Commit: a4215905cca27c128686d10d799e4c7536c2e42a

# redshift-data__redshift-serverless: 2 net-new causes — the serverless Data API SQL engine has no geometric/money types (SQLite-shaped 'no such function: point'), and DeleteSnapshot is an unimplemented 501 stub

> _Note: this issue was identified against the code base at commit `a4215905cca27c128686d10d799e4c7536c2e42a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ExecuteStatement

## Description
Two independent root causes, both reached through a redshift-serverless workgroup driven by the Redshift Data API.

Cause 1 of 2: ExecuteStatement on a serverless workgroup cannot evaluate Redshift's geometric and money types.
AWS: recorded behavior only — the Smithy model for redshift-data describes no SQL dialect. The recorded run shows each of `SELECT point(3, 4)`, `SELECT box '((2,3),(9,7))'`, `SELECT circle '<(5,6),4>'`, `SELECT lseg '[(1,2),(8,9)]'`, `SELECT path '[(1,1),(4,2),(6,5)]'`, `SELECT polygon '((0,0),(4,0),(4,3),(0,3))'` and `SELECT '256.78'::money` reaching DescribeStatement Status FINISHED with HasResultSet true, GetStatementResult returning exactly one column whose ColumnMetadata.typeName is the type's own name (point/box/circle/lseg/path/polygon/money) and one cell holding the ENGINE's normalized printed form — a stringValue for the geometric types and a doubleValue for money — never the literal text the SQL carried.
Emulator: the first statement never finishes. DescribeStatement reports Status 'FAILED' with Error 'no such function: point in SELECT point(3, 4) at offset 7'. That text is the underlying SQLite engine's wording ('no such function: <name>'), not a Redshift error, so no geometric or money literal can be evaluated, typed or printed at all.
Why: a client that stores or renders Redshift geometric or money columns through the Data API sees every such statement fail, and the failure it is handed carries a non-Redshift error string, so error-classification on the message fails too. Type-name and printed-form round trips (the only way a client learns the engine's normalization) are unreachable.
Fix: the SQL engine behind the serverless-workgroup Data API path — its type system/function registry must carry the geometric constructors and types and the money cast, and the Data API's DescribeStatement Error projection must not surface SQLite's wording. Related but NOT covering: open issue 20261029T193810Z-redshift-cluster-sql-engine-multi-statement-sql-refused.md records SQLite-shaped error text on the PROVISIONED cluster path; this cause is the serverless workgroup path and the geometric/money types specifically.
Traces: redshift-data__redshift-serverless.geometric-and-money-literals-each-print-the-engines-normalized-form.

Cause 2 of 2: redshift-serverless DeleteSnapshot is a scaffold stub that answers 501.
AWS: Smithy: DeleteSnapshot takes required `snapshotName` (String), returns `snapshot` (Snapshot), and models only ConflictException (409), InternalServerException (500), ResourceNotFoundException (404) and ValidationException (400). A snapshot that exists is deleted; afterwards GetSnapshot refuses with ResourceNotFoundException whose message is the fixed text 'The snapshot ' + <snapshotName from the request> + ' was not found. Please Check whether the snapshot exists and is owned by or shared with account ' + <the caller's account id> + '.' and ListSnapshots for the namespace no longer carries it.
Emulator: every DeleteSnapshot call is refused with HTTP 501, error code `NotImplemented`, message 'operation DeleteSnapshot is scaffolded but not implemented in this emulator'. 501/NotImplemented is not a modeled error for this operation, so clients cannot even classify it.
Why: the snapshot lifecycle has no terminating step — a client can create and restore snapshots but can never delete one, snapshots accumulate, and every teardown path that deletes a snapshot fails with an unmodeled status.
Fix: the DeleteSnapshot handler and the service's snapshot state (services/service-com-amazonaws-redshiftserverless-redshiftserverless/src/ops/delete_snapshot.rs is still marked SCAFFOLDED, NOT IMPLEMENTED and returns Scaffolded::not_implemented). Note: the suite did NOT mark this check as already filed and no open issue names redshift-serverless DeleteSnapshot.
Traces: redshift-data__redshift-serverless.restore-into-same-namespace-erases-a-row-committed-after-the-snapshot.

## Evidence
Cause 1 — expected: DescribeStatement Status 'FINISHED', HasResultSet true, GetStatementResult ColumnMetadata[0].typeName 'point', Records[0][0].stringValue the engine's normalized '(3.0,4.0)'. Actual: AssertionError: 'SELECT point(3, 4)' ended 'FAILED': 'no such function: point in SELECT point(3, 4) at offset 7'. Fixed text in the diverging string is SQLite's 'no such function: <name>'; the rest echoes the submitted SQL.

Cause 2 — expected: DeleteSnapshot 200 with the deleted Snapshot, then GetSnapshot 404 ResourceNotFoundException 'The snapshot <snapshotName> was not found. Please Check whether the snapshot exists and is owned by or shared with account <account>.' Actual: botocore.exceptions.ClientError: An error occurred (NotImplemented) when calling the DeleteSnapshot operation: operation DeleteSnapshot is scaffolded but not implemented in this emulator (HTTP 501).

## Steps to Reproduce
Cause 1: CreateNamespace(namespaceName=<ns>, dbName='dev'); wait AVAILABLE; CreateWorkgroup(workgroupName=<wg>, namespaceName=<ns>, baseCapacity=8, publiclyAccessible=false, subnetIds=[...]); wait AVAILABLE. Then redshift-data ExecuteStatement(WorkgroupName=<wg>, Database='dev', Sql="SELECT point(3, 4)") and poll DescribeStatement(Id=...) to a terminal status; it reaches FAILED instead of FINISHED. Repeat for the box/circle/lseg/path/polygon literals and "SELECT '256.78'::money".

Cause 2: With the same namespace/workgroup, CreateSnapshot(namespaceName=<ns>, snapshotName=<snap>, retentionPeriod=1); wait for GetSnapshot status AVAILABLE; then DeleteSnapshot(snapshotName=<snap>) — it answers HTTP 501 NotImplemented.
