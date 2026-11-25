Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda__rds__secretsmanager.lambda-rds-proxy-missing-permission-denied
Issue-Key: lambda__rds__secretsmanager.lambda-rds-proxy-missing-permission-denied|run#33|AssertionError|assert ok_result["ok"] is True, (
Issue-Commit: 74a62806d9bf98fd5d44f7f49a4e3b8f76e4fbff

# lambda__rds__secretsmanager: an IAM-auth RDS Proxy accepts no connection at its published endpoint (MySQL 2003), so rds-db:connect authorization is unobservable

> _Note: this issue was identified against the code base at commit `74a62806d9bf98fd5d44f7f49a4e3b8f76e4fbff`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDBProxy

## Description
1 net-new root cause.

Cause 1 of 1: a DB proxy created with IAM authentication REQUIRED and RequireTLS=true reports itself available, with an available registered target, but nothing accepts a MySQL connection at the endpoint it publishes — so neither the authorized nor the unauthorized IAM-auth outcome can occur.

AWS: DescribeDBProxies/CreateDBProxy return DBProxy.Endpoint, documented in the Smithy model as "The endpoint that you can use to connect to the DB proxy. You include the endpoint value in the connection string for a database client application." (rds model, shape DBProxy, member Endpoint). Against real AWS, once DBProxy.Status is `available` and the registered target's status is `available`, a client in the proxy's VPC that dials that endpoint on port 3306 completes a TLS MySQL handshake: a principal holding `rds-db:connect` on `arn:aws:rds-db:<region>:<account>:dbuser:<proxy-resource-id>/<db-user>` authenticates with a generate_db_auth_token password and its query runs; a principal holding no such grant is refused by the proxy with MySQL error 1045 (access denied for user) and no query is ever executed. The IAM-auth/rds-db:connect semantics are not expressed in the Smithy model — recorded behavior only for them; the model only requires that Endpoint be the address a client connects to.
Emulator: the connect attempt from inside the proxy's VPC never reaches a MySQL server. pymysql fails with OperationalError code 2003 ("Can't connect to MySQL server on <published endpoint>") for the role that IS granted rds-db:connect on the exact dbuser ARN, so the positive control fails before the negative case is ever evaluated: `AssertionError: role granted rds-db:connect on arn:aws:rds-db:<region>:<account>:dbuser:<proxy-resource-id>/admin must authenticate through the IAM-auth proxy and run a query; got {'ok': False, 'mysql_error_code': 2003}`. In that message the fixed text is AWS-independent harness wording; the ARN, the proxy resource id and the user name are values taken from the request and the emulator's own DescribeDBProxies response, and `2003` is the MySQL client error the emulator's (absent) listener produced where the two legitimate outcomes are a successful handshake or 1045. The same shape of failure appears in the password-credential proxy path (lambda__rds__secretsmanager.lambda-rds-connect-via-proxy-positive, whose invocation kept returning FunctionError until the run budget ended and is therefore reported as not graded) — corroboration, not a separate graded cause.
Why: a client that follows AWS's contract — create proxy, wait for `available`, read Endpoint, dial it — gets a transport-level connection failure indistinguishable from a VPC/security-group misconfiguration. Every behavior that only exists on the proxy's wire is unreachable: IAM proxy authentication, the 1045 refusal that proves a missing rds-db:connect grant, RequireTLS enforcement and connection pinning. Worse, an authorization test written against this emulator sees a failure for the *authorized* principal and can wrongly conclude its policy is wrong.
Fix: the RDS service's DB-proxy data plane — the listener behind DBProxy.Endpoint (the address published by CreateDBProxy/DescribeDBProxies must be one the emulator actually accepts MySQL connections on) and the IAM-auth decision it must make for the token user against `rds-db:connect` on the dbuser ARN, which must answer MySQL 1045 when the grant is absent and complete the handshake when it is present. Related but not covering: open issue 20261109T135215Z-lambda-rds-secretsmanager-db-proxy-endpoints-are-published.md describes the published proxy host not being an address the emulator serves (keyed to deep-rds-3-proxy-require-tls-plaintext-rejected and two scheduler checks), and 20261026T235825Z-lambda-rds-secretsmanager-an-rds-proxy-created-with.md describes RequireTLS not being enforced; neither carries this check's replay key nor the IAM-auth refusal contract.
Traces: lambda__rds__secretsmanager.lambda-rds-proxy-missing-permission-denied.

## Evidence
Expected (AWS): the Lambda role granted rds-db:connect on arn:aws:rds-db:<region>:<account>:dbuser:<proxy-resource-id>/admin connects through the IAM-auth proxy endpoint and `SELECT 1` returns 1 -> {'ok': True, 'value': 1}; the Lambda role with no rds-db:connect grant is refused by the proxy with MySQL access-denied -> {'ok': False, 'mysql_error_code': 1045}, with no query executed.

