Issue-Source: trace-sweep-divergence
Issue-Trace: endusermessaging-brand-profile-attribute-create-validation
Issue-Step: task#11
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: endusermessaging-brand-profile-attribute-create-validation endusermessaging-brand-profile-attribute-update endusermessaging-brand-profile-identifier-and-list-errors
Issue-Shape-Id: com.amazonaws.endusermessaging#EndUserMessaging
Issue-Key: endusermessaging|GetBrandProfileAttribute|4xx->2xx|$.message
Issue-Key: endusermessaging|GetBrandProfileAttribute|4xx->2xx|$.resourceId
Issue-Key: endusermessaging|GetBrandProfileAttribute|4xx->2xx|$.resourceType
Issue-Key: endusermessaging|GetBrandProfileAttribute|4xx->2xx|$headers.x-amzn-errortype
Issue-Key: endusermessaging|GetBrandProfileAttribute|4xx->2xx|status
Issue-Key: endusermessaging|GetBrandProfileAttribute|2xx->2xx|$.category
Issue-Key: endusermessaging|GetBrandProfileAttribute|2xx->2xx|$.description
Issue-Key: endusermessaging|UpdateBrandProfileAttribute|4xx->4xx|$.fieldList[*].path
Issue-Key: endusermessaging|ListJobs|4xx->4xx|$.message
Issue-Commit: 75e93429d3ea083e11be0578db4814848b1cd3ff

# endusermessaging brand-profile attribute reads and refusal envelopes diverge: 4 net-new causes

> _Note: this issue was identified against the code base at commit `75e93429d3ea083e11be0578db4814848b1cd3ff`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetBrandProfileAttribute

## Description
Replaying the wire corpus simparity-wire-aws-v1 against the booted emulator (built from this checkout) leaves four endusermessaging divergence clusters that NO open issue owns and that no scaffold stub blocks: two on the brand-profile-attribute READ path and two on the per-field refusal envelope of an attribute update and of ListJobs. This bundle carries 4 net-new causes; ALL 4 must be closed before the issue is submitted. Each cause names its own fixture and step below.

Model evidence used throughout (services/service-com-amazonaws-endusermessaging-endusermessaging/smithy-model/com-amazonaws-endusermessaging-endusermessaging.json): GetBrandProfileAttribute GET /v1/brand-profiles/{brandProfileId}/attributes/{attributeName}, output members attributeName/attributeType/createdAt/updatedAt @required plus optional attributeValue, description (@length 0..1024, @sensitive), category (BrandProfileAttributeCategory @length 1..64), mediaContentType, mediaSizeBytes, mediaDownloadUrl; errors ResourceNotFoundException(404), ValidationException(400). CreateBrandProfileAttributes POST on the same collection, input attributes @required @length 1..10 of elements carrying attributeName/attributeType/attributeValue/attachmentBody/description/category, clientToken @idempotencyToken; errors ValidationException(400), ConflictException(409), ResourceNotFoundException(404), ServiceQuotaExceededException(400). UpdateBrandProfileAttribute PUT on one attribute; errors ValidationException(400), ConflictException(409), ResourceNotFoundException(404). ListJobs GET /v1/jobs with operationType as an httpQuery of JobOperationType (a plain string, @length 1..128 — the model declares NO value set); errors ValidationException(400). ValidationException models message @required and fieldList -> ValidationExceptionField{path @required, message @required}.

