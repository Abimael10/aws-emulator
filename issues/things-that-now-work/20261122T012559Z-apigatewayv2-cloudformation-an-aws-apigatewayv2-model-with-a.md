Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigatewayv2__cloudformation.deploy-apigatewayv2-cfn-websocket-response-and-model-removal
Issue-Key: apigatewayv2__cloudformation.deploy-apigatewayv2-cfn-websocket-response-and-model-removal|run#8|AssertionError|assert status == "CREATE_COMPLETE", (
Issue-Commit: 1bab6ccfadd8e456dfdfaa55eb16233f496412b2

# apigatewayv2__cloudformation: an AWS::ApiGatewayV2::Model with a valid draft-04 object schema is CREATE_FAILED, rolling the whole WebSocket stack back to ROLLBACK_COMPLETE

> _Note: this issue was identified against the code base at commit `1bab6ccfadd8e456dfdfaa55eb16233f496412b2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
One net-new cause behind 1 failing apigatewayv2__cloudformation check. This is a DIFFERENT cause from the open issue 20261109T123922Z-apigatewayv2-cloudformation-3-net-new-causes-aws-apigatewayv2.md (whose cause 2 is an INVALID Model schema being accepted, and whose cause 3 is GetRouteResponses returning nothing): here a VALID Model is refused outright and the stack no longer reaches CREATE_COMPLETE at all, so that issue's contract is now blocked even earlier. It is not covered by that issue.

Cause 1 of 1: the AWS::ApiGatewayV2::Model resource handler fails to create a model whose Schema is a well-formed JSON-Schema draft-04 object, and the failure rolls the whole stack back.

AWS: recorded behavior only for the CloudFormation resource; the underlying operation is apigatewayv2 CreateModel (POST /v2/apis/{ApiId}/models, 201), whose required members are Name (StringWithLengthBetween1And128) and Schema (StringWithLengthBetween0And32K), with optional ContentType (StringWithLengthBetween1And256). The recording shows that a template declaring AWS::ApiGatewayV2::Model with ApiId={"Ref": WsApi}, a Name of word characters only, ContentType "application/json" and a Schema of {"$schema": "http://json-schema.org/draft-04/schema#", "title": ..., "type": "object", "properties": {"action": {"type": "string"}}} creates successfully: the stack settles CREATE_COMPLETE, Ref of the Model yields its ModelId, and GetModels for the API returns exactly that (ModelId, Name, ContentType, Schema) — i.e. the schema given to CloudFormation as a JSON object is accepted and read back as the equivalent schema string. Only a later UPDATE that drops the resource removes the model (after which GetModel answers 404 NotFoundException "Invalid model identifier specified: <modelId>" — fixed text "Invalid model identifier specified: " plus the requested model id from the request).

Emulator: the Model resource reaches CREATE_FAILED, which fails the create and rolls the stack back; the observed decisive event sequence is WsApi CREATE_COMPLETE, Integration CREATE_COMPLETE, Model CREATE_FAILED, IntegrationResponse/Route/RouteResponse CREATE_COMPLETE, then every resource DELETE_COMPLETE and the stack ROLLBACK_COMPLETE. Every sibling resource in the template creates fine, so the refusal is specific to the Model resource. Hypothesis: the Model handler's Schema validation rejects the draft-04 object form that CloudFormation passes (the `Schema` property arrives as a JSON object, not a pre-serialized string) or rejects the "$schema"/"title" keywords, and the handler reports the resource failed instead of serializing the object and calling CreateModel.

Why: no client can provision an API Gateway V2 Model from a template. A stack that AWS creates successfully is rolled back wholesale, so the API, integration, route, route response and integration response declared alongside the model are destroyed too — the user sees a total deployment failure for a template that is valid on AWS, and request/response model validation built on that model is unreachable.

Fix: the AWS::ApiGatewayV2::Model CloudFormation resource handler's create path and its Schema handling — it must accept the Schema property in the JSON-object form a template supplies (serializing it for CreateModel), accept standard draft-04 schema keywords, and succeed so the stack reaches CREATE_COMPLETE with Ref resolving to the created ModelId and GetModels returning it.

Traces: apigatewayv2__cloudformation.deploy-apigatewayv2-cfn-websocket-response-and-model-removal.

## Evidence
Failing check apigatewayv2__cloudformation.deploy-apigatewayv2-cfn-websocket-response-and-model-removal:

  File "check_apigatewayv2__cloudformation_deploy_apigatewayv2_cfn_websocket_response_and_model_removal.py", line 242, in run
    assert status == "CREATE_COMPLETE", (
  AssertionError: stack ended 'ROLLBACK_COMPLETE': [('WsApi', 'CREATE_COMPLETE'), ('Integration', 'CREATE_COMPLETE'), ('Model', 'CREATE_FAILED'), ('IntegrationResponse', 'CREATE_COMPLETE'), ('Route', 'CREATE_COMPLETE'), ('RouteResponse', 'CREATE_COMPLETE'), ('Model', 'DELETE_COMPLETE'), ('RouteResponse', 'DELETE_COMPLETE'), ('IntegrationResponse', 'DELETE_COMPLETE'), ('Route', 'DELETE_COMPLETE'), ('Integration', 'DELETE_COMPLETE'), ('<stack>', 'ROLLBACK_COMPLETE')]

Expected: stack status CREATE_COMPLETE, with the Model row reaching CREATE_COMPLETE; Outputs.ModelId = Ref(Model); GetModels(ApiId) returning one item whose (ModelId, Name, ContentType, json.loads(Schema)) equals (that ModelId, the templated model name, "application/json", the draft-04 object given in the template).
Actual: the Model row is CREATE_FAILED and the stack settles ROLLBACK_COMPLETE, deleting every other resource in the template. All sibling apigatewayv2 resource types (Api, Integration, IntegrationResponse, Route, RouteResponse) create successfully in the same stack.

The template's Model properties: ApiId={"Ref":"WsApi"}, Name=<word-character name>, ContentType="application/json", Schema={"$schema":"http://json-schema.org/draft-04/schema#","title":<title>,"type":"object","properties":{"action":{"type":"string"}}} (the concrete name/title are per-run values, not part of the requirement).

## Steps to Reproduce
1. CreateStack with a template containing AWS::ApiGatewayV2::Api (ProtocolType WEBSOCKET, RouteSelectionExpression "$request.body.action"), an AWS::ApiGatewayV2::Integration (MOCK), and an AWS::ApiGatewayV2::Model with ApiId={"Ref": <api>}, a valid Name, ContentType "application/json" and Schema as a JSON object: {"$schema": "http://json-schema.org/draft-04/schema#", "title": "SimParityModel", "type": "object", "properties": {"action": {"type": "string"}}}.
2. Wait for the stack to settle and read DescribeStackEvents. Expect CREATE_COMPLETE with the Model CREATE_COMPLETE; observe Model CREATE_FAILED and the stack ROLLBACK_COMPLETE.
3. On a passing emulator, continue: GetModels(ApiId) must return the model with the schema read back, and an UpdateStack that drops the Model must make GetModel answer NotFoundException "Invalid model identifier specified: <modelId>".
4. Re-replay apigatewayv2__cloudformation.deploy-apigatewayv2-cfn-websocket-response-and-model-removal.
