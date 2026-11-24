Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: f74c2e237ecba449a4252c2c022ae2f28d9c08d0

# SERVER_TIMESTAMP/PRODUCER_TIMESTAMP start selector skips every fragment but the last when fragments share a timestamp

_Filed: 2026-11-23T22:46:52Z_
_Operation: GetMedia | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `f74c2e237ecba449a4252c2c022ae2f28d9c08d0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The timestamp start-selector arm resolves the cursor with `rposition` — the LAST fragment whose timestamp is `<= StartTimestamp` — instead of the FIRST one. When several fragments carry the same (millisecond-truncated) timestamp, a read whose `StartTimestamp` equals that timestamp starts at the newest of them and silently drops all the earlier fragments that carry exactly the requested instant, even though the model says a timestamp selector starts with "the chunk containing a fragment with the specified producer or server timestamp".

This is not a hypothetical collision: this crate's own ingest plane stamps EVERY fragment of one `/putMedia` session with ONE server instant (`let received_at = SystemTime::now();` taken once per request, then handed to each cluster as `server_timestamp: received_at`). So after a producer submits a container with N clusters, the stream's log holds N fragments with an identical server timestamp T, and `GetMedia` with `StartSelectorType=SERVER_TIMESTAMP, StartTimestamp=T` answers only fragment N — the N-1 fragments the caller asked for are missing from the payload, with a 200 and no continuation token that would recover them from the requested position. The same happens for `PRODUCER_TIMESTAMP` when two clusters carry equal timecodes, which the ingest path explicitly admits (only a timecode moving BACKWARDS is a fault: `if previous.is_some_and(|held| cluster.timecode < held)`).

The handler's own comment states the model the code is supposed to implement — "fragment i covers from its own timestamp until fragment i+1's" — which is exactly the assumption that breaks when fragment i and fragment i+1 share a timestamp: fragment i's span becomes empty and the code steps over it. Picking the first (rather than the last) fragment whose timestamp is `>= `/`<=` the requested instant is what makes the documented sentence true for both the distinct- and the equal-timestamp case.

## Evidence

Smithy, the sentence the arm implements — `smithy_query(query="doc", shape="StartSelector", member="StartSelectorType")`:
"PRODUCER_TIMESTAMP or SERVER_TIMESTAMP - Start with the chunk containing a fragment with the specified producer or server timestamp. You specify the timestamp by adding StartTimestamp."

The code that picks the LAST match instead of the first — services/service-com-amazonaws-kinesisvideomedia-awsacuityinletservice/src/ops/get_media.rs:662-681:
```
// "PRODUCER_TIMESTAMP or SERVER_TIMESTAMP - Start with the chunk
// CONTAINING a fragment with the specified timestamp": fragment i
// covers from its own timestamp until fragment i+1's, and the newest
// fragment covers everything after its own. A timestamp older than
// every fragment lands on the earliest one.
"PRODUCER_TIMESTAMP" | "SERVER_TIMESTAMP" => {
    let wanted = start_timestamp_millis.unwrap_or_default();
    let producer = selector_type == "PRODUCER_TIMESTAMP";
    fragments
        .iter()
        .rposition(|fragment| {
            let at = crate::state::epoch_millis(if producer {
                fragment.producer_timestamp
            } else {
                fragment.server_timestamp
            });
            at <= wanted
        })
        .unwrap_or(0)
}
```

The sibling path in THIS crate that makes equal timestamps the norm — src/service/put_media.rs:295-297:
```
// ONE instant dates everything this session persists: the server timestamp of
// every fragment it takes.
let received_at = SystemTime::now();
```
and src/service/put_media.rs:430-439, once per cluster:
```
let Some(fragment_number) = store.ingest_fragment(
    &*self.engine, name, arn,
    state::ProducedFragment {
        chunk: cluster.document,
        producer_timestamp: producer_instant,
        server_timestamp: received_at,
        duration_millis: cluster.span_millis,
    },
)
```
Equal producer timecodes are accepted by the same loop — src/service/put_media.rs:388: `if previous.is_some_and(|held| cluster.timecode < held) {` (only a STRICTLY smaller timecode is the `FRAGMENT_TIMECODE_LESSER_THAN_PREVIOUS` fault), so `producer_timestamp` can repeat too.

Test coverage: every timestamp row in tests/all/get_media.rs:499-521 seeds fragments with DISTINCT timestamps (`server_millis(index)` / `producer_millis(index)` per fragment, tests/all/get_media.rs:462-520), so no existing test pins the equal-timestamp case; `grep SERVER_TIMESTAMP tests/` finds no row where two fragments share an instant, and the ingest tests (tests/all/put_media_ingest.rs) read the multi-cluster result back with `FragmentSelectorType` on the archive plane, never with a `GetMedia` `SERVER_TIMESTAMP` selector.

## Steps to Reproduce

1. Boot the emulator; `CreateStream(StreamName="ts-stream")` and poll `DescribeStream` until ACTIVE.
2. `GetDataEndpoint(APIName="PUT_MEDIA", StreamName="ts-stream")` and POST /putMedia there with one Matroska container carrying THREE Clusters (increasing timecodes). All three fragments are persisted with the single `received_at` instant T (src/service/put_media.rs:297, :437).
3. Read the stream once with `GetMedia(StreamName="ts-stream", StartSelector={"StartSelectorType":"EARLIEST"})` and note the three chunks' `AWS_KINESISVIDEO_SERVER_TIMESTAMP` MKV tags: all three carry the same value T.
4. Now call `GetMedia(StreamName="ts-stream", StartSelector={"StartSelectorType":"SERVER_TIMESTAMP","StartTimestamp":T})` — the timestamp the response itself just reported for fragment 1.
5. Observe HTTP 200 with a payload containing only fragment 3: `rposition` at src/ops/get_media.rs:672-680 resolved the cursor to the LAST fragment whose server timestamp is `<= T`, so fragments 1 and 2 — the chunks that contain a fragment with the specified server timestamp — are never delivered.
6. Repeat with `PRODUCER_TIMESTAMP` and a container whose two Clusters carry the SAME timecode (accepted by src/service/put_media.rs:388) for the same loss on the producer-timestamp selector.
