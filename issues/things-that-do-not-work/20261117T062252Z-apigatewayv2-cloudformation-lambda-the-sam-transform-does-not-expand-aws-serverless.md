Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigatewayv2__cloudformation__lambda.sam-websocketapi-missing-route-selection-expression-rejected apigatewayv2__cloudformation__lambda.sam-websocketapi-round-trip-and-removed-on-delete
Issue-Key: apigatewayv2__cloudformation__lambda.sam-websocketapi-missing-route-selection-expression-rejected|run#9|AssertionError|assert got == ("FAILED", _TRANSFORM_REASON), (
Issue-Key: apigatewayv2__cloudformation__lambda.sam-websocketapi-round-trip-and-removed-on-delete|run#10|AssertionError|assert status == "CREATE_COMPLETE", (
Issue-Commit: 090d1e6cd43e0643cbf490ec920c2df0995b313e

# The SAM transform does not expand AWS::Serverless::WebSocketApi — CloudFormation refuses every WebSocket SAM template as an unrecognized resource type

> _Note: this issue was identified against the code base at commit `090d1e6cd43e0643cbf490ec920c2df0995b313e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateChangeSet

## Description
Cause 1 of 1: a template carrying `Transform: AWS::Serverless-2016-10-31` and an `AWS::Serverless::WebSocketApi` resource is not transformed at all — the serverless resource survives the transform and is then refused by CloudFormation's resource-type registry, so neither the valid WebSocket API nor its documented SAM validation refusal can be reached. Both failing checks below are this one root cause: one exercises the happy path, the other the missing-required-property path, and both die on the same template-format refusal.

AWS: `AWS::Serverless::WebSocketApi` is a resource type of the SAM transform (`AWS::Serverless-2016-10-31`), so CloudFormation macro-expands it into AWS::ApiGatewayV2::Api (ProtocolType WEBSOCKET) plus its stage/deployment/route children before any resource-type validation runs; the transformed template contains only AWS::* native types and the stack reaches CREATE_COMPLETE, with Ref on the serverless resource resolving to the minted ApiId (Smithy, service-com-amazonaws-apigatewayv2-apigatewayv2: CreateApi input ProtocolType : ProtocolType (WEBSOCKET|HTTP) and RouteSelectionExpression; GetApis — GET /v2/apis — lists the created api under Items[].ApiId/Name/ProtocolType). When a required SAM property is missing the transform itself fails, and the change set's Status/StatusReason is the SAM validator's text, not a type-registry message: `FAILED` with StatusReason `Transform AWS::Serverless-2016-10-31 failed with: Invalid Serverless Application Specification document. Number of errors found: 1. Resource with id [<LogicalId>] is invalid. Missing required property 'RouteSelectionExpression'.` — fixed text apart from the error count and the bracketed logical id, both taken from the submitted template.

Emulator: the change set is refused with Status `FAILED` and StatusReason `Template format error: Unrecognized resource types: [AWS::Serverless::WebSocketApi]` for BOTH templates — the valid one and the invalid one. The valid-template check therefore never gets a stack (`stack sim2real-sam-wsapi-<id> ended None (change set 'FAILED': 'Template format error: Unrecognized resource types: [AWS::Serverless::WebSocketApi]')`), and the negative check sees the type-registry refusal where AWS reports the SAM validator's missing-property sentence.

Why: a client cannot deploy any SAM WebSocket API against the emulator — the template is rejected before a single resource is created, so no api exists in apigatewayv2, GetApis lists nothing, and no @connections/WebSocket estate can be stood up. Worse for validation work, a genuinely malformed SAM template is refused for the wrong reason: a client that keys recovery on the SAM validator's `Invalid Serverless Application Specification document` text (or on which property is named) sees an unrelated `Template format error` instead, so the error is indistinguishable from an unsupported-type deployment problem.

Fix: the SAM (`AWS::Serverless-2016-10-31`) transform in the cloudformation crate — AWS::Serverless::WebSocketApi must be registered as a transformable serverless type, expanded into the equivalent AWS::ApiGatewayV2::Api/Stage/Deployment/Route resources (creating the api in the same apigatewayv2 state GetApis reads and propagating its ApiId as Ref), and its required-property validation must run inside the transform so the failure is reported as the transform's own StatusReason rather than reaching the resource-type registry at all.

Related but not covering: open issue 20261109T154749Z-apigatewayv2-cloudformation-lambda-3-net-new-causes-the.md describes these same two checks failing LATER (the stack reaching CREATE_COMPLETE for the invalid template, and GetApis listing nothing for a deployed one); it carries different replay keys and its causes assume the template is accepted. The template is now refused outright, which is an earlier and distinct defect.

Traces: apigatewayv2__cloudformation__lambda.sam-websocketapi-missing-route-selection-expression-rejected, apigatewayv2__cloudformation__lambda.sam-websocketapi-round-trip-and-removed-on-delete.

## Evidence
apigatewayv2__cloudformation__lambda.sam-websocketapi-missing-route-selection-expression-rejected [mismatch]
  AssertionError: the transform must refuse the missing property: expected ('FAILED', "Transform AWS::Serverless-2016-10-31 failed with: Invalid Serverless Application Specification document. Number of errors found: 1. Resource with id [WsApi] is invalid. Missing required property 'RouteSelectionExpression'."), got ('FAILED', 'Template format error: Unrecognized resource types: [AWS::Serverless::WebSocketApi]')
  Fixed text of the expected message: "Transform AWS::Serverless-2016-10-31 failed with: Invalid Serverless Application Specification document. Number of errors found: " / ". Resource with id [" / "] is invalid. Missing required property '" / "'." — the variable parts are the error count (1 here, one bad property), the logical id from the template (WsApi) and the member name the SAM schema requires (RouteSelectionExpression).

apigatewayv2__cloudformation__lambda.sam-websocketapi-round-trip-and-removed-on-delete [mismatch]
  AssertionError: stack sim2real-sam-wsapi-2b0623735fe7 ended None (change set 'FAILED': 'Template format error: Unrecognized resource types: [AWS::Serverless::WebSocketApi]'): []
  Expected: CREATE_COMPLETE, with the api listed by apigatewayv2 GetApis and removed after DeleteStack. The stack name is a per-run generated value; the requirement is that a valid SAM WebSocket template is transformed and deployed.

## Steps to Reproduce
1. Build and boot the emulator from this checkout.
2. Submit a template with `Transform: AWS::Serverless-2016-10-31` and one `AWS::Serverless::WebSocketApi` resource that declares RouteSelectionExpression (e.g. `$request.body.action`) plus a Lambda-backed $connect/$default route, via CreateChangeSet + DescribeChangeSet.
   Observed: the change set reaches Status FAILED with StatusReason "Template format error: Unrecognized resource types: [AWS::Serverless::WebSocketApi]"; no stack and no apigatewayv2 api is created, and GetApis lists nothing.
   Expected: the transform expands the resource, the change set is CREATE_COMPLETE, ExecuteChangeSet reaches CREATE_COMPLETE, and GetApis lists the api under the template's Name with ProtocolType WEBSOCKET; DeleteStack removes it.
3. Submit the same template with RouteSelectionExpression omitted.
   Observed: the same "Template format error: Unrecognized resource types" StatusReason.
   Expected: Status FAILED with StatusReason "Transform AWS::Serverless-2016-10-31 failed with: Invalid Serverless Application Specification document. Number of errors found: 1. Resource with id [<LogicalId>] is invalid. Missing required property 'RouteSelectionExpression'."
4. Replay the two checks in trace_ids to confirm the fix.
