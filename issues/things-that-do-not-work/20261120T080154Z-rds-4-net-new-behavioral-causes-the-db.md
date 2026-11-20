Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: rds.db-proxy-mysql-data-plane-round-trip-pinning-and-iam-auth rds.proxy-default-endpoint-postgres-session-reaches-target rds.proxy-require-tls-rejects-plaintext-login rds.modify-activity-stream-resource-reference-rejected
Issue-Shape-Id: com.amazonaws.rds#AmazonRDSv19
Issue-Key: rds.db-proxy-mysql-data-plane-round-trip-pinning-and-iam-auth|run#24|AssertionError|assert resource_id.startswith("prx-"), (
Issue-Key: rds.proxy-default-endpoint-postgres-session-reaches-target|_rows#25|UndefinedTable|cur.execute(sql, params)
Issue-Key: rds.proxy-require-tls-rejects-plaintext-login|run#26|AssertionError|assert message == expected, (
Issue-Key: rds.modify-activity-stream-resource-reference-rejected|run#15|AssertionError|assert err_cluster.get("Message") == expected_cluster_msg, err_cluster.get("Message")
Issue-Commit: 8dd5be77b3d35a914e458e1c3403e40fd701f0e0

# rds: 4 net-new behavioral causes — the DB proxy ARN carries the proxy name instead of a prx- resource id, the PostgreSQL engine has no inet_server_addr(), a RequireTLS proxy refuses plaintext with pg_hba.conf text, and ModifyActivityStream's non-instance refusal carries the wrong message

