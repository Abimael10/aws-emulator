Issue-Source: trace-sweep-divergence
Issue-Trace: ec2-sshp-traj-nacl-entry-authoring-mistakes
Issue-Step: task#11
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ec2-sshp-traj-nacl-entry-authoring-mistakes ec2-describe-nat-gateways-pagination ec2-xrg-traj-instance-id-is-region-scoped
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2|DescribeNetworkAcls|2xx->2xx|$.DescribeNetworkAclsResponse.networkAclSet.item.entrySet.item[*]
Issue-Key: ec2|DescribeNatGateways|2xx->2xx|$.DescribeNatGatewaysResponse.natGatewaySet.item[subnetId="subnet-000000000000000aa"].attachedApplianceSet
Issue-Key: ec2|DescribeNatGateways|2xx->2xx|$.DescribeNatGatewaysResponse.natGatewaySet.item[subnetId="subnet-000000000000000ac"].attachedApplianceSet
Issue-Key: ec2|DescribeNatGateways|2xx->2xx|$.DescribeNatGatewaysResponse.natGatewaySet.item[subnetId="subnet-000000000000000ae"].attachedApplianceSet
Issue-Key: ec2|DescribeNatGateways|2xx->2xx|$.DescribeNatGatewaysResponse.natGatewaySet.item[subnetId="subnet-000000000000000b0"].attachedApplianceSet
Issue-Key: ec2|DescribeNatGateways|2xx->2xx|$.DescribeNatGatewaysResponse.natGatewaySet.item[subnetId="subnet-000000000000000b2"].attachedApplianceSet
Issue-Key: ec2|DescribeNatGateways|2xx->2xx|$.DescribeNatGatewaysResponse.natGatewaySet.item[subnetId="subnet-000000000000000b4"].attachedApplianceSet
Issue-Key: ec2|DescribeNatGateways|2xx->2xx|$.DescribeNatGatewaysResponse.natGatewaySet.item[subnetId="subnet-000000000000000b6"].attachedApplianceSet
Issue-Key: ec2|DescribeInstances|2xx->2xx|$.DescribeInstancesResponse.reservationSet.item.instancesSet.item.imageId
Issue-Commit: 58a84f52bf4284d464bddc60006b86eef897b096

# EC2 read paths report resource state that does not match what was created

> _Note: this issue was identified against the code base at commit `58a84f52bf4284d464bddc60006b86eef897b096`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeNetworkAcls

## Description
Three net-new causes sharing one fault: an EC2 read path answers 200 with a resource view that does not reflect the request that created or modified the resource, or omits a list member AWS always emits.

=== Cause 1 of 3: a network ACL entry keeps the wrong rule number ===
AWS: the model (services/service-com-amazonaws-ec2-amazonec2/smithy-model/com-amazonaws-ec2-amazonec2.json) declares NetworkAclEntry.RuleNumber (xmlName ruleNumber) and documents "The rule number for the entry. ACL entries are processed in ascending order by rule number." After the recorded authoring sequence the entrySet contains the entry with ruleNumber 90 (the number the last successful CreateNetworkAclEntry/ReplaceNetworkAclEntry call asked for), with protocol 6, ruleAction deny, egress false, cidrBlock 0.0.0.0/0 and portRange 22-22.
Emulator: the same entry is reported with ruleNumber 100 - the number from the earlier call in the sequence - while every other member matches. The requested rule number is therefore not applied (or an existing entry is reused instead of being re-keyed).
Why: rule number IS the evaluation order and the entry's identity in EC2; a client that authors rule 90 to take precedence, or that later deletes/replaces rule 90, operates on an ACL whose entries are numbered differently from what it asked for - the resulting filtering order is wrong and DeleteNetworkAclEntry(90) will not find it.
Fix: the network-ACL entry state and its authoring handlers - services/service-com-amazonaws-ec2-amazonec2/src/ops/create_network_acl_entry.rs / replace_network_acl_entry.rs and the stored entry list keyed by (ruleNumber, egress) that services/service-com-amazonaws-ec2-amazonec2/src/ops/describe_network_acls.rs projects.
Traces: ec2-sshp-traj-nacl-entry-authoring-mistakes#task#11.

