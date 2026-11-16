Issue-Source: trace-sweep-divergence
Issue-Trace: ec2-asghc-traj-describe-launch-template-versions-version-mistakes
Issue-Step: task#4
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ec2-asghc-traj-describe-launch-template-versions-version-mistakes ec2-page-describe-launch-template-versions-two-pages ec2-capacity-reservation-error-paths ec2-capacity-reservation-interruptible-allocation-error ec2-client-vpn-endpoint-invalid-cidr-size ec2-client-vpn-endpoint-route-workflow ec2-copy-ami-cross-region-workflow ec2-image-copy-and-fpga-invalid-input-errors ec2-internet-gateway-attachment-errors ec2-pip-traj-secondary-ip-request-mistakes ec2-t12-eni-attach-and-private-ip ec2-volume-snapshot-copy-lifecycle
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2|DescribeLaunchTemplateVersions|4xx->4xx|$.Response.Errors.Error.Code
Issue-Key: ec2|DescribeLaunchTemplateVersions|4xx->4xx|$.Response.Errors.Error.Message
Issue-Key: ec2|CreateCapacityReservation|4xx->4xx|$.Response.Errors.Error.Message
Issue-Key: ec2|CreateInterruptibleCapacityReservationAllocation|4xx->4xx|$.Response.Errors.Error.Message
Issue-Key: ec2|CreateClientVpnEndpoint|4xx->4xx|$.Response.Errors.Error.Message
Issue-Key: ec2|CopyImage|4xx->4xx|$.Response.Errors.Error.Code
Issue-Key: ec2|CopyImage|4xx->4xx|$.Response.Errors.Error.Message
Issue-Key: ec2|AttachInternetGateway|4xx->4xx|$.Response.Errors.Error.Code
Issue-Key: ec2|AttachInternetGateway|4xx->4xx|$.Response.Errors.Error.Message
Issue-Key: ec2|AssignPrivateIpAddresses|4xx->4xx|$.Response.Errors.Error.Message
Issue-Key: ec2|AssignPrivateIpAddresses|4xx->4xx|$.Response.Errors.Error.Code
Issue-Key: ec2|CopyVolumes|4xx->4xx|$.Response.Errors.Error.Code
Issue-Key: ec2|CopyVolumes|4xx->4xx|$.Response.Errors.Error.Message
Issue-Commit: 0771e25bba8212ca0e4339575fc1faa7cfb6765d

# ec2 error responses use wrong error codes and message text

