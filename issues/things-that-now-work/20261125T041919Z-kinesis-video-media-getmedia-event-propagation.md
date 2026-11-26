Issue-Severity: MEDIUM
Issue-Gap-Class: event-propagation
Issue-Commit: 590a46ff404522650c3e609eb9b73b6bb68c4301

# A media log dropped by the store's log ceiling restarts the stream's fragment numbering at 1, so the ingest propagates a fragment number the archive read plane already holds for different media

_Filed: 2026-11-25T04:19:19Z_
_Operation: GetMedia | Class: event-propagation_

> _Note: this issue was identified against the code base at commit `590a46ff404522650c3e609eb9b73b6bb68c4301`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`log_mut` is the one creator of a stream's media log, and it is reached again after the store's log ceiling has dropped that stream's log (`drop_logs_beyond_the_ceiling`). The stream itself still exists in the control-plane registry and its `StreamARN` is unchanged, but the re-created log is built with `next_fragment_number: 1`, so the next fragment a producer sends to that stream is minted as fragment number "1" a second time.

Two things in this crate's own code are then broken:

1. `Fragment::fragment_number` is documented as "The stream-scoped fragment number ... increasing with write order" (src/state.rs:50-51). After the log drop the numbering restarts, so one stream mints the SAME number for two different chunks, and `GetMedia` goes on to serve the new chunk with an `AWS_KINESISVIDEO_FRAGMENT_NUMBER` tag whose value already named a different chunk of that same stream. A `FRAGMENT_NUMBER` start selector for "1" (src/ops/get_media.rs:659-674 matches on `fragment.fragment_number == after`) can no longer name one chunk of the stream's lifetime.

2. The cross-service propagation into the Kinesis Video Archived Media plane records the re-minted number beside the old one. `archive_fragment` only RE-CREATES the archive entry when the registered ARN has changed (src/state.rs:645-648, `held.arn == registered.arn`), and here it has not — the control plane never removed the stream, only this store's ceiling removed the log — so the existing archive entry is kept and the fragment is appended to it. The archive's `append_fragment` inserts by numeric order and keeps duplicates (services/service-com-amazonaws-kinesisvideoarchivedmedia-awsacuityreader/src/state.rs:909-919), so the archive ends up holding two fragments numbered "1" for one stream, and `ListFragments` / `GetMediaForFragmentList("1")` can no longer resolve one of them. `ingest_fragment`'s own doc forbids exactly this: the two read planes are "two VIEWS of one ingested fragment, not two fragments ... under the same minted fragment number".

The same constructor shows that the recreation hazard was seen for the OTHER identifier namespace and handled there: `log_mut` deliberately hands out a FRESH `id` so "two streams never share a continuation-token prefix" (src/state.rs:202-203), while in the same `or_insert_with` the fragment counter is reset to 1. The per-stream fragment ceiling (`drop_oldest_beyond_the_ceiling`) does NOT reset the counter — it only increments `dropped` — so the log-drop path is the single site where this stream-scoped counter is lost.

The defect is not the ceiling itself (a stream whose log went reads empty, which this crate documents and tests): it is that the recreation forgets the stream's fragment-number high-water mark, so a number that already identified media is minted again.

## Evidence

