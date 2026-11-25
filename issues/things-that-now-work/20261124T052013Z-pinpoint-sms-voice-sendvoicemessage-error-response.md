Issue-Severity: LOW
Issue-Gap-Class: error-response
Issue-Commit: 25659208ec3649d48f969c288139b3a129410688

# SendVoiceMessage refuses on the first parameter the handler looked at, so a request that also fails on the earlier-bound Content is answered with the destinationPhoneNumber sentence alone

_Filed: 2026-11-24T05:20:13Z_
_Operation: SendVoiceMessage | Class: error-response_

> _Note: this issue was identified against the code base at commit `25659208ec3649d48f969c288139b3a129410688`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

This crate states ONE rule for how a request with several unusable parameters is refused: the handler reads ALL of its own validation's parameters first, records a finding for each, and words the single refusal in the order the OPERATION'S INPUT binds them — explicitly "not merely the first one a handler looked at". The two sibling handlers whose own validation reads more than one parameter (`ListConfigurationSets`: NextToken + PageSize; `CreateConfigurationSetEventDestination`: the `{ConfigurationSetName}` label + the body `EventDestinationName`) both do exactly that: they build a `Vec<(&str, Unusable)>` of findings before anything is refused.

`SendVoiceMessage` is the third handler whose own validation can refuse more than one parameter — `DestinationPhoneNumber` (blank/absent) and `Content` (not exactly one of the three message bodies) — and it is the one path that still refuses on whichever parameter it happened to look at first. It returns on `DestinationPhoneNumber` with `?` before it ever inspects `Content`. Because `SendVoiceMessageRequest` binds `Content` BEFORE `DestinationPhoneNumber` (the member order the sibling handlers' comments cite `smithy_query(query="operation",...)` for), a request that fails on both is answered with the sentence for the LATER-bound parameter, and the earlier-bound `Content` failure is never reported at all — the caller has to fix the number, resend, and only then learn that the message body was unusable too.

## Evidence

The rule, stated in this crate's own wording module — services/service-com-amazonaws-pinpointsmsvoice-pinpointsmsvoice/src/ops/types/refusal_wording.rs:22-26: "[`unusable_parameters`] is which of the two a request gets, so the layer split is decided in one place rather than per handler. It is handed a finding ([`Unusable`]) for EVERY parameter the request could not be served on before it words anything, because a sentence names every parameter the request failed on, not merely the first one a handler looked at — so a handler reads all of its own parameters first and words the refusal once." and refusal_wording.rs:105-111: "A request is refused ONCE, so the one sentence names EVERY parameter unusable in the layer that refused it ... Either way the names are listed in the order given, which is the order the input binds them — not the order a handler happened to look at them in, which is why a handler records a finding for every one of its own parameters before calling this."

Sibling handler that follows it — src/ops/list_configuration_sets.rs:174-180: "BOTH parameters this handler's own validation can reject are read before anything is refused, and a finding is recorded in the order the input binds them (`NextToken`, then `PageSize` — smithy_query(...)), so the ONE refusal reports every parameter this layer cannot use rather than whichever was looked at first." and src/ops/create_configuration_set_event_destination.rs:154-159: "BOTH parameters this handler's own validation can reject are read before anything is refused, and a finding is recorded in the order the input binds them (`ConfigurationSetName`, then `EventDestinationName` — smithy_query(...)), so the ONE refusal reports every parameter the refusing layer cannot use rather than whichever was looked at first." (the findings vector is built at create_configuration_set_event_destination.rs:171-184).

The handler that breaks it — src/ops/send_voice_message.rs:173-182 refuses and returns on the second-bound parameter:
```
        let destination = input
            .destination_phone_number
            .filter(|number| !number.trim().is_empty())
            .ok_or_else(|| {
                SendVoiceMessageErrorModeled::BadRequestException(BadRequestException {
                    message: Some(super::types::refusal_wording::invalid_parameters(&[
                        "DestinationPhoneNumber",
                    ])),
                })
            })?;
```
and only AFTER that return point does it look at the other parameter — send_voice_message.rs:189-200:
```
        let content = input.content.unwrap_or_default();
        let bodies = usize::from(content.plain_text_message.is_some())
            + usize::from(content.ssml_message.is_some())
            + usize::from(content.call_instructions_message.is_some());
        if bodies != 1 {
            return Err(SendVoiceMessageErrorModeled::BadRequestException(
                BadRequestException { message: Some(UNUSABLE_CONTENT_MESSAGE.to_string()) },
            )
            .into());
        }
```

The binding order the rule refers to — smithy_query(query="operation", operation="SendVoiceMessage"): `"input": {"CallerId": "String", "ConfigurationSetName": "WordCharactersWithDelimiters", "Content": "VoiceMessageContent", "DestinationPhoneNumber": "NonEmptyString", "OriginationPhoneNumber": "NonEmptyString"}` — `Content` is bound before `DestinationPhoneNumber`. Both refusals are the same error and status (the modelled `BadRequestException`, HTTP 400), and both are answered by this handler's own validation before any state is read.

Test coverage: tests/all/parameter_refusal_wording.rs:353-379 exercises each fault ALONE ("SendVoiceMessage with no DestinationPhoneNumber", "with a blank DestinationPhoneNumber", "whose Content carries no message body", "whose Content carries two message bodies"), never both in one request; the multi-parameter suite's own rule names only the other two operations — parameter_refusal_wording.rs:995-998: "`ListConfigurationSets` is where the two classes meet ... and `CreateConfigurationSetEventDestination` is where a path label meets a body member in that same layer", and its rows (parameter_refusal_wording.rs:965-986) cover only those two, which is why the divergence is not caught.

## Steps to Reproduce

1. Boot the emulator and grant the account the entitlement this API's resource-scoped actions need (as tests/all/voice_message_content_record.rs:52-55 does: set `legacy_surface_entitled = true`).
2. POST /v1/sms-voice/voice/message with a body that makes BOTH own-validation parameters unusable: {"DestinationPhoneNumber": " ", "Content": {}} (a blank number, and a Content carrying none of the three bodies).
3. Observe HTTP 400 BadRequestException with message "The following parameter is null, empty or invalid: ['destinationPhoneNumber']" — the `Content` failure, whose member the input binds FIRST, is not reported at all.
4. Fix only the number and resend ({"DestinationPhoneNumber": "+12065550100", "Content": {}}): the second call now answers "Message is invalid. The message must have either a plain text, SSML, or call instructions message", so one request's two faults took two round-trips.
5. Compare with the sibling path, which reports both faults of one request at once: POST /v1/sms-voice/configuration-sets/%20/event-destinations with {"EventDestinationName": "  "} answers "The following parameter is null, empty or invalid: ['configurationSetName', 'eventDestinationName']" (tests/all/parameter_refusal_wording.rs:965-971).