=== Cause 1 of 4: an attribute from a batch AWS refuses outright survives in state, so GetBrandProfileAttribute answers 200 for an attribute that does not exist ===
AWS: CreateBrandProfileAttributes is atomic — when one element of the batch is refused (here, an element whose attachment bytes are not a supported media type), NO element of the batch is created. A subsequent GetBrandProfileAttribute for a name created by that refused batch is therefore a miss, answered HTTP 404 ResourceNotFoundException with message 'Resource not found - ResourceType="brand-profile-attribute"' and the members resourceType='brand-profile-attribute' and resourceId=<the attributeName from the request>. Fixed text: 'Resource not found - ResourceType=' plus the quoted resource-type token 'brand-profile-attribute'; the attribute name is reported ONLY in the modelled resourceId member and is never interpolated into the sentence, because BrandProfileAttributeName is @sensitive. The attribute name in the recording ('fresh') is this run's request value, not part of the requirement. Atomicity and the media-type gate are recorded behavior only; the 404 class and its members are modelled (ResourceNotFoundException httpError 404).
Emulator: answers HTTP 200 with a full attribute projection — no error, no x-amzn-errortype header — because the earlier batch create was accepted instead of refused and its attributes were written to the profile. The handler's miss path itself is correct (services/service-com-amazonaws-endusermessaging-endusermessaging/src/ops/get_brand_profile_attribute.rs:194-202 builds exactly the 404 above via crate::state::brand_profile_attribute_not_found); what is wrong is that the attribute is in the store at all.
Why: a client that probes for an attribute to decide whether to create it, or that asserts a refused batch created nothing, sees a resource AWS does not have. Every later call branches on that phantom attribute — the legitimate create of the same name is then refused 409 (see cause 2), so a flow that works against AWS cannot make progress against the emulator.
Fix: the CreateBrandProfileAttributes handler's media/content validation stage and its all-or-nothing commit — the element must be refused before anything is written, so the store never holds an attribute AWS never created. Open issue 20261116T224049Z-endusermessaging-createbrandprofileattributes-refusals-diverge-conflict-validation-gates-inverted.md owns the CREATE step that accepts the bad batch, but carries no replay key for this read step, so this read-side divergence is filed here and must be closed against this step too.
Traces: endusermessaging-brand-profile-attribute-create-validation#task#11.

=== Cause 2 of 4: GetBrandProfileAttribute omits the modelled category and description of the attribute the request addressed ===
AWS: GetBrandProfileAttribute reports the attribute as it was created: an attribute created with category and description carries both in the 200 body (category a supported category token, description the free-text note from the create request). An attribute name that a previous, properly refused batch did not create is free, so the create that supplies category and description succeeds and the read reports them.
Emulator: answers 200 but with category and description absent from the body entirely. The projection code is right (get_brand_profile_attribute.rs:204-231 copies attribute.description and attribute.category, and create_brand_profile_attributes.rs:303-313 persists both from the request), so the body is read off the WRONG record: the stale attribute left behind by cause 1 (created without those members), while the create that would have set them was refused 409 against that stale name. Hypothesis: with cause 1 fixed the stale record disappears and this read reports both members; if any projection path still drops them, the create-side persistence of description/category must be checked per element.
Why: a client reading back an attribute it just created finds two members it supplied silently missing, so a console or sync job that keys off category (for grouping brand data) or surfaces description shows empty values for data AWS returns.
Fix: same component as cause 1 (CreateBrandProfileAttributes atomicity/validation) plus the GetBrandProfileAttribute projection if the members are still absent once the stale record is gone.
Traces: endusermessaging-brand-profile-attribute-create-validation#task#13.

=== Cause 3 of 4: UpdateBrandProfileAttribute's cross-member refusal reports a JSON-pointer path instead of the member's own name ===
AWS: supplying a member the stored attribute's type does not take (attachmentBody on a TEXT attribute, attributeValue on an IMAGE/DOCUMENT attribute) is refused with HTTP 400 ValidationException whose message is '<member> cannot be provided for <attributeType> attribute' and whose single fieldList entry names the member at fault by its BARE member name — path 'attachmentBody' (and 'attributeValue' for the mirrored case) — repeating that same sentence as its message. Fixed text: ' cannot be provided for ' and ' attribute'; the variable parts are the member name supplied by the request and the stored attribute's own type. This is the same bare-name convention this service uses for its other own-gate refusals (state.rs:393-414 documents it for brandProfileId; state/jobs.rs:66-68 for operationType). The status, error class and message already match — only the path diverges.
Emulator: emits the JSON pointer '/attachmentBody' (and '/attributeValue') as ValidationExceptionField.path, because the pointer literal is passed in at services/service-com-amazonaws-endusermessaging-endusermessaging/src/ops/update_brand_profile_attribute.rs:272-295, where member_not_for_attribute_type is called with "/attachmentBody" / "/attributeValue" alongside the bare member name.
Why: a client or console that indexes fieldList by input member name to highlight the offending field looks up 'attachmentBody' and finds nothing, because the key handed back is '/attachmentBody'; the machine-readable half of the refusal is unusable even though its text is right.
Fix: the UpdateBrandProfileAttribute handler's validation stage — pass the bare member name as the fieldList path for this cross-member check (state/brand_profile_attributes.rs:237-250 takes the path as a parameter, so only the call sites change).
Traces: endusermessaging-brand-profile-attribute-update#task#6 (cluster also covers endusermessaging-brand-profile-attribute-update#task#7, the attributeValue-on-media-type mirror of the same gate).

