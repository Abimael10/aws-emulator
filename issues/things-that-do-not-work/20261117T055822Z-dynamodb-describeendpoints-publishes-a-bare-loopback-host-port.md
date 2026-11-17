Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: dynamodb.describe-endpoints-returns-server-endpoints
Issue-Shape-Id: com.amazonaws.dynamodb#DynamoDB_20120810
Issue-Key: dynamodb.describe-endpoints-returns-server-endpoints|run#7|AssertionError|assert matching_region, f"no endpoint address referenced region {ctx.region}: {endpoints1}"
Issue-Commit: 1f5fd9d954c15ebb463483e9fc6bcde2d93c0e61

# dynamodb: DescribeEndpoints publishes a bare loopback host:port, so no returned Address names the service or the region

> _Note: this issue was identified against the code base at commit `1f5fd9d954c15ebb463483e9fc6bcde2d93c0e61`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeEndpoints

## Description
Cause 1 of 1: DescribeEndpoints answers with an endpoint Address that is a raw host:port with neither the `dynamodb` service name nor the caller's region in it.

AWS: DescribeEndpoints is a no-input, server-side control-plane operation whose output is the required list `Endpoints`, each member an `Endpoint` with required `Address` (String, documented "IP address of the endpoint") and required `CachePeriodInMinutes` (Long) — Smithy: DescribeEndpoints -> DescribeEndpointsResponse{Endpoints: Endpoints (required)}; Endpoint{Address: String (required), CachePeriodInMinutes: Long (required)}; no modeled errors. The model does not constrain the Address text, so recorded behavior only for the content: real DynamoDB returns the regional DynamoDB service host for the region the call was made against, i.e. an address of the form `dynamodb.<region>.<dns-suffix>` (for a us-west-2 caller, `dynamodb.us-west-2.amazonaws.com`), with CachePeriodInMinutes = 1440. The invariant a client relies on is structural: every published Address names the dynamodb service and the region it serves, and the set is stable across an immediate repeat call.

Emulator: the response carries exactly one endpoint whose Address is a bare loopback socket address — `[{'Address': '127.0.0.1:<port>', 'CachePeriodInMinutes': 1440}]`. The host part is an IP literal with no service label and no region label at all. (The particular IP/port of a run is a per-run value and is NOT the defect; the defect is that the Address is a raw socket address which, by construction, can never contain the service name or the region.) CachePeriodInMinutes and the stability-across-repeat property are correct.

Why: DynamoDB endpoint discovery feeds this Address straight back into the client's endpoint resolver. A client that derives its routing host from DescribeEndpoints — or that asserts the discovered endpoint belongs to the region it asked for, which is how SDK endpoint-discovery caches are keyed and how cross-region misrouting is caught — gets a host that identifies neither the service nor the region, so the region check fails and region-scoped endpoint caching degenerates to a single unkeyed entry. Code written against AWS's published endpoint naming does not work against the emulator.

Fix: the DescribeEndpoints handler/serializer in the DynamoDB service crate (the operation's response construction, where the endpoint list is built). It must publish a service-and-region-qualified host for the address it actually serves — a `dynamodb.<region>.<emulator dns suffix>` style name (or the request's Host header carrying that service/region prefix) — rather than echoing the bound listener's `ip:port`.

Traces: dynamodb.describe-endpoints-returns-server-endpoints.

## Evidence
Check: dynamodb.describe-endpoints-returns-server-endpoints [mismatch]

Expected (AWS): Endpoints contains at least one entry whose Address names the dynamodb service and the request region, e.g. {"Address": "dynamodb.us-west-2.amazonaws.com", "CachePeriodInMinutes": 1440}; HTTP 200.

Actual (emulator): HTTP 200 with Endpoints = [{'Address': '127.0.0.1:44381', 'CachePeriodInMinutes': 1440}].

Replay failure:
  File "check_dynamodb_describe_endpoints_returns_server_endpoints.py", line 28, in run
    assert matching_region, f"no endpoint address referenced region {ctx.region}: {endpoints1}"
  AssertionError: no endpoint address referenced region us-west-2: [{'Address': '127.0.0.1:44381', 'CachePeriodInMinutes': 1440}]

The second (stability) assertion and the CachePeriodInMinutes type/positivity assertions pass; only the service/region qualification of Address diverges.

## Steps to Reproduce
1. Point an AWS SDK/CLI client for region us-west-2 at the emulator.
2. Call DynamoDB DescribeEndpoints (no input): `aws dynamodb describe-endpoints`.
3. Inspect each Endpoints[].Address. AWS returns a host containing both `dynamodb` and the region (e.g. dynamodb.us-west-2.amazonaws.com); the emulator returns a bare `127.0.0.1:<port>` containing neither.
4. Equivalent replay: run behavioral check dynamodb.describe-endpoints-returns-server-endpoints.
