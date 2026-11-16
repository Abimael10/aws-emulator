Issue-Source: trace-sweep-divergence
Issue-Trace: endusermessaging-brand-profile-attribute-create-validation
Issue-Step: task#3
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: endusermessaging-brand-profile-attribute-create-validation
Issue-Shape-Id: com.amazonaws.endusermessaging#EndUserMessaging
Issue-Key: endusermessaging|CreateBrandProfileAttributes|4xx->4xx|$headers.x-amzn-errortype
Issue-Key: endusermessaging|CreateBrandProfileAttributes|4xx->4xx|status
Issue-Key: endusermessaging|CreateBrandProfileAttributes|4xx->4xx|$.fieldList[*].message
Issue-Key: endusermessaging|CreateBrandProfileAttributes|4xx->4xx|$.fieldList[*].path
Issue-Key: endusermessaging|CreateBrandProfileAttributes|4xx->2xx|$.fieldList
Issue-Key: endusermessaging|CreateBrandProfileAttributes|4xx->2xx|$.message
Issue-Key: endusermessaging|CreateBrandProfileAttributes|4xx->2xx|$headers.x-amzn-errortype
Issue-Key: endusermessaging|CreateBrandProfileAttributes|4xx->2xx|status
Issue-Key: endusermessaging|CreateBrandProfileAttributes|4xx->2xx|$.resourceType
Issue-Key: endusermessaging|CreateBrandProfileAttributes|2xx->4xx|$.attributes
Issue-Key: endusermessaging|CreateBrandProfileAttributes|2xx->4xx|status
Issue-Related: 20261111T021035Z-endusermessaging-brand-profile-validation-fieldlist-omitted-length-range.md
Issue-Commit: 47208604cde63241f79c213c4b593a23b3ff78ba

# endusermessaging CreateBrandProfileAttributes refusals diverge: conflict/validation gates inverted and fieldList paths malformed

> _Note: this issue was identified against the code base at commit `47208604cde63241f79c213c4b593a23b3ff78ba`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateBrandProfileAttributes

## Description
CreateBrandProfileAttributes decides the WRONG error class for its request-shape and resource-state gates, and when it does refuse it emits a non-JSONPointer field path and re-worded sentences. This bundle carries 5 net-new causes; all 5 must be closed before the issue is submitted.

Cause 1 of 5 — a duplicate attributeName INSIDE one request is answered with ConflictException instead of ValidationException.
AWS: a request whose `attributes` list names the same attributeName twice is malformed input, not a state conflict, so it is refused with HTTP 400 ValidationException. The refusal carries `message` = "Duplicate attribute name: '<attributeName>'" and a `fieldList` entry whose `path` is the JSONPointer to the offending list element ("/attributes/<index>/attributeName") and whose `message` repeats that sentence. Smithy: CreateBrandProfileAttributes errors include ValidationException (httpError 400) and ConflictException (httpError 409); ValidationException.fieldList is a ValidationExceptionFieldList whose `path` is documented as "A JSONPointer expression to the structure member whose value failed to satisfy the modeled constraints". Fixed text of the message: "Duplicate attribute name: " plus the duplicated name, in single quotes, taken from the request.
Emulator: answers HTTP 409 with x-amzn-errortype ConflictException, message 'Conflict occurred - Reason="RESOURCE_ALREADY_EXISTS" ResourceType="brand-profile-attribute"', and no fieldList at all.
Why: a client cannot distinguish "I sent a bad request" from "the resource already exists". 409 is a retry-after-state-change signal, so SDK/retry logic and user-facing error handling take the wrong branch, and the client is never told WHICH list element is duplicated.
Fix: the CreateBrandProfileAttributes handler's input-validation stage (intra-request uniqueness must be checked during validation, before the state-conflict check) and the ValidationException serializer.
Traces: endusermessaging-brand-profile-attribute-create-validation#task#3.