=== Cause 4 of 4: ListJobs re-words the unknown-operationType refusal and lists the valid values in its own format and order ===
AWS: operationType is a plain string with no modelled value set, so an unrecognised value is refused by the service's own gate with HTTP 400 ValidationException whose message is "Invalid value for 'operationType': unknown operation type: <value>. Valid values: CreateRegistrationsFromBrandProfile | UpdateRegistrationsFromBrandProfile | CreateBrandProfileFromRegistration | UpdateBrandProfileFromRegistration". Fixed text: "Invalid value for 'operationType': unknown operation type: ", then '. Valid values: ', then the four accepted values separated by ' | ' in that order; the only variable part is the rejected value taken verbatim from the request's query string. Recorded behavior only for the value set and its rendering (the model declares neither); the 400 ValidationException class is modelled.
Emulator: answers 400 ValidationException but with the message "Invalid operationType '<value>'. Supported values: CreateBrandProfileFromRegistration, CreateRegistrationsFromBrandProfile, UpdateBrandProfileFromRegistration, UpdateRegistrationsFromBrandProfile" — a different sentence (the value single-quoted inline rather than after a 'unknown operation type: ' clause, 'Supported values' instead of 'Valid values') and a comma-separated list in alphabetical rather than the service's order. Built at services/service-com-amazonaws-endusermessaging-endusermessaging/src/state/jobs.rs:260-272 (invalid_job_operation_type) from JOB_OPERATION_TYPE_VALUES.join(", ") (state/jobs.rs:41-46), reached from src/ops/list_jobs.rs:174-185.
Why: the accepted values are only discoverable from this message, so a client or operator parsing it gets a list in the wrong separator and order, and any client or test matching AWS's sentence does not recognise the refusal. The status and error class are right, which makes the wording divergence silent.
Fix: the ListJobs refusal constructor for operationType (state/jobs.rs invalid_job_operation_type) — emit AWS's sentence with the ' | '-separated list in the service's own order; keep the fieldList entry naming the bare member 'operationType'.
Traces: endusermessaging-brand-profile-identifier-and-list-errors#task#7.

