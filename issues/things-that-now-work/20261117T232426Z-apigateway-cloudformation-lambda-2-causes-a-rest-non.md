Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway__cloudformation__lambda.cfn-apigw-lambda-request-integration-mapping-positive apigateway__cloudformation__lambda.cfn-apigw-role-trust-policy-positive
Issue-Key: apigateway__cloudformation__lambda.cfn-apigw-lambda-request-integration-mapping-positive|run#6|AssertionError|assert status == 200, f"expected 200, got {status}: {body!r}"
Issue-Key: apigateway__cloudformation__lambda.cfn-apigw-role-trust-policy-positive|run#7|AssertionError|assert status == 200, f"expected 200, got {status}: {body!r}"
Issue-Commit: 64b7111d560a56db8d780b5c63dd8a34d5bc25c3

# apigateway__cloudformation__lambda: 2 causes — a REST non-proxy (Type: AWS) Lambda integration answers 500 "Internal server error" through a deployed stage, both with resource-based permission and with an apigateway-trusted credentials role

> _Note: this issue was identified against the code base at commit `64b7111d560a56db8d780b5c63dd8a34d5bc25c3`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutIntegration

## Description
Two net-new root causes in the apigateway__cloudformation__lambda scenario. Both were listed in the replay report WITHOUT an 'ALREADY FILED' or 'OWNED by' marker, so neither replay key is carried by an open issue. Related-but-not-covering: open issue 20261108T120220Z-apigateway-cloudformation-lambda-non-proxy-aws-integration-with.md describes the same family of defect (its Cause 1), but it carries only the replay key of apigateway__cloudformation__lambda.cfn-apigw-role-missing-trust-principal-negative; the two checks below are not bound to it and are reported here so they are not closed unrun.

Cause 1 of 2: a REST method whose integration is a non-proxy AWS Lambda integration with NO `credentials` (relying on resource-based permission from AWS::Lambda::Permission) and with a VTL requestTemplate + integrationResponse responseTemplate answers HTTP 500 instead of the mapped 200.
AWS: Smithy (apigateway) Integration.type = AWS; Integration.uri for an AWS integration is of the form arn:aws:apigateway:{region}:lambda:path/2015-03-31/functions/{function-arn}/invocations; Integration.credentials — "To use resource-based permissions on supported Amazon Web Services services, specify null"; Integration.requestTemplates is "a map of Velocity templates that are applied on the request payload"; Integration.passthroughBehavior = WHEN_NO_TEMPLATES; Integration.integrationResponses carries the responseTemplates applied to the backend result. With a lambda:InvokeFunction resource policy granting apigateway.amazonaws.com on the function, a GET through the deployed stage must invoke the function with EXACTLY the mapped payload (here the single key produced by the request template, not a proxy event) and return the integration response — HTTP 200 with the function's JSON return value passed through $input.json('$'). Recorded behavior: 200 with the mapped body.
Emulator: the stage answers HTTP 500 with body {"message":"Internal server error"} — the generic API Gateway 5xx envelope. The request template is never applied and the function's return value never reaches the client.
Why: a client relying on AWS behavior sees every non-proxy AWS Lambda integration as permanently broken: VTL request/response mapping (the whole point of a non-proxy integration) cannot be exercised, and the 500 is indistinguishable from a genuine backend fault, so a caller cannot tell a misconfigured integration from a working one.
Fix: the execute-api data-plane handler for REST integrations of type AWS — resolution of the lambda path-style integration uri, the resource-based-permission authorization path (credentials absent), and application of requestTemplates / integrationResponses.responseTemplates.
Traces: apigateway__cloudformation__lambda.cfn-apigw-lambda-request-integration-mapping-positive.

Cause 2 of 2: the same non-proxy AWS Lambda integration, this time carrying `credentials` = the ARN of an IAM role whose AssumeRolePolicyDocument trusts apigateway.amazonaws.com and whose inline policy allows lambda:InvokeFunction on the target function, also answers HTTP 500 instead of 200.
AWS: Smithy (apigateway) Integration.credentials — "Specifies the credentials required for the integration, if any. For AWS integrations ... To specify an IAM Role for API Gateway to assume, use the role's Amazon Resource Name (ARN)." When that role trusts apigateway.amazonaws.com and permits lambda:InvokeFunction on the function ARN, API Gateway assumes it and the method invokes the function; the stage serves the integration response, HTTP 200 with the handler's body. A 5xx is correct ONLY when the role does not trust apigateway.amazonaws.com. Recorded behavior: 200 with the handler body.
Emulator: the correctly-trusted-role method answers HTTP 500 with body {"message":"Internal server error"} — the same status and body the mis-trusted-role arm is supposed to produce, so the positive and negative arms of integration-credentials behavior are indistinguishable.
Why: integration-credentials/IAM-trust diagnosis is impossible — a working credentials role looks identical to one trusting the wrong principal, and no non-proxy AWS integration that assumes a role can ever serve traffic.
Fix: the execute-api data-plane handler for REST type=AWS integrations — resolution and assumption of the integration `credentials` role, and the downstream integration call.
Traces: apigateway__cloudformation__lambda.cfn-apigw-role-trust-policy-positive.

