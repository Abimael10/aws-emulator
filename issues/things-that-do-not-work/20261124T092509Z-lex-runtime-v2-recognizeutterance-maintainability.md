Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: cb1793cf7c5a4b5a69e7dca7c8a291b52f4b88d5

# The responseContentType rule (default to the text type, then refuse speech with one sentence) is written out twice, once in PutSession and once in RecognizeUtterance

_Filed: 2026-11-24T09:25:09Z_
_Operation: RecognizeUtterance | Class: maintainability_

> _Note: this issue was identified against the code base at commit `cb1793cf7c5a4b5a69e7dca7c8a291b52f4b88d5`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

Both operations that take a `responseContentType` implement the same three-step rule in their own handler: default an absent value to `TEXT_CONTENT_TYPE`, test the result with `crate::state::asks_for_speech`, and refuse a speech request with the modelled `DependencyFailedException` carrying a byte-identical sentence. Only the predicate is shared; the defaulting and the refusal sentence are duplicated, so the two copies can drift — and the crate's own rationale for sharing the predicate says they must not.

The drift is not hypothetical: the already-open `responseContentType` validation gap (an unlisted value such as `application/json` is served and echoed) has to be fixed in BOTH copies, because each handler re-decides on its own what an absent or non-audio value means.

A fix is a single helper beside `asks_for_speech` in `crate::state` that resolves `Option<String>` to either the content type to report or the one refusal sentence, with each handler lifting that sentence into its own modelled error variant (the error enums are per-operation, which is the only part that genuinely differs). Every response byte and every existing test stays the same.

## Evidence

Copy 1 — src/ops/put_session.rs:263-281:
  `// The content type the response is reported with: "the same as the type specified in the responseContentType field in the request".`
  `let content_type = input.response_content_type.unwrap_or_else(|| TEXT_CONTENT_TYPE.to_string());`
  `if asks_for_speech(&content_type) { return Err(PutSessionErrorModeled::DependencyFailedException(DependencyFailedException { message: format!("responseContentType {content_type} requests synthesized speech, which this emulator cannot produce; ask for {TEXT_CONTENT_TYPE}") }).into()); }`

Copy 2 — src/ops/recognize_utterance.rs:410-421:
  `let content_type = input.response_content_type.unwrap_or_else(|| TEXT_CONTENT_TYPE.to_string());`
  `if asks_for_speech(&content_type) { return Err(speech_unavailable(format!("responseContentType {content_type} requests synthesized speech, which this emulator cannot produce; ask for {TEXT_CONTENT_TYPE}"))); }`
(and src/ops/recognize_utterance.rs:302-307 wraps the sentence in `DependencyFailedException`, as copy 1 does inline.)

The crate already states that this rule belongs in one place — src/state.rs:65-77:
  "/// Held here rather than in either operation, because the two operations that take a `responseContentType` must answer the same way for the same value"
  `pub fn asks_for_speech(response_content_type: &str) -> bool { ... }`
and src/ops/put_session.rs:111-113 repeats it: "`asks_for_speech` and `TEXT_CONTENT_TYPE` are `crate::state`'s, because the two operations that take a `responseContentType` must answer the same way for the same value."

Tests that cover both copies and must stay unchanged: tests/all/session_lifecycle.rs:875-920 (`an_audio_response_content_type_is_refused`: `no content type asked for` -> 200, `text`/`text, spelled with a space` -> 200 with the content type echoed, `audio/mpeg`/`audio/ogg`/`audio/pcm`/`AUDIO/PCM` -> 424) for PutSession, and tests/all/recognize_utterance.rs:612-629 (`a text turn asking for a spoken answer`, `a text turn asking for pcm speech` -> 424 `DependencyFailedException`) for RecognizeUtterance.

## Steps to Reproduce

1. Read src/ops/put_session.rs:263-281 and src/ops/recognize_utterance.rs:410-421 side by side: the same default-then-test-then-refuse rule and the same refusal sentence appear in both.
2. Note src/state.rs:65-77, which centralises only the predicate and states that the two operations "must answer the same way for the same value".
3. Consider the fix the open issue 20261123T234016Z-lex-runtime-v2-recognizeutterance-validation asks for (refuse a `responseContentType` outside the documented values): it has to be applied to both copies, or the two operations start answering the same value differently.
4. Run `cargo test -p service-com-amazonaws-lexruntimev2-awsdeepsenseruntimeserviceapi2-0` — the two suites named above pin the current responses on both paths and must keep passing byte-for-byte after the rule is stated once.
