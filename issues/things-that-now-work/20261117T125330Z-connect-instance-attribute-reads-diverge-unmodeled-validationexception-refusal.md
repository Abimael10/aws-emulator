Issue-Source: trace-sweep-divergence
Issue-Trace: connect-instance-attribute-unknown-instance-and-invalid-input
Issue-Step: task#5
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: connect-instance-attribute-unknown-instance-and-invalid-input
Issue-Shape-Id: com.amazonaws.connect#AmazonConnectService
Issue-Key: connect|DescribeInstanceAttribute|4xx->4xx|$.Message
Issue-Key: connect|DescribeInstanceAttribute|4xx->4xx|$headers.x-amzn-errortype
Issue-Key: connect|ListInstanceAttributes|2xx->4xx|$.Attributes
Issue-Key: connect|ListInstanceAttributes|2xx->4xx|$.NextToken
Issue-Key: connect|ListInstanceAttributes|2xx->4xx|status
Issue-Commit: cea05121548b3a16dc11942b4ef340d67b81162b

# connect instance-attribute reads diverge: unmodeled ValidationException refusal and no ListInstanceAttributes page

> _Note: this issue was identified against the code base at commit `cea05121548b3a16dc11942b4ef340d67b81162b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeInstanceAttribute

## Description
Two net-new causes on the Amazon Connect instance-attribute read path. Both are reads against an instance that EXISTS: one refusal is worded and typed as a generic framework validation failure instead of the Connect error AWS raises, and the sibling listing operation never serves a page at all. (Separately, four more divergences in this corpus — DescribeInstance, CreateQueue, DescribeQueue, DeleteQueue 404 Message text — are already owned by open issue 20261117T032509Z-connect-resourcenotfoundexception-message-is-emulator-authored-instance-prose.md and are NOT part of this issue.)

Cause 1 of 2: DescribeInstanceAttribute refuses an AttributeType outside the modeled enum with a generic ValidationException instead of Connect's InvalidParameterException.
AWS: Smithy (services/service-com-amazonaws-connect-amazonconnectservice/smithy-model) models DescribeInstanceAttribute with input members InstanceId (@httpLabel, required, @length 1..100) and AttributeType (@httpLabel, required, enum InstanceAttributeType), bound to GET /instance/{InstanceId}/attribute/{AttributeType}. The errors it declares are InternalServiceException (500), InvalidParameterException (400, "One or more of the specified parameters are not valid", single member Message), InvalidRequestException (400), ResourceNotFoundException (404) and ThrottlingException (429). ValidationException is NOT a modeled error of this operation or of this service. The recording shows AWS answering HTTP 400 with body member Message exactly 'Invalid AttributeType' — that string is entirely FIXED text: AWS does not interpolate the rejected value, the member name or the permitted enum set into it. The x-amzn-errortype header must therefore name one of the operation's modeled 400 errors (InvalidParameterException for a bad parameter value).
Emulator: answers HTTP 400 but with x-amzn-errortype 'ValidationException' and Message "1 validation error detected: Value 'NOT_AN_ATTRIBUTE' at 'attributeType' failed to satisfy constraint: Member must satisfy enum value set: [INBOUND_CALLS, OUTBOUND_CALLS, CONTACTFLOW_LOGS, CONTACT_LENS, AUTO_RESOLVE_BEST_VOICES, USE_CUSTOM_TTS_VOICES, EARLY_MEDIA, MULTI_PARTY_CONFERENCE, AUTO_MUTE_AGENT_ON_HOLD, HIGH_VOLUME_OUTBOUND, ENHANCED_CONTACT_MONITORING, ENHANCED_CHAT_MONITORING, MULTI_PARTY_CHAT_CONFERENCE, MESSAGE_STREAMING]" — i.e. the centralized pre-handler enum check's generic template (whose variable parts here are the rejected value and the member name), not Connect's wording. Status matches; error code and message text do not.
Why: a client that catches Connect's modeled InvalidParameterException (the SDK's typed exception for this operation) sees an unknown error type instead and falls through to its generic/unmodeled-error branch, so the refusal is unhandled or retried; code or tests that match the documented 'Invalid AttributeType' text never match, and the emulator leaks its own validation vocabulary (member name 'attributeType', the full enum list) that AWS never returns.
Fix: the AttributeType enum rejection for this operation must not be left to the framework's generic enum validator — DescribeInstanceAttribute's handler/validation must map an unrecognized AttributeType onto the modeled InvalidParameterException with the fixed Message 'Invalid AttributeType' and the matching x-amzn-errortype.
Traces: connect-instance-attribute-unknown-instance-and-invalid-input#task#5.

Cause 2 of 2: ListInstanceAttributes serves no attribute page for an existing instance — it answers 400 instead of 200.
AWS: Smithy models ListInstanceAttributes as GET /instance/{InstanceId}/attributes with input InstanceId (@httpLabel, required, @length 1..100), NextToken (@httpQuery nextToken) and MaxResults (@httpQuery maxResults, MaxResult7, @range 1..7), output Attributes (list of Attribute{AttributeType, Value}) plus NextToken, @paginated (items Attributes, pageSize MaxResults). Modeled errors are the same five as above; success status 200. The recording shows AWS answering 200 with a populated Attributes list of AttributeType/Value feature toggles for the instance and a NextToken for the following page. Recorded behavior only, worth noting for the fix: the recorded list contains attribute types the model's InstanceAttributeType enum does NOT declare (AUTOMATED_INTERACTION_LOG, BOT_MANAGEMENT, ENABLE_BOT_ANALYTICS_AND_TRANSCRIPTS alongside modeled ones such as AUTO_MUTE_AGENT_ON_HOLD, AUTO_RESOLVE_BEST_VOICES, CONTACTFLOW_LOGS, CONTACT_LENS, EARLY_MEDIA), with 'true'/'false' string values — so the served set is the instance's full toggle set, not just the modeled enum, and the page is capped by MaxResults with NextToken set when more remain.
Emulator: answers HTTP 400 with no Attributes and no NextToken (both reported missing). The operation is still an unimplemented scaffold — its handler returns the NotImplemented scaffold error and the file carries the 'SCAFFOLDED, NOT IMPLEMENTED' marker — so nothing ever produces a page; the observed 400 means the request is refused by the centralized pre-handler validation before `handle` even runs. Hypothesis: the refusal is the generic validation of a bound query parameter (e.g. maxResults against MaxResult7's @range 1..7) rather than anything Connect raises for this request, since AWS answered the same request 200.
Why: list-then-use is broken for instance attributes — a client enumerating an instance's feature toggles gets a hard client error on a request AWS satisfies, so it cannot discover which features are enabled and has no page cursor to continue with; it also cannot distinguish 'this instance has no attributes' from 'the request was bad'.
Fix: implement ListInstanceAttributes' handler and the backing instance-attribute state so an existing instance returns 200 with the instance's AttributeType/Value pairs, honouring MaxResults/NextToken pagination, and reserve 400 for requests Connect actually refuses (ResourceNotFoundException 404 for an unknown instance) instead of letting generic request validation refuse a well-formed listing.
Traces: connect-instance-attribute-unknown-instance-and-invalid-input#task#6.

## Evidence
Replay of trace set simparity-wire-aws-v1 (score 52.0%, 25 steps, 13 passed, 12 mismatched).

Cause 1 — fixture connect-instance-attribute-unknown-instance-and-invalid-input, step task#5, operation DescribeInstanceAttribute, verdict mismatch, expected status 400 / actual 400:
  $.Message: value_mismatch — expected 'Invalid AttributeType', got "1 validation error detected: Value 'NOT_AN_ATTRIBUTE' at 'attributeType' failed to satisfy constraint: Member must satisfy enum value set: [INBOUND_CALLS, OUTBOUND_CALLS, CONTACTFLOW_LOGS, CONTACT_LENS, AUTO_RESOLVE_BEST_VOICES, USE_CUSTOM_TTS_VOICES, EARLY_MEDIA, MULTI_PARTY_CONFERENCE, AUTO_MUTE_AGENT_ON_HOLD, HIGH_VOLUME_OUTBOUND, ENHANCED_CONTACT_MONITORING, ENHANCED_CHAT_MONITORING, MULTI_PARTY_CHAT_CONFERENCE, MESSAGE_STREAMING]"
  $headers.x-amzn-errortype: shape_mismatch — got 'ValidationException', which is not an error DescribeInstanceAttribute models (expected a modeled 400 error, InvalidParameterException).
  Fixed text of AWS's message: the whole string 'Invalid AttributeType'; it has NO variable parts (the rejected value, member name and enum set the emulator appends are not part of AWS's message).

Cause 2 — same fixture, step task#6, operation ListInstanceAttributes, verdict mismatch, expected status 200 / actual 400:
  status: expected 200, got 400
  $.Attributes: missing — AWS returned a list of AttributeType/Value toggles (recorded page: AUTOMATED_INTERACTION_LOG=false, AUTO_MUTE_AGENT_ON_HOLD=false, AUTO_RESOLVE_BEST_VOICES=true, BOT_MANAGEMENT=false, CONTACTFLOW_LOGS=false, CONTACT_LENS=true, EARLY_MEDIA=true, ENABLE_BOT_ANALYTICS_AND_TRANSCRIPTS=false); emulator returned None.
  $.NextToken: missing — AWS returned a continuation token (value not asserted); emulator returned None.
  services/service-com-amazonaws-connect-amazonconnectservice/src/ops/list_instance_attributes.rs lines 4 and 168: the module still carries '//! SCAFFOLDED, NOT IMPLEMENTED' and `handle` returns `Err(crate::scaffolded::Scaffolded::not_implemented("ListInstanceAttributes"))`.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay trace set simparity-wire-aws-v1.
2. Cause 1: replay fixture connect-instance-attribute-unknown-instance-and-invalid-input step task#5 — GET /instance/{InstanceId}/attribute/NOT_AN_ATTRIBUTE for the instance the fixture created, i.e. DescribeInstanceAttribute with an AttributeType outside InstanceAttributeType. Observe HTTP 400 with x-amzn-errortype: ValidationException and the '1 validation error detected: ...' Message, instead of a modeled 400 InvalidParameterException whose Message is exactly 'Invalid AttributeType'.
3. Cause 2: replay the same fixture's step task#6 — GET /instance/{InstanceId}/attributes (ListInstanceAttributes) for that same existing instance. Observe HTTP 400 with neither Attributes nor NextToken in the body, instead of HTTP 200 carrying the instance's AttributeType/Value toggle list and a NextToken.
