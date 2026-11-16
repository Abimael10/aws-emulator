Issue-Source: trace-sweep-divergence
Issue-Trace: ec2-accept-vpc-peering-connection-lifecycle
Issue-Step: task#6
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ec2-accept-vpc-peering-connection-lifecycle ec2-asgssh-nacl-entries-round-trip-deny-22-before-allow-all ec2-network-acl-create-configure-entries-delete ec2-network-acl-entries-lifecycle ec2-network-acl-lifecycle ec2-describe-nat-gateways-pagination ec2-pip-traj-instance-private-ip-reverse-lookup ec2-pip-traj-secondary-ip-request-mistakes ec2-t12-eni-attach-and-private-ip ec2-madl-traj-vpc-subnet-discovery-filters ec2-subnets-subnet-ids-and-empty-list ec2-ipam-trajwire-read-side-mistakes ec2-ipam-trajwire-reference-path-regional-child-pool
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2|AcceptVpcPeeringConnection|2xx->2xx|$.AcceptVpcPeeringConnectionResponse.vpcPeeringConnection.accepterVpcInfo.peeringOptions
Issue-Key: ec2|AcceptVpcPeeringConnection|2xx->2xx|$.AcceptVpcPeeringConnectionResponse.vpcPeeringConnection.requesterVpcInfo.peeringOptions
Issue-Key: ec2|AcceptVpcPeeringConnection|2xx->2xx|$.AcceptVpcPeeringConnectionResponse.vpcPeeringConnection.status.code
Issue-Key: ec2|AcceptVpcPeeringConnection|2xx->2xx|$.AcceptVpcPeeringConnectionResponse.vpcPeeringConnection.status.message
Issue-Key: ec2|CreateNetworkAcl|2xx->2xx|$.CreateNetworkAclResponse.networkAcl.entrySet.item[*]
Issue-Key: ec2|CreateNetworkAcl|2xx->2xx|$.CreateNetworkAclResponse.networkAcl.entrySet.item[*].icmpTypeCode
Issue-Key: ec2|CreateNetworkAcl|2xx->2xx|$.CreateNetworkAclResponse.networkAcl.entrySet.item[*].portRange
Issue-Key: ec2|DescribeNatGateways|2xx->2xx|$.DescribeNatGatewaysResponse.natGatewaySet.item[subnetId="subnet-00000000000000096"].attachedApplianceSet
Issue-Key: ec2|DescribeNatGateways|2xx->2xx|$.DescribeNatGatewaysResponse.natGatewaySet.item[subnetId="subnet-00000000000000098"].attachedApplianceSet
Issue-Key: ec2|DescribeNatGateways|2xx->2xx|$.DescribeNatGatewaysResponse.natGatewaySet.item[subnetId="subnet-0000000000000009a"].attachedApplianceSet
Issue-Key: ec2|DescribeNatGateways|2xx->2xx|$.DescribeNatGatewaysResponse.natGatewaySet.item[subnetId="subnet-0000000000000009c"].attachedApplianceSet
Issue-Key: ec2|DescribeNatGateways|2xx->2xx|$.DescribeNatGatewaysResponse.natGatewaySet.item[subnetId="subnet-0000000000000009e"].attachedApplianceSet
Issue-Key: ec2|DescribeNatGateways|2xx->2xx|$.DescribeNatGatewaysResponse.natGatewaySet.item[subnetId="subnet-000000000000000a0"].attachedApplianceSet
Issue-Key: ec2|DescribeNatGateways|2xx->2xx|$.DescribeNatGatewaysResponse.natGatewaySet.item[subnetId="subnet-000000000000000a2"].attachedApplianceSet
Issue-Key: ec2|AssignPrivateIpAddresses|2xx->2xx|$.AssignPrivateIpAddressesResponse.assignedIpv4PrefixSet
Issue-Key: ec2|AssignPrivateIpAddresses|2xx->2xx|$.AssignPrivateIpAddressesResponse.return
Issue-Key: ec2|DescribeSubnets|2xx->2xx|$.DescribeSubnetsResponse.subnetSet.item[subnetId="subnet-00000000000000096"].dhcpOptionsId
Issue-Key: ec2|DescribeSubnets|2xx->2xx|$.DescribeSubnetsResponse.subnetSet.item[subnetId="subnet-00000000000000098"].dhcpOptionsId
Issue-Key: ec2|GetIpamPoolAllocations|2xx->2xx|$.GetIpamPoolAllocationsResponse.ipamPoolAllocationSet.item.allocationId
Issue-Key: ec2|GetIpamPoolAllocations|2xx->2xx|$.GetIpamPoolAllocationsResponse.ipamPoolAllocationSet.item.cidrBlock
Issue-Key: ec2|GetIpamPoolAllocations|2xx->2xx|$.GetIpamPoolAllocationsResponse.ipamPoolAllocationSet.item.ipamPoolAllocationId
Issue-Key: ec2|GetIpamPoolAllocations|2xx->2xx|$.GetIpamPoolAllocationsResponse.ipamPoolAllocationSet.item.status
Issue-Key: ec2|GetIpamPoolAllocations|2xx->2xx|$.GetIpamPoolAllocationsResponse.ipamPoolAllocationSet.item.tagSet
Issue-Related: 20261115T223640Z-ec2-describesubnets-omits-dhcpoptionssource-on-every-subnet.md
Issue-Commit: 0771e25bba8212ca0e4339575fc1faa7cfb6765d

