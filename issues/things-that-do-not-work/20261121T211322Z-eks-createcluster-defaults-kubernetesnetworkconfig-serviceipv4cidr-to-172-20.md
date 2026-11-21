Issue-Source: trace-sweep-divergence
Issue-Trace: eks-ekscc-traj-lenient-scalar-coercion
Issue-Step: task#5
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: eks-ekscc-traj-lenient-scalar-coercion eks-ekscc-traj-reference-create-retry-and-delete
Issue-Shape-Id: com.amazonaws.eks#AWSWesleyFrontend
Issue-Key: eks|CreateCluster|2xx->2xx|$.cluster.kubernetesNetworkConfig.serviceIpv4Cidr
Issue-Key: eks|DeleteCluster|2xx->2xx|$.cluster.kubernetesNetworkConfig.serviceIpv4Cidr
Issue-Commit: ef5208a2316f6e603f33dcb5e1ac1bc1a8b9d2da

# eks CreateCluster defaults kubernetesNetworkConfig.serviceIpv4Cidr to 172.20.0.0/16 instead of 10.100.0.0/16 (2 causes)

> _Note: this issue was identified against the code base at commit `ef5208a2316f6e603f33dcb5e1ac1bc1a8b9d2da`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateCluster

## Description
When CreateCluster omits kubernetesNetworkConfig.serviceIpv4Cidr, the emulator assigns the wrong one of the two blocks EKS can pick from, and the wrong value is then echoed by every later read of that cluster. Two causes, at the two operations the corpus exercises.

Cause 1 of 2: CreateCluster response (fixture eks-ekscc-traj-lenient-scalar-coercion, step task#5).
AWS: Smithy: CreateCluster.kubernetesNetworkConfig targets KubernetesNetworkConfigRequest; its serviceIpv4Cidr documentation states 'If you don't specify a block, Kubernetes assigns addresses from either the 10.100.0.0/16 or 172.20.0.0/16 CIDR blocks', and the chosen block must not overlap the CIDR assigned to the cluster's VPC. The recording shows the block AWS actually assigns for a cluster whose VPC does not overlap it: 10.100.0.0/16, returned at $.cluster.kubernetesNetworkConfig.serviceIpv4Cidr on the 200 CreateCluster response. Hypothesis: AWS prefers 10.100.0.0/16 and only falls back to 172.20.0.0/16 when that block collides with the cluster VPC's CIDR; the recording establishes the preference, not the fallback rule.
Emulator: returns 172.20.0.0/16 unconditionally for a request that specified no serviceIpv4Cidr, on an otherwise-matching 200 response.
Why: the service CIDR is what a client derives cluster-internal addresses from — notably the DNS service address (the .10 of the service CIDR) used to configure kubelet/CoreDNS, and any route/security-group or peering plan that must avoid the cluster's service range. A client that reads the emulator's cluster and configures 172.20.0.10 where AWS would have handed it 10.100.0.10 mis-configures node bootstrap and network planning, and any assertion on the default block fails.
Fix: the CreateCluster handler's default-assignment for kubernetesNetworkConfig (the state it stores for the new cluster) must prefer 10.100.0.0/16 when no serviceIpv4Cidr is supplied, falling back to 172.20.0.0/16 only when 10.100.0.0/16 overlaps the cluster VPC's CIDR; the stored value, not just the response projection, must change.
Traces: eks-ekscc-traj-lenient-scalar-coercion#task#5; eks-ekscc-traj-reference-create-retry-and-delete#task#5.

Cause 2 of 2: DeleteCluster response echoes the same wrong block (fixture eks-ekscc-traj-lenient-scalar-coercion, step task#6).
AWS: Smithy: DeleteCluster output member cluster targets Cluster, so the deletion response projects the cluster record including kubernetesNetworkConfig.serviceIpv4Cidr; the recording shows 10.100.0.0/16 there, matching what CreateCluster assigned.
Emulator: returns 172.20.0.0/16 at $.cluster.kubernetesNetworkConfig.serviceIpv4Cidr on the 200 DeleteCluster response.
Why: the terminal read of the cluster reports a service CIDR the cluster never would have had on AWS, so a client that records or audits the cluster's network configuration at teardown captures the wrong range.
Fix: same component — the cluster record written by CreateCluster; the DeleteCluster projection is faithful to whatever state holds, so it is correct once the stored default is. It is listed separately because it is a separately recorded step that must be re-replayed.
Traces: eks-ekscc-traj-lenient-scalar-coercion#task#6; eks-ekscc-traj-reference-create-retry-and-delete#task#7.

Issue 20261117T163844Z-eks-cluster-plane-responses-diverge-default-serviceipv4cidr-oidc.md named a default-serviceIpv4Cidr cause but is RESOLVED, so it does not cover this: the corpus still diverges on the same member, now in the 172.20.0.0/16 direction.

## Evidence
Replay of fixture corpus simparity-wire-aws-v1 against the emulator built from this checkout:

CreateCluster — expected status 200, actual status 200; divergence '$.cluster.kubernetesNetworkConfig.serviceIpv4Cidr: value_mismatch (expected 10.100.0.0/16, got 172.20.0.0/16)'. Cluster key service=eks|CreateCluster|status=200->200|keys=$.cluster.kubernetesNetworkConfig.serviceIpv4Cidr, 2 affected steps.

DeleteCluster — expected status 200, actual status 200; divergence '$.cluster.kubernetesNetworkConfig.serviceIpv4Cidr: value_mismatch (expected 10.100.0.0/16, got 172.20.0.0/16)'. Cluster key service=eks|DeleteCluster|status=200->200|keys=$.cluster.kubernetesNetworkConfig.serviceIpv4Cidr, 2 affected steps.

Smithy (com-amazonaws-eks-awswesleyfrontend): KubernetesNetworkConfigRequest.serviceIpv4Cidr — 'If you don't specify a block, Kubernetes assigns addresses from either the 10.100.0.0/16 or 172.20.0.0/16 CIDR blocks ... Doesn't overlap with any CIDR block assigned to the VPC that you selected for VPC.'

Expected (AWS, recorded): cluster.kubernetesNetworkConfig.serviceIpv4Cidr = 10.100.0.0/16 for a cluster created without serviceIpv4Cidr on a non-overlapping VPC.
Actual (emulator): 172.20.0.0/16, both on create and on the delete echo. Every other member of both responses matched.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus.
2. POST /clusters creating a cluster with resourcesVpcConfig for a VPC that does not overlap 10.100.0.0/16 and with kubernetesNetworkConfig.serviceIpv4Cidr OMITTED (fixture eks-ekscc-traj-lenient-scalar-coercion, step task#5).
3. Read cluster.kubernetesNetworkConfig.serviceIpv4Cidr in the 200 response: the emulator says 172.20.0.0/16 where AWS says 10.100.0.0/16.
4. DELETE /clusters/<name> (same fixture, step task#6) and read the same member on the 200 deletion response: the wrong block is echoed from stored state.
5. The same pair reproduces in fixture eks-ekscc-traj-reference-create-retry-and-delete at steps task#5 and task#7.