> _Note: this issue was identified against the code base at commit `0771e25bba8212ca0e4339575fc1faa7cfb6765d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeLaunchTemplateVersions

## Description
The emulator rejects bad input on ten different EC2 operations with its own error code and/or its own wording instead of the code and the message AWS returns for that error class. Every step below already answers HTTP 400, so the status is right; the Errors.Error.Code and/or Errors.Error.Message inside the ec2Query error document diverge. This bundle carries 10 separate net-new divergence clusters, none of them owned by an open issue; they share one fault: the per-operation validation/error-mapping layer picks the wrong error identity and writes emulator-authored prose.

GENERAL FAULT
AWS: an EC2 ec2Query error carries <Code> drawn from the documented error set for that failure and <Message> whose fixed text is the same for every caller, with only the request/state values (ids, addresses, supplied values, limits) substituted into it.
Emulator: the handlers emit a differently-named code and/or a locally-invented sentence for the same failure.
Why: SDK and CLI retry/branching logic keys off Error.Code (for example InvalidAMIID.Malformed vs InvalidAMIID.NotFound, or InvalidVolume.NotFound vs InvalidVolumeID.Malformed select completely different remediation paths), and operator tooling and tests match on the message text; both take the wrong branch against the emulator.
Fix: the validation/error-construction paths of each operation's handler listed below (the code constant and the message template), not the serializer - the error envelope itself is shaped correctly.

Cause 1 of 10: DescribeLaunchTemplateVersions with an empty version value. AWS: 400 Code 'MissingParameter', Message 'The launch template version number cannot be empty.' (entirely fixed text - no request values). Emulator: Code 'InvalidParameterValue', Message "Invalid value '' for version. Valid values are $Latest, $Default, and version numbers". Traces: ec2-asghc-traj-describe-launch-template-versions-version-mistakes#task#4.

Cause 2 of 10: DescribeLaunchTemplateVersions for a version that does not exist. AWS: Message 'Could not find the specified version <version> for the launch template with ID <launch-template-id>.' - fixed text 'Could not find the specified version ... for the launch template with ID ...', variable parts the requested version number and the launch-template id from the request. Emulator: "Launch template version '<version>' does not exist for launch template '<launch-template-id>'". Code matches. Traces: ec2-asghc-traj-describe-launch-template-versions-version-mistakes#task#5 (cluster also covers task#6 of the same fixture and ec2-page-describe-launch-template-versions-two-pages#task#9).

Cause 3 of 10: CreateCapacityReservation with InstanceCount outside the allowed range. AWS: Message '<supplied-value> is an invalid value for InstanceCount. InstanceCount must be between 1 and 1000.' - fixed text apart from the value the caller supplied; Smithy: CreateCapacityReservation.InstanceCount is a required integer. Emulator: "The instance count isn't valid. Enter a value between 1 and 1000 instances, and try again" (never echoes the supplied value). Traces: ec2-capacity-reservation-error-paths#task#4.

Cause 4 of 10: CreateInterruptibleCapacityReservationAllocation asking for more instances than the reservation has available. AWS: Message "The capacity reservation doesn't have enough available instance capacity to fulfill this allocation request. Available instances: <available>, Requested allocation: <requested>." - the two numbers come from state and request. Emulator: "Invalid value '<requested>' for instanceCount: Capacity Reservation '<cr-id>' has <available> instances available for allocation". Traces: ec2-capacity-reservation-interruptible-allocation-error#task#2.

Cause 5 of 10: CreateClientVpnEndpoint with a client CIDR smaller than /22. AWS: Message 'Client cidr block must be at least a /22 range' (fixed text, no values). Emulator: 'Client CIDR range must have a size of at least /22 and must not be greater than /12.'. Traces: ec2-client-vpn-endpoint-invalid-cidr-size#task#1 (cluster also covers ec2-client-vpn-endpoint-route-workflow#task#1).

Cause 6 of 10: CopyImage with a malformed SourceImageId. AWS: Code 'InvalidAMIID.Malformed', Message 'Invalid id: "<supplied-id>"' - fixed text 'Invalid id: ' plus the id exactly as supplied, in double quotes. Smithy: CopyImage.SourceImageId is a required String. Emulator: Code 'InvalidAMIID.NotFound' with "The image id '[<mangled-id>]' does not exist" - it classifies a malformed id as a lookup miss and additionally rewrites the id it echoes (bracketed and padded) instead of echoing the request value. Traces: ec2-copy-ami-cross-region-workflow#task#3 (cluster also covers ec2-image-copy-and-fpga-invalid-input-errors#task#1).

Cause 7 of 10: AttachInternetGateway for a VPC that already has an internet gateway. AWS: Code 'InvalidParameterValue', Message 'Network <vpc-id> already has an internet gateway attached' - fixed text with the VPC id from the request. Emulator: Code 'Resource.AlreadyAssociated', Message 'resource <igw-id> is already attached to network <vpc-id>'. Traces: ec2-internet-gateway-attachment-errors#task#4.

Cause 8 of 10: AssignPrivateIpAddresses for an address inside the subnet's reserved range (the first four / last address of the CIDR). AWS: Message "Address <ip> is in subnet's reserved range." - fixed text with the requested address. Emulator: "Address does not fall within the subnet's address range", which is the wrong diagnosis (the address IS inside the CIDR, it is reserved) and drops the address. Traces: ec2-pip-traj-secondary-ip-request-mistakes#task#5 (cluster also covers task#6 of the same fixture).

Cause 9 of 10: AssignPrivateIpAddresses re-requesting an address already assigned to another interface without AllowReassignment. AWS: Code 'InvalidParameterValue', Message '[<ip>] assigned, but move is not allowed.' - fixed text with the requested address in brackets. Smithy: AssignPrivateIpAddresses.AllowReassignment is the modeled opt-in for the move. Emulator: Code 'InvalidIPAddress.InUse', Message 'Address <ip> is in use.'. Traces: ec2-t12-eni-attach-and-private-ip#task#11.

Cause 10 of 10: CopyVolumes naming a well-formed volume id that does not exist. AWS: Code 'InvalidVolume.NotFound', Message "The volume '<volume-id>' does not exist." Emulator: Code 'InvalidVolumeID.Malformed', Message "The volume ID '<volume-id>' is malformed" - it treats a syntactically valid vol-<17 hex> id as malformed, so a caller can never distinguish a typo from a missing volume. Traces: ec2-volume-snapshot-copy-lifecycle#task#3.

## Evidence
All ten clusters: expected status 400, actual status 400, verdict mismatch - the divergence is in $.Response.Errors.Error.Code and/or .Message.
Cause 1 (ec2-asghc-traj-describe-launch-template-versions-version-mistakes#task#4): Code expected 'MissingParameter', got 'InvalidParameterValue'; Message expected 'The launch template version number cannot be empty.', got "Invalid value '' for version. Valid values are $Latest, $Default, and version numbers".
Cause 2 (same fixture#task#5): Message expected 'Could not find the specified version 99 for the launch template with ID lt-0000000000000008d.', got "Launch template version '99' does not exist for launch template 'lt-0000000000000008d'".
Cause 3 (ec2-capacity-reservation-error-paths#task#4): Message expected '0 is an invalid value for InstanceCount. InstanceCount must be between 1 and 1000.', got "The instance count isn't valid. Enter a value between 1 and 1000 instances, and try again".
Cause 4 (ec2-capacity-reservation-interruptible-allocation-error#task#2): Message expected "The capacity reservation doesn't have enough available instance capacity to fulfill this allocation request. Available instances: 1, Requested allocation: 5.", got "Invalid value '5' for instanceCount: Capacity Reservation 'cr-0000000000000008d' has 1 instances available for allocation".
Cause 5 (ec2-client-vpn-endpoint-invalid-cidr-size#task#1): Message expected 'Client cidr block must be at least a /22 range', got 'Client CIDR range must have a size of at least /22 and must not be greater than /12.'.
Cause 6 (ec2-copy-ami-cross-region-workflow#task#3): Code expected 'InvalidAMIID.Malformed', got 'InvalidAMIID.NotFound'; Message expected 'Invalid id: "ami-0123456789abcdef"', got "The image id '[ami-0123456789abcdef0]' does not exist".
Cause 7 (ec2-internet-gateway-attachment-errors#task#4): Code expected 'InvalidParameterValue', got 'Resource.AlreadyAssociated'; Message expected 'Network vpc-0000000000000008d already has an internet gateway attached', got 'resource igw-0000000000000009c is already attached to network vpc-0000000000000008d'.
Cause 8 (ec2-pip-traj-secondary-ip-request-mistakes#task#5): Message expected "Address 10.240.1.1 is in subnet's reserved range.", got "Address does not fall within the subnet's address range".
Cause 9 (ec2-t12-eni-attach-and-private-ip#task#11): Code expected 'InvalidParameterValue', got 'InvalidIPAddress.InUse'; Message expected '[10.234.1.200] assigned, but move is not allowed.', got 'Address 10.234.1.200 is in use.'.
Cause 10 (ec2-volume-snapshot-copy-lifecycle#task#3): Code expected 'InvalidVolume.NotFound', got 'InvalidVolumeID.Malformed'; Message expected "The volume 'vol-0b40dcc22096bb199' does not exist.", got "The volume ID 'vol-0b40dcc22096bb199' is malformed".
(The ids, addresses and numbers above are the values of these particular recordings; the requirement is the fixed text plus substitution of the request/state values, not these literals.)

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus simparity-wire-aws-v1.
2. Replay ec2-asghc-traj-describe-launch-template-versions-version-mistakes steps task#4 and task#5 and compare $.Response.Errors.Error.Code and .Message to the recordings.
3. Repeat for ec2-capacity-reservation-error-paths#task#4, ec2-capacity-reservation-interruptible-allocation-error#task#2, ec2-client-vpn-endpoint-invalid-cidr-size#task#1, ec2-copy-ami-cross-region-workflow#task#3, ec2-internet-gateway-attachment-errors#task#4, ec2-pip-traj-secondary-ip-request-mistakes#task#5, ec2-t12-eni-attach-and-private-ip#task#11 and ec2-volume-snapshot-copy-lifecycle#task#3.
4. Each returns 400 with an emulator-authored code and/or message instead of AWS's.