Message note for both causes: the diverging response is HTTP 500 with the fixed JSON body {"message":"Internal server error"} (API Gateway's generic 5xx envelope — fixed text, no variable parts). The expected response is HTTP 200 whose body is the integration response produced from the function's return value; the body's contents are values from the request and the function, not fixed text. Stack names, api ids and stage hostnames in the recordings are per-run generated values and are not part of the requirement.

## Evidence
Cause 1 — check apigateway__cloudformation__lambda.cfn-apigw-lambda-request-integration-mapping-positive, at check line 118:
  AssertionError: expected 200, got 500: b'{"message":"Internal server error"}'
Expected: HTTP 200 and a JSON body in which receivedKeys == ["greetingTarget"] (proving Lambda saw ONLY the key minted by the request template {"greetingTarget": "$input.params('name')"}, not a proxy event) and message == "Hello, <name-from-querystring>". Actual: HTTP 500, body {"message":"Internal server error"}; the request/response templates are never applied.

Cause 2 — check apigateway__cloudformation__lambda.cfn-apigw-role-trust-policy-positive, at check line 134:
  AssertionError: expected 200, got 500: b'{"message":"Internal server error"}'
Expected: HTTP 200 with the echo handler's body ({"ok": true}) passed through the integration responseTemplate $input.json('$'), because the integration's Credentials role trusts apigateway.amazonaws.com and allows lambda:InvokeFunction on the function ARN. Actual: HTTP 500, body {"message":"Internal server error"} — identical to the refusal expected only of a role trusting the wrong principal.

In both cases the CloudFormation stack itself reached CREATE_COMPLETE (the checks got past _deploy_cfn), so the divergence is at invoke time on the execute-api data plane, not in stack provisioning.

## Steps to Reproduce
Cause 1: CreateStack a template with (a) an AWS::Lambda::Function (python3.12) whose handler returns {'receivedKeys': sorted(event.keys()), 'message': 'Hello, ' + event.get('greetingTarget','stranger')}, (b) an AWS::ApiGateway::RestApi + Resource 'greet' + GET Method with AuthorizationType NONE and an Integration of Type 'AWS', IntegrationHttpMethod 'POST', PassthroughBehavior 'WHEN_NO_TEMPLATES', RequestTemplates {"application/json": "{\"greetingTarget\": \"$input.params('name')\"}"}, Uri arn:aws:apigateway:${AWS::Region}:lambda:path/2015-03-31/functions/${FunctionArn}/invocations, IntegrationResponses [{StatusCode 200, ResponseTemplates {"application/json": "$input.json('$')"}}], and NO Credentials, (c) an AWS::Lambda::Permission granting lambda:InvokeFunction to apigateway.amazonaws.com with SourceArn arn:aws:execute-api:${AWS::Region}:${AWS::AccountId}:${RestApi}/*/*/*, plus a Deployment and a Stage 'test'. Wait for CREATE_COMPLETE, then GET https://<api-id>.execute-api.<region>.<host>/test/greet?name=Alice -> expect HTTP 200 with receivedKeys == ["greetingTarget"] and message == "Hello, Alice"; observe HTTP 500 {"message":"Internal server error"}.

Cause 2: CreateStack a template with an echo AWS::Lambda::Function returning {'ok': True}, an AWS::IAM::Role whose AssumeRolePolicyDocument allows sts:AssumeRole for Principal Service apigateway.amazonaws.com and whose inline policy allows lambda:InvokeFunction on the function ARN, and a REST API with a GET method whose Integration is Type 'AWS', IntegrationHttpMethod 'POST', Credentials = that role's ARN, RequestTemplates {"application/json": "{}"}, Uri arn:aws:apigateway:${AWS::Region}:lambda:path/2015-03-31/functions/${FunctionArn}/invocations, IntegrationResponses [{StatusCode 200, ResponseTemplates {"application/json": "$input.json('$')"}}], plus Deployment and Stage 'test'. Wait for CREATE_COMPLETE, then GET the stage URL /echo -> expect HTTP 200 with the handler body; observe HTTP 500 {"message":"Internal server error"}.
