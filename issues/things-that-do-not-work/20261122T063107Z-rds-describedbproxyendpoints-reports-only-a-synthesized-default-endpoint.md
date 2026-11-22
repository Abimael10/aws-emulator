Issue-Source: trace-sweep-divergence
Issue-Trace: rds-db-proxy-lifecycle
Issue-Step: task#8
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: rds-db-proxy-lifecycle
Issue-Shape-Id: com.amazonaws.rds#AmazonRDSv19
Issue-Key: rds|DescribeDBProxyEndpoints|2xx->4xx|$.DescribeDBProxyEndpointsResponse
Issue-Key: rds|DescribeDBProxyEndpoints|2xx->4xx|status
Issue-Commit: 9f15073fe42cc4661a8a99924cff5eec339d1b59

# rds DescribeDBProxyEndpoints reports only a synthesized default endpoint, so a custom proxy endpoint is answered 404 instead of a 200 report

> _Note: this issue was identified against the code base at commit `9f15073fe42cc4661a8a99924cff5eec339d1b59`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeDBProxyEndpoints

## Description
One net-new divergence cluster on the rds service; the replay reported no owning issue and no blocking stub for it.

Cause 1 of 1: DescribeDBProxyEndpoints has no DB-proxy-endpoint state to read — it builds its report by synthesizing one default endpoint per proxy, so a proxy's CUSTOM endpoints are invisible and a report narrowed to one of them is refused HTTP 404 instead of served HTTP 200.

AWS: Smithy `DescribeDBProxyEndpoints` — "Returns information about DB proxy endpoints". Its input members are modelled as filters over the endpoints that EXIST: `DBProxyName` (DBProxyName, len 1..63, pattern ^[a-zA-Z](?:-?[a-zA-Z0-9]+)*$) — "The name of the DB proxy whose endpoints you want to describe. If you omit this parameter, the output includes information about all DB proxy endpoints associated with all your DB proxies"; `DBProxyEndpointName` — "The name of a DB proxy endpoint to describe. If you omit this parameter, the output includes information about all DB proxy endpoints associated with the specified proxy". The output member `DBProxyEndpoints` (DBProxyEndpointList) is "The list of ProxyEndpoint objects returned by the API operation", and each `DBProxyEndpoint` carries the endpoint's own record — DBProxyEndpointName, DBProxyName, DBProxyEndpointArn, EndpointType, TargetRole, EndpointNetworkType, IsDefault, Endpoint, Status, VpcId, VpcSubnetIds, VpcSecurityGroupIds, CreatedDate. The modelled 404s are `DBProxyEndpointNotFoundFault` ("The DB proxy endpoint doesn't exist") and `DBProxyNotFoundFault` ("The specified proxy name doesn't correspond to a proxy owned by your Amazon Web Services account in the specified Amazon Web Services Region") — i.e. 404 is reserved for a name that names NOTHING. An endpoint that a preceding CreateDBProxyEndpoint accepted exists, so AWS answers HTTP 200 and reports it (the recording shows it still `creating`, `IsDefault` false, `EndpointType` CUSTOM, `TargetRole` READ_WRITE, with the VPC placement the create request carried). Recorded behavior for the status/shape: HTTP 200 with a DescribeDBProxyEndpointsResponse/DescribeDBProxyEndpointsResult/DBProxyEndpoints envelope holding that one member.

Emulator: the request is answered HTTP 404 with NO DescribeDBProxyEndpointsResponse envelope at all. The handler never reads any stored endpoint: for every proxy in scope it pushes exactly `crate::state::default_proxy_endpoint(proxy)` into the report, then, when the request narrows the report by `DBProxyEndpointName`, retains the entries whose name matches and — finding none, because the only entry is the synthesized default endpoint — returns the modelled 404 fault (`DBProxyEndpointNotFoundFault`, or `DBProxyNotFoundFault` when the named proxy is unknown). There is consequently no code path by which any endpoint other than the one synthesized default per proxy can ever be reported, and no count of a proxy's endpoints other than exactly one.

Why: a client that creates a custom read/write or read-only proxy endpoint and then polls DescribeDBProxyEndpoints for it — the documented way to wait for `creating` -> `available` and to learn the endpoint's address, ARN and VPC placement — gets a 404 "doesn't exist" for an endpoint it just created. Waiters never settle, so the whole custom-proxy-endpoint lifecycle (create, wait, connect through the endpoint, modify, delete) is unusable, and a client that treats the 404 as authoritative concludes its create was lost. Listing a proxy's endpoints also under-reports: a proxy with N endpoints is always reported with exactly one.

