Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: rds.describe-snapshot-tenant-databases rds.mariadb-require-secure-transport-plaintext-login-rejected
Issue-Shape-Id: com.amazonaws.rds#AmazonRDSv19
Issue-Key: rds.describe-snapshot-tenant-databases|_client_error#13|AssertionError|raise AssertionError(f"expected DBSnapshotNotFound, but the call SUCCEEDED and returned {result!r}")
Issue-Key: rds|ModifyDBParameterGroup|InvalidParameterValue
Issue-Commit: 860eb666e989875e1daa85fefb4b7f7b73b30cd3

# rds: 3 net-new behavioral causes — every DB instance publishes the same loopback Endpoint.Address, DescribeDBSnapshotTenantDatabases answers 200 for an existing non-multi-tenant snapshot, and the mariadb parameter family has no require_secure_transport

> _Note: this issue was identified against the code base at commit `860eb666e989875e1daa85fefb4b7f7b73b30cd3`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

> _Causes 2 and 3 are fixed. Cause 1 — two live DB instances of a SERVED engine publishing the
> one address the core binds — is blocked on the read-only core
> (`vendor/server/src/listeners.rs`: `bind_std`/`bind_tokio` bind every resource listener on the
> one operator-chosen interface, and `ResourceListener::host`/`port` are the only address a
> service can publish), so its three trace ids and replay keys were moved to the open report
> that already owns that blocker and states the required core change:
> `issues/things-that-do-not-work/20261118T181500Z-rds-the-published-endpoint-of-a-served-engine-is-the-core-listeners.md`
> (its cause 1). The header above therefore carries only the keys that pass._

## Operation
DescribeDBInstances

## Description
Three independent root causes in the RDS emulator, all grounded in failing behavioral checks of the `rds` suite that no open issue carries. Each cause is numbered and lists its own failing check ids; a fixer must close all three before submitting.

=== Cause 1 of 3: a DB instance's Endpoint.Address is the same host for every instance, so a read replica (and a replaced instance) is indistinguishable from the instance it was made from ===
AWS: `DBInstance.Endpoint` is the modeled `Endpoint` structure {Address, Port, HostedZoneId} (Smithy: operations CreateDBInstance / CreateDBInstanceReadReplica / DescribeDBInstances all return `DBInstance`; `Endpoint.Address` is documented as the DNS address of the DB instance). Every DB instance — including a read replica created by CreateDBInstanceReadReplica from a source instance, and the brand-new instance CloudFormation provisions when a DBName change forces a replacement — owns its OWN address, derived from its own DB instance identifier (`<db-instance-identifier>.<random>.<region>.rds.amazonaws.com`), so a client can point readers at the replica and can tell a replacement apart from the instance it replaced. No two live instances ever share one address.
Emulator: the replica and its source both report Endpoint.Address `127.0.0.1` (replay: "the replica must serve its own endpoint so a reader can be pointed at it; it advertises '127.0.0.1', the same host as the source"), and after a CloudFormation replacement the new instance still reports Endpoint.Address `127.0.0.1` ("replacement kept the same Endpoint.Address"). The host is a single shared value rather than one per instance; only the recorded value happens to be the loopback host of that run — the requirement is that the addresses DIFFER per instance and derive from the instance's identifier, not that any particular host or port is used.
Why: a client cannot route reads to a replica or verify that a replacement happened. Any scenario that opens a session "on the replica" silently talks to the primary, so replica read-only semantics, replication lag and promotion effects are all unobservable, and a CloudFormation replacement looks like an in-place update.
Fix: the DB instance endpoint projection in the service's state (the instance-to-wire projection in services/service-com-amazonaws-rds-amazonrdsv19/src/state.rs) and the read-replica creation path (src/ops/create_db_instance_read_replica.rs) must mint and publish a per-instance endpoint keyed on the instance identifier, and the data plane must serve each instance on the endpoint it publishes. Related but NOT the same cause: open issue 20261109T225353Z-rds-an-oracle-db-instance-publishes-endpoint-address.md (an Oracle instance publishes an unserved *.rds.amazonaws.com host) and 20261111T060457Z-rds-secretsmanager-describedbclusters-returns-a-literal-host-port-in-readerendpoint.md (cluster-level endpoints); this cause is about two DB INSTANCES sharing one address.
Traces: rds.read-replica-replication-query-and-promote, rds.replica-uncommitted-primary-write-absent-until-commit, rds.deploy-rds-2-cfn-mysql-sql-then-dbname-change-replaces-instance.

=== Cause 2 of 3: DescribeDBSnapshotTenantDatabases answers HTTP 200 with an empty list for an EXISTING snapshot of a non-multi-tenant instance, instead of refusing it as not found ===
AWS: the operation "only applies to RDS for Oracle DB instances in the multi-tenant configuration" and models exactly one error, `DBSnapshotNotFoundFault` (Smithy: DescribeDBSnapshotTenantDatabases errors = DBSnapshotNotFoundFault, httpError 404, awsQueryError code `DBSnapshotNotFound`). Recorded behavior: naming a snapshot that exists but was taken from a single-tenant (here postgres) instance is refused exactly like a never-created identifier — HTTP 404, Code `DBSnapshotNotFound`, Message `DBSnapshot <DBSnapshotIdentifier> not found.` The fixed text is `DBSnapshot ` … ` not found.`; the only variable part is the `DBSnapshotIdentifier` the request supplied, echoed in the request's own spelling. An unnarrowed call (no DBSnapshotIdentifier) still answers 200 with no entry for that snapshot.
Emulator: the call succeeds — HTTP 200 with `{'DBSnapshotTenantDatabases': []}`. The handler raises the 404 only when the store holds no snapshot record under the identifier, then drops a non-multi-tenant snapshot with the `is_multi_tenant_engine` filter further down, so an existing postgres snapshot becomes a successful empty report.
Why: a client that uses this call to decide whether a snapshot is a multi-tenant Oracle snapshot before restoring it gets "yes, zero tenants" for every non-CDB snapshot instead of the refusal AWS gives, so it cannot distinguish "this snapshot has no tenant databases" from "this snapshot is not a multi-tenant snapshot at all".
Fix: the snapshot-resolution step of the operation's handler, services/service-com-amazonaws-rds-amazonrdsv19/src/ops/describe_db_snapshot_tenant_databases.rs — when `DBSnapshotIdentifier` names a snapshot whose engine is not a multi-tenant one, that is the modeled `DBSnapshotNotFoundFault`, not a row filtered out of the report.
Traces: rds.describe-snapshot-tenant-databases.

