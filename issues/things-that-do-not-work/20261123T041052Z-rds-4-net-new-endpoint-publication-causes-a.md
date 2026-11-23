Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: rds.aurora-cluster-writer-and-reader-endpoints-share-cluster-domain rds.deploy-db-instance-cfn-behaves rds.mysql-custom-port-and-session-identity-match-creation-parameters rds.snapshot-restore-different-class-and-port rds.aurora-mysql-reader-endpoint-write-refused-read-only rds.aurora-writer-reader-endpoint-query-routing rds.sqlserver-express-admin-login-identity-and-privilege-boundary rds.sqlserver-express-database-table-round-trip-visible-to-second-session rds.sqlserver-express-managed-secret-login-and-dynamic-parameter-applies rds.sqlserver-express-transaction-rollback-and-unique-constraint-violation
Issue-Shape-Id: com.amazonaws.rds#AmazonRDSv19
Issue-Key: rds.aurora-cluster-writer-and-reader-endpoints-share-cluster-domain|run#12|AssertionError|assert suffix and reader == r_prefix + suffix, (
Issue-Key: rds.deploy-db-instance-cfn-behaves|run#12|AssertionError|assert endpoint["Address"].lower().startswith(instance_id.lower()), (
Issue-Key: rds.mysql-custom-port-and-session-identity-match-creation-parameters|run#7|AssertionError|assert inst["Endpoint"]["Port"] == _PORT, (
Issue-Key: rds.snapshot-restore-different-class-and-port|run#29|AssertionError|assert int(dst["Endpoint"]["Port"]) == _DST_PORT, (
Issue-Key: rds.aurora-mysql-reader-endpoint-write-refused-read-only|run#22|PublishedUrlRejected|reader_host, reader_port = ctx.endpoint_host(reader_ep, int(port))
Issue-Key: rds.aurora-writer-reader-endpoint-query-routing|run#31|PublishedUrlRejected|reader_ep_dial, reader_port_dial = ctx.endpoint_host(reader_ep, int(port))
Issue-Key: rds.sqlserver-express-admin-login-identity-and-privilege-boundary|run#8|PublishedUrlRejected|host, port = ctx.endpoint_host(inst["Endpoint"]["Address"], int(inst["Endpoint"]["Port"]))
Issue-Key: rds.sqlserver-express-database-table-round-trip-visible-to-second-session|run#6|PublishedUrlRejected|host, port = ctx.endpoint_host(inst["Endpoint"]["Address"], int(inst["Endpoint"]["Port"]))
Issue-Key: rds.sqlserver-express-managed-secret-login-and-dynamic-parameter-applies|run#6|PublishedUrlRejected|host, port = ctx.endpoint_host(inst["Endpoint"]["Address"], int(inst["Endpoint"]["Port"]))
Issue-Key: rds.sqlserver-express-transaction-rollback-and-unique-constraint-violation|run#6|PublishedUrlRejected|host, port = ctx.endpoint_host(inst["Endpoint"]["Address"], int(inst["Endpoint"]["Port"]))
Issue-Commit: 616e510b6c9fc5a742ae081415d2f4cf49efcb62

# rds: 4 net-new endpoint-publication causes — a DB instance/cluster writer Endpoint.Address is still the bare listener literal, Endpoint.Port is the OS-assigned port not the requested one, and the Aurora reader and SQL Server endpoints are published on hosts the emulator does not serve

> _Note: this issue was identified against the code base at commit `616e510b6c9fc5a742ae081415d2f4cf49efcb62`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeDBInstances

## Description
Four net-new root causes in how RDS publishes the address a client dials. All four are in the rds service and all four are observed on checks the replay report does NOT mark ALREADY FILED or OWNED.

Related open issues a fixer should read first, none of which carries these replay keys: 20261118T181500Z-rds-the-published-endpoint-of-a-served-engine-is-the-core-listeners.md (same family, but its keys are the earlier `assert port == 5432` / `assert writer_ep != reader_ep` failures; the endpoints have since MOVED to a DNS form, so the failure state below is different), 20261121T105515Z-rds-5-net-new-behavioral-causes-aurora-cluster.md, 20261122T000359Z-rds-db-instance-envelopes-publish-the-emulator-s.md, 20261121T073752Z-rds-db-cluster-envelopes-publish-the-emulator-s.md. Those do not cover these causes; they are named here so the work is not duplicated.

