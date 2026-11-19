Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway__dynamodb__lambda__logs.lambda-update-apigateway-stage-variables-positive
Issue-Key: apigateway__dynamodb__lambda__logs.lambda-update-apigateway-stage-variables-positive|run#21|AssertionError|assert result["ok"] is True, f"unexpected failure: {result}"
Issue-Commit: f7526cbca79906f89a28c5d197946d676b42fe8d

# apigateway__dynamodb__lambda__logs: an in-function apigateway:UpdateStage is refused AccessDeniedException although the execution role grants apigateway:PATCH on exactly that stage ARN

> _Note: this issue was identified against the code base at commit `f7526cbca79906f89a28c5d197946d676b42fe8d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateStage

## Description
Cause 1 of 1: API Gateway control-plane authorization refuses a least-privilege grant written against API Gateway's own ARN form, so a Lambda handler cannot patch a stage variable it is explicitly allowed to patch.

AWS: Smithy (apigateway, BackplaneControlService): UpdateStage = PATCH /restapis/{restApiId}/stages/{stageName}, input {restApiId (required, httpLabel), stageName (required, httpLabel), patchOperations: ListOfPatchOperation}, output shape Stage (which carries the `variables` MapOfStringToString), HTTP success code 200. Its modeled errors are BadRequestException (400), ConflictException (409), LimitExceededException (429), NotFoundException (404), TooManyRequestsException (429) and UnauthorizedException (401) — AccessDeniedException is NOT among them. API Gateway's IAM action for this call is `apigateway:PATCH`, and an API Gateway resource ARN has the path-style, ACCOUNT-LESS form `arn:aws:apigateway:<region>::/restapis/<api-id>/stages/<stage-name>` (the account-id segment is empty). An execution-role identity policy with Effect Allow / Action apigateway:PATCH / Resource that exact stage ARN therefore authorizes the call: AWS answers HTTP 200 with the Stage representation whose `variables` map contains the replaced key, and an independent GetStage shows the same key.

Emulator: the in-function boto3 call raises a ClientError whose Error.Code is `AccessDeniedException`; the handler reports {'ok': False, 'error_code': 'AccessDeniedException'} and no stage variable is set. The emulator both refuses a call AWS authorizes and answers with an error code that is not in UpdateStage's modeled error set.

Why: any caller that scopes a role to exactly the API Gateway resource it touches — the normal least-privilege pattern, and what CDK's grant helpers emit — is denied. A provisioning/automation Lambda that sets stage variables, patches method settings or otherwise PATCHes a stage can never succeed on the emulator, and the refusal it gets back is an error code the SDK's UpdateStage error set does not contain, so modeled-error handling does not catch it.

Fix: the authorization component that evaluates identity policies for API Gateway control-plane actions, on UpdateStage's request path — specifically the resource matching for API Gateway's account-less, path-style ARN (`arn:aws:apigateway:<region>::/restapis/...`) and the mapping of the HTTP verb to the `apigateway:PATCH` action. The refusal must not be emitted at all for a matching Allow; and where an API Gateway control-plane call genuinely is unauthorized it is UnauthorizedException (401), not AccessDeniedException.

Traces: apigateway__dynamodb__lambda__logs.lambda-update-apigateway-stage-variables-positive (mismatch).

## Evidence
Replay output for apigateway__dynamodb__lambda__logs.lambda-update-apigateway-stage-variables-positive:

  AssertionError: unexpected failure: {'ok': False, 'error_code': 'AccessDeniedException'}

The check creates a REST API, a MOCK GET method on the root resource, a deployment on stage `prod`, and asserts the stage starts with no variables. It then creates a Lambda whose inline role policy is exactly:

  {"Effect": "Allow", "Action": "apigateway:PATCH",
   "Resource": "arn:aws:apigateway:<region>::/restapis/<api-id>/stages/prod"}

and the handler calls apigateway.update_stage(restApiId=..., stageName='prod', patchOperations=[{'op':'replace','path':'/variables/envName','value':<value>}]).

Expected (real AWS): the call is authorized — HTTP 200, the returned Stage's `variables` contains envName=<value>, and an independent GetStage reads the same value back.
Actual (emulator): ClientError with Error.Code = 'AccessDeniedException'; the handler's reported `variables` is never produced and the stage keeps no variables. 'AccessDeniedException' is not one of UpdateStage's modeled errors (BadRequestException/ConflictException/LimitExceededException/NotFoundException/TooManyRequestsException/UnauthorizedException).

## Steps to Reproduce
1. CreateRestApi; GetResources to take the root resource id; PutMethod GET authorizationType=NONE; PutIntegration type=MOCK; PutMethodResponse/PutIntegrationResponse 200; CreateDeployment stageName=prod.
2. GetStage(restApiId, 'prod') and confirm `variables` is absent/empty.
3. CreateRole trusting lambda.amazonaws.com, attach AWSLambdaBasicExecutionRole, and put an inline policy granting ONLY Action "apigateway:PATCH" on Resource "arn:aws:apigateway:<region>::/restapis/<api-id>/stages/prod".
4. CreateFunction (python3.12) with a handler that calls boto3 apigateway.update_stage(restApiId=..., stageName='prod', patchOperations=[{'op':'replace','path':'/variables/envName','value':'v1'}]) and returns the error code on ClientError.
5. Invoke the function. Expect ok=True and variables['envName']=='v1'; observe error_code='AccessDeniedException'.
6. GetStage(restApiId,'prod') and confirm the variable is still unset.
Or simply re-replay the behavioral check apigateway__dynamodb__lambda__logs.lambda-update-apigateway-stage-variables-positive.
