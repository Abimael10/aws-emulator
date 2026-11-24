Issue-Severity: MEDIUM
Issue-Gap-Class: validation
Issue-Commit: 2fdb0bca35dbf2816333afc14f0d0074e1e4571d

# SendVoiceMessage skips the name-form and NonEmptyString blankness checks its sibling paths apply, recording a ConfigurationSetName no configuration set could exist under

_Filed: 2026-11-15T11:08:50Z_
_Operation: SendVoiceMessage | Class: validation_

> _Note: this issue was identified against the code base at commit `2fdb0bca35dbf2816333afc14f0d0074e1e4571d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The crate states one rule for the shape `WordCharactersWithDelimiters` and one for `NonEmptyString`, applies both on other paths, and applies neither to SendVoiceMessage's correspondingly-targeted members.

1. `SendVoiceMessageRequest$ConfigurationSetName` targets the SAME shape (`WordCharactersWithDelimiters`) as `CreateConfigurationSetRequest$ConfigurationSetName`. `create_configuration_set.rs` derives a concrete admitted character set from that shape and refuses anything else with the modelled `BadRequestException` (and `tests/all/configuration_set_names.rs` pins exactly that list of refused names, including `"sim2real behav cfgset !!"`). `send_voice_message.rs` validates nothing about its `ConfigurationSetName` at all: it moves `input.configuration_set_name` straight into the stored record. The result is self-contradictory state — a `SendVoiceMessage` naming `"voice events!!"` is accepted (200, a MessageId minted) and that name is recorded as the set the call used, while `CreateConfigurationSet` refuses that same name with 400 so no such set can ever exist.

2. Inside the SAME handler, `DestinationPhoneNumber` is refused when blank with the reason "the member's shape is `NonEmptyString`", and `CreateConfigurationSetEventDestination` applies the identical trim-non-empty refusal to its own `NonEmptyString` member (`EventDestinationName`). `OriginationPhoneNumber`, also `NonEmptyString`, gets no check: a whitespace-only (or empty) origination number is accepted and stored verbatim. Two of the crate's three `NonEmptyString` members get the blankness refusal, this one does not.

## Evidence

Rule stated and applied for the name shape — services/service-com-amazonaws-pinpointsmsvoice-pinpointsmsvoice/src/ops/create_configuration_set.rs:80-88: "`CreateConfigurationSetRequest$ConfigurationSetName` targets `WordCharactersWithDelimiters` ... a named string shape carrying no `@pattern` — so the form it admits is the one its name states: word characters, separated by the delimiters a name is written with. These are those delimiters; everything else (a space, punctuation) is not part of a name."; create_configuration_set.rs:176-185: `let name = input.configuration_set_name.filter(|name| is_word_characters_with_delimiters(name)).ok_or_else(|| ... BadRequestException ...)`; helper at create_configuration_set.rs:245-250.

Same shape on the other operation — smithy_query(query="shape", shape="SendVoiceMessageRequest"): `"ConfigurationSetName": {"target": "WordCharactersWithDelimiters", "doc": "The name of the configuration set that you want to use to send the message."}` (and `"OriginationPhoneNumber": {"target": "NonEmptyString"}`, `"DestinationPhoneNumber": {"target": "NonEmptyString"}`).

Unchecked in the handler — src/ops/send_voice_message.rs:232-237:
```
store.sent_messages.push(crate::state::SentVoiceMessage {
    message_id: message_id.clone(),
    configuration_set_name: input.configuration_set_name,
    caller_id: input.caller_id,
    destination_phone_number: Some(destination),
    origination_phone_number: input.origination_phone_number,
```
no `is_word_characters_with_delimiters` and no trim check reach either member.

Blankness rule stated beside the member that is skipped — src/ops/send_voice_message.rs:169-182: "There is no message without a recipient, and the member's shape is `NonEmptyString`, so a blank or absent destination is a parameter this service's validation cannot use" ... `.filter(|number| !number.trim().is_empty())`. Sibling path applying the same rule to its own `NonEmptyString` member — src/ops/create_configuration_set_event_destination.rs:177-188: `let destination_name = input.event_destination_name.filter(|name| !name.trim().is_empty()).ok_or_else(...)`.

Test coverage: tests/all/configuration_set_names.rs:119-140 pins the refused-name table for `CreateConfigurationSet` (`("an inner space", "voice events")`, `("exclamation marks", "voice-events!!")`, `("an all-whitespace name", "   ")`); tests/all/parameter_refusal_wording.rs:320-333 covers only `SendVoiceMessage with no DestinationPhoneNumber` / `with a blank DestinationPhoneNumber` — no row sends a malformed `ConfigurationSetName` or a blank `OriginationPhoneNumber` to `SendVoiceMessage`, and tests/all/configuration_set_lifecycle.rs:966-972 only sends well-formed values.

## Steps to Reproduce

1. Boot the emulator and grant the account the entitlement the resource-scoped actions need (as tests/all/configuration_set_names.rs:51-54 does: set `legacy_surface_entitled = true`).
2. POST /v1/sms-voice/configuration-sets with {"ConfigurationSetName": "voice events!!"} — observe HTTP 400 BadRequestException, message "The following parameter is null, empty or invalid: ['configurationSetName']" (the name cannot exist).
3. POST /v1/sms-voice/voice/message with {"ConfigurationSetName": "voice events!!", "DestinationPhoneNumber": "+12065550100", "Content": {"PlainTextMessage": {"Text": "hi"}}} — observe HTTP 200 with a MessageId, and `sent_messages[0].configuration_set_name == Some("voice events!!")` in the store: the same string one handler calls unusable is accepted and recorded by the other.
4. POST the same body again with "OriginationPhoneNumber": "   " — observe HTTP 200 and `origination_phone_number == Some("   ")` stored, while a "   " DestinationPhoneNumber in the same request is refused 400 by the check two lines above it.
