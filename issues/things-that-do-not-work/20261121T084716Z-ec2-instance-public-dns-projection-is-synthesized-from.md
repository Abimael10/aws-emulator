Issue-Source: trace-sweep-divergence
Issue-Trace: ec2-instance-monitoring-toggle
Issue-Step: task#3
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ec2-instance-monitoring-toggle ec2-instance-reboot-and-status-report ec2-sshp-traj-keyless-public-instance-eic-push-and-eip-swap
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2|DescribeInstances|2xx->2xx|$.DescribeInstancesResponse.reservationSet.item.instancesSet.item.networkInterfaceSet.item.publicDnsName
Issue-Key: ec2|DescribeInstances|2xx->2xx|$.DescribeInstancesResponse.reservationSet.item.instancesSet.item.networkInterfaceSet.item.publicIpDnsNameOptions.publicIpv4DnsName
Issue-Key: ec2|DescribeInstances|2xx->2xx|$.DescribeInstancesResponse.reservationSet.item.instancesSet.item.publicIpDnsNameOptions.publicIpv4DnsName
Issue-Key: ec2|RunInstances|2xx->2xx|$.RunInstancesResponse.instancesSet.item.networkInterfaceSet.item.publicDnsName
Issue-Key: ec2|RunInstances|2xx->2xx|$.RunInstancesResponse.instancesSet.item.networkInterfaceSet.item.publicIpDnsNameOptions.publicIpv4DnsName
Issue-Key: ec2|RunInstances|2xx->2xx|$.RunInstancesResponse.instancesSet.item.publicIpDnsNameOptions.publicIpv4DnsName
Issue-Related: 20261112T182924Z-ec2-instance-projections-ignore-requested-metadata-options-and.md
Issue-Related: 20261120T081949Z-ec2-dns-projections-announce-invented-hostnames-and-omit.md
Issue-Commit: 17b8d6926fcb04a314774f8dcf7dba28eb7ebe93

# ec2 instance public-DNS projection is synthesized from a stale address and emitted where AWS omits it

> _Note: this issue was identified against the code base at commit `17b8d6926fcb04a314774f8dcf7dba28eb7ebe93`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeInstances

## Description
3 causes, all in how the ec2 instance projection builds public DNS names and usage-operation metadata. The emulator announces a public DNS name derived from an address it is not reached on, keeps it after the instance's public address changes, and populates nested members AWS leaves absent.

Cause 1 of 3: DescribeInstances omits usageOperationUpdateTime and announces a public DNS name for an address that is not the instance's public address.
AWS: Smithy model com-amazonaws-ec2-amazonec2.json - Instance carries usageOperationUpdateTime (ec2QueryName UsageOperationUpdateTime), and AWS emits it for every running instance (the recording asserts it present, shape-only). Instance.dnsName / NetworkInterface.publicDnsName / publicIpDnsNameOptions.publicIpv4DnsName are the instance's PUBLIC DNS name, and AWS derives it from the instance's current public IPv4 address, as ec2-<public-ipv4-with-dots-replaced-by-dashes>.<region>.compute.amazonaws.com. The fixed text is the 'ec2-' prefix, the '.compute.amazonaws.com' suffix and the region label; the variable part is the instance's own public IPv4 address, taken from state.
Emulator: usageOperationUpdateTime is absent entirely, and every public-DNS member is filled with a name built from a fixed, emulator-internal address rather than the address the instance actually holds - the replay flags these host_not_own, i.e. the emulator announced a host it does not own.
Why: a client that reads an instance's dnsName to connect reaches nothing (the name encodes an address the instance does not have), and a client that reads usageOperationUpdateTime to know when billing metadata last changed finds the member missing and cannot distinguish 'never set' from 'not reported'.
Fix: the ec2 DescribeInstances output projection - derive every public-DNS member from the instance's associated public IPv4 in state, and populate usageOperationUpdateTime (ec2 instance serializer / instance state).
Traces: ec2-instance-monitoring-toggle#task#3 (cluster also covers ec2-instance-monitoring-toggle#task#5, ec2-instance-reboot-and-status-report#task#2, #task#3, #task#4).

Cause 2 of 3: RunInstances returns a populated public DNS name in the launch response, where AWS returns it empty/absent.
AWS: recorded behavior only - at RunInstances the instance is still pending and has no public DNS name yet, so AWS returns instancesSet.item.dnsName as the empty value and omits the nested networkInterfaceSet.item.publicDnsName, networkInterfaceSet.item.association.publicDnsName, networkInterfaceSet.item.privateIpAddressesSet.item.association.publicDnsName and publicIpDnsNameOptions.publicIpv4DnsName members altogether. The public name only appears on a later DescribeInstances once the address is associated.
Emulator: all of those members are already populated in the RunInstances response, with a name synthesized from an emulator-internal address (flagged host_not_own).
Why: a client that treats a non-empty dnsName in the RunInstances reply as 'the instance is addressable now' skips the wait-for-running/describe step AWS requires, then fails to connect; and code that branches on the presence of publicIpDnsNameOptions sees a configuration the launch never requested.
Fix: the ec2 RunInstances output projection - leave the public-DNS members empty/absent at launch and let the describe path fill them once a public address exists (ec2 RunInstances serializer).
Traces: ec2-instance-reboot-and-status-report#setup#1.

