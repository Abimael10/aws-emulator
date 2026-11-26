Issue-Source: trace-sweep-divergence
Issue-Trace: dsql-vpc-endpoint-service-name
Issue-Step: task#3
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: dsql-vpc-endpoint-service-name
Issue-Shape-Id: com.amazonaws.dsql#DSQL
Issue-Key: dsql|GetVpcEndpointServiceName|2xx->2xx|$.clusterVpcEndpoint
Issue-Commit: 48ea639b2644fe3267d5fb5c94b4b1fdebe2ae92

# dsql GetVpcEndpointServiceName returns a clusterVpcEndpoint host the emulator does not serve

> _Note: this issue was identified against the code base at commit `48ea639b2644fe3267d5fb5c94b4b1fdebe2ae92`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetVpcEndpointServiceName

## Description
One cause.

Cause 1 of 1: GetVpcEndpointServiceName's clusterVpcEndpoint is rendered under a pseudo-domain that is not an address the emulator answers on.

AWS: GetVpcEndpointServiceName returns, besides the required serviceName, an optional clusterVpcEndpoint — Smithy: GetVpcEndpointServiceNameOutput.clusterVpcEndpoint targets ClusterVpcEndpoint with pattern ^[a-zA-Z0-9.-]+$, documented as "The VPC connection endpoint for the cluster" (GET /clusters/{identifier}/vpc-endpoint-service-name, 200). The model states no domain rule, so for the host's composition this is recorded behavior only: the recording shows a resolvable DNS name of the form <cluster identifier>.dsql-<short per-cluster suffix>.<region>.on.aws — a name a client can actually dial to reach the cluster's endpoint. The identifier and region come from the request/state and the short suffix is the per-cluster endpoint-service token; the concrete recorded suffix and the concrete recorded domain host are per-environment values and are NOT themselves the requirement. The requirement is that the announced VPC connection endpoint is a host the service is actually reachable on.

Emulator: returns <cluster identifier>.dsql-<suffix>.<region>.localhost — a bare `.localhost` subdomain (replay verdict host_not_own: expected '…dsql-7cwu.us-west-2.on.aws', got '…dsql-85e3.us-west-2.localhost'). That name is neither the AWS-form endpoint nor the address the emulator is reached on: the emulator serves its data listener on a concrete host:port, and the sub-labelled `.localhost` name it announces does not resolve to it and carries no port. The differing short suffix (7cwu vs 85e3) is a per-run generated value and is not part of this finding.

Why: a client that takes clusterVpcEndpoint and connects to it — which is the member's entire purpose — gets a name-resolution failure instead of a connection, so endpoint-discovery flows that work against AWS cannot be exercised at all against the emulator. A client that parses the endpoint (region label, domain suffix) also reads a different structure than AWS produces.

Fix: GetVpcEndpointServiceName's handler/response builder for the clusterVpcEndpoint member — compose the announced endpoint host from the address the emulator is actually reached on (and keep the AWS name structure <cluster id>.dsql-<short suffix>.<region>.<domain>) instead of hard-coding a `.localhost` pseudo-domain.

Related but NOT the same cause: open issue 20261029T193445Z-dsql-success-replies-diverge-tagging-answers-200-with.md covers the sibling member $.serviceName in this same operation (it embeds the 26-character cluster id instead of a short suffix). Its replay key does not cover $.clusterVpcEndpoint, which is reported as an unowned divergence here.

Traces: dsql-vpc-endpoint-service-name#task#3.

## Evidence
Replay of trace set simparity-wire-aws-v1 (score 91.7%, 8 mismatched steps):
- dsql-vpc-endpoint-service-name#task#3 GetVpcEndpointServiceName, status 200 on both sides; divergence key service=dsql|GetVpcEndpointServiceName|status=200->200|keys=$.clusterVpcEndpoint, verdict mismatch (host_not_own).
- detail: "$.clusterVpcEndpoint: host_not_own (expected '01m8b7th804kmbs3gb0bwq8xt5.dsql-7cwu.us-west-2.on.aws', got '01m8b7th804kmbs3gb0bwq8xt5.dsql-85e3.us-west-2.localhost')".
- Fixed text of the value's template: the cluster identifier (from the request's `identifier` httpLabel), the literal 'dsql-' prefix on the suffix label, the region label, and a domain the host is reachable under. Variable, per-environment parts: the short endpoint-service suffix ('7cwu' / '85e3') and the domain/host itself — neither recorded literal is a requirement.
- This cluster has affected_count 1 and owned_by is empty, i.e. no open issue carries its replay key; stub_path is empty, so the operation is implemented.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay wire fixture dsql-vpc-endpoint-service-name.
2. At step task#3, GET /clusters/{identifier}/vpc-endpoint-service-name (GetVpcEndpointServiceName) for the cluster created earlier in the fixture.
3. Read $.clusterVpcEndpoint from the 200 response: the emulator returns a '<cluster id>.dsql-<suffix>.<region>.localhost' name.
4. Attempt to resolve/connect to the returned host: it is not the address the emulator's data listener is reached on, whereas AWS returns a dialable VPC connection endpoint for the cluster.
