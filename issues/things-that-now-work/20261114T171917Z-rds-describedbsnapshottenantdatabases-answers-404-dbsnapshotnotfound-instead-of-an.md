Issue-Source: trace-sweep-divergence
Issue-Trace: rds-snapshot-describe-empty
Issue-Step: task#3
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: rds-snapshot-describe-empty
Issue-Shape-Id: com.amazonaws.rds#AmazonRDSv19
Issue-Key: rds|DescribeDBSnapshotTenantDatabases|2xx->4xx|$.DescribeDBSnapshotTenantDatabasesResponse
Issue-Key: rds|DescribeDBSnapshotTenantDatabases|2xx->4xx|status
Issue-Commit: a2d6c7a74c3b04e1146167d89a2a7f728862e388

# rds DescribeDBSnapshotTenantDatabases answers 404 DBSnapshotNotFound instead of an empty 200 report

> _Note: this issue was identified against the code base at commit `a2d6c7a74c3b04e1146167d89a2a7f728862e388`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeDBSnapshotTenantDatabases

## Description
One cause.

Cause 1 of 1: DescribeDBSnapshotTenantDatabases refuses a request whose result set is EMPTY instead of serving an empty report (fixture rds-snapshot-describe-empty, step task#3).

AWS: Smithy (services/service-com-amazonaws-rds-amazonrdsv19/smithy-model/com-amazonaws-rds-amazonrdsv19.json) models DescribeDBSnapshotTenantDatabases with output shape DBSnapshotTenantDatabasesMessage carrying the list member `DBSnapshotTenantDatabases` (target DBSnapshotTenantDatabasesList) plus `Marker`, and declares exactly ONE error: DBSnapshotNotFoundFault, errorKind client, httpError 404, awsQueryError code `DBSnapshotNotFound` / httpResponseCode 404, documented as "DBSnapshotIdentifier doesn't refer to an existing DB snapshot." So the fault is reserved for a DB snapshot identifier that names no existing snapshot. The operation is a paginated report (inputToken Marker / outputToken Marker / items DBSnapshotTenantDatabases), and a report that selects nothing is a SUCCESS: HTTP 200 whose DescribeDBSnapshotTenantDatabasesResult carries a `DBSnapshotTenantDatabases` element with no members (the recording asserts exactly that — status 200 and an empty DBSnapshotTenantDatabases in the query-protocol envelope, alongside ResponseMetadata.RequestId). The documentation also notes the command "only applies to RDS for Oracle DB instances in the multi-tenant configuration" and is used "to inspect the tenant databases within a snapshot" — inspecting a snapshot that captured no tenant database yields nothing to report, not a missing-snapshot fault.

Emulator: the replayed step is answered with HTTP 404 and no DescribeDBSnapshotTenantDatabasesResponse envelope at all; the replay reports `status: expected 200, got 404` and `$.DescribeDBSnapshotTenantDatabasesResponse: missing`. The 404 carries this service's DBSnapshotNotFoundFault error class rather than the modeled success document.

Hypothesis (not stated by the model or the recording): the handler collapses "nothing matched" into the modeled fault. After narrowing, it drops every snapshot whose engine is not multi-tenant and then, when the surviving set is empty and the request named a snapshot (via DBSnapshotIdentifier or the db-snapshot-id filter), returns DBSnapshotNotFoundFault with the message `DBSnapshot <named> not found.` — where `DBSnapshot ` and ` not found.` are the fixed text and `<named>` is the identifier taken verbatim from the request. The same collapse applies to an existing snapshot that simply captured no tenant databases.

Why: a client that lists the tenant databases of a snapshot (to decide whether a restore will bring tenants with it, or to iterate a paginated report) gets a hard 404 ClientError for a snapshot that exists and is perfectly describable. Against real RDS the call succeeds with an empty list, so the client's "no tenant databases" branch is never reachable on the emulator and the SDK raises DBSnapshotNotFound instead; error-handling code then concludes the snapshot was deleted and may abort a restore or a cleanup loop. A 404 is also not retryable, so pollers that wait for an empty report never converge.

Fix: the operation's handler — the emptiness/not-found decision in `DescribeDBSnapshotTenantDatabases::handle`. DBSnapshotNotFoundFault must be raised ONLY when the identifier the request named resolves to no stored DB snapshot in the signed region; an existing snapshot that contributes no tenant database (including one taken off a non-multi-tenant engine) must be served as a 200 with an empty `DBSnapshotTenantDatabases` list and no Marker. The multi-tenant engine filter must narrow the report's rows, never participate in the existence test.

Traces: fixture rds-snapshot-describe-empty, step task#3 (the cluster's only member). Code: services/service-com-amazonaws-rds-amazonrdsv19/src/ops/describe_db_snapshot_tenant_databases.rs lines 264-289 (the multi-tenant filter feeding the `if in_scope.is_empty()` -> `named_snapshot(..)` -> `not_found(..)` path) and lines 349-356 (`db_snapshot_not_found_message`). Related but NOT covering this: the resolved issue 20261104T053605Z-rds-describedbsnapshottenantdatabases-stub.md only asked for the operation to be implemented at all (it answered 501 as a scaffold); the handler now exists and diverges on its empty-report semantics, so that resolved issue does not own this failure.

## Evidence
Replay of fixture `rds-snapshot-describe-empty`, step `task#3`, operation DescribeDBSnapshotTenantDatabases (service rds, verdict mismatch, divergence_key `service=rds|DescribeDBSnapshotTenantDatabases|status=200->404|keys=$.DescribeDBSnapshotTenantDatabasesResponse,status`).

Expected (recorded AWS response): HTTP 200 with body
  {'DescribeDBSnapshotTenantDatabasesResult': {'DBSnapshotTenantDatabases': ''}, 'ResponseMetadata': {'RequestId': '<per-run request id>'}}
i.e. a DescribeDBSnapshotTenantDatabasesResponse envelope whose result carries an EMPTY DBSnapshotTenantDatabases element. (The RequestId value is per-run and is not part of the requirement.)

Actual (emulator): HTTP 404; `$.DescribeDBSnapshotTenantDatabasesResponse: missing` — no success envelope was served. The status class changed from success to the operation's only modeled client fault, DBSnapshotNotFoundFault (awsQuery error code `DBSnapshotNotFound`, HTTP 404), whose message template in this handler is `DBSnapshot <identifier> not found.` — fixed text `DBSnapshot ` + ` not found.` with the identifier copied from the request.

Model evidence: smithy_query(query="operation", operation="DescribeDBSnapshotTenantDatabases") on services/service-com-amazonaws-rds-amazonrdsv19/smithy-model/com-amazonaws-rds-amazonrdsv19.json -> output {Marker, DBSnapshotTenantDatabases}; errors [{shape: DBSnapshotNotFoundFault, errorKind: client, httpError: 404, awsQueryError: {code: DBSnapshotNotFound, httpResponseCode: 404}}]; paginated {inputToken: Marker, outputToken: Marker, items: DBSnapshotTenantDatabases, pageSize: MaxRecords}. The fault's documentation: "DBSnapshotIdentifier doesn't refer to an existing DB snapshot."

## Steps to Reproduce
1. Boot the emulator built from this checkout and replay the wire corpus trace set simparity-wire-aws-v1.
2. Replay fixture `rds-snapshot-describe-empty` and watch step `task#3` (DescribeDBSnapshotTenantDatabases against the rds endpoint).
3. Observe HTTP 404 with the DBSnapshotNotFoundFault error class instead of HTTP 200 with a DescribeDBSnapshotTenantDatabasesResponse whose DescribeDBSnapshotTenantDatabasesResult carries an empty DBSnapshotTenantDatabases list.
4. Equivalently, by hand: create a DB instance and take a DB snapshot of it on an engine that is not Oracle multi-tenant, then call DescribeDBSnapshotTenantDatabases naming that snapshot's DBSnapshotIdentifier (or the db-snapshot-id filter). The emulator answers 404 `DBSnapshot <identifier> not found.`; AWS answers 200 with an empty DBSnapshotTenantDatabases list.
