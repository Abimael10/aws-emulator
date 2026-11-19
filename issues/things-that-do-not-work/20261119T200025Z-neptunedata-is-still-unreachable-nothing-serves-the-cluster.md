Issue-Severity: CRITICAL
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: neptunedata.create-ml-endpoint-nonexistent-training-job-rejected neptunedata.engine-status-reports-query-language-endpoints neptunedata.execute-fast-reset-perform-without-token-rejected neptunedata.get-propertygraph-statistics-after-delete-raises-not-available neptunedata.gremlin-addv-and-traverse-roundtrip neptunedata.gremlin-explain-query-returns-plan neptunedata.gremlin-malformed-query-rejected neptunedata.gremlin-profile-query-returns-timing-metrics neptunedata.gremlin-query-malformed-queryid-rejected-by-status-and-cancel neptunedata.loader-job-unknown-loadid-rejected-by-get-and-cancel neptunedata.manage-propertygraph-statistics-autocompute-toggle-persists neptunedata.opencypher-concurrent-writes-all-committed neptunedata.opencypher-create-and-match-roundtrip neptunedata.opencypher-explain-query-returns-plan neptunedata.opencypher-malformed-query-rejected neptunedata.opencypher-parameterized-query-binds-values neptunedata.opencypher-query-malformed-queryid-rejected-by-status-and-cancel neptunedata.propertygraph-summary-reflects-added-vertices neptunedata.start-loader-job-invalid-iam-role-arn-rejected
Issue-Shape-Id: com.amazonaws.neptunedata#AmazonNeptuneDataplane
Issue-Key: neptunedata.create-ml-endpoint-nonexistent-training-job-rejected|<lambda>#6|EndpointConnectionError|lambda: client.create_ml_endpoint(
Issue-Key: neptunedata.engine-status-reports-query-language-endpoints|run#4|EndpointConnectionError|resp = client.get_engine_status()
Issue-Key: neptunedata.execute-fast-reset-perform-without-token-rejected|run#4|EndpointConnectionError|initiate_resp = client.execute_fast_reset(action="initiateDatabaseReset")
Issue-Key: neptunedata.get-propertygraph-statistics-after-delete-raises-not-available|run#6|EndpointConnectionError|client.manage_propertygraph_statistics(mode="refresh")
Issue-Key: neptunedata.gremlin-addv-and-traverse-roundtrip|run#8|EndpointConnectionError|add_resp = client.execute_gremlin_query(
Issue-Key: neptunedata.gremlin-explain-query-returns-plan|run#4|EndpointConnectionError|resp = client.execute_gremlin_explain_query(gremlinQuery="g.V().limit(1).count()")
Issue-Key: neptunedata.gremlin-malformed-query-rejected|<lambda>#8|EndpointConnectionError|code, message = _error(lambda: client.execute_gremlin_query(gremlinQuery=bogus_query))
Issue-Key: neptunedata.gremlin-profile-query-returns-timing-metrics|run#7|EndpointConnectionError|client.execute_gremlin_query(gremlinQuery=f"g.addV('{label}')")
Issue-Key: neptunedata.gremlin-query-malformed-queryid-rejected-by-status-and-cancel|<lambda>#6|EndpointConnectionError|lambda: client.get_gremlin_query_status(queryId=bogus_query_id)
Issue-Key: neptunedata.loader-job-unknown-loadid-rejected-by-get-and-cancel|run#26|EndpointConnectionError|before_ids = set(client.list_loader_jobs()["payload"]["loadIds"])
Issue-Key: neptunedata.manage-propertygraph-statistics-autocompute-toggle-persists|run#5|EndpointConnectionError|client.manage_propertygraph_statistics(mode="enableAutoCompute")
Issue-Key: neptunedata.opencypher-concurrent-writes-all-committed|_create#10|EndpointConnectionError|client.execute_open_cypher_query(
Issue-Key: neptunedata.opencypher-create-and-match-roundtrip|run#8|EndpointConnectionError|before = client.execute_open_cypher_query(
Issue-Key: neptunedata.opencypher-explain-query-returns-plan|run#4|EndpointConnectionError|static_resp = client.execute_open_cypher_explain_query(
Issue-Key: neptunedata.opencypher-malformed-query-rejected|<lambda>#8|EndpointConnectionError|lambda: client.execute_open_cypher_query(openCypherQuery=bogus_query)
Issue-Key: neptunedata.opencypher-parameterized-query-binds-values|run#6|EndpointConnectionError|resp = client.execute_open_cypher_query(
Issue-Key: neptunedata.opencypher-query-malformed-queryid-rejected-by-status-and-cancel|<lambda>#6|EndpointConnectionError|lambda: client.get_open_cypher_query_status(queryId=bogus_query_id)
Issue-Key: neptunedata.propertygraph-summary-reflects-added-vertices|run#7|EndpointConnectionError|client.execute_gremlin_query(gremlinQuery=f"g.addV('{label}')")
Issue-Key: neptunedata.start-loader-job-invalid-iam-role-arn-rejected|run#4|EndpointConnectionError|before_ids = set(client.list_loader_jobs()["payload"]["loadIds"])
Issue-Commit: 0d5fdb6d7a78988c971aec85849026eed9fa5958

