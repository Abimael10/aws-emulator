Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__ec2.cfn-ec2-subnet-route-table-association-replace-deletes-first-despite-retain-then-delete cloudformation__ec2.deploy-ec2-eip-network-interface-association-round-trip-and-delete cloudformation__ec2.deploy-ec2-network-interface-attachment-delete-on-termination-round-trip-and-delete
Issue-Key: cloudformation__ec2.cfn-ec2-subnet-route-table-association-replace-deletes-first-despite-retain-then-delete|run#26|AssertionError|assert rows == want_rows, (
Issue-Key: cloudformation__ec2.deploy-ec2-eip-network-interface-association-round-trip-and-delete|run#23|KeyError|assert addr["NetworkInterfaceId"] == eni_id, (
Issue-Key: cloudformation__ec2.deploy-ec2-network-interface-attachment-delete-on-termination-round-trip-and-delete|run#12|KeyError|attachment = eni["Attachment"]
Issue-Commit: 9af7726e852eadd1ad5f5bb12067c11cebe7515a

# cloudformation__ec2: 3 net-new causes — a delete-first replacement emits the wrong stack events, an EIPAssociation never shows on the Address, and a NetworkInterfaceAttachment never shows on the ENI

> _Note: this issue was identified against the code base at commit `9af7726e852eadd1ad5f5bb12067c11cebe7515a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeStackEvents

## Description
Three independent net-new root causes in the cloudformation__ec2 surface, grounded in three failing behavioral checks. Each cause must be closed separately.

=== Cause 1 of 3: a replacement that must delete the old resource first emits the wrong ResourceStatusReason, an empty PhysicalResourceId and UPDATE_* instead of CREATE_* events ===

AWS: recorded behavior only — no Smithy trait governs stack-event text; `StackEvent` members `ResourceStatus`, `ResourceStatusReason` and `PhysicalResourceId` are plain members of the DescribeStackEvents output. For an `AWS::EC2::SubnetRouteTableAssociation` whose SubnetId changes (a replacement a resource of this type cannot create before deleting, because one subnet may hold only one explicit association), AWS emits, for that logical resource, exactly:
  1. ResourceStatus `UPDATE_IN_PROGRESS`, no ResourceStatusReason, PhysicalResourceId = the OLD association id
  2. `DELETE_IN_PROGRESS`, ResourceStatusReason `Requested update requires the replacement of the existing resource; deleting existing resource, then creating a new one.`, PhysicalResourceId = OLD id
  3. `DELETE_COMPLETE`, ResourceStatusReason `Resource <old-physical-id> delete complete`, PhysicalResourceId = OLD id
  4. `CREATE_IN_PROGRESS`, ResourceStatusReason `Resource creation Initiated`, PhysicalResourceId = the NEW association id
  5. `CREATE_COMPLETE`, no ResourceStatusReason, PhysicalResourceId = NEW id
  6. `UPDATE_COMPLETE`, no ResourceStatusReason, PhysicalResourceId = NEW id
Fixed text in (2) is the whole sentence; in (3) the fixed text is `Resource ` + ` delete complete` around the old physical id taken from state; in (4) the whole string `Resource creation Initiated`. The physical ids are values from state, not requirements.

Emulator: the delete-before-create ORDER is right, but five of the six records diverge — it emits `UPDATE_IN_PROGRESS` with reason `Requested update requires the creation of a new physical resource; hence creating one.` (AWS's wording for the OTHER replacement variant, create-before-delete) and an EMPTY PhysicalResourceId; `DELETE_IN_PROGRESS` and `DELETE_COMPLETE` with NO ResourceStatusReason at all; and reports the new resource's creation as `UPDATE_IN_PROGRESS` / `UPDATE_COMPLETE`, so the `CREATE_IN_PROGRESS` and `CREATE_COMPLETE` records never exist.

Why: stack events are the only record of what a stack update did. A client that reads them to prove a replacement was delete-first (CDK/CLI progress output, drift and audit tooling, CI gates that assert no create-before-delete) sees no CREATE_* event for the new physical resource, cannot tie the leading UPDATE_IN_PROGRESS to the resource being replaced because PhysicalResourceId is blank, and is told the replacement created before deleting when the opposite happened.

Fix: the cloudformation service's stack-event emission for resource replacement — pick the delete-first reason variant when the handler deletes before creating, carry the old physical id on the leading UPDATE_IN_PROGRESS, attach the delete reasons, and emit CREATE_IN_PROGRESS/CREATE_COMPLETE for the new physical resource before the closing UPDATE_COMPLETE. The open issue 20261025T053459Z-cloudformation-emits-no-per-resource-stack-events-and.md concerns events being absent and does not name these reason templates or the UPDATE-for-CREATE substitution; it does not cover this.

Traces: cloudformation__ec2.cfn-ec2-subnet-route-table-association-replace-deletes-first-despite-retain-then-delete.

=== Cause 2 of 3: a stack-created AWS::EC2::EIPAssociation is never recorded on the Elastic IP, so DescribeAddresses omits NetworkInterfaceId (and AssociationId) ===

AWS: Smithy `Address` (DescribeAddressesResult member) declares `NetworkInterfaceId` (xmlName networkInterfaceId), `NetworkInterfaceOwnerId`, `AssociationId` (xmlName associationId) and `PrivateIpAddress`. After an `AWS::EC2::EIPAssociation` with `AllocationId` = the EIP's allocation and `NetworkInterfaceId` = an ENI reaches CREATE_COMPLETE, DescribeAddresses(AllocationIds=[<alloc>]) returns an Address whose `NetworkInterfaceId` is that ENI's id and whose `AssociationId` is the value `Ref` on the EIPAssociation resource returns (an `eipassoc-…`). Both members are present on an associated address; AWS omits them only while the address is unassociated.

Emulator: the stack reaches CREATE_COMPLETE and publishes the outputs, but the Address returned for the allocation has NO `NetworkInterfaceId` member at all (the check's dict lookup raises KeyError), so the association the stack declared was never applied to EC2's address state. `AssociationId` is asserted immediately after and is unreachable, so it is in scope of the same cause.

Why: a client that resolves which interface an Elastic IP is bound to — DescribeAddresses before a re-association, failover scripts, reachability checks, any teardown that disassociates before releasing — reads an address that looks unassociated, so it either re-associates over a live binding or skips the disassociate it needs.

Fix: the AWS::EC2::EIPAssociation resource handler in the cloudformation service plus the ec2 service's address state it writes through (AssociateAddress) — record the network-interface binding and the generated association id on the Address so DescribeAddresses reports both.

Traces: cloudformation__ec2.deploy-ec2-eip-network-interface-association-round-trip-and-delete.

=== Cause 3 of 3: a stack-created AWS::EC2::NetworkInterfaceAttachment is never recorded on the interface, so DescribeNetworkInterfaces omits Attachment ===

AWS: Smithy `NetworkInterface.Attachment` targets `NetworkInterfaceAttachment` (xmlName attachment), whose members include `AttachmentId`, `InstanceId`, `DeviceIndex`, `DeleteOnTermination` (boolean), `Status` and `AttachTime`. After an `AWS::EC2::NetworkInterfaceAttachment` naming an InstanceId, a NetworkInterfaceId, DeviceIndex and `DeleteOnTermination: false` reaches CREATE_COMPLETE, DescribeNetworkInterfaces(NetworkInterfaceIds=[<eni>]) returns that interface with an `Attachment` whose `InstanceId` is the instance, whose `DeviceIndex` is the declared index and whose `DeleteOnTermination` is the declared `false`.

Emulator: the stack reaches CREATE_COMPLETE, but the NetworkInterface returned for the ENI has NO `Attachment` member at all (the check's dict lookup raises KeyError) — the attachment the stack declared was never applied to EC2's interface state, so the declared DeleteOnTermination has no effect either.

Why: a client reads `Attachment` to learn whether an interface is in use, which instance and device index holds it, and whether terminating that instance will take the interface with it. With the member absent, the interface looks free: tooling detaches or deletes an attached ENI, and the `DeleteOnTermination: false` guarantee a caller relies on to let an interface outlive its instance is silently unenforced.

Fix: the AWS::EC2::NetworkInterfaceAttachment resource handler in the cloudformation service plus the ec2 service's network-interface state it writes through (AttachNetworkInterface / ModifyNetworkInterfaceAttribute) — record the attachment (id, instance, device index, DeleteOnTermination, status) on the interface so DescribeNetworkInterfaces reports it, and honour DeleteOnTermination on instance termination.

The two open bundles 20261109T223435Z-cloudformation-ec2-6-net-new-causes-26-aws.md and 20261117T184012Z-cloudformation-ec2-6-net-new-causes-on-cloudformation-ec2-stacks.md cover other cloudformation__ec2 resource types and do not name EIPAssociation, NetworkInterfaceAttachment or the replacement-event wording; they do not cover these three causes.

Traces: cloudformation__ec2.deploy-ec2-network-interface-attachment-delete-on-termination-round-trip-and-delete.

## Evidence
Cause 1 — cloudformation__ec2.cfn-ec2-subnet-route-table-association-replace-deletes-first-despite-retain-then-delete [mismatch], at line 140 of the check:
  AssertionError: Association must be deleted before its replacement is created, its events were [('UPDATE_IN_PROGRESS', 'Requested update requires the creation of a new physical resource; hence creating one.', ''), ('DELETE_IN_PROGRESS', None, 'rtbassoc-000000000000000aa'), ('DELETE_COMPLETE', None, 'rtbassoc-000000000000000aa'), ('UPDATE_IN_PROGRESS', 'Resource creation Initiated', 'rtbassoc-000000000000000ab'), ('UPDATE_COMPLETE', None, 'rtbassoc-000000000000000ab')]
  Tuples are (ResourceStatus, ResourceStatusReason, PhysicalResourceId) for the 'Association' logical resource, in event order.
  Expected (AWS), with <old>/<new> the association ids from state:
    ('UPDATE_IN_PROGRESS', None, <old>)
    ('DELETE_IN_PROGRESS', 'Requested update requires the replacement of the existing resource; deleting existing resource, then creating a new one.', <old>)
    ('DELETE_COMPLETE', 'Resource <old> delete complete', <old>)
    ('CREATE_IN_PROGRESS', 'Resource creation Initiated', <new>)
    ('CREATE_COMPLETE', None, <new>)
    ('UPDATE_COMPLETE', None, <new>)
  The ids 'rtbassoc-000000000000000aa'/'ab' are this run's generated values, not part of the requirement. Earlier assertions pass: the stack reaches CREATE_COMPLETE, the replacement really does get a new association id, and the delete precedes the create.

Cause 2 — cloudformation__ec2.deploy-ec2-eip-network-interface-association-round-trip-and-delete [error], at line 244 of the check:
  KeyError: 'NetworkInterfaceId'
  raised by `assert addr["NetworkInterfaceId"] == eni_id` where addr = ec2.describe_addresses(AllocationIds=[<alloc>])["Addresses"][0].
  Template: AWS::EC2::VPC + Subnet + InternetGateway + VPCGatewayAttachment + NetworkInterface + EIP(Domain=vpc) + EIPAssociation{AllocationId: GetAtt Eip.AllocationId, NetworkInterfaceId: Ref Eni}. The stack reached CREATE_COMPLETE and the preceding assertions on the Address's AllocationId, Domain ('vpc') and PublicIp all passed — only the association members are missing.
  Expected: Address.NetworkInterfaceId == the ENI's id, and Address.AssociationId == the value Ref on the EIPAssociation returns (asserted on the next line, unreached).
  Actual: the Address has no NetworkInterfaceId member.
  Smithy: com.amazonaws.ec2#Address declares NetworkInterfaceId, NetworkInterfaceOwnerId and AssociationId.

Cause 3 — cloudformation__ec2.deploy-ec2-network-interface-attachment-delete-on-termination-round-trip-and-delete [error], at line 210 of the check:
  KeyError: 'Attachment'
  raised by `attachment = eni["Attachment"]` where eni = ec2.describe_network_interfaces(NetworkInterfaceIds=[<eni>])["NetworkInterfaces"][0].
  Template: AWS::EC2::VPC + Subnet + Instance(t3.micro, SSM-resolved AL2023 AMI) + NetworkInterface + NetworkInterfaceAttachment{InstanceId: Ref Instance, NetworkInterfaceId: Ref Eni, DeviceIndex: '1', DeleteOnTermination: false}. The stack reached CREATE_COMPLETE and published InstanceId and EniId outputs.
  Expected: NetworkInterface.Attachment present, with Attachment.DeleteOnTermination false and Attachment.InstanceId == the stack's instance id.
  Actual: the NetworkInterface has no Attachment member.
  Smithy: com.amazonaws.ec2#NetworkInterface.Attachment -> NetworkInterfaceAttachment{AttachmentId, InstanceId, DeviceIndex, DeleteOnTermination, Status, AttachTime, ...}.

## Steps to Reproduce
Cause 1:
1. CreateStack with an AWS::EC2::VPC, two subnets, an AWS::EC2::RouteTable and one AWS::EC2::SubnetRouteTableAssociation (UpdateReplacePolicy Retain) pointing at subnet A; wait for CREATE_COMPLETE.
2. UpdateStack with the same template but the association pointing at subnet B; wait for UPDATE_COMPLETE.
3. DescribeStackEvents and keep the events for the association's logical id that are new since step 1.
4. Observe: a leading UPDATE_IN_PROGRESS with reason 'Requested update requires the creation of a new physical resource; hence creating one.' and an empty PhysicalResourceId; DELETE_IN_PROGRESS/DELETE_COMPLETE with no ResourceStatusReason; the new association reported as UPDATE_IN_PROGRESS('Resource creation Initiated')/UPDATE_COMPLETE with no CREATE_IN_PROGRESS or CREATE_COMPLETE. AWS emits the six records listed in the description.

Cause 2:
1. CreateStack with AWS::EC2::VPC + Subnet + InternetGateway + VPCGatewayAttachment + NetworkInterface + EIP(Domain: vpc) + EIPAssociation{AllocationId: !GetAtt Eip.AllocationId, NetworkInterfaceId: !Ref Eni}; wait for CREATE_COMPLETE.
2. Call ec2 DescribeAddresses(AllocationIds=[<the EIP's allocation id>]).
3. Observe the returned Address has no NetworkInterfaceId and no AssociationId. AWS reports NetworkInterfaceId = the ENI id and AssociationId = the EIPAssociation's Ref value.

Cause 3:
1. CreateStack with AWS::EC2::VPC + Subnet + Instance + NetworkInterface + NetworkInterfaceAttachment{InstanceId: !Ref Instance, NetworkInterfaceId: !Ref Eni, DeviceIndex: '1', DeleteOnTermination: false}; wait for CREATE_COMPLETE.
2. Call ec2 DescribeNetworkInterfaces(NetworkInterfaceIds=[<the ENI id>]).
3. Observe the returned NetworkInterface has no Attachment member. AWS reports Attachment with InstanceId = the stack's instance, DeviceIndex 1 and DeleteOnTermination false.
4. Follow-up the check then makes: TerminateInstances on that instance only; with DeleteOnTermination false the interface must outlive it.

Re-replay: cloudformation__ec2.cfn-ec2-subnet-route-table-association-replace-deletes-first-despite-retain-then-delete, cloudformation__ec2.deploy-ec2-eip-network-interface-association-round-trip-and-delete, cloudformation__ec2.deploy-ec2-network-interface-attachment-delete-on-termination-round-trip-and-delete.
