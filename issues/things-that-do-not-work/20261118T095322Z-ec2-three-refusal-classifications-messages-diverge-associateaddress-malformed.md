Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2.op-contract-associate-address-already-associated-rejected ec2.op-contract-replace-route ec2.op-contract-replace-route-table-association
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2.op-contract-associate-address-already-associated-rejected|run#33|AssertionError|assert message2 == expected2, f"expected message {expected2!r}, got {message2!r}"
Issue-Key: ec2.op-contract-replace-route|run#20|AssertionError|assert code == "InvalidParameterValue", f"expected InvalidParameterValue, got {code}"
Issue-Key: ec2.op-contract-replace-route-table-association|run#20|AssertionError|assert code == "InvalidAssociationID.NotFound", (
Issue-Commit: cd94cc265dda0020a7368311398ddefc098e50a1

# ec2: three refusal classifications/messages diverge - AssociateAddress malformed-instance message, ReplaceRoute missing-route code, ReplaceRouteTableAssociation unknown-association code

> _Note: this issue was identified against the code base at commit `cd94cc265dda0020a7368311398ddefc098e50a1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ReplaceRoute

## Description
Three net-new causes, all on EC2 control-plane refusal representation. Each is independent; none is blocked by an unimplemented stub.

Cause 1 of 3: AssociateAddress with a well-formed-looking but unissued instance id is classified correctly but worded by the emulator. AWS: the error code is InvalidInstanceID.Malformed and the message is exactly 'Invalid id: "<the instance id from the request>"' - fixed text 'Invalid id: ' plus the request's id in double quotes. Smithy: ec2 AssociateAddress (ec2Query) models no per-operation errors, so this is recorded behaviour only; the code already matches. Emulator: serves "The instance ID '<id>' is malformed" instead. Why: a client matching AWS's message text (or surfacing it to users) sees different prose for the same code. Fix: the AssociateAddress handler's validation of InstanceId - emit AWS's 'Invalid id: "..."' wording. Failing check: ec2.op-contract-associate-address-already-associated-rejected.

Cause 2 of 3: ReplaceRoute against a destination CIDR that has no existing route in the table is given the wrong error code. AWS: it is rejected as InvalidParameterValue with message "There is no route defined for '<destination cidr from the request>' in the route table. Use CreateRoute instead." (the InvalidRoute.NotFound code some SDK docs list is not what the service returns for this operation). Smithy: ec2 ReplaceRoute (ec2Query) models no per-operation errors - recorded behaviour only. Emulator: answers InvalidRoute.NotFound. Why: a client that retries or branches on InvalidParameterValue for 'no such route' (the documented contract for ReplaceRoute) does not recognise the emulator's code and treats a validation failure as a not-found. Fix: the ReplaceRoute handler's missing-route branch - classify as InvalidParameterValue and emit that message. Failing check: ec2.op-contract-replace-route.

Cause 3 of 3: ReplaceRouteTableAssociation with an unknown but correctly formatted 8-hex-character association id is refused as malformed instead of not-found. AWS: route-table association ids use the 8-hex-char form, so 'rtbassoc-12345678' passes the format check and the existence check refuses it with code InvalidAssociationID.NotFound and message "The association ID '<association id from the request>' does not exist". Smithy: ec2 ReplaceRouteTableAssociation (ec2Query) models no per-operation errors - recorded behaviour only. Emulator: answers InvalidRouteTableAssociationId.Malformed, i.e. it rejects the id's shape. Why: a client cannot distinguish a typo'd id from a deleted association; code-based handling of NotFound never fires, and the emulator's code is not one AWS emits for this operation. Fix: the ReplaceRouteTableAssociation handler's id validation - accept the 8-hex form and fall through to the existence check with InvalidAssociationID.NotFound. Failing check: ec2.op-contract-replace-route-table-association.

Traces: the three check ids listed above (also in trace_ids).

## Evidence
Cause 1 - expected message 'Invalid id: "i-<id>"'; actual: "The instance ID 'i-<id>' is malformed" (replay: expected message 'Invalid id: "i-f0000000000000001"', got "The instance ID 'i-f0000000000000001' is malformed"; the id itself is the check's unissued placeholder, not part of the requirement).

Cause 2 - expected code InvalidParameterValue with message "There is no route defined for '10.55.0.0/24' in the route table. Use CreateRoute instead."; actual: AssertionError: expected InvalidParameterValue, got InvalidRoute.NotFound (the CIDR is the request's value).

Cause 3 - expected code InvalidAssociationID.NotFound with message "The association ID 'rtbassoc-12345678' does not exist"; actual: AssertionError: expected InvalidAssociationID.NotFound, got InvalidRouteTableAssociationId.Malformed.

## Steps to Reproduce
Cause 1: AllocateAddress(Domain='vpc'), then AssociateAddress(AllocationId=<alloc>, InstanceId='i-f0000000000000001') for an id never issued - read Error.Code/Message.
Cause 2: CreateRouteTable in a VPC, CreateRoute(DestinationCidrBlock='10.99.0.0/24', NetworkInterfaceId=<eni>), then ReplaceRoute(RouteTableId=<rt>, DestinationCidrBlock='10.55.0.0/24', NetworkInterfaceId=<eni>) - read Error.Code/Message.
Cause 3: Create a VPC, subnet and two route tables, AssociateRouteTable, then ReplaceRouteTableAssociation(AssociationId='rtbassoc-12345678', RouteTableId=<rt2>) - read Error.Code/Message.
