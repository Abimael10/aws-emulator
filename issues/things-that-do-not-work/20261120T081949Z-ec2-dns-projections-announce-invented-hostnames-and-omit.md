Issue-Source: trace-sweep-divergence
Issue-Trace: ec2-instance-monitoring-toggle
Issue-Step: task#3
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ec2-instance-monitoring-toggle ec2-instance-reboot-and-status-report ec2-sshp-traj-keyless-public-instance-eic-push-and-eip-swap ec2-interface-vpc-endpoint-without-subnets-and-private-dns-off-reads-back-and-not-found
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2|DescribeInstances|2xx->2xx|$.DescribeInstancesResponse.reservationSet.item.instancesSet.item.networkInterfaceSet.item.privateIpAddressesSet.item.association.publicDnsName
Issue-Key: ec2|RunInstances|2xx->2xx|$.RunInstancesResponse.instancesSet.item.networkInterfaceSet.item.privateIpAddressesSet.item.association.publicDnsName
Issue-Key: ec2|CreateVpcEndpoint|2xx->2xx|$.CreateVpcEndpointResponse.vpcEndpoint.dnsEntrySet
Issue-Key: ec2|DescribeVpcEndpoints|2xx->2xx|$.DescribeVpcEndpointsResponse.vpcEndpointSet.item.dnsEntrySet
Issue-Related: 20261031T181451Z-ec2-instance-volume-snapshot-and-iam-profile-projections.md
Issue-Related: 20261112T182924Z-ec2-instance-projections-ignore-requested-metadata-options-and.md
Issue-Commit: 58a84f52bf4284d464bddc60006b86eef897b096

# EC2 DNS projections announce invented hostnames and omit the public-DNS members AWS emits

> _Note: this issue was identified against the code base at commit `58a84f52bf4284d464bddc60006b86eef897b096`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeInstances

## Description
Five net-new causes with one shared fault: the emulator's DNS projections are synthesized from a fixed address rather than from the resource's real state, so it announces hostnames it does not serve, emits them where AWS emits nothing, and omits the public-DNS members AWS always includes.

=== Cause 1 of 5: DescribeInstances omits the public-IP DNS members and emits a constant public DNS name ===
AWS: recorded behavior only - the EC2 model (services/service-com-amazonaws-ec2-amazonec2/smithy-model/com-amazonaws-ec2-amazonec2.json) gives Instance the members dnsName (PublicDnsName), publicIpDnsNameOptions (dnsHostnameType + publicIpv4DnsName), usageOperationUpdateTime, and gives InstanceNetworkInterface both publicDnsName and publicIpDnsNameOptions; AWS populates all of them for a running instance with a public IPv4 address, and the public DNS name is DERIVED from that address (ec2-<a-b-c-d>.<region>.compute.amazonaws.com).
Emulator: publicIpDnsNameOptions (on the instance and on each network interface), the interface's publicDnsName and usageOperationUpdateTime are all absent, and every public DNS name it does emit is the constant 'ec2-54-0-0-2.us-west-2.compute.amazonaws.com' - a host in AWS's own namespace that the emulator does not serve and that does not correspond to the instance's address.
Why: a client that resolves or displays PublicDnsName is handed a name that resolves to someone else's address (or not at all), and code reading publicIpDnsNameOptions/usageOperationUpdateTime finds the members missing entirely rather than populated.
Fix: the DescribeInstances projection and the instance-state DNS derivation - services/service-com-amazonaws-ec2-amazonec2/src/ops/describe_instances.rs plus the shared instance projection/state that computes DNS names: derive the name from the instance's current public address, serve a host the emulator actually answers for, and emit the modeled members.
Traces: ec2-instance-monitoring-toggle#task#3 (cluster also covers ec2-instance-monitoring-toggle#task#5 and ec2-instance-reboot-and-status-report#task#2..#task#4).

=== Cause 2 of 5: RunInstances emits a public DNS name for an instance that has none yet ===
AWS: recorded behavior only - in the RunInstances reply the instance is still 'pending' and has no public address, so dnsName is EMPTY and the network interface's association carries no publicDnsName at all.
Emulator: the RunInstances reply already carries dnsName 'ec2-54-0-0-2.us-west-2.compute.amazonaws.com' and repeats it on networkInterfaceSet.item.association.publicDnsName and on privateIpAddressesSet.item.association.publicDnsName, where AWS emits nothing.
Why: a caller that treats a non-empty dnsName in the launch reply as "the instance is reachable at this name" proceeds immediately against a name that is neither assigned nor served.
Fix: the launch path's response projection - services/service-com-amazonaws-ec2-amazonec2/src/ops/run_instances.rs: leave dnsName empty and omit association publicDnsName until a public address is actually associated.
Traces: ec2-instance-reboot-and-status-report#setup#1.

=== Cause 3 of 5: the public DNS name does not follow the associated Elastic IP ===
AWS: recorded behavior only - after AssociateAddress the instance's dnsName and its interface association publicDnsName are re-derived from the NEW public address (the recording shows the name derived from the associated EIP), and the per-private-address association carries no publicDnsName.
Emulator: both still report the constant 'ec2-54-0-0-2.us-west-2.compute.amazonaws.com' after the EIP swap, and it additionally appears on privateIpAddressesSet.item.association.publicDnsName where AWS emits nothing.
Why: an EIP swap is invisible in the DNS projection, so a client that re-reads the instance to learn the new name keeps the stale/invented one.
Fix: same derivation as cause 1, driven by address association state - the AssociateAddress/DisassociateAddress state update plus the instance projection.
Traces: ec2-sshp-traj-keyless-public-instance-eic-push-and-eip-swap#task#19 (cluster also covers #task#24).

