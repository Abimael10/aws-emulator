Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 590a46ff404522650c3e609eb9b73b6bb68c4301

# A LIVE (or open-ended LIVE_REPLAY) session serves a playlist frozen at mint: newly ingested fragments never appear, although the playlist it writes advertises itself as continually updated

_Filed: 2026-11-25T04:18:24Z_
_Operation: GetHLSStreamingSessionURL | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `590a46ff404522650c3e609eb9b73b6bb68c4301`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

Both streaming-session handlers compute the set of fragments a session serves ONCE, at mint time, and record it on the session (`StreamingSession::fragment_numbers`). The playback plane then serves exactly that recorded list on every fetch: `resolved_session` walks `session.fragment_numbers` and only drops numbers the archive no longer holds — it never re-selects from the archive. So the media playlist / manifest a player re-fetches for the life of the session is byte-identical (modulo retention drops) to the one minted at session creation, whatever has been ingested since.

That contradicts the model for the two modes whose playlist is defined as a growing one:

* `PlaybackMode` LIVE (the DEFAULT this handler applies when the request names none): "the HLS media playlist is continually updated with the latest fragments as they become available. We recommend that the media player retrieve a new playlist on a one-second interval."
* `HLSTimestampRange$EndTimestamp` / `DASHTimestampRange$EndTimestamp`: "If the EndTimestamp is not set for LIVE_REPLAY mode then the session will continue to include newly ingested fragments until the session expires."

It also contradicts what this crate's own playback plane WRITES into the answer it serves: for LIVE / LIVE_REPLAY the media playlist is deliberately left open (no `#EXT-X-PLAYLIST-TYPE:VOD`, no `#EXT-X-ENDLIST`) and the MPEG-DASH manifest is typed `dynamic` precisely so "a player re-reads [it] for new fragments" — but every re-read is answered from the frozen snapshot, so the player is told to poll a resource that can never change.

`GetDASHStreamingSessionURL` has the identical gap through the same two shared sites (`ResolvedStream::session_fragment_numbers` at mint, `resolved_session` at playback); that operation's `wrong-behavior` slot is held by an open issue about the duplicate-producer-timestamp collapse rule, so it is not re-filed here. A fix in those two shared sites covers both operations.

## Evidence

The model sentences that state the behaviour:

* smithy_query(query="doc", shape="GetHLSStreamingSessionURLInput", member="PlaybackMode"): "<b><code>LIVE</code></b>: For sessions of this type, the HLS media playlist is continually updated with the latest fragments as they become available. We recommend that the media player retrieve a new playlist on a one-second interval." and "The default is <code>LIVE</code>."
* smithy_query(query="shape", shape="HLSTimestampRange") — member `EndTimestamp`: "The <code>EndTimestamp</code> value is required for <code>ON_DEMAND</code> mode, but optional for <code>LIVE_REPLAY</code> mode. If the <code>EndTimestamp</code> is not set for <code>LIVE_REPLAY</code> mode then the session will continue to include newly ingested fragments until the session expires."

The handler snapshots the fragment set at mint — services/service-com-amazonaws-kinesisvideoarchivedmedia-awsacuityreader/src/ops/get_hls_streaming_session_url.rs:486-496 and :505-518:
```
            // The playlist snapshot, by the one selection rule both
            // streaming-session operations share. A session with nothing to
            // serve does not exist.
            let selected =
                stream.session_fragment_numbers(window, origin, max_fragments, SystemTime::now());
...
        state.record_session(StreamingSession {
            token: token.clone(),
            stream_name,
...
            fragment_numbers,
```

The playback plane serves that recorded list and never re-selects — src/service/playback_session.rs:245-248:
```
    let played: Vec<(&crate::state::StoredFragment, PlayedFragment)> = session
        .fragment_numbers
        .iter()
        .filter_map(|number| stream.fragment(number))
```
with its own doc stating only retention drops are reflected (src/service/playback_session.rs:216-218): "the fragments of the session's recorded playlist the archive still holds — a fragment retention has since dropped is no longer served".

The other side of the inconsistency — the answer this crate writes tells the player the list grows:

* src/service/playback_manifest.rs:46-50 (the quote the module itself carries): "/// The `PlaybackMode` whose playlist is complete, so it is the only one closed with /// an end tag: \"The HLS media playlist is typically static for sessions with a /// PlaybackType of ON_DEMAND. The HLS media playlist is continually updated with /// new fragments for sessions with a PlaybackType of LIVE.\""
* src/service/playback_manifest.rs:126-128 and :159-161 — `#EXT-X-PLAYLIST-TYPE:VOD` and `#EXT-X-ENDLIST` are emitted only `if session.playback_mode == ON_DEMAND`.
* src/service/playback_manifest.rs:227-231 — "// A complete presentation is static; one a player re-reads for new // fragments is dynamic, which is what the two live modes serve." `presentation = match session.playback_mode == ON_DEMAND { true => "static", false => "dynamic" }`.

Test coverage: nothing pins the frozen behaviour. tests/all/archived_media_read_path.rs:1596-1679 ("mints a session per call and RECORDS it — its parameters, its expiry and the playlist snapshot it serves") and tests/all/get_dash_streaming_session_url.rs:79-215 assert the snapshot recorded AT MINT only; tests/all/archived_media_read_path.rs:2805 and :3049 assert the playlist/manifest text of a session whose archive is not appended to afterwards. A grep of tests/all for `append_fragment` (lines 258, 155/170/184, 234, 634, 652, 1294, 1439, 2086) shows every call happens during seeding, before any session is minted — no test ingests a fragment after minting a session.

## Steps to Reproduce

1. Register a stream in the Kinesis Video Streams control plane with DataRetentionInHours > 0 and ingest fragments 1..3 into this service's archive with server timestamps inside the last 30 seconds (as tests/all/archived_media_read_path.rs::seeded does).
2. POST /getHLSStreamingSessionURL with {"StreamName": "<stream>"} (no PlaybackMode, so the documented default LIVE applies) and keep the returned HLSStreamingSessionURL.
3. GET the media playlist of that session (follow the master playlist's EXT-X-STREAM-INF sibling): it lists fragments 1..3 and carries no #EXT-X-ENDLIST, i.e. it tells the player to poll for more.
4. Append fragment 4 to the stream's archive (crate::state::append_fragment) with a server timestamp of now.
5. GET the same media playlist URL again: it still lists only fragments 1..3 — the newly ingested fragment never appears, for the life of the session, although the model says a LIVE playlist "is continually updated with the latest fragments as they become available".
6. Repeat with PlaybackMode LIVE_REPLAY and an HLSFragmentSelector.TimestampRange carrying only StartTimestamp (no EndTimestamp): the session likewise never includes the fragment ingested after the mint, although EndTimestamp's own documentation says such a session "will continue to include newly ingested fragments until the session expires".
7. The same two steps against /getDASHStreamingSessionURL and its type="dynamic" manifest give the identical frozen <SegmentTimeline>.