=== Cause 2 of 3: DescribeNatGateways omits attachedApplianceSet ===
AWS: the model declares NatGateway.AttachedAppliances (xmlName attachedApplianceSet, list NatGatewayAttachedApplianceList, "The proxy appliances attached to the NAT Gateway..."). AWS emits the element for every NAT gateway, EMPTY when no appliance is attached - the recording shows an empty attachedApplianceSet on each of the seven gateways in the page.
Emulator: the member is absent from every natGatewaySet item.
Why: EC2's ec2Query XML distinguishes "present but empty list" from "member absent"; SDK consumers and diffing clients that read nat_gateway.AttachedAppliances get a missing member instead of an empty list, so round-tripping and shape assertions fail.
Fix: the NAT gateway response projection - services/service-com-amazonaws-ec2-amazonec2/src/ops/describe_nat_gateways.rs (and the shared NatGateway serializer) must always emit attachedApplianceSet. The open issue 20261117T164306Z-ec2-create-and-describe-output-members-diverge-from.md covers other NAT-gateway output members but carries no replay key for this step.
Traces: ec2-describe-nat-gateways-pagination#task#20.

=== Cause 3 of 3: DescribeInstances does not report the AMI the instance was launched from ===
AWS: recorded behavior only - Instance.imageId is the ImageId the launch actually used, echoed verbatim for the life of the instance (the recording shows the AMI id the RunInstances request named).
Emulator: imageId is one of the emulator's own synthetic catalog ids ('ami-0000000000000...') rather than the requested AMI, so the launch input is lost in the instance's state.
Why: a client that re-reads the instance to learn its AMI (to re-launch, to tag by image, or to assert the right image was used) is told the instance runs an image it never asked for; the id also does not resolve in DescribeImages the same way the requested one does.
Fix: the launch path's state - services/service-com-amazonaws-ec2-amazonec2/src/ops/run_instances.rs must store the request's (or the launch template's) ImageId on the instance, and services/service-com-amazonaws-ec2-amazonec2/src/ops/describe_instances.rs must project it unchanged.
Traces: ec2-xrg-traj-instance-id-is-region-scoped#task#8.

## Evidence
Cause 1 - fixture ec2-sshp-traj-nacl-entry-authoring-mistakes step task#11 (DescribeNetworkAcls, 200 vs 200):
  networkAclSet.item.entrySet.item[1]: expected {'ruleNumber': '90', 'protocol': '6', 'ruleAction': 'deny', 'egress': 'false', 'cidrBlock': '0.0.0.0/0', 'portRange': {'from': '22', 'to': '22'}}, got the identical entry with 'ruleNumber': '100'

Cause 2 - fixture ec2-describe-nat-gateways-pagination step task#20 (DescribeNatGateways, 200 vs 200):
  natGatewaySet.item[subnetId="subnet-...aa"].attachedApplianceSet: missing (expected '')
  ... the same missing member on all seven items in the page (subnet-...aa, ...ac, ...ae, ...b0, ...b2, ...b4, ...b6)

Cause 3 - fixture ec2-xrg-traj-instance-id-is-region-scoped step task#8 (DescribeInstances, 200 vs 200):
  reservationSet.item.instancesSet.item.imageId: expected the AMI id the launch named ('ami-04b938437b69a2c9c' in the recording), got the emulator's synthetic 'ami-0000000000000004d'

## Steps to Reproduce
Cause 1: create a network ACL, CreateNetworkAclEntry rule 100 (deny tcp 22 from 0.0.0.0/0), then author the intended entry with rule number 90 as the fixture's step sequence does, and DescribeNetworkAcls: expect an entry with ruleNumber 90; observe ruleNumber 100.
Cause 2: create NAT gateways and call DescribeNatGateways (paginated): expect every natGatewaySet item to carry an empty attachedApplianceSet element; observe the member absent.
Cause 3: RunInstances with an explicit ImageId, then DescribeInstances: expect instancesSet.item.imageId to equal the requested AMI id; observe an emulator-generated ami- id.