=== Cause 3 of 3: ModifyDBParameterGroup refuses `require_secure_transport` on a mariadb-family parameter group ===
AWS: `require_secure_transport` is a modifiable (dynamic) parameter of the MariaDB parameter-group families, so ModifyDBParameterGroup on a group whose family is the region's default mariadb family (as reported by DescribeDBEngineVersions(Engine='mariadb', DefaultOnly=true).DBParameterGroupFamily) accepts `require_secure_transport` and answers 200 with the group name; DescribeDBParameters for that group then lists the parameter. Smithy models no error for "unknown parameter" beyond InvalidParameterValue, and AWS raises it only for a name the family genuinely lacks.
Emulator: HTTP 400 `InvalidParameterValue`, message "Could not find parameter with name: require_secure_transport" — the emulator's parameter catalog for the mariadb family does not contain the parameter at all, so the whole TLS-enforcement scenario (a plaintext login must be refused with "Connections using insecure transport are prohibited while --require_secure_transport=ON.") can never be set up.
Why: a client cannot enforce TLS on a MariaDB instance through its parameter group — the configuration AWS accepts is rejected outright, and every downstream behavior that depends on it is unreachable.
Fix: the engine parameter catalog that backs ModifyDBParameterGroup / DescribeDBParameters for the mariadb families (services/service-com-amazonaws-rds-amazonrdsv19/src/ops/modify_db_parameter_group.rs and the parameter catalog in src/state.rs) must carry `require_secure_transport` for the mariadb families, and the emulated MariaDB engine must honour it. Related open issues that do NOT cover this parameter: 20261113T145556Z-rds-parameter-group-reads-are-served-from-the.md and 20261114T091417Z-rds-reference-catalogs-and-parameter-metadata-are-emulator.md.
Traces: rds.mariadb-require-secure-transport-plaintext-login-rejected.

## Evidence
Cause 1 — rds.read-replica-replication-query-and-promote: AssertionError "the replica must serve its own endpoint so readers can be pointed at it"; rds.replica-uncommitted-primary-write-absent-until-commit: AssertionError "the replica must serve its own endpoint so a reader can be pointed at it; it advertises '127.0.0.1', the same host as the source"; rds.deploy-rds-2-cfn-mysql-sql-then-dbname-change-replaces-instance: AssertionError "replacement kept the same Endpoint.Address '127.0.0.1'". Expected: each DB instance's DescribeDBInstances Endpoint.Address is its own identifier-derived address, so replica != source and replacement != replaced. Actual: one shared address for all instances.

Cause 2 — rds.describe-snapshot-tenant-databases: AssertionError "expected DBSnapshotNotFound, but the call SUCCEEDED and returned {'DBSnapshotTenantDatabases': [], 'ResponseMetadata': {... 'HTTPStatusCode': 200 ...}}" for DescribeDBSnapshotTenantDatabases(DBSnapshotIdentifier=<an existing postgres snapshot>). Expected: HTTP 404, Code DBSnapshotNotFound, Message "DBSnapshot <snapshot-id> not found." (the same refusal the never-created identifier already gets). Actual: HTTP 200 with an empty DBSnapshotTenantDatabases list.

Cause 3 — rds.mariadb-require-secure-transport-plaintext-login-rejected: botocore.exceptions.ClientError: An error occurred (InvalidParameterValue) when calling the ModifyDBParameterGroup operation: Could not find parameter with name: require_secure_transport. Expected: 200 with the modified group (the parameter exists in the mariadb family). Actual: 400 InvalidParameterValue.

## Steps to Reproduce
1. Build and boot the emulator from this checkout and replay the behavioral suite (run_behavioral_suite).
2. Cause 1: CreateDBInstance (mysql/postgres), wait for available, CreateDBInstanceReadReplica from it, wait for available, then DescribeDBInstances on both and compare Endpoint.Address — they are identical. Separately, deploy an AWS::RDS::DBInstance via CloudFormation, update DBName to force a replacement, and compare Endpoint.Address before and after.
3. Cause 2: CreateDBInstance (Engine=postgres), CreateDBSnapshot, wait until the snapshot is available, then call DescribeDBSnapshotTenantDatabases(DBSnapshotIdentifier=<that snapshot>) — it answers 200 with an empty list instead of 404 DBSnapshotNotFound "DBSnapshot <id> not found.".
4. Cause 3: read DescribeDBEngineVersions(Engine='mariadb', DefaultOnly=true).DBParameterGroupFamily, CreateDBParameterGroup in that family, then ModifyDBParameterGroup with Parameters=[{ParameterName:'require_secure_transport', ParameterValue:'1', ApplyMethod:'immediate'}] — refused 400 InvalidParameterValue "Could not find parameter with name: require_secure_transport".
5. Re-replay the checks in trace_ids to confirm the fix.
