Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2.srcdest-check-enforcement-gates-appliance-forwarded-traffic ec2.dpeer-bidirectional-traffic-needs-the-peer-route-on-both-sides
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2.srcdest-check-enforcement-gates-appliance-forwarded-traffic|_arms#6|AssertionError|assert f"{_VIP}/32" in boot["lo_addrs"], (
Issue-Key: ec2.dpeer-bidirectional-traffic-needs-the-peer-route-on-both-sides|_poll#5|Timeout|time.sleep(interval_s)
Issue-Commit: 724276c7155ec07ee34cf1e38d33e217c3831042

# ec2: 2 net-new causes — an appliance guest's loopback/`ip addr` reads back empty so SourceDestCheck enforcement is unobservable, and the two-sided VPC-peering data-plane check never settles inside its budget

> _Note: this issue was identified against the code base at commit `724276c7155ec07ee34cf1e38d33e217c3831042`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ModifyInstanceAttribute

## Description
Two net-new root causes in the EC2 data plane. Neither is marked ALREADY FILED or OWNED in the replay report.

Cause 1 of 2: an appliance instance's guest reports nothing for `ip addr`, so a loopback-held virtual address never exists and SourceDestCheck enforcement can never be exercised.
  AWS: recorded behavior only — the in-guest shell and `ip addr` output are not Smithy-modelled. Smithy: EC2 `ModifyInstanceAttribute` carries member `SourceDestCheck` (AttributeBooleanValue) and `DescribeInstances`/`DescribeNetworkInterfaces` expose `SourceDestCheck`; AWS documents (https://docs.aws.amazon.com/vpc/latest/userguide/VPC_NAT_Instance.html) that with SourceDestCheck disabled the VPC forwards packets whose destination is not an address of the sending ENI, and with it enabled such packets are silently DROPPED. The recording's precondition is that a forwarder guest, after being configured through Run Command, holds the virtual address as a /32 on `lo` and that `ip addr` in that guest prints it.
  Emulator: `ip addr` in the forwarder guest returns the empty string, so the virtual address is absent from loopback. The check fails its precondition: "the forwarder guest does not hold 192.0.2.10/32 on loopback; `ip addr` reported '', so it could not answer a packet for it even if one arrived". The address and port here are the check's own values; the requirement is that whatever address the guest was configured to hold is reported back by `ip addr`, and that a guest command's stdout is the command's real output rather than an empty string.
  Why: a client cannot build or validate any NAT-instance / firewall-appliance topology. Both arms of SourceDestCheck (on → drop, off → forward) become indistinguishable because the appliance can never be made to answer for a non-ENI address, and the on/off toggle is therefore untested even though the control-plane attribute round-trips.
  Fix: the EC2 instance guest runtime — the component that executes in-guest commands and captures their stdout — plus the VPC router's source/destination-check enforcement that depends on it.
  Traces: ec2.srcdest-check-enforcement-gates-appliance-forwarded-traffic.

Cause 2 of 2: the two-sided VPC-peering data-plane flow never settles — the check exhausts its whole budget without reaching a verdict.
  AWS: recorded behavior only for timing. Smithy: EC2 `CreateVpcPeeringConnection` → `AcceptVpcPeeringConnection` moves `VpcPeeringConnection.Status.Code` `pending-acceptance` → `active`; `CreateRoute`/`DeleteRoute` with `VpcPeeringConnectionId` install and remove the peer route; AWS's rule is that TCP crosses the peering only while BOTH route tables hold the peer route, and a route on one side only carries nothing in either direction. The recording's sequence is: build both sides, accept the peering, poll for `active`, launch a guest per side, poll `DescribeInstanceInformation` for `PingStatus == Online`, then probe both directions.
  Emulator: no verdict is produced at all — the replay reports "Error: Check timed out after 60.0s". The emulator never advances one of the polled states (peering `active`, instance running, or agent `Online`) to the value the flow waits on, so the peering data-plane assertions are never reached. Sibling checks on this emulator return within the same budget, so the stall is the emulator's state machine not converging, not the budget.
  Why: a client that creates and accepts a peering and then waits for it (or for its instances' agents) to become usable waits forever; peering is unusable as a data path and no route-symmetry behavior can be relied on.
  Fix: whichever EC2 state transition the flow polls never reaches its terminal value — the VPC-peering status progression in the peering handler/state and the instance + SSM-agent readiness progression in the instance state machine.
  Traces: ec2.dpeer-bidirectional-traffic-needs-the-peer-route-on-both-sides.

Note: the open issues 20261115T003554Z-ec2-guests-observe-nothing-in-guest-commands-exit.md and 20261111T133150Z-ec2-4-net-new-causes-resources-leak-across.md describe neighbouring guest/peering defects, but the replay report does not mark either of these two checks as owned by them.

## Evidence
Cause 1 — ec2.srcdest-check-enforcement-gates-appliance-forwarded-traffic [mismatch]
  Expected: the forwarder guest, after its configuration command, holds the appliance virtual address as a /32 on `lo`, and `ip addr` in that guest prints that address; with SourceDestCheck=true the VPC then DROPS a probe for it (curl exit 28) while a control probe to the ENI's own address is REFUSED by the guest kernel (curl exit 7); with SourceDestCheck=false the identical probe reaches the appliance (HTTP 200).
  Actual: `ip addr` in the forwarder guest returned '' — no address at all. AssertionError: "the forwarder guest does not hold 192.0.2.10/32 on loopback; `ip addr` reported '', so it could not answer a packet for it even if one arrived". The check aborts in its setup arm, so neither SourceDestCheck arm is ever measured.

Cause 2 — ec2.dpeer-bidirectional-traffic-needs-the-peer-route-on-both-sides [mismatch]
  Expected: CreateVpcPeeringConnection reaches Status.Code 'pending-acceptance', AcceptVpcPeeringConnection drives it to 'active', both guests come up with SSM PingStatus 'Online', and then traffic crosses only while both route tables hold the peer route.
  Actual: "Error: Check timed out after 60.0s" — the replay produced no assertion at all; some polled state never reaches its expected value, so the flow never gets past its build phase.

## Steps to Reproduce
Cause 1: create a VPC and subnet, launch an m5.large AL2023 instance with an SSM instance profile, wait for PingStatus Online, then use SSM AWS-RunShellScript to add a /32 address outside the VPC CIDR (e.g. 192.0.2.10/32) to `lo` and start a listener on it. Run `ip addr` in the guest through Run Command and read its stdout: the emulator returns an empty string instead of the interface listing containing that /32. Then set ModifyInstanceAttribute SourceDestCheck=false and probe the virtual address from a second guest in the same subnet — the appliance cannot answer.

Cause 2: create two VPCs (10.61.0.0/16, 10.62.0.0/16) each with a subnet, an internet gateway and a route table; CreateVpcPeeringConnection from A to B; poll DescribeVpcPeeringConnections for Status.Code 'pending-acceptance'; AcceptVpcPeeringConnection; poll for 'active'; CreateRoute for the peer CIDR on both route tables; launch one AL2023 t3.micro per side with an SSM instance profile; poll DescribeInstanceInformation for PingStatus 'Online' on both. The sequence never completes — the replay aborts on the suite's time budget with no verdict.
