Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2.create-client-vpn-route-conflicting-destination-rejected ec2.op-contract-deleteClientVpnRoute ec2.op-contract-create-snapshot ec2.op-contract-describeApplicationStatusCheckAssociations ec2.op-contract-modify-fpga-image-attribute
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2|ApplySecurityGroupsToClientVpnTargetNetwork|InvalidClientVpnActiveAssociationNotFound
Issue-Key: ec2.op-contract-create-snapshot|run#15|AssertionError|assert code == "InvalidVolumeID.Malformed", f"expected InvalidVolumeID.Malformed, got {code!r}"
Issue-Key: ec2.op-contract-describeApplicationStatusCheckAssociations|run#12|AssertionError|assert code == "InvalidFilter", f"expected InvalidFilter, got {code!r}"
Issue-Key: ec2.op-contract-modify-fpga-image-attribute|run#9|AssertionError|assert code_forbidden == "AuthFailure", f"expected AuthFailure, got {code_forbidden}"
Issue-Commit: 25f334ef49f0fe78ce039b48068cddfc6b5b1616

# EC2: 4 net-new causes — ApplySecurityGroupsToClientVpnTargetNetwork invents an "active association" precondition, CreateSnapshot/DescribeApplicationStatusCheckAssociations misclassify refusals, and DescribeFpgaImages publishes phantom foreign AFIs

> _Note: this issue was identified against the code base at commit `25f334ef49f0fe78ce039b48068cddfc6b5b1616`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ApplySecurityGroupsToClientVpnTargetNetwork

## Description
Four net-new root causes in the ec2 emulator, each reached by its own behavioral check. All four are refusal-fidelity / over-permissive-read defects in EC2 control-plane operations.

=== Cause 1 of 4: ApplySecurityGroupsToClientVpnTargetNetwork requires a target-network association that AWS does not require ===
Failing checks: ec2.create-client-vpn-route-conflicting-destination-rejected, ec2.op-contract-deleteClientVpnRoute

AWS: recorded behavior only — the EC2 Smithy model (services/service-com-amazonaws-ec2-amazonec2/smithy-model/com-amazonaws-ec2-amazonec2.json) declares ApplySecurityGroupsToClientVpnTargetNetwork with required members ClientVpnEndpointId, VpcId and SecurityGroupIds and output member SecurityGroupIds, and models NO errors at all (ec2Query carries untyped errors). The recorded sequence is: CreateClientVpnEndpoint → ApplySecurityGroupsToClientVpnTargetNetwork(ClientVpnEndpointId, VpcId, SecurityGroupIds) → AssociateClientVpnTargetNetwork(SubnetId). AWS answers the ApplySecurityGroups call 200 with the applied SecurityGroupIds echoed back, i.e. the security groups may be applied to the endpoint BEFORE any target network is associated; the association, once created, picks them up.

Emulator: the call is refused 400 with error code `InvalidClientVpnActiveAssociationNotFound` and message `There is no active association for vpc <vpc-id> in Client VPN endpoint <cvpn-endpoint-id>`. Fixed text: "There is no active association for vpc " + " in Client VPN endpoint "; the variable parts are the VpcId and the ClientVpnEndpointId taken from the request. The handler states this precondition explicitly in its own doc comment: "the VPC the association is in (`InvalidVpcID.NotFound`, and the endpoint has to HAVE an association in it)".

Why: the refusal aborts both checks before any Client VPN route is created, so the whole CreateClientVpnRoute / DeleteClientVpnRoute flow is unreachable. Any client that follows AWS's documented order (apply security groups while provisioning the endpoint, then associate the subnet) gets a hard 400 on a call AWS accepts, and cannot provision a Client VPN endpoint with non-default security groups at all.

Fix: the operation's handler/validation ordering — services/service-com-amazonaws-ec2-amazonec2/src/ops/apply_security_groups_to_client_vpn_target_network.rs. The endpoint, the VPC and each security group must still be resolved, but "the endpoint has an active association in this VPC" must not be a precondition for applying the groups; the applied groups belong on the endpoint record and must be adopted by an association created later.

Traces: ec2.create-client-vpn-route-conflicting-destination-rejected, ec2.op-contract-deleteClientVpnRoute (both abort with the identical ClientError on the ApplySecurityGroupsToClientVpnTargetNetwork step).

=== Cause 2 of 4: CreateSnapshot reports a hand-crafted VolumeId as not-found instead of malformed ===
Failing check: ec2.op-contract-create-snapshot

