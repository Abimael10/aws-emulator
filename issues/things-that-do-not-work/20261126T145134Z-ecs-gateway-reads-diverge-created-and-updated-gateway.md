Issue-Source: trace-sweep-divergence
Issue-Trace: ecs-gateway-lifecycle-and-deletion-protection
Issue-Step: task#4
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ecs-gateway-lifecycle-and-deletion-protection ecs-gateway-not-found-error-paths
Issue-Shape-Id: com.amazonaws.ecs#AmazonEC2ContainerServiceV20141113
Issue-Key: ecs|DescribeGateway|2xx->2xx|$.gateway.configuration.hostSharedAlb.networkConfiguration.ipAddressType
Issue-Key: ecs|DescribeGateway|2xx->2xx|$.gateway.resourceManagementType
Issue-Key: ecs|DescribeGateway|2xx->2xx|$
Issue-Key: ecs|DescribeGateway|2xx->2xx|$.gateway.deletionProtection
Issue-Key: ecs|DescribeGateway|2xx->2xx|$.gateway.status
Issue-Key: ecs|DescribeGateway|4xx->4xx|$.__type
Issue-Key: ecs|DescribeGateway|4xx->4xx|$.message
Issue-Commit: e1287cf409e72f27d41b5362b1ef9b2f278addc7

# ECS gateway reads diverge: created and updated gateway state is not reflected and a missing gateway is misclassified

> _Note: this issue was identified against the code base at commit `e1287cf409e72f27d41b5362b1ef9b2f278addc7`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeGateway

## Description
The ECS gateway resource the emulator stores does not carry what the caller asked for and does not change when it is updated, and a read of a gateway that is not there is refused with the wrong error class. 3 causes.

Related but NOT covering: open issue 20261120T005727Z-ecs-gateway-operations-answer-a-generic-smithy-required.md covers only the missing-required-member refusals of CreateGateway/UpdateGateway/AssociateGatewayWithCluster/DescribeGatewayRoute (fixture ecs-gateway-required-field-validation-errors) and carries none of the replay keys below; open issue 20261126T031516Z-ecs-listgateways-stub.md covers the unimplemented ListGateways operation only.

Cause 1 of 3: DescribeGateway of a just-created gateway ignores the caller's network configuration and omits resourceManagementType.
AWS: Smithy: CreateGateway$configuration is @required and targets the GatewayConfiguration union, whose hostSharedAlb variant is HostSharedAlbConfiguration; its networkConfiguration targets GatewayElbNetworkConfiguration, whose ipAddressType member is documented 'The IP address type for the load balancers that the gateway manages. The default is dualstack.' — the default applies only when the caller omits the member, so a gateway created with ipAddressType ipv4 reads back as ipv4 under gateway.configuration.hostSharedAlb.networkConfiguration.ipAddressType (DescribeGateway returns DescribeGatewayConfiguration/DescribeGatewayAlbConfiguration, which carries the same GatewayElbNetworkConfiguration). For resourceManagementType: recorded behavior only — the recording shows DescribeGateway returning gateway.resourceManagementType = 'CUSTOMER' for a customer-created gateway; the checked-in model declares the enum ResourceManagementType (values CUSTOMER, ECS) but the Gateway shape in this model revision does not declare the member, so the recording is the authority.
Emulator: DescribeGateway answers 200 but reports gateway.configuration.hostSharedAlb.networkConfiguration.ipAddressType = 'dualstack' for a gateway created with ipAddressType = 'ipv4' (the create-time value is discarded and the default substituted), and omits gateway.resourceManagementType entirely.
Why: a client that creates an IPv4-only gateway and reads it back to confirm its address family is told the gateway is dualstack, and a client that branches on resourceManagementType to tell a customer-managed gateway from an ECS-managed one finds the member absent and cannot decide. Any read-after-create reconciliation (CloudFormation-style drift checking, an operator console) reports a gateway the caller never asked for.
Fix: the gateway state record and the CreateGateway handler that populates it — the caller-supplied hostSharedAlb.networkConfiguration must be stored instead of defaulted — together with the DescribeGateway serializer, which must emit resourceManagementType.
Traces: ecs-gateway-lifecycle-and-deletion-protection#task#4.

Cause 2 of 3: UpdateGateway changes nothing and the gateway's status never leaves ACTIVE, so the update lifecycle never settles.
AWS: Smithy: UpdateGateway takes gateway (@required), deletionProtection and an UpdateGatewayConfiguration, and returns the gateway's status (GatewayStatus). GatewayStatus documents the update lifecycle explicitly: UPDATE_IN_PROGRESS 'An update to the gateway is in progress', UPDATE_COMPLETE 'The update to the gateway completed successfully', UPDATE_FAILED 'The update to the gateway failed. See statusReason for details'. So after an UpdateGateway a DescribeGateway poll reaches a terminal UPDATE_COMPLETE (or UPDATE_FAILED) and the gateway reads back with the requested change applied — here deletionProtection true, the value the request set.
Emulator: after UpdateGateway, DescribeGateway keeps returning gateway.status = 'ACTIVE' (never UPDATE_IN_PROGRESS and never a terminal UPDATE_COMPLETE/UPDATE_FAILED) — the fixture's poll made 19 attempts and was cut at its 180 s cap — and gateway.deletionProtection stays false although the update requested true. The same read also still shows ipAddressType 'dualstack' and no resourceManagementType (cause 1).
Why: UpdateGateway is a no-op that reports success, so deletion protection is silently not in force and a caller that deletes the gateway afterwards succeeds where AWS would refuse. A client that waits for the documented UPDATE_COMPLETE state before continuing waits forever, because the status it waits for is never reachable.
Fix: the UpdateGateway handler and the gateway state it mutates — the requested deletionProtection and configuration must be applied, and the gateway must transition through UPDATE_IN_PROGRESS to a terminal UPDATE_COMPLETE/UPDATE_FAILED that DescribeGateway reports.
Traces: ecs-gateway-lifecycle-and-deletion-protection#task#6.

