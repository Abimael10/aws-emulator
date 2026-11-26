Issue-Source: trace-sweep-divergence
Issue-Trace: ecs-gateway-required-field-validation-errors
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ecs-gateway-required-field-validation-errors
Issue-Shape-Id: com.amazonaws.ecs#AmazonEC2ContainerServiceV20141113
Issue-Key: ecs|CreateGateway|4xx->4xx|$.message
Issue-Key: ecs|CreateGateway|5xx->4xx|$.__type
Issue-Key: ecs|CreateGateway|5xx->4xx|$.message
Issue-Key: ecs|CreateGateway|5xx->4xx|status
Issue-Key: ecs|CreateGateway|4xx->4xx|$.__type
Issue-Key: ecs|UpdateGateway|4xx->4xx|$.message
Issue-Key: ecs|AssociateGatewayWithCluster|4xx->4xx|$.__type
Issue-Key: ecs|AssociateGatewayWithCluster|4xx->4xx|$.message
Issue-Key: ecs|DescribeGatewayRoute|4xx->4xx|$.message
Issue-Commit: 16caeccf34fd30fafc2febabf9312168f85b2a49

# ECS gateway operations answer a generic Smithy required-member error instead of AWS's refusal class and message

> _Note: this issue was identified against the code base at commit `16caeccf34fd30fafc2febabf9312168f85b2a49`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateGateway

## Description
Every ECS gateway operation refuses a request with a missing required member by running a generic Smithy required-member check and reporting InvalidParameterException with the message template "1 validation error detected: Value null at '<member>' failed to satisfy constraint: Member must not be null". AWS does not refuse these requests that way: each gateway operation has its own refusal for the member it is missing, with its own error shape, its own HTTP status and its own message, and in two of the six cases AWS does not treat the missing member as a request-validation error at all. 6 causes. Related but NOT covering: open issue 20261117T121318Z-ecs-creategateway-stub.md ('Implement ecs CreateGateway: unimplemented scaffold stub') — CreateGateway now answers, so these refusals are live behavior and that stub issue carries none of these replay keys.

Cause 1 of 6: CreateGateway with no `name`.
AWS: Smithy: CreateGateway input member `name` is @required; modeled errors are AccessDeniedException(403), ClientException(400), InvalidParameterException(400), ServerException(500), UnsupportedFeatureException(400). The recorded refusal is HTTP 400 with message 'GatewayName must be provided.' — fixed text, no variable part.
Emulator: HTTP 400 with message "1 validation error detected: Value null at 'name' failed to satisfy constraint: Member must not be null" (fixed text is the generic Smithy template; 'name' is the member name from the request).
Why: a client that matches on AWS's message to tell the user which gateway field it forgot, or that logs the refusal, sees wording AWS never emits.
Fix: the CreateGateway handler's input validation, which must refuse a missing name itself before the generic required-member pass.
Traces: ecs-gateway-required-field-validation-errors#task#1.

Cause 2 of 6: CreateGateway with no `infrastructureRoleArn` — wrong HTTP STATUS CLASS and wrong error shape.
AWS: recorded behavior only for the trigger; the recording is HTTP 500 with __type 'ServerException' and message 'Service Unavailable. Please try again later.' (all fixed text). Smithy: ServerException is modeled on CreateGateway with errorKind=server, httpError 500.
Emulator: HTTP 400 with __type 'InvalidParameterException' and the generic "1 validation error detected: Value null at 'infrastructureRoleArn' failed to satisfy constraint: Member must not be null".
Why: this is a retryability inversion — an AWS SDK retries a 500 ServerException and does not retry a 400 InvalidParameterException, so the caller's error handling takes the opposite branch from the one AWS drives it to.
Fix: the CreateGateway handler — the missing infrastructureRoleArn path must not be answered by the generic required-member check.
Traces: ecs-gateway-required-field-validation-errors#task#2.

Cause 3 of 6: CreateGateway with no `configuration`.
AWS: recorded behavior only for the trigger; the recording is HTTP 400 with __type 'AccessDeniedException' and message 'Cross-account pass role is not allowed' (fixed text). Smithy: AccessDeniedException is modeled on CreateGateway; CreateGateway$configuration is @required and targets the GatewayConfiguration union.
Emulator: __type 'InvalidParameterException' with "1 validation error detected: Value null at 'configuration' failed to satisfy constraint: Member must not be null".
Why: a client distinguishes an authorization refusal from a malformed-request refusal to decide whether to fix the role or the request body; it is pushed to the wrong one.
Fix: the CreateGateway handler's refusal ordering and classification.
Traces: ecs-gateway-required-field-validation-errors#task#3.

