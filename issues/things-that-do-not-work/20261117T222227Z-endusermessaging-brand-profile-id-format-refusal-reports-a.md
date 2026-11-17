Issue-Source: trace-sweep-divergence
Issue-Trace: endusermessaging-brand-profile-identifier-and-list-errors
Issue-Step: task#3
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: endusermessaging-brand-profile-identifier-and-list-errors endusermessaging-brand-profile-lifecycle
Issue-Shape-Id: com.amazonaws.endusermessaging#EndUserMessaging
Issue-Key: endusermessaging|GetBrandProfile|4xx->4xx|$.fieldList[*].message
Issue-Key: endusermessaging|GetBrandProfile|4xx->4xx|$.fieldList[*].path
Issue-Commit: 952a95f6f44f35dec8503ef518e3b550df5d6797

# endusermessaging brand-profile id-format refusal reports a JSON-pointer path and a substituted fieldList sentence

> _Note: this issue was identified against the code base at commit `952a95f6f44f35dec8503ef518e3b550df5d6797`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetBrandProfile

## Description
ONE root cause, observed replaying the wire corpus simparity-wire-aws-v1 against the running (bootstrapped) emulator. The replay report shows this cluster unowned by any open issue. Two open issues touch the neighbourhood but do not cover it and carry no replay key for it: '20261104T183251Z-endusermessaging-input-validation-refusals-omit-fieldlist-and-diverge.md' cause 3 was about the id-format gate not FIRING at all (the emulator then answered 404 ResourceNotFoundException) — the gate now fires with the correct 400 ValidationException status and summary message, and what diverges is the fieldList entry it builds; and '20261111T021035Z-endusermessaging-brand-profile-validation-fieldlist-omitted-length-range.md' is about the MODELLED-constraint envelope (@length/@range on CreateBrandProfileAttributes and ListBrandProfiles), not about this service-level identifier gate. Neither issue's replay exercises this step, so it is filed here.

=== Cause 1 of 1: the brand-profile id-format ValidationException's fieldList carries a JSON-pointer path and a different per-field sentence than the refusal's own message ===
AWS: Smithy: GetBrandProfile.brandProfileId is an @httpLabel (@required) targeting BrandProfileIdOrArn (@length 1..256, @pattern ^[A-Za-z0-9_:/-]+$, documented as 'either the bare ID or the full Amazon Resource Name'); the operation declares ValidationException (HTTP 400, @error client) whose members are message (@required, 'A summary of the validation failure') and fieldList -> ValidationExceptionFieldList of ValidationExceptionField{path @required, message @required}. A value that satisfies the member's @pattern but is not a well-formed brand-profile identifier (neither a bare bp- id nor the ARN of one) is refused by the service's own id-format gate, before any store lookup, with 400 ValidationException. Recorded behavior: for this service-level gate the per-field entry REPEATS the summary sentence and names the member by its BARE name, i.e. message 'invalid brand profile ID format: <value>' and fieldList [{path: 'brandProfileId', message: 'invalid brand profile ID format: <value>'}]. Fixed text: 'invalid brand profile ID format: '; the only variable part is the identifier taken verbatim from the request (the request path segment here). The path is the bare member name 'brandProfileId' — unlike the modelled-constraint envelope of this same service, which uses a JSON pointer ('/maxResults', '/attributes'); the two envelopes are worded differently and this gate is the bare-name one.
Emulator: answers 400 with the correct summary message, but builds the fieldList entry with path '/brandProfileId' (a JSON pointer) and with a wholly different per-field message, the explanatory sentence 'The value must be a brand profile identifier: the bare ID, or the full Amazon Resource Name of the form arn:<partition>:end-user-messaging:<region>:<account-id>:brand-profile/<id>' — a sentence AWS does not emit, that neither repeats the summary nor echoes the rejected value.
Why: a client or console that keys off ValidationExceptionField.path to highlight the offending input member looks up 'brandProfileId' and finds nothing under that key, because the emulator hands back '/brandProfileId'; and a client that surfaces or matches the per-field message gets emulator-authored prose (including the emulator's own ARN template) instead of AWS's 'invalid brand profile ID format: <value>', so the rejected value never reaches the user and any assertion on the refusal's wording fails. Both halves of the machine-readable field entry are wrong even though the status, error class and summary message are right, which makes the divergence easy to miss and impossible to code against.
Fix: this service's brand-profile identifier refusal constructor — the single helper that renders the id-format ValidationException reached from every operation accepting a BrandProfileIdOrArn (GetBrandProfile's identifier resolution among them). It must emit the bare member name as path and repeat the 'invalid brand profile ID format: <value>' sentence as the per-field message, rather than a JSON pointer plus a substituted explanation.
Traces: endusermessaging-brand-profile-identifier-and-list-errors#task#3 (the cluster also covers endusermessaging-brand-profile-lifecycle#task#5). The constructor is services/service-com-amazonaws-endusermessaging-endusermessaging/src/state.rs:343-366 (BRAND_PROFILE_ID_POINTER = "/brandProfileId" and the substituted field message in invalid_brand_profile_id), reached from services/service-com-amazonaws-endusermessaging-endusermessaging/src/state.rs:252-262 (resolve_brand_profile_id) via services/service-com-amazonaws-endusermessaging-endusermessaging/src/ops/get_brand_profile.rs:174-184.

