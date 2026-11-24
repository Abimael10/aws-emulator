Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway__cloudformation.cfn-apigateway-stage-in-place-variables-retained-api-move-then-not-found
Issue-Key: apigateway__cloudformation.cfn-apigateway-stage-in-place-variables-retained-api-move-then-not-found|_expect#36|AssertionError|raise AssertionError(
Issue-Commit: 7feef7c3a108bb63e3851ac69c48ec064beaa56b

# apigateway__cloudformation: DeleteStack ends DELETE_FAILED because the AWS::ApiGateway::Deployment delete propagates API Gateway's "Active stages pointing to this deployment" refusal, so neither REST API is removed

> _Note: this issue was identified against the code base at commit `7feef7c3a108bb63e3851ac69c48ec064beaa56b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteStack

## Description
Cause 1 of 1: deleting a stack that still holds a retained AWS::ApiGateway::Stage inside one of its REST APIs is refused on the API's AWS::ApiGateway::Deployment, so the stack ends DELETE_FAILED and every API Gateway resource of the stack survives the delete. Failing check: apigateway__cloudformation.cfn-apigateway-stage-in-place-variables-retained-api-move-then-not-found.

AWS:      recorded behavior only for the orchestration (CloudFormation's delete ordering is not modelled). The recording's flow is: a stack declares two REST APIs (ApiA, ApiB), a MOCK method and a Deployment for each, plus one AWS::ApiGateway::Stage carrying UpdateReplacePolicy: Retain; an update moves the Stage from ApiA to ApiB, which CloudFormation performs as a replacement and reports as UPDATE_IN_PROGRESS with reason "Requested update requires the creation of a new physical resource; hence creating one.", UPDATE_COMPLETE on physical id "live", then DELETE_SKIPPED with an empty PhysicalResourceId for the retained old stage — so ApiA keeps a live stage that still points at ApiADeployment. A later update removes the Stage from the template (DELETE_IN_PROGRESS/DELETE_COMPLETE for ApiB's stage) and the final DeleteStack reaches DELETE_COMPLETE, after which apigateway GetRestApi for both api ids answers NotFoundException with message "Invalid API identifier specified <account>:<api-id>" (fixed text "Invalid API identifier specified " plus the account id and the api id from the request/state). Smithy (apigateway): DeleteDeployment is DELETE /restapis/{restApiId}/deployments/{deploymentId} with success code 202 and declares BadRequestException (400) — the active-stage refusal is a modelled 400 of a direct DeleteDeployment call, not of this stack delete. Hypothesis: real CloudFormation does not let a deployment whose only active stage belongs to a RestApi the same delete removes fail the stack — the API and its stages go away with the RestApi.
Emulator: DeleteStack settles on DELETE_FAILED. The stack's StackStatusReason is "The following resource(s) failed to delete: [ApiADeployment]. " and the resource event is ApiADeployment DELETE_FAILED with ResourceStatusReason 'Resource handler returned message: "Active stages pointing to this deployment must be moved or deleted (Service: ApiGateway, Status Code: 400, Request ID: <id>)" (RequestToken: <token>, HandlerErrorCode: InvalidRequest)' — i.e. the CloudFormation handler calls DeleteDeployment, gets API Gateway's 400 BadRequestException for the retained stage, and fails the resource instead of completing the stack delete. The APIs, deployments and the retained stage all remain.
Why:      a client that deletes a stack after a retaining stage move never gets its stack back to DELETE_COMPLETE: the stack is wedged in DELETE_FAILED, every subsequent delete retries the same refusal, and the REST APIs, deployments and stages are leaked (GetRestApi keeps answering 200 instead of the NotFoundException AWS gives). Any teardown or CI job keyed on DELETE_COMPLETE hangs or fails.
Fix:      the CloudFormation AWS::ApiGateway::Deployment delete handler (its delete path and the stack delete's ordering/tolerance for the RestApi's dependent stages): a deployment whose active stages belong to a RestApi being deleted in the same operation must not fail the resource. Related open issues name neighbouring API Gateway/CloudFormation causes but carry neither this check nor this replay key: 20261115T073242Z-apigateway-cloudformation-3-net-new-causes-in-place.md (in-place updates of Deployment/Method/Stage refused NotImplemented) and 20261117T184012Z-apigateway-cloudformation-4-net-new-causes-in-cloudformation-s-api.md (a Stage/Deployment stage collision cannot roll back).
Traces:   apigateway__cloudformation.cfn-apigateway-stage-in-place-variables-retained-api-move-then-not-found (AssertionError in _delete_stack/_expect: stack ended 'DELETE_FAILED', expected 'DELETE_COMPLETE').

## Evidence
Check apigateway__cloudformation.cfn-apigateway-stage-in-place-variables-retained-api-move-then-not-found [mismatch], AssertionError raised by the check's _expect() after _delete_stack():
  AssertionError: stack ended 'DELETE_FAILED' ('The following resource(s) failed to delete: [ApiADeployment]. '), expected 'DELETE_COMPLETE'; failed events: [('ApiADeployment', 'DELETE_FAILED', 'Resource handler returned message: "Active stages pointing to this deployment must be moved or deleted (Service: ApiGateway, Status Code: 400, Request ID: <id>)" (RequestToken: <token>, HandlerErrorCode: InvalidRequest)'), ('<stack-name>', 'DELETE_FAILED', 'The following resource(s) failed to delete: [ApiADeployment]. ')]
Expected (AWS): DescribeStacks reports DELETE_COMPLETE for the stack id, and apigateway GetRestApi for both api ids then answers NotFoundException / "Invalid API identifier specified <account>:<api-id>".
The request id, request token, stack name and api ids above are per-run values; the requirement is the terminal status (DELETE_COMPLETE) and that no resource is left behind.

## Steps to Reproduce
1. CreateStack a template with two AWS::ApiGateway::RestApi resources (ApiA with a GET MOCK method, ApiB with a POST MOCK method), one AWS::ApiGateway::Deployment per API, and a single AWS::ApiGateway::Stage named "live" on ApiA's deployment carrying UpdateReplacePolicy: Retain; wait for CREATE_COMPLETE.
2. UpdateStack changing only the stage's Description and Variables (in-place update of the stage).
3. UpdateStack moving the Stage to ApiB's deployment: the stage is replaced, and the retained old stage stays live in ApiA pointing at ApiADeployment (DELETE_SKIPPED with an empty physical id).
4. UpdateStack removing the Stage from the template (ApiB's stage is deleted).
5. DeleteStack by StackId and poll DescribeStacks: the stack settles DELETE_FAILED with ApiADeployment DELETE_FAILED / "Active stages pointing to this deployment must be moved or deleted", and GetRestApi on both api ids still answers 200.