=== Cause 1 of 4: the WRITER endpoint of a DB instance and of a DB cluster is published as the bare listener literal, while the READER endpoint of the same cluster is published in AWS's DNS form ===
AWS: Smithy `Endpoint$Address` is documented "Specifies the DNS address of the DB instance" — one DNS name per resource, derived from that resource's own identifier, so `DBInstance.Endpoint.Address` begins with the DB instance identifier and `DBCluster.Endpoint` is `<cluster-id>.cluster-<suffix>.<region>.rds.amazonaws.com` whose `ReaderEndpoint` is the same name with the `cluster-ro-` prefix and the IDENTICAL suffix. The two cluster endpoints therefore always share one domain suffix.
Emulator: the writer side is the listener's bind literal. For a DB cluster the emulator answers `Endpoint='127.0.0.1'` while `ReaderEndpoint='<cluster-id>.cluster-ro-<suffix>.<region>.rds.localhost'`, so the two endpoints do not share a domain and the writer carries no resource identity at all; for a DB instance provisioned through CloudFormation `Endpoint.Address` is `127.0.0.1` where the identifier prefix is required. The reader path already builds a per-resource name, so the two sides of the same envelope are produced by different code.
Why: a client cannot tell two instances apart, cannot verify that a restore or a replacement produced a different resource, and cannot derive the writer name from the reader name (or vice versa) as every read/write-split client library does.
Fix: the DB-instance and DB-cluster envelope serializers / the state that records a resource's published endpoint — the writer address must be built from the resource identifier by the same code path that already builds the `cluster-ro-` reader name.
Traces: rds.aurora-cluster-writer-and-reader-endpoints-share-cluster-domain ("Endpoint must be <id>.cluster-<suffix> and ReaderEndpoint <id>.cluster-ro-<same suffix>; got Endpoint='127.0.0.1', ReaderEndpoint='<id>.cluster-ro-<suffix>.us-west-2.rds.localhost'"), rds.deploy-db-instance-cfn-behaves ("expected endpoint address to start with '<instance-id>', got '127.0.0.1'").

=== Cause 2 of 4: Endpoint.Port is the OS-assigned listener port, not the Port the request asked for ===
AWS: Smithy `Endpoint$Port` is "Specifies the port that the database engine is listening on". `CreateDBInstance$Port` and `RestoreDBInstanceFromDBSnapshot$Port` are echoed verbatim when supplied, and default to the engine family's port otherwise (PostgreSQL 5432, MySQL/MariaDB 3306, SQL Server 1433, Oracle 1521).
Emulator: the requested port is discarded and the port the OS assigned that resource's listener is published instead — a MySQL instance created with `Port=3315` reports `Endpoint.Port` of an ephemeral high port, and a snapshot restore asking for `Port=5433` reports a different ephemeral high port. The value is per-run, so no client can predict or configure against it.
Why: a client that configures a non-default port (a common hardening step) connects to the wrong port, and a tool that asserts "this instance serves its engine's documented port" is wrong on every resource.
Fix: `CreateDBInstance` / `RestoreDBInstanceFromDBSnapshot` handlers and the DB-instance state that records the port, plus the envelope serializer that reads it: record and publish the requested (or engine-default) port rather than the listener's assigned port.
Traces: rds.mysql-custom-port-and-session-identity-match-creation-parameters ("Endpoint.Port must be the requested 3315; got <ephemeral port>"), rds.snapshot-restore-different-class-and-port ("restore must publish the requested port 5433, got <ephemeral port>").

