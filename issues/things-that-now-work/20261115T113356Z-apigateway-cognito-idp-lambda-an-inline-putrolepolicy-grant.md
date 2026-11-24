Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway__cognito-idp__lambda.lambda-apigateway-create-deployment-missing-permission-negative apigateway__cognito-idp__lambda.lambda-apigateway-get-rest-apis-missing-permission-negative
Issue-Key: apigateway__cognito-idp__lambda.lambda-apigateway-create-deployment-missing-permission-negative|_poll_until_ok#15|AssertionError|raise AssertionError(
Issue-Key: apigateway__cognito-idp__lambda.lambda-apigateway-get-rest-apis-missing-permission-negative|run#11|AssertionError|assert granted["ok"] is True, f"expected success after granting apigateway:GET, got: {granted}"
Issue-Commit: 38e24911c56fad04803701729ab45a93a1394fb2

# apigateway__cognito-idp__lambda: an inline PutRolePolicy grant of apigateway:GET/POST on a Lambda execution role is never honored — in-function API Gateway control-plane calls stay refused AccessDeniedException after the grant

> _Note: this issue was identified against the code base at commit `38e24911c56fad04803701729ab45a93a1394fb2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDeployment

## Description
Cause 1 of 1: the denied half of the least-privilege contract now works (an in-function API Gateway call under a role with no apigateway permissions is refused with error code AccessDeniedException), but the GRANT is never honored: after PutRolePolicy adds an inline policy on the SAME role allowing the exact action on the matching apigateway resource ARN, the identical in-function call keeps coming back AccessDeniedException indefinitely (polled for 45s). The authorization decision for in-function API Gateway callers is effectively stuck at deny.

AWS: recorded behavior only for the refusal shape — AccessDeniedException is not a modeled member of these operations in the apigateway Smithy model (service-com-amazonaws-apigateway-backplanecontrolservice models GetRestApis GET /restapis -> 200 RestApis{items,position} and CreateDeployment POST /restapis/{restApiId}/deployments -> 201 Deployment{id,...}); it is the IAM authorization refusal returned ahead of the operation, HTTP 403, error code `AccessDeniedException`, message template `User: <caller-arn> is not authorized to perform: <action> on resource: <resource-arn>` whose fixed text is `User: `, ` is not authorized to perform: `, ` on resource: ` and whose variable parts are the function's assumed-role session ARN, the action (`apigateway:GET`, `apigateway:POST`) and the resource ARN derived from the request. On AWS that refusal is CONDITIONAL on the role's policies: once an inline policy on the same role allows the action on a resource pattern that matches the request (`arn:aws:apigateway:<region>::/restapis*` for GetRestApis, `arn:aws:apigateway:<region>::/restapis/<api-id>/deployments*` for CreateDeployment), the identical call stops being refused and returns the modeled success — GetRestApis 200 with the created api id among `items`, CreateDeployment 201 with an `id` that GetDeployments then lists. Policy updates take effect within seconds (the checks poll up to 45s), so the grant must become visible to the next invocation.

Emulator: the before-grant invocation returns {'ok': False, 'error_code': 'AccessDeniedException'} (correct) and the after-grant invocation returns exactly the same thing for the full 45s polling window — the newly added inline role policy is never consulted, so no in-function API Gateway call can ever be authorized. Hypothesis: either the inline policy written by PutRolePolicy is not re-read when the authorization decision for an invocation is made (a cached/at-creation-time snapshot of the role's permissions), or the apigateway-flavored resource ARN in the grant (`arn:aws:apigateway:<region>::/restapis...`, which has an EMPTY account-id field and a slash-path resource, with a trailing `*`) is not matched against the request's resource, so the Allow never applies. Note the open issue 20261106T143312Z-apigateway-cognito-idp-lambda-an-api-gateway-sdk.md covers the earlier state of this surface (in-function calls raising a non-service exception with no Error.Code, both before and after the grant); it does NOT cover this divergence — the refusal is now a proper AccessDeniedException, and what remains is that the grant has no effect — and the replay did not mark these checks as owned by it.

Why: a client that does API Gateway control-plane work from a Lambda (CI/CD redeploy, stage-variable flips, resource discovery) and follows least privilege can never get its function working: it grants precisely the action AWS asks for and the call stays denied, so the permission model is unlearnable and unverifiable against the emulator. A negative/positive permission-boundary pair also loses all signal — denial is indistinguishable from a permanent outage — and automation that waits for a grant to propagate hangs or fails closed.

Fix: the IAM authorization evaluation applied to API Gateway control-plane requests originating from a Lambda execution role — specifically the component that resolves the caller role's CURRENT inline policies at decision time and the resource-ARN matching for apigateway-style ARNs (empty account field, `/restapis...` path, trailing wildcard). The apigateway handlers themselves answer these operations correctly when the suite calls them directly, and the 403/AccessDeniedException refusal path must keep working for the ungranted role.

Traces: apigateway__cognito-idp__lambda.lambda-apigateway-create-deployment-missing-permission-negative, apigateway__cognito-idp__lambda.lambda-apigateway-get-rest-apis-missing-permission-negative (both failing checks for this one cause).

## Evidence
Both failing checks create a role trusting lambda.amazonaws.com with NO apigateway permissions, create a python function whose handler builds boto3 client('apigateway') and returns {'ok': True, ...} or {'ok': False, 'error_code': exc.response['Error']['Code']}, invoke it (correctly denied), then PutRolePolicy the exact Allow on the SAME role and invoke again.

lambda-apigateway-create-deployment-missing-permission-negative (assertion at line 70 / _poll_until_ok line 214):
  grant: {"Effect":"Allow","Action":"apigateway:POST","Resource":"arn:aws:apigateway:<region>::/restapis/<api-id>/deployments*"}
  Expected after the grant: handler returns {'ok': True, 'deployment_id': <id>} (CreateDeployment 201) and GetDeployments lists that id.
  Actual: AssertionError: CreateDeployment did not succeed within 45s of granting apigateway:POST; last observation: {'ok': False, 'error_code': 'AccessDeniedException'}

lambda-apigateway-get-rest-apis-missing-permission-negative (assertion at line 63):
  grant: {"Effect":"Allow","Action":"apigateway:GET","Resource":"arn:aws:apigateway:<region>::/restapis*"}
  Expected after the grant: handler returns {'ok': True, 'ids': [... <api-id> ...]} (GetRestApis 200).
  Actual: AssertionError: expected success after granting apigateway:GET, got: {'ok': False, 'error_code': 'AccessDeniedException'}

In both cases the pre-grant assertions (ok is False, error_code == 'AccessDeniedException', and for CreateDeployment: GetDeployments still empty, i.e. no effect landed) PASSED — the divergence is confined to the post-grant phase, where the response is byte-identical to the denied one.

## Steps to Reproduce
1. CreateRestApi(name=<any>, endpointConfiguration={'types':['REGIONAL']}); for the CreateDeployment arm also read the root resource via GetResources and add a MOCK GET method on it.
2. CreateRole trusting lambda.amazonaws.com (sts:AssumeRole) with NO apigateway permissions.
3. CreateFunction (python runtime) whose handler is: `import boto3; c = boto3.client('apigateway')` then either `c.get_rest_apis(limit=500)` or `c.create_deployment(restApiId=<api-id>, stageName='prod')`, returning {'ok': True, ...} on success or {'ok': False, 'error_code': exc.response['Error']['Code']} on error.
4. Invoke the function. Observed (correct): {'ok': False, 'error_code': 'AccessDeniedException'}; GetDeployments(restApiId=<api-id>) still returns [].
5. PutRolePolicy on the SAME role granting exactly the action the call needs: apigateway:GET on arn:aws:apigateway:<region>::/restapis* , or apigateway:POST on arn:aws:apigateway:<region>::/restapis/<api-id>/deployments* .
6. Invoke the function again, re-polling every 3s for up to 45s. Observed: still {'ok': False, 'error_code': 'AccessDeniedException'} every time. Expected: {'ok': True, 'ids': [... <api-id> ...]} for GetRestApis, and {'ok': True, 'deployment_id': <id>} for CreateDeployment with that id visible in GetDeployments(restApiId=<api-id>).
7. For contrast, call GetRestApis / CreateDeployment directly against the emulator with the suite's own client: they answer normally, which isolates the defect to the authorization decision made for an in-function caller after a grant.