## Evidence
Fixture endusermessaging-brand-profile-identifier-and-list-errors step task#3 (GetBrandProfile, expected status 400, actual status 400 — status and error class agree; cluster also covers endusermessaging-brand-profile-lifecycle#task#5):
  $.fieldList[0].path expected: 'brandProfileId'
  $.fieldList[0].path actual:   '/brandProfileId'
  $.fieldList[0].message expected: 'invalid brand profile ID format: sim2real-probe-eum-ident-9a16e6bf'
  $.fieldList[0].message actual:   'The value must be a brand profile identifier: the bare ID, or the full Amazon Resource Name of the form arn:<partition>:end-user-messaging:<region>:<account-id>:brand-profile/<id>'
  $.message (the summary) matched.

Fixed text vs values: 'invalid brand profile ID format: ' is this refusal class's fixed text and 'brandProfileId' is the input member's name taken from the model; the trailing identifier is whatever value the request supplied — the string 'sim2real-probe-eum-ident-9a16e6bf' is this recording's request value and is not part of the requirement.

Source confirming the cause: services/service-com-amazonaws-endusermessaging-endusermessaging/src/state.rs:345 defines BRAND_PROFILE_ID_POINTER as "/brandProfileId" and state.rs:354-366 (invalid_brand_profile_id) sets field_list[0].path to it while setting field_list[0].message to the emulator-authored 'The value must be a brand profile identifier: ...' sentence, even though message on the same structure is already formatted as "invalid brand profile ID format: {id_or_arn}".

## Steps to Reproduce
1. GET /v1/brand-profiles/sim2real-probe-eum-ident-9a16e6bf — any value that satisfies BrandProfileIdOrArn's @pattern ^[A-Za-z0-9_:/-]+$ and @length 1..256 but is neither a bare bp- brand-profile id nor the ARN of one.
2. The emulator answers 400 ValidationException with the correct summary message 'invalid brand profile ID format: sim2real-probe-eum-ident-9a16e6bf' but with fieldList [{path: '/brandProfileId', message: 'The value must be a brand profile identifier: the bare ID, or the full Amazon Resource Name of the form arn:<partition>:end-user-messaging:<region>:<account-id>:brand-profile/<id>'}].
3. AWS answers 400 ValidationException with the same summary message and fieldList [{path: 'brandProfileId', message: 'invalid brand profile ID format: <the value from the request>'}] — the bare member name as path, and the summary sentence repeated as the per-field message.

Replay: wire corpus simparity-wire-aws-v1, fixture endusermessaging-brand-profile-identifier-and-list-errors#task#3 (same cluster: endusermessaging-brand-profile-lifecycle#task#5).
