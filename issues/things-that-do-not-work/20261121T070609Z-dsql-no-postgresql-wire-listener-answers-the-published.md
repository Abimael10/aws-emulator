Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: dsql.expired-auth-token-rejected-open-session-survives dsql.get-vpc-endpoint-service-name-valid
Issue-Shape-Id: com.amazonaws.dsql#DSQL
Issue-Key: dsql.expired-auth-token-rejected-open-session-survives|_connect_ready#4|AssertionError|raise AssertionError(f"no SQL connection within {_CONNECT_TIMEOUT_S}s: {exc}") from exc
Issue-Key: dsql.get-vpc-endpoint-service-name-valid|run#11|AssertionError|assert re.fullmatch(r"[A-Za-z0-9.-]+(:\d+)?", endpoint) and endpoint.startswith(f"{identifier}."), (
Issue-Commit: 9c4e35a7eb2cfc44174bf14b75d4647ba0e4b17d

# dsql: no PostgreSQL wire listener answers the published cluster endpoint, and GetVpcEndpointServiceName's clusterVpcEndpoint is a bare loopback literal instead of the cluster's VPC-connection host

> _Note: this issue was identified against the code base at commit `9c4e35a7eb2cfc44174bf14b75d4647ba0e4b17d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetVpcEndpointServiceName (cause 2); GetCluster/CreateCluster published endpoint + DSQL PostgreSQL data plane (cause 1)

## Description
Two net-new root causes in the dsql service, both surviving the replay report's ownership marks (neither failing check is marked ALREADY FILED or OWNED by).

Cause 1 of 2: a cluster that reports ACTIVE with a published `endpoint` has no PostgreSQL wire listener behind that address, so authentication-plane behavior (token lifetime vs. an established session) is never exercised. Failing check: dsql.expired-auth-token-rejected-open-session-survives.
  AWS: Smithy (services/service-com-amazonaws-dsql-dsql/smithy-model/com-amazonaws-dsql-dsql.json; query="operation" operation="GetCluster"): GetCluster GET /cluster/{identifier} -> 200 carries `status` (required, enum ClusterStatus) and `endpoint` (target Endpoint, pattern ^[a-zA-Z0-9.-]+$). The model constrains only the character set, so the contract is structural: once `status` is ACTIVE, the host named by `endpoint` accepts a TLS PostgreSQL connection for THAT cluster, authenticated with the password being a DSQL auth token (GenerateDbConnectAdminAuthToken for user `admin`). AWS documents the token as authenticating the connection only: a token presented after its ExpiresIn window is refused at connect with PostgreSQL `FATAL:  unable to accept connection, access denied` plus `HINT:  Signature expired ...` (fixed text; the variable part is the expiry instant), while a session opened before expiry keeps serving SQL (`SELECT current_user` -> `admin`) and a freshly minted token connects again. Reference: https://docs.aws.amazon.com/aurora-dsql/latest/userguide/SECTION_authentication-token.html; the per-connection refusal wording itself is recorded behavior only.
  Emulator: the control plane returns 200/ACTIVE with an endpoint the harness now accepts as an own host, but nothing listens for PostgreSQL on it. The check fails in _connect_ready after exhausting its 180 s budget: `AssertionError: no SQL connection within 180.0s: connection failed: connection to server at "127.0.0.1", port 5432 failed: Connection refused Is the server running on that host and accepting TCP/IP connections?`. The loopback address and port in that message are this environment's values, not the requirement; the invariant is that the host the control plane published refuses TCP at the PostgreSQL port.
  Why: a client that follows the documented flow (CreateCluster, poll GetCluster to ACTIVE, connect to `endpoint` with an auth token) can never open a session, so nothing that depends on the data plane works: no token-expiry enforcement, no established-session survival, no `SELECT`. Every refusal and every success the auth contract specifies is unreachable, and ACTIVE is reported for a cluster that serves no connections.
  Fix: the dsql service's data-plane listener — the component that must bind a PostgreSQL wire endpoint for a cluster when the cluster's state becomes ACTIVE, and the cluster state/endpoint derivation that publishes the address it binds, so the announced address and the served address cannot diverge. The auth path behind it must validate the token's expiry at connect only, leaving established sessions untouched.
  Related open issues that do NOT carry this check: 20261023T121409Z-dsql-clusters-report-active-with-a-published-endpoint.md, 20261107T211056Z-dsql-the-published-cluster-endpoint-accepts-no-postgresql.md and 20261022T193920Z-dsql-publishes-a-cluster-endpoint-but-serves-no.md describe the same missing listener for the native-SQL checks; the replay report marks the other nine data-plane checks OWNED by the first of those, but not this one. 20261109T235613Z-dsql-publishes-a-cluster-endpoint-on-a-host.md lists this check id, but for the previous, now-different failure (PublishedUrlRejected on a foreign host); the endpoint host is no longer refused by the harness, the connection is refused by the absent listener.
  Traces: dsql.expired-auth-token-rejected-open-session-survives.

Cause 2 of 2: GetVpcEndpointServiceName answers 200 with clusterVpcEndpoint set to a bare address literal that is not derived from the cluster at all. Failing check: dsql.get-vpc-endpoint-service-name-valid.
  AWS: Smithy (same model; query="operation" operation="GetVpcEndpointServiceName", query="doc" shape="GetVpcEndpointServiceNameOutput" member="clusterVpcEndpoint"): GET /clusters/{identifier}/vpc-endpoint-service-name -> 200 with required `serviceName` (ServiceName, @length 1..128, @pattern ^com\.amazonaws\.[a-z0-9-]+\.dsql[a-z0-9-]*$) and `clusterVpcEndpoint` (ClusterVpcEndpoint, @pattern ^[a-zA-Z0-9.-]+$), documented as "The VPC connection endpoint for the cluster." The model constrains the character set only, so the layout is recorded behavior only: the value is a DNS host whose leading label is the 26-character cluster identifier from the request, followed by the dsql label carrying the same per-cluster endpoint-service suffix that appears after `dsql` in the `serviceName` of the SAME reply, then the region and the service domain — <clusterId>.dsql-<suffix>.<region>.on.aws. The requirement is that form and that cross-member consistency; any one recording's cluster id, suffix and region fill the template and are not part of it. No error is involved: this is a member value in a 200 body.
  Emulator: returns 200 with clusterVpcEndpoint = '127.0.0.1' — an address literal with no cluster-identifier label, no dsql label and no endpoint-service suffix, bearing no relation to the `identifier` in the request. The check fails with `AssertionError: expected clusterVpcEndpoint to be a well-formed host named for identifier '01m7yek9zbe2jmtw4qdmatsaj5', got '127.0.0.1'` (the identifier shown is this run's minted id; the invariant is that the returned value does not start with the requested identifier and is not a per-cluster host).
  Why: a client that uses this field to create or validate an interface VPC endpoint, or that pairs clusterVpcEndpoint with serviceName from the same reply, receives a value that names no cluster and no endpoint service; code that parses the host to recover the cluster id or the endpoint-service suffix finds neither, and the same literal is returned for every cluster, so two clusters are indistinguishable by their VPC connection endpoint.
  Fix: GetVpcEndpointServiceName's handler/serializer in the dsql service crate together with the cluster state that should mint and store the per-cluster endpoint-service suffix, so one stored suffix builds both `serviceName` and the `clusterVpcEndpoint` host, the host's leading label is the cluster's identifier, and the host is one the emulator actually answers on.
  Related open issue that does NOT cover this: 20261117T165811Z-dsql-getvpcendpointservicename-returns-a-clustervpcendpoint-host-without-the.md files the same member for the WIRE corpus when the emulator returned '<clusterId>.dsql.<region>.on.aws' (identifier present, suffix missing); the value has since changed to a bare loopback literal with no cluster identifier at all, and that issue carries no behavioral check id. 20261029T193445Z-dsql-success-replies-diverge-tagging-answers-200-with.md covers this operation's $.serviceName only.
  Traces: dsql.get-vpc-endpoint-service-name-valid.

## Evidence
Behavioral suite simparity-behavioral-aws-v1: 37/51 passed; service heading `### dsql` 37/49 with 12 failures, of which 10 are marked OWNED/ALREADY FILED. The two surviving failures are clustered here.

