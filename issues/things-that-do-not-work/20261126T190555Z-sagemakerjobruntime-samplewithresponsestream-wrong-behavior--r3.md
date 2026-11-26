Issue-Severity: LOW
Issue-Gap-Class: wrong-behavior
Issue-Commit: 14ec769a5321afdf37a20bd54803749025ed3b5a

# A stream chunk that cannot be framed is dropped and the truncated body is served as 200 instead of reported

_Filed: 2026-11-26T19:05:55Z_
_Operation: SampleWithResponseStream | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `14ec769a5321afdf37a20bd54803749025ed3b5a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`crate::state::sample_stream_response_body` — the body builder `SampleWithResponseStream` renders its `@httpPayload` stream with — swallows a framing failure. The `Err` arm of the `frame_payload_event` match only emits a `tracing::warn!` and the `for` loop continues, so the chunk is silently omitted from `body` and the function returns the remaining frames. The handler then answers `200` with that body AND `append_turn` captures it as the turn's `response_body`.

Two things are wrong with that:

1. The comment sitting directly on the `Err` arm states the opposite outcome: it says the frame is "traced rather than served half-written", but serving the accumulated `body` with one chunk missing is exactly a half-written stream. A client that concatenates the `delta` members (the reconstruction `stream_chunk_documents` is documented to guarantee: "A client that concatenates the deltas reconstructs exactly the text `completion_envelope` reports in one document") gets text with a token missing, and the `usage` totals in the final chunk then describe text the stream did not deliver. The stored turn records the same corrupted bytes, so the state is wrong too.
2. The crate already has the path to report this honestly: both sampling handlers construct their own modelled `InternalServiceError` for the other "this emulator could not serve the call" condition, and `InternalServiceError (HTTP 500)` is a modelled error of `SampleWithResponseStream`. The framing path is the one failure that does not take it; it converts an error into a success.

The comment itself argues the arm is unreachable for the documents this crate renders (each a few hundred bytes), which is why this is LOW: there is no behavioural impact today. The defect is that the error branch's code does not do what the comment above it says, and that if it is ever reached it reports success for a body it knows is incomplete. No test in the crate exercises this arm (`tests/all/trajectory_lifecycle.rs:400 the_streamed_answer_is_a_series_of_payload_part_events` only drives the success path, asserting `expected_events` frames are all present).

## Evidence

The comment and the code it sits on, `services/service-com-amazonaws-sagemakerjobruntime-agenticrftruntimeservice/src/state.rs:1211-1228`:

```
1211    let mut body = Vec::new();
1212    for document in stream_chunk_documents(job_arn, trajectory_id, index, prompt, created) {
1213        let payload = serde_json::to_vec(&document).unwrap_or_default();
1214        match ::service_common::event_stream::frame_payload_event(
1215            PAYLOAD_PART_EVENT,
1216            JSON_CONTENT_TYPE,
1217            payload,
1218        ) {
1219            Ok(frame) => body.extend_from_slice(&frame),
1220            // Unreachable for the documents above — each a few hundred bytes, far
1221            // inside the framing's own size limits — and a handler must not panic, so a
1222            // frame that could not be encoded is traced rather than served half-written.
1223            Err(error) => {
1224                ::tracing::warn!(%error, "a streamed inference chunk could not be framed");
1225            }
1226        }
1227    }
1228    body
```

The comment says "traced rather than served half-written"; the `Err` arm keeps the partially accumulated `body` (line 1228) and the loop goes on, so the body IS served half-written.

The invariant that body is supposed to hold, `state.rs:1133-1135`:

```
1133 /// `finish_reason` the completion ended for and the turn's `usage`. A client that
1134 /// concatenates the deltas reconstructs exactly the text [`completion_envelope`]
1135 /// reports in one document, so the two framings deliver the same answer.
```

The sibling path in the SAME handler that DOES report an emulator failure as a modelled error, `services/service-com-amazonaws-sagemakerjobruntime-agenticrftruntimeservice/src/ops/sample_with_response_stream.rs:170-177`:

```
170        let Some(state) = crate::state::service_state(&*self.engine) else {
171            return Err(SampleWithResponseStreamErrorModeled::InternalServiceError(
172                InternalServiceError {
173                    message: "the AgenticRFT runtime emulator has no state store".to_string(),
174                },
175            )
176            .into());
177        };
```

`InternalServiceError` is modelled for this operation — `OPERATIONS.md:46`: "**Errors:** `AccessDeniedException (HTTP 403)`, `InternalServiceError (HTTP 500)`, …" — and the handler's `respond` closure (`sample_with_response_stream.rs:205-216`) returns `(Vec<u8>, String)` with no channel for the failure, so the dropped frame cannot be surfaced.

Test coverage: `tests/all/trajectory_lifecycle.rs:399-477` (`the_streamed_answer_is_a_series_of_payload_part_events`) asserts the full frame count on the success path only; nothing covers the `Err` arm.

## Steps to Reproduce

1. Read `services/.../src/state.rs:1204-1229` (`sample_stream_response_body`).
2. Observe that the `Err(error)` arm at line 1223 only logs and the `for` loop at line 1212 continues, so the frame is omitted from `body` and the function returns the remaining frames at line 1228.
3. Compare with the comment on lines 1220-1222, which states such a frame is "traced rather than served half-written" — omitting one frame from a multi-frame stream is precisely a half-written body.
4. Follow the return value to `src/ops/sample_with_response_stream.rs:205-216`: the closure hands it to `serve_sampling_turn`, which captures it as the turn's `response_body` and the handler answers `200` with it at lines 224-227 — a success for a stream the code knows is incomplete.
5. Contrast with lines 170-177 of the same handler, which report a different emulator failure through the modelled `InternalServiceError` the operation declares (`OPERATIONS.md:46`).
