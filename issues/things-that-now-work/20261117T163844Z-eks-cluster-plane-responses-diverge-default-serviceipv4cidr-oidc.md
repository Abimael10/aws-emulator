Issue-Source: trace-sweep-divergence
Issue-Trace: eks-cluster-provider-explicit-off-switches-logging-update-tags-and-node-group-held-at-zero
Issue-Step: task#9
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: eks-cluster-provider-explicit-off-switches-logging-update-tags-and-node-group-held-at-zero eks-node-group-tags-labels-and-taints-at-create-tag-round-trip-label-and-taint-updates-and-not-found
Issue-Shape-Id: com.amazonaws.eks#AWSWesleyFrontend
Issue-Key: eks|CreateCluster|2xx->2xx|$.cluster.createdAt
Issue-Key: eks|CreateCluster|2xx->2xx|$.cluster.kubernetesNetworkConfig.serviceIpv4Cidr
Issue-Key: eks|DescribeCluster|2xx->2xx|$.cluster.computeConfig.nodePools
Issue-Key: eks|DescribeCluster|2xx->2xx|$.cluster.createdAt
Issue-Key: eks|DescribeCluster|2xx->2xx|$.cluster.identity.oidc.issuer
Issue-Key: eks|DescribeCluster|2xx->2xx|$.cluster.kubernetesNetworkConfig.serviceIpv4Cidr
Issue-Key: eks|TagResource|2xx->2xx|status
Issue-Key: eks|UntagResource|2xx->2xx|status
Issue-Key: eks|UpdateClusterConfig|2xx->2xx|$.update.createdAt
Issue-Key: eks|UpdateClusterConfig|2xx->2xx|$.update.errors
Issue-Key: eks|UpdateClusterConfig|2xx->2xx|$.update.params[*].value
Issue-Key: eks|DescribeUpdate|2xx->2xx|$.update.createdAt
Issue-Key: eks|DescribeUpdate|2xx->2xx|$.update.errors
Issue-Key: eks|DescribeUpdate|2xx->2xx|$.update.params[*].value
Issue-Key: eks|DeleteCluster|2xx->2xx|$.cluster.computeConfig.nodePools
Issue-Key: eks|DeleteCluster|2xx->2xx|$.cluster.createdAt
Issue-Key: eks|DeleteCluster|2xx->2xx|$.cluster.identity.oidc.issuer
Issue-Key: eks|DeleteCluster|2xx->2xx|$.cluster.kubernetesNetworkConfig.serviceIpv4Cidr
Issue-Commit: 81e21ca007588719de802b4dd67d83498ccb7823

# eks cluster-plane responses diverge: default serviceIpv4Cidr, OIDC issuer host, omitted empty lists, tag status and ClusterLogging order (6 causes)

> _Note: this issue was identified against the code base at commit `81e21ca007588719de802b4dd67d83498ccb7823`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateCluster

## Description
Replaying the EKS cluster-provider fixture exposes six independent defects in the cluster control-plane surface (CreateCluster / DescribeCluster / DeleteCluster / UpdateClusterConfig / DescribeUpdate / TagResource / UntagResource). All six must be closed before the affected steps replay clean.

Cause 1 of 6 - the default Kubernetes service CIDR is a hard-coded constant.
AWS: Smithy KubernetesNetworkConfigResponse.serviceIpv4Cidr documents that "If you didn't specify a CIDR block when you created the cluster, then Kubernetes assigns addresses from either the 10.100.0.0/16 or 172.20.0.0/16 CIDR blocks" - i.e. the value is chosen per cluster, not fixed. For a cluster created with no kubernetesNetworkConfig over the fixture's VPC, the recorded response carries serviceIpv4Cidr 172.20.0.0/16. Hypothesis: EKS falls back to 172.20.0.0/16 when 10.100.0.0/16 would overlap the cluster VPC's CIDR, and only uses 10.100.0.0/16 otherwise.
Emulator: always serves serviceIpv4Cidr 10.100.0.0/16, on CreateCluster, DescribeCluster and DeleteCluster, regardless of the cluster's VPC.
Why: a client that programs Service/kube-dns addressing or security-group rules from cluster.kubernetesNetworkConfig.serviceIpv4Cidr derives a range that does not match the one EKS would hand out, and a client asserting the overlap rule never sees the alternate block at all.
Fix: the CreateCluster handler's network-config defaulting, and the cluster state it persists for the describe/delete serializers.
Traces: eks-cluster-provider-explicit-off-switches-logging-update-tags-and-node-group-held-at-zero#task#9 (CreateCluster), #task#10 (DescribeCluster), #task#28 (DeleteCluster).

