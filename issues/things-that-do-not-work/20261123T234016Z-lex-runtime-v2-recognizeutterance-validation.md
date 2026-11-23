Issue-Severity: MEDIUM
Issue-Gap-Class: validation
Issue-Commit: 144bcacdff36cb6d5f74daf1c12856f62206ef70

# A `responseContentType` outside the model's accepted values is served and echoed back as `contentType` instead of being refused

_Filed: 2026-11-23T23:40:16Z_
_Operation: RecognizeUtterance | Class: validation_

> _Note: this issue was identified against the code base at commit `144bcacdff36cb6d5f74daf1c12856f62206ef70`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`RecognizeUtteranceRequest$responseContentType` enumerates every value the operation accepts (`audio/mpeg`, `audio/ogg`, `audio/pcm`, `audio/*`, `text/plain; charset=utf-8`). The handler only asks whether the value begins with `audio/` (`asks_for_speech`), refusing those with `DependencyFailedException`; everything else falls through as if it were the text value, the turn is taken and persisted, and the unlisted value is echoed back verbatim in the response's `contentType` header. So `Response-Content-Type: application/json` (or `video/mp4`, `text/html`, `banana`) is answered 200 with `Content-Type: application/json` over a gzip/base64 Lex payload.

The same handler already enforces exactly this kind of rule for the sibling member on the request side: a `requestContentType` that is none of the documented prefixes is refused with the modelled `ValidationException` naming the accepted prefixes. The crate's own constant also states the rule it then does not enforce — `TEXT_CONTENT_TYPE` is documented as "the one value of `responseContentType` that is not speech", citing the member doc "which lists every accepted value" — yet the code treats EVERY non-`audio/` value as that one text value.

`asks_for_speech` is shared with `PutSession` (state.rs: "the two operations that take a `responseContentType` must answer the same way for the same value"), so `PutSession` has the identical hole through the same helper.

## Evidence

Smithy, `smithy_query(query="doc", shape="RecognizeUtteranceRequest", member="responseContentType")`: "The following are the accepted values:" — "audio/mpeg", "audio/ogg", "audio/pcm (16 KHz)", "audio/* (defaults to mpeg)", "text/plain; charset=utf-8".

The crate states the same rule — src/state.rs:42-47:
  "/// The response content type a caller gets when it asks for none, and the one
   /// value of `responseContentType` that is not speech: \"if the value is
   /// `text/plain;charset=utf-8`, Amazon Lex V2 returns text in the response\"
   /// (`RecognizeUtteranceRequest$responseContentType`, which lists every accepted
   /// value)."
  `pub const TEXT_CONTENT_TYPE: &str = "text/plain;charset=utf-8";`

The code only tests the audio prefix — src/ops/recognize_utterance.rs:413-421:
  `let content_type = input.response_content_type.unwrap_or_else(|| TEXT_CONTENT_TYPE.to_string());`
  `if asks_for_speech(&content_type) { return Err(speech_unavailable(...)); }`
and echoes whatever arrived — src/ops/recognize_utterance.rs:532: `content_type: Some(content_type),`
while `asks_for_speech` (src/state.rs:72-77) is only `response_content_type.trim_start().to_ascii_lowercase().starts_with(AUDIO_PREFIX)`.

The sibling member in the SAME handler IS checked against its documented list — src/ops/recognize_utterance.rs:400-407:
  `None => { return Err(invalid(format!("requestContentType must start with one of the prefixes Amazon Lex V2 accepts (audio/l16, audio/x-l16, audio/lpcm, audio/x-cbr-opus-with-preamble or {TEXT_REQUEST_PREFIX}), got {}", input.request_content_type))); }`

The same hole in the sibling operation — src/ops/put_session.rs:265-281 performs only the `asks_for_speech` test on `responseContentType`.

Test coverage: tests/all/session_lifecycle.rs:884-894 tables only the listed values ("The accepted values come from `RecognizeUtterance$responseContentType`: \"audio/mpeg audio/ogg audio/pcm\" and \"text/plain;charset=utf-8\"") and tests/all/dialog_turn_reporting.rs:609-628 only text spellings — no row sends a value outside the documented list, so nothing pins the current acceptance.

## Steps to Reproduce

1. Build a bot locale through the Lex Models V2 control plane mounted on the same engine (as tests/all/harness.rs `built_bot` does).
2. POST /bots/{botId}/botAliases/{botAliasId}/botLocales/en_US/sessions/s1/utterance with headers `Content-Type: text/plain; charset=utf-8` and `Response-Content-Type: application/json`, body `hello`.
3. Observe HTTP 200 with response header `Content-Type: application/json`, although `application/json` is none of the values `RecognizeUtteranceRequest$responseContentType` lists as accepted; the turn was taken and persisted.
4. Repeat with `Response-Content-Type: video/mp4` (or any non-`audio/` string) — same 200 and the same verbatim echo.
5. Equivalently POST the PutSession path with header `ResponseContentType: application/json` — also 200 with that value echoed as `Content-Type`.