Cause 2 of 5 — the "attachmentBody on a TEXT attribute" refusal uses a dotted member path and a different sentence.
AWS: supplying `attachmentBody` for an attribute whose attributeType is TEXT is refused with HTTP 400 ValidationException whose `message` and whose single `fieldList[0].message` are both "attachmentBody cannot be provided for TEXT attribute '<attributeName>'", and whose `fieldList[0].path` is the JSONPointer "/attributes/<index>/attachmentBody". Smithy: BrandProfileAttributeInput.attachmentBody is documented "The binary content for an attribute of type IMAGE or DOCUMENT", attributeValue "applies to attributes of type TEXT"; ValidationExceptionField.path is a JSONPointer. Fixed text: "attachmentBody cannot be provided for TEXT attribute " plus the attribute's name in single quotes; the list index in the path comes from the request.
Emulator: emits path 'attributes.1.member.attachmentBody' — a dotted Smithy member path, not a JSONPointer, and with an index that does not match the offending element's position — fieldList[0].message 'Member must not be specified for an attribute of type TEXT; supply attributeValue instead', and the generic envelope message "1 validation error detected: Value at 'attributes.1.member.attachmentBody' failed to satisfy constraint: Member must not be specified for an attribute of type TEXT; supply attributeValue instead".
Why: clients and consoles that map fieldList[].path back onto the request document (JSONPointer) cannot resolve a dotted ".member." path, and the attribute name is absent from the message, so the user is not told which attribute is wrong.
Fix: the CreateBrandProfileAttributes validation stage — the type/attachment cross-member check and the JSONPointer path formatter shared by its fieldList entries.
Traces: endusermessaging-brand-profile-attribute-create-validation#task#4 (cluster also covers task#5, task#6).

Cause 3 of 5 — an unsupported `category` value is ACCEPTED instead of refused.
AWS: `category` is validated against the service's supported category list at runtime; an unsupported value is refused with HTTP 400 ValidationException whose `message` and whose fieldList entry (path "/attributes/<index>/category") read "Invalid category '<value>'. Supported values: BUSINESS_IDENTITY, BUSINESS_ADDRESS, BUSINESS_CONTACT, MESSAGING_USE_CASE, OPT_IN_CONSENT, MESSAGE_CONTENT, COMPLIANCE_KEYWORDS, TERMS_AND_POLICY, BRAND_DISPLAY, SUPPORTING_DOCUMENT, OTHER". Smithy: BrandProfileAttributeCategory is a string with @length 1..64 and the doc "Category for the attribute. Validated against supported values at runtime" — the enumeration itself is recorded behavior only, taken from the recorded refusal. Fixed text: "Invalid category " + the rejected value in single quotes + ". Supported values: " + the supported list; only the rejected value comes from the request.
Emulator: answers HTTP 200 and creates the attribute, with no message and no x-amzn-errortype header.
Why: the emulator silently persists a category the real service rejects, so a client that passes a bad category appears to succeed against the emulator and fails in production — and later reads return a category value AWS can never return.
Fix: the CreateBrandProfileAttributes validation stage — add the supported-category check (and the same check belongs on UpdateBrandProfileAttribute).
Traces: endusermessaging-brand-profile-attribute-create-validation#task#7 (cluster also covers task#8).

Cause 4 of 5 — re-creating an attribute that ALREADY EXISTS is accepted instead of answered with ConflictException.
AWS: attributeName is unique within a brand profile (Smithy: BrandProfileAttributeInput.attributeName doc "The name is unique within a brand profile"), so creating an attribute whose name is already present is refused with HTTP 409 ConflictException. The refusal carries `resourceType` = "brand-profile-attribute" and, per the recorded response, a `message` of the service's conflict form 'Conflict occurred - Reason="RESOURCE_ALREADY_EXISTS" ResourceType="brand-profile-attribute"' — the fixed text is the Reason/ResourceType envelope; the resource type is the conflicting resource's type from state. Smithy: ConflictException httpError 409 with optional resourceId/resourceType.
Emulator: answers HTTP 200 with no error body or errortype header, i.e. it has no already-exists gate — exactly the gate it misapplies in Cause 1.
Why: a client that relies on 409 to detect an existing attribute (create-or-update flows, idempotent retries that must not clobber) gets a success and overwrites or duplicates state; the emulator's state then diverges from what AWS would hold.
Fix: the CreateBrandProfileAttributes handler's state-conflict check against the brand profile's existing attribute set, in the brand-profile attribute state layer.
Traces: endusermessaging-brand-profile-attribute-create-validation#task#10.

Cause 5 of 5 — a legitimate create is answered with ConflictException instead of 200.
AWS: creating attributes whose names are not already present in the brand profile succeeds with HTTP 200 and an `attributes` output list echoing each created attribute's attributeName and attributeType (Smithy: CreateBrandProfileAttributesOutput.attributes is required, BrandProfileAttributeOutputList, @length 1..10).
Emulator: answers HTTP 409 and returns no `attributes` member. Hypothesis: this is the downstream effect of Cause 4 — the emulator persisted the attribute it should have refused at task#10, so the subsequent legitimate create now collides in emulator-only state. Whether or not that is the mechanism, the step must return 200 with the attributes list after the other causes are fixed.
Why: a correct client request is refused, so any flow that creates an attribute after a refused create cannot proceed at all.
Fix: the CreateBrandProfileAttributes handler's conflict check and the attribute-set state it consults; re-verify after Cause 4 lands.
Traces: endusermessaging-brand-profile-attribute-create-validation#task#12.

