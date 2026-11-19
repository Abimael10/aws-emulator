Issue-Source: trace-sweep-divergence
Issue-Trace: dynamodb-catalog-describe-endpoints
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: dynamodb-catalog-describe-endpoints dynamodb-table-control-plane-lifecycle dynamodb-table-lifecycle-core dynamodb-table-tagging-lifecycle
Issue-Shape-Id: com.amazonaws.dynamodb#DynamoDB_20120810
Issue-Key: dynamodb|DescribeEndpoints|2xx->2xx|$.Endpoints[*].Address
Issue-Commit: 8b466838d4bdc1ab2a4a37a0f7b7d85466ef4e9d

# dynamodb DescribeEndpoints publishes an authority the emulator does not serve

> _Note: this issue was identified against the code base at commit `8b466838d4bdc1ab2a4a37a0f7b7d85466ef4e9d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeEndpoints

## Description
Cause 1 of 1: DescribeEndpoints answers 200 but the published Endpoints[].Address is an authority that is neither a resolvable DynamoDB service host nor the address the emulator actually accepts requests on, so endpoint discovery hands a client a dead route. Fixture dynamodb-catalog-describe-endpoints, step task#1.

AWS: DescribeEndpoints is a no-input control-plane operation. Smithy: DescribeEndpoints -> DescribeEndpointsResponse{Endpoints: Endpoints (required, list<Endpoint>)}; Endpoint{Address: String (required, doc 'IP address of the endpoint'), CachePeriodInMinutes: Long (required, doc 'Endpoint cache time to live (TTL) value')}; no modeled errors. The model puts no constraint on the Address text, so recorded behavior only for its content: the recorded response publishes a host that (a) names the dynamodb service and the caller's region and (b) is an authority the service is genuinely reachable at for that region, with no extra port component, i.e. the discovery result is directly usable as the client's next request authority. The requirement is structural - the Address must be an authority the responder actually serves - and not the literal recorded hostname, which is environment-specific.

Emulator: the single published Address is a service/region-labelled name carrying an explicit port that is not the emulator's own data authority. In this replay the emulator's data listener accepts on port 46629 (replay emulator_detail: '/__admin/health answered 200 and the :46629 data listener accepts') while DescribeEndpoints announced the authority 'dynamodb.<region>.localhost:38295'. The replay classifies this as host_not_own: the emulator announced an address other than the one it is reached on. CachePeriodInMinutes and the list shape are fine; only the announced authority diverges. (The concrete hostname and port numbers are per-run values and are not themselves the requirement - the defect is that the announced authority is not derived from the authority the request arrived on / the listener that serves the data plane.)

Why: DynamoDB endpoint discovery exists so that an SDK can feed Endpoints[].Address straight back into its endpoint resolver and cache it for CachePeriodInMinutes. A client that honours discovery - which the AWS SDKs do automatically for DynamoDB when endpoint discovery is enabled - retargets every subsequent request at the announced authority. Because nothing listens there, those requests fail to connect rather than reaching the emulator, so a workload that works with discovery disabled breaks the moment it is enabled, and the failure looks like a network fault rather than a fidelity gap. A client that instead asserts the discovered endpoint belongs to the service/region it asked for still cannot use it, since the authority is not routable.

Fix: the DescribeEndpoints handler/response construction in the DynamoDB service crate, where the endpoint list is built. It must publish an authority the emulator genuinely serves - derived from the inbound request's Host/authority (or the bound data listener), with the dynamodb service and region labels applied to that same authority - instead of a host:port that names a port the data plane does not accept on.

Related but not covering: open issue 20261117T055822Z-dynamodb-describeendpoints-publishes-a-bare-loopback-host-port.md is a BEHAVIORAL-corpus issue about the Address being a bare '127.0.0.1:<port>' with no service or region label. The service/region labelling is now present; this wire divergence is the remaining, distinct fault that the announced authority is not one the emulator answers on, and that open issue carries no replay key for this fixture step.

Traces: dynamodb-catalog-describe-endpoints#task#1 (representative); same cluster: dynamodb-table-control-plane-lifecycle#task#1, dynamodb-table-lifecycle-core#task#10, dynamodb-table-tagging-lifecycle#task#2.

## Evidence
Fixture dynamodb-catalog-describe-endpoints, step task#1, operation DescribeEndpoints, service dynamodb.

Expected (recorded AWS): HTTP 200, $.Endpoints[0].Address = a dynamodb service/region authority that is actually served for the caller's region (recorded: 'dynamodb.us-west-2.amazonaws.com'), CachePeriodInMinutes present.

Actual (emulator): HTTP 200, $.Endpoints[0].Address = 'dynamodb.us-west-2.localhost:38295'. Replay detail: "$.Endpoints[0].Address: host_not_own (expected 'dynamodb.us-west-2.amazonaws.com', got 'dynamodb.us-west-2.localhost:38295')", divergence_key service=dynamodb|DescribeEndpoints|status=200->200|keys=$.Endpoints[0].Address, verdict mismatch.

The announced port (38295) is not the emulator's data authority: the same replay reports the accepting data listener as :46629. The host and port literals are per-run values; the invariant that fails is that the announced authority is not the one the emulator is reached on.

Cluster members (4 steps): dynamodb-catalog-describe-endpoints#task#1, dynamodb-table-control-plane-lifecycle#task#1, dynamodb-table-lifecycle-core#task#10, dynamodb-table-tagging-lifecycle#task#2.

## Steps to Reproduce
1. Boot the emulator and note the authority its data listener accepts on (the replay reports it, e.g. :46629).
2. Point an AWS SDK/CLI client for the region under test at that authority.
3. Call DynamoDB DescribeEndpoints with no input: `aws dynamodb describe-endpoints`.
4. Read Endpoints[0].Address and compare its authority to the one from step 1: the emulator returns a different authority (a <service>.<region>.localhost host with a port nothing listens on), while AWS returns an authority the service is reachable at.
5. Attempt a request against the returned authority (what an SDK with endpoint discovery enabled does): the connection is not accepted.
6. Equivalent replay: replay fixture dynamodb-catalog-describe-endpoints, step task#1.
