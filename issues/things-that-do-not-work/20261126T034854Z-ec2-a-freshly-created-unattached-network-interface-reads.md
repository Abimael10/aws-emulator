Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2.network-interface-attach-detach-lifecycle ec2.ec2mr-byvpc-interface-derived-grouping-agrees-with-instance-grouping
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2.network-interface-attach-detach-lifecycle|run#21|AssertionError|assert fresh["Status"] == "available", f"a new ENI must be 'available', got {fresh['Status']!r}"
Issue-Key: ec2.ec2mr-byvpc-interface-derived-grouping-agrees-with-instance-grouping|run#17|AssertionError|assert len(unattached) == 1 and unattached[0]["Status"] == "available", (
Issue-Commit: 857f59baf94ca876ddeb888a415f04d0868c32ab

# ec2: a freshly created unattached network interface reads back Status 'pending', an off-enum value, instead of 'available'

> _Note: this issue was identified against the code base at commit `857f59baf94ca876ddeb888a415f04d0868c32ab`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateNetworkInterface

## Description
One net-new root cause (Cause 1 of 1).

Cause 1 of 1: a network interface created by CreateNetworkInterface and read back through DescribeNetworkInterfaces reports Status 'pending'. An unattached ENI must report 'available', and 'pending' is not a value of the modelled status enum at all.
AWS: Smithy: NetworkInterface$Status targets com.amazonaws.ec2#NetworkInterfaceStatus, an enum whose complete value set is [available, associated, attaching, in-use, detaching]; the member doc is only "The status of the network interface.". CreateNetworkInterface and DescribeNetworkInterfaces both declare errors: [], so the status progression itself is recorded behavior only: the recordings require that an interface created with CreateNetworkInterface(SubnetId=..., Groups=[...], Description=...) and immediately described is reported with Status 'available' and with NO Attachment member, both when fetched by NetworkInterfaceIds and when enumerated by Filters=[{Name: 'vpc-id', Values: [<vpc>]}]. 'available' is the resting state of an interface attached to nothing; 'attaching'/'in-use' only follow an AttachNetworkInterface. Since 'pending' is outside the enum's value set, no AWS response can carry it for this member.
Emulator: the first DescribeNetworkInterfaces after CreateNetworkInterface answers 200 with Status 'pending'. In ec2.network-interface-attach-detach-lifecycle the describe immediately following the create yields 'a new ENI must be \'available\', got \'pending\''; in ec2.ec2mr-byvpc-interface-derived-grouping-agrees-with-instance-grouping the same interface, enumerated later by the vpc-id filter after several other EC2 round-trips, is still reported as ('eni-...', 'pending'), so the value is not a short-lived transient that settles — it is the stored status the read path serves. (The eni id in that message is a per-run value; the requirement is the status string.)
Why: a client that waits for an interface to become usable waits on Status == 'available' before AttachNetworkInterface, a CloudFormation-style provisioner or an SDK waiter polls for exactly that value, and both hang until their own deadline on an interface that is in fact ready. Worse, the value is off-enum: SDKs that deserialise NetworkInterfaceStatus into a closed enum (or any consumer that matches exhaustively over the five modelled values) see an unknown variant for every freshly created ENI, and inventory code that groups interfaces by status silently drops them.
Fix: the CreateNetworkInterface handler's state write and the NetworkInterface projection used by DescribeNetworkInterfaces in the EC2 service — a new interface with no attachment must be stored and serialised as 'available', and the serialiser must only ever emit a value of the modelled NetworkInterfaceStatus set ('attaching'/'in-use'/'detaching' driven by the attachment lifecycle, 'available' whenever there is no attachment).
Traces: ec2.network-interface-attach-detach-lifecycle, ec2.ec2mr-byvpc-interface-derived-grouping-agrees-with-instance-grouping. Open issue 20261124T204233Z-ec2-3-net-new-causes-createnetworkinterfacepermission-grants-a.md touches the ENI surface (permissions) but neither names this status nor carries either of these replay keys, so this cause is filed here.

## Evidence
ec2.network-interface-attach-detach-lifecycle, verdict [mismatch]: AssertionError at check line 232 — "a new ENI must be 'available', got 'pending'". The sequence is CreateNetworkInterface(SubnetId=<subnet>, Description='sim2real secondary eni', TagSpecifications=[network-interface]) followed immediately by DescribeNetworkInterfaces(NetworkInterfaceIds=[<id>]); the check then asserts Status == 'available' and that no Attachment member is present, before AttachNetworkInterface.
ec2.ec2mr-byvpc-interface-derived-grouping-agrees-with-instance-grouping, verdict [mismatch]: AssertionError at check line 63 — "the unattached interface must still be enumerated by the VPC, with status 'available'; got [('eni-000000000000000cb', 'pending')]". Here the interface is read through DescribeNetworkInterfaces(Filters=[{Name: 'vpc-id', Values: [<vpc>]}]) after two instances have already been launched and described, i.e. well after the create.
Expected in both: Status 'available' (one of the five modelled NetworkInterfaceStatus values) with no Attachment. Actual in both: Status 'pending', which the Smithy enum does not define. Interface ids, subnet/vpc ids and descriptions shown are per-run values and not part of the requirement.

## Steps to Reproduce
1. ec2 CreateVpc(CidrBlock=<any /16>) and CreateSubnet(VpcId=<vpc>, CidrBlock=<a /24 inside it>).
2. ec2 CreateNetworkInterface(SubnetId=<subnet>, Description='probe') and note the NetworkInterfaceId. Do not attach it.
3. ec2 DescribeNetworkInterfaces(NetworkInterfaceIds=[<id>]): NetworkInterfaces[0].Status must be 'available' and no Attachment member may be present. The emulator answers 'pending'.
4. ec2 DescribeNetworkInterfaces(Filters=[{Name: 'vpc-id', Values: [<vpc>]}]): the same interface must still be enumerated with Status 'available'; the emulator still reports 'pending', so the value does not settle.
5. Re-replay ec2.network-interface-attach-detach-lifecycle and ec2.ec2mr-byvpc-interface-derived-grouping-agrees-with-instance-grouping to confirm.
