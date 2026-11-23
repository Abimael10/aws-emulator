Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: dsql.expired-auth-token-rejected-open-session-survives
Issue-Shape-Id: com.amazonaws.dsql#DSQL
Issue-Key: dsql.expired-auth-token-rejected-open-session-survives|_connect_ready#4|AssertionError|raise AssertionError(f"no SQL connection within {_CONNECT_TIMEOUT_S}s: {exc}") from exc
Issue-Commit: 9c4e35a7eb2cfc44174bf14b75d4647ba0e4b17d

# dsql: the cluster's PostgreSQL data plane is served, but not on the port a client dials it at, so the auth-token check never reaches a session

> _Split out of `issues/things-that-now-work/20261121T070609Z-dsql-no-postgresql-wire-listener-answers-the-published.md`, whose cause 2 (the `clusterVpcEndpoint` host) is fixed. The cause below is NOT fixed; it keeps the original report's trace id and Issue-Key so it stays filed and replayable. It was identified against the code base at commit `9c4e35a7eb2cfc44174bf14b75d4647ba0e4b17d`, so trace it back to the current code base._

## Operation
GetCluster/CreateCluster published endpoint + the DSQL PostgreSQL data plane

## Description
Cause 1 of the split report: a cluster that reports `ACTIVE` with a published `endpoint` now HAS
a PostgreSQL wire endpoint of its own, and the recorded check still cannot open a session on it,
because the address it dials and the address the emulator serves differ in the PORT.

AWS: once `GetCluster` reports `status` `ACTIVE`, the host named by `endpoint` accepts a TLS
PostgreSQL connection for THAT cluster on TCP 5432, with the password being a DSQL auth token
(`GenerateDbConnectAdminAuthToken` for the `admin` role, a client-side presigner). The token
authenticates the CONNECTION: one presented after its `ExpiresIn` window is refused at connect
with `FATAL:  unable to accept connection, access denied` plus `HINT:  Signature expired …`
(fixed text; the expiry instant is the variable part), a session opened before expiry keeps
serving SQL (`SELECT current_user` -> `admin`), and a freshly minted token connects again.

What this emulator now serves. `CreateCluster` opens the cluster's data plane
(`services/service-com-amazonaws-dsql-dsql/src/state/cluster_data_plane.rs`, over
`services/common/src/sql_wire_planes`): a core-owned listener on the emulator's own loopback
interface, the PostgreSQL frontend/backend protocol with the in-band `SSLRequest` upgrade run
by the core's own TLS upgrader, one in-process SQL engine per cluster, and a login admitted
against the STORED cluster plus the auth token itself
(`src/state/connection_auth.rs`: the action-to-role binding, the host the token was presigned
against and its lifetime, judged at connect only — an open session is never re-checked). The
plane is held beside the cluster record and dropped when the clock-settled deletion retires the
cluster, which closes the port. The whole contract above is exercised in process by
`services/service-com-amazonaws-dsql-dsql/tests/all/cluster_data_plane_session.rs`: a session
in the clear and one over the in-band TLS upgrade, `SELECT current_user` -> `admin`, a
`CREATE`/`INSERT`/`SELECT` round-trip on the cluster's own storage, the expired token refused
`FATAL` / `28000` with the `Signature expired:` hint while the session opened earlier keeps
serving and a fresh token reopens, a cluster still `CREATING` refused `57P03`, and the port
closed once the deletion has settled.

Emulator, as the check sees it: the check resolves the published endpoint and dials
PostgreSQL's port — `db_host, port = ctx.endpoint_host(host, 5432)` — and the listener the core
opened for the cluster is on a port the CORE assigned, so the connect is refused for the whole
180 s budget: `AssertionError: no SQL connection within 180.0s: connection failed: connection to
server at "127.0.0.1", port 5432 failed: Connection refused`. The loopback address in that
message is this environment's value; the invariant is that the port the client dials is not the
port the cluster's wire answers on.

Blocked by (as of the commit above): the core assigns the port of every listener a service
opens. `vendor/server/src/listeners.rs` offers `Listeners::tcp` (line 204),
`Listeners::tcp_plaintext` (line 230) and `Listeners::http` (line 381); none takes a requested
port, and the binds behind them (`bind_std`/`bind_tokio`, lines 67-107) use port 0 or walk the
operator's `EMULATOR_LISTENER_PORT_RANGE`. A service crate may not bind a socket of its own
(`vendor/engine/src/lib.rs`: the core owns the socket, TLS and the bind interface), and
`vendor/` is read-only from a service fix. Publishing the served port inside the member is not
open either: `Endpoint`'s pattern is `^[a-zA-Z0-9.-]+$`
(`smithy_query(query="shape", shape="Endpoint")`), which admits no `:port`, so the member can
carry only the host — and two clusters of one process could not both hold 5432 in any case,
where AWS gives each cluster its own DNS name on the one engine port.

