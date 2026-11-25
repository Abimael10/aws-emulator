Issue-Severity: MEDIUM
Issue-Gap-Class: pagination
Issue-Commit: 6e4695f2e2e0380b6e4530904193a77f767a96ef

# A continuation token stops naming the fragment it was minted for when an ingest inserts an earlier-captured fragment, so the resumed session replays a delivered chunk and never delivers the new one

_Filed: 2026-11-25T16:14:31Z_
_Operation: GetMedia | Class: pagination_

> _Note: this issue was identified against the code base at commit `6e4695f2e2e0380b6e4530904193a77f767a96ef`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

A `GetMedia` continuation token is persisted as an ABSOLUTE INDEX into the stream's fragment vector (`Continuation::next_fragment_position` = `MediaStream::dropped` + index). This crate maintains that absolute position against exactly one mutation of the vector — fragments DROPPED off its oldest end, where `dropped` is incremented and `forget_cursors_behind_the_log` prunes the cursors that went with the media. The ingest path has a SECOND mutation that shifts indices and does no such bookkeeping: `ingest_fragment` INSERTS a fragment at a computed position inside the vector (a reconnecting producer's session legitimately names instants an earlier session already covered), which renumbers every later fragment without touching `dropped` and without adjusting any recorded cursor.

After such an insert, a token the client already holds resolves to a different fragment than the one it was minted to resume after. With a one-fragment log [F1] and a token minted at absolute position 1 ("nothing left to send"), a second session ingesting a fragment captured EARLIER inserts it at index 0, so the log becomes [F2, F1]; the client's token still resolves to index 1, which is now F1. The resumed call answers F1 a second time, and F2 — the only new media on the stream — is never delivered on that cursor chain. With a longer log the same shift duplicates one chunk and skips one for every token held across the insert.

That is exactly what the handler's own comment above the token minting says cannot happen ("neither repeat nor skip a fragment"), what `record_continuation`'s doc promises ("the token keeps naming the same fragment"), and what `GetMediaOutput$Payload` documents the token for ("get the next chunk where the last request terminated"). The insert path was plainly added without revisiting the log's documented invariants: the field doc of `MediaStream::fragments` still states a cross-session ingest rule ("a Cluster captured before the newest fragment the log already holds is refused rather than appended, across sessions as within one") that `put_media.rs` explicitly declines to apply and `ingest_fragment` contradicts by inserting instead.

The insert ordering itself is pinned by tests (`put_media_reconnecting_producer.rs`) and must stay; what is missing is the cursor bookkeeping on insert that the sibling drop path already performs.

## Evidence

The handler's claim about its tokens — services/service-com-amazonaws-kinesisvideomedia-awsacuityinletservice/src/ops/get_media.rs:761-766:

    // One continuation token per emitted chunk, each resuming immediately after
    // the chunk that carries it — which is what makes two sequential calls
    // neither repeat nor skip a fragment.
    let tokens: Vec<String> = (0..chunks)
        .map(|offset| store.record_continuation(&stream_arn, stream_id, start + offset + 1))
        .collect();

What is recorded, and the promise attached to it — src/state.rs:462-464 (doc of `record_continuation`):

    /// `next_fragment_index` is an index into the log AS IT IS NOW; what is recorded (and
    /// what the token text carries) is the ABSOLUTE position that index names, so the
    /// token keeps naming the same fragment after older ones are dropped.

src/state.rs:85-91 (doc of `MediaStream::dropped`):

    /// How many fragments this log has dropped off its oldest end ... It is what makes a
    /// cursor OUTLIVE a drop: a position in this log is `dropped` plus an index into
    /// [`Self::fragments`], and that absolute position is what a continuation token is
    /// minted from, so a token still names the fragment it was minted for after older ones
    /// have gone.

The resolution that turns the absolute position straight back into an index — src/state.rs:516-519:

    let dropped = self.streams.get(stream_arn).map_or(0, |stream| stream.dropped);
    let index = usize::try_from(continuation.next_fragment_position.saturating_sub(dropped))
        .unwrap_or(usize::MAX);
    (index <= fragment_count).then_some(index)

