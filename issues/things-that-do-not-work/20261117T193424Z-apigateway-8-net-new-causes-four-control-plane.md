Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: apigateway.create-rest-api-with-an-empty-description-rejected apigateway.create-authorizer-token-identitysource-not-header-rejected apigateway.create-domain-name-endpoint-access-mode-requires-tls13-policy-rejected apigateway.put-method-nonexistent-request-validator-rejected apigateway.apigw-put-integration-rejects-invalid-passthrough-behavior apigateway.create-documentation-version-nonexistent-stage-rejected apigateway.tag-resource-past-fifty-tags-on-the-api-rejected apigateway.get-usage-plan-keys-nonexistent-usage-plan-not-found
Issue-Shape-Id: com.amazonaws.apigateway#BackplaneControlService
Issue-Key: apigateway.create-rest-api-with-an-empty-description-rejected|_refusal#4|AssertionError|raise AssertionError(f"expected {code}, but the call succeeded: {result!r}")
Issue-Key: apigateway.create-authorizer-token-identitysource-not-header-rejected|_error#7|AssertionError|raise AssertionError(f"expected the call to be rejected, but it succeeded: {result!r}")
Issue-Key: apigateway.create-domain-name-endpoint-access-mode-requires-tls13-policy-rejected|_error#10|AssertionError|raise AssertionError(f"expected the call to be rejected, but it succeeded: {result!r}")
Issue-Key: apigateway.put-method-nonexistent-request-validator-rejected|_error#11|AssertionError|raise AssertionError(f"expected the call to be rejected, but it succeeded: {result!r}")
Issue-Key: apigateway.apigw-put-integration-rejects-invalid-passthrough-behavior|run#8|AssertionError|assert message == (
Issue-Key: apigateway.create-documentation-version-nonexistent-stage-rejected|run#13|AssertionError|assert code == "BadRequestException", (
Issue-Key: apigateway.tag-resource-past-fifty-tags-on-the-api-rejected|run#13|AssertionError|assert got == (_EXPECTED_CODE, _EXPECTED_MESSAGE), (
Issue-Key: api-gateway|GetUsagePlanKeys|NotFoundException
Issue-Commit: 5782d860642bee99336bbb1ce42eb572c637eba4

# apigateway: 8 net-new causes — four control-plane refusals API Gateway makes are accepted, and four more are refused with the wrong code or message

> _Note: this issue was identified against the code base at commit `5782d860642bee99336bbb1ce42eb572c637eba4`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateRestApi

## Description
8 net-new root causes, all in the apigateway control plane, all about the refusal contract of one operation each: four requests AWS refuses are ACCEPTED (and leave state behind), and four refusals carry the wrong error code or a reworded message. Each cause is a distinct operation with its own failing check and must be closed on its own.

Smithy model referenced throughout: services/service-com-amazonaws-apigateway-backplanecontrolservice/smithy-model/com-amazonaws-apigateway-backplanecontrolservice.json. Every operation below models BadRequestException (httpError 400) and NotFoundException (httpError 404) among its errors; the model carries no length/pattern constraint for the members involved, so the exact rule and wording of each refusal is recorded behavior, labelled as such per cause.

Cause 1 of 8: CreateRestApi accepts an empty description.
AWS: Smithy CreateRestApi (POST /restapis, success 201), input member `description` (String, unconstrained in the model), errors include BadRequestException 400. Recorded behavior only for the rule: a `description` of "" is refused with BadRequestException and the message "Description cannot be an empty string" — entirely fixed text, no variable part — and no API is created, so GetRestApis filtered to that name lists nothing and the same create with a one-character description succeeds and reads back `description == "x"`.
Emulator: answers 201 and creates the API (body carries id, name, apiKeySource HEADER, endpointConfiguration, rootResourceId, apiStatus AVAILABLE), so the refusal never happens and an API exists under that name.
Why: a client that relies on AWS rejecting an empty description silently gets a created REST API; validation tests and CI guards that assert the refusal pass against AWS and fail against the emulator, and the account accumulates an API the caller believes was never created.
Fix: input validation in the CreateRestApi handler (services/service-com-amazonaws-apigateway-backplanecontrolservice/src/ops/create_rest_api.rs) — refuse before the record is persisted.
Traces: apigateway.create-rest-api-with-an-empty-description-rejected.

