Issue-Source: trace-sweep-divergence
Issue-Trace: autoscaling-asgssh-traj-vpczoneidentifier-comma-string-to-private-subnets
Issue-Step: task#13
Issue-Severity: LOW
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: autoscaling-asgssh-traj-vpczoneidentifier-comma-string-to-private-subnets
Issue-Shape-Id: com.amazonaws.autoscaling#AutoScaling_2011_01_01
Issue-Key: ec2|DescribeSubnets|2xx->2xx|$.DescribeSubnetsResponse.subnetSet.item[subnetId="subnet-00000000000000096"].dhcpOptionsSource
Issue-Key: ec2|DescribeSubnets|2xx->2xx|$.DescribeSubnetsResponse.subnetSet.item[subnetId="subnet-00000000000000098"].dhcpOptionsSource
Issue-Commit: c2a51bbc379e00e6c722f42efee8d77cde344066

# EC2 DescribeSubnets omits dhcpOptionsSource on every subnet

> _Note: this issue was identified against the code base at commit `c2a51bbc379e00e6c722f42efee8d77cde344066`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeSubnets

## Description
Cause 1 of 1: DescribeSubnets never serializes the subnet's DHCP-options source.
AWS: recorded behavior only — the checked-out Smithy model's com.amazonaws.ec2#Subnet shape has no DhcpOptionsSource member, so the model says nothing about it; the recorded AWS response for this step carries dhcpOptionsSource with the value 'vpc' inside EVERY subnetSet item (the subnet inherits its DHCP option set from its VPC rather than having a subnet-level override). Status is 200 in both the recording and the emulator. Hypothesis: the element is emitted unconditionally by the live service for ordinary VPC subnets and is simply newer than the vendored model snapshot.
Emulator: each subnetSet item is serialized without any dhcpOptionsSource element (reported missing for every subnet in the response, e.g. both subnets of the probe VPC).
Why: a client that reads dhcpOptionsSource to decide whether a subnet has a subnet-level DHCP option set override finds the field absent and cannot tell 'inherits from VPC' from 'not reported', so DHCP-configuration discovery against the emulator silently differs from AWS.
Fix: the DescribeSubnets response serializer / the Subnet projection — emit dhcpOptionsSource per subnet item, defaulting to 'vpc' when the subnet has no own option set.
Traces: autoscaling-asgssh-traj-vpczoneidentifier-comma-string-to-private-subnets#task#13.

## Evidence
Replay summary: trace_set simparity-wire-aws-v1, emulator_up true, 1044 steps, 795 passed, 245 mismatched, score 76.1%.

Fixture autoscaling-asgssh-traj-vpczoneidentifier-comma-string-to-private-subnets step task#13, DescribeSubnets, expected status 200 / actual status 200:
  $.DescribeSubnetsResponse.subnetSet.item[subnetId="subnet-...096"].dhcpOptionsSource: missing (expected 'vpc', got None)
  $.DescribeSubnetsResponse.subnetSet.item[subnetId="subnet-...098"].dhcpOptionsSource: missing (expected 'vpc', got None)
No other member of the subnet projection diverged in this cluster; the cluster has no owning open issue and no blocking stub.

## Steps to Reproduce
1. git clone the repo and boot the emulator built from it.
2. Replay the wire fixture corpus (trace set simparity-wire-aws-v1).
3. Replay fixture autoscaling-asgssh-traj-vpczoneidentifier-comma-string-to-private-subnets up to step task#13, which calls ec2 DescribeSubnets for the probe VPC's subnets.
4. Inspect each <item> of <subnetSet> in the 200 response: the recording has a <dhcpOptionsSource>vpc</dhcpOptionsSource> element per subnet; the emulator's items have none.
