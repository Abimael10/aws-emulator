Issue-Severity: MEDIUM
Issue-Gap-Class: validation
Issue-Commit: 985f778d374dab6bb5a4d520cf582de4208fb882

# GetMedia's SERVER_TIMESTAMP cursor still reads the fragment log as server-time-ordered, but the log is ordered by PRODUCER timestamp, so a server-timestamp read silently omits fragments received after the requested instant

_Filed: 2026-11-26T18:38:56Z_
_Operation: GetMedia | Class: validation_

> _Note: this issue was identified against the code base at commit `985f778d374dab6bb5a4d520cf582de4208fb882`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The fragment log `GetMedia` walks is kept in PRODUCER-capture order: `ingest_fragment` inserts each produced fragment at the position its `producer_timestamp` names, explicitly so that a later session naming instants an earlier session already covered takes its place in that order instead of landing at the end. Every fragment in one ingest request shares ONE server timestamp (`server_timestamp: received_at`), and a later request's server timestamp is always greater than an earlier one's — so as soon as a session is accepted whose producer instants fall inside the span the log already covers, index order and SERVER-stamp order disagree: the inserted fragment carries the NEWEST server stamp at a MIDDLE index.

The handler's `SERVER_TIMESTAMP` branch nonetheless resolves its cursor as if index order were server order: it takes the maximum server stamp that is not after the requested instant, finds the FIRST index carrying that stamp, and then emits a contiguous run from there to the end. Any fragment whose server stamp is at or after the requested instant but which sits at a LOWER index is never delivered by that read.

Concrete loss. Session 1 (ABSOLUTE) submits one Cluster captured at producer instant p=20, received at server instant s=100. Session 2 submits one Cluster captured at p=10 (earlier — allowed across sessions, see put_media.rs:388-396), received at s=300. `ingest_fragment` places it first, so the log is [F2(p=10,s=300), F1(p=20,s=100)]. A `GetMedia` with StartSelectorType=SERVER_TIMESTAMP and StartTimestamp=100 computes max{stamps <= 100} = 100, whose first index is 1, and delivers F1 only — F2, received at s=300 (after the requested instant), is delivered by no server-timestamp read at that instant. Asking for the LATER instant 300 delivers MORE (both fragments), which inverts the selector's own monotonicity.

Same root cause, second symptom: AWS_KINESISVIDEO_MILLIS_BEHIND_NOW is computed against `fragments.last()`'s server stamp as "the latest chunk on the stream", which after such an insertion is not the newest server stamp at all — and the handler's own `.max(0)` clamp on that subtraction is the admission that the difference can go negative, i.e. that server stamps are NOT monotone in log order.

This is the SERVER_TIMESTAMP residue of the fixed validation issue 20261124T150502Z, which reported the same shape for PRODUCER_TIMESTAMP. That fix made the log producer-ordered, which closes the producer half for good — a vector can only be ordered by one of the two stamps — and leaves the server half reading an ordering the log does not have. Nothing here asks to reverse that fix; the SERVER_TIMESTAMP branch is what still assumes it.

## Evidence

The log's ordering, and the writer that establishes it — services/service-com-amazonaws-kinesisvideomedia-awsacuityinletservice/src/state.rs:420-431:

    // The log is ordered by the instant the producer captured each fragment at, which
    // is the order its readers resolve a cursor in ([`MediaStream::fragments`]). A
    // producer may open a new session whose timecodes name instants an earlier session
    // already covered — a RELATIVE session restarts its offsets at 0 — so the fragment
    // takes its place in that order rather than the end of the vector, AFTER every
    // fragment captured at the same instant, so fragments of one instant stay in the
    // order they were received.
    let position = stream
        .fragments
        .iter()
        .rposition(|held| held.producer_timestamp <= produced.producer_timestamp)
        .map_or(0, |last| last + 1);

That a later session is ACCEPTED with earlier producer instants, and that all its fragments take one server stamp — src/service/put_media.rs:388-396 and :459:

    // The timecode ordering is a property of the SESSION: a Cluster's timecode is read
    // against this connection's own timecode type and producer start, so the only
    // fragment it can be behind is one the SAME connection submitted before it. A new
    // connection states a new baseline — a RELATIVE session's timecodes legitimately
    // restart at 0, naming instants its predecessor's offsets already covered — so
    // nothing in it is compared against what another session left, and the log is kept
    // in time order where it is written instead
    // ([`state::...::ingest_fragment`]).
    let mut previous: Option<SystemTime> = None;
...
                    server_timestamp: received_at,

