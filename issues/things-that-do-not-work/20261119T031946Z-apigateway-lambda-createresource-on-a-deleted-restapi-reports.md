Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway__lambda.deep-apigw-integration-rest-aws-proxy-event-carries-the-v1-structure
Issue-Key: apigateway__lambda|CreateResource|NotFoundException
Issue-Commit: 0cb56282b0b5db0d156916db95a8cefdc5d77df2

# apigateway__lambda: CreateResource on a deleted RestApi reports the resource-level NotFoundException message, not "Invalid API identifier specified <account>:<restApiId>"

> _Note: this issue was identified against the code base at commit `0cb56282b0b5db0d156916db95a8cefdc5d77df2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateResource

## Description
Cause 1 of 1: CreateResource's NotFoundException message does not name the missing REST API. Reached here at the end of the REST AWS_PROXY v1-event check, which tears the API down and then asserts that a mutation against the dead id is refused with the api-level not-found message.

AWS: Smithy (apigateway, BackplaneControlService): CreateResource POST /restapis/{restApiId}/resources/{parentId}, required httpLabel members restApiId and parentId, modeled errors include NotFoundException(404). The model does not state message text, so the message requirement is recorded behavior only: when restApiId names no (or a deleted) REST API, AWS answers HTTP 404 / NotFoundException with `Invalid API identifier specified <accountId>:<restApiId>` — fixed text `Invalid API identifier specified `, then the caller's account id, a colon, and the restApiId from the request path. The restApiId precondition is resolved before parentId; `Invalid Resource identifier specified` is AWS's message only when the API exists and parentId does not resolve inside it.

Emulator: CreateResource on the deleted api id answers NotFoundException with `Invalid Resource identifier specified` — no account id, no restApiId. The status and error code are right; the message is the wrong class of message.

Why: a client that distinguishes "the whole API is gone" from "that parent resource is gone" by the not-found message mis-classifies the refusal, and retries re-resolving a root resource under an API that no longer exists; matching on the documented api-level text never fires.

Fix: the CreateResource handler's precondition validation in the apigateway service — resolve restApiId first and emit the api-level NotFoundException message templated from the request's restApiId and the caller's account id.

Traces: apigateway__lambda.deep-apigw-integration-rest-aws-proxy-event-carries-the-v1-structure (its _gone_rest_api step). The same cause also fails two checks under the `apigateway` heading and is filed there as its own issue, because one issue carries one service token.

## Evidence
Check apigateway__lambda.deep-apigw-integration-rest-aws-proxy-event-carries-the-v1-structure, in _gone_rest_api:
  AssertionError: CreateResource tyn7spzw5h: NotFoundException message 'Invalid Resource identifier specified', expected 'Invalid API identifier specified 000000000000:tyn7spzw5h'

Expected: HTTP 404, Error.Code NotFoundException, Error.Message "Invalid API identifier specified <accountId>:<restApiId>" (fixed text plus account id and the restApiId from the request).
Actual:   HTTP 404, Error.Code NotFoundException, Error.Message "Invalid Resource identifier specified".

## Steps to Reproduce
1. Create a REST API with a Lambda AWS_PROXY integration and deploy a stage (as the check does).
2. DeleteRestApi(restApiId=api_id).
3. CreateResource(restApiId=api_id, parentId=root_id, pathPart="gone") -> NotFoundException with message "Invalid Resource identifier specified" instead of "Invalid API identifier specified <account>:<api_id>".
