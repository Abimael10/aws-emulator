Issue-Source: trace-sweep-divergence
Issue-Trace: endusermessaging-brand-profile-attribute-create-validation
Issue-Step: task#11
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: endusermessaging-brand-profile-attribute-create-validation endusermessaging-brand-profile-attribute-lifecycle endusermessaging-brand-profile-attribute-update
Issue-Shape-Id: com.amazonaws.endusermessaging#EndUserMessaging
Issue-Key: endusermessaging|GetBrandProfileAttribute|4xx->2xx|$.message
Issue-Key: endusermessaging|GetBrandProfileAttribute|4xx->2xx|$.resourceId
Issue-Key: endusermessaging|GetBrandProfileAttribute|4xx->2xx|$.resourceType
Issue-Key: endusermessaging|GetBrandProfileAttribute|4xx->2xx|$headers.x-amzn-errortype
Issue-Key: endusermessaging|GetBrandProfileAttribute|4xx->2xx|status
Issue-Key: endusermessaging|GetBrandProfileAttribute|2xx->2xx|$.category
Issue-Key: endusermessaging|GetBrandProfileAttribute|2xx->2xx|$.createdAt
Issue-Key: endusermessaging|GetBrandProfileAttribute|2xx->2xx|$.description
Issue-Key: endusermessaging|GetBrandProfileAttribute|2xx->2xx|$.updatedAt
Issue-Commit: 47208604cde63241f79c213c4b593a23b3ff78ba

# endusermessaging brand-profile attribute reads diverge: missing attribute returns 200, projection drops members, timestamps mis-serialized

> _Note: this issue was identified against the code base at commit `47208604cde63241f79c213c4b593a23b3ff78ba`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetBrandProfileAttribute

## Description
The GetBrandProfileAttribute read path diverges on all three of lookup outcome, projected members and timestamp serialization. This bundle carries 4 net-new causes; all 4 must be closed before the issue is submitted.

Cause 1 of 4 — reading an attribute that does not exist returns HTTP 200 instead of ResourceNotFoundException.
AWS: GET /v1/brand-profiles/{brandProfileId}/attributes/{attributeName} for an attributeName the brand profile does not hold is refused with HTTP 404 ResourceNotFoundException, carrying `resourceType` = "brand-profile-attribute", `resourceId` = the requested attributeName, and the service's not-found message form 'Resource not found - ResourceType="brand-profile-attribute"'. Smithy: GetBrandProfileAttribute errors include ResourceNotFoundException (httpError 404), documented "The request references a resource that does not exist"; its optional members are resourceId ("The identifier of the resource that could not be found") and resourceType. Fixed text: the 'Resource not found - ResourceType="..."' envelope; the resource type is constant for this resource and the resource id comes from the request path.
Emulator: answers HTTP 200 with no message, no resourceId/resourceType and no x-amzn-errortype header — i.e. it serves a body for an attribute that was never created.
Why: existence checks are the normal way a client decides whether to create or update an attribute. A 200 for an absent attribute makes every such check report "present", and the client then reads a body AWS would never return.
Fix: the GetBrandProfileAttribute handler's lookup in the brand-profile attribute state — a miss must raise ResourceNotFoundException rather than fall through to the success serializer.
Traces: endusermessaging-brand-profile-attribute-create-validation#task#11 (cluster also covers endusermessaging-brand-profile-attribute-lifecycle#task#6 and endusermessaging-brand-profile-attribute-update#task#11).

Cause 2 of 4 — the success projection omits `category` and `description`.
AWS: the GetBrandProfileAttribute output projects the attribute's stored category and description whenever they are set, alongside attributeName/attributeType/createdAt/updatedAt. Smithy: GetBrandProfileAttributeOutput members include `description` (BrandProfileAttributeDescription, @length 0..1024, sensitive) and `category` (BrandProfileAttributeCategory, @length 1..64) — both modeled output members of this operation.
Emulator: returns neither member (both reported missing) even for an attribute created with a category and a description, so whatever the create call stored for those two members is not read back.
Why: a client that round-trips an attribute loses its category and description — the category is what groups attributes in a brand profile, so downstream logic that branches on it sees every attribute as uncategorised.
Fix: the GetBrandProfileAttribute output projection/serializer and, if the members are not retained, the attribute record in the brand-profile attribute state written by CreateBrandProfileAttributes.
Traces: endusermessaging-brand-profile-attribute-create-validation#task#13.

