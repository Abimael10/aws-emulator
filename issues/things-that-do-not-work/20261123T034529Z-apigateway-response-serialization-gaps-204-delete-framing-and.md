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
Issue-Commit: 616e510b6c9fc5a742ae081415d2f4cf49efcb62

# apigateway response serialization gaps: 204 delete framing and GetModelTemplate template

> _Note: this issue was identified against the code base at commit `616e510b6c9fc5a742ae081415d2f4cf49efcb62`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteRouteRequestParameter

## Description
Two net-new clusters in which the emulator's serialized response for an apigateway operation differs from AWS's while the status class is otherwise right. Both were reported with empty stub_path and empty owned_by.

Cause 1 of 2: the 204 response to DeleteRouteRequestParameter carries no Content-Length header.
AWS: Smithy (com.amazonaws.apigatewayv2#ApiGatewayV2) DeleteRouteRequestParameter - http DELETE /v2/apis/{ApiId}/routes/{RouteId}/requestparameters/{RequestParameterKey}, code 204, output shape Unit (no members); errors NotFoundException (404), TooManyRequestsException (429). AWS frames the empty success body explicitly: the recorded 204 carries a Content-Length header (sibling recordings of the same empty-body deletes assert the literal '0'), so the response is a complete HTTP message with a declared zero-length body rather than an unframed one.
Emulator: answers 204 with the header absent - detail `$headers.content-length: missing (expected '<shaped-not-asserted>', got None)`. The status itself matches (204/204) and no body is expected; only the framing header is missing.
Why: a client or proxy that reads the declared body length to decide the message is complete sees no Content-Length and no chunked framing, so strict HTTP clients and intermediaries must guess, and connection reuse after the delete can stall or the response be treated as malformed. A fixture asserting AWS's framing never matches.
Fix: the response serializer / framing layer for empty-output (204) apigateway deletes - emit 'Content-Length: 0' for an operation whose output shape is Unit, rather than dropping the header when the body is empty.
Traces: apigatewayv2-http-api-route-config-cleanup#task#6, apigatewayv2-route-request-parameter-lifecycle#task#5.
Related open issues that do NOT carry this replay key: 20261029T220031Z-apigateway-204-responses-omit-the-content-length-0.md and 20261031T154120Z-apigateway-content-length-0-missing-on-204-deletes-for.md enumerate other operations' 204s; DeleteRouteRequestParameter is unowned, so it is filed here and shares that root cause.

Cause 2 of 2: GetModelTemplate generates a mapping template whose properties are alphabetized and whose numeric sample value is hard-coded, instead of the template AWS derives from the model's JSON Schema.
AWS: Smithy GetModelTemplate - http GET /v2/apis/{ApiId}/models/{ModelId}/template, code 200; output member Value (jsonName 'value', target __string), documented only as 'The template value', so the generator's rules are recorded behavior only. The recording shows AWS emitting the object's properties in the order the model's schema declares them and taking each property's sample value from the schema: `#set($inputRoot = $input.path('$'))` followed by the object with the number property, the boolean property, the array property (wrapped in the '##TODO: Update this foreach loop...' #foreach block) and the nested object, in that declared order, with the numeric property rendered as the schema's value (recorded 42) and the string property as the schema's string sample.
Emulator: returns 200 with a template whose keys are sorted alphabetically (boolean, nested object, number, array) and whose numeric property is rendered as 0 - detail `$.value: value_mismatch`, expected '... "n" : 42, "b" : true, "tags" : [ ... ], "inner" : { "s" : "foo" } ...' versus actual '... "b" : true, "inner" : { "s" : "foo" }, "n" : 0, "tags" : [ ... ] ...'. The VTL preamble, the #foreach/#if TODO block and the string and boolean sample values match; what diverges is property order and the numeric sample. Hypothesis: the generator serializes the schema through a sorted map and substitutes a type-default zero for numbers instead of carrying the schema-supplied sample.
Why: this template is meant to be pasted into an integration request/response mapping as a starting point. A client that diffs or regenerates the template gets spurious changes on every model, and the zeroed numeric sample silently changes the mapping's meaning for the numeric field; template text is the whole payload of this operation, so a client has nothing else to fall back on.
Fix: the GetModelTemplate handler's template generator - walk the model schema in declaration order (preserve the schema's property order instead of sorting) and take each property's sample value from the schema rather than from a per-type default.
Traces: apigatewayv2-model-template-by-schema-type#task#6.

## Evidence
Replay of simparity-wire-aws-v1 (116 fixtures, 1057 steps, score 85.3%). Clusters with empty stub_path and empty owned_by:

1) divergence_key `service=apigateway|DeleteRouteRequestParameter|status=204->204|keys=$headers.content-length`, fixture apigatewayv2-http-api-route-config-cleanup step task#6, verdict mismatch, expected 204 / actual 204, 2 affected steps (apigatewayv2-http-api-route-config-cleanup#task#6, apigatewayv2-route-request-parameter-lifecycle#task#5). Expected: Content-Length present (asserted as '0' on sibling empty-body deletes). Actual: header absent.

2) divergence_key `service=apigateway|GetModelTemplate|status=200->200|keys=$.value`, fixture apigatewayv2-model-template-by-schema-type step task#6, verdict mismatch, expected 200 / actual 200, 1 affected step. Expected value (newlines shown as line breaks):
#set($inputRoot = $input.path('$'))
{
  "n" : 42,
  "b" : true,
  "tags" : [
##TODO: Update this foreach loop to reference array from input json
#foreach($elem in $inputRoot.TODO)
 "foo" 
#if($foreach.hasNext),#end
#end
],
  "inner" : {
    "s" : "foo"
  }
}
Actual value:
#set($inputRoot = $input.path('$'))
{
  "b" : true,
  "inner" : {
    "s" : "foo"
  },
  "n" : 0,
  "tags" : [
##TODO: Update this foreach loop to reference array from input json
#foreach($elem in $inputRoot.TODO)
 "foo" 
#if($foreach.hasNext),#end
#end
]
}
The divergences are property order (declaration order vs alphabetical) and the numeric sample (schema-supplied vs 0); the fixed VTL text matches exactly.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay simparity-wire-aws-v1.
2. Cause 1: replay apigatewayv2-http-api-route-config-cleanup#task#6 - DELETE /v2/apis/{apiId}/routes/{routeId}/requestparameters/{key}. The response is 204 with no Content-Length header; AWS sends Content-Length: 0. Same on apigatewayv2-route-request-parameter-lifecycle#task#5.
3. Cause 2: replay apigatewayv2-model-template-by-schema-type#task#6 - GET /v2/apis/{apiId}/models/{modelId}/template for a model whose schema declares a number, a boolean, an array and a nested object. The returned 'value' lists the properties alphabetically and renders the number as 0; AWS lists them in the schema's declaration order and renders the schema's numeric sample.
4. Expected: Content-Length: 0 on the 204, and a template whose property order and sample values follow the model schema.