# neptunedata is still unreachable: nothing serves the cluster endpoint the Neptune control plane publishes, so every data-plane call is refused at connect (https://<Endpoint>:8182 — connection refused)

> _Note: this issue was identified against the code base at commit `0d5fdb6d7a78988c971aec85849026eed9fa5958`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetEngineStatus

## Description
THIS ISSUE HAS 1 CAUSE — it must be closed before submitting.

Cause 1 of 1: the address the Neptune control plane publishes for a DB cluster (DBCluster.Endpoint + DBCluster.Port) is not served by the emulator at the TRANSPORT level: a client that connects to https://<Endpoint>:<Port> gets a connection failure, not an HTTP response. Every one of the 43 modelled neptunedata operations is therefore unreachable and all 19 neptunedata behavioral checks die before a single neptunedata request is answered. Failing check ids for this cause: all 19 ids in trace_ids (representatives: neptunedata.engine-status-reports-query-language-endpoints, neptunedata.opencypher-create-and-match-roundtrip, neptunedata.gremlin-addv-and-traverse-roundtrip, neptunedata.loader-job-unknown-loadid-rejected-by-get-and-cancel).

AWS: neptunedata has no regional, account-wide front door a client can be pointed at generically — every operation is addressed at the owning cluster's own connection endpoint on the Neptune engine port. Smithy (services/service-com-amazonaws-neptunedata-amazonneptunedataplane/smithy-model/com-amazonaws-neptunedata-amazonneptunedataplane.json): service AmazonNeptuneDataplane, version 2023-08-01, sdkId neptunedata, protocol restJson1, sigv4Name/arnNamespace neptune-db, 43 bound operations; its endpoint rule set resolves only host names (https://neptune-db.{Region}.{PartitionResult#dnsSuffix} plus fips/dualstack variants) and never a host:port pair, so the address must come from the cluster record. Smithy (services/service-com-amazonaws-neptune-amazonrdsv19): com.amazonaws.neptune#DBCluster member Endpoint is 'Specifies the connection endpoint for the primary instance of the DB cluster' (a bare host name, no scheme, no port) and the sibling member Port is 'Specifies the port that the database engine is listening on', with CreateDBClusterMessage.Port documenting 'Default: 8182' for this engine. The general requirement that follows: whatever host/port pair the control plane publishes, https://<Endpoint>:<Port> MUST be an address that answers this crate's restJson1 neptunedata API for that cluster. For an emulator that means the published pair has to be an address the emulator is actually listening on — either a per-cluster listener it binds on the published port, or a published port equal to a port it serves.