Cause 2 of 6 - cluster.identity.oidc.issuer is published on a real AWS host the emulator does not serve.
AWS: recorded behavior only - the recording asserts the issuer's shape, not its literal value, but the host must be one the responding endpoint actually serves.
Emulator: answers DescribeCluster/DeleteCluster with identity.oidc.issuer on the real AWS OIDC host (host_not_own: 'https://oidc.eks.<region>.amazonaws.com/id/<32-hex-id>'), an address the emulator is not reachable on.
Why: any client that fetches the issuer's /.well-known/openid-configuration or JWKS (IRSA setup, token validation) leaves the emulator for the real internet and fails, or silently talks to AWS.
Fix: the cluster serializer's identity/OIDC construction - derive the issuer host from the endpoint the request arrived on. Related open issue 20261111T111216Z-cloudformation-eks-eks-publishes-an-oidc-issuer-url-on-a.md reports the same host class from the CloudFormation path; it does not carry this fixture's replay key, so this cause is filed here.
Traces: eks-cluster-provider-explicit-off-switches-logging-update-tags-and-node-group-held-at-zero#task#10 (DescribeCluster), #task#28 (DeleteCluster).

Cause 3 of 6 - epoch timestamps are serialized with sub-millisecond float precision.
AWS: Smithy Cluster.createdAt / Update.createdAt target Timestamp ("The Unix epoch timestamp at object creation"); in this REST-JSON service they are serialized as epoch seconds with at most millisecond precision. The recording asserts the timestamp's shape, not its value (a clock time is never a requirement).
Emulator: emits a raw float with seven fractional digits (e.g. createdAt 1794932925.1860259), which fails the shape assertion.
Why: clients and SDK deserializers that expect AWS's epoch-seconds-with-millisecond form read a value whose precision AWS never emits; strict decoders and fixture-based assertions on timestamp shape break.
Fix: the service's timestamp serializer (shared by the cluster, nodegroup and update serializers) - round/format epoch seconds to millisecond precision.
Traces: eks-cluster-provider-explicit-off-switches-logging-update-tags-and-node-group-held-at-zero#task#9, #task#10, #task#18, #task#19, #task#28.

Cause 4 of 6 - members AWS returns as an empty list are omitted entirely.
AWS: Smithy ComputeConfigResponse.nodePools is a StringList and Update.errors is an ErrorDetails list; the recording shows the cluster of a non-Auto-Mode cluster carrying computeConfig.nodePools [] and every non-failed update carrying errors [].
Emulator: omits both members (nodePools: missing, expected [], got None; update.errors: missing, expected [], got None) on DescribeCluster/DeleteCluster and on UpdateClusterConfig/DescribeUpdate.
Why: a client that reads len(update.errors) or iterates computeConfig.nodePools without a null guard raises on the emulator but not against AWS - the empty-list-versus-absent distinction is part of the wire contract.
Fix: the cluster and update serializers - emit the empty list rather than dropping the member.
Traces: eks-cluster-provider-explicit-off-switches-logging-update-tags-and-node-group-held-at-zero#task#10, #task#18, #task#19, #task#28.