Cause 2 of 8: CreateAuthorizer accepts a TOKEN authorizer whose identitySource is not a method request header.
AWS: Smithy CreateAuthorizer (POST /restapis/{restApiId}/authorizers, 201), input `type` (AuthorizerType enum) and `identitySource` (String), errors include BadRequestException 400. Recorded behavior only: for `type=TOKEN` the identity source must be a method request header matching 'method.request.header.[a-zA-Z0-9._-]+'; an identitySource such as a querystring expression is refused BadRequestException with the message "Invalid token source expression: <identitySource>. The source must be a method request header, matching 'method.request.header.[a-zA-Z0-9._-]+'" — the fixed text is everything except <identitySource>, which is the value copied from the request — and no authorizer is created.
Emulator: answers 201 and creates the authorizer, echoing type TOKEN with the querystring identitySource and authorizerResultTtlInSeconds 300.
Why: a TOKEN authorizer whose identity source can never be read is accepted, so the misconfiguration surfaces only later as an unauthorized/configuration error at the stage instead of at create time, and templates that AWS rejects deploy clean here.
Fix: validation in the CreateAuthorizer handler (src/ops/create_authorizer.rs) — the TOKEN-type identitySource pattern check.
Traces: apigateway.create-authorizer-token-identitysource-not-header-rejected.

Cause 3 of 8: CreateDomainName accepts endpointAccessMode on a domain whose securityPolicy is not a TLS-1.3 (SecurityPolicy_*) policy.
AWS: Smithy CreateDomainName (POST /domainnames, 201), input `securityPolicy` (SecurityPolicy enum) and `endpointAccessMode` (EndpointAccessMode enum), errors include BadRequestException 400. Recorded behavior only: endpointAccessMode is supported only with the SecurityPolicy_* (TLS 1.3) policies; supplying it together with securityPolicy TLS_1_2 is refused with BadRequestException and the message "Endpoint access mode is not supported for this security policy" (entirely fixed text), and the domain is NOT created — a following GetDomainName answers NotFoundException "Invalid domain name identifier specified". The identical endpointAccessMode is accepted once the securityPolicy is such a policy.
Emulator: answers 201 and creates the domain, reporting securityPolicy TLS_1_2 together with endpointAccessMode STRICT and domainNameStatus AVAILABLE.
Why: a client gets a custom domain whose reported access mode is not actually backed by the policy AWS requires for it; the configuration cannot be reproduced on AWS, and the check's later NotFound read finds a live domain instead.
Fix: cross-member validation in the CreateDomainName handler (src/ops/create_domain_name.rs) before the DomainName record is stored.
Traces: apigateway.create-domain-name-endpoint-access-mode-requires-tls13-policy-rejected.

Cause 4 of 8: PutMethod accepts a requestValidatorId that names no RequestValidator on the API.
AWS: Smithy PutMethod (PUT /restapis/{restApiId}/resources/{resourceId}/methods/{httpMethod}, 201), input `requestValidatorId` (String), errors include BadRequestException 400 and NotFoundException 404. Recorded behavior only: a requestValidatorId naming no validator on that API is refused BadRequestException "Invalid Request Validator identifier specified" (fixed text) and no method is created, so GetMethod for that verb answers NotFoundException "Invalid Method identifier specified"; a requestValidatorId naming a real validator is accepted and echoed.
Emulator: answers 201 and creates the method, echoing the bogus requestValidatorId verbatim (httpMethod POST, authorizationType NONE, requestValidatorId 'sim2realnone').
Why: the dangling reference is persisted, so the method is deployed with a validator id that resolves to nothing — requests that AWS would have had validated are served unvalidated, and the deploy-time error a client expects never arrives.
Fix: referential validation in the PutMethod handler (src/ops/put_method.rs) — resolve requestValidatorId against the API's validators before storing the method.
Traces: apigateway.put-method-nonexistent-request-validator-rejected.

Cause 5 of 8: PutIntegration's invalid-passthroughBehavior refusal carries the emulator's own wording.
AWS: Smithy PutIntegration (PUT .../methods/{httpMethod}/integration, 201), input `passthroughBehavior` (plain String — the model does NOT enumerate it), errors include BadRequestException 400. Recorded behavior only for the wording: a value outside WHEN_NO_MATCH, NEVER and WHEN_NO_TEMPLATES is refused BadRequestException with the message "Invalid PassthroughBehavior specified. Valid options are WHEN_NO_MATCH,NEVER,WHEN_NO_TEMPLATES" — all fixed text (the rejected value does NOT appear in it, the option list is comma-separated with no spaces and in that order) — and no integration is created, so GetIntegration answers NotFoundException "Invalid Integration identifier specified".
Emulator: refuses with BadRequestException but the message "Invalid passthroughBehavior specified: <value>; expected one of WHEN_NO_MATCH, WHEN_NO_TEMPLATES, NEVER" — the member name is lower-cased, the rejected request value is interpolated, the sentence structure and the option order/spacing all differ.
Why: the code is right but the message is not AWS's; clients and tools that match on the refusal text (CI assertions, user-facing error surfacing) diverge, and the message leaks a request value AWS does not echo.
Fix: the refusal message in the PutIntegration handler's validation (src/ops/put_integration.rs).
Traces: apigateway.apigw-put-integration-rejects-invalid-passthrough-behavior.

