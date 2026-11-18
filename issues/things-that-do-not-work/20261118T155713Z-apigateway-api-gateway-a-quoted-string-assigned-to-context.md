Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway.apigw-response-override-status-requires-numeric-value
Issue-Shape-Id: com.amazonaws.apigateway#BackplaneControlService
Issue-Key: apigateway.apigw-response-override-status-requires-numeric-value|run#23|AssertionError|assert status2 == 200, (
Issue-Commit: dcd9af8ac8cd87336436cdc13addcf6e6d4020a5

# API Gateway: a quoted STRING assigned to $context.responseOverride.status overrides the served status code (must be ignored; only an integer overrides)

> _Note: this issue was identified against the code base at commit `dcd9af8ac8cd87336436cdc13addcf6e6d4020a5`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutIntegrationResponse

## Description
One net-new cause.

Cause 1 of 1: the data-plane evaluation of `$context.responseOverride.status` in an integration response template does not type-check the assigned value. A quoted string literal is coerced to a number and applied as the response status, where AWS applies the override only when the assigned value is an Integer.

AWS: API Gateway's mapping-template `$context` variables are typed; `$context.responseOverride.status` is documented as "The HTTP status code of the response" with type Integer. A `#set($context.responseOverride.status = 201)` in an integration response template therefore overrides the method response's declared `statusCode`, and the stage serves 201. A `#set($context.responseOverride.status = "201")` assigns a String to an Integer-typed override: the status assignment does not take effect and the stage serves the status code declared on the integration response (here the `statusCode` 200 that `PutIntegrationResponse` was keyed by), even though the string spells a valid status number. The override object is per-member, so a `#set($context.responseOverride.header.<Name> = "<plain string>")` on the same request still takes effect regardless of whether the status assignment did — a header override is String-typed. Nothing in the Smithy model constrains template contents: `PutIntegrationResponse.responseTemplates` is a plain `MapOfStringToString` and `statusCode` is a `StatusCode` with pattern `^[1-5]\d\d$` (model: services/service-com-amazonaws-apigateway-backplanecontrolservice), so this is documented/recorded behavior of the data plane, not a modeled constraint.

Emulator: with a MOCK integration whose integration response for statusCode 200 carries the template `#set($context.responseOverride.status = "201")` plus a header override, the deployed stage serves HTTP 201 with body `{"served":true}`. The numeric-literal half of the same check passes (201 is served for `= 201`) and the header override is applied in both halves, so the emulator reaches the override machinery but assigns the status from the string value instead of discarding a non-Integer assignment. Replay reports: `AssertionError: the quoted STRING literal "201" assigned to responseOverride.status must leave the declared statusCode 200 untouched, even though it spells a valid status number; got 201: '{"served":true}'`.

Why: a client whose template writes a quoted status (a common accident, e.g. `"$input.path('$.code')"` or any string-producing expression) gets a silently different HTTP status from the emulator than from AWS. On AWS the method keeps its declared status and the caller's status-based branching (and any integration/method response mapped to that status) behaves as declared; against the emulator the quoted value hijacks the status line, so tests that assert AWS's no-op semantics pass locally and the deployed behavior differs. It also makes the integer/string distinction — the only signal that the template is wrong — unobservable.

Fix: the API Gateway data-plane integration-response path, in the VTL/`$context.responseOverride` evaluation that maps a template's `#set` assignments onto the outgoing response. The override setter must reject (ignore) a non-Integer value for `responseOverride.status` — a quoted VTL string must not be parsed into an integer — while continuing to apply `responseOverride.header.*` (String) and `responseOverride.requestheader.*`/path overrides independently of the discarded status assignment. The status actually served must then fall back to the integration response's declared `statusCode`.

Traces: apigateway.apigw-response-override-status-requires-numeric-value (the first, numeric-literal half of this check passes; the failure is the quoted-string half).

## Evidence
Check: apigateway.apigw-response-override-status-requires-numeric-value (verdict: mismatch)

Setup (from the replayed check): REGIONAL REST API, resource /ov, GET with authorizationType NONE, MOCK integration with requestTemplates {"application/json": '{"statusCode": 200}'}, method responses declared for 200 and 201, and one integration response for statusCode 200 with selectionPattern "".

Phase 1 — numeric literal (passes, shown for contrast):
  responseTemplates[application/json] =
    #set($context.responseOverride.status = 201)
    #set($context.responseOverride.header.X-Overridden = "numeric-literal")
    {"served":true}
  Expected and observed: HTTP 201, body {"served":true}, header x-overridden: numeric-literal.

Phase 2 — quoted string literal (the divergence), after re-putting the integration response and re-deploying the same stage:
  responseTemplates[application/json] =
    #set($context.responseOverride.status = "201")
    #set($context.responseOverride.header.X-Overridden = "string-literal")
    {"served":true}
  Expected (AWS): HTTP 200 (the declared statusCode of the integration response is untouched because "201" is a String, not an Integer), body {"served":true}, header x-overridden: string-literal.
  Actual (emulator): HTTP 201, body {"served":true} (the header override was applied, so the stage did serve the re-deployed template).

Assertion text from the replay:
  the quoted STRING literal "201" assigned to responseOverride.status must leave the declared statusCode 200 untouched, even though it spells a valid status number; got 201: '{"served":true}'

Fixed text vs. variable parts: the requirement is the pair of status lines, not the sample values — the served status must equal the integration response's declared statusCode (200 here, whatever the request declared in general) when the assigned value is not an Integer, and must equal the assigned integer when it is. The api id, resource id, stage name, body and header name/value above are request-supplied values from this run, not part of the contract.

## Steps to Reproduce
1. CreateRestApi (endpointConfiguration types=[REGIONAL]); CreateResource pathPart="ov" under the root resource.
2. PutMethod restApiId=<api> resourceId=<ov> httpMethod=GET authorizationType=NONE.
3. PutIntegration ... type=MOCK requestTemplates={"application/json": "{\"statusCode\": 200}"}.
4. PutMethodResponse statusCode="200" responseParameters={"method.response.header.X-Overridden": false}; PutMethodResponse statusCode="201".
5. PutIntegrationResponse statusCode="200" selectionPattern="" responseTemplates={"application/json": "#set($context.responseOverride.status = \"201\")\n#set($context.responseOverride.header.X-Overridden = \"string-literal\")\n{\"served\":true}"}  (note the QUOTED 201).
6. CreateDeployment restApiId=<api> stageName=<stage>.
7. GET https://<api>.execute-api.<region>.<host>/<stage>/ov and poll until the response carries header x-overridden: string-literal (so the deployed snapshot is the one above).
8. Observe the status line: AWS serves 200; the emulator serves 201. Re-run with the UNQUOTED literal 201 in step 5 and both serve 201, which isolates the string/integer type check as the divergence.
