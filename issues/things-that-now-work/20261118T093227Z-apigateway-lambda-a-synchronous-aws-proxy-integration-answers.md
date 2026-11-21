Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway__lambda.apigw-integration-timeout-maps-to-504-and-does-not-cancel-the-lambda
Issue-Key: apigateway__lambda.apigw-integration-timeout-maps-to-504-and-does-not-cancel-the-lambda|run#40|AssertionError|assert fast_elapsed >= float(_SLEEP_S), (
Issue-Commit: 79dfdcc6eb9143691c8a5d9dab7f4566d735ec1d

# apigateway__lambda: a synchronous AWS_PROXY integration answers 200 before the Lambda handler has finished — invocation wall-clock is not real, so timeoutInMillis can never be exceeded

> _Note: this issue was identified against the code base at commit `79dfdcc6eb9143691c8a5d9dab7f4566d735ec1d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
Invoke

## Description
One net-new root cause.

Cause 1 of 1: a Lambda proxy (AWS_PROXY) integration on a deployed REST API stage returns its 200 in LESS wall-clock time than the handler's own execution takes, so no invocation can ever outlive its integration deadline. Failing check: apigateway__lambda.apigw-integration-timeout-maps-to-504-and-does-not-cancel-the-lambda.

AWS: for an AWS_PROXY integration API Gateway invokes the function synchronously (RequestResponse) and holds the client's request open until the function returns; the HTTP response body IS the function's own returned payload, so the client's observed latency is at least the function's own execution duration. Smithy: com.amazonaws.apigateway#Integration.timeoutInMillis — "Custom timeout between 50 and 29,000 milliseconds. The default value is 29,000 milliseconds or 29 seconds"; com.amazonaws.apigateway#Integration.type enum value AWS_PROXY; GatewayResponse responseType INTEGRATION_TIMEOUT. The model states the deadline but not the clock semantics, so for the clock itself: recorded behavior only — a handler whose body is `time.sleep(12)` followed by a `{"statusCode": 200, ...}` return answers 200 no sooner than ~12 s on a resource configured with timeoutInMillis=29000, and a twin resource configured with timeoutInMillis=3000 instead answers 504 after at least 3 s and before the backend's 12 s completion, serving the stage's customised INTEGRATION_TIMEOUT gateway response with $context.error.responseType substituting to the fixed text INTEGRATION_TIMEOUT (the request-dependent part of that body is only the template the caller supplied to PutGatewayResponse).

Emulator: the positive-control leg — GET on the resource whose timeoutInMillis is the documented maximum 29000 — returns 200 carrying the handler's OWN marker body after 7.02 s, i.e. strictly less than the 12 s the handler spends sleeping before it builds that body. The response is the handler's, so the handler ran, yet the invocation's elapsed time is shorter than the handler's own blocking sleep. Hypothesis: the emulator's Python execution environment does not advance real wall-clock time across a blocking sleep (or the integration returns a completed result before the handler's sleep has elapsed). Because of this the check never reaches its 3000 ms leg at all, and no invocation can ever cross any timeoutInMillis value.

Why: every behaviour a client gates on real elapsed time inside an invocation is unreachable. A Lambda-proxy integration can never exceed its timeoutInMillis, so the stage never returns 504 INTEGRATION_TIMEOUT, never serves the customised INTEGRATION_TIMEOUT gateway response template, and never substitutes $context.error.responseType=INTEGRATION_TIMEOUT; the default 29 s deadline is indistinguishable from an explicitly configured 3 s one; Lambda's own function Timeout is likewise unreachable; and AWS's contract that a timed-out request does NOT cancel the backend (the function still completes and still performs its side effects after the client already holds the 504) cannot be reproduced at all. Client code validated against the emulator will ship with untested timeout, retry and idempotency paths and with latency assumptions that real AWS violates.

Fix: belongs in two components. (a) the Lambda invoke data plane's execution environment — the runtime that runs the handler must advance real wall-clock time, honour blocking sleeps, and complete the synchronous invocation only when the handler returns. (b) the API Gateway data-plane integration handler for type=AWS_PROXY — it must hold the client request open for the full synchronous invocation, measure elapsed time against the integration's stored timeoutInMillis (default 29000), and on expiry answer 504 rendered from the stage's INTEGRATION_TIMEOUT GatewayResponse template with $context.error.responseType bound to INTEGRATION_TIMEOUT, while leaving the in-flight invocation running rather than cancelling it.

Related but not covering: open issue 20261107T091747Z-lambda-the-execution-environment-has-no-filesystem-or.md describes a crippled stdlib/process surface in the Lambda execution environment, and 20261114T213419Z-codebuild-build-clock-is-not-real... describes an unreal clock in CodeBuild; neither carries this check's replay key nor the API Gateway integration-deadline half of this cause.

Traces: apigateway__lambda.apigw-integration-timeout-maps-to-504-and-does-not-cancel-the-lambda (positive-control assertion at check_apigateway__lambda_apigw_integration_timeout_maps_to_504_and_does_not_cancel_the_lambda.py line 294).

## Evidence
Expected (real AWS): GET <stage-invoke-url>/fast?leg=fast on a resource with type=AWS_PROXY and timeoutInMillis=29000, backed by a handler that executes `time.sleep(12)` before returning {"statusCode":200,...}, answers HTTP 200 with the handler's own body after an elapsed time >= 12 s. The twin resource with timeoutInMillis=3000 then answers HTTP 504 after >= 3.0 s and < 12 s, with the body produced by the stage's customised INTEGRATION_TIMEOUT GatewayResponse template and $context.error.responseType == "INTEGRATION_TIMEOUT"; the function nevertheless completes afterwards and its side effect (an SQS message) is observed AFTER the client already holds the 504.

Actual (emulator): the 29000 ms control request returns HTTP 200 carrying the handler's own marker body after only 7.02 s — shorter than the handler's own 12 s sleep. The check aborts there with:

  AssertionError: positive control: the backend sleeps 12s, so the 200 must take at least that long — got 7.02s, which means the backend did not really run and the timeout comparison below would be meaningless

(7.02 s and 12 s are this run's configured/observed values filling the template; the requirement is the invariant elapsed >= the handler's own execution duration, not those numbers.) The 3000 ms leg, the 504, the INTEGRATION_TIMEOUT gateway response and the not-cancelled side-effect assertions are therefore never exercised.

## Steps to Reproduce
1. Create an SQS queue, an IAM role assumable by lambda.amazonaws.com with sqs:SendMessage on that queue plus AWSLambdaBasicExecutionRole.
2. CreateFunction (python3.12) whose handler does `time.sleep(12)`, then sends a message to the queue, then returns {"statusCode":200,"headers":{"Content-Type":"application/json"},"body":json.dumps({"marker":<marker>,"leg":<leg>})}. Set the function Timeout to 30 s.
3. CreateRestApi (REGIONAL); AddPermission for apigateway.amazonaws.com on arn:aws:execute-api:<region>:<account>:<api-id>/*/*/*.
4. CreateResource /fast and /slow; PutMethod GET authorizationType=NONE on each; PutIntegration type=AWS_PROXY integrationHttpMethod=POST uri=arn:aws:apigateway:<region>:lambda:path/2015-03-31/functions/<function-arn>/invocations with timeoutInMillis=29000 on /fast and timeoutInMillis=3000 on /slow.
5. PutGatewayResponse responseType=INTEGRATION_TIMEOUT with responseTemplates {"application/json": '{"gw":"timeout-<gw-marker>","type":"$context.error.responseType"}'}.
6. CreateDeployment + CreateStage, then GET <stage-url>/fast?leg=fast and time it. Observe 200 returned in ~7 s, i.e. less than the handler's own 12 s sleep, instead of >= 12 s.
7. GET <stage-url>/slow?leg=slow: AWS answers 504 after >= 3 s with the customised INTEGRATION_TIMEOUT body and then still delivers the handler's SQS message; the emulator cannot reach this state because no invocation ever outlives the 3000 ms deadline.
