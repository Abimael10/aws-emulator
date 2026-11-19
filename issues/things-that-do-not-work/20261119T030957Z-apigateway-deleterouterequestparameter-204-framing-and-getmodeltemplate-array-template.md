Issue-Source: trace-sweep-divergence
Issue-Trace: apigatewayv2-http-api-route-config-cleanup
Issue-Step: task#6
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: apigatewayv2-http-api-route-config-cleanup apigatewayv2-route-request-parameter-lifecycle apigatewayv2-model-template-by-schema-type
Issue-Shape-Id: com.amazonaws.apigatewayv2#ApiGatewayV2
Issue-Key: apigateway|DeleteRouteRequestParameter|2xx->2xx|$headers.content-length
Issue-Key: apigateway|GetModelTemplate|2xx->2xx|$.value
Issue-Commit: 0cb56282b0b5db0d156916db95a8cefdc5d77df2

# apigateway DeleteRouteRequestParameter 204 framing and GetModelTemplate array template diverge

> _Note: this issue was identified against the code base at commit `0cb56282b0b5db0d156916db95a8cefdc5d77df2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteRouteRequestParameter

## Description
Two ApiGatewayV2 (service token `apigateway`) operations serve responses that diverge from the recorded AWS responses. This issue bundles 2 causes; BOTH must be closed before submitting.

Cause 1 of 2: DeleteRouteRequestParameter omits the Content-Length header on its 204.
AWS: recorded behavior only - Smithy: DeleteRouteRequestParameter @http {method: DELETE, uri: /v2/apis/{ApiId}/routes/{RouteId}/requestparameters/{RequestParameterKey}, code: 204}, output Unit (no modeled headers), errors NotFoundException (404) / TooManyRequestsException (429). The model says nothing about response headers; the recording shows the successful delete answering 204 with an explicit `content-length` response header (value `0`, the only value an empty body permits).
Emulator: answers 204 with no `content-length` header at all - replay detail `$headers.content-length: missing (expected '<shaped-not-asserted>', got None)`, expected_status == actual_status == 204.
Why: an HTTP client that closes out the message on an explicit zero content length, rather than on 204's implicit no-body rule, can block waiting for a body or treat the response as malformed; the delete itself succeeded, so the failure surfaces as a transport-level hang or parse error instead of an API error.
Fix: the REST-JSON response serializer path for this operation's empty (Unit) output - it must set `Content-Length: 0` when answering 204. Two open issues report the SAME serializer fault for OTHER operations and neither covers this one: `20261029T220031Z-apigateway-204-responses-omit-the-content-length-0.md` (DeleteApiMapping, DeleteAuthorizer, DeleteCorsConfiguration, DeleteDeployment, DeleteAccessLogSettings, DeleteIntegration, DeletePortalProductSharingPolicy, DeleteProductRestEndpointPage, DeleteRoute, DeleteStage) and `20261031T154120Z-apigateway-content-length-0-missing-on-204-deletes-for.md`. DeleteRouteRequestParameter appears in neither, so it is filed here; a fix to the shared serializer may resolve all of them, but this cluster must be re-replayed on its own.
Traces: apigatewayv2-http-api-route-config-cleanup#task#6, apigatewayv2-route-request-parameter-lifecycle#task#5.

Cause 2 of 2: GetModelTemplate returns a collapsed array template instead of AWS's VTL foreach skeleton.
AWS: recorded behavior only - Smithy: GetModelTemplate @http {method: GET, uri: /v2/apis/{ApiId}/models/{ModelId}/template, code: 200}, output GetModelTemplateResponse.Value (target __string, jsonName `value`, no constraints); the model does not describe the template text. The recording shows that for a model whose JSON Schema type is an array, AWS returns a multi-line VTL mapping-template SKELETON in `value`: the `#set($inputRoot = $input.path('$'))` preamble, then `[`, a `##TODO:` comment line instructing the caller to point the loop at the input array, a `#foreach($elem in $inputRoot.TODO)` loop wrapping the per-element expression, a `#if($foreach.hasNext),#end` comma separator, `#end`, and the closing `]`. Fixed text: the `#set` preamble, the brackets, the `##TODO:` comment, the `#foreach`/`#end` and `#if($foreach.hasNext),#end` scaffolding. Variable part: the per-element expression, which is derived from the referenced model's item schema (in the recording it renders as `"foo"`).
Emulator: returns the preamble and a single-line, already-collapsed array - `#set($inputRoot = $input.path('$'))\n[ "foo" ]` - with no `##TODO:` comment, no `#foreach` loop over the input array and no `hasNext` separator, where AWS returns the loop skeleton. Replay verdict mismatch, status 200 both sides, detail `$.value: value_mismatch`.
Why: a client calls GetModelTemplate to seed an integration request/response mapping template for a model. The emulator's template emits one hard-coded element and never iterates the incoming payload, so an integration built from it silently produces a wrong body (a single literal element) for every multi-element input, and the `##TODO:` marker a caller greps for to find the spot to edit is absent.
Fix: GetModelTemplate's handler - the model-template generator that walks the referenced model's JSON Schema; its array branch must emit the foreach skeleton rather than inlining one rendered element. No open issue reports this operation.
Traces: apigatewayv2-model-template-by-schema-type#task#4, apigatewayv2-model-template-by-schema-type#task#6.

## Evidence
Cause 1 - cluster `service=apigateway|DeleteRouteRequestParameter|status=204->204|keys=$headers.content-length`, verdict mismatch. Expected: 204 carrying a `content-length` response header. Actual: 204 with the header absent (`$headers.content-length: missing (expected '<shaped-not-asserted>', got None)`). 2 recorded steps affected: apigatewayv2-http-api-route-config-cleanup#task#6, apigatewayv2-route-request-parameter-lifecycle#task#5.

Cause 2 - cluster `service=apigateway|GetModelTemplate|status=200->200|keys=$.value`, verdict mismatch. Expected `$.value`:
  #set($inputRoot = $input.path('$'))
  [
  ##TODO: Update this foreach loop to reference array from input json
  #foreach($elem in $inputRoot.TODO)
   "foo" 
  #if($foreach.hasNext),#end
  #end
  ]
Actual `$.value`:
  #set($inputRoot = $input.path('$'))
  [ "foo" ]
Status 200 on both sides, so the divergence is purely the template body. 2 recorded steps affected: apigatewayv2-model-template-by-schema-type#task#4 and #task#6.

## Steps to Reproduce
Replay the wire corpus (trace set simparity-wire-aws-v1) against the emulator and inspect the two fixture steps.

Cause 1 - apigatewayv2-http-api-route-config-cleanup#task#6: create an HTTP API, create a route, add a route request parameter, then send DELETE /v2/apis/{ApiId}/routes/{RouteId}/requestparameters/{RequestParameterKey} and dump the raw response headers. The emulator answers 204 with no Content-Length header. Same via apigatewayv2-route-request-parameter-lifecycle#task#5.

Cause 2 - apigatewayv2-model-template-by-schema-type#task#4: create an API, create a model whose JSON Schema declares an array type, then GET /v2/apis/{ApiId}/models/{ModelId}/template and read `value`. The emulator returns a one-line `[ ... ]` body instead of the #foreach skeleton with the ##TODO comment and the #if($foreach.hasNext),#end separator. Step task#6 of the same fixture repeats it for the other recorded schema type.