Required core change: a `tcp_plaintext`-style opener that takes a REQUESTED port (and, so two
clusters can each answer the engine's port, an optional loopback address), reporting whether it
was granted so the service can answer its own "port in use" case. `open_cluster_data_plane`
would then ask for 5432 and the published host would be reachable exactly as the recorded client
expects. The same gap is already filed for Amazon RDS in
`issues/things-that-do-not-work/20261105T090000Z-rds-a-db-instance-publishes-the-listeners-port-not-the-requested-port.md`
and `issues/things-that-do-not-work/20261106T031005Z-rds-7-net-new-causes-endpoint-port-publishes.md`,
and for a DNS listener in
`issues/things-that-do-not-work/20261122T001500Z-route53-a-hosted-zone-s-delegated-nameservers-resolve-nowhere.md`.

Related open issues on the same DSQL data plane, which this report does not replace:
`20261023T121409Z-dsql-clusters-report-active-with-a-published-endpoint.md`,
`20261107T211056Z-dsql-the-published-cluster-endpoint-accepts-no-postgresql.md`,
`20261022T193920Z-dsql-publishes-a-cluster-endpoint-but-serves-no.md` and
`20261122T035959Z-dsql-the-published-cluster-endpoint-accepts-no-postgresql-67a5be.md` carry the
other data-plane checks (TLS mandatory, the DbConnect-for-admin mapping, REPEATABLE READ,
optimistic-concurrency 40001, the refusals of sequences and TEMP tables); every one of them is
behind this same port gap at connect, and the semantics each asks for are theirs to add behind
the wire this report's plane already serves.

Traces: dsql.expired-auth-token-rejected-open-session-survives.

## Evidence
  [mismatch] dsql.expired-auth-token-rejected-open-session-survives
  File "check_dsql_expired_auth_token_rejected_open_session_survives.py", line 32, in run
    _connect_ready(ctx, host, dsql.generate_db_connect_admin_auth_token(
  File "check_dsql_expired_auth_token_rejected_open_session_survives.py", line 129, in _connect_ready
    raise AssertionError(f"no SQL connection within {_CONNECT_TIMEOUT_S}s: {exc}") from exc
  AssertionError: no SQL connection within 180.0s: connection failed: connection to server at "127.0.0.1", port 5432 failed: Connection refused
  Is the server running on that host and accepting TCP/IP connections?

Expected (AWS): the first connection to the ACTIVE cluster's published endpoint succeeds with an
admin auth token; a token 15 s past its 20 s lifetime is then refused at connect with FATAL
"unable to accept connection, access denied" and HINT "Signature expired", the session opened
before expiry still answers SELECT current_user -> ('admin',), and a freshly generated token
opens a working session (SELECT 1 -> (1,)).

Actual (emulator): the control plane answers 200/ACTIVE with a non-empty `endpoint` (the check's
`_wait_active` assertions pass) and the cluster's PostgreSQL wire is open on the port the core
assigned it, which is not the port the check dials; nothing is accepted on 5432, so none of the
assertions above is reached. No HTTP status or error code is involved.

## Steps to Reproduce
1. CreateCluster(deletionProtectionEnabled=false); poll GetCluster until `status` == ACTIVE and
   read `endpoint`.
2. Generate an admin auth token for that host and connect with libpq
   (sslmode=require, dbname=postgres, user=admin, password=<token>) to `endpoint` on port 5432.
3. Observe "Connection refused": the cluster's wire is bound on the port the core assigned, not
   on 5432.
4. Connect instead to the port that cluster's listener reports (the emulator's own store holds
   it, and `tests/all/cluster_data_plane_session.rs` dials exactly that) and the whole contract
   of step 5 holds, which is what shows the gap is the port and nothing else.
5. The contract behind it: a token 15 s past a 20 s ExpiresIn refused at connect with FATAL
   "unable to accept connection, access denied" / HINT "Signature expired", the session opened
   before expiry still answering SELECT current_user -> admin, and a fresh token opening a
   working session.