Cause 4 of 6: UpdateGateway with no `gateway`.
AWS: Smithy: UpdateGateway$gateway is @required and identifies the gateway; errors include GatewayNotFoundException and InvalidParameterException. The recorded refusal message is 'The gateway identifier is invalid' (fixed text).
Emulator: "1 validation error detected: Value null at 'gateway' failed to satisfy constraint: Member must not be null".
Why: AWS words this as an identifier-validity refusal; a client parsing the message cannot tell the identifier was rejected.
Fix: the UpdateGateway handler's gateway-identifier validation.
Traces: ecs-gateway-required-field-validation-errors#task#4.

Cause 5 of 6: AssociateGatewayWithCluster with no `cluster`.
AWS: Smithy: AssociateGatewayWithCluster$cluster is @required; modeled errors include AccessDeniedException, ClusterNotFoundException, GatewayNotFoundException. The recorded refusal is __type 'AccessDeniedException' with message 'You are not authorized to perform this operation on the specified gateway, or it does not exist.' (fixed text).
Emulator: __type 'InvalidParameterException' with "1 validation error detected: Value null at 'cluster' failed to satisfy constraint: Member must not be null".
Why: AWS deliberately conflates not-authorized with does-not-exist here; a client that treats AccessDeniedException as a permissions problem to surface to an operator instead sees a malformed-request error.
Fix: the AssociateGatewayWithCluster handler's authorization/addressing refusal.
Traces: ecs-gateway-required-field-validation-errors#task#5.

Cause 6 of 6: DescribeGatewayRoute with no `gatewayRouteArn`.
AWS: Smithy: DescribeGatewayRoute$gatewayRouteArn is @required; errors include InvalidParameterException and ResourceNotFoundException. The recorded refusal message is 'The gateway route ARN is invalid' (fixed text).
Emulator: "1 validation error detected: Value null at 'gatewayRouteArn' failed to satisfy constraint: Member must not be null".
Why: as cause 4 — the message a client surfaces or matches on is not AWS's.
Fix: the DescribeGatewayRoute handler's ARN validation.
Traces: ecs-gateway-required-field-validation-errors#task#6.

## Evidence
All six steps are in fixture ecs-gateway-required-field-validation-errors.
Cause 1 — task#1, CreateGateway, 400 vs 400: `$.message` expected 'GatewayName must be provided.', got "1 validation error detected: Value null at 'name' failed to satisfy constraint: Member must not be null".
Cause 2 — task#2, CreateGateway: status expected 500, got 400; `$.__type` expected 'ServerException', got 'InvalidParameterException'; `$.message` expected 'Service Unavailable. Please try again later.', got "1 validation error detected: Value null at 'infrastructureRoleArn' failed to satisfy constraint: Member must not be null".
Cause 3 — task#3, CreateGateway, 400 vs 400: `$.__type` expected 'AccessDeniedException', got 'InvalidParameterException'; `$.message` expected 'Cross-account pass role is not allowed', got "1 validation error detected: Value null at 'configuration' failed to satisfy constraint: Member must not be null".
Cause 4 — task#4, UpdateGateway, 400 vs 400: `$.message` expected 'The gateway identifier is invalid', got "1 validation error detected: Value null at 'gateway' failed to satisfy constraint: Member must not be null".
Cause 5 — task#5, AssociateGatewayWithCluster, 400 vs 400: `$.__type` expected 'AccessDeniedException', got 'InvalidParameterException'; `$.message` expected 'You are not authorized to perform this operation on the specified gateway, or it does not exist.', got "1 validation error detected: Value null at 'cluster' failed to satisfy constraint: Member must not be null".
Cause 6 — task#6, DescribeGatewayRoute, 400 vs 400: `$.message` expected 'The gateway route ARN is invalid', got "1 validation error detected: Value null at 'gatewayRouteArn' failed to satisfy constraint: Member must not be null".
In every case the emulator's message is the generic Smithy required-member template with the request's member name substituted; AWS's messages above are entirely fixed text with no per-run values.

## Steps to Reproduce
1. Replay fixture ecs-gateway-required-field-validation-errors against the emulator.
2. task#1: CreateGateway with `name` omitted -> expect 400 'GatewayName must be provided.'; emulator returns the generic required-member message.
3. task#2: CreateGateway with `infrastructureRoleArn` omitted -> expect 500 ServerException 'Service Unavailable. Please try again later.'; emulator returns 400 InvalidParameterException.
4. task#3: CreateGateway with `configuration` omitted -> expect 400 AccessDeniedException 'Cross-account pass role is not allowed'.
5. task#4: UpdateGateway with `gateway` omitted -> expect 400 'The gateway identifier is invalid'.
6. task#5: AssociateGatewayWithCluster with `cluster` omitted -> expect 400 AccessDeniedException 'You are not authorized to perform this operation on the specified gateway, or it does not exist.'.
7. task#6: DescribeGatewayRoute with `gatewayRouteArn` omitted -> expect 400 'The gateway route ARN is invalid'.