Emulator: the host half is now accepted (the harness resolves the published Endpoint onto the emulator's own loopback host, so client construction succeeds — that was the previous state of this surface, see below), and Port is published as 8182. But nothing is bound on that port: every request fails at connect with botocore.exceptions.EndpointConnectionError: Could not connect to the endpoint URL: "https://127.0.0.1:8182/<path>", raised from botocore httpsession.py send(). The emulator's own HTTP listener is on its run-assigned ephemeral port, and no data-plane listener exists on the published 8182, so the two never meet. The refused paths cover the whole surface, one per failing check: /status, /gremlin, /gremlin/explain, /gremlin/status/<queryId>, /opencypher, /opencypher/explain, /opencypher/status/<queryId>, /propertygraph/statistics, /propertygraph/summary (via /gremlin setup), /loader, /ml/endpoints, /system. No status code, no error code and no body is produced for any of them, because no HTTP exchange takes place. The loopback host, the emulator's run port and the generated cluster identifiers vary per run and are not part of the requirement; the requirement is that the published pair be served.

This is the NEXT state of the reachability surface already reported by open issue 20261109T113928Z-neptunedata-is-still-entirely-unreachable-the-neptune-db.md, and the replay reports none of these 19 checks as owned by it or as ALREADY FILED, because the failure signature has changed: there the published Endpoint was a fabricated *.neptune.amazonaws.com name and the harness refused the URL at CLIENT CONSTRUCTION with simparity.published.PublishedUrlRejected ('its host is not one of the emulator's own hosts'); here the URL is accepted and a real TCP connection is attempted to 127.0.0.1:8182 and refused. The host half of that issue is addressed; the remaining half — a listener bound to the published port that dispatches to the owning cluster's graph state — is what this issue reports. Also related but NOT the same defect: open issue 20261116T010226Z-neptune-dbcluster-projection-publishes-the-emulator-s-listener.md argues the published Port must be the engine constant 8182 rather than the emulator's listener port (that is the value now published, and it is correct); it says nothing about anything serving 8182. Open issues 20261115T144528Z-neptunedata-refusal-paths-diverge-ml-opencypher-status-cancel.md, 20261116T193419Z-neptunedata-modelled-error-bodies-carry-the-numeric-http.md, 20261109T174758Z-neptunedata-data-plane-responses-diverge-from-neptune-engine.md and 20261113T062625Z-neptunedata-executegremlinexplainquery-explain-report-diverges-from-neptune-s.md describe divergences in neptunedata RESPONSES, which presupposes a reachable data plane; none of them covers the fact that the published endpoint answers nothing.

Why: DBCluster.Endpoint/Port is the ONLY documented way a client finds a Neptune cluster. A client doing exactly the documented thing — create the cluster and instance, read Endpoint and Port, connect to https://Endpoint:Port — is sent to an address that refuses the connection, and it sees a transport error rather than any AWS error it can classify or retry on. Every Gremlin and openCypher query, every explain/profile, every query status/cancel, every bulk-loader job, every statistics/summary call, every ML endpoint call and fast reset is unusable, and any client that hardcodes a workaround port against the emulator will not work against AWS. Because nothing is exercised, the whole 43-operation neptunedata surface remains ungradable (0/19) and no fidelity statement about neptunedata can be made until the published endpoint answers.

Fix: the serving/listener layer that binds the neptunedata restJson1 front door, together with the Neptune control plane's DBCluster endpoint projection and the cluster state that records it — the pair must be made consistent, by binding a per-cluster data-plane listener on the published host and port (8182) that routes each request to the owning cluster's graph state, or by publishing a port the emulator already serves while keeping Endpoint port-less; the published Endpoint:Port and the socket the emulator accepts on must be the same address, and a second cluster's endpoint must not see the first cluster's data.

Traces: the 19 failing checks in trace_ids, each ending in EndpointConnectionError against https://<published host>:8182/<operation path>.

## Evidence
Replay of simparity-behavioral-aws-v1 against the emulator built from this checkout: Total 0/19 passed (0.0%); neptunedata 0/19 (0%), 19 failed — Mismatched 0, Errored 19, Skipped 0, Unloadable 0. No failing check was marked ALREADY FILED or OWNED by an open issue.

All 19 failures carry the identical tail:

  File "endpoint.py", line 385, in _send
    return self.http_session.send(request)
  File "httpsession.py", line 540, in send
    raise EndpointConnectionError(endpoint_url=request.url, error=e)
  botocore.exceptions.EndpointConnectionError: Could not connect to the endpoint URL: "https://127.0.0.1:8182/<path>"

Fixed part of that message: "Could not connect to the endpoint URL" — i.e. no HTTP exchange occurred at all, so there is no status, no error code and no body to compare. Variable parts, not requirements: the loopback host the published Endpoint resolved to and the per-run cluster identifiers. The <path> is the operation's modelled HTTP binding, and the refused paths show the whole surface is affected:
  - neptunedata.engine-status-reports-query-language-endpoints -> https://127.0.0.1:8182/status (GetEngineStatus)
  - neptunedata.execute-fast-reset-perform-without-token-rejected -> .../system (ExecuteFastReset)
  - neptunedata.gremlin-addv-and-traverse-roundtrip, .gremlin-malformed-query-rejected, .gremlin-profile-query-returns-timing-metrics, .propertygraph-summary-reflects-added-vertices -> .../gremlin (ExecuteGremlinQuery / ExecuteGremlinProfileQuery)
  - neptunedata.gremlin-explain-query-returns-plan -> .../gremlin/explain
  - neptunedata.gremlin-query-malformed-queryid-rejected-by-status-and-cancel -> .../gremlin/status/totally-invalid-query-id-not-a-uuid
  - neptunedata.opencypher-create-and-match-roundtrip, .opencypher-concurrent-writes-all-committed, .opencypher-malformed-query-rejected, .opencypher-parameterized-query-binds-values -> .../opencypher
  - neptunedata.opencypher-explain-query-returns-plan -> .../opencypher/explain
  - neptunedata.opencypher-query-malformed-queryid-rejected-by-status-and-cancel -> .../opencypher/status/totally-invalid-query-id-not-a-uuid
  - neptunedata.get-propertygraph-statistics-after-delete-raises-not-available, .manage-propertygraph-statistics-autocompute-toggle-persists -> .../propertygraph/statistics
  - neptunedata.loader-job-unknown-loadid-rejected-by-get-and-cancel, .start-loader-job-invalid-iam-role-arn-rejected -> .../loader
  - neptunedata.create-ml-endpoint-nonexistent-training-job-rejected -> .../ml/endpoints

Every check reaches that point only because the Neptune control plane (CreateDBSubnetGroup, CreateDBCluster, CreateDBInstance, DescribeDBInstances) and EC2 (CreateSecurityGroup, AuthorizeSecurityGroupIngress) all answered and the instance reached 'available' — so the emulator IS serving; it just is not serving the address it published. Each check then builds its client from the values the emulator published:
  endpoint = cluster["Endpoint"]
  client = ctx.client("neptunedata", endpoint_url=ctx.endpoint_url(f"{endpoint}:{cluster['Port']}"))
with Port published as 8182; ctx.endpoint_url ACCEPTS that URL now (no PublishedUrlRejected, unlike the earlier state recorded in 20261109T113928Z), and the subsequent TCP connect to port 8182 fails.

Expected: https://<DBCluster.Endpoint>:<DBCluster.Port> reaches the emulator's restJson1 neptunedata front door for that cluster, and each check is then graded on its real assertions — e.g. GetEngineStatus answers 200 with status in {"healthy","recovery"}; ExecuteOpenCypherQuery answers 200 with results for 'CREATE (n:Probe {k:\'v\'}) RETURN n' and a subsequent MATCH; CreateMLEndpoint naming a nonexistent mlModelTrainingJobId answers MLResourceNotFoundException with the message 'Invalid ID provided. Please check whether a resource with the given ID exists.' (fixed text; no variable parts) and GetMLEndpoint on the same id answers the same code and message.
Actual: no neptunedata request is answered at all — the connection to the published port is refused, so all 19 checks error out and none of the 43 neptunedata operations is exercised.

Model evidence (smithy_query service_info on services/service-com-amazonaws-neptunedata-amazonneptunedataplane/smithy-model/com-amazonaws-neptunedata-amazonneptunedataplane.json): sdkId neptunedata, arnNamespace/sigv4Name neptune-db, protocols [restJson1], operation_count 43, endpointResolution urlTemplates are host-only (https://neptune-db[-fips].{Region}.{PartitionResult#dnsSuffix|dualStackDnsSuffix}) — confirming there is no regional front door and that the cluster's own published Endpoint:Port is the only address for these operations.

## Steps to Reproduce
1. Clone the repo, build and boot the emulator from this checkout, and replay the simparity-behavioral-aws-v1 behavioral suite.
2. Observe neptunedata 0/19, with all 19 checks erroring with botocore EndpointConnectionError against https://<published Endpoint host>:8182/<path> — no HTTP response is produced for any of them.
3. Reproduce by hand against the Neptune control plane: CreateDBSubnetGroup over the private subnets, CreateDBCluster with Engine=neptune, StorageEncrypted=true, EnableIAMDatabaseAuthentication=true, then CreateDBInstance with DBInstanceClass=db.t4g.medium, PubliclyAccessible=false, and wait for the instance to become 'available'.
4. Read DBCluster.Endpoint and DBCluster.Port from CreateDBCluster/DescribeDBClusters (Endpoint port-less, Port 8182 today) and open a TCP connection to <Endpoint>:<Port>. It must be accepted by the emulator; today the connect is refused because the emulator listens only on its run-assigned port.
5. Point a neptunedata client at https://<Endpoint>:<Port> and call GetEngineStatus: it must answer 200 with status 'healthy' or 'recovery'.
6. Confirm the data-plane route is bound to the owning cluster: ExecuteOpenCypherQuery 'CREATE (n:Probe {k:\'v\'}) RETURN n' against that endpoint, read it back with a MATCH, and confirm a second cluster's endpoint does not see the first cluster's data.
7. Re-replay the 19 check ids in trace_ids to confirm the fix — each must get past the connect and then be graded on its actual neptunedata assertions.
