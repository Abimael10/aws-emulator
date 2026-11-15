Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway__dynamodb__lambda.lambda-describe-apigateway-positive
Issue-Key: apigateway__dynamodb__lambda.lambda-describe-apigateway-positive|run#15|AssertionError|assert body.get("name") == api_name_val, f"API name mismatch: {body}"
Issue-Commit: f7526cbca79906f89a28c5d197946d676b42fe8d

# apigateway__dynamodb__lambda: an in-function GetRestApi never yields the REST API's name — the handler's invocation produces no body, so a role granted apigateway:GET on arn:aws:apigateway:*::/restapis/* cannot describe an API

> _Note: this issue was identified against the code base at commit `f7526cbca79906f89a28c5d197946d676b42fe8d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetRestApi

## Description
Cause 1 of 1: a Lambda handler calling apigateway GetRestApi with an execution role that grants apigateway:GET on the REST-API ARN wildcard never returns the API's name; the invocation comes back with no `body` member at all, i.e. the in-function call raised instead of returning.

AWS: Smithy (apigateway, BackplaneControlService): GetRestApi = GET /restapis/{restApiId}, input {restApiId (required, httpLabel)}, output shape RestApi whose members include `id` and `name` (plus description/createdDate/version/binaryMediaTypes/minimumCompressionSize/apiKeySource/endpointConfiguration/policy/tags/disableExecuteApiEndpoint/rootResourceId/securityPolicy/apiStatus...), HTTP success code 200. Modeled errors: BadRequestException (400), NotFoundException (404), TooManyRequestsException (429), UnauthorizedException (401). The IAM action is `apigateway:GET` and an API Gateway REST-API ARN has the account-less, path-style form `arn:aws:apigateway:<region>::/restapis/<api-id>`, which the grant `Resource: arn:aws:apigateway:*::/restapis/*` matches. So a handler holding that grant must receive HTTP 200 and a RestApi carrying `name` equal to the name the API was created with, and the handler's own 200 response body must carry it.

Emulator: the handler returns a payload with no `body` member — the harness reads the body as `{}` and the asserted `name` is absent — which means the in-function `apigateway.get_rest_api(restApiId=...)` call (or the `api["name"]` read of its response) raised an unhandled exception inside the invocation rather than producing the API's name. Nothing about the API's name is observable through the in-function SDK.

Hypothesis: the same root cause as the sibling divergence filed for apigateway__dynamodb__lambda__logs.lambda-update-apigateway-stage-variables-positive, where an in-function UpdateStage with an exactly-scoped `apigateway:PATCH` grant on `arn:aws:apigateway:<region>::/restapis/<id>/stages/prod` is refused `AccessDeniedException`: the identity-policy evaluation for API Gateway control-plane actions does not match API Gateway's account-less, path-style ARN, so boto3 raises inside the handler and the handler's return value never contains `body`. The alternative consistent reading is that GetRestApi's response omits the `name` member, making `api["name"]` a KeyError. Both land in the same two components named under Fix.

Why: a client that reads back an API it just created — the ordinary "create, then describe to confirm" step, and the way any in-function automation resolves an API's name — gets an invocation failure instead of the name, and gets it from a correctly, narrowly granted role. The failure is silent at the SDK layer of the caller: the invoke returns 200 with a FunctionError payload, so the caller sees an empty body rather than an API Gateway error class it can branch on.

Fix: GetRestApi's request path in the apigateway service — its authorization component (identity-policy resource matching for the account-less `arn:aws:apigateway:<region>::/restapis/*` ARN form and the HTTP-verb-to-`apigateway:GET` action mapping) and its response serializer, which must populate RestApi.name.

Traces: apigateway__dynamodb__lambda.lambda-describe-apigateway-positive (mismatch).

## Evidence
Replay output for apigateway__dynamodb__lambda.lambda-describe-apigateway-positive:

  File "check_apigateway__dynamodb__lambda_lambda_describe_apigateway_positive.py", line 127, in run
  assert body.get("name") == api_name_val, f"API name mismatch: {body}"
  AssertionError: API name mismatch: {}

The check first invokes a setup Lambda (broad apigateway GET/POST/PUT/DELETE/PATCH grant) that calls create_rest_api(name=<unique name>) and returns its id — that leg succeeds. It then creates a second Lambda whose role grants ONLY:

  {"Action": ["apigateway:GET"], "Resource": "arn:aws:apigateway:*::/restapis/*"}

whose handler is:

  api = apigw.get_rest_api(restApiId="<api-id>")
  return {"statusCode": 200, "body": json.dumps({"name": api["name"]})}

Expected (real AWS): the invocation returns a payload whose `body` parses to {"name": "<the created API's name>"}.
Actual (emulator): the payload carries no `body` member, so the parsed body is `{}` and `body.get("name")` is None — the handler raised before returning. Nothing in the modeled GetRestApi error set (BadRequestException 400 / NotFoundException 404 / TooManyRequestsException 429 / UnauthorizedException 401) is surfaced to the caller; only an empty body is.

## Steps to Reproduce
1. Create a Lambda with a broad apigateway grant whose handler calls create_rest_api(name=<unique>) and returns the new api id; invoke it and keep the id.
2. Create a second role granting ONLY Action ["apigateway:GET"] on Resource "arn:aws:apigateway:*::/restapis/*", and a python3.12 function on it whose handler does api = boto3.client("apigateway").get_rest_api(restApiId=<id>) and returns {"statusCode":200,"body":json.dumps({"name":api["name"]})}.
3. Invoke it with an empty payload. Expect a payload whose body parses to {"name": "<the name used in step 1>"}; observe a payload with no body member (parsed body {}).
4. For the direct comparison, call GetRestApi(restApiId=<id>) from outside the function and confirm whether the response carries `name`; and re-run the handler with the ClientError caught to read the error code the in-function call actually raises.
Or simply re-replay the behavioral check apigateway__dynamodb__lambda.lambda-describe-apigateway-positive.
