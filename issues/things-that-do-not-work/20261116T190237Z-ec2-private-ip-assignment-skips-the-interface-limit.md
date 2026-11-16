Issue-Source: trace-sweep-divergence
Issue-Trace: ec2-pip-traj-instance-private-ip-reverse-lookup
Issue-Step: task#11
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ec2-pip-traj-instance-private-ip-reverse-lookup
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2|AssignPrivateIpAddresses|4xx->2xx|$.Response
Issue-Key: ec2|AssignPrivateIpAddresses|4xx->2xx|status
Issue-Key: ec2|DescribeInstances|2xx->2xx|$.DescribeInstancesResponse.reservationSet.item.instancesSet.item.networkInterfaceSet.item.privateIpAddressesSet.item[*].privateDnsName
Issue-Key: ec2|DescribeNetworkInterfaces|2xx->2xx|$.DescribeNetworkInterfacesResponse.networkInterfaceSet.item.privateIpAddressesSet.item[*]
Issue-Key: ec2|DescribeNetworkInterfaces|2xx->2xx|$.DescribeNetworkInterfacesResponse.networkInterfaceSet.item.privateIpAddressesSet.item[*].privateIpAddress
Issue-Related: 20261103T123451Z-ec2-describe-projections-omit-modeled-response-members.md
Issue-Commit: 0771e25bba8212ca0e4339575fc1faa7cfb6765d

# ec2 private IP assignment skips the interface limit and invents DNS names

> _Note: this issue was identified against the code base at commit `0771e25bba8212ca0e4339575fc1faa7cfb6765d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
AssignPrivateIpAddresses

## Description
The emulator's secondary-private-IP model diverges from AWS in three ways on one trajectory: it never enforces the per-interface IPv4 address limit, it fabricates reverse DNS names for secondary addresses, and because the over-limit assignment is wrongly accepted the interface's address set afterwards does not match AWS. This bundle carries 3 net-new divergence clusters, none owned by an open issue.

GENERAL FAULT
AWS: the number of private IPv4 addresses an ENI may hold is fixed by the instance type (networkInfo.ipv4AddressesPerInterface), an assignment that would exceed it fails with 400 PrivateIpAddressLimitExceeded and no address is added; and only the primary private address of an interface has a privateDnsName - secondary addresses are returned with an empty privateDnsName.
Emulator: the limit is not checked (the call succeeds with 200 and the address is added), and every address in privateIpAddressesSet gets a synthesized ip-<a>-<b>-<c>-<d>.<region>.compute.internal name.
Why: a client probing capacity sees a success where AWS fails, so it over-commits an interface and its subsequent describe output disagrees with AWS; reverse-lookup logic that treats a non-empty privateDnsName as 'this address has a hostname' resolves names AWS never publishes.
Fix: AssignPrivateIpAddresses' validation (limit check against the instance type's ipv4AddressesPerInterface before mutating state) and the network-interface/instance serializers that fill privateDnsName per address.

Cause 1 of 3: AssignPrivateIpAddresses past the interface's address limit. AWS: 400 with Code 'PrivateIpAddressLimitExceeded' and Message 'Number of private addresses will exceed limit.' (fixed text, no request values). Emulator: 200, and the address is retained. Traces: ec2-pip-traj-instance-private-ip-reverse-lookup#task#11.

Cause 2 of 3: DescribeInstances - secondary entries of networkInterfaceSet.item.privateIpAddressesSet must have an empty privateDnsName. Emulator fills in the derived internal hostname. Traces: ec2-pip-traj-instance-private-ip-reverse-lookup#task#5 (cluster also covers task#7).

Cause 3 of 3: DescribeNetworkInterfaces after the assignment sequence - AWS shows exactly one secondary address, the one the caller requested, because the over-limit request in Cause 1 was rejected; the emulator shows the auto-assigned address still in slot 1 and the requested one as an extra slot 2, both with synthesized privateDnsName values. This is the state consequence of Cause 1 plus Cause 2, listed separately because it is its own divergence cluster. Traces: ec2-pip-traj-instance-private-ip-reverse-lookup#task#14.

## Evidence
Cause 1 (ec2-pip-traj-instance-private-ip-reverse-lookup#task#11): expected status 400, actual 200; expected body $.Response.Errors.Error = {'Code':'PrivateIpAddressLimitExceeded','Message':'Number of private addresses will exceed limit.'}, got no error document at all.
Cause 2 (same fixture#task#5): expected status 200, actual 200; $.DescribeInstancesResponse.reservationSet.item.instancesSet.item.networkInterfaceSet.item.privateIpAddressesSet.item[1].privateDnsName expected '', got 'ip-10-238-1-11.us-west-2.compute.internal'.
Cause 3 (same fixture#task#14): expected status 200, actual 200; privateIpAddressesSet.item[1].privateIpAddress expected '10.238.1.12', got '10.238.1.11'; item[1].privateDnsName expected '', got 'ip-10-238-1-11.us-west-2.compute.internal'; item[2] expected absent, got {'primary':'false','privateDnsName':'ip-10-238-1-12.us-west-2.compute.internal','privateIpAddress':'10.238.1.12'}.
(The 10.238.1.x addresses are this recording's values; the requirements are the limit rejection, the empty privateDnsName on secondaries and the address set that follows from them.)

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus simparity-wire-aws-v1.
2. Replay ec2-pip-traj-instance-private-ip-reverse-lookup step task#11 - AssignPrivateIpAddresses that would push the ENI past its instance type's ipv4AddressesPerInterface: the emulator answers 200 instead of 400 PrivateIpAddressLimitExceeded.
3. Replay step task#5 of the same fixture and read privateIpAddressesSet on the instance's network interface: the secondary address carries a synthesized privateDnsName.
4. Replay step task#14 and compare DescribeNetworkInterfaces' privateIpAddressesSet with the recording: the emulator keeps the address its 200 wrongly granted and adds an extra entry.