=== Cause 3 of 4: the Aurora cluster READER endpoint is published on a `.rds.localhost` host nothing serves, so reader routing is unaddressable ===
AWS: a cluster's `ReaderEndpoint` resolves and accepts a session, and that session is read-only — an Aurora MySQL reader refuses a write with MySQL error 1836 "Running in read-only mode" (ER_INNODB_READ_ONLY), and an Aurora PostgreSQL reader refuses one with SQLSTATE 25006.
Emulator: the host it announces for the reader, `<cluster-id>.cluster-ro-<suffix>.<region>.rds.localhost`, is not one of the emulator's own hosts, so the harness refuses the published target before any session is opened: `simparity.published.PublishedUrlRejected: endpoint_host was given <...>.rds.localhost:<port> (published by the emulator or built by the check): its host is not one of the emulator's own hosts`. The emulator announced an address other than one it is reached on, so no reader behaviour — routing, read-only refusal — can be observed at all.
Why: every read-scaling client is pointed at an unreachable name; the read/write split is announced but not served.
Fix: the DB-cluster endpoint publication path and the data-plane listener registration for cluster reader endpoints — the announced reader host must be a host the emulator answers on (its own serving host, or a loopback address for a loopback endpoint).
Traces: rds.aurora-mysql-reader-endpoint-write-refused-read-only, rds.aurora-writer-reader-endpoint-query-routing.

=== Cause 4 of 4: a SQL Server Express DB instance is published on the REAL AWS domain `<id>.<suffix>.<region>.rds.amazonaws.com`, which the emulator does not serve, so the whole sqlserver-ee/ex data plane is unreachable ===
AWS: a `sqlserver-ex` instance publishes `Endpoint.Address` as its own DNS name on port 1433 and accepts a TDS session there; the admin login is the master user with the instance's own privilege boundary, DDL/DML round-trips are visible to a second session, and a transaction rollback plus a unique-constraint violation behave as SQL Server documents.
Emulator: `Endpoint.Address` is announced as `<instance-id>.<suffix>.us-west-2.rds.amazonaws.com` on 1433 — a real AWS production hostname — and nothing in the emulator answers it: `PublishedUrlRejected: endpoint_host was given <id>.<suffix>.us-west-2.rds.amazonaws.com:1433 ... its host is not one of the emulator's own hosts`. Note this differs from Cause 3's suffix: the Aurora path announces `.rds.localhost` while the SQL Server path announces `.rds.amazonaws.com`, so the two are produced by different code and must both be corrected.
Why: announcing a real `amazonaws.com` name is worse than unreachable — a client that resolves it is pointed at AWS itself rather than at the emulator, and four distinct SQL Server behaviours (login identity, cross-session visibility, managed-secret login plus dynamic parameter apply, rollback/unique-constraint) cannot be exercised.
Fix: the `sqlserver-*` branch of the DB-instance endpoint publication path (and the engine's data-plane listener registration): publish a host the emulator serves, never the real AWS endpoint suffix.
Traces: rds.sqlserver-express-admin-login-identity-and-privilege-boundary, rds.sqlserver-express-database-table-round-trip-visible-to-second-session, rds.sqlserver-express-managed-secret-login-and-dynamic-parameter-applies, rds.sqlserver-express-transaction-rollback-and-unique-constraint-violation.

## Evidence
Cause 1: AssertionError: Endpoint must be <cluster-id>.cluster-<suffix> and ReaderEndpoint <cluster-id>.cluster-ro-<same suffix>; got Endpoint='127.0.0.1', ReaderEndpoint='<cluster-id>.cluster-ro-<suffix>.us-west-2.rds.localhost'. AssertionError: expected endpoint address to start with '<instance-id>', got '127.0.0.1'.
Cause 2: AssertionError: Endpoint.Port must be the requested 3315; got <OS-assigned ephemeral port>. AssertionError: restore must publish the requested port 5433, got <OS-assigned ephemeral port>. (The exact ephemeral numbers differ per run and are not part of the requirement; the requirement is that the published port equals the requested one.)
Cause 3: simparity.published.PublishedUrlRejected: endpoint_host was given <cluster-id>.cluster-ro-<suffix>.us-west-2.rds.localhost:<port> (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs an own host[:port].
Cause 4: simparity.published.PublishedUrlRejected: endpoint_host was given <instance-id>.<suffix>.us-west-2.rds.amazonaws.com:1433 (published by the emulator or built by the check): its host is not one of the emulator's own hosts.

## Steps to Reproduce
Cause 1: CreateDBCluster(Engine=aurora-mysql) with a writer member, poll to available, DescribeDBClusters — Endpoint and ReaderEndpoint must share one domain suffix and differ only by the `cluster-`/`cluster-ro-` prefix. Then CreateDBInstance (or deploy AWS::RDS::DBInstance via CloudFormation), poll to available, DescribeDBInstances — Endpoint.Address must begin with the DB instance identifier.
Cause 2: CreateDBInstance(Engine=mysql, Port=3315), poll to available, DescribeDBInstances — Endpoint.Port must be 3315. Then CreateDBSnapshot and RestoreDBInstanceFromDBSnapshot(Port=5433) — the restored instance's Endpoint.Port must be 5433.
Cause 3: CreateDBCluster(Engine=aurora-mysql) + writer, poll to available, read ReaderEndpoint, and open a MySQL session to it — the host must be one the emulator answers on, and a write on that session must be refused with MySQL error 1836 "Running in read-only mode".
Cause 4: CreateDBInstance(Engine=sqlserver-ex), poll to available, read Endpoint.Address — it must not be a real `*.rds.amazonaws.com` name, and a TDS session must be accepted at the announced host:1433.
