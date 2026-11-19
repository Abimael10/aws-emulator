Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway.e2e-deploy-invoke-teardown-consequence apigateway.e2e-full-api-lifecycle-deploy-invoke-teardown
Issue-Shape-Id: com.amazonaws.apigateway#BackplaneControlService
Issue-Key: apigateway.e2e-deploy-invoke-teardown-consequence|run#23|AssertionError|assert mutate_err == not_found, (
Issue-Key: apigateway.e2e-full-api-lifecycle-deploy-invoke-teardown|run#28|AssertionError|assert mutate_message == gone_message, (
Issue-Commit: 0cb56282b0b5db0d156916db95a8cefdc5d77df2

# apigateway: CreateResource on a deleted/unknown RestApi returns NotFoundException "Invalid Resource identifier specified" instead of "Invalid API identifier specified <account>:<restApiId>"

> _Note: this issue was identified against the code base at commit `0cb56282b0b5db0d156916db95a8cefdc5d77df2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateResource

## Description
Cause 1 of 1: CreateResource does not report the missing REST API in its NotFoundException message. After DeleteRestApi, a mutation against the dead api id is refused, but with the resource-level not-found message instead of the api-level one; the read path (GetRestApi) on the same id returns the correct api-level message, so the two diverge within one service.

AWS: Smithy (apigateway, BackplaneControlService): CreateResource POST /restapis/{restApiId}/resources/{parentId}, required httpLabel members restApiId and parentId, modeled errors BadRequestException(400), ConflictException(409), LimitExceededException(429), NotFoundException(404), TooManyRequestsException(429), UnauthorizedException(401). The model does not state message text, so the message requirement is recorded behavior only: when restApiId names no (or a just-deleted) REST API, AWS refuses with HTTP 404 / NotFoundException and the message `Invalid API identifier specified <accountId>:<restApiId>` — fixed text `Invalid API identifier specified ` followed by the caller's account id, a colon, and the restApiId taken from the request path. AWS resolves the restApiId precondition BEFORE parentId, so the api-level message is what a caller sees even when parentId is also stale. Only when the REST API exists and parentId names no resource in it does AWS answer NotFoundException with the resource-level message `Invalid Resource identifier specified`.

Emulator: for a restApiId that was deleted, CreateResource answers NotFoundException with the message `Invalid Resource identifier specified` — the resource-level text, with no account id and no restApiId in it. The code (NotFoundException) and the refusal itself are right; the message is wrong. GetRestApi on the same id in the same check returns `Invalid API identifier specified 000000000000:<id>`, so the api-existence check exists in the service but the CreateResource path does not run it (or runs the parentId lookup first and reports its failure).

Why: a client cannot tell which identifier was rejected. Teardown/idempotency logic and CDK/CloudFormation-style reconcilers branch on the not-found message to distinguish "the API is gone" (stop, the whole tree is gone) from "this parent resource is gone" (re-resolve the root resource and retry); seeing the resource message for a deleted API drives them into a retry loop against an API that will never come back, and log/alert matching on `Invalid API identifier specified <account>:<id>` never fires.

Fix: the CreateResource handler's precondition validation in the apigateway service — look up restApiId first and, when it is absent, raise NotFoundException with the api-level message templated from the request's restApiId and the caller's account id; keep `Invalid Resource identifier specified` only for the case where the API exists and parentId does not resolve.

Traces: apigateway.e2e-deploy-invoke-teardown-consequence (asserts the mutation after DeleteRestApi fails with ('NotFoundException', 'Invalid API identifier specified <account>:<apiId>'), got ('NotFoundException', 'Invalid Resource identifier specified')); apigateway.e2e-full-api-lifecycle-deploy-invoke-teardown (same assertion, reports "unexpected NotFoundException message 'Invalid Resource identifier specified'"). The same defect is reached from the apigateway__lambda suite and is filed separately under that service heading because an issue carries one service token.

## Evidence
Check apigateway.e2e-deploy-invoke-teardown-consequence:
  AssertionError: expected a mutation on the deleted api to fail with ('NotFoundException', 'Invalid API identifier specified 000000000000:c1f8jayut9'), got ('NotFoundException', 'Invalid Resource identifier specified')

Check apigateway.e2e-full-api-lifecycle-deploy-invoke-teardown:
  AssertionError: unexpected NotFoundException message 'Invalid Resource identifier specified'
  (expected gone_message = f"Invalid API identifier specified {ctx.account}:{api_id}")

Expected: HTTP 404, Error.Code NotFoundException, Error.Message "Invalid API identifier specified <accountId>:<restApiId>" — fixed text "Invalid API identifier specified " plus the account id and the restApiId from the request.
Actual:   HTTP 404, Error.Code NotFoundException, Error.Message "Invalid Resource identifier specified" (resource-level text; identifiers absent).
In the same checks GetRestApi on the deleted id DOES return the expected api-level message, so only the mutation path diverges.

## Steps to Reproduce
1. apigateway CreateRestApi -> api_id; GetResources -> root resource id.
2. CreateResource(restApiId=api_id, parentId=root_id, pathPart="hello"); PutMethod/PutMethodResponse/PutIntegration(MOCK)/PutIntegrationResponse; CreateDeployment; CreateStage.
3. DeleteRestApi(restApiId=api_id).
4. GetRestApi(restApiId=api_id) -> NotFoundException, message "Invalid API identifier specified <account>:<api_id>" (correct today).
5. CreateResource(restApiId=api_id, parentId=root_id, pathPart="post-delete") -> NotFoundException, but message is "Invalid Resource identifier specified" instead of "Invalid API identifier specified <account>:<api_id>".