# ec2 responses omit members AWS always emits in describe and create output

> _Note: this issue was identified against the code base at commit `0771e25bba8212ca0e4339575fc1faa7cfb6765d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
AcceptVpcPeeringConnection

## Description
Six EC2 read/create paths answer HTTP 200 with a body that is missing members AWS emits on every call, including empty container elements that AWS always writes. This bundle carries 8 net-new divergence clusters covering 6 causes; none is owned by an open issue. They share one fault: the response builders populate only the members the emulator happens to track and skip the always-present ones.

GENERAL FAULT
AWS: the ec2Query response for these operations contains every modeled member the resource has, and writes an empty element (for example <tagSet/>, <portRange/>) for a container that is present but empty - a client that reads the member unconditionally always finds it.
Emulator: the member is absent from the XML entirely, or (for peering) the sub-structure is dropped.
Why: clients and SDK deserializers that read these members unconditionally see null where AWS gives a value or an empty collection; code that drives follow-up calls off them (the DHCP options set of a subnet, a peering connection's DNS-resolution options, the assignment result of AssignPrivateIpAddresses, an IPAM allocation's CIDR and status) either crashes or silently skips the step.
Fix: the response-building/serializer paths of the operations named below, plus the state that should carry the values (the peering status transition, the subnet's inherited DHCP options, the IPAM allocation record).

Cause 1 of 6: CreateNetworkAcl - the two default deny-all entries must carry an empty icmpTypeCode and an empty portRange element. Smithy: NetworkAclEntry.IcmpTypeCode (xmlName icmpTypeCode) and NetworkAclEntry.PortRange (xmlName portRange). Emulator emits each entry with only cidrBlock/egress/protocol/ruleAction/ruleNumber, so the comparator sees the whole item differ. Traces: ec2-asgssh-nacl-entries-round-trip-deny-22-before-allow-all#task#2; ec2-network-acl-create-configure-entries-delete#setup#2 (second cluster, also covering ec2-network-acl-entries-lifecycle#task#2 and ec2-network-acl-lifecycle#task#2).

Cause 2 of 6: DescribeNatGateways - every natGatewaySet item must carry attachedApplianceSet, empty when no appliance is attached. Smithy: NatGateway.AttachedAppliances (xmlName attachedApplianceSet). Emulator omits it on all seven gateways in the page. Traces: ec2-describe-nat-gateways-pagination#task#20.

Cause 3 of 6: AssignPrivateIpAddresses - the success body must carry return='true' and an empty assignedIpv4PrefixSet. Smithy: AssignPrivateIpAddressesResult models AssignedIpv4Prefixes (xmlName assignedIpv4PrefixSet); the return flag is recorded behavior only (the on-disk model does not list it, the recordings show AWS emitting it). Emulator emits neither. Traces: ec2-pip-traj-instance-private-ip-reverse-lookup#task#13 (cluster also covers ec2-pip-traj-secondary-ip-request-mistakes#task#9, ec2-t12-eni-attach-and-private-ip#task#10 and #task#12).

Cause 4 of 6: DescribeSubnets - each subnet must report the DHCP options set it inherits: dhcpOptionsId (the VPC's dopt- id) and dhcpOptionsSource 'vpc'. Recorded behavior only - the on-disk Smithy Subnet shape carries neither member, and the recordings show AWS emitting both for every subnet. Emulator omits both. Traces: ec2-madl-traj-vpc-subnet-discovery-filters#task#8; ec2-subnets-subnet-ids-and-empty-list#task#4 (second cluster).

Cause 5 of 6: AcceptVpcPeeringConnection - requesterVpcInfo and accepterVpcInfo must each carry peeringOptions with allowDnsResolutionFromRemoteVpc, allowEgressFromLocalClassicLinkToRemoteVpc and allowEgressFromLocalVpcToRemoteClassicLink all 'false', and the status immediately after accepting is code 'provisioning' / message 'Provisioning' before it settles to active. Smithy: VpcPeeringConnectionVpcInfo.PeeringOptions -> VpcPeeringConnectionOptionsDescription; VpcPeeringConnectionStateReason.Code/.Message. Emulator drops both peeringOptions blocks and reports code 'active' / message 'Active' straight away. (The sibling CreateVpcPeeringConnection cluster of the same fixture is owned by 20261101T141251Z-ec2-peering-id-format-prefix-list-and-capacity.md; the Accept step below is not owned by it or any other open issue.) Traces: ec2-accept-vpc-peering-connection-lifecycle#task#6.

Cause 6 of 6: GetIpamPoolAllocations - each ipamPoolAllocationSet item must carry allocationId, cidrBlock, status 'active' and tagSet (empty when untagged), and the allocation id is 'ipam-pool-alloc-' followed by 32 lowercase hex characters. Smithy models IpamPoolAllocation.Cidr/IpamPoolAllocationId/Tags; allocationId, cidrBlock and status as named in the recording are recorded behavior only. Emulator emits only an ipamPoolAllocationId built from its sequential counter ('ipam-pool-alloc-' + a 17-digit number) and nothing else, so a poll on status never succeeds. Traces: ec2-ipam-trajwire-read-side-mistakes#task#11 (cluster also covers ec2-ipam-trajwire-reference-path-regional-child-pool#task#9).

## Evidence
All eight clusters: expected status 200, actual status 200, verdict mismatch - the divergence is in the body.
Cause 1 (ec2-asgssh-nacl-entries-round-trip-deny-22-before-allow-all#task#2): $.CreateNetworkAclResponse.networkAcl.entrySet.item[0] expected {'ruleNumber':'32767','protocol':'-1','ruleAction':'deny','egress':'true','cidrBlock':'0.0.0.0/0','icmpTypeCode':'','portRange':''}, got the same item without icmpTypeCode and portRange. Second cluster (ec2-network-acl-create-configure-entries-delete#setup#2): item[0].icmpTypeCode expected '', got None; item[0].portRange expected '', got None (same for item[1]).
Cause 2 (ec2-describe-nat-gateways-pagination#task#20): natGatewaySet.item[*].attachedApplianceSet expected '', got None for all seven items.
Cause 3 (ec2-pip-traj-instance-private-ip-reverse-lookup#task#13): $.AssignPrivateIpAddressesResponse.return expected 'true', got None; .assignedIpv4PrefixSet expected '', got None.
Cause 4 (ec2-madl-traj-vpc-subnet-discovery-filters#task#8): subnetSet.item[*].dhcpOptionsId expected a dopt- id, got None; .dhcpOptionsSource expected 'vpc', got None. Same pair in ec2-subnets-subnet-ids-and-empty-list#task#4.
Cause 5 (ec2-accept-vpc-peering-connection-lifecycle#task#6): requesterVpcInfo.peeringOptions and accepterVpcInfo.peeringOptions expected {'allowEgressFromLocalClassicLinkToRemoteVpc':'false','allowEgressFromLocalVpcToRemoteClassicLink':'false','allowDnsResolutionFromRemoteVpc':'false'}, got None; status.code expected 'provisioning', got 'active'; status.message expected 'Provisioning', got 'Active'.
Cause 6 (ec2-ipam-trajwire-read-side-mistakes#task#11): ipamPoolAllocationSet.item.allocationId / .cidrBlock / .status / .tagSet expected present ('active', the allocated CIDR, an empty tagSet), got None; .ipamPoolAllocationId expected the 'ipam-pool-alloc-' + 32-hex form, got 'ipam-pool-alloc-' + a sequential 17-digit counter; the fixture's poll on status gave up after 30 attempts.
(Concrete ids and CIDRs above are this recording's values; the requirement is that the members are present and shaped as described.)

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus simparity-wire-aws-v1.
2. Replay ec2-accept-vpc-peering-connection-lifecycle step task#6 and compare the vpcPeeringConnection block (peeringOptions on both vpcInfo members, status.code/message) with the recording.
3. Replay ec2-asgssh-nacl-entries-round-trip-deny-22-before-allow-all#task#2 and ec2-network-acl-create-configure-entries-delete#setup#2 (CreateNetworkAcl default entries), ec2-describe-nat-gateways-pagination#task#20 (attachedApplianceSet), ec2-pip-traj-instance-private-ip-reverse-lookup#task#13 (return / assignedIpv4PrefixSet), ec2-madl-traj-vpc-subnet-discovery-filters#task#8 and ec2-subnets-subnet-ids-and-empty-list#task#4 (dhcpOptionsId / dhcpOptionsSource) and ec2-ipam-trajwire-read-side-mistakes#task#11 (allocation members).
4. Each returns 200 with the listed members absent.