AWS: recorded behavior only — the model declares CreateSnapshot.VolumeId as required (target VolumeId) and models no errors. Real AWS validates the checksum embedded in a long-format resource id, so a syntactically shaped but never-issued id ("vol-" + 17 lowercase hex digits) fails that checksum and is refused as MALFORMED, not as absent: error code `InvalidVolumeID.Malformed` with message `Value ( <volume-id> ) for parameter VOLUME is invalid. ` — fixed text "Value ( ", " ) for parameter VOLUME is invalid. " (note the surrounding spaces and the trailing space), with the volume id taken verbatim from the request.

Emulator: the same call is refused with error code `InvalidVolume.NotFound`, the class reserved for a checksum-valid id the account simply does not hold.

Why: a client cannot tell a typo/fabricated id from a real id that was deleted. Retry and error-handling logic keyed on InvalidVolumeID.Malformed (a permanent client fault) versus InvalidVolume.NotFound (a resource-lifecycle fault) takes the wrong branch.

Fix: the operation's input validation — services/service-com-amazonaws-ec2-amazonec2/src/ops/create_snapshot.rs: validate the long-format resource-id checksum before the state lookup, and raise InvalidVolumeID.Malformed with AWS's message template when it fails.

Traces: ec2.op-contract-create-snapshot.

=== Cause 3 of 4: DescribeApplicationStatusCheckAssociations misclassifies an unknown filter name ===
Failing check: ec2.op-contract-describeApplicationStatusCheckAssociations

AWS: recorded behavior only — the model declares DescribeApplicationStatusCheckAssociations with input members ApplicationStatusCheckIds, Filters (FilterList), MaxResults (range 5–1000), NextToken, DryRun, and output Associations/NextToken/Tags; no errors are modeled. EC2 Describe* operations refuse an unrecognized filter NAME with error code `InvalidFilter` and message `Invalid filter name: '<name>'` — fixed text "Invalid filter name: '" + "'", with the name taken verbatim from the request's Filters[].Name.

Emulator: the same call is refused with error code `InvalidParameterValue`, the class EC2 uses for a bad filter VALUE or a bad scalar parameter, not for an unknown filter name.

Why: filter-name validation and value validation are distinct client faults in EC2; a client that distinguishes them (to report "you asked for a filter this API does not have" versus "this filter's value is wrong") is told the wrong thing, and generic EC2 filter-handling layers that special-case InvalidFilter do not fire.

Fix: this operation's filter validation — services/service-com-amazonaws-ec2-amazonec2/src/ops/describe_application_status_check_associations.rs (and the shared filter-name validation it should route through): an unrecognized Filters[].Name must raise InvalidFilter with AWS's message, before any value or pagination validation.

Traces: ec2.op-contract-describeApplicationStatusCheckAssociations.

=== Cause 4 of 4: DescribeFpgaImages publishes foreign-owned AFIs that do not exist, and ModifyFpgaImageAttribute then misclassifies them ===
Failing check: ec2.op-contract-modify-fpga-image-attribute

AWS: recorded behavior only — the model declares DescribeFpgaImages with Owners (OwnerStringList) and FpgaImages output, and ModifyFpgaImageAttribute with required FpgaImageId; no errors are modeled. AWS publishes no public AFI catalog, so DescribeFpgaImages(Owners=["amazon","aws-marketplace"]) returns an EMPTY FpgaImages list in a normal account; and when an AFI owned by another account IS discoverable, ModifyFpgaImageAttribute on it is refused with error code `AuthFailure` (a permission refusal), not with a not-found class. Only an id that resolves to nothing is refused `InvalidFpgaImageID.NotFound` with message `The fpga-image ID '<afi-id>' does not exist` — fixed text "The fpga-image ID '" + "' does not exist", id from the request.

Emulator: DescribeFpgaImages(Owners=["amazon","aws-marketplace"], MaxResults=10) returns a NON-EMPTY FpgaImages list, and ModifyFpgaImageAttribute on an FpgaImageId taken straight out of that list is then refused `InvalidFpgaImageID.NotFound` instead of `AuthFailure`. The two answers contradict each other: the describe path advertises AFIs the modify path says do not exist.

Why: a client enumerating the marketplace/amazon AFI catalog is handed ids it can never use, and a cross-account permission refusal is reported as a missing resource — so an authorization failure is retried as a lifecycle race instead of surfaced as a permissions problem.

