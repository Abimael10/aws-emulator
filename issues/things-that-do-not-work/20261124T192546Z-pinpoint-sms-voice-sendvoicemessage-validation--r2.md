Issue-Severity: MEDIUM
Issue-Gap-Class: validation
Issue-Commit: 9353c1bb256472de141f0c55c99b9667dc6fcfc6

# A bound but blank Content Text is accepted and recorded, though its shape is the same NonEmptyString the handler refuses a blank OriginationPhoneNumber for

_Filed: 2026-11-24T19:25:46Z_
_Operation: SendVoiceMessage | Class: validation_

> _Note: this issue was identified against the code base at commit `9353c1bb256472de141f0c55c99b9667dc6fcfc6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

This crate states the form a value may take as a property of the SHAPE the member targets, held once per shape in `ops/types/member_form.rs`, and `is_non_empty_string` is the predicate for `NonEmptyString`: whitespace alone is "as empty as the empty string — the shape admits neither". `member_form::unusable_optional_member` is the crate's helper for exactly this case: an OPTIONAL member the request BOUND a value its shape does not admit becomes a `Unusable::Unreadable` finding, while an ABSENT member stays served.

Six members of this model target `NonEmptyString`: `CreateConfigurationSetEventDestinationRequest$EventDestinationName`, `SendVoiceMessageRequest$DestinationPhoneNumber`, `SendVoiceMessageRequest$OriginationPhoneNumber`, and the `Text` member of all three message bodies (`CallInstructionsMessageType$Text`, `PlainTextMessageType$Text`, `SSMLMessageType$Text`). `SendVoiceMessage`'s handler builds its findings vector for the first three only; nothing ever looks at the `Text` values. So in ONE handler, one member targeting `NonEmptyString` (`OriginationPhoneNumber: "   "`) is refused with `BadRequestException`, while another member targeting that same shape in the same request (`Content.PlainTextMessage.Text: "   "`, or `""`) is accepted with HTTP 200, a minted `MessageId`, and is stored verbatim — the handler records a message body the crate's own predicate says the shape cannot hold, and answers "sent" for a call with nothing to speak.

This is NOT the (now fixed) `SendVoiceMessage` validation gap: that one enumerated "the crate's three `NonEmptyString` members" (EventDestinationName, DestinationPhoneNumber, OriginationPhoneNumber) and added checks for those; the three `Content` `Text` members targeting the same shape were not counted and are still unchecked. The fix must keep an ABSENT `Text` served (the crate pins that at 200), which is precisely what `unusable_optional_member` already does — only a BOUND blank/empty value is the unusable one.

## Evidence

The rule, stated as a property of the shape — services/service-com-amazonaws-pinpointsmsvoice-pinpointsmsvoice/src/ops/types/member_form.rs:8-12:

    //! Two of this model's string shapes are named for the FORM their value takes rather than
    //! for the thing it names — `WordCharactersWithDelimiters` and `NonEmptyString` ... The form a shape
    //! admits belongs to THE SHAPE, so it is the same for every member targeting it, whichever
    //! operation that member belongs to: this module is the single home of one predicate per
    //! shape, so two operations cannot admit different values for one shape

and member_form.rs:51-57:

    /// Whether `value` is a non-empty string: the form `NonEmptyString` states.
    ///
    /// A value of whitespace alone carries no content, so it is as empty as the empty string —
    /// the shape admits neither.
    pub fn is_non_empty_string(value: &str) -> bool {
        !value.trim().is_empty()
    }

The helper that exists for exactly this member class — member_form.rs:59-66 + :77-84:

    /// Why the value a request bound to one OPTIONAL member cannot be served, or `None` when
    /// the member bound nothing at all or bound a value its shape (`admits`) accepts.
    /// ... The two cases are kept apart here so no handler reads a value it cannot use as though the request had named none.
    pub fn unusable_optional_member(value: Option<&str>, admits: fn(&str) -> bool) -> Option<super::refusal_wording::Unusable>

The handler applies it to two members and skips the three `Text` members — src/ops/send_voice_message.rs:180-207 (comment and findings vector):

    // There is no message without a recipient, and `DestinationPhoneNumber`'s shape is
    // `NonEmptyString`, so a blank or absent destination is a parameter this service's
    // validation cannot use. `OriginationPhoneNumber` targets that same shape but is
    // optional, so binding nothing is served (the request simply names no origination
    // number) while a blank value is refused like any other unusable one.
    ...
        let unusable: Vec<(&str, super::types::refusal_wording::Unusable)> = [
            super::types::member_form::unusable_optional_member(
                input.configuration_set_name.as_deref(),
                super::types::member_form::is_word_characters_with_delimiters,
            ) ...
            super::types::member_form::unusable_optional_member(
                input.origination_phone_number.as_deref(),
                super::types::member_form::is_non_empty_string,
            )
            .map(|why| ("OriginationPhoneNumber", why)),
        ]

The only thing done with `Content` is a count of bodies, then verbatim storage — send_voice_message.rs:228-231 and :282-297:

        let bodies = usize::from(content.plain_text_message.is_some())
            + usize::from(content.ssml_message.is_some())
            + usize::from(content.call_instructions_message.is_some());
    ...
            content: crate::state::VoiceContentRecord {
                call_instructions: content.call_instructions_message.map(|body| {
                    crate::state::CallInstructionsRecord { text: body.text }
                }),
                plain_text: content.plain_text_message.map(|body| {
                    crate::state::VoiceTextRecord { text: body.text, ... }

The shapes those `Text` members target — smithy_query(query="shape", shape="PlainTextMessageType,SSMLMessageType,CallInstructionsMessageType"): `"PlainTextMessageType": {"members": {"LanguageCode": {"target": "String"}, "Text": {"target": "NonEmptyString"}, "VoiceId": {"target": "String"}}}`, `"SSMLMessageType": {... "Text": {"target": "NonEmptyString"} ...}`, `"CallInstructionsMessageType": {"members": {"Text": {"target": "NonEmptyString"}}}` — and `"NonEmptyString": {"type": "string"}` with no `@length`/`@pattern`, so the boundary validator enforces nothing and this crate's predicate is the only statement of the form.

Test coverage: no suite sends a blank `Text` (grep for `"Text"` across tests/all finds only populated values: configuration_set_lifecycle.rs:943-955, voice_message_content_record.rs:179-231, parameter_refusal_wording.rs:403-422, send_voice_message_member_forms.rs:158). What IS pinned is the ABSENT case at 200 — voice_message_content_record.rs:186-192 (`"call instructions with no members at all"`, `json!({ "CallInstructionsMessage": {} })`), :212-218, :229-236 — which `unusable_optional_member` preserves (it answers `None` for an absent member). The blank-member rows of the predicate's own unit test already state the intended answer — member_form.rs:181-191: `("the non-empty form, whitespace alone", Some("   "), is_non_empty_string, Some(Unusable::Unreadable))`.

## Steps to Reproduce

1. Boot the emulator and grant the account the entitlement this API's resource-scoped actions need (as tests/all/voice_message_content_record.rs:52-55 does: set `legacy_surface_entitled = true`).
2. POST /v1/sms-voice/voice/message with {"DestinationPhoneNumber": "+12065550100", "Content": {"PlainTextMessage": {"Text": "   "}}} — observe HTTP 200 with a minted MessageId, and `sent_messages.last().content.plain_text.text == Some("   ")` in the store.
3. POST the same body with {"Text": ""} — observe HTTP 200 again, though member_form.rs:181-191 pins `Some("")`/whitespace for this shape as `Unusable::Unreadable`.
4. Compare, in the same handler and the same request shape: POST /v1/sms-voice/voice/message with {"DestinationPhoneNumber": "+12065550100", "OriginationPhoneNumber": "   ", "Content": {"PlainTextMessage": {"Text": "hello"}}} — observe HTTP 400 BadRequestException "The following parameter is null, empty or invalid: ['originationPhoneNumber']", the refusal every other member targeting `NonEmptyString` gets.
5. Confirm the absent case must stay 200: POST with {"Content": {"PlainTextMessage": {}}} — pinned at 200 by tests/all/voice_message_content_record.rs:212-218, and still 200 under `unusable_optional_member`, which refuses only a BOUND unusable value.