Cause 3 of 3: DescribeGateway on a gateway that does not exist is refused GatewayNotFoundException instead of AWS's AccessDeniedException.
AWS: Smithy: DescribeGateway models AccessDeniedException ('You don't have authorization to perform the requested action'), ClientException, GatewayNotFoundException, InvalidParameterException, ServerException and UnsupportedFeatureException. The recorded refusal for an unknown gateway identifier is __type 'AccessDeniedException' with message 'You are not authorized to perform this operation on the specified gateway, or it does not exist.' — entirely fixed text, with no part taken from the request or from state. AWS deliberately conflates not-authorized with does-not-exist here so that a caller cannot probe for gateways it cannot see; the same conflated message is what AssociateGatewayWithCluster is recorded answering. (The recorded HTTP status is 400 and the emulator also answers 400, so the status itself does not diverge, even though the model marks AccessDeniedException httpError 403.)
Emulator: 400 with __type 'GatewayNotFoundException' and message 'Gateway not found.' (fixed text of its own, no variable part).
Why: a client that handles AccessDeniedException as an operator-facing permissions problem — surface the role/policy to fix, do not retry, do not create — instead receives a not-found error and takes the resource-lifecycle branch: it concludes the gateway is absent and may try to create a gateway it is simply not allowed to see. Error-code matching on the modeled AccessDeniedException never fires.
Fix: the DescribeGateway handler's addressing/authorization refusal — an unresolvable gateway identifier must be refused with AccessDeniedException and AWS's conflated message, not with GatewayNotFoundException.
Traces: ecs-gateway-lifecycle-and-deletion-protection#task#12, ecs-gateway-not-found-error-paths#task#3.

## Evidence
Cause 1 — fixture ecs-gateway-lifecycle-and-deletion-protection, step task#4, DescribeGateway, 200 vs 200:
  $.gateway.configuration.hostSharedAlb.networkConfiguration.ipAddressType: expected 'ipv4', got 'dualstack'
  $.gateway.resourceManagementType: expected 'CUSTOMER', got (member absent)

Cause 2 — fixture ecs-gateway-lifecycle-and-deletion-protection, step task#6, DescribeGateway, 200 vs 200 (after UpdateGateway):
  $.gateway.status: expected 'UPDATE_COMPLETE', got 'ACTIVE'; poll: not in ['UPDATE_COMPLETE','UPDATE_FAILED'] after 19 attempt(s), capped at 180 s
  $.gateway.deletionProtection: expected true, got false
  $.gateway.configuration.hostSharedAlb.networkConfiguration.ipAddressType: expected 'ipv4', got 'dualstack'
  $.gateway.resourceManagementType: expected 'CUSTOMER', got (member absent)
  (the expected body also carries the four subnets the create request named; the emulator's networkConfiguration carries no subnets at all)

Cause 3 — fixture ecs-gateway-lifecycle-and-deletion-protection, step task#12, DescribeGateway, 400 vs 400 (same cluster: ecs-gateway-not-found-error-paths#task#3):
  $.__type: expected 'AccessDeniedException', got 'GatewayNotFoundException'
  $.message: expected 'You are not authorized to perform this operation on the specified gateway, or it does not exist.', got 'Gateway not found.'
  Both messages are wholly fixed text; neither interpolates a request value, so the divergence is not a per-run value difference.

All three clusters are reported by the replay with no owning open issue and no blocking stub.

## Steps to Reproduce
1. Replay fixture ecs-gateway-lifecycle-and-deletion-protection against the emulator.
2. task#4: CreateGateway a hostSharedAlb gateway whose configuration.hostSharedAlb.networkConfiguration sets ipAddressType='ipv4' and names explicit subnets, then DescribeGateway it -> expect ipAddressType 'ipv4' and gateway.resourceManagementType 'CUSTOMER'; the emulator reports 'dualstack' and omits resourceManagementType.
3. task#6: UpdateGateway the same gateway with deletionProtection=true, then poll DescribeGateway for a terminal status -> expect status 'UPDATE_COMPLETE' with deletionProtection true; the emulator stays at 'ACTIVE' with deletionProtection false until the poll is cut at its 180 s cap.
4. task#12 (and ecs-gateway-not-found-error-paths#task#3): DescribeGateway with a gateway identifier that does not exist -> expect 400 __type 'AccessDeniedException', message 'You are not authorized to perform this operation on the specified gateway, or it does not exist.'; the emulator answers 400 'GatewayNotFoundException' / 'Gateway not found.'.