The invariant this crate states for a fragment number — services/service-com-amazonaws-kinesisvideomedia-awsacuityinletservice/src/state.rs:49-52:

    pub struct Fragment {
        /// The stream-scoped fragment number, a decimal string (the modelled
        /// `FragmentNumberString` is `^[0-9]+$`, 1..128), increasing with write order.
        pub fragment_number: String,

The code that breaks it — the re-created log restarts the counter, in the very constructor that protects the token namespace against the same recreation — src/state.rs:201-227:

    fn log_mut(&mut self, stream_arn: &str, stream_name: &str) -> &mut MediaStream {
        // Ids are handed out from 1, so the `0` a stream with no log reports can never
        // be a live log's id, and two streams never share a continuation-token prefix.
        let fresh_id = self.next_stream_id.saturating_add(1);
        if !self.streams.contains_key(stream_arn) {
            self.next_stream_id = fresh_id;
            // A log this store does not hold yet is one more log, so the global ceiling is
            // enforced here — the one place the number of logs grows.
            self.drop_logs_beyond_the_ceiling();
        }
        ...
            .or_insert_with(|| MediaStream {
                ...
                next_fragment_number: 1,
                id: fresh_id,
                dropped: 0,

The drop that makes a live stream's log absent again — src/state.rs:242-255:

    fn drop_logs_beyond_the_ceiling(&mut self) {
        while self.streams.len() >= MAX_STORED_STREAM_LOGS {
            ...
            self.streams.remove(&oldest);

The rule the propagation then breaks — `ingest_fragment`'s own doc, src/state.rs:296-303:

    /// This is the ingest plane's one write. A fragment a producer submits is readable
    /// two ways — as a chunk of the live stream (`GetMedia`, this service's own log) and
    /// as an archived fragment (`ListFragments`, `GetMediaForFragmentList`, the Kinesis
    /// Video Archived Media archive) — and those are two VIEWS of one ingested fragment,
    /// not two fragments: both are written here, from the same bytes, under the same
    /// minted fragment number and the same producer and server timestamps. A read plane
    /// that reported a fragment the other did not would be reporting media nothing
    /// produced.

Why the archive keeps the earlier fragments (the ARN did not change, so the entry is not re-created) — src/state.rs:645-665:

        let held = store
            .streams
            .get(&registered.name)
            .is_some_and(|held| held.arn == registered.arn);
        if held { ... } else { store.put_stream(archive::ArchivedStream { ... fragments: Vec::new(), }); }
        store.append_fragment(&registered.name, fragment);

And that the archive keeps DUPLICATE numbers — services/service-com-amazonaws-kinesisvideoarchivedmedia-awsacuityreader/src/state.rs:909-919:

    pub fn append_fragment(&mut self, stream_name: &str, fragment: StoredFragment) -> bool {
        ...
        let at = stream
            .fragments
            .binary_search_by(|held| {
                numeric_order(&held.fragment_number, &fragment.fragment_number)
            })
            .unwrap_or_else(|insertion_point| insertion_point);
        stream.fragments.insert(at, fragment);

The consumer in the read path that assumes one chunk per number — src/ops/get_media.rs:659-664:

                "FRAGMENT_NUMBER" => {
                    let after = selector.after_fragment_number.as_deref().unwrap_or_default();
                    fragments
                        .iter()
                        .position(|fragment| fragment.fragment_number == after)

Test coverage: tests/all/media_log_store_ceiling.rs:87-99 pins only that a dropped log's stream "reads — empty" and that its old continuation token no longer resolves; it produces to each stream exactly once (tests/all/media_log_store_ceiling.rs:45-48) and never produces to a stream whose log was dropped, so nothing in the suite exercises or justifies the counter reset. tests/all/put_media_ingest.rs:814 ("successive sessions on one stream EXTEND it — a later session's fragments get …") pins the opposite expectation for the ordinary case.

## Steps to Reproduce

1. Mount the Kinesis Video control plane, this media service and the archived-media service on one engine (the harness tests/all/put_media_ingest.rs uses).
2. CreateStream 65 streams (`media::state::MAX_STORED_STREAM_LOGS` is 64), and for each one call GetDataEndpoint(APIName=PUT_MEDIA) and POST /putMedia at the announced address with a one-Cluster container; each session is acknowledged with FragmentNumber "1".
3. Observe that producing to the 65th stream runs `drop_logs_beyond_the_ceiling` (src/state.rs:242-255) and removes the FIRST stream's media log — the state tests/all/media_log_store_ceiling.rs:87-99 already asserts (that stream now reads an empty GetMedia payload).
4. POST /putMedia again to that first stream — still registered in the control plane, still the same StreamARN — with a second one-Cluster container.
5. Read the acknowledgement stream: the fragment is minted as FragmentNumber "1" again, because `log_mut` re-created the log with `next_fragment_number: 1`.
6. POST /getMedia on that stream with StartSelectorType EARLIEST: the new chunk carries AWS_KINESISVIDEO_FRAGMENT_NUMBER = 1, the number that already named the stream's earlier chunk.
7. Call ListFragments (or GetMediaForFragmentList with FragmentNumber "1") on the same stream against the archived-media plane: the archive now holds TWO fragments numbered "1" — the pre-drop chunk and the post-drop chunk — so the two read planes no longer agree on what fragment "1" of that stream is.
