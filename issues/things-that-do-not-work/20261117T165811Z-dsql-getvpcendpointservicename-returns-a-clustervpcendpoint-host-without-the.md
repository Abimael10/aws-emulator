Issue-Source: trace-sweep-divergence
Issue-Trace: dsql-vpc-endpoint-service-name
Issue-Step: task#3
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: dsql-vpc-endpoint-service-name
Issue-Shape-Id: com.amazonaws.dsql#DSQL
Issue-Key: dsql|GetVpcEndpointServiceName|2xx->2xx|$.clusterVpcEndpoint
Issue-Commit: 3d549b51f10ecf86bd29287e87008ba36d5f0d1d

# dsql GetVpcEndpointServiceName returns a clusterVpcEndpoint host without the per-cluster endpoint-service suffix

> _Note: this issue was identified against the code base at commit `3d549b51f10ecf86bd29287e87008ba36d5f0d1d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetVpcEndpointServiceName

## Description
One cause.

Cause 1 of 1: GetVpcEndpointServiceName's clusterVpcEndpoint is rendered as the plain cluster endpoint host instead of the cluster's VPC-connection endpoint host, so it omits the per-cluster endpoint-service suffix that the serviceName returned by the SAME call carries.

AWS: GetVpcEndpointServiceName answers 200 with two members. Smithy: GetVpcEndpointServiceNameOutput.serviceName is a required ServiceName (@length 1..128, @pattern ^com\.amazonaws\.[a-z0-9-]+\.dsql[a-z0-9-]*$) and GetVpcEndpointServiceNameOutput.clusterVpcEndpoint is a ClusterVpcEndpoint (@pattern ^[a-zA-Z0-9.-]+$), documented as "The VPC connection endpoint for the cluster." The model constrains the character set but not the host layout, so the layout is recorded behavior only: the recording shows the VPC-connection endpoint as <clusterId>.dsql-<short per-cluster endpoint-service suffix>.<region>.on.aws, i.e. the dsql label in the host carries the same short suffix that appears after `dsql` in the serviceName this call returns, and that host is distinct from the cluster's ordinary endpoint (<clusterId>.dsql.<region>.on.aws). The requirement is that FORM and that consistency between the two members of one reply; the concrete 26-character cluster id and the suffix value in any one recording (`7cwu`) are per-environment values that fill the template and are NOT the requirement.

Emulator: returns clusterVpcEndpoint = <clusterId>.dsql.<region>.on.aws — the cluster's ordinary endpoint host, with no endpoint-service suffix on the dsql label. Replay of dsql-vpc-endpoint-service-name#task#3 reports, with status 200 on both sides, $.clusterVpcEndpoint host_not_own: expected '01m7n6yw08s3kcvvjfgrbp7esk.dsql-7cwu.us-west-2.on.aws', got '01m7n6yw08s3kcvvjfgrbp7esk.dsql.us-west-2.on.aws'. The replay additionally flags the announced host as one the emulator does not serve, so the value is not only shaped differently, it names an address the emulator cannot be reached on.

Why: a client that uses this field to create or validate an interface VPC endpoint, or that pairs clusterVpcEndpoint with serviceName from the same reply, gets a host that belongs to a different (non-VPC) access path and does not correspond to the endpoint service just named. Code that parses the host to recover the endpoint-service suffix finds no suffix at all, and anything that dials the announced host fails because the emulator does not serve it.

Fix: GetVpcEndpointServiceName's handler/serializer in the dsql service crate, together with the cluster state that should mint and store the per-cluster endpoint-service suffix. The same stored suffix must build both serviceName and the clusterVpcEndpoint host, and the host must be one the emulator actually answers on.

Related open issues that do NOT cover this: 20261029T193445Z-dsql-success-replies-diverge-tagging-answers-200-with.md files the same operation's $.serviceName (it embeds the full 26-character cluster id instead of a short suffix) but not $.clusterVpcEndpoint; 20261109T163226Z-dsql-createcluster-publishes-a-cluster-endpoint-on-a.md and 20261109T235613Z-dsql-publishes-a-cluster-endpoint-on-a-host.md cover CreateCluster's $.endpoint host, a different operation and member.

Traces: dsql-vpc-endpoint-service-name#task#3.

## Evidence
Replay of trace set simparity-wire-aws-v1 (score 88.5%, 11 mismatched steps):
- dsql-vpc-endpoint-service-name#task#3 GetVpcEndpointServiceName, status 200 expected and 200 actual, divergence key dsql|GetVpcEndpointServiceName|status=200->200|keys=$.clusterVpcEndpoint.
  Expected (recorded): $.clusterVpcEndpoint = '01m7n6yw08s3kcvvjfgrbp7esk.dsql-7cwu.us-west-2.on.aws' — template <clusterId>.dsql-<endpoint-service suffix>.<region>.on.aws; the fixed text is the '.dsql-' label prefix and the '.on.aws' tail, while the cluster id, the suffix and the region come from the request and the emulator's stored cluster.
  Actual: $.clusterVpcEndpoint = '01m7n6yw08s3kcvvjfgrbp7esk.dsql.us-west-2.on.aws' — the suffix segment is absent, so the dsql label is bare.
  The replay verdict is host_not_own, i.e. the emulator also does not serve the host it announced.
- affected_count 1, owned_by empty, stub_path empty: no open issue carries this replay key (the existing dsql success-reply bundle lists dsql|GetVpcEndpointServiceName|2xx->2xx|$.serviceName only).
- Smithy consulted: GetVpcEndpointServiceName operation signature (output members serviceName/clusterVpcEndpoint, their patterns) and the doc for GetVpcEndpointServiceNameOutput$clusterVpcEndpoint.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay fixture dsql-vpc-endpoint-service-name.
2. Setup creates a DSQL cluster, which returns a cluster identifier.
3. At step task#3 call GetVpcEndpointServiceName with identifier = that cluster id (GET /clusters/{identifier}/vpc-endpoint-service-name).
4. Read the 200 reply: $.clusterVpcEndpoint is '<clusterId>.dsql.<region>.on.aws' with no endpoint-service suffix on the dsql label, where AWS returns '<clusterId>.dsql-<short per-cluster suffix>.<region>.on.aws' matching the suffix in the same reply's $.serviceName.
5. Try to connect to the announced host: the emulator does not serve it.
