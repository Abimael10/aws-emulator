Issue-Source: trace-sweep-divergence
Issue-Trace: apigateway-documentation-part-conflict-duplicate-location
Issue-Step: task#2
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: apigateway-documentation-part-conflict-duplicate-location apigateway-documentation-parts-multiple-locations-and-listing apigateway-stage-method-settings-duplicate-spelling
Issue-Shape-Id: com.amazonaws.apigateway#BackplaneControlService
Issue-Key: apigateway|CreateDocumentationPart|2xx->2xx|$.id
Issue-Key: apigateway|DeleteRestApi|4xx->2xx|status
Issue-Commit: 0cc1593284dc57d1fa665d25f671e4cff118eb06

# apigateway duplicate-location CreateDocumentationPart mints a second part and DeleteRestApi accepts an unknown restApiId (2 causes)

> _Note: this issue was identified against the code base at commit `0cc1593284dc57d1fa665d25f671e4cff118eb06`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDocumentationPart

## Description
Two net-new root causes in the API Gateway REST control plane, both apigateway, both cases of the emulator treating a resource identity it already holds as if it were new. Neither is covered by an open issue (see the Why-not-related notes).

Cause 1 of 2: CreateDocumentationPart for a documentation-part location the REST API already has a part for mints a brand-new part id instead of reporting the existing part's id, so the API ends up with two parts for one location.
AWS: Smithy: CreateDocumentationPart, POST /restapis/{restApiId}/documentation/parts, http success code 201, output shape DocumentationPart {id: String, location: DocumentationPartLocation, properties: String}; modeled errors BadRequestException(400), ConflictException(409), LimitExceededException(429), NotFoundException(404), TooManyRequestsException(429), UnauthorizedException(401). The per-location identity rule is recorded behavior only: the model names ConflictException but does not spell out part identity, and the fixture asserts this response's `id` BY REFERENCE to the documentation-part id already bound earlier in the same fixture for the same location (the expectation is printed as the capture token, not as a literal id string). The recorded service answers 201 and reports THAT id — i.e. a documentation part is identified by its (restApiId, location) pair, and a create naming a location that already has a part resolves to the part that is already there rather than adding a second one. Hypothesis: AWS keys the documentation-part collection on the normalized location, which is also why its ConflictException exists for the same location at all; the recording only establishes the id equality.
Emulator: the same request is answered 201 with a freshly generated documentation-part id, so the assertion on `$.id` is reported as a value mismatch (expected the id already bound for that location, got a newly minted 10-character id). The same cluster covers three further CreateDocumentationPart steps in apigateway-documentation-parts-multiple-locations-and-listing, where a create for an already-populated location likewise returns a new id. No status divergence: 201 either way.
Why: a client that re-asserts documentation for a location (the normal pattern when documentation is regenerated from source control, or when the same CreateDocumentationPart call is retried after a timeout) silently accumulates one extra documentation part per call instead of landing on the one part for that location. It cannot correlate the returned id with the part it already tracks, GetDocumentationParts then enumerates duplicates for a single entity, and GetExport/OpenAPI publication renders the location's documentation more than once. Nothing in the response signals the duplication. The id VALUE in any one run is per-run and is NOT the requirement — the requirement is that it EQUALS the id of the part that already exists for that location.
Why-not-related: 20261107T204751Z-apigateway-api-gateway-documentation-parts-method-location-parts-dropped.md covers the same per-location identity rule on the ImportDocumentationParts merge path (its cause 3), plus METHOD-location retention and the listing's location filter; it does not cover the CreateDocumentationPart path, which mints its own id. 20261113T185449Z-apigateway-2-net-new-causes-importdocumentationparts-failonwarnings-rejection.md covers ImportDocumentationParts failOnWarnings refusal wording only.
Fix: the CreateDocumentationPart handler and the documentation-part state it writes — match the incoming location against the restApi's existing parts and resolve to that part (returning its id, location and properties) instead of inserting a new entry with a new id.
Traces: apigateway-documentation-part-conflict-duplicate-location#task#2 (representative); apigateway-documentation-parts-multiple-locations-and-listing#task#5, #task#6, #task#7.

