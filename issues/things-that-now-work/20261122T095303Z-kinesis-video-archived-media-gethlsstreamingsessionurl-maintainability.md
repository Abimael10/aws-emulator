Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: 68c71f6585a425304635c9d8c1cc1d9408beb2ea

# The playback-mode selector rule and the session parameter defaults are implemented twice, once per streaming-session handler

_Filed: 2026-11-22T09:53:03Z_
_Operation: GetHLSStreamingSessionURL | Class: maintainability_

> _Note: this issue was identified against the code base at commit `68c71f6585a425304635c9d8c1cc1d9408beb2ea`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The two streaming-session handlers of this crate implement the SAME rule twice, line for line: the playback-mode default, the `Expires`/max-fragments defaults (LIVE vs ON_DEMAND), the `LIVE` must-not-carry-a-TimestampRange refusal, the non-LIVE StartTimestamp requirement, the ON_DEMAND EndTimestamp requirement, the end-later-than-start refusal, and the `SessionWindow` built from those bounds. The copies differ only in the member name quoted in the refusal text (`HLSFragmentSelector` vs `DASHFragmentSelector`) and in the per-protocol members each input additionally carries.

The crate already treats the other halves of this operation pair as ONE rule held in `state.rs` — `session_fragment_numbers` is documented as "The one selection rule both of them use" and `session_url` as "The one rule both streaming-session operations use, so they cannot drift on which endpoint a session URL names". The mode/selector rule is the remaining copy, and it has already drifted in one direction at least once: the GetHLS/GetDASH comment asserting that "the `TimestampRange` of ListFragments and GetClip ... accept a single-instant range" is false for GetClip (src/ops/get_clip.rs:398 refuses `end_millis <= start_millis`). Extracting the rule once (parameterised by the selector member name so every refusal sentence stays byte-identical) leaves all responses and tests unchanged.

## Evidence

Copy 1 — src/ops/get_hls_streaming_session_url.rs:389-427:
```
        let range = selector.and_then(|s| s.timestamp_range.as_ref());
        let live = playback_mode == "LIVE";
        if live {
            if range.is_some() {
                return Err(invalid(
                    "HLSFragmentSelector.TimestampRange must not be set when PlaybackMode is LIVE"
                        .to_string(),
                ));
            }
        } else {
            let Some(range) = range else {
                return Err(invalid(format!(
                    "HLSFragmentSelector with a TimestampRange is required when PlaybackMode is \
                     {playback_mode}"
                )));
            };
```
Copy 2 — src/ops/get_dash_streaming_session_url.rs:375-413:
```
        let range = selector.and_then(|s| s.timestamp_range.as_ref());
        let live = playback_mode == "LIVE";
        if live {
            if range.is_some() {
                return Err(invalid(
                    "DASHFragmentSelector.TimestampRange must not be set when PlaybackMode is LIVE"
                        .to_string(),
                ));
            }
        } else {
            let Some(range) = range else {
                return Err(invalid(format!(
                    "DASHFragmentSelector with a TimestampRange is required when PlaybackMode is \
                     {playback_mode}"
                )));
            };
```
(The `Expires`/max-fragments defaulting blocks are likewise duplicated: get_hls_streaming_session_url.rs:374-385 vs get_dash_streaming_session_url.rs:360-371, and the `SessionWindow` construction: 431-443 vs 417-429.)

The crate's own precedent for holding such a rule once — src/state.rs:416-419: "The one selection rule both of them use, so an HLS and an MPEG-DASH session over the same archive and the same parameters carry the same fragments." and src/state.rs:512-514: "The one rule both streaming-session operations use, so they cannot drift on which endpoint a session URL names."

Tests pinning both copies, which must keep passing unchanged: tests/all/archived_media_read_path.rs:1755 `get_hls_streaming_session_url_enforces_the_selector_rules_of_each_mode`, tests/all/archived_media_read_path.rs:1578 `get_hls_streaming_session_url_applies_the_documented_defaults`, tests/all/get_dash_streaming_session_url.rs:392 `the_selector_rules_of_each_mode_are_enforced`, tests/all/get_dash_streaming_session_url.rs:228 `the_documented_defaults_apply_and_a_stated_parameter_is_recorded`.

## Steps to Reproduce

1. Read src/ops/get_hls_streaming_session_url.rs:374-443 and src/ops/get_dash_streaming_session_url.rs:360-429 side by side: the defaulting, the per-mode selector refusals and the window construction are the same logic twice, differing only in the selector member name embedded in the messages.
2. Compare with src/state.rs:411-467 and src/state.rs:504-533, where the two other rules shared by the same pair of operations are each held exactly once.
3. Note the drift the duplication already produced: the comment at get_hls_streaming_session_url.rs:417-421 (repeated at get_dash_streaming_session_url.rs:403-407) claims GetClip accepts a single-instant range, while get_clip.rs:398 refuses `end_millis <= start_millis`.
