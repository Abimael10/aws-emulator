Issue-Source: trace-sweep-divergence
Issue-Trace: apigatewayv2-http-api-explicit-default-selection-expressions-cleared-by-empty-strings-and-tags
Issue-Step: task#3
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: apigatewayv2-http-api-explicit-default-selection-expressions-cleared-by-empty-strings-and-tags
Issue-Shape-Id: com.amazonaws.apigatewayv2#ApiGatewayV2
Issue-Key: apigatewayv2|UpdateApi|2xx->2xx|$.description
Issue-Commit: 52117eded78ef5a24c989f741f900c823ab6c32b

# apigatewayv2 UpdateApi stores an empty-string description instead of clearing the member

> _Note: this issue was identified against the code base at commit `52117eded78ef5a24c989f741f900c823ab6c32b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateApi

## Description
This issue carries 1 cause.

Cause 1 of 1: UpdateApi now ACCEPTS a request that clears Description with an empty string, but it stores and echoes the empty string as a value instead of removing the member from the API.

AWS: Smithy apigatewayv2#UpdateApi (PATCH /v2/apis/{ApiId}, http code 200) takes Description -> StringWithLengthBetween0And1024 (jsonName "description"), so the empty string is a legal input value, and returns Description in UpdateApiResponse; errors are BadRequestException (400), ConflictException (409), NotFoundException (404), TooManyRequestsException (429). The model does not state what an empty Description means on the wire, so for the clearing semantics this is recorded behavior only: the recording for this step shows the update accepted and the response body echoing the API (apiId, name, protocolType, ipAddressType, routeSelectionExpression, apiKeySelectionExpression, apiEndpoint, createdDate, tags, disableExecuteApiEndpoint) with NO description member at all - i.e. API Gateway v2 treats an empty-string Description in a PATCH as "remove this field" and the member is then absent from every response that would carry it. Generally: when a member is cleared, AWS omits it from the serialized response rather than serializing a zero-length value.

Emulator: answers the same status as the recording for this step (201) but its response body contains "description": "" where AWS omits the member entirely. Replay reports `$.description: unexpected_present (expected None, got '')`. No status, error code or error message diverges on this step - the divergence is purely the presence of a cleared member in the response body.

Why: a client that clears an API's description and then inspects the response cannot tell the clear happened - `"description" in api` is true where AWS gives absence, so code that branches on member presence (very common for optional REST-JSON members) concludes the description is still set. Anything that round-trips the returned body into a later UpdateApi/CreateApi call now re-sends an explicit empty description, and any diff of "API as AWS reports it" against "API as the emulator reports it" shows a spurious field forever.

Fix: the UpdateApi handler's API state mutation and its response serializer in the apigatewayv2 service - an empty-string Description in the PATCH must delete the stored member rather than store "", and the serializer must omit a cleared/absent Description instead of emitting a zero-length string. The same rule applies to the other optional string members UpdateApi can clear.

Related open issue: 20261117T231134Z-apigatewayv2-empty-string-field-clears-are-rejected-or.md carries the same fixture and step for the EARLIER behavior (UpdateApi refused the request with 400 and serialized none of the API members). That refusal no longer reproduces - the emulator now accepts the update - so the replay no longer attributes this cluster to that issue. What remains is the second half of the same semantics gap: the clear is accepted but recorded as an empty value instead of an absence. That issue should not be closed on the strength of the 400 disappearing while this body divergence stands.

Traces: apigatewayv2-http-api-explicit-default-selection-expressions-cleared-by-empty-strings-and-tags#task#3 (UpdateApi).

## Evidence
Fixture apigatewayv2-http-api-explicit-default-selection-expressions-cleared-by-empty-strings-and-tags, step task#3, operation UpdateApi, service apigatewayv2.
Expected status 201, actual status 201 (status matches).
Body divergence: $.description: unexpected_present (expected None, got '').
Expected: the response carries no `description` member, because the PATCH cleared it with an empty string.
Actual: the response carries `"description": ""`.
No error code or error message is involved in this step; the only requirement that diverges is the omission of the cleared member from the response body.

## Steps to Reproduce
1. Replay fixture apigatewayv2-http-api-explicit-default-selection-expressions-cleared-by-empty-strings-and-tags against the emulator.
2. Step task#3 PATCHes /v2/apis/{apiId} setting the explicit default routeSelectionExpression/apiKeySelectionExpression and clearing description with an empty string.
3. Compare the response body against the recording: expect NO `description` member, observe `"description": ""`.
4. Equivalent by hand: CreateApi with a description, then UpdateApi with `{"description": ""}`, and check whether `description` appears in the UpdateApi response (and in a subsequent GetApi).