Cause 1 — [mismatch] dsql.expired-auth-token-rejected-open-session-survives:
  File "check_dsql_expired_auth_token_rejected_open_session_survives.py", line 32, in run
    _connect_ready(ctx, host, dsql.generate_db_connect_admin_auth_token(
  File "check_dsql_expired_auth_token_rejected_open_session_survives.py", line 129, in _connect_ready
    raise AssertionError(f"no SQL connection within {_CONNECT_TIMEOUT_S}s: {exc}") from exc
  AssertionError: no SQL connection within 180.0s: connection failed: connection to server at "127.0.0.1", port 5432 failed: Connection refused
  Is the server running on that host and accepting TCP/IP connections?
  Expected (AWS): the first connection to the ACTIVE cluster's published endpoint succeeds with an admin auth token; a token 15 s past its 20 s lifetime is then refused at connect with FATAL "unable to accept connection, access denied" and HINT "Signature expired" (fixed sentences; the expiry instant is the variable part), the session opened before expiry still answers SELECT current_user -> ('admin',), and a freshly generated token opens a working session (SELECT 1 -> (1,)).
  Actual (emulator): no TCP connection is ever accepted at the published endpoint's PostgreSQL port, so not one of those assertions is reached. No HTTP status or error code is involved on the control plane: GetCluster answered 200 with status ACTIVE and a non-empty endpoint (the check's _wait_active assertions passed). The only fixed part of the failure is "Connection refused" at the published host's port 5432; the host 127.0.0.1 and the 180 s budget are environment values.

Cause 2 — [mismatch] dsql.get-vpc-endpoint-service-name-valid:
  File "check_dsql_get_vpc_endpoint_service_name_valid.py", line 33, in run
    assert re.fullmatch(r"[A-Za-z0-9.-]+(:\d+)?", endpoint) and endpoint.startswith(f"{identifier}.")
  AssertionError: expected clusterVpcEndpoint to be a well-formed host named for identifier '01m7yek9zbe2jmtw4qdmatsaj5', got '127.0.0.1'
  Expected (AWS): 200 with $.clusterVpcEndpoint = '<clusterId>.dsql-<per-cluster endpoint-service suffix>.<region>.on.aws'; the fixed text is the '.dsql-' label prefix and the '.on.aws' tail, while the cluster id (the identifier sent in the request), the suffix (the same one appearing after `dsql` in this reply's $.serviceName) and the region come from the request and the emulator's stored cluster.
  Actual (emulator): 200 with $.clusterVpcEndpoint = '127.0.0.1' — no identifier label, no dsql label, no suffix, no region; identical for every cluster. The check's earlier assertions on $.serviceName (pattern ^com\.amazonaws\.[a-z0-9-]+\.dsql[a-z0-9-]*$ and region membership) passed, so only clusterVpcEndpoint diverges.

Smithy consulted (services/service-com-amazonaws-dsql-dsql/smithy-model/com-amazonaws-dsql-dsql.json): GetCluster and GetVpcEndpointServiceName operation signatures and the documentation of GetVpcEndpointServiceNameOutput$clusterVpcEndpoint.

## Steps to Reproduce
Cause 1 (dsql.expired-auth-token-rejected-open-session-survives):
1. Point a DSQL client at the emulator and call CreateCluster(deletionProtectionEnabled=false, tags={...}); note the returned identifier.
2. Poll GetCluster(identifier=...) until status == ACTIVE and read `endpoint` (non-empty, as the emulator reports).
3. Generate an admin auth token for that hostname (ExpiresIn=900) and open a TLS PostgreSQL connection (sslmode=require, dbname=postgres, user=admin, password=token) to the published endpoint on port 5432.
4. Observe the connection is refused — "Connection refused" at the published host's PostgreSQL port — and keeps being refused for the full 180 s retry budget. On AWS this first connection succeeds.
5. The remaining contract is therefore never reached: a token 15 s past a 20 s ExpiresIn must be refused at connect with FATAL "unable to accept connection, access denied" / HINT "Signature expired", the session opened before expiry must still answer SELECT current_user -> admin, and a fresh token must open a working session.

Cause 2 (dsql.get-vpc-endpoint-service-name-valid):
1. Call CreateCluster(deletionProtectionEnabled=false) and note the 26-character identifier.
2. Call GetVpcEndpointServiceName(identifier=<that id>) (GET /clusters/{identifier}/vpc-endpoint-service-name).
3. Read the 200 reply: $.serviceName matches com.amazonaws.<region>.dsql<suffix>, but $.clusterVpcEndpoint is '127.0.0.1' instead of '<identifier>.dsql-<same suffix>.<region>.on.aws' — it does not begin with the requested identifier and carries no endpoint-service suffix.
4. Repeat with a second cluster: the same literal is returned, so the value does not identify the cluster at all.
