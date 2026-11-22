Issue-Severity: MEDIUM
Issue-Gap-Class: error-response
Issue-Commit: f4ffe9b86bfc5a913ab49c40dcefce09fac2412b

# A 400 raised by the pre-handler constraint walk is answered with no audit record, so a refused call is unattributable — unlike the identical refusal the handler raises

_Filed: 2026-11-22T22:10:16Z_
_Operation: UpdateConnectionRecordingPreferences | Class: error-response_

> _Note: this issue was identified against the code base at commit `f4ffe9b86bfc5a913ab49c40dcefce09fac2412b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

This crate words a boundary (pre-handler) constraint refusal itself, in `service.rs::override_refusal`, but that path emits NO `emulator::audit` record. Every refusal the HANDLERS raise does emit one (`AuditOutcome::ValidationRefused`, `AccessDenied`, `NotFound`, `StoreUnavailable`), and `crate::state::audit`'s own doc states the purpose that every branch calls it for: "so a reader of the emulator's log can attribute each call".

The result is that two refusals of the SAME class — HTTP 400 `ValidationException`, worded by the SAME renderer (`crate::state::constraint_message` / `invalid_value_sentence`), with the same one-sentence body — are treated differently by the crate's own audit rule depending only on which layer happened to refuse:

* `S3Bucket$BucketName` outside its `@pattern` is refused inside the Update handler (its look-around pattern does not compile in the framework's regex), and audits `outcome=validation-refused`;
* `S3Bucket$BucketOwner` outside `^[0-9]{12}$`, `KMSKeyArn` outside `@length(1,2048)`, `S3Buckets` outside `@length(1,1)`, an absent `@required` member, and `ClientToken` outside `@length(1,64)` (on Update and on Delete) are all refused by the boundary walk before the handler runs — and leave NO record at all.

So a caller can drive an unbounded number of refused, un-attributable calls at this service while the crate's test suite claims "every call leaves a structured, attributable access record". The crate already owns this refusal path (it rewords the message there), so wiring the same audit call into it is not new machinery; nothing about the status, code or body need change.

## Evidence

The rule, in this crate — services/service-com-amazonaws-ssmguiconnect-ssmguiconnect/src/state.rs:654-658:
    `/// Emit the structured access record for one call to this service.`
    `///`
    `/// Every operation calls this on every branch it can return from, so a reader of the`
    `/// emulator's log can attribute each call — above all the two state-changing ones,`
and the outcome the closed set declares for exactly this response — src/state.rs:723-724:
    `/// The request was refused as invalid (400).`
    `ValidationRefused,`

The side that honours it — src/ops/update_connection_recording_preferences.rs:173-174:
    `if let Some(message) = crate::state::refuse_preferences(&input.connection_recording_preferences) {`
    `    audit(Self::NAME, ctx, AuditOutcome::ValidationRefused);`

The side that breaks it — src/service.rs:163-169, the only other place this crate answers a request, and it calls no `audit`:
    `match refusal {`
    `    Refusal::Invalid(violations) => {`
    `        crate::state::constraint_message(violations).map(|message| Refused {`
    `            code: "ValidationException".to_string(),`
    `            message,`
    `        })`
    `    }`
(`grep 'audit('` over src/ finds call sites only in state.rs:675 and the three handlers — none in service.rs.)

Proof these 400s never reach a handler, from the crate itself — src/ops/delete_connection_recording_preferences.rs:172-174:
    `// modeled \`@length(1, 64)\` was already enforced by the framework's boundary`
    `// walk before this handler ran, so the value here is inside that bound.`
and src/ops/update_connection_recording_preferences.rs:166-168:
    `// Modeled constraints the boundary validator cannot compile (see`
    `// \`crate::state::refuse_preferences\`); everything else it declares was`
    `// already refused before this handler ran.`

Proof the two @pattern refusals split across the two layers — tests/all/pattern_validation_message.rs:18-21:
    `//! are refused by two different LAYERS, which is the other thing these rows span: the`
    `//! boundary constraint walk refuses \`BucketOwner\`, whose pattern compiles, while`
    `//! \`BucketName\`'s look-around pattern does not compile there and is checked by the handler`
    `//! — and both must answer the one sentence.`

Test coverage, and the gap in it: tests/all/connection_recording_preferences.rs:1299-1301 `// Rule 4 — every call leaves a structured, attributable access record`, asserted by `async fn every_call_emits_a_structured_audit_record()` (:1393). Its only refusal row is the HANDLER-checked one — :1451-1458 `("the refusal", "UpdateConnectionRecordingPreferences", ... preferences("123456789012", "Not-A-Bucket", KEY_ARN) ... "validation-refused")`. No row covers a boundary-refused call, and the boundary refusals that ARE pinned on the wire (:1047 `an_input_outside_a_modeled_constraint_is_refused`, :1225 `a_delete_client_token_outside_its_length_is_refused`) assert only status and `__type`, so the missing record is unpinned and no comment or test states a reason for the asymmetry.

## Steps to Reproduce

1. Install a tracing subscriber capturing the `emulator::audit` target, as tests/all/connection_recording_preferences.rs:1375 `audit_capture()` does, and dispatch through the production `MultiServiceRouter` with this service registered.
2. Call POST /UpdateConnectionRecordingPreferences on an enrolled account with body {"ConnectionRecordingPreferences":{"RecordingDestinations":{"S3Buckets":[{"BucketOwner":"123456789012","BucketName":"Not-A-Bucket"}]},"KMSKeyArn":"arn:aws:kms:us-east-1:123456789012:key/abc"}}. Observe 400 ValidationException AND one `emulator::audit` record with `outcome=validation-refused` (src/ops/update_connection_recording_preferences.rs:174).
3. Repeat with the SAME body except BucketName="rdp-recordings" and BucketOwner="abcdefghijkl" (outside `^[0-9]{12}$`, a pattern the framework compiles). Observe the same 400 ValidationException, worded by the same renderer (src/service.rs:165) — and ZERO `emulator::audit` records.
4. Repeat with POST /DeleteConnectionRecordingPreferences and {"ClientToken":"ttt…"} (65 characters), the case tests/all/connection_recording_preferences.rs:1225 pins as 400 ValidationException. Observe again that no audit record is emitted, because the call never reaches the handler (src/ops/delete_connection_recording_preferences.rs:172-174).
5. grep for the audit call sites: `audit(` appears in state.rs:675 and the three handlers only; src/service.rs's `override_refusal` (:150-173) answers a request without one, contradicting state.rs:656-658.
