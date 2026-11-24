Issue-Severity: MEDIUM
Issue-Gap-Class: error-response
Issue-Commit: 2af414c51a998bccb0ceedc9ab36026ce1caaf60

# Any `audio/`-prefixed requestContentType is treated as a listed audio format, so an unlisted one gets 424 instead of the ValidationException the handler itself describes

_Filed: 2026-11-24T00:02:09Z_
_Operation: RecognizeUtterance | Class: error-response_

> _Note: this issue was identified against the code base at commit `2af414c51a998bccb0ceedc9ab36026ce1caaf60`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`input_format` classifies `requestContentType` by two prefixes only: `text/plain` (Text) and `audio/` (Speech). The model enumerates the accepted prefixes exhaustively, and the four audio ones are `audio/l16`, `audio/x-l16`, `audio/lpcm` and `audio/x-cbr-opus-with-preamble` — `audio/` is not one of them. `AUDIO_PREFIX` is deliberately the loose superset of the RESPONSE content types (state.rs:49-52 names `audio/mpeg`, `audio/ogg`, `audio/pcm`, `audio/*`), and reusing it on the request side means a value the model does not list — e.g. `requestContentType: audio/mpeg` — never reaches the `None` branch that refuses an unlisted prefix. It is classified as speech and answered with `DependencyFailedException` (424, "declares audio input, which this emulator cannot transcribe"), i.e. the emulator reports a missing dependency for a request format the service never accepted. The handler's own ValidationException message contradicts its check: it tells the caller the accepted prefixes are exactly the four audio ones plus `text/plain`, a set the code never actually enforces.

## Evidence

Smithy doc, `smithy_query(query="doc", shape="RecognizeUtteranceRequest", member="requestContentType")`: "Indicates the format for audio input or that the content is text. The header must start with one of the following prefixes:" … "audio/l16; rate=16000; channels=1", "audio/x-l16; sample-rate=16000; channel-count=1", "audio/lpcm; sample-rate=8000; sample-size-bits=16; channel-count=1; is-big-endian=false", "audio/x-cbr-opus-with-preamble;preamble-size=0;bit-rate=256000;frame-size-milliseconds=4", "text/plain; charset=utf-8".

The check, `src/ops/recognize_utterance.rs:209-218`:
```
fn input_format(content_type: &str) -> Option<InputFormat> {
    let normalized = content_type.trim_start().to_ascii_lowercase();
    if normalized.starts_with(TEXT_REQUEST_PREFIX) { Some(InputFormat::Text) }
    else if normalized.starts_with(AUDIO_PREFIX) { Some(InputFormat::Speech) }
    else { None }
}
```
with `AUDIO_PREFIX` defined in `src/state.rs:49-52` as the RESPONSE-side superset: "The prefix every speech `responseContentType` carries: all of the model's speech values (`audio/mpeg`, `audio/ogg`, `audio/pcm`, `audio/*`) begin with it" — `src/state.rs:52`: `pub const AUDIO_PREFIX: &str = "audio/";`.

The handler's own refusal message, `src/ops/recognize_utterance.rs:400-407`, states the set the check does not enforce: "requestContentType must start with one of the prefixes Amazon Lex V2 accepts (audio/l16, audio/x-l16, audio/lpcm, audio/x-cbr-opus-with-preamble or {TEXT_REQUEST_PREFIX}), got {}" — unreachable for any `audio/…` value, which is diverted at line 393-399 to `speech_unavailable(..)` = `DependencyFailedException` (424).

The tests only cover the two ends of the rule (`tests/all/recognize_utterance.rs:26-29`: "speech with `DependencyFailedException` (424 …), and a malformed request with `ValidationException` (400)"); no row pins `audio/mpeg` or any other unlisted audio prefix, so nothing in the crate justifies the loose request-side check.

## Steps to Reproduce

1. Create and build a bot locale through the mounted Lex Models V2 control plane
2. POST /bots/{botId}/botAliases/{aliasId}/botLocales/en_US/sessions/sess-001/utterance with header `Content-Type: audio/mpeg` and any body
3. Observe HTTP 424 DependencyFailedException ("declares audio input, which this emulator cannot transcribe") although `audio/mpeg` is not one of the prefixes `requestContentType` documents, so the handler's own enumerated ValidationException (400) is what the value is owed
4. Repeat with `Content-Type: audio/anything-at-all` — same 424; the `None` branch at recognize_utterance.rs:400 is unreachable for every `audio/`-prefixed value
