Issue-Source: trace-sweep-divergence
Issue-Trace: ec2-asgeni-traj-attach-device-index-and-limit
Issue-Step: task#9
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ec2-asgeni-traj-attach-device-index-and-limit ec2-asgeni-traj-attach-placement-ids-and-teardown-order ec2-instance-event-window-tag-association-lifecycle ec2-ipam-transit-gateway-and-instance-association-error-paths ec2-t12-eni-attach-and-private-ip ec2-tgwc-traj-tgw-route-table-lookup-mistakes ec2-tgwc-traj-vpc-attachment-to-absent-tgw ec2-usg-traj-groupname-lookup-resolves-only-in-default-vpc ec2-usg-traj-rule-referenced-group-is-unattached-yet-used ec2-vvd-traj-subnet-create-mistakes-then-recovery
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2|AttachNetworkInterface|4xx->4xx|$.Response.Errors.Error.Message
Issue-Key: ec2|AttachNetworkInterface|4xx->4xx|$.Response.Errors.Error.Code
Issue-Key: ec2|AssociateInstanceEventWindow|4xx->4xx|$.Response.Errors.Error.Code
Issue-Key: ec2|AssociateInstanceEventWindow|4xx->4xx|$.Response.Errors.Error.Message
Issue-Key: ec2|DetachNetworkInterface|4xx->4xx|$.Response.Errors.Error.Message
Issue-Key: ec2|SearchTransitGatewayRoutes|4xx->4xx|$.Response.Errors.Error.Message
Issue-Key: ec2|SearchTransitGatewayRoutes|4xx->4xx|$.Response.Errors.Error.Code
Issue-Key: ec2|CreateTransitGatewayVpcAttachment|4xx->4xx|$.Response.Errors.Error.Code
Issue-Key: ec2|CreateTransitGatewayVpcAttachment|4xx->4xx|$.Response.Errors.Error.Message
Issue-Key: ec2|DeleteSecurityGroup|4xx->4xx|$.Response.Errors.Error.Message
Issue-Key: ec2|DescribeSecurityGroupReferences|4xx->4xx|$.Response.Errors.Error.Code
Issue-Key: ec2|DescribeSecurityGroupReferences|4xx->4xx|$.Response.Errors.Error.Message
Issue-Key: ec2|CreateSubnet|4xx->4xx|$.Response.Errors.Error.Message
Issue-Commit: 688abb98d0ad3ae2c07107aa110c7330aa2e99f9

# ec2 refusal error codes and messages for malformed or absent identifiers diverge from AWS

