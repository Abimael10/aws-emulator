Issue-Severity: MEDIUM
Issue-Gap-Class: validation
Issue-Commit: 68c71f6585a425304635c9d8c1cc1d9408beb2ea

# GetHLSStreamingSessionURL ignores the documented 24-hour bound on HLSTimestampRange$EndTimestamp while quoting the rest of the same sentence

_Filed: 2026-11-22T09:53:03Z_
_Operation: GetHLSStreamingSessionURL | Class: validation_

> _Note: this issue was identified against the code base at commit `68c71f6585a425304635c9d8c1cc1d9408beb2ea`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`HLSTimestampRange$EndTimestamp` documents, in one sentence, that the end "must be within 24 hours of the specified StartTimestamp, and it must be later than the StartTimestamp value". The handler quotes the second clause of that very sentence in its comment and enforces only it; the 24-hour span is never computed, so an `ON_DEMAND` or `LIVE_REPLAY` session over a range spanning months is minted and recorded.

The same crate enforces the analogous documented window for `GetImages` (`MAX_WINDOW_MILLIS`, src/ops/get_images.rs:167-170), and it enforces every other documented clause of this range (StartTimestamp required, EndTimestamp required for ON_DEMAND, end later than start). Nothing in the crate justifies skipping this one.

## Evidence

Model (smithy_query doc, shape=HLSTimestampRange, member=EndTimestamp): "The end of the timestamp range for the requested media. This value must be within 24 hours of the specified StartTimestamp, and it must be later than the StartTimestamp value."

The handler quotes that sentence and checks only its ordering half — src/ops/get_hls_streaming_session_url.rs:417-426:
```
            // STRICTER than the `TimestampRange` of ListFragments and GetClip,
            // which accept a single-instant range: `HLSTimestampRange`'s own
            // documentation says the end "must be later than the StartTimestamp
            // value", wording the other two ranges do not carry. The divergence
            // is the model's, not an inconsistency here.
            if let (Some(start), Some(end)) = (&range.start_timestamp, &range.end_timestamp) {
                if date_time_millis(end) <= date_time_millis(start) {
                    return Err(invalid(TIMESTAMP_RANGE_ORDER_MESSAGE.to_string()));
                }
            }
```
The window is then built from those unbounded bounds — src/ops/get_hls_streaming_session_url.rs:431-443 (`SessionWindow::Range { start_millis, end_millis }`), and no span check exists anywhere in the file.

## Steps to Reproduce

1. Create and activate a stream with non-zero retention and archive at least one fragment.
2. POST /getHLSStreamingSessionURL with `PlaybackMode: ON_DEMAND` and `HLSFragmentSelector.TimestampRange` = { StartTimestamp: t, EndTimestamp: t + 30 days }.
3. Expected per the modelled sentence: `InvalidArgumentException` (400) — the end is not within 24 hours of the start.
4. Actual: HTTP 200, a session is minted and recorded over the whole multi-month range.
