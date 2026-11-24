Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 455b877557af92974b1e1b0549e81b06a05b022f

# A turn reports the request attributes the whole session accumulated, not "the attributes sent in the request" — and the sibling PutSession applies the opposite rule to the same member

_Filed: 2026-11-24T22:22:23Z_
_Operation: RecognizeUtterance | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `455b877557af92974b1e1b0549e81b06a05b022f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`RecognizeUtteranceResponse$requestAttributes` (the `x-amz-lex-request-attributes` response header) and `RecognizeTextResponse$requestAttributes` are both documented as "The attributes sent in the request." The handlers instead report the session's ACCUMULATED attribute map: `open_turn` merges whatever the request carried into the stored `Session::request_attributes` (documented in the state module as "Request attributes accumulated over the session"), and `record_turn` hands that whole stored map back as `Turn::request_attributes`, which `RecognizeUtterance` encodes into the header and `RecognizeText` returns as the body member.

So a turn that sends NO `requestAttributes` is answered with the attributes some EARLIER turn sent, and a turn that sends `{"turn":"2"}` is answered with `{"channel":"web","turn":"2"}` — neither of which is "the attributes sent in the request". A caller cannot read back what it just sent, and cannot distinguish "this request carried no attributes" from "this request carried the ones a previous request left".

The crate is also inconsistent with itself on the very same member: `PutSession` REPLACES the stored map (`session.request_attributes = input.request_attributes.unwrap_or_default();`), so it reports exactly what its own request carried — including wiping the map to `{}` when the request omits the member — while the two turn operations merge. One member, two opposite rules in one crate, with no doc or model text distinguishing them (unlike `sessionState.sessionAttributes`, whose own model doc describes a map carried across the turns of a conversation, which is why `merge_attributes` is right there and not here).

The handler comment at `recognize_text.rs:295-299` quotes the model sentence and then states the deviation in the same breath ("as the session has accumulated them"), so the code itself records that it does not do what the sentence says.

## Evidence

Smithy, `smithy_query(query="doc", shape="RecognizeUtteranceResponse", member="requestAttributes")`: "The attributes sent in the request." (plus the gzip/base64 note). The same query for `RecognizeTextResponse` member `requestAttributes`: "The attributes sent in the request."

The handler reports the accumulated map, not the request's —
src/ops/recognize_utterance.rs:525-527:
  `let request_attributes =`
  `    encode_map_member(turn_schema, "requestAttributes", &turn.request_attributes)`
  `        .map_err(internal)?;`
src/ops/recognize_text.rs:295-300 quotes the sentence and states the deviation:
  "// \"The attributes sent in the request\" (`requestAttributes`), as the
   // session has accumulated them: the map is reported on every turn,
   // empty for a conversation nothing has sent an attribute on, so a
   // client threading attributes across turns can read it without
   // first testing whether the member is there."
  `request_attributes: Some(turn.request_attributes),`

What that value is — src/state.rs:400-402 (in `open_turn`):
  `if let Some(supplied) = supplied_attributes {`
  `    merge_attributes(&mut session.request_attributes, supplied);`
  `}`
src/state.rs:450 (in `record_turn`, building the `Turn`): `request_attributes: session.request_attributes.clone(),`
src/state.rs:549-550: `/// Request attributes accumulated over the session.` / `pub request_attributes: HashMap<String, String>,`
src/state.rs:765-769: `pub fn merge_attributes(stored: &mut HashMap<String, String>, supplied: HashMap<String, String>) { for (key, value) in supplied { stored.insert(key, value); } }`

The sibling operation applies the OPPOSITE rule to the same member — src/ops/put_session.rs:355:
  `session.request_attributes = input.request_attributes.unwrap_or_default();`

Test coverage (both pin the accumulation, and the reason each states is the sentence this contradicts):
- tests/all/always_reported_members.rs:409-413 row `("a second turn that adds to the first", &[json!({"channel": "web"}), json!({"turn": "2"})], json!({"channel": "web", "turn": "2"}))`, asserted at :446-451 "the turn must report the attributes the session accumulated"; the suite's stated rule 3 at tests/all/always_reported_members.rs:20-21 is only about the member always being present: "A turn carries `requestAttributes`, \"the attributes sent in the request\": the empty map for a conversation that accumulated none."
- tests/all/structured_collection_headers.rs:327-336: "// The turn carried no attributes of its own, so what it reports is the map the session already held" with `let stored_attributes = case.request_attributes.clone().unwrap_or_else(|| json!({}));` compared against the turn's `x-amz-lex-request-attributes`.

## Steps to Reproduce

1. Build a bot locale through the Lex Models V2 control plane mounted on the same engine (as tests/all/harness.rs `built_bot` does); note botId, botAliasId and localeId.
2. POST /bots/{botId}/botAliases/{botAliasId}/botLocales/{localeId}/sessions/s1/utterance with `Content-Type: text/plain; charset=utf-8`, header `x-amz-lex-session-state` omitted, header `x-amz-lex-request-attributes` = gzip-then-base64 of {"channel":"web"}, body `I want to order a pizza`. Observe 200 and `x-amz-lex-request-attributes` decoding to {"channel":"web"}.
3. POST the same /utterance path again with NO `x-amz-lex-request-attributes` header at all and body `thin`. Observe 200 and `x-amz-lex-request-attributes` still decoding to {"channel":"web"} — although the model says the member is "The attributes sent in the request" and this request sent none.
4. POST the same /utterance path a third time with `x-amz-lex-request-attributes` = gzip-then-base64 of {"turn":"3"}. Observe the header decoding to {"channel":"web","turn":"3"} rather than the {"turn":"3"} the request sent.
5. Repeat steps 2-4 against the sibling .../sessions/s1/text path with JSON bodies ({"text":"...","requestAttributes":{...}}): the `requestAttributes` body member of the second turn is {"channel":"web"} for a request that carried none.
6. Contrast the sibling operation on the same session: POST .../sessions/s1 (PutSession) with {"sessionState":{"dialogAction":{"type":"Delegate"}}} and no `requestAttributes`. Its own `x-amz-lex-request-attributes` is absent (the stored map was replaced with the empty map by put_session.rs:355), and the next turn then reports {} — the same member governed by the opposite rule inside one crate.
