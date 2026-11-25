Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: lambda__rds__secretsmanager.lambda-rds-connect-via-proxy-positive
Issue-Key: lambda__rds__secretsmanager.lambda-rds-connect-via-proxy-positive|_invoke#24|TimeoutError|raise TimeoutError(f"lambda {name} to query through the proxy: the run budget ended first; not graded")
Issue-Commit: 7f5dd6cdee8fc233932a8e3b3b251e9cc2539379

# lambda__rds__secretsmanager: a Lambda in the proxy's VPC never completes a query through the published RDS Proxy endpoint, so the proxy data-plane positive never reaches a verdict

> _Note: this issue was identified against the code base at commit `7f5dd6cdee8fc233932a8e3b3b251e9cc2539379`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
Invoke

## Description
One net-new cause, found by replaying the behavioral suite against the emulator built from this checkout.

Cause 1 of 1: a Lambda invoked inside the RDS Proxy's VPC cannot complete a query through the proxy's published endpoint.
AWS: DescribeDBProxies publishes the proxy's Endpoint, and a client inside the proxy's VPC connects to that host on the proxy's port with the Secrets-Manager-backed credentials the proxy is configured with and runs statements against the target DB instance — here 'SELECT 1, VERSION(), CURRENT_USER()' over the MySQL wire protocol. The Lambda invocation therefore returns its handler's result (ok/value/version/current_user) well within its timeout. Recorded behavior only for the handler's own payload shape; the requirement is that the published proxy endpoint accepts the connection and serves the query.
Emulator: the Lambda never returns a usable result — the replay keeps re-invoking it until the run budget is exhausted ('lambda <function> to query through the proxy: the run budget ended first; not graded'). Nothing about the proxy endpoint ever becomes serviceable within the run, so the data-plane positive is never gradeable.
Why: the RDS Proxy is the connection path a serverless application is told to use — Lambda is the canonical client. If a Lambda in the proxy's VPC cannot open a session on the endpoint the emulator itself published, every proxy-backed application is unusable, and the proxy's pinning/credential behaviour cannot be exercised at all.
Fix: the RDS Proxy data-plane listener that must serve the endpoint DescribeDBProxies publishes (and the Lambda sandbox's reachability of it), so a connection from a function in the proxy's VPC is accepted with the proxy's Secrets-Manager credentials and the statement is executed against the target. NOTE: open issue 20261117T042331Z-lambda-rds-secretsmanager-an-iam-auth-rds-proxy.md reports an IAM-auth RDS Proxy accepting no connection at its published endpoint (MySQL 2003); this check uses secret-based credentials, not IAM auth, and the replay does not report it as owned by that issue — the two are very likely the same listener and should be closed together.
Traces: lambda__rds__secretsmanager.lambda-rds-connect-via-proxy-positive.

Caveat for the fixer: the replay's own wording for this check is 'the run budget ended first; not graded', so the evidence is non-convergence rather than a captured wrong response. It is filed because the thing that failed to converge is the emulator's proxy endpoint, not the runner.

## Evidence
Behavioral suite replayed against the emulator built from this checkout: lambda__rds__secretsmanager scored 57/70 (81%) with 13 failures; 12 of them the replay reported as OWNED by open issues, and this one as neither owned nor blocked by an unimplemented stub.

lambda__rds__secretsmanager.lambda-rds-connect-via-proxy-positive:
  File "check_lambda__rds__secretsmanager_lambda_rds_connect_via_proxy_positive.py", line 88, in run -> _invoke(...)
  line 313, in _invoke: raise TimeoutError(f"lambda {name} to query through the proxy: the run budget ended first; not graded")
  TimeoutError: lambda <generated function name> to query through the proxy: the run budget ended first; not graded

The function's handler is:
  conn = pymysql.connect(host=event["proxy_endpoint"], port=int(event["port"]), user=event["user"], password=event["password"], connect_timeout=15)
  cur.execute("SELECT 1, VERSION(), CURRENT_USER()")
  return {"ok": True, "value": row[0], "version": row[1], "current_user": row[2]}
Expected: that payload returned from the invocation (ok true, value 1, a MySQL version string, and the proxy's configured user). Observed: no successful invocation before the run budget ended. The generated function name and the host/port are per-run values and are not requirements.

## Steps to Reproduce
1. Build and boot the emulator from this checkout.
2. Replay the behavioral suite (simparity-behavioral-aws-v1).
3. Run lambda__rds__secretsmanager.lambda-rds-connect-via-proxy-positive: create a MySQL DB instance, a Secrets Manager secret with its credentials, an RDS Proxy targeting it in the same VPC, and a Lambda in that VPC whose handler connects to DescribeDBProxies' published Endpoint on the proxy port with the secret's credentials and runs 'SELECT 1, VERSION(), CURRENT_USER()'.
4. Invoke the function with {proxy_endpoint, port, user, password}. It never returns the handler's {ok, value, version, current_user} payload; the check re-invokes until the run budget is exhausted.

## Resolution

FIXED — `DBProxy$Endpoint` carries no port member, so a client completes the host it publishes
with the port of the databases the proxy's `EngineFamily` names (3306 for `MYSQL`, 5432 for
`POSTGRESQL`, the documented defaults of those engines). The proxy's endpoint now answers that
port as well as the port the core assigned its listener — one protocol handler at both
addresses, over the same session context — so a function in the proxy's VPC that reads the
published endpoint and connects on its engine family's own port opens a session, is admitted
with the credential the proxy's `Auth` secret holds, and runs its statements against the
database the proxy's target group holds.

Residual, unchanged: the family's port is one port of the ONE interface the core binds its
listeners on, so it names no proxy in particular — the proxy that published its endpoint last
answers there, and each proxy's own published `host:port` keeps reaching that proxy. An address
per proxy needs a core listener that accepts an address or a port, which
`vendor/server/src/listeners.rs` does not offer; that is the same core gap already recorded as
the residual of
`issues/things-that-now-work/20261109T135215Z-lambda-rds-secretsmanager-db-proxy-endpoints-are-published.md`.
