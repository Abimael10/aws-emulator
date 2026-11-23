Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 76e71109e5ec3d0bf0db8980074fa780d3508a79

# The manifest snapshot keeps every fragment sharing a producer start timestamp, although PlaybackMode documents that only the larger fragment number is included

_Filed: 2026-11-23T23:10:18Z_
_Operation: GetDASHStreamingSessionURL | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `76e71109e5ec3d0bf0db8980074fa780d3508a79`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`GetDASHStreamingSessionURL` computes the manifest snapshot a session serves through the shared helper `ResolvedStream::session_fragment_numbers`, whose ON_DEMAND / LIVE_REPLAY branch is just `fragments_in_range(origin, start, end).take(max_fragments)` — every archived fragment whose selected timestamp falls in the window, in ingestion order.

The model states a collapse rule for exactly this computation: when `FragmentSelectorType` is `PRODUCER_TIMESTAMP` and several fragments share the same start timestamp, only the one with the larger fragment number belongs in the manifest and "The other fragments are not included". The handler implements no such collapse — grep for `dedup`/`duplicate` over the whole crate matches nothing outside the model JSON — so two fragments ingested with the same producer timestamp both land in the recorded snapshot. Two consequences: the session lists a fragment the model excludes, and because the cap is applied after the (uncollapsed) selection, `MaxManifestFragmentResults` is spent on the superseded fragment — with a cap of 1 the session serves the OLDER fragment number, the exact one the sentence says is not included.

The crate already has everything the rule needs: the archive is kept in ascending fragment-number order by `numeric_order`, and the helper already implements the other mode-specific selection rules quoted from the same documentation paragraphs (the LIVE "most recently ingested ... based on server timestamps" rule). `GetHLSStreamingSessionURL` carries the identical sentence in its own `PlaybackMode` doc and reaches the same helper, so one fix in `session_fragment_numbers` covers both (that operation's `wrong-behavior` slot is already held by the write-only-session issue, so it is not re-filed here).

## Evidence

The model sentence that states the behaviour — smithy_query(query="doc", shape="GetDASHStreamingSessionURLInput", member="PlaybackMode"):

  "In all playback modes, if `FragmentSelectorType` is `PRODUCER_TIMESTAMP`, and if there are multiple fragments with the same start timestamp, the fragment that has the larger fragment number (that is, the newer fragment) is included in the MPEG-DASH manifest. The other fragments are not included. Fragments that have different timestamps but have overlapping durations are still included in the MPEG-DASH manifest."

(The HLS twin is identical — smithy_query(query="doc", shape="GetHLSStreamingSessionURLInput", member="PlaybackMode"): "... the fragment that has the largest fragment number (that is, the newest fragment) is included in the HLS media playlist. The other fragments are not included.")

The handler takes the snapshot through the shared helper — services/service-com-amazonaws-kinesisvideoarchivedmedia-awsacuityreader/src/ops/get_dash_streaming_session_url.rs:472-479:
```
            // The manifest snapshot, by the one selection rule both
            // streaming-session operations share. A session with nothing to
            // serve does not exist.
            let selected =
                stream.session_fragment_numbers(window, origin, max_fragments, SystemTime::now());
```

and that helper's ON_DEMAND / LIVE_REPLAY branch collapses nothing — src/state.rs:455-465:
```
            // ON_DEMAND and LIVE_REPLAY serve the OLDEST fragments in the
            // requested range, up to the maximum.
            SessionWindow::Range {
                start_millis,
                end_millis,
            } => self
                .fragments_in_range(origin, start_millis, end_millis)
                .into_iter()
                .take(max_fragments)
                .map(|fragment| fragment.fragment_number.clone())
                .collect(),
```

with the range rule it delegates to returning every matching fragment — src/state.rs:344-357:
```
pub fn fragments_in_range(
    fragments: &[StoredFragment],
    origin: TimestampOrigin,
    start_millis: i64,
    end_millis: i64,
) -> Vec<&StoredFragment> {
    fragments
        .iter()
        .filter(|f| {
            let at = epoch_millis(origin.of(f));
            at >= start_millis && at <= end_millis
        })
        .collect()
}
```

The ordering the rule needs is already maintained by the store — src/state.rs:977-985: "/// Order two fragment numbers the way ingestion order does: numerically when both are decimal (the model's `^[0-9]+$`) ..." `fn numeric_order(a: &str, b: &str) -> std::cmp::Ordering`.

Test coverage: tests/all/get_dash_streaming_session_url.rs:88-109 (`a_minted_session_serves_the_fragments_its_mode_and_selector_select`) asserts the recorded `session.fragment_numbers` (read back at tests/all/get_dash_streaming_session_url.rs:38-51, 209) for ON_DEMAND / LIVE_REPLAY / LIVE and for caps of 2 and 5, but every seeded fragment has its own distinct timestamp — no test in tests/all seeds two fragments sharing one producer timestamp, so nothing pins the current behaviour.

## Steps to Reproduce

1. Register a stream in the Kinesis Video Streams control plane with DataRetentionInHours > 0, as tests/all/get_dash_streaming_session_url.rs does.
2. Append two fragments to this service's archive for that stream with DIFFERENT fragment numbers ("1" and "2") and the SAME producer_timestamp T0 (crate::state::append_fragment).
3. POST /getDASHStreamingSessionURL with {"StreamName": "<stream>", "PlaybackMode": "ON_DEMAND", "MaxManifestFragmentResults": 1, "DASHFragmentSelector": {"FragmentSelectorType": "PRODUCER_TIMESTAMP", "TimestampRange": {"StartTimestamp": T0, "EndTimestamp": T0 + 10}}}.
4. Read the recorded session out of the store by the token in the returned DASHStreamingSessionURL (state.sessions, as recorded_session() in the test does) and inspect fragment_numbers: it is ["1"] — the superseded, smaller fragment number — where the model says only the larger fragment number ("2") is included.
5. Repeat without MaxManifestFragmentResults: fragment_numbers is ["1", "2"], i.e. both fragments with the same start timestamp, where the model says "The other fragments are not included".