Cause 5 of 6 - TagResource and UntagResource answer 200 instead of the recorded 204.
AWS: TagResource (POST /tags/{resourceArn}) and UntagResource (DELETE /tags/{resourceArn}) both have an empty output shape; the recorded AWS responses are HTTP 204 with no body (the on-disk Smithy model's http code trait says 200, so the recorded 204 is the authority here - recorded behavior only).
Emulator: answers both with HTTP 200.
Why: a client that branches on the response status (or on "no content") for tag mutations sees a different status than AWS returns, and status-strict assertions and retry/idempotency logic diverge.
Fix: the TagResource and UntagResource handlers' response construction (status code and empty body).
Traces: eks-cluster-provider-explicit-off-switches-logging-update-tags-and-node-group-held-at-zero#task#12 (TagResource), #task#14 (UntagResource); the same cluster also covers eks-node-group-tags-labels-and-taints-at-create-tag-round-trip-label-and-taint-updates-and-not-found#task#14, #task#16 and #task#18.

Cause 6 of 6 - the ClusterLogging update param echoes the request's log-type order instead of the model's enum order.
AWS: Smithy UpdateParam.value is a String carrying the JSON text of the submitted value, and LogType's enum order is api, audit, authenticator, controllerManager, scheduler. The recorded Update for an enable-logging call carries value '{"clusterLogging":[{"types":["api","audit"],"enabled":true},{"types":["authenticator","controllerManager","scheduler"],"enabled":false}]}' - the enabled types normalized into enum order.
Emulator: serializes the types in whatever order the request listed them ('{"clusterLogging":[{"types":["audit","api"],"enabled":true},...]}'), on both UpdateClusterConfig and the later DescribeUpdate.
Why: a client that compares the recorded update params (or diffs desired against reported logging configuration) sees a spurious difference purely from member ordering.
Fix: the UpdateClusterConfig handler's update-param construction (and the update record it persists for DescribeUpdate) - normalize log types to LogType enum order.
Traces: eks-cluster-provider-explicit-off-switches-logging-update-tags-and-node-group-held-at-zero#task#18 (UpdateClusterConfig), #task#19 (DescribeUpdate).

## Evidence
Cause 1: $.cluster.kubernetesNetworkConfig.serviceIpv4Cidr: expected '172.20.0.0/16', got '10.100.0.0/16' (CreateCluster task#9, DescribeCluster task#10, DeleteCluster task#28; all status 200->200).
Cause 2: $.cluster.identity.oidc.issuer: host_not_own (expected '<shaped-not-asserted>', got 'https://oidc.eks.us-west-2.amazonaws.com/id/FCC45D5E639F1FF3FD20390292960768') - the recorded host is a real AWS host, not one the emulator serves.
Cause 3: $.cluster.createdAt: value_mismatch (expected '<shaped-not-asserted>', got 1794932925.1860259); $.update.createdAt: value_mismatch (expected '<shaped-not-asserted>', got 1794932945.290898).
Cause 4: $.cluster.computeConfig.nodePools: missing (expected [], got None); $.update.errors: missing (expected [], got None).
Cause 5: TagResource status: expected 204, got 200 (task#12, 1 cluster covering 2 steps); UntagResource status: expected 204, got 200 (task#14, 1 cluster covering 4 steps).
Cause 6: $.update.params[0].value: expected '{"clusterLogging":[{"types":["api","audit"],"enabled":true},{"types":["authenticator","controllerManager","scheduler"],"enabled":false}]}', got '{"clusterLogging":[{"types":["audit","api"],"enabled":true},{"types":["authenticator","controllerManager","scheduler"],"enabled":false}]}'.

## Steps to Reproduce
1. Boot the emulator and replay fixture eks-cluster-provider-explicit-off-switches-logging-update-tags-and-node-group-held-at-zero.
2. Cause 1/2/3/4: CreateCluster with no kubernetesNetworkConfig over the fixture's VPC (task#9), then DescribeCluster (task#10) and DeleteCluster (task#28): compare cluster.kubernetesNetworkConfig.serviceIpv4Cidr, cluster.identity.oidc.issuer's host, the fractional precision of cluster.createdAt, and the presence of cluster.computeConfig.nodePools.
3. Cause 5: TagResource on the cluster ARN (task#12) and UntagResource (task#14) and observe the HTTP status - AWS answers 204 with no body, the emulator answers 200.
4. Cause 6: UpdateClusterConfig enabling log types in a non-enum order (task#18) and then DescribeUpdate on the returned update id (task#19): inspect update.params[0].value's types array order and whether update.errors is present as [].
