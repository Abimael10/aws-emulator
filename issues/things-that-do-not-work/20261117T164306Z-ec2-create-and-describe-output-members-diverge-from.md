Issue-Source: trace-sweep-divergence
Issue-Trace: ec2-nat-gateway-explicit-public-connectivity-tag-change-and-remove-last-then-deleted
Issue-Step: task#6
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ec2-nat-gateway-explicit-public-connectivity-tag-change-and-remove-last-then-deleted ec2-interface-vpc-endpoint-without-subnets-and-private-dns-off-reads-back-and-not-found ec2-traffic-mirror-filter-lifecycle
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2|CreateNatGateway|2xx->2xx|$.CreateNatGatewayResponse.natGateway.attachedApplianceSet
Issue-Key: ec2|DescribeNatGateways|2xx->2xx|$.DescribeNatGatewaysResponse.natGatewaySet.item.natGatewayAddressSet.item.allocationId
Issue-Key: ec2|DescribeNatGateways|2xx->2xx|$.DescribeNatGatewaysResponse.natGatewaySet.item.natGatewayAddressSet.item.associationId
Issue-Key: ec2|DescribeNatGateways|2xx->2xx|$.DescribeNatGatewaysResponse.natGatewaySet.item.natGatewayAddressSet.item.publicIp
Issue-Key: ec2|CreateVpcEndpoint|2xx->2xx|$.CreateVpcEndpointResponse.vpcEndpoint.dnsEntrySet.item.dnsName
Issue-Key: ec2|DescribeVpcEndpoints|2xx->2xx|$.DescribeVpcEndpointsResponse.vpcEndpointSet.item.dnsEntrySet.item.dnsName
Issue-Key: ec2|DescribeTrafficMirrorFilters|2xx->2xx|$.DescribeTrafficMirrorFiltersResponse.trafficMirrorFilterSet.item.ingressFilterRuleSet.item.ruleAction
Issue-Related: 20261030T000411Z-ec2-mutations-are-not-observable-reads-after-modify.md
Issue-Related: 20261105T144252Z-ec2-route-table-and-vpc-endpoint-lifecycle-operations.md
Issue-Related: 20261106T072955Z-ec2-nat-gateway-create-and-describe-paths-report.md
Issue-Commit: 81e21ca007588719de802b4dd67d83498ccb7823

# ec2 create and describe output members diverge from AWS for NAT gateways, interface endpoints and traffic mirror filters

> _Note: this issue was identified against the code base at commit `81e21ca007588719de802b4dd67d83498ccb7823`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateNatGateway

## Description
5 causes, one shared fault family: EC2 create/describe replies carry a different set of members than AWS - some members AWS always emits are missing, and some the emulator synthesizes are not in AWS's output at all (including a DNS name the emulator does not serve).

Cause 1 of 5: CreateNatGateway omits attachedApplianceSet
AWS: Smithy: NatGateway.AttachedAppliances -> xmlName `attachedApplianceSet` / ec2QueryName AttachedApplianceSet, target NatGatewayAttachedApplianceList ("The proxy appliances attached to the NAT Gateway..."). Recorded response: HTTP 200 with `natGateway.attachedApplianceSet` present as an empty element.
Emulator: HTTP 200 with no `attachedApplianceSet` member at all.
Why: a client that reads AttachedAppliances off the create result (SDKs that require the member to be present to materialize the field, and schema-validating proxies) sees a NAT gateway shape AWS never returns.
Fix: the CreateNatGateway response serializer in the EC2 crate.
Traces: ec2-nat-gateway-explicit-public-connectivity-tag-change-and-remove-last-then-deleted#task#6

Cause 2 of 5: DescribeNatGateways omits attachedApplianceSet and the Elastic-IP members of natGatewayAddressSet
AWS: Smithy: NatGateway.NatGatewayAddresses -> xmlName `natGatewayAddressSet`, items NatGatewayAddress whose `allocationId`, `publicIp` and `associationId` are documented as "[Public NAT gateway only]" - i.e. a public NAT gateway reports all three. Plus NatGateway.AttachedAppliances as above. Recorded response: HTTP 200 with attachedApplianceSet present (empty) and the address item carrying allocationId, associationId and publicIp.
Emulator: HTTP 200 with `attachedApplianceSet` missing and the address item missing `allocationId`, `associationId` and `publicIp` entirely.
Why: a public NAT gateway reads back with no Elastic IP attached - a client cannot learn which EIP allocation backs the gateway, cannot release it, and cannot correlate the gateway with its address association.
Fix: the DescribeNatGateways handler/response serializer and the NAT-gateway address state in the EC2 crate - carry the allocation and association ids assigned at create time.
Traces: ec2-nat-gateway-explicit-public-connectivity-tag-change-and-remove-last-then-deleted#task#26

Cause 3 of 5: CreateVpcEndpoint invents a dnsEntrySet for an interface endpoint created with no subnets and private DNS disabled
AWS: Smithy: VpcEndpoint.DnsEntries -> xmlName `dnsEntrySet`, documented "(Interface endpoint) The DNS entries for the endpoint"; DNS entries exist only for the endpoint's interfaces/private hosted zone. Recorded response: HTTP 200 with `vpcEndpoint.dnsEntrySet` as an empty element, because the endpoint has no subnets and PrivateDnsEnabled=false.
Emulator: HTTP 200 with a fabricated entry - `dnsEntrySet.item.dnsName` is a vpce-*.<service>.<region>.vpce.amazonaws.com host (flagged host_not_own: an address the emulator does not serve) plus a synthesized hostedZoneId.
Why: a client resolves/connects to the announced DNS name and reaches nothing; worse, it believes private DNS is in effect for an endpoint explicitly created with it off.
Fix: the CreateVpcEndpoint handler in the EC2 crate - derive DnsEntries from the endpoint's actual interfaces and private-DNS setting, and emit none when there are none.
Traces: ec2-interface-vpc-endpoint-without-subnets-and-private-dns-off-reads-back-and-not-found#task#2