> _Note: this issue was identified against the code base at commit `688abb98d0ad3ae2c07107aa110c7330aa2e99f9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
AttachNetworkInterface

## Description
Ten net-new causes, all ec2, all of them the same fault: when a request carries a malformed, out-of-range or absent resource identifier, the ec2Query error reply the emulator serialises carries an emulator-authored Code and/or Message instead of the Code and message template AWS uses for that error class. Status is 400 in every case, so a client that only checks the status passes; a client that branches on Code, or that surfaces the message, is wrong. The open issue 20261116T190237Z-ec2-error-responses-use-wrong-error-codes-and.md describes the same family but does not carry any of these replay keys, so these ten are filed here.

EC2 uses the ec2Query protocol and models no operation-level errors (smithy_query operation=... reports "errors": [] for every operation below), so the Code/message requirement is recorded behaviour only; the AWS text below is taken from the recorded response of each step, with the fixed text and the request/state-derived parts named per cause.

Cause 1 of 10: AttachNetworkInterface with a negative DeviceIndex.
AWS: recorded behavior only. 400, Code InvalidParameterValue, Message "Invalid network device index '<deviceIndex>'." — fixed text "Invalid network device index '" ... "'."; the quoted value is the DeviceIndex taken verbatim from the request. Smithy: AttachNetworkInterface.DeviceIndex is a required Integer with no @range, so the bound is enforced by the service, not the model.
Emulator: 400 with Message "Value (-1) for parameter deviceIndex is invalid. A device index is zero or above" — emulator-authored prose in the generic "Value (x) for parameter y is invalid" template.
Why: a client that matches on the refusal text to tell "index out of range" from other InvalidParameterValue refusals never matches.
Fix: AttachNetworkInterface's input validation, in the ec2 service crate's attach_network_interface operation.
Traces: ec2-asgeni-traj-attach-device-index-and-limit#task#9.

Cause 2 of 10: AttachNetworkInterface with a non-integer DeviceIndex.
AWS: recorded behavior only. 400, Code InvalidParameterValue, Message "Invalid integer value <value>" — fixed text "Invalid integer value " plus the raw query-string value from the request.
Emulator: 400, Code MalformedInput, Message "invalid integer `abc`: invalid digit found in string" — the Rust parse error leaked to the wire, under a code AWS does not use here.
Why: the code is the part clients branch on; MalformedInput is a different error class from InvalidParameterValue, and the message exposes emulator internals.
Fix: the ec2Query request-deserialisation layer's integer coercion, which must raise InvalidParameterValue with AWS's wording rather than surfacing the parser's Display output.
Traces: ec2-asgeni-traj-attach-device-index-and-limit#task#10 (cluster also covers task#12 of the same fixture and task#10-task#12 of ec2-asgeni-traj-attach-placement-ids-and-teardown-order).

Cause 3 of 10: AssociateInstanceEventWindow naming an instance event window that does not exist.
AWS: recorded behavior only. 400, Code InvalidInstanceEventWindowId.NotFound, Message "The instance-event-window ID '<id>' does not exist" — fixed text "The instance-event-window ID '" ... "' does not exist", the quoted id taken from the request.
Emulator: 400, Code InvalidInstanceEventWindowIDNotFound (the dot before NotFound dropped and ID capitalised), Message "Invalid Instance Event Window IDs [iew-...]." — emulator-authored prose in a list form AWS does not use for a single id.
Why: EC2 not-found codes are "<Resource>Id.NotFound"; a client matching that shape, or an SDK waiter keyed on it, does not recognise the emulator's code.
Fix: the AssociateInstanceEventWindow handler's id-resolution error, plus the shared not-found error constructor it uses.
Traces: ec2-instance-event-window-tag-association-lifecycle#task#7 (cluster also covers ec2-ipam-transit-gateway-and-instance-association-error-paths#task#2).

Cause 4 of 10: DetachNetworkInterface naming an attachment that does not exist.
AWS: recorded behavior only. 400, Message "The interface attachment '<attachmentId>' does not exist." — fixed text "The interface attachment '" ... "' does not exist." (note the trailing period), the quoted id from the request.
Emulator: 400, Message "The attachment ID '<attachmentId>' does not exist" — different noun phrase and no trailing period.
Why: the message is a requirement; text that differs is a fidelity defect and breaks consumers that assert on it.
Fix: the DetachNetworkInterface handler's attachment-lookup error. The open issue 20261115T003554Z-ec2-control-plane-runinstances-discards-cpuoptions-the-instance.md mentions DetachNetworkInterface but does not carry this replay key.
Traces: ec2-t12-eni-attach-and-private-ip#task#15.

Cause 5 of 10: SearchTransitGatewayRoutes with TransitGatewayRouteTableId absent.
AWS: recorded behavior only. 400, Message "Missing required parameter in request: <MemberName>." — fixed text "Missing required parameter in request: " plus the modelled member name and a period. Smithy: SearchTransitGatewayRoutes.TransitGatewayRouteTableId is required.
Emulator: 400 with "The request must include the TransitGatewayRouteTableId parameter. Add the required parameter and retry the request." — emulator-authored prose.
Why: every EC2 missing-parameter refusal shares one template; a client normalising that template over all operations fails on this one.
Fix: the shared missing-required-parameter error used by ec2Query input validation.
Traces: ec2-tgwc-traj-tgw-route-table-lookup-mistakes#task#5.

Cause 6 of 10: SearchTransitGatewayRoutes naming a transit-gateway route table that does not exist, with Filters absent.
AWS: recorded behavior only. 400, Code InvalidRouteTableID.NotFound, Message "Transit Gateway Route Table <id> was deleted or does not exist." — i.e. AWS resolves the route-table id BEFORE it checks the required Filters member. Smithy: both TransitGatewayRouteTableId and Filters are required on this operation, so the divergence is one of validation ORDER, not of which members are required.
Emulator: 400, Code MissingParameter, Message "Filters cannot be null or empty" — the Filters presence check runs first and the absent route table is never reported.
Why: a client probing whether a route table exists gets MissingParameter and concludes its request was malformed rather than that the resource is gone.
Fix: the SearchTransitGatewayRoutes handler's validation order — resolve the route-table id before validating Filters — and the Filters refusal's own code/message.
Traces: ec2-tgwc-traj-tgw-route-table-lookup-mistakes#task#8.

Cause 7 of 10: CreateTransitGatewayVpcAttachment with a syntactically invalid TransitGatewayId.
AWS: recorded behavior only. 400, Code InvalidTransitGatewayID.Malformed, Message "Invalid Transit Gateway id <id>." — AWS classifies an id that does not match the tgw-<17 hex> form as MALFORMED, not as not-found.
Emulator: 400, Code InvalidTransitGatewayID.NotFound, Message "Transit Gateway <id> was deleted or does not exist." — the id-format check is missing, so a malformed id falls through to the lookup.
Why: malformed and not-found are different conditions; a client retrying on NotFound (expecting eventual consistency) retries forever on an id it should have been told to fix.
Fix: an id-format (syntax) check ahead of state lookup in the CreateTransitGatewayVpcAttachment handler's validation.
Traces: ec2-tgwc-traj-vpc-attachment-to-absent-tgw#task#8 (cluster also covers task#9 and task#10 of the same fixture).

Cause 8 of 10: DeleteSecurityGroup by GroupName for a group that is not in the default VPC.
AWS: recorded behavior only. 400, Message "The security group '<groupName>' does not exist in default VPC '<vpcId>'" — fixed text "The security group '" ... "' does not exist in default VPC '" ... "'"; the group name comes from the request and the VPC id from the account's default VPC in the emulator's state. GroupName lookup resolves only within the default VPC, and AWS says so.
Emulator: 400 with "The security group '<groupName>' does not exist" — the default-VPC clause is dropped entirely.
Why: the dropped clause is the whole diagnostic: it tells the caller the name was looked up in the default VPC only, which is why a group that exists elsewhere was not found.
Fix: the DeleteSecurityGroup handler's group-name resolution error, which must name the default VPC it searched.
Traces: ec2-usg-traj-groupname-lookup-resolves-only-in-default-vpc#task#6.

Cause 9 of 10: DescribeSecurityGroupReferences with a short/abbreviated security-group id.
AWS: recorded behavior only. 400, Code InvalidGroup.NotFound, Message "The security group '<groupId>' does not exist" — on this operation AWS treats an id it cannot resolve as not-found. Smithy: DescribeSecurityGroupReferences.GroupId is a required list of GroupIds.
Emulator: 400, Code InvalidGroupId.Malformed, Message "Invalid id: \"sg-12345\"" — a different code, a different error class, and emulator-authored prose.
Why: inverse of cause 7 — here the emulator classifies as malformed what AWS classifies as not-found, so a client's not-found branch never runs.
Fix: the DescribeSecurityGroupReferences handler's GroupId resolution, which must report unresolvable ids as InvalidGroup.NotFound with AWS's message.
Traces: ec2-usg-traj-rule-referenced-group-is-unattached-yet-used#task#9.

Cause 10 of 10: CreateSubnet naming an AvailabilityZone the region does not offer.
AWS: recorded behavior only. 400, Message "Value (<az>) for parameter availabilityZone is invalid. Subnets can currently only be created in the following availability zones: <comma-separated list of the region's subnet-capable AZ names>." The template is fixed; the list is state — every AZ the region offers.
Emulator: same template but the list is short by one zone (it enumerated us-west-2a, us-west-2b, us-west-2c where the region offers us-west-2a, us-west-2b, us-west-2c, us-west-2d), so the AZ catalogue backing the message is incomplete. The specific names are this region's values, not a requirement — the requirement is that the list equal the region's subnet-capable AZ set.
Why: the message is the only place a caller learns which zones it may retry in, so it is steered away from a zone that works. The same incomplete catalogue also makes CreateSubnet accept/refuse the wrong zones. The open issue 20261116T190237Z-ec2-region-availability-zone-and-vpc-endpoint-service.md describes an incomplete AZ catalogue but does not carry this replay key.
Fix: the region AZ catalogue in the ec2 service crate's state, and the CreateSubnet validation that renders it into this message.
Traces: ec2-vvd-traj-subnet-create-mistakes-then-recovery#task#5.

## Evidence
Replay of trace set simparity-wire-aws-v1 (429 fixtures, 3941 steps, score 80.4%). Ten mismatch clusters, all service=ec2, all with empty stub_path and empty owned_by:

1. ec2-asgeni-traj-attach-device-index-and-limit#task#9 AttachNetworkInterface 400->400: $.Response.Errors.Error.Message expected "Invalid network device index '-1'." got 'Value (-1) for parameter deviceIndex is invalid. A device index is zero or above'.
2. ec2-asgeni-traj-attach-device-index-and-limit#task#10 AttachNetworkInterface 400->400: Code expected 'InvalidParameterValue' got 'MalformedInput'; Message expected 'Invalid integer value abc' got 'invalid integer `abc`: invalid digit found in string'. affected_count 5.
3. ec2-instance-event-window-tag-association-lifecycle#task#7 AssociateInstanceEventWindow 400->400: Code expected 'InvalidInstanceEventWindowId.NotFound' got 'InvalidInstanceEventWindowIDNotFound'; Message expected "The instance-event-window ID 'iew-0123456789abcdef0' does not exist" got 'Invalid Instance Event Window IDs [iew-0123456789abcdef0].'. affected_count 2.
4. ec2-t12-eni-attach-and-private-ip#task#15 DetachNetworkInterface 400->400: Message expected "The interface attachment 'eni-attach-000000000000000ad' does not exist." got "The attachment ID 'eni-attach-000000000000000ad' does not exist".
5. ec2-tgwc-traj-tgw-route-table-lookup-mistakes#task#5 SearchTransitGatewayRoutes 400->400: Message expected 'Missing required parameter in request: TransitGatewayRouteTableId.' got 'The request must include the TransitGatewayRouteTableId parameter. Add the required parameter and retry the request.'
6. ec2-tgwc-traj-tgw-route-table-lookup-mistakes#task#8 SearchTransitGatewayRoutes 400->400: Code expected 'InvalidRouteTableID.NotFound' got 'MissingParameter'; Message expected 'Transit Gateway Route Table tgw-rtb-0a1b2c3d4e5f60718 was deleted or does not exist.' got 'Filters cannot be null or empty'.
7. ec2-tgwc-traj-vpc-attachment-to-absent-tgw#task#8 CreateTransitGatewayVpcAttachment 400->400: Code expected 'InvalidTransitGatewayID.Malformed' got 'InvalidTransitGatewayID.NotFound'; Message expected 'Invalid Transit Gateway id tgw-xyz.' got 'Transit Gateway tgw-xyz was deleted or does not exist.'. affected_count 3.
8. ec2-usg-traj-groupname-lookup-resolves-only-in-default-vpc#task#6 DeleteSecurityGroup 400->400: Message expected "The security group '<name>' does not exist in default VPC '<vpcId>'" got "The security group '<name>' does not exist".
9. ec2-usg-traj-rule-referenced-group-is-unattached-yet-used#task#9 DescribeSecurityGroupReferences 400->400: Code expected 'InvalidGroup.NotFound' got 'InvalidGroupId.Malformed'; Message expected "The security group 'sg-12345' does not exist" got 'Invalid id: "sg-12345"'.
10. ec2-vvd-traj-subnet-create-mistakes-then-recovery#task#5 CreateSubnet 400->400: Message AZ list expected '... us-west-2a, us-west-2b, us-west-2c, us-west-2d.' got '... us-west-2a, us-west-2b, us-west-2c.'

Model grounding (smithy_query on services/service-com-amazonaws-ec2-amazonec2/smithy-model/com-amazonaws-ec2-amazonec2.json): AttachNetworkInterface.DeviceIndex required Integer, no @range; SearchTransitGatewayRoutes.TransitGatewayRouteTableId and .Filters both required; DescribeSecurityGroupReferences.GroupId required list; every one of these operations models "errors": [] (ec2Query), so codes and messages are recorded behaviour.

## Steps to Reproduce
1. git clone the repo and boot the emulator built from it.
2. Replay the wire fixture corpus (trace set simparity-wire-aws-v1).
3. Re-replay the representative step ec2-asgeni-traj-attach-device-index-and-limit#task#9 and each step listed in trace_ids. Each is an ec2Query POST whose recorded response is a 400 <Response><Errors><Error><Code>/<Message> body; compare Code and Message against the expected values quoted in the evidence above.
