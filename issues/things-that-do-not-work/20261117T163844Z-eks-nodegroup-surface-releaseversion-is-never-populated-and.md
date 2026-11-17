Issue-Source: trace-sweep-divergence
Issue-Trace: eks-cluster-provider-explicit-off-switches-logging-update-tags-and-node-group-held-at-zero
Issue-Step: task#21
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: eks-cluster-provider-explicit-off-switches-logging-update-tags-and-node-group-held-at-zero eks-node-group-tags-labels-and-taints-at-create-tag-round-trip-label-and-taint-updates-and-not-found
Issue-Shape-Id: com.amazonaws.eks#AWSWesleyFrontend
Issue-Key: eks|CreateNodegroup|2xx->2xx|$.nodegroup.createdAt
Issue-Key: eks|CreateNodegroup|2xx->2xx|$.nodegroup.labels
Issue-Key: eks|CreateNodegroup|2xx->2xx|$.nodegroup.modifiedAt
Issue-Key: eks|CreateNodegroup|2xx->2xx|$.nodegroup.releaseVersion
Issue-Key: eks|CreateNodegroup|2xx->2xx|$.nodegroup.resources
Issue-Key: eks|CreateNodegroup|2xx->2xx|$.nodegroup.taints
Issue-Key: eks|DescribeNodegroup|2xx->2xx|$.nodegroup.createdAt
Issue-Key: eks|DescribeNodegroup|2xx->2xx|$.nodegroup.modifiedAt
Issue-Key: eks|DescribeNodegroup|2xx->2xx|$.nodegroup.releaseVersion
Issue-Key: eks|DescribeNodegroup|2xx->2xx|$.nodegroup.taints
Issue-Key: eks|DeleteNodegroup|2xx->2xx|$.nodegroup.createdAt
Issue-Key: eks|DeleteNodegroup|2xx->2xx|$.nodegroup.modifiedAt
Issue-Key: eks|DeleteNodegroup|2xx->2xx|$.nodegroup.releaseVersion
Issue-Key: eks|DeleteNodegroup|2xx->2xx|$.nodegroup.taints
Issue-Commit: 81e21ca007588719de802b4dd67d83498ccb7823

# eks nodegroup surface: releaseVersion is never populated and empty labels/taints/resources are serialized where AWS omits them (2 causes)

> _Note: this issue was identified against the code base at commit `81e21ca007588719de802b4dd67d83498ccb7823`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateNodegroup

## Description
Every managed-nodegroup response the emulator serves (CreateNodegroup, DescribeNodegroup, DeleteNodegroup) diverges from the recorded AWS nodegroup object in two independent ways. Both must be closed before the affected steps replay clean.