Cause 2 of 2: DeleteRestApi on a restApiId the account does not hold is accepted with 202 instead of being refused 404 NotFoundException.
AWS: Smithy: DeleteRestApi, DELETE /restapis/{restApiId}, http success code 202 with an empty body (output Unit), modeled errors BadRequestException(400), ConflictException(409), NotFoundException(404), TooManyRequestsException(429), UnauthorizedException(401). A DELETE naming a REST API id that does not exist in the account — one that never existed, or one that a previous delete already removed — is refused, not absorbed: the recorded service answers HTTP 404 and the error is NotFoundException (the REST protocol reports it in x-amzn-errortype and in the JSON body's `message`). The recorded step asserts the status for this cause; the error class is the modeled NotFoundException. The message for this refusal is API Gateway's fixed REST-API-identifier sentence whose only variable part is the identifier taken from the request path — no part of it is drawn from one run's state.
Emulator: the same DELETE is answered 202 with an empty body, exactly as a successful delete of an existing API, so the step diverges on status only (expected 404, got 202) and no x-amzn-errortype or message is produced at all.
Why: DeleteRestApi is how a client both deletes and probes. A caller that deletes an API it already deleted, or that deletes using a stale/typo'd id, is told the delete succeeded — so idempotency and cleanup logic concludes the named API existed and was removed. A teardown loop that drives deletion until it sees NotFoundException can never observe completion, a client reconciling desired against actual state believes a resource it never created was present, and a typo'd restApiId is reported as a successful destructive operation instead of being rejected.
Why-not-related: no open apigateway issue covers DeleteRestApi's refusal for an unknown identifier; the id-validation issues on file concern other operations (e.g. 20261107T204751Z-apigateway-api-gateway-rest-refusals-updategatewayresponse-accepts-an-invalid.md for PutMethod/UpdateGatewayResponse message wording).
Fix: the DeleteRestApi handler's existence check before the delete, against the REST API state — look the restApiId up and raise the modeled NotFoundException (404) when the account holds no such API, rather than returning the 202 success code unconditionally.
Traces: apigateway-stage-method-settings-duplicate-spelling#teardown#12.

## Evidence
Replay of trace set simparity-wire-aws-v1 (200 fixtures, 1943 steps, 1886 passed) against the emulator built from this checkout; emulator up and serving (/__admin/health 200).

Cause 1 — fixture apigateway-documentation-part-conflict-duplicate-location, step task#2, CreateDocumentationPart, expected 201 / actual 201:
  $.id: value_mismatch (expected the documentation-part id already bound for that location earlier in the fixture — printed by the harness as the capture token '{{first_part_id}}' — got a newly generated id)
  divergence_key: service=apigateway|CreateDocumentationPart|status=201->201|keys=$.id
  cluster members (4): apigateway-documentation-part-conflict-duplicate-location#task#2, apigateway-documentation-parts-multiple-locations-and-listing#task#5, #task#6, #task#7
  (The generated id string observed in this run is a per-run value and is not part of the requirement; the requirement is the equality with the existing part's id for that location.)

Cause 2 — fixture apigateway-stage-method-settings-duplicate-spelling, step teardown#12, DeleteRestApi, expected 404 / actual 202:
  status: expected 404, got 202
  divergence_key: service=apigateway|DeleteRestApi|status=404->202|keys=status
  Expected: 404 with the modeled NotFoundException (x-amzn-errortype: NotFoundException) and API Gateway's invalid-REST-API-identifier message, whose only variable part is the identifier from the request path. Actual: 202 with an empty body, i.e. the success response of a delete that did remove an API.

Both clusters are reported with empty stub_path and empty owned_by, so neither is blocked by an unimplemented operation nor already carried by an open issue's replay key.

## Steps to Reproduce
1. Build and boot the emulator from this checkout.
2. Cause 1: CreateRestApi; CreateDocumentationPart on it with some location (e.g. {type: RESOURCE, path: '/pets'}) and a properties JSON string, and note the returned `id`. Call CreateDocumentationPart again on the same restApiId with the SAME location. The second response is 201 with a different, freshly minted `id`; GetDocumentationParts for that restApiId then enumerates two parts for the one location. Expected: the response reports the id of the part that already exists for that location (one part per location).
3. Cause 2: call DeleteRestApi with a restApiId the account does not hold — either a syntactically valid id that was never created, or the id of an API that a previous DeleteRestApi already removed. The emulator answers 202 with an empty body. Expected: 404 with NotFoundException (x-amzn-errortype: NotFoundException) and API Gateway's invalid-REST-API-identifier message naming the id from the request path.
4. Or replay the fixture steps directly: apigateway-documentation-part-conflict-duplicate-location#task#2 (plus apigateway-documentation-parts-multiple-locations-and-listing#task#5, #task#6, #task#7) and apigateway-stage-method-settings-duplicate-spelling#teardown#12.
