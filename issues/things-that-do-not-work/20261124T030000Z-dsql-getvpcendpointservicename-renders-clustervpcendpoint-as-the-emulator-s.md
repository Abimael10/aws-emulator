Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: dsql.get-vpc-endpoint-service-name-valid
Issue-Shape-Id: com.amazonaws.dsql#DSQL
Issue-Key: dsql.get-vpc-endpoint-service-name-valid|run#11|AssertionError|assert re.fullmatch(r"[A-Za-z0-9.-]+(:\d+)?", endpoint) and endpoint.startswith(f"{identifier}."), (
Issue-Commit: c67fd313bd95bd11e1fae665856d19009c1b14ab

# dsql GetVpcEndpointServiceName renders clusterVpcEndpoint as the emulator's bind address instead of a per-cluster VPC-connection host

> _Note: this issue was identified against the code base at commit `c67fd313bd95bd11e1fae665856d19009c1b14ab`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetVpcEndpointServiceName

## Description
One cause.

Cause 1 of 1: GetVpcEndpointServiceName answers 200 but renders $.clusterVpcEndpoint as a bare literal loopback address (the emulator's own bind host) rather than the cluster's VPC-connection DNS host, so the value carries no cluster-identifier label, no `dsql` service label and no region at all. Failing check: dsql.get-vpc-endpoint-service-name-valid.

AWS: GetVpcEndpointServiceName is GET /clusters/{identifier}/vpc-endpoint-service-name and answers 200. Smithy (com.amazonaws.dsql#DSQL): GetVpcEndpointServiceNameOutput$serviceName is a required ServiceName with @length 1..128 and @pattern ^com\.amazonaws\.[a-z0-9-]+\.dsql[a-z0-9-]*$; GetVpcEndpointServiceNameOutput$clusterVpcEndpoint is a ClusterVpcEndpoint with @pattern ^[a-zA-Z0-9.-]+$, documented as "The VPC connection endpoint for the cluster." The model constrains the character set but not the host layout, so the layout is recorded behavior only: AWS returns a DNS host for the cluster being addressed, of the form <clusterId>.dsql-<short per-cluster endpoint-service suffix>.<region>.on.aws — i.e. the host BEGINS with the 26-character cluster identifier supplied in the request's `identifier` label, and its `dsql` label carries the same short suffix that appears after `dsql` in the $.serviceName returned by the SAME call. The fixed text is the leading cluster-id label separator, the `.dsql`/`.dsql-` label and the `.on.aws` tail; the cluster id, the suffix value and the region are values from the request and from the emulator's stored cluster, and no single recording's id, suffix or region is part of the requirement.

Emulator: returns $.clusterVpcEndpoint = '127.0.0.1'. Replay of dsql.get-vpc-endpoint-service-name-valid reports, at the clusterVpcEndpoint assertion, `AssertionError: expected clusterVpcEndpoint to be a well-formed host named for identifier '01m85qrk9nq3kp90txw4dvfw8p', got '127.0.0.1'`. The reply's $.serviceName passed its pattern and region assertions in the same check, so only this member diverges: the field is filled with the emulator's listening address instead of being derived from the addressed cluster. The precise loopback literal is a per-environment bind value and is not itself the requirement; the defect is that the member is not a per-cluster host derived from the request's identifier and the stored cluster's endpoint-service suffix.

Why: a client that uses this field to create or validate an interface VPC endpoint, or that pairs clusterVpcEndpoint with serviceName from the same reply, receives a value that identifies no cluster and no endpoint service. Code that parses the host to recover the cluster id or the endpoint-service suffix finds neither, code that keys a connection pool or TLS SNI on the host collides across every cluster in the account, and anything that resolves the value reaches the emulator itself rather than the named cluster's VPC path.

Fix: GetVpcEndpointServiceName's handler/serializer in the dsql service crate, together with the cluster state that should mint and store the per-cluster endpoint-service suffix; the same stored suffix must build both $.serviceName and the $.clusterVpcEndpoint host, and the host must be built from the request's `identifier` rather than from the server's bind address.

Related open issue that does NOT cover this: 20261117T165811Z-dsql-getvpcendpointservicename-returns-a-clustervpcendpoint-host-without-the.md files the same member for a DIFFERENT rendering — a host of the form <clusterId>.dsql.<region>.on.aws that was correctly named for the cluster but merely lacked the endpoint-service suffix. The behavior observed here is not that host: the member no longer contains the cluster id, the dsql label or the region, so the per-cluster host is not being constructed at all. 20261029T193445Z-dsql-success-replies-diverge-tagging-answers-200-with.md covers this operation's $.serviceName, a different member.

Traces: behavioral check dsql.get-vpc-endpoint-service-name-valid.

## Evidence
Behavioral suite simparity-behavioral-aws-v1, 37/51 passed; dsql 37/49 (76%).

- [mismatch] dsql.get-vpc-endpoint-service-name-valid
  File "check_dsql_get_vpc_endpoint_service_name_valid.py", line 33, in run
    assert re.fullmatch(r"[A-Za-z0-9.-]+(:\d+)?", endpoint) and endpoint.startswith(f"{identifier}."), ...
  AssertionError: expected clusterVpcEndpoint to be a well-formed host named for identifier '01m85qrk9nq3kp90txw4dvfw8p', got '127.0.0.1'

Expected: a 200 whose $.clusterVpcEndpoint is a DNS host beginning with the request's cluster identifier, of the AWS form <clusterId>.dsql-<short endpoint-service suffix>.<region>.on.aws, whose suffix matches the one in the same reply's $.serviceName. Fixed text: the `.dsql`/`.dsql-` label and the `.on.aws` tail. Variable parts taken from the request and the emulator's state: the 26-character cluster id (here '01m85qrk9nq3kp90txw4dvfw8p'), the per-cluster endpoint-service suffix, and the region.
Actual: $.clusterVpcEndpoint = '127.0.0.1' — the emulator's own bind address; no cluster-id label, no dsql label, no region. Status is 200 on both sides and $.serviceName satisfied its @pattern and contained the region, so the divergence is confined to this member.

Smithy consulted: GetVpcEndpointServiceName operation signature (input identifier @pattern ^[a-z0-9]{26}$ as httpLabel; output serviceName required @pattern ^com\.amazonaws\.[a-z0-9-]+\.dsql[a-z0-9-]*$ @length 1..128; output clusterVpcEndpoint @pattern ^[a-zA-Z0-9.-]+$) and the doc for GetVpcEndpointServiceNameOutput$clusterVpcEndpoint ("The VPC connection endpoint for the cluster.").

## Steps to Reproduce
1. Boot the emulator from this checkout and run the behavioral suite check dsql.get-vpc-endpoint-service-name-valid.
2. Call CreateCluster with deletionProtectionEnabled=false; note the returned `identifier` (a 26-character lowercase-alphanumeric cluster id).
3. Call GetVpcEndpointServiceName with identifier = that cluster id (GET /clusters/{identifier}/vpc-endpoint-service-name).
4. Read the 200 reply: $.serviceName matches ^com\.amazonaws\.[a-z0-9-]+\.dsql[a-z0-9-]*$ and contains the region, but $.clusterVpcEndpoint is '127.0.0.1' — the emulator's bind address — instead of a host starting with '<identifier>.' and ending in '.on.aws' whose dsql label carries the same endpoint-service suffix as $.serviceName.
5. The assertion `endpoint.startswith(f"{identifier}.")` fails.
