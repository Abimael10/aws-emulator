Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: be68e03a3b164dfd55295662738d8b5c0a02662c

# The 'optional structured header: absent when empty, else the encoded collection' rule is implemented twice, once in PutSession and once in RecognizeUtterance

_Filed: 2026-11-21T02:56:07Z_
_Operation: PutSession | Class: maintainability_

> _Note: this issue was identified against the code base at commit `be68e03a3b164dfd55295662738d8b5c0a02662c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`crate::state::headers` exists so that the wire form of the structured `x-amz-lex-*` headers lives in one place, and it owns the gzip+base64 step. But the per-member rule around it — look the member schema off the operation's schema, answer `None` when the stored collection is empty, else write the list with its element schema (or the map with its `key()`/`member()` schemas) and encode — is written out twice, in `put_session.rs` and in `recognize_utterance.rs`, down to the same `internal("the model declares no … element member")` strings. The two copies can drift: a change to how an empty collection is reported, or to the map key/value schema lookup, has to be made in both handlers or the two operations start reporting the same `messages`/`requestAttributes` differently. Extracting the shared encode-a-list-member / encode-a-map-member helpers into `state::headers` (whose own module doc already claims that role) leaves every response byte and every existing test unchanged.

## Evidence

Copy 1 — src/ops/put_session.rs:391-443:
```
let messages_schema = member_schema(input_schema, "messages").ok_or_else(|| internal("the model declares no messages member"))?;
...
let messages = if stored_messages.is_empty() { None } else {
    let element_schema = messages_schema.member().ok_or_else(|| internal("the model declares no messages element member"))?;
    Some(header_value(|serializer| { serializer.write_list(messages_schema, &|element| { for message in &stored_messages { element.write_struct(element_schema, message)?; } Ok(()) }) })?)
};
let request_attributes = if stored_attributes.is_empty() { None } else {
    let key_schema = attributes_schema.key().ok_or_else(|| internal("the model declares no requestAttributes key member"))?;
    let value_schema = attributes_schema.member().ok_or_else(|| internal("the model declares no requestAttributes value member"))?; ...
```
Copy 2 — src/ops/recognize_utterance.rs:522-597:
```
let messages_schema = member_schema(turn_schema, "messages").ok_or_else(|| internal("the model declares no messages member"))?;
...
let messages = if turn.messages.is_empty() { None } else {
    let element_schema = messages_schema.member().ok_or_else(|| internal("the model declares no messages element member"))?;
    Some(encode_json(|serializer| { serializer.write_list(messages_schema, &|element| { for message in &turn.messages { element.write_struct(element_schema, message)?; } Ok(()) }) }).map_err(internal)?)
};
let request_attributes = if turn.request_attributes.is_empty() { None } else {
    let key_schema = attributes_schema.key().ok_or_else(|| internal("the model declares no requestAttributes key member"))?; ...
```
The module that is supposed to own this — src/state/headers.rs:3-5 and :23-24: "The wire form of this service's structured `x-amz-lex-*` HTTP headers, read and written in ONE place so the request side and the response side of the runtime cannot disagree about it." / "The functions below are that rule; nothing else in this crate encodes or decodes a header payload."
Tests that pin the current responses and must stay green unchanged: tests/all/bot_addressing.rs:613-639 (PutSession `x-amz-lex-messages` contents and the GetSession read-back), tests/all/recognize_utterance.rs (the turn's encoded headers), tests/all/always_reported_members.rs:391-420 (the reported request attributes), src/state/headers.rs:216-321 (the encoding's own round-trip and refusal tests).

## Steps to Reproduce

1. Read src/ops/put_session.rs:391-443 and src/ops/recognize_utterance.rs:522-597 side by side: the empty-collection branch, the member/key/value schema lookups and the error strings are the same logic written twice.
2. Read src/state/headers.rs:3-24, which states the header wire form is owned in one place.
3. Change the empty-collection rule or a schema lookup in one handler only: the two operations then report `messages`/`requestAttributes` differently for the same stored session, with no test forcing the other copy to follow.
