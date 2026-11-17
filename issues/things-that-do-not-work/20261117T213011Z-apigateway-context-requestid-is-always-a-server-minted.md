Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway.apigw-extended-request-id-is-minted-per-api-per-request-not-echoed
Issue-Shape-Id: com.amazonaws.apigateway#BackplaneControlService
Issue-Key: apigateway.apigw-extended-request-id-is-minted-per-api-per-request-not-echoed|run#25|AssertionError|assert req1 == client_token, (
Issue-Commit: a39251e5c9db86afe29d1902591682be414d06f8

# apigateway: $context.requestId is always a server-minted UUID — a client-supplied x-amzn-RequestId header is not honoured at a deployed stage

> _Note: this issue was identified against the code base at commit `a39251e5c9db86afe29d1902591682be414d06f8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
Invoke

## Description
One net-new root cause in the execute-api data plane's request-identifier context.

Cause 1 of 1: a REST stage ignores the client-supplied `x-amzn-RequestId` request header, so `$context.requestId` rendered by a mapping template is always an emulator-minted id.

AWS: `$context.requestId` is the API Gateway request id a stage surfaces both in the `x-amzn-RequestId` response header and to VTL mapping templates, and a client MAY supply it: when a request carries an `x-amzn-RequestId` header, API Gateway adopts that value as the request id for the invocation, so a template rendering `$context.requestId` renders exactly the value the client sent (and two identical requests carrying the same header render the same value deterministically); with no such header the service mints the id itself. `$context.extendedRequestId` is a separate, always-server-minted id that is echoed in `x-amz-apigw-id` and is minted fresh per API per request, so the two identifiers never collapse. Recorded behavior only — the execute-api data plane is not described by the apigateway control-plane Smithy model (services/service-com-amazonaws-apigateway-backplanecontrolservice/smithy-model); the model only covers the control-plane shapes used to build the API (`Integration$requestTemplates`, `IntegrationResponse$responseTemplates`). The requirement here is the RULE (client header wins for `$context.requestId`, server mints `$context.extendedRequestId`), not any one id value: ids differ on every run and are never part of the contract.

Emulator: with `x-amzn-RequestId: <client token>` on the request, a MOCK/HTTP-backed stage whose integration response template renders `{"frontRequestId":"$context.requestId",...}` renders a freshly generated UUID instead of the client's token (observed: a token of the client's shape was sent, the body came back with a server UUID in that member). The HTTP status is 200 and the template otherwise renders, so only the identifier binding diverges.

Why: a client that stamps its own correlation id on the request (the documented override) cannot correlate its call with the stage's access/execution logs or with the body the template rendered — the id it chose never appears anywhere in the response, and a request-id-driven idempotency or tracing layer silently loses the join. Downstream assertions on the pair ($context.requestId vs $context.extendedRequestId, and the x-amz-apigw-id header) cannot be reached at all while this binding is wrong.

Fix: the execute-api data-plane request-context builder for REST stages — when populating the invocation context, seed `requestId` from the incoming `x-amzn-RequestId` header when present (falling back to a freshly minted id when absent), and keep `extendedRequestId` independently server-minted per API per request so the two never collapse.

Traces: apigateway.apigw-extended-request-id-is-minted-per-api-per-request-not-echoed (the assertion on `$context.requestId` is the first one in the check; the extendedRequestId / x-amz-apigw-id / per-API-freshness assertions that follow it were never reached and are not claimed here).

## Evidence
Behavioral suite replay (simparity-behavioral-aws-v1), service heading `### apigateway`, check not owned by any open issue:

  [mismatch] apigw-extended-request-id-is-minted-per-api-per-request-not-echoed
  AssertionError: expected $context.requestId to be the client-supplied x-amzn-RequestId 'sim2real-kimw0te5z3rg18km', got '57f67bf9-fc76-472a-91b0-c3e143010bb8' — clients are documented as able to override this id
  (check line 197: `assert req1 == client_token`)

Setup the check performs: CreateRestApi(REGIONAL) x2 (a backend API with a MOCK integration whose integration-response template renders `{"backendExtended":"$context.extendedRequestId"}`, and a front API with a non-proxy HTTP integration to the backend whose integration-response template renders `{"frontRequestId":"$context.requestId","frontExtended":"$context.extendedRequestId","backendExtended":"$input.path('$.backendExtended')"}`), CreateDeployment + CreateStage on both, then `GET <front stage>/trace` with the header `x-amzn-RequestId: <client token>`.

Expected: 200 and `frontRequestId` == the client-supplied token (the same value again on an identical second request); with no `x-amzn-RequestId` header, a server-minted value distinct from both the earlier token and `$context.extendedRequestId`.
Actual: 200, but `frontRequestId` is a freshly generated server UUID — the client's token is discarded. (The literal token and UUID above are one run's values; only the binding rule is the requirement.)

## Steps to Reproduce
1. git_clone(), then run_behavioral_suite().
2. In the report find `### apigateway` and replay apigw-extended-request-id-is-minted-per-api-per-request-not-echoed.
3. By hand: CreateRestApi(endpointConfiguration={types:[REGIONAL]}); create a child resource; PutMethod(GET, authorizationType=NONE); PutIntegration(type=MOCK, requestTemplates={'application/json': '{"statusCode": 200}'}); PutMethodResponse(200); PutIntegrationResponse(200, selectionPattern='', responseTemplates={'application/json': '{"frontRequestId":"$context.requestId"}'}); CreateDeployment + CreateStage.
4. GET the stage's invoke URL for that resource twice with the header `x-amzn-RequestId: my-correlation-id-1`.
   AWS renders `{"frontRequestId":"my-correlation-id-1"}` both times; the emulator renders a different server-minted UUID each time.
5. Repeat the GET with no `x-amzn-RequestId` header: both AWS and the emulator mint a value, which is the only case the emulator currently handles.