Cause 3 of 3: after an Elastic IP swap the instance's public DNS name does not follow the newly associated address, and per-private-address association names are emitted where AWS has none.
AWS: recorded behavior only - after AssociateAddress moves a new Elastic IP onto the instance, Instance.dnsName and networkInterfaceSet.item.association.publicDnsName become the name derived from the NEWLY associated public IPv4 (same ec2-<dashed-ipv4>.<region>.compute.amazonaws.com template), while networkInterfaceSet.item.publicDnsName, networkInterfaceSet.item.privateIpAddressesSet.item.association.publicDnsName and both publicIpDnsNameOptions.publicIpv4DnsName members stay absent.
Emulator: all six members carry the same stale, emulator-synthesized name; it neither tracks the association change nor respects which members AWS leaves unset.
Why: an EIP swap is exactly the operation a client performs to change where a name points; a client that re-reads DescribeInstances to pick up the new name gets the old one and keeps talking to the wrong address.
Fix: the ec2 DescribeInstances projection and the address-association state it reads - recompute the public DNS name from the current association and emit only the members AWS emits (ec2 instance/network-interface serializer, address-association state).
Traces: ec2-sshp-traj-keyless-public-instance-eic-push-and-eip-swap#task#19 (cluster also covers #task#24).

Related but NOT covering these: 20261120T081949Z-ec2-dns-projections-announce-invented-hostnames-and-omit.md describes the same family of defect but does not carry these replay keys (the replay reports these three clusters as unowned), so they must be re-replayed against this issue.

## Evidence
Cause 1 - fixture ec2-instance-monitoring-toggle, step task#3, DescribeInstances, status 200 vs 200:
  $...instancesSet.item.usageOperationUpdateTime: missing (expected present, got None)
  $...instancesSet.item.dnsName: host_not_own (got 'ec2-54-0-0-2.us-west-2.compute.amazonaws.com' - a name for an address the emulator does not own)
  same host_not_own on networkInterfaceSet.item.publicDnsName, networkInterfaceSet.item.association.publicDnsName, networkInterfaceSet.item.privateIpAddressesSet.item.association.publicDnsName, networkInterfaceSet.item.publicIpDnsNameOptions.publicIpv4DnsName, instancesSet.item.publicIpDnsNameOptions.publicIpv4DnsName.
  Cluster: 5 steps (ec2-instance-monitoring-toggle#task#3, #task#5, ec2-instance-reboot-and-status-report#task#2, #task#3, #task#4).

Cause 2 - fixture ec2-instance-reboot-and-status-report, step setup#1, RunInstances, status 200 vs 200:
  $.RunInstancesResponse.instancesSet.item.dnsName: expected '' (empty), got a populated 'ec2-...compute.amazonaws.com' name.
  $...networkInterfaceSet.item.publicDnsName / .association.publicDnsName / .privateIpAddressesSet.item.association.publicDnsName / .publicIpDnsNameOptions.publicIpv4DnsName and $...instancesSet.item.publicIpDnsNameOptions.publicIpv4DnsName: expected None (member absent), got a populated name (host_not_own).

Cause 3 - fixture ec2-sshp-traj-keyless-public-instance-eic-push-and-eip-swap, step task#19, DescribeInstances, status 200 vs 200:
  $...networkInterfaceSet.item.association.publicDnsName and $...instancesSet.item.dnsName: expected the name derived from the newly associated Elastic IP, got the emulator's stale synthesized name (host_not_own).
  $...networkInterfaceSet.item.publicDnsName, $...networkInterfaceSet.item.privateIpAddressesSet.item.association.publicDnsName, $...networkInterfaceSet.item.publicIpDnsNameOptions.publicIpv4DnsName, $...instancesSet.item.publicIpDnsNameOptions.publicIpv4DnsName: expected None (absent), got a populated name.
  Cluster also covers #task#24.

## Steps to Reproduce
1. Replay fixture ec2-instance-reboot-and-status-report, step setup#1: RunInstances for a public-subnet instance and compare the instancesSet.item public-DNS members against the recording (expected empty/absent).
2. Replay fixture ec2-instance-monitoring-toggle, step task#3 (and task#5): DescribeInstances on that instance and check that usageOperationUpdateTime is present and that every public-DNS member encodes the instance's actual public IPv4.
3. Replay fixture ec2-sshp-traj-keyless-public-instance-eic-push-and-eip-swap through the EIP swap, then step task#19 (and task#24): DescribeInstances must report dnsName and networkInterfaceSet.item.association.publicDnsName derived from the newly associated Elastic IP, with the other public-DNS members absent.