> _Note: this issue was identified against the code base at commit `8dd5be77b3d35a914e458e1c3403e40fd701f0e0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDBProxy

## Description
Four net-new root causes on the `rds` service, all from failing behavioral checks that the replay reported with no owning issue and no blocking stub. Each cause is independent and must be closed separately.

Cause 1 of 4: a DB proxy's ARN uses the proxy NAME as its resource part, so the proxy has no immutable `prx-` resource id.
AWS: recorded behavior only for the id format (Smithy: `DBProxy$DBProxyArn` — "The Amazon Resource Name (ARN) for the proxy"; `CreateDBProxy.DBProxyName` is a 1..63 name matching `^[a-zA-Z](?:-?[a-zA-Z0-9]+)*$` and is NOT the ARN's resource part). AWS mints a separate, opaque, immutable proxy resource id of the form `prx-<lowercase hex>` and the ARN is `arn:aws:rds:<region>:<account-id>:db-proxy:prx-<hex>`. That same `prx-` id — not the proxy name — is the resource part of the IAM `rds-db:connect` dbuser ARN (`arn:aws:rds-db:<region>:<account-id>:dbuser:prx-<hex>/<db-user>`), which is how IAM authorization for proxy connections is expressed. Fixed template: `arn:aws:rds:` + region + `:` + account + `:db-proxy:prx-` + the minted hex id; region, account and the hex id are per-request/per-resource values, never literals from one run.
Emulator: `CreateDBProxy`/`DescribeDBProxies` report `DBProxyArn` as `arn:aws:rds:<region>:<account>:db-proxy:<DBProxyName>` — the resource part is the caller-supplied proxy name and no `prx-` id exists anywhere in the envelope.
Why: a client cannot derive the proxy's immutable id from the ARN, so it cannot build the `rds-db:connect` dbuser ARN that IAM authorization for an IAM-auth proxy is written against; any policy, log correlation or state keyed on the immutable id breaks, and two proxies that reuse a name over time become indistinguishable from distinct resources.
Fix: the proxy resource-id minting in the rds service state plus the ARN construction in the DBProxy projection used by CreateDBProxy/DescribeDBProxies (and by the dbuser-ARN authorization path). Open issue 20261109T205028Z-rds-global-cluster-proxy-and-cluster-envelopes-carry.md covers other DBProxy envelope members (VpcId, VpcSecurityGroupIds, Auth[].ClientPasswordAuthType, Endpoint host) but says nothing about the ARN's resource id, and does not cover this.
Traces: rds.db-proxy-mysql-data-plane-round-trip-pinning-and-iam-auth — `AssertionError: DBProxyArn 'arn:aws:rds:us-west-2:000000000000:db-proxy:sim2real-behav-rdspx-8cbbe75c9d7d' does not carry a prx- resource id`. Source: services/service-com-amazonaws-rds-amazonrdsv19/src/ops/create_db_proxy.rs.

Cause 2 of 4: the emulated PostgreSQL data plane does not provide the built-in `inet_server_addr()`.
AWS: recorded behavior only (data plane, not modeled in Smithy). An RDS for PostgreSQL session can call the server's built-in system-information functions; `SELECT inet_server_addr()::text` returns the address of the server the session is attached to for a TCP session, and that value is identical for two sessions that reach the same backend (directly and through an RDS Proxy). A genuinely absent function in PostgreSQL is refused with SQLSTATE 42883 (undefined_function) and the message `function <name>() does not exist`.
Emulator: the query is refused with SQLSTATE 42P01 (undefined_table — psycopg raises `psycopg.errors.UndefinedTable`) and the message `no such function: inet_server_addr`. That message text and the table-class SQLSTATE are the underlying storage engine's wording leaking through the PostgreSQL wire protocol, not PostgreSQL's.
Why: any client or tool that identifies the backend it reached (connection diagnostics, proxy-vs-direct verification, monitoring agents, `pg_stat`-style introspection) fails outright; and because the refusal is reported as an undefined TABLE rather than an undefined FUNCTION, error handling that branches on SQLSTATE class (42P01 vs 42883) takes the wrong branch and reports a missing relation that was never referenced.
Fix: the emulated PostgreSQL engine's built-in function catalog behind the RDS PostgreSQL listener — provide the server system-information functions (`inet_server_addr()`/`inet_server_port()` alongside the already-served `current_user`) and map an unknown function to PostgreSQL's 42883 `function <name>() does not exist` rather than the storage engine's "no such function" text in the undefined_table class. Related open issues 20261112T105401Z-rds-4-net-new-causes-the-emulated-postgresql.md and 20261117T083457Z-rds-7-net-new-behavioral-causes-in-the.md record other dialect gaps in this engine (string_agg, ANY(), INTERVAL, EXTRACT) but neither covers `inet_server_addr()` or the undefined-function error class.
Traces: rds.proxy-default-endpoint-postgres-session-reaches-target — `psycopg.errors.UndefinedTable: no such function: inet_server_addr` raised on `SELECT current_user, inet_server_addr()::text`.

Cause 3 of 4: a proxy created with RequireTLS=true refuses a plaintext login with PostgreSQL's pg_hba.conf message instead of RDS Proxy's own refusal.
AWS: recorded behavior only (Smithy: `CreateDBProxy.RequireTLS` (Boolean) — whether TLS is required for connections to the proxy). When RequireTLS is enabled, the REFUSAL comes from the proxy, not from the backend's host-based authentication: the client is refused with the message `This RDS Proxy requires TLS connections`. That text is fixed and has no variable parts.
Emulator: the plaintext login is refused with `no pg_hba.conf entry for host "<client ip>", user "<user>", database "<database>", no encryption` — i.e. a backend PostgreSQL host-based-auth refusal whose variable parts are the client address, user and database. The proxy's TLS requirement is never named.
Why: a client cannot tell "the proxy requires TLS" from "the database's pg_hba rules do not allow me"; remediation differs (enable sslmode on the connection vs. change server authentication config), and tooling that matches the documented proxy refusal text to retry with TLS never matches.
Fix: the RDS Proxy listener's connection-admission path — when the proxy's RequireTLS is set, refuse a non-TLS login at the proxy with RDS Proxy's message before the backend's authentication is consulted. Open issue 20261026T235825Z-lambda-rds-secretsmanager-an-rds-proxy-created-with.md reports that a RequireTLS MySQL proxy does not enforce TLS at all, and 20261105T081139Z-lambda-rds-secretsmanager-2-net-new-root-causes-e2a51e.md owns the Lambda-driven variant of the same probe; neither covers the PostgreSQL listener now refusing with pg_hba text, so this message defect is net-new.
Traces: rds.proxy-require-tls-rejects-plaintext-login — `AssertionError: a plaintext login to a RequireTLS proxy must be refused with 'This RDS Proxy requires TLS connections'; got 'no pg_hba.conf entry for host "127.0.0.1", user "sim2real", database "postgres", no encryption'`.

Cause 4 of 4: ModifyActivityStream's refusal of a non-DB-instance ResourceArn carries the wrong message.
AWS: Smithy models `ModifyActivityStream(ResourceArn: String, AuditPolicyState: AuditPolicyState)` with errors DBInstanceNotFoundFault (awsQueryError code `DBInstanceNotFound`, 404), InvalidDBInstanceStateFault (`InvalidDBInstanceState`, 400) and ResourceNotFoundFault (404); the refusal of a ResourceArn that is not a DB instance is the service-common `InvalidParameterValue` at HTTP 400 — recorded behavior only for its text, which is the fixed sentence `You can't modify the database activity stream on this resource.` with NO variable parts (the ARN is not echoed). The emulator already gets the sibling case right: a nonexistent DB-instance ARN is refused `DBInstanceNotFound` / `DBInstance not found: <instance identifier>`, which the same check asserts and passes.
Emulator: HTTP 400, code `InvalidParameterValue` (correct), message `Invalid resource name: <the ResourceArn exactly as supplied>` — fixed text `Invalid resource name: ` followed by the request's ARN. It reads as a malformed-ARN complaint for a well-formed DB cluster ARN, and it is not AWS's sentence.
Why: a caller that distinguishes "this ARN is malformed" from "activity streams are not modifiable on this kind of resource" (clusters vs instances) is told the wrong thing, and any assertion or log matcher on AWS's documented sentence fails.
Fix: the refusal branch of the ModifyActivityStream handler, services/service-com-amazonaws-rds-amazonrdsv19/src/ops/modify_activity_stream.rs — when the resolved ResourceArn is well formed but names a resource type whose activity stream cannot be modified (e.g. `:cluster:`), return InvalidParameterValue with AWS's fixed sentence and no echoed ARN.
Traces: rds.modify-activity-stream-resource-reference-rejected — `AssertionError: Invalid resource name: arn:aws:rds:us-west-2:000000000000:cluster:sim2real-dbc-161821948d6e` (the assertion prints the emulator's message; expected `You can't modify the database activity stream on this resource.`).

## Evidence
Cause 1 — rds.db-proxy-mysql-data-plane-round-trip-pinning-and-iam-auth, check line 417:
  expected: DBProxyArn resource part is the proxy's immutable id, `arn:aws:rds:<region>:<account>:db-proxy:prx-<hex>`
  actual:   `arn:aws:rds:us-west-2:000000000000:db-proxy:sim2real-behav-rdspx-8cbbe75c9d7d`
  `AssertionError: DBProxyArn '...' does not carry a prx- resource id` (the region/account/name shown are this run's values; the requirement is the `prx-` id form).

Cause 2 — rds.proxy-default-endpoint-postgres-session-reaches-target, check line 257 executing `SELECT current_user, inet_server_addr()::text`:
  expected: one row whose second column is the backend address, equal for a proxied and a direct session
  actual:   `psycopg.errors.UndefinedTable: no such function: inet_server_addr` (SQLSTATE 42P01; PostgreSQL would use 42883 `function inet_server_addr() does not exist` if the function were absent).

Cause 3 — rds.proxy-require-tls-rejects-plaintext-login, check line 356:
  expected message: `This RDS Proxy requires TLS connections`
  actual message:   `no pg_hba.conf entry for host "127.0.0.1", user "sim2real", database "postgres", no encryption` (host/user/database are per-run values; the defect is that a backend pg_hba refusal is served where the proxy's TLS refusal belongs).

Cause 4 — rds.modify-activity-stream-resource-reference-rejected, check line 60:
  ModifyActivityStream(ResourceArn=<a DB cluster ARN>, AuditPolicyState='locked')
  expected: HTTP 400, Code `InvalidParameterValue`, Message `You can't modify the database activity stream on this resource.`
  actual:   HTTP 400, Code `InvalidParameterValue` (match), Message `Invalid resource name: arn:aws:rds:us-west-2:000000000000:cluster:sim2real-dbc-161821948d6e` (the ARN is the request's value; AWS's message echoes no ARN).
  The preceding leg of the same check (nonexistent DB instance ARN -> `DBInstanceNotFound` / `DBInstance not found: <id>`) passes, so only the non-instance branch diverges.

## Steps to Reproduce
1. git_clone(), then run_behavioral_suite() and look at the `rds` group.
2. Cause 1: CreateDBProxy(DBProxyName=<name>, EngineFamily=MYSQL, RoleArn=..., VpcSubnetIds=[2 subnets]) then DescribeDBProxies(DBProxyName=<name>) and read DBProxyArn — observe the resource part is `<name>` rather than a minted `prx-<hex>` id; then try to build `arn:aws:rds-db:<region>:<account>:dbuser:<proxy resource id>/<user>` from it.
3. Cause 2: create a PostgreSQL DB instance, connect to its endpoint with sslmode=require and run `SELECT inet_server_addr()::text` — observe SQLSTATE 42P01 `no such function: inet_server_addr` instead of the backend address.
4. Cause 3: create a proxy with RequireTLS=true in front of that PostgreSQL instance, then open a session to the proxy endpoint with TLS disabled — observe the pg_hba.conf refusal instead of `This RDS Proxy requires TLS connections`.
5. Cause 4: call ModifyActivityStream(ResourceArn=`arn:aws:rds:<region>:<account>:cluster:<any cluster id>`, AuditPolicyState='locked') — observe InvalidParameterValue with message `Invalid resource name: <arn>` instead of `You can't modify the database activity stream on this resource.`