Cause 6 of 8: CreateDocumentationVersion refuses an unknown stageName with NotFoundException instead of BadRequestException.
AWS: Smithy CreateDocumentationVersion (POST /restapis/{restApiId}/documentation/versions, 201), input `stageName` (String), errors include BadRequestException 400 and NotFoundException 404. Recorded behavior only: a stageName naming no stage on the API is refused with code BadRequestException (HTTP 400) and the message "Invalid stage identifier specified" (fixed text), and no documentation version is created; the identical call naming a real deployed stage succeeds and echoes the version.
Emulator: refuses with NotFoundException (HTTP 404) instead, so the class of the refusal is wrong.
Why: client code branches on the error code — a 400 BadRequestException means "your input named no stage", a 404 NotFoundException means "the documentation-version resource does not exist"; retry/abort logic keyed on that distinction takes the wrong branch.
Fix: the error selection in the CreateDocumentationVersion handler's stage lookup (src/ops/create_documentation_version.rs).
Traces: apigateway.create-documentation-version-nonexistent-stage-rejected.

Cause 7 of 8: TagResource's past-50-tags refusal carries the emulator's own wording.
AWS: Smithy TagResource (PUT /tags/{resourceArn}, success 204), input `tags` (MapOfStringToString, required), errors include BadRequestException 400. Recorded behavior only for the wording: tagging a resource past the 50-tag limit is refused BadRequestException with the message "Up to 50 tags can be set per resource. No tags were set on this request." — entirely fixed text, with no count, no key list and no resource name interpolated — and none of the requested tags is applied.
Emulator: refuses BadRequestException with "tags may carry at most 50 entries, got 51" — a different sentence that interpolates the resulting count.
Why: the limit is enforced but the message is not AWS's, so text-matching clients diverge; the AWS wording additionally states the all-or-nothing effect ("No tags were set on this request.") that callers rely on.
Fix: the tag-limit refusal message in the TagResource handler/validation (src/ops/tag_resource.rs).
Traces: apigateway.tag-resource-past-fifty-tags-on-the-api-rejected.

Cause 8 of 8: GetUsagePlanKeys refuses an unknown usagePlanId instead of answering an empty listing.
AWS: Smithy GetUsagePlanKeys (GET /usageplans/{usagePlanId}/keys, success 200), output `items` (ListOfUsagePlanKey, wire key "item") + `position`, paginated {inputToken: position, outputToken: position, items: items, pageSize: limit}; NotFoundException 404 is modeled but, recorded behavior, is NOT what a usagePlanId naming no plan produces: the call answers 200 with an empty key list.
Emulator: answers NotFoundException (HTTP 404) "Invalid Usage Plan ID specified" for a usagePlanId that names no plan, aborting the caller.
Why: "does this plan have keys?" is answered with an exception instead of an empty list, so a client enumerating keys for ids it did not create must special-case an error AWS never raises. (Note: the empty listing this operation must then serve has to carry the `item` member present-and-empty — see open issue 20261114T023619Z-apigateway-nine-empty-collection-listings-omit-the-items.md, Cause 3 of 9, which covers the empty-member defect for this same operation but does not carry this check or this refusal.)
Fix: the unknown-plan path of the GetUsagePlanKeys handler (src/ops/get_usage_plan_keys.rs) — answer a 200 empty page rather than NotFoundException.
Traces: apigateway.get-usage-plan-keys-nonexistent-usage-plan-not-found.

## Evidence
Cause 1 — expected: BadRequestException, message "Description cannot be an empty string", no API created. Actual: "AssertionError: expected BadRequestException, but the call succeeded: {'ResponseMetadata': {'HTTPStatusCode': 201, ...}, 'id': ..., 'name': ..., 'apiKeySource': 'HEADER', 'rootResourceId': ..., 'apiStatus': 'AVAILABLE'}" (the id/name/date shown are per-run values, not requirements).

Cause 2 — expected: BadRequestException, message "Invalid token source expression: method.request.querystring.token. The source must be a method request header, matching 'method.request.header.[a-zA-Z0-9._-]+'". Actual: "AssertionError: expected the call to be rejected, but it succeeded: {'ResponseMetadata': {'HTTPStatusCode': 201, ...}, 'type': 'TOKEN', 'identitySource': 'method.request.querystring.token', 'authorizerResultTtlInSeconds': 300}".