Cause 4 of 5: DescribeVpcEndpoints reports the same invented dnsEntrySet on read-back
AWS: as cause 3; recorded response HTTP 200 with `vpcEndpointSet.item.dnsEntrySet` empty.
Emulator: HTTP 200 with the same fabricated dnsName/hostedZoneId pair on the describe path (host_not_own).
Why: the fabrication is not confined to the create reply - any inventory read of the endpoint hands clients an unreachable hostname.
Fix: the DescribeVpcEndpoints handler / the VPC-endpoint state the describe path projects.
Traces: ec2-interface-vpc-endpoint-without-subnets-and-private-dns-off-reads-back-and-not-found#task#3

Cause 5 of 5: DescribeTrafficMirrorFilters loses the rule action and the network services of the filter
AWS: Smithy: TrafficMirrorFilter.IngressFilterRules -> `ingressFilterRuleSet` (items carry `ruleAction`, from the required RuleAction of CreateTrafficMirrorFilterRule, enum TrafficMirrorRuleAction) and TrafficMirrorFilter.NetworkServices -> `networkServiceSet` (set by ModifyTrafficMirrorFilterNetworkServices). Recorded response: HTTP 200 with ingressFilterRuleSet.item.ruleAction='reject' and networkServiceSet.item='amazon-dns'.
Emulator: HTTP 200 with ruleAction='accept' (the request's 'reject' is not retained) and `networkServiceSet` serialized as an empty element (the added amazon-dns network service is lost).
Why: a mirror filter that a client configured to reject traffic reads back as accepting it, and the DNS network-service opt-in disappears - the filter's effective policy is reported as the opposite of what was requested.
Fix: the CreateTrafficMirrorFilterRule / ModifyTrafficMirrorFilterNetworkServices state and the DescribeTrafficMirrorFilters projection in the EC2 crate.
Traces: ec2-traffic-mirror-filter-lifecycle#task#6

Related open issues that do NOT own these steps (the replay reports no owner): 20261117T072456Z-ec2-vpc-endpoint-dnsentryset-announces-hostnames-the-emulator.md and 20261116T190237Z-ec2-responses-omit-members-aws-always-emits-in.md.

## Evidence
Replay of trace set simparity-wire-aws-v1 (service ec2), verdict=mismatch, expected_status 200 / actual_status 200:
- CreateNatGateway, fixture ec2-nat-gateway-explicit-public-connectivity-tag-change-and-remove-last-then-deleted step task#6: $.CreateNatGatewayResponse.natGateway.attachedApplianceSet: missing (expected '', got None)
- DescribeNatGateways, same fixture task#26: natGatewaySet.item.attachedApplianceSet: missing (expected '', got None); natGatewayAddressSet.item.allocationId: missing (expected 'eipalloc-000000000000000a8', got None); natGatewayAddressSet.item.associationId: missing (expected '<shaped-not-asserted>', got None); natGatewayAddressSet.item.publicIp: missing (expected '54.0.0.2', got None)
- CreateVpcEndpoint, fixture ec2-interface-vpc-endpoint-without-subnets-and-private-dns-off-reads-back-and-not-found step task#2: vpcEndpoint.dnsEntrySet: type_mismatch (expected '', got {'item': {'dnsName': 'vpce-...-xqyxtd8n.ec2.us-west-2.vpce.amazonaws.com', 'hostedZoneId': 'Z0000000000097'}}); dnsEntrySet.item.dnsName: host_not_own
- DescribeVpcEndpoints, same fixture task#3: vpcEndpointSet.item.dnsEntrySet: type_mismatch (expected '', got the same item); dnsEntrySet.item.dnsName: host_not_own
- DescribeTrafficMirrorFilters, fixture ec2-traffic-mirror-filter-lifecycle step task#6: ingressFilterRuleSet.item.ruleAction: value_mismatch (expected 'reject', got 'accept'); networkServiceSet: type_mismatch (expected {'item': 'amazon-dns'}, got '')
(The allocation id, public IP, hosted-zone id and generated host above are this recording's values; the requirement is which members are present and that announced names resolve to the served endpoint.)

## Steps to Reproduce
Causes 1-2:
1. AllocateAddress, then CreateNatGateway with ConnectivityType=public and that AllocationId.
2. Inspect the create reply -> no attachedApplianceSet member (AWS emits an empty one).
3. DescribeNatGateways -> no attachedApplianceSet, and natGatewayAddressSet.item has no allocationId/associationId/publicIp.
Causes 3-4:
4. CreateVpcEndpoint of type Interface with no SubnetIds and PrivateDnsEnabled=false.
5. Inspect the create reply and then DescribeVpcEndpoints -> both carry a dnsEntrySet item with a vpce-*.vpce.amazonaws.com dnsName the emulator does not serve; AWS returns an empty dnsEntrySet.
Cause 5:
6. CreateTrafficMirrorFilter; CreateTrafficMirrorFilterRule with TrafficDirection=ingress, RuleAction=reject; ModifyTrafficMirrorFilterNetworkServices adding amazon-dns.
7. DescribeTrafficMirrorFilters -> ruleAction is 'accept' and networkServiceSet is empty.
Or replay the fixture steps listed under Traces.