Cause 3 of 4 — createdAt/updatedAt are serialized in a form the recorded shape does not accept.
AWS: createdAt and updatedAt are required Timestamp members of GetBrandProfileAttributeOutput and are serialized in the form the recording asserts (the fixture asserts their SHAPE, not their value, so the clock reading itself is not the finding). Smithy: GetBrandProfileAttributeOutput.createdAt / .updatedAt, target Timestamp, both required.
Emulator: emits a bare JSON float with sub-microsecond fractional digits for both members, which fails the recorded shape assertion. This is the same serialization fault the open issue 20261104T183251Z-endusermessaging-brand-profile-success-bodies-diverge-nanosecond-precision.md reports for brand-PROFILE bodies; that issue does not cover the brand-profile-ATTRIBUTE projections, which is why it is filed here.
Why: a client parsing these members against the modeled timestamp form either rejects the body or reads a precision AWS never emits; two attributes written in one call also become indistinguishable from distinct writes.
Fix: the timestamp serializer used by the brand-profile attribute output projections (GetBrandProfileAttribute and its siblings), not the clock source.
Traces: endusermessaging-brand-profile-attribute-lifecycle#task#3.

Cause 4 of 4 — an attribute update is not visible to the following read (stale `description`).
AWS: after a successful attribute update, a read of the same attributeName projects the UPDATED member values; `description` reflects the most recent write. Smithy: UpdateBrandProfileAttribute is PUT /v1/brand-profiles/{brandProfileId}/attributes/{attributeName}, @idempotent, taking attributeValue/description/category and returning the updated projection.
Emulator: the read still projects the value written at create time, so the update did not reach the attribute record. Hypothesis: the update never lands because UpdateBrandProfileAttribute is still an unimplemented scaffold (open issue 20261110T210354Z-endusermessaging-updatebrandprofileattribute-stub.md); that stub issue owns implementing the operation, but it does not own this read-after-write divergence, which must be re-verified on the read path once the operation exists.
Why: read-after-write is the basic contract of this resource; a client that updates then reads sees its own write silently dropped and will re-apply or report failure.
Fix: UpdateBrandProfileAttribute's write into the brand-profile attribute state, and the GetBrandProfileAttribute projection that must read it back.
Traces: endusermessaging-brand-profile-attribute-update#task#4.

## Evidence
Replay of trace set simparity-wire-aws-v1 (13 fixtures, 113 steps, 28 passed, 85 mismatched, score 24.8%):

- endusermessaging-brand-profile-attribute-create-validation#task#11 (GetBrandProfileAttribute; cluster also endusermessaging-brand-profile-attribute-lifecycle#task#6, endusermessaging-brand-profile-attribute-update#task#11): status expected 404, got 200. $.message expected 'Resource not found - ResourceType="brand-profile-attribute"', got None. $.resourceId expected 'fresh', got None. $.resourceType expected 'brand-profile-attribute', got None. $headers.x-amzn-errortype expected present, got None.
- endusermessaging-brand-profile-attribute-create-validation#task#13 (GetBrandProfileAttribute, 200/200): $.category expected 'OTHER', got None. $.description expected 'a custom note', got None. $.createdAt / $.updatedAt expected shape-asserted, got 1794868669.7007089.
- endusermessaging-brand-profile-attribute-lifecycle#task#3 (GetBrandProfileAttribute, 200/200): $.createdAt and $.updatedAt expected shape-asserted, got 1794868670.5073433 (bare float, sub-microsecond fraction). The value itself is per-run; the divergence is the serialized form.
- endusermessaging-brand-profile-attribute-update#task#4 (GetBrandProfileAttribute, 200/200): $.description expected 'second', got 'first' — the pre-update value; plus the same createdAt/updatedAt form divergence (1794868672.1456664).

Model evidence (services/service-com-amazonaws-endusermessaging-endusermessaging/smithy-model/com-amazonaws-endusermessaging-endusermessaging.json): GetBrandProfileAttribute GET /v1/brand-profiles/{brandProfileId}/attributes/{attributeName}, @readonly, errors ValidationException(400)/ResourceNotFoundException(404)/AccessDeniedException(403)/ThrottlingException(429)/InternalServerException(500); output members attributeName, attributeType, attributeValue, description, category, mediaContentType, mediaSizeBytes, mediaDownloadUrl, createdAt (required Timestamp), updatedAt (required Timestamp). ResourceNotFoundException{message required, resourceId, resourceType}. UpdateBrandProfileAttribute PUT same URI, @idempotent.

## Steps to Reproduce
1. git clone the repo and boot the emulator from the checkout.
2. Replay the wire corpus (trace set simparity-wire-aws-v1).
3. Cause 1: GET /v1/brand-profiles/{id}/attributes/{neverCreatedName} (endusermessaging-brand-profile-attribute-create-validation#task#11) -> emulator answers 200; expected 404 ResourceNotFoundException with resourceType 'brand-profile-attribute' and resourceId = the requested name.
4. Cause 2: create an attribute with a category and a description, then GET it (same fixture, task#13) -> response omits both `category` and `description`.
5. Cause 3: GET any existing attribute (endusermessaging-brand-profile-attribute-lifecycle#task#3) -> createdAt/updatedAt come back as bare floats with sub-microsecond fractions and fail the recorded shape assertion.
6. Cause 4: PUT /v1/brand-profiles/{id}/attributes/{name} changing `description`, then GET the same attribute (endusermessaging-brand-profile-attribute-update#task#4) -> the read still returns the create-time description.