Cause 3 — expected: BadRequestException, message "Endpoint access mode is not supported for this security policy", then GetDomainName -> NotFoundException "Invalid domain name identifier specified". Actual: "AssertionError: expected the call to be rejected, but it succeeded: {'ResponseMetadata': {'HTTPStatusCode': 201, ...}, 'domainNameStatus': 'AVAILABLE', 'securityPolicy': 'TLS_1_2', 'endpointAccessMode': 'STRICT', ...}".

Cause 4 — expected: BadRequestException, message "Invalid Request Validator identifier specified", then GetMethod(POST) -> NotFoundException "Invalid Method identifier specified". Actual: "AssertionError: expected the call to be rejected, but it succeeded: {'ResponseMetadata': {'HTTPStatusCode': 201, ...}, 'httpMethod': 'POST', 'authorizationType': 'NONE', 'apiKeyRequired': False, 'requestValidatorId': 'sim2realnone'}".

Cause 5 — expected message: "Invalid PassthroughBehavior specified. Valid options are WHEN_NO_MATCH,NEVER,WHEN_NO_TEMPLATES". Actual: "AssertionError: unexpected message: 'Invalid passthroughBehavior specified: sim2real-not-a-real-value; expected one of WHEN_NO_MATCH, WHEN_NO_TEMPLATES, NEVER'" (the rejected value itself is a per-run string; the divergence is the fixed text, the interpolation and the option order).

Cause 6 — expected code: BadRequestException with message "Invalid stage identifier specified". Actual: "AssertionError: expected BadRequestException for a stageName naming no stage, got 'NotFoundException'".

Cause 7 — expected: ('BadRequestException', 'Up to 50 tags can be set per resource. No tags were set on this request.'). Actual: "AssertionError: three new keys onto 48 gave ('BadRequestException', 'tags may carry at most 50 entries, got 51'), expected ('BadRequestException', 'Up to 50 tags can be set per resource. No tags were set on this request.')".

Cause 8 — expected: HTTP 200 with items == [] for a usagePlanId naming no plan. Actual: "botocore.errorfactory.NotFoundException: An error occurred (NotFoundException) when calling the GetUsagePlanKeys operation: Invalid Usage Plan ID specified".

## Steps to Reproduce
Point a boto3 `apigateway` client at the emulator.
1. Cause 1: `create_rest_api(name=<name>, description="")` — expect BadRequestException "Description cannot be an empty string"; the emulator returns 201. Then `get_rest_apis()` and confirm no API under <name>.
2. Cause 2: `create_rest_api(...)`, then `create_authorizer(restApiId=..., name=..., type="TOKEN", authorizerUri=<lambda invoke uri>, identitySource="method.request.querystring.token")` — expect BadRequestException with the token-source message; the emulator returns 201.
3. Cause 3: import a certificate into ACM, then `create_domain_name(domainName=<d>, regionalCertificateArn=<arn>, endpointConfiguration={"types":["REGIONAL"]}, securityPolicy="TLS_1_2", endpointAccessMode="STRICT")` — expect BadRequestException "Endpoint access mode is not supported for this security policy" and `get_domain_name(domainName=<d>)` -> NotFoundException; the emulator creates the domain.
4. Cause 4: `create_request_validator(...)`, `put_method(..., httpMethod="GET", requestValidatorId=<real id>)` (succeeds), then `put_method(..., httpMethod="POST", authorizationType="NONE", requestValidatorId="sim2realnone")` — expect BadRequestException "Invalid Request Validator identifier specified"; the emulator returns 201.
5. Cause 5: `put_method(...GET)` then `put_integration(..., type="MOCK", requestTemplates={...}, passthroughBehavior=<not WHEN_NO_MATCH/NEVER/WHEN_NO_TEMPLATES>)` and compare the BadRequestException message with AWS's fixed text.
6. Cause 6: create an API with a method+integration, `create_deployment(stageName=<stage>)`, `create_documentation_part(location={"type":"API"}, ...)`, `create_documentation_version(documentationVersion="1.0.0", stageName=<stage>)` (succeeds), then the same call with `stageName="<no-such-stage>"` — expect BadRequestException "Invalid stage identifier specified"; the emulator raises NotFoundException.
7. Cause 7: tag a REST API up to 48 tags, then `tag_resource(resourceArn=<api arn>, tags=<3 new keys>)` and compare the message.
8. Cause 8: `get_usage_plan_keys(usagePlanId="sim2real-missing-usage-plan")` — expect HTTP 200 with `items == []`; the emulator raises NotFoundException "Invalid Usage Plan ID specified".
Full scenarios: replay the eight check ids in trace_ids.