Cause 1 of 2 - nodegroup.releaseVersion is never populated.
AWS: Smithy Nodegroup.releaseVersion is a String member; for a node group created without a launch-template custom AMI it carries the AMI release version EKS resolved for the group (the recording shows a value of the form '<kubernetesVersion>.<patch>-<YYYYMMDD>', e.g. the cluster's Kubernetes version followed by the AMI build date). It is present on CreateNodegroup, DescribeNodegroup and DeleteNodegroup responses.
Emulator: omits the member entirely (releaseVersion: missing, got None) on all three operations, both for a group created with an explicit version and for one created with defaults.
Why: a client that pins or reconciles AMI releases - comparing nodegroup.releaseVersion against DescribeAddonVersions/AMI release data, or passing it back into UpdateNodegroupVersion - reads null and cannot perform the upgrade decision at all.
Fix: the CreateNodegroup handler's nodegroup state (resolve and store a release version) and the nodegroup serializer shared by DescribeNodegroup/DeleteNodegroup.
Traces: eks-cluster-provider-explicit-off-switches-logging-update-tags-and-node-group-held-at-zero#task#21 (CreateNodegroup), #task#22 (DescribeNodegroup), #task#26 (DeleteNodegroup); eks-node-group-tags-labels-and-taints-at-create-tag-round-trip-label-and-taint-updates-and-not-found#task#11 (CreateNodegroup), #task#12 (DescribeNodegroup), #task#30 (DeleteNodegroup).

Cause 2 of 2 - empty collections and a synthesized resources block are emitted where AWS omits the member.
AWS: Smithy Nodegroup.labels (labelsMap), Nodegroup.taints (taintsList) and Nodegroup.resources (NodegroupResources with autoScalingGroups) are all optional. In the recording, a node group created with no labels and no taints comes back with those members ABSENT, and resources is absent while the group is still being provisioned (the Auto Scaling group is reported only once EKS has created it).
Emulator: always serializes labels as {}, taints as [] and resources as {"autoScalingGroups":[{"name":"eks-<nodegroup>-<uuid>"}]} immediately at CreateNodegroup, and keeps taints [] on DescribeNodegroup/DeleteNodegroup for a group that has none.
Why: a client that uses member presence to decide whether EKS has recorded labels/taints or has finished provisioning the backing ASG - the common "if resources: ..." pattern - sees a provisioned ASG that does not exist and treats an untainted group as explicitly empty.
Fix: the nodegroup serializer (omit empty labels/taints) and the CreateNodegroup handler's state, which must not publish a resources block before the backing ASG exists.
Traces: eks-cluster-provider-explicit-off-switches-logging-update-tags-and-node-group-held-at-zero#task#21 (labels {}, taints [], resources synthesized), #task#22 and #task#26 (taints []); eks-node-group-tags-labels-and-taints-at-create-tag-round-trip-label-and-taint-updates-and-not-found#task#11 (resources synthesized).

The same steps additionally carry the epoch-timestamp precision defect (nodegroup.createdAt / modifiedAt emitted as a 7-fractional-digit float); that is one shared serializer cause, filed as cause 3 of the issue "eks cluster-plane responses diverge: default serviceIpv4Cidr, OIDC issuer host, omitted empty lists, tag status and ClusterLogging order", and is not re-counted here.

## Evidence
CreateNodegroup (eks-cluster-provider...#task#21, status 200->200): $.nodegroup.releaseVersion: missing (expected '<shaped-not-asserted>', got None); $.nodegroup.labels: type_mismatch (expected None, got {}); $.nodegroup.taints: type_mismatch (expected None, got []); $.nodegroup.resources: type_mismatch (expected None, got {'autoScalingGroups': [{'name': 'eks-sim2real-probe-eks-ng-7812e8f9-c23a10f2-e545-352d-a613-7dfca6aa2e88'}]}).
CreateNodegroup (eks-node-group-tags...#task#11): $.nodegroup.releaseVersion: missing (expected '1.37.1-20261112', got None); $.nodegroup.resources: type_mismatch (expected None, got {'autoScalingGroups': [{'name': 'eks-sim2real-probe-eksng-ng-...'}]}).
DescribeNodegroup (eks-cluster-provider...#task#22, 2 steps; eks-node-group-tags...#task#12, 4 steps): releaseVersion missing; taints type_mismatch (expected None, got []).
DeleteNodegroup (eks-cluster-provider...#task#26; eks-node-group-tags...#task#30): releaseVersion missing; taints type_mismatch (expected None, got []).
The generated ASG name and the recorded release-version date are per-environment values; the requirement is that the member is present (releaseVersion) or absent (labels/taints/resources) as AWS serves it.

## Steps to Reproduce
1. Boot the emulator and replay fixtures eks-cluster-provider-explicit-off-switches-logging-update-tags-and-node-group-held-at-zero and eks-node-group-tags-labels-and-taints-at-create-tag-round-trip-label-and-taint-updates-and-not-found.
2. CreateNodegroup on an existing cluster with no labels and no taints (eks-cluster-provider...#task#21): the response has no releaseVersion, but does carry labels {}, taints [] and a resources.autoScalingGroups entry.
3. DescribeNodegroup the same group (#task#22) and then DeleteNodegroup it (#task#26): releaseVersion is still absent and taints is still [].
4. Repeat with the node-group fixture's CreateNodegroup (#task#11), DescribeNodegroup (#task#12) and DeleteNodegroup (#task#30) to see the same two causes on a group created with labels and taints.