Fix: the DescribeDBProxyEndpoints handler and the rds proxy-endpoint state it reads. The service state needs a keyed store of DB proxy endpoints (per region + proxy + endpoint name) that the create/modify/delete proxy-endpoint handlers write and this read path projects, with the synthesized default endpoint reported as one member of that collection (IsDefault true) rather than as the whole report; the `DBProxyEndpointName` narrowing and the modelled 404 faults must then be evaluated against the stored endpoints, so 404 is raised only for a name that truly resolves nothing. Related open issues name neighbouring causes but carry no replay key for this step: 20261104T053605Z-rds-createdbproxyendpoint-stub.md (CreateDBProxyEndpoint is still an unimplemented scaffold that answers 501, so nothing writes endpoint state today) and 20261109T205028Z-rds-global-cluster-proxy-and-cluster-envelopes-carry.md (DescribeDBProxies envelope members and unserved proxy endpoint hosts). Fixing either leaves this read path still unable to report a custom endpoint.

Traces: rds-db-proxy-lifecycle#task#8 (cluster of 1). Handler: services/service-com-amazonaws-rds-amazonrdsv19/src/ops/describe_db_proxy_endpoints.rs lines 200-233 (the report is built from `crate::state::default_proxy_endpoint`, then the name narrowing returns DBProxyEndpointNotFoundFault); the create path it should read from is services/service-com-amazonaws-rds-amazonrdsv19/src/ops/create_db_proxy_endpoint.rs line 195 (`Scaffolded::not_implemented`).

## Evidence
Replay of trace set simparity-wire-aws-v1 (106 fixtures, 1132 steps, score 62.5%). Divergence cluster service=rds|DescribeDBProxyEndpoints|status=200->404|keys=$.DescribeDBProxyEndpointsResponse,status — verdict mismatch, affected_count 1, stub_path empty, owned_by empty.

Fixture rds-db-proxy-lifecycle, step task#8, operation DescribeDBProxyEndpoints:
  status: expected 200, got 404
  $.DescribeDBProxyEndpointsResponse: missing — expected the full envelope
    DescribeDBProxyEndpointsResult.DBProxyEndpoints.member = {
      DBProxyEndpointName (the name the preceding create was given),
      DBProxyName (the owning proxy),
      DBProxyEndpointArn, EndpointType 'CUSTOM', TargetRole 'READ_WRITE',
      EndpointNetworkType 'IPV4', IsDefault 'false', Status 'creating',
      Endpoint (the endpoint's own DNS address), VpcId,
      VpcSubnetIds.member (the subnets the create request carried),
      VpcSecurityGroupIds.member (the security group the create request carried),
      CreatedDate }
  got None — the emulator answered a 404 modelled fault body instead of the report.

The per-run values in the recording (the generated endpoint/proxy names, the sg-/vpc-/subnet- ids, the ARN, the CreatedDate timestamp, the request id and the host portion of Endpoint) are environment-specific and are NOT the requirement; the requirement is the HTTP 200 status, the DBProxyEndpoints envelope, and that the reported member is the endpoint that was created, with IsDefault false, EndpointType/TargetRole as requested and Status reflecting its lifecycle.

Code evidence (why no stored endpoint can ever be reported): describe_db_proxy_endpoints.rs builds `reported` as one `crate::state::default_proxy_endpoint(proxy)` per in-scope proxy and then, for a request carrying DBProxyEndpointName, `reported.retain(...)` followed by `return Err(DBProxyEndpointNotFoundFault { .. })` when the retain empties the list.

## Steps to Reproduce
1. git_clone(), then replay_wire_corpus() against the built emulator.
2. Fixture rds-db-proxy-lifecycle, step task#8 — the proxy created earlier in the fixture is read back by DescribeDBProxies at task#7 (200), then DescribeDBProxyEndpoints at task#8 answers 404 where the recording has 200 with one CUSTOM endpoint.
3. By hand: CreateDBProxy, then CreateDBProxyEndpoint on it with VpcSubnetIds/VpcSecurityGroupIds and TargetRole=READ_WRITE, then DescribeDBProxyEndpoints with DBProxyEndpointName set to that endpoint — observe HTTP 404 instead of HTTP 200 with the endpoint record.
4. Also call DescribeDBProxyEndpoints with only DBProxyName — observe exactly one member (the synthesized default endpoint) regardless of how many endpoints the proxy was given.
