Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway__iam__stepfunctions__sts.apigateway-invoke-surfaces-backend-status-as-apigateway-dot-code-error
Issue-Key: apigateway__iam__stepfunctions__sts.apigateway-invoke-surfaces-backend-status-as-apigateway-dot-code-error|run#19|AssertionError|assert isinstance(err500["Cause"], str) and err500["Cause"] == '{"marker":"err500"}', (
Issue-Commit: c0e060f2f067b39cd5ab9988281e3b80e6bce20a

# apigateway__iam__stepfunctions__sts: an apigateway:invoke task's ApiGateway.<status> failure carries the whole task-result object as its Cause instead of the backend response body

> _Note: this issue was identified against the code base at commit `c0e060f2f067b39cd5ab9988281e3b80e6bce20a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartExecution

## Description
Cause 1 of 1: for a Task state whose Resource is arn:aws:states:::apigateway:invoke, a non-2XX backend response now fails the task with the right error name (ApiGateway.500 was asserted and matched) but the failure's Cause is the serialized success-result object rather than the backend response body. Failing check: apigateway__iam__stepfunctions__sts.apigateway-invoke-surfaces-backend-status-as-apigateway-dot-code-error.

AWS: recorded behavior only — Step Functions' optimized API Gateway integration is not described by a Smithy shape (the Cause is free text produced by the integration, not a modeled member). The recording shows that when the invoked stage answers a non-2XX status, the task fails with Error 'ApiGateway.<HTTP status code>' (e.g. ApiGateway.500, ApiGateway.404) and a STRING Cause that is exactly the backend response body — for the recorded MOCK integration returning the body {"marker":"err500"}, the Cause is the text of that body and nothing else. The composite object with Headers / ResponseBody / StatusCode / StatusText is the task's OUTPUT on a 2XX answer (the same check asserts that shape for the 200 branch and it matched); it is not what AWS puts in Cause on failure. The body text itself is a value from the request's backend configuration, not part of any fixed wording.

Emulator: the 500 branch's Cause is '{"Headers":{"content-type":[...],"content-length":[...],"x-amzn-requestid":[...],"x-amz-apigw-id":[...]},"ResponseBody":{"marker":"err500"},"StatusCode":500,"StatusText":"Internal Server Error"}' — i.e. the whole task-result envelope, with the response body nested under ResponseBody, plus per-run header values. The expected Cause is just the response body text.

Why: a client that catches ApiGateway.<status> and parses $.Cause to read the backend's error payload (the documented way to surface an API's own error document into a Catch/Fail path) gets an envelope whose top-level keys are not the body's, so every JSON path into the error payload misses and the error document is effectively unreadable. The nested per-run headers and request ids also make the Cause non-deterministic where AWS's is the body alone.

Fix: the Step Functions Task-state apigateway:invoke integration's failure mapping — when the backend status is non-2XX, set Cause to the response body text alone (keeping Error = ApiGateway.<status code>), rather than reusing the success-result serializer.

Related (does not cover this check): open issue 20261109T152222Z-apigateway-iam-stepfunctions-sts-a-task-state-using.md records the earlier cause in the same check (the integration not being dispatched at all, failing States.Runtime) and is keyed to the execution-status assertion; the task now runs and reaches the ApiGateway.<status> assertions, so this Cause divergence is a distinct, uncovered cause.

Traces: apigateway__iam__stepfunctions__sts.apigateway-invoke-surfaces-backend-status-as-apigateway-dot-code-error.

## Evidence
Expected (recorded AWS): the 500 branch fails with Error 'ApiGateway.500' and Cause '{"marker":"err500"}' — the backend response body as a string; likewise the 404 branch with Error 'ApiGateway.404' and Cause '{"marker":"err404"}'. The marker bodies are the values configured on the MOCK integration by the request.
Actual (replay): Error 'ApiGateway.500' (correct), Cause '{"Headers":{"content-type":["application/json"],"content-length":["19"],"x-amzn-requestid":["<id>"],"x-amz-apigw-id":["<id>"]},"ResponseBody":{"marker":"err500"},"StatusCode":500,"StatusText":"Internal Server Error"}'. AssertionError: the 500 branch's cause was that envelope. The request ids inside it are per-run values; the divergence is the envelope shape, not those values.

## Steps to Reproduce
1. Create a REST API with MOCK GET methods that answer 200 {"marker":"ok"}, 500 {"marker":"err500"} on /err500 and 404 {"marker":"err404"} on /err404; attach the resource policy allowing the state machine's role before the first deployment, then deploy a stage.
2. CreateStateMachine with Task states whose Resource is arn:aws:states:::apigateway:invoke (Parameters ApiEndpoint, Method, Stage, Path, AuthType) for the 200, 500 and 404 paths, each with a Catch that records Error and Cause.
3. StartExecution and read the caught results. Expected: the 500 branch has Error 'ApiGateway.500' and a string Cause equal to the backend body '{"marker":"err500"}'. Actual: Cause is the full {Headers, ResponseBody, StatusCode, StatusText} task-result envelope.
