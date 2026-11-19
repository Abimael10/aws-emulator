Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway__dynamodb__lambda.apigw-lambda-event-invocation-returns-200-empty-and-runs-async
Issue-Key: apigateway__dynamodb__lambda.apigw-lambda-event-invocation-returns-200-empty-and-runs-async|run#22|AssertionError|assert text == "", f"an Event-invocation integration's method response must be empty, got {text[:300]!r}"
Issue-Commit: 15eaad9fa74cff8f1dc14d7f5abd0b99014b7462

# apigateway__dynamodb__lambda: an Event-invocation-type integration now answers 200 but serves the handler's return value as the body instead of an empty body

> _Note: this issue was identified against the code base at commit `15eaad9fa74cff8f1dc14d7f5abd0b99014b7462`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
Invoke

## Description
One net-new root cause in the execute-api integration path. Cause 1 of 1: an AWS (non-proxy) Lambda integration that sets the Event invocation type serves a non-empty method response body.

AWS: an API Gateway REST API integration whose requestParameters set 'integration.request.header.X-Amz-Invocation-Type' to "'Event'" invokes the function ASYNCHRONOUSLY (Lambda Invoke with InvocationType=Event, Smithy: Invoke, member InvocationType bound to header X-Amz-Invocation-Type, enum value 'Event'; model services/service-com-amazonaws-lambda-awsgirapiservice/smithy-model/com-amazonaws-lambda-awsgirapiservice.json). Because the invocation is asynchronous, Lambda answers 202 with an EMPTY payload and there is no function result for the gateway to map, so the stage answers the caller HTTP 200 with a zero-length body while the function runs in the background. Recorded behavior only for the gateway half: the status (200) and the empty body are what the recording shows; the function's own return value is never observable to the caller on this path.

Emulator: the stage answers HTTP 200 (the status is now correct) but the body is the function's serialized return value — '{"stored": true}' for a handler that returns {"stored": True}. The fixed requirement is a zero-length body; the quoted JSON is this run's handler payload, not part of the contract.

Why: the fire-and-forget contract is broken in the one way a client can observe. A caller that treats a non-empty body as the synchronous result (or that asserts an empty 200 to confirm the call was accepted, not completed) reads the handler's private return value, which on AWS is discarded. It also leaks whatever the background handler returns — on AWS unreachable to the HTTP caller — through the API.

Fix: the execute-api integration path's response mapping for the Event invocation type — when the integration request carries X-Amz-Invocation-Type: Event, dispatch the invoke asynchronously and build the method response from the accepted (202/empty) invoke, discarding the function result, so the stage returns 200 with an empty body. The async dispatch itself and the DynamoDB side effect are already correct (the check's proof that the function ran passes).

Traces: apigateway__dynamodb__lambda.apigw-lambda-event-invocation-returns-200-empty-and-runs-async. NOTE: open issue 20261109T152222Z-apigateway-dynamodb-lambda-an-event-invocation-type-integration.md describes an EARLIER form of this path (it answered 500 for the whole window and the check failed in _poll with a TimeoutError); the 500 is gone, the check now reaches the body assertion, and this remaining divergence is not covered by that issue's replay key.

## Evidence
Behavioral suite replay against the emulator built from this checkout.

[mismatch] apigateway__dynamodb__lambda.apigw-lambda-event-invocation-returns-200-empty-and-runs-async
  File "check_apigateway__dynamodb__lambda_apigw_lambda_event_invocation_returns_200_empty_and_runs_async.py", line 110, in run
    assert text == "", f"an Event-invocation integration's method response must be empty, got {text[:300]!r}"
  AssertionError: an Event-invocation integration's method response must be empty, got '{"stored": true}'

Expected: HTTP 200 with a zero-length body. Actual: HTTP 200 whose body is the handler's return value. The earlier assertions of the same check (the stage answering 200, and the DynamoDB item written by the background invocation proving the function ran) are satisfied, so only the response body diverges.

## Steps to Reproduce
1. Clone the repo, build and boot the emulator from this branch, and replay the simparity-behavioral-aws-v1 suite filtered to apigateway__dynamodb__lambda.
2. Create a REST API with a POST method backed by an AWS (non-proxy) integration to a Lambda function, with requestParameters {'integration.request.header.X-Amz-Invocation-Type': "'Event'"}, and deploy it to a stage.
3. POST a body to the stage path.
4. Observe the response: status 200 (correct) but the body is the handler's return value. AWS returns 200 with an empty body while the handler runs in the background.
5. Replay apigateway__dynamodb__lambda.apigw-lambda-event-invocation-returns-200-empty-and-runs-async to confirm.