The second mutation, which shifts those indices and adjusts nothing — src/state.rs:334-352:

    let position = stream
        .fragments
        .iter()
        .rposition(|held| held.producer_timestamp <= produced.producer_timestamp)
        .map_or(0, |last| last + 1);
    stream.fragments.insert(
        position,
        Fragment { ... },
    );
    drop_oldest_beyond_the_ceiling(stream);
    // The cursors that named the fragments which just went are dropped with them: a
    // continuation store that outlived the media would be the one piece of
    // per-fragment bookkeeping this ceiling did not bound.
    self.forget_cursors_behind_the_log(&registered.arn);

(the drop half of the same function keeps the cursor bookkeeping; the insert half has none — no cursor at or after `position` is moved.)

The model text the token exists for, quoted in the handler's own scaffolded doc — src/ops/get_media.rs:114-117:

    //! AWS_KINESISVIDEO_CONTINUATION_TOKEN (UTF-8 string) - In the event your GetMedia call
    //! terminates, you can use this continuation token in your next request to get the next
    //! chunk where the last request terminated.

The log invariant the insert path left stale, stated two opposite ways in this crate — src/state.rs:72-77:

    /// The fragments this log still holds, oldest first. `GetMedia` emits a contiguous
    /// run of these, so the index of a fragment in this vector IS the stream cursor.
    ///
    /// The ingest plane ([`crate::service::put_media`]) is what keeps it so: a Cluster
    /// captured before the newest fragment the log already holds is refused rather than
    /// appended, across sessions as within one.

versus src/service/put_media.rs:388-395:

    // The timecode ordering is a property of the SESSION ... A new connection states a new
    // baseline — a RELATIVE session's timecodes legitimately restart at 0, naming instants
    // its predecessor's offsets already covered — so nothing in it is compared against what
    // another session left, and the log is kept in time order where it is written instead

Test coverage: the insert ordering is pinned (tests/all/put_media_reconnecting_producer.rs:345-396, e.g. case "an ABSOLUTE session inside the span already held" with `expected_log_millis: vec![BASE_MILLIS, BASE_MILLIS + 5_000, BASE_MILLIS + 10_000]`), but NO test resumes a continuation token across such a session: every continuation suite only ever appends — tests/all/get_media.rs:572-661 (`a_continuation_token_resumes_exactly_after_its_own_chunk`) writes the later fragment through `put_fragment` with a strictly later instant (tests/all/get_media.rs:618-631), tests/all/get_media.rs:1054-1118 resumes inside one unchanged log, and tests/all/media_log_store_ceiling.rs only exercises a token whose fragment was DROPPED. So nothing pins the behaviour reported here.

## Steps to Reproduce

1. CreateStream "probe" on the Kinesis Video control plane, then GetDataEndpoint(StreamName=probe, APIName=PUT_MEDIA) and GetDataEndpoint(StreamName=probe, APIName=GET_MEDIA) for the two announced addresses.
2. POST /putMedia at the ingest address with `x-amzn-stream-name: probe`, `x-amzn-fragment-timecode-type: ABSOLUTE`, a declared `x-amz-content-sha256`, and a container whose single Cluster carries Timestamp = 1700000010000 (epoch ms). Observe PERSISTED for fragment 1.
3. POST /getMedia at the read address with {"StreamName":"probe","StartSelector":{"StartSelectorType":"EARLIEST"}}; read the chunk's AWS_KINESISVIDEO_FRAGMENT_NUMBER ("1") and keep its AWS_KINESISVIDEO_CONTINUATION_TOKEN as T (it was minted at absolute position 1).
4. POST /putMedia again, same headers, with a container whose single Cluster carries Timestamp = 1700000000000 — ten seconds EARLIER than the fragment already persisted. Observe 200 with PERSISTED for fragment 2 (a reconnecting producer's legitimate session); `ingest_fragment` inserts it at index 0, so the log is now [fragment 2, fragment 1].
5. POST /getMedia with {"StreamName":"probe","StartSelector":{"StartSelectorType":"CONTINUATION_TOKEN","ContinuationToken":"<T>"}}.
6. Observe the payload carries AWS_KINESISVIDEO_FRAGMENT_NUMBER "1" — the chunk the client already received — instead of the chunk it has not seen, and that repeating the call with each token this response hands back never yields fragment 2: the token's absolute position was silently re-pointed by the insert.