The reader that assumes the OTHER ordering — src/ops/get_media.rs:676-706:

                // "PRODUCER_TIMESTAMP or SERVER_TIMESTAMP - Start with the chunk
                // CONTAINING a fragment with the specified timestamp": the chunks that
                // contain the requested instant are the ones carrying the newest stamp
                // that is not after it, and the session starts with the FIRST of them,
                ...
                "PRODUCER_TIMESTAMP" | "SERVER_TIMESTAMP" => {
                    let wanted = start_timestamp_millis.unwrap_or_default();
                    let producer = selector_type == "PRODUCER_TIMESTAMP";
                    let stamped = |fragment: &crate::state::Fragment| {
                        crate::state::epoch_millis(if producer {
                            fragment.producer_timestamp
                        } else {
                            fragment.server_timestamp
                        })
                    };
                    fragments
                        .iter()
                        .filter_map(|fragment| {
                            let at = stamped(fragment);
                            (at <= wanted).then_some(at)
                        })
                        .max()
                        .and_then(|covering| {
                            fragments
                                .iter()
                                .position(|fragment| stamped(fragment) == covering)
                        })
                        .unwrap_or(0)
                }

and then emits contiguously from that index — src/ops/get_media.rs:731-744 (`for fragment in &fragments[start..]`).

The crate's own admission that server stamps are not monotone in log order — src/ops/get_media.rs:793-795:

                    // "how far behind the chunk returned in the response is from the
                    // latest chunk on the stream".
                    newest_server_millis.saturating_sub(server_millis).max(0),

with `newest_server_millis` taken from the LAST element of the producer-ordered log — src/ops/get_media.rs:745-748:

            let newest_server_millis = fragments
                .last()
                .map(|fragment| crate::state::epoch_millis(fragment.server_timestamp))
                .unwrap_or_default();

And the stale invariant that still claims the ordering is maintained by REFUSAL, contradicting put_media.rs:388-396 above — src/state.rs:72-78:

    /// The fragments this log still holds, oldest first. `GetMedia` emits a contiguous
    /// run of these, so the index of a fragment in this vector IS the stream cursor.
    ///
    /// The ingest plane ([`crate::service::put_media`]) is what keeps it so: a Cluster
    /// captured before the newest fragment the log already holds is refused rather than
    /// appended, across sessions as within one.
    pub fragments: Vec<Fragment>,

The rule the crate's own test states, and the selector it states it for — tests/all/put_media_log_time_order.rs:579-583 and :660:

    /// RULE: `GetMedia`'s timestamp start selector starts with the chunk CONTAINING the
    /// requested instant and delivers every chunk after it, so no fragment stamped at or
    /// after that instant is left undelivered — which holds because a session naming instants
    /// the log already covers has its fragments placed at those instants, leaving the log in
    /// capture order.
...
                    "StartSelectorType": "PRODUCER_TIMESTAMP",

Test coverage: that suite reads back an out-of-order log with PRODUCER_TIMESTAMP only — every `Read` row goes through `"StartSelectorType": "PRODUCER_TIMESTAMP"` (tests/all/put_media_log_time_order.rs:622-664) — and the stated justification ("leaving the log in capture order") is exactly the property that does not hold for server stamps. The SERVER_TIMESTAMP rows in tests/all/get_media.rs:499-521 are seeded with strictly increasing server stamps (`fn server_millis(index) = producer_millis(index) + SERVER_LAG_MILLIS`, tests/all/get_media.rs:137-139), so no test ever presents a log whose server stamps are out of index order. Nothing pins the behaviour reported here.

## Steps to Reproduce

1. CreateStream "probe" on the Kinesis Video control plane; GetDataEndpoint(StreamName=probe, APIName=PUT_MEDIA) for the ingest address and APIName=GET_MEDIA for the read address.
2. POST /putMedia at the ingest address with x-amzn-fragment-timecode-type: ABSOLUTE and one Cluster captured at producer instant p2 (say epoch-ms 1761000020000). It is received now, so its server timestamp is s1 = the current instant. Observe 200 / PERSISTED for fragment 1.
3. Wait a couple of seconds, then POST /putMedia again with one Cluster captured EARLIER, at producer instant p1 = 1761000010000. put_media.rs:388-396 does not compare across sessions, so it is accepted; its server timestamp s2 is greater than s1. Observe 200 / PERSISTED for fragment 2.
4. The log is now [fragment 2 (p=p1, s=s2), fragment 1 (p=p2, s=s1)] — producer-ordered by state.rs:420-431, with the NEWEST server stamp at index 0.
5. POST /getMedia at the read address with {"StreamName":"probe","StartSelector":{"StartSelectorType":"SERVER_TIMESTAMP","StartTimestamp":<s1 as epoch seconds>}} and read the AWS_KINESISVIDEO_FRAGMENT_NUMBER / AWS_KINESISVIDEO_SERVER_TIMESTAMP tags out of the payload.
6. Observe only fragment 1 (server stamp s1) is delivered: fragment 2, whose server stamp s2 is AFTER the requested instant, is omitted — while repeating the call with StartTimestamp=<s2> delivers BOTH fragments, so asking for a later server instant returns strictly more than asking for an earlier one.
7. In the same payload, note AWS_KINESISVIDEO_MILLIS_BEHIND_NOW for fragment 1 is 0, computed against `fragments.last()`'s server stamp (s1) rather than the stream's newest server stamp (s2).