Actual (emulator): the granted role's connection attempt fails at the transport layer -> {'ok': False, 'mysql_error_code': 2003} (pymysql "Can't connect to MySQL server"), raising
`AssertionError: role granted rds-db:connect on arn:aws:rds-db:us-west-2:<account>:dbuser:<proxy-resource-id>/admin must authenticate through the IAM-auth proxy and run a query; got {'ok': False, 'mysql_error_code': 2003}`
This happens after DescribeDBProxies reported the proxy `available` and the registered target `available`, i.e. the control plane claims a usable proxy while the published Endpoint accepts no connection. The negative (1045) assertion is never reached.

Corroborating, not graded: lambda__rds__secretsmanager.lambda-rds-connect-via-proxy-positive (password credentials through the same proxy path) kept returning FunctionError for 600 s and ended with `TimeoutError: lambda <fn> to query through the proxy: the run budget ended first; not graded`.

## Steps to Reproduce
1. CreateDBInstance (engine mysql, not publicly accessible) in the shared VPC and wait for `available`; store its admin credentials in a Secrets Manager secret.
2. CreateDBProxy with EngineFamily MYSQL, RequireTLS=true and an Auth entry using IAMAuth=REQUIRED pointing at that secret, in >=2 subnets of the VPC; RegisterDBProxyTargets with the instance; wait for the proxy and its target to report `available`.
3. Read DescribeDBProxies -> DBProxies[0].Endpoint and derive the dbuser ARN arn:aws:rds-db:<region>:<account>:dbuser:<proxy-resource-id>/admin from DBProxyArn.
4. Create two VPC-attached Lambda functions from the same package: one with a role granting rds-db:connect on that exact ARN, one with no such grant.
5. Invoke the granted function; its handler calls generate_db_auth_token for the proxy endpoint and connects with pymysql over TLS, then runs `SELECT 1`.
   Expected: {'ok': True, 'value': 1}. Observed: {'ok': False, 'mysql_error_code': 2003}.
6. Invoke the ungranted function and expect {'ok': False, 'mysql_error_code': 1045} — unreachable today, since step 5 already fails.

## Resolution

FIXED — a DB proxy now authenticates the CLIENT hop itself, per database user its `Auth`
names. "Whether to require or disallow … IAM authentication for connections to the proxy"
(`UserAuthConfigInfo$IAMAuth`) is read off the proxy record when a session on its endpoint is
admitted: where it is `REQUIRED` the proxy asks the client for an authentication token and no
password of the database behind it admits the connection; where it is `ENABLED` either
credential does; where it is `DISABLED` — or the user is not one the proxy logs in as — the
proxy decides nothing and the database's own credential stands, as before.

A token admits the connection only when every part holds: it was generated for an address this
proxy's endpoint answers at, for that database user, in the proxy's Region and inside its
window; the IAM identity that SIGNED it (the session the token's access key was minted for, or
the IAM user that owns it) is allowed `rds-db:connect` on
`arn:aws:rds-db:<region>:<account>:dbuser:<proxy id>/<database user>`, the proxy being named by
the id its own `DBProxyArn` carries; and the database behind the proxy holds a login for that
user. Anything else takes the engine's own access-denied refusal (MySQL 1045) and runs no
statement. The grant is read through IAM's own accessors and the principal through the token
service's own session registry, both on the shared engine, so a document
`iam:PutRolePolicy` wrote is in force on the next connection; a principal nothing readable
bounds (no IAM mounted, the account's own published credential, a role this account's IAM holds
no record of, an attachment whose document it does not hold) is admitted, the same deliberate
non-refusal this crate's control-plane gate takes.

Residual, unchanged: the behavioral check's own handler drives this with a vendored `pymysql`
and does not reach a verdict inside the check's 60 s budget. What the proxy's wire serves is
observable from the same vantage point without it — an in-VPC function that reads
`DBProxy$Endpoint`, negotiates TLS in band and speaks the MySQL protocol opens a session on the
published endpoint in well under a second
(`services/service-com-amazonaws-lambda-awsgirapiservice/tests/all/handler_rds_proxy_native_session.rs`)
— so what remains is the cost of that package in the guest runtime rather than anything the
proxy plane decides. The published endpoint still carries the port the core assigned the
proxy's listener, because `DBProxy$Endpoint` has no port member and a per-proxy address needs a
core listener that takes one; that is the core gap recorded as the residual of
`issues/things-that-now-work/20261109T135215Z-lambda-rds-secretsmanager-db-proxy-endpoints-are-published.md`,
and the token check accepts every address a client composes from that published value.