Fix: the read path and the refusal classification — services/service-com-amazonaws-ec2-amazonec2/src/ops/describe_fpga_images.rs must not synthesize AFIs for the amazon/aws-marketplace owner filters (the account's own state holds none), and services/service-com-amazonaws-ec2-amazonec2/src/ops/modify_fpga_image_attribute.rs must answer AuthFailure for an AFI that resolves but is owned by another account, reserving InvalidFpgaImageID.NotFound for an id that resolves to nothing.

Traces: ec2.op-contract-modify-fpga-image-attribute.

## Evidence
Cause 1 — ec2.create-client-vpn-route-conflicting-destination-rejected and ec2.op-contract-deleteClientVpnRoute
  Expected: ApplySecurityGroupsToClientVpnTargetNetwork(ClientVpnEndpointId, VpcId, SecurityGroupIds), called after CreateClientVpnEndpoint and before AssociateClientVpnTargetNetwork, answers 200 with SecurityGroupIds echoed.
  Actual: botocore.exceptions.ClientError: An error occurred (InvalidClientVpnActiveAssociationNotFound) when calling the ApplySecurityGroupsToClientVpnTargetNetwork operation: There is no active association for vpc <vpc-id> in Client VPN endpoint <cvpn-endpoint-id>
  (the vpc id and endpoint id are per-run values filling AWS-shaped slots; the message's fixed text and the error code are the requirement here, and the refusal itself is the defect).

Cause 2 — ec2.op-contract-create-snapshot
  Expected: CreateSnapshot(VolumeId="vol-<17 hex>" never issued) → code InvalidVolumeID.Malformed, message "Value ( <volume-id> ) for parameter VOLUME is invalid. "
  Actual: AssertionError: expected InvalidVolumeID.Malformed, got 'InvalidVolume.NotFound'

Cause 3 — ec2.op-contract-describeApplicationStatusCheckAssociations
  Expected: DescribeApplicationStatusCheckAssociations(Filters=[{Name: <unknown>, Values: ["x"]}]) → code InvalidFilter, message "Invalid filter name: '<name>'"
  Actual: AssertionError: expected InvalidFilter, got 'InvalidParameterValue'

Cause 4 — ec2.op-contract-modify-fpga-image-attribute
  Expected: DescribeFpgaImages(Owners=["amazon","aws-marketplace"]) → FpgaImages == [] (no public AFI catalog), so the foreign-AFI branch is skipped; when a foreign AFI IS discoverable, ModifyFpgaImageAttribute on it → code AuthFailure.
  Actual: DescribeFpgaImages returned a non-empty FpgaImages list, and ModifyFpgaImageAttribute on its first FpgaImageId gave: AssertionError: expected AuthFailure, got InvalidFpgaImageID.NotFound
  (the earlier assertion in the same check — ModifyFpgaImageAttribute("afi-00000000000000000") → InvalidFpgaImageID.NotFound with "The fpga-image ID 'afi-00000000000000000' does not exist" — passed, so only the foreign-owned/phantom-catalog path diverges.)

## Steps to Reproduce
Cause 1:
1. CreateVpc + CreateSubnet + CreateSecurityGroup in that VPC; import a self-signed server certificate into ACM.
2. CreateClientVpnEndpoint with certificate-authentication over that certificate.
3. Call ApplySecurityGroupsToClientVpnTargetNetwork(ClientVpnEndpointId=<endpoint>, VpcId=<vpc>, SecurityGroupIds=[<sg>]) WITHOUT having called AssociateClientVpnTargetNetwork.
4. Expected 200 with SecurityGroupIds echoed; observed 400 InvalidClientVpnActiveAssociationNotFound "There is no active association for vpc <vpc> in Client VPN endpoint <endpoint>".

Cause 2:
1. ec2 CreateSnapshot(VolumeId="vol-0123456789abcdef0") — well-formed shape, never issued by this account.
2. Expected InvalidVolumeID.Malformed / "Value ( vol-0123456789abcdef0 ) for parameter VOLUME is invalid. "; observed InvalidVolume.NotFound.

Cause 3:
1. ec2 DescribeApplicationStatusCheckAssociations(Filters=[{"Name": "not-a-real-filter", "Values": ["x"]}]).
2. Expected InvalidFilter / "Invalid filter name: 'not-a-real-filter'"; observed InvalidParameterValue.

Cause 4:
1. ec2 DescribeFpgaImages(Owners=["self"]) → [] (passes).
2. ec2 DescribeFpgaImages(Owners=["amazon","aws-marketplace"], MaxResults=10). Expected []; observed a non-empty list.
3. Take FpgaImages[0].FpgaImageId from step 2 and call ModifyFpgaImageAttribute(FpgaImageId=<that id>, Attribute="description", Description="x"). Expected AuthFailure; observed InvalidFpgaImageID.NotFound — the id the describe path just published.
