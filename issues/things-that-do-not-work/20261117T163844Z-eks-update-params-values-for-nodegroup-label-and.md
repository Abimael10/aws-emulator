Issue-Source: trace-sweep-divergence
Issue-Trace: eks-node-group-tags-labels-and-taints-at-create-tag-round-trip-label-and-taint-updates-and-not-found
Issue-Step: task#23
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: eks-node-group-tags-labels-and-taints-at-create-tag-round-trip-label-and-taint-updates-and-not-found eks-cluster-provider-explicit-off-switches-logging-update-tags-and-node-group-held-at-zero
Issue-Shape-Id: com.amazonaws.eks#AWSWesleyFrontend
Issue-Key: eks|UpdateNodegroupConfig|2xx->2xx|$.update.createdAt
Issue-Key: eks|UpdateNodegroupConfig|2xx->2xx|$.update.errors
Issue-Key: eks|UpdateNodegroupConfig|2xx->2xx|$.update.params[*].value
Issue-Related: 20261117T163844Z-eks-cluster-plane-responses-diverge-default-serviceipv4cidr-oidc.md
Issue-Commit: 81e21ca007588719de802b4dd67d83498ccb7823

# eks Update.params values for nodegroup label and taint updates are not JSON-encoded, and Update.errors is omitted (2 causes)

> _Note: this issue was identified against the code base at commit `81e21ca007588719de802b4dd67d83498ccb7823`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateNodegroupConfig

## Description
UpdateNodegroupConfig and the DescribeUpdate that reads the resulting update record serve an Update object whose params values use an emulator-invented text encoding, and whose errors member is dropped. Two independent causes.

Cause 1 of 2 - update params carry a human-readable string instead of the JSON text of the submitted value.
AWS: Smithy UpdateParam.value is a String and UpdateParamType includes LabelsToAdd, LabelsToRemove, TaintsToAdd and TaintsToRemove. AWS fills value with the JSON text of the value submitted in the request: a LabelsToRemove param carries a JSON array of label keys ('["<key>", ...]'), and a TaintsToAdd/TaintsToRemove param carries a JSON array of taint objects with the Taint members key, value and effect ('[{"effect":"<EFFECT>","value":"<value>","key":"<key>"}, ...]'), effect being a TaintEffect enum value.
Emulator: writes its own flat encoding - a bare label key ('key1') instead of '["key1"]', and a comma-joined kubectl-style taint string ('<key>=<value>:<EFFECT>,<key>=<value>:<EFFECT>') instead of the JSON array of taint objects. The same wrong value is then replayed by DescribeUpdate from the persisted update record.
Why: a client that json-parses update.params[*].value to confirm which labels/taints an asynchronous update applied (the only machine-readable record of the requested change) gets a parse error or, worse, treats the flat string as a single label key.
Fix: the UpdateNodegroupConfig handler's update-param construction and the update record it persists for DescribeUpdate - serialize each param value as the JSON text of the submitted shape.
Traces: eks-node-group-tags-labels-and-taints-at-create-tag-round-trip-label-and-taint-updates-and-not-found#task#23 (LabelsToRemove), #task#26 (TaintsToAdd/TaintsToRemove), #task#27 (DescribeUpdate replays the same wrong values).

Cause 2 of 2 - Update.errors is omitted where AWS returns an empty list.
AWS: Smithy Update.errors targets the ErrorDetails list ("Any errors associated with a Failed update"); the recording shows every successful/in-progress nodegroup update carrying errors [].
Emulator: omits the member (missing, expected [], got None) on both UpdateNodegroupConfig and DescribeUpdate.
Why: a client that inspects update.errors without a null guard before deciding whether an update failed raises on the emulator but not against AWS.
Fix: the update serializer - emit the empty list rather than dropping the member.
Traces: eks-node-group-tags-labels-and-taints-at-create-tag-round-trip-label-and-taint-updates-and-not-found#task#23, #task#26, #task#27; eks-cluster-provider-explicit-off-switches-logging-update-tags-and-node-group-held-at-zero#task#23 (UpdateNodegroupConfig) and #task#24 (DescribeUpdate), where a scalingConfig-only update diverges on this cause alone.

The same steps additionally carry the epoch-timestamp precision defect on update.createdAt; that is the shared serializer cause filed as cause 3 of "eks cluster-plane responses diverge: default serviceIpv4Cidr, OIDC issuer host, omitted empty lists, tag status and ClusterLogging order", and is not re-counted here.

## Evidence
UpdateNodegroupConfig removing a label (eks-node-group-tags...#task#23, status 200->200): $.update.params[0].value: expected '["key1"]', got 'key1'; $.update.errors: missing (expected [], got None).
UpdateNodegroupConfig updating taints (#task#26) and DescribeUpdate (#task#27): $.update.params[0].value: expected '[{"effect":"NO_EXECUTE","value":"value1updated","key":"key1"},{"effect":"NO_SCHEDULE","value":"value2","key":"key2"}]', got 'key1=value1updated:NO_EXECUTE,key2=value2:NO_SCHEDULE'; $.update.params[1].value: expected '[{"effect":"NO_SCHEDULE","value":"value1","key":"key1"}]', got 'key1=value1:NO_SCHEDULE'.
UpdateNodegroupConfig/DescribeUpdate for a scalingConfig change (eks-cluster-provider...#task#23, #task#24): $.update.errors: missing (expected [], got None).
The literal key/value names above come from the request; the requirement is the JSON encoding and member set, not these names.

## Steps to Reproduce
1. Boot the emulator and replay fixture eks-node-group-tags-labels-and-taints-at-create-tag-round-trip-label-and-taint-updates-and-not-found.
2. Call UpdateNodegroupConfig with labels.removeLabels=["key1"] (task#23) and read update.params: the LabelsToRemove value is the bare string 'key1' instead of the JSON array, and update.errors is absent.
3. Call UpdateNodegroupConfig with taints.addOrUpdateTaints / removeTaints (task#26), then DescribeUpdate on the returned update id (task#27): both report the taint params as 'key=value:EFFECT' comma-joined text instead of a JSON array of {key,value,effect} objects.
4. Replay eks-cluster-provider-explicit-off-switches-logging-update-tags-and-node-group-held-at-zero#task#23 and #task#24 (a scalingConfig-only update) to see the omitted update.errors on its own.