Related open issues that do NOT cover these causes: 20261104T183251Z-endusermessaging-input-validation-refusals-omit-fieldlist-and-diverge.md and 20261111T021035Z-endusermessaging-brand-profile-validation-fieldlist-omitted-length-range.md own the brand-PROFILE and @length/@range envelope refusals; none of them own the attribute-level conflict/category/attachment gates above.

## Evidence
Replay of trace set simparity-wire-aws-v1 (13 fixtures, 113 steps, 28 passed, score 24.8%), fixture endusermessaging-brand-profile-attribute-create-validation:

- task#3: expected 400, got 409. $.message expected "Duplicate attribute name: 'repeated'", got 'Conflict occurred - Reason="RESOURCE_ALREADY_EXISTS" ResourceType="brand-profile-attribute"'. $.fieldList expected [{'path': '/attributes/1/attributeName', 'message': "Duplicate attribute name: 'repeated'"}], got None. $headers.x-amzn-errortype got 'ConflictException'.
- task#4 (also task#5, task#6): status 400 both sides. $.fieldList[0].path expected '/attributes/0/attachmentBody', got 'attributes.1.member.attachmentBody'. $.fieldList[0].message expected "attachmentBody cannot be provided for TEXT attribute 'plain'", got 'Member must not be specified for an attribute of type TEXT; supply attributeValue instead'. $.message expected "attachmentBody cannot be provided for TEXT attribute 'plain'", got "1 validation error detected: Value at 'attributes.1.member.attachmentBody' failed to satisfy constraint: Member must not be specified for an attribute of type TEXT; supply attributeValue instead".
- task#7 (also task#8): expected 400, got 200. $.message expected "Invalid category 'MARKETING'. Supported values: BUSINESS_IDENTITY, BUSINESS_ADDRESS, BUSINESS_CONTACT, MESSAGING_USE_CASE, OPT_IN_CONSENT, MESSAGE_CONTENT, COMPLIANCE_KEYWORDS, TERMS_AND_POLICY, BRAND_DISPLAY, SUPPORTING_DOCUMENT, OTHER", got None. $.fieldList expected [{'path': '/attributes/0/category', ...}], got None. $headers.x-amzn-errortype expected present, got None.
- task#10: expected 409, got 200. $.message expected 'Conflict occurred - Reason="RESOURCE_ALREADY_EXISTS" ResourceType="brand-profile-attribute"', got None. $.resourceType expected 'brand-profile-attribute', got None. $headers.x-amzn-errortype expected present, got None.
- task#12: expected 200, got 409. $.attributes expected [{'attributeName': 'fresh', 'attributeType': 'TEXT'}, {'attributeName': 'logo', 'attributeType': 'IMAGE'}], got None.

Model evidence (services/service-com-amazonaws-endusermessaging-endusermessaging/smithy-model/com-amazonaws-endusermessaging-endusermessaging.json): CreateBrandProfileAttributes POST /v1/brand-profiles/{brandProfileId}/attributes, errors ValidationException(400), ConflictException(409), ResourceNotFoundException(404); ValidationException.fieldList -> ValidationExceptionField{path: JSONPointer expression, message}; BrandProfileAttributeInput.attributeName "unique within a brand profile"; attachmentBody "for an attribute of type IMAGE or DOCUMENT"; BrandProfileAttributeCategory "Validated against supported values at runtime".

## Steps to Reproduce
1. git clone the repo and boot the emulator from the checkout.
2. Replay the wire corpus (trace set simparity-wire-aws-v1) and watch fixture endusermessaging-brand-profile-attribute-create-validation.
3. Cause 1: POST /v1/brand-profiles/{id}/attributes with two list elements sharing one attributeName (step task#3) -> emulator answers 409 ConflictException; expected 400 ValidationException with fieldList path /attributes/1/attributeName.
4. Cause 2: POST the same URI with an element of attributeType TEXT that also sets attachmentBody (task#4) -> fieldList[0].path is 'attributes.1.member.attachmentBody'; expected JSONPointer '/attributes/0/attachmentBody' and the "attachmentBody cannot be provided for TEXT attribute '<name>'" sentence.
5. Cause 3: POST with category 'MARKETING' (task#7) -> 200; expected 400 ValidationException naming the supported category list.
6. Cause 4: POST an attributeName that already exists on the profile (task#10) -> 200; expected 409 ConflictException with resourceType 'brand-profile-attribute'.
7. Cause 5: POST a fresh, non-colliding attribute set afterwards (task#12) -> 409; expected 200 with the attributes list.