Related open issues that do NOT own any of the four causes above (checked against the replay report's ownership marks): 20261116T224049Z-...conflict-validation-gates-inverted.md (owns the CREATE steps task#8/#10/#12 of the same fixture, not the reads in causes 1-2), 20261117T222227Z-endusermessaging-brand-profile-id-format-refusal-reports-a.md (the brandProfileId id-format gate, a different refusal from cause 3), 20261111T021035Z-...fieldlist-omitted-length-range.md (the modelled-constraint envelope), 20261117T003651Z-endusermessaging-brand-profile-attributes-8-net-new-causes.md (behavioral corpus; its cause 7 is the create-side media sniffing upstream of cause 1).

## Evidence
Replay of wire corpus simparity-wire-aws-v1 against the booted emulator: 113 steps, 67 passed, 46 mismatched, score 59.3%, emulator_up=true, bootstrap_signal=false. 21 divergence clusters; the four below carry an EMPTY owned_by and an EMPTY stub_path.

Cause 1 — fixture endusermessaging-brand-profile-attribute-create-validation step task#11, operation GetBrandProfileAttribute (expected 404, actual 200):
  status: expected 404, got 200
  $.message expected 'Resource not found - ResourceType="brand-profile-attribute"', got None
  $.resourceId expected 'fresh' (the attributeName the request addressed), got None
  $.resourceType expected 'brand-profile-attribute', got None
  $headers.x-amzn-errortype expected (shaped, not asserted), got None
  Fixed vs variable: 'Resource not found - ResourceType="brand-profile-attribute"' is the whole fixed sentence for this class (no ResourceId clause, the name being @sensitive); 'fresh' is this recording's request value, reported only in resourceId.

Cause 2 — same fixture, step task#13, operation GetBrandProfileAttribute (expected 200, actual 200):
  $.category expected 'OTHER', got None
  $.description expected 'a custom note', got None
  'OTHER' and 'a custom note' are this recording's create-request values; the requirement is that both modelled members are reported for an attribute created with them.

Cause 3 — fixture endusermessaging-brand-profile-attribute-update, step task#6, operation UpdateBrandProfileAttribute (expected 400, actual 400; cluster also covers task#7):
  $.fieldList[0].path expected 'attachmentBody', got '/attachmentBody'
  $.message and the error class matched. Source: src/ops/update_brand_profile_attribute.rs:272-295 passes "/attachmentBody" / "/attributeValue" as the fieldList path.

Cause 4 — fixture endusermessaging-brand-profile-identifier-and-list-errors, step task#7, operation ListJobs (expected 400, actual 400):
  $.message expected "Invalid value for 'operationType': unknown operation type: CreateBrandProfile. Valid values: CreateRegistrationsFromBrandProfile | UpdateRegistrationsFromBrandProfile | CreateBrandProfileFromRegistration | UpdateBrandProfileFromRegistration"
  $.message actual   "Invalid operationType 'CreateBrandProfile'. Supported values: CreateBrandProfileFromRegistration, CreateRegistrationsFromBrandProfile, UpdateBrandProfileFromRegistration, UpdateRegistrationsFromBrandProfile"
  'CreateBrandProfile' is this recording's rejected query value; the clause text, the ' | ' separator and the order of the four values are the requirement. Source: src/state/jobs.rs:260-272.

## Steps to Reproduce
1. git_clone the repo, build and boot the emulator from the checkout.
2. Replay the wire corpus simparity-wire-aws-v1 and watch the four steps named in trace_ids / Issue-Step.
3. Cause 1: CreateBrandProfile, then CreateBrandProfileAttributes with a batch whose element 'terms' carries attachment bytes of an unsupported media type alongside a valid element named 'fresh' — AWS refuses the whole batch (400 ValidationException) and creates nothing; the emulator answers 200. Then GET /v1/brand-profiles/{id}/attributes/fresh: the emulator answers 200 with the phantom attribute; AWS answers 404 ResourceNotFoundException, message 'Resource not found - ResourceType="brand-profile-attribute"', resourceType 'brand-profile-attribute', resourceId 'fresh'.
4. Cause 2: with the batch properly refused, CreateBrandProfileAttributes [{attributeName:'fresh', attributeType:'TEXT', attributeValue:..., category:'OTHER', description:'a custom note'}, {attributeName:'logo', attributeType:'IMAGE'}] -> 200, then GET the 'fresh' attribute: AWS returns category 'OTHER' and description 'a custom note'; the emulator (serving the stale record, its create having been refused 409) returns neither.
5. Cause 3: PUT /v1/brand-profiles/{id}/attributes/{a TEXT attribute} with attachmentBody -> 400 ValidationException with the correct message '<member> cannot be provided for TEXT attribute', but fieldList[0].path '/attachmentBody'; AWS reports path 'attachmentBody'. Repeat with attributeValue on an IMAGE/DOCUMENT attribute -> expected path 'attributeValue', emulator '/attributeValue'.
6. Cause 4: GET /v1/jobs?operationType=CreateBrandProfile -> 400 ValidationException with message "Invalid operationType 'CreateBrandProfile'. Supported values: ..." (comma-separated, alphabetical); AWS answers "Invalid value for 'operationType': unknown operation type: CreateBrandProfile. Valid values: CreateRegistrationsFromBrandProfile | UpdateRegistrationsFromBrandProfile | CreateBrandProfileFromRegistration | UpdateBrandProfileFromRegistration".