=== Cause 4 of 5: CreateVpcEndpoint returns DNS entries for an endpoint that has none ===
AWS: recorded behavior only - the model gives VpcEndpoint the member dnsEntrySet (list of DnsEntry{dnsName, hostedZoneId}). An interface endpoint created with NO subnets and PrivateDnsEnabled=false has no interface and no private hosted zone, so AWS returns an EMPTY dnsEntrySet.
Emulator: dnsEntrySet carries one entry whose dnsName is a '<vpce-id>-<suffix>.ec2.<region>.vpce.localhost' host - both a non-empty set where AWS has none and a name outside AWS's namespace.
Why: a client wiring a service client to the endpoint's first DNS entry is handed a hostname that exists nowhere; and the presence of an entry contradicts the endpoint's own configuration (no subnets, private DNS off).
Fix: the create path's projection - services/service-com-amazonaws-ec2-amazonec2/src/ops/create_vpc_endpoint.rs: derive dnsEntrySet from the endpoint's network interfaces and private-DNS setting, leaving it empty when there are none. The open issue 20261117T072456Z-ec2-vpc-endpoint-dnsentryset-announces-hostnames-the-emulator.md covers invented endpoint hostnames generally but carries no replay key for these steps.
Traces: ec2-interface-vpc-endpoint-without-subnets-and-private-dns-off-reads-back-and-not-found#task#2.

=== Cause 5 of 5: DescribeVpcEndpoints repeats the phantom DNS entry on read-back ===
AWS: recorded behavior only - the read-back of the same endpoint also shows an empty dnsEntrySet.
Emulator: DescribeVpcEndpoints returns the same invented '<vpce-id>-<suffix>.ec2.<region>.vpce.localhost' entry, so the defect is in the stored/derived endpoint state, not only in the create reply.
Why: the invented entry is durable, so every later read of the endpoint repeats it.
Fix: services/service-com-amazonaws-ec2-amazonec2/src/ops/describe_vpc_endpoints.rs and the shared endpoint projection/state.
Traces: ec2-interface-vpc-endpoint-without-subnets-and-private-dns-off-reads-back-and-not-found#task#3.

## Evidence
Cause 1 - fixture ec2-instance-monitoring-toggle step task#3 (DescribeInstances, 200 vs 200):
  instancesSet.item.publicIpDnsNameOptions: missing (expected {'dnsHostnameType': 'public-ipv4-dns-name', 'publicIpv4DnsName': <shaped>})
  instancesSet.item.networkInterfaceSet.item.publicDnsName: missing
  instancesSet.item.networkInterfaceSet.item.publicIpDnsNameOptions: missing
  instancesSet.item.usageOperationUpdateTime: missing
  instancesSet.item.dnsName / networkInterfaceSet.item.association.publicDnsName / privateIpAddressesSet.item.association.publicDnsName: host_not_own, got 'ec2-54-0-0-2.us-west-2.compute.amazonaws.com'
  (affected: ec2-instance-monitoring-toggle#task#3, #task#5, ec2-instance-reboot-and-status-report#task#2, #task#3, #task#4)

Cause 2 - fixture ec2-instance-reboot-and-status-report step setup#1 (RunInstances, 200 vs 200):
  instancesSet.item.dnsName: expected '', got 'ec2-54-0-0-2.us-west-2.compute.amazonaws.com'
  networkInterfaceSet.item.association.publicDnsName: expected None, got 'ec2-54-0-0-2...' (host_not_own)
  privateIpAddressesSet.item.association.publicDnsName: expected None, got 'ec2-54-0-0-2...' (host_not_own)

Cause 3 - fixture ec2-sshp-traj-keyless-public-instance-eic-push-and-eip-swap step task#19 (DescribeInstances, 200 vs 200):
  dnsName: expected the name derived from the associated EIP, got 'ec2-54-0-0-2.us-west-2.compute.amazonaws.com' (host_not_own)
  networkInterfaceSet.item.association.publicDnsName: same divergence; privateIpAddressesSet.item.association.publicDnsName: expected None, got the same constant

Cause 4 - fixture ec2-interface-vpc-endpoint-without-subnets-and-private-dns-off-reads-back-and-not-found step task#2 (CreateVpcEndpoint, 200 vs 200):
  vpcEndpoint.dnsEntrySet: type_mismatch - expected '' (empty), got {'item': {'dnsName': 'vpce-...-....ec2.us-west-2.vpce.localhost', 'hostedZoneId': 'Z...'}}

Cause 5 - same fixture step task#3 (DescribeVpcEndpoints, 200 vs 200):
  vpcEndpointSet.item.dnsEntrySet: type_mismatch - expected '' (empty), got the same invented entry

## Steps to Reproduce
Cause 1/3: RunInstances a public-subnet instance, then DescribeInstances: expect publicIpDnsNameOptions, networkInterfaceSet.item.publicDnsName and usageOperationUpdateTime present, and dnsName derived from the instance's current public address; observe the members missing and a constant 'ec2-54-0-0-2.us-west-2.compute.amazonaws.com'. AssociateAddress a new EIP and re-read: the name does not change.
Cause 2: RunInstances and read the launch reply: expect dnsName '' and no association publicDnsName; observe the constant name in all three places.
Cause 4/5: CreateVpcEndpoint(VpcEndpointType=Interface, SubnetIds=[], PrivateDnsEnabled=false), then DescribeVpcEndpoints: expect an empty dnsEntrySet in both replies; observe one entry naming a '*.vpce.localhost' host.
