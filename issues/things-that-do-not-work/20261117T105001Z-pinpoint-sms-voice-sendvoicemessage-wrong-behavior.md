Issue-Severity: LOW
Issue-Gap-Class: wrong-behavior
Issue-Commit: 77035eca171922ea8614ad2c6ab7d0c41b15dcc7

# SendVoiceMessage flattens CallInstructionsMessage to its Text, so the record cannot say which message body the caller supplied

_Filed: 2026-11-17T10:50:01Z_
_Operation: SendVoiceMessage | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `77035eca171922ea8614ad2c6ab7d0c41b15dcc7`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`SendVoiceMessage` records every accepted message in `state.sent_messages`, and the stored `VoiceContentRecord` documents itself as holding "whichever one of the three message bodies the caller supplied". Two of the three bodies are stored as `Option<VoiceTextRecord>`, so their PRESENCE survives even when the body's own `Text` member was omitted. The third, `CallInstructionsMessage`, is flattened into the single `call_instructions_text: Option<String>` with `.and_then(|body| body.text)`, so the presence of the body is lost whenever its `Text` is absent.

Concretely, a request whose `Content` is `{"CallInstructionsMessage": {}}` passes the handler's one-body check (`bodies == 1`, because `call_instructions_message.is_some()`), is answered 200 with a minted `MessageId`, and is stored with `call_instructions_text: None`, `plain_text: None`, `ssml: None` — a record identical to one that carried no body at all, which the handler refuses outright. The record therefore contradicts the one thing its own doc says it holds, and a later reader of the state (the only way this record is observable) cannot tell the call-instructions message apart from no content. The same input shaped as `{"PlainTextMessage": {}}` keeps `plain_text: Some(..)`, so the three bodies are handled inconsistently inside one expression.

## Evidence

The documented contract of the record — services/service-com-amazonaws-pinpointsmsvoice-pinpointsmsvoice/src/state.rs:141-146:
```
/// Stored form of `VoiceMessageContent`: whichever one of the three message bodies
/// the caller supplied.
#[derive(Debug, Clone, Default)]
pub struct VoiceContentRecord {
    /// `CallInstructionsMessageType$Text`.
    pub call_instructions_text: Option<String>,
```

The code under it — src/ops/send_voice_message.rs:238-253:
```
            content: crate::state::VoiceContentRecord {
                call_instructions_text: content
                    .call_instructions_message
                    .and_then(|body| body.text),
                plain_text: content.plain_text_message.map(|body| {
                    crate::state::VoiceTextRecord {
                        text: body.text,
...
                ssml: content.ssml_message.map(|body| crate::state::VoiceTextRecord {
```
`and_then(|body| body.text)` for one body against `map(..)` for the other two: presence is preserved for `PlainTextMessage`/`SSMLMessage` and discarded for `CallInstructionsMessage`.

The body is accepted as one body — src/ops/send_voice_message.rs:189-200:
```
        let content = input.content.unwrap_or_default();
        let bodies = usize::from(content.plain_text_message.is_some())
            + usize::from(content.ssml_message.is_some())
            + usize::from(content.call_instructions_message.is_some());
        if bodies != 1 {
```
and `CallInstructionsMessageType$Text` carries no `@required` (smithy_query query="shape" shape="CallInstructionsMessageType": `{"Text": {"target": "NonEmptyString", ...}}`), so `{"CallInstructionsMessage": {}}` is input the model admits and the handler reaches this line with.

Test coverage: tests/all/configuration_set_lifecycle.rs:950 only ever sends a populated body (`json!({ "CallInstructionsMessage": { "Text": "press 1" } })`), and tests/all/parameter_refusal_wording.rs:384-396 covers only zero bodies (`message_without_content`) and two bodies (`message_with_two_bodies`) — no test pins what is stored for a body with no `Text`.

## Steps to Reproduce

1. Boot the emulator and grant the account the entitlement the resource-scoped actions need (`legacy_surface_entitled = true`, as tests/all/configuration_set_lifecycle.rs:60-63 does).
2. POST /v1/sms-voice/voice/message with {"DestinationPhoneNumber": "+12065550100", "Content": {"CallInstructionsMessage": {}}} — observe HTTP 200 and a minted MessageId.
3. Read the store: `sent_messages.last().unwrap().content` has `call_instructions_text: None`, `plain_text: None`, `ssml: None` — the record states no body at all, though the request supplied one and the handler counted it.
4. Repeat with {"Content": {"PlainTextMessage": {}}} — observe the same 200 but `plain_text: Some(VoiceTextRecord { text: None, .. })`, i.e. presence preserved, showing the two paths disagree.
