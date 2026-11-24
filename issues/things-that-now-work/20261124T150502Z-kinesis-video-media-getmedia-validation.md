Issue-Severity: MEDIUM
Issue-Gap-Class: validation
Issue-Commit: cb16ceff5b6eb3896a41d4deca735a39546ae6bc

# GetMedia's timestamp cursor reads the fragment log as time-ordered, but the ingest path enforces that ordering only within a single request

_Filed: 2026-11-24T15:05:02Z_
_Operation: GetMedia | Class: validation_

> _Note: this issue was identified against the code base at commit `cb16ceff5b6eb3896a41d4deca735a39546ae6bc`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`GetMedia`'s start-selector resolution assumes the fragment log is in time order, and the only writer of that log does not keep it so.

The assumption is stated twice in this crate: `MediaStream::fragments` is "The fragments this log still holds, oldest first. `GetMedia` emits a contiguous run of these, so the index of a fragment in this vector IS the stream cursor" (src/state.rs:72-74), and the PRODUCER_TIMESTAMP / SERVER_TIMESTAMP branch of the handler resolves a cursor as "the newest stamp that is not after it" and then emits contiguously from the first fragment carrying that stamp (src/ops/get_media.rs:676-706).

The writer that is supposed to maintain it is this service's own ingest route, which refuses a Cluster whose timecode moves backwards and states the reason as a property of the LOG: "the fragment log is ordered by time, so a fragment that goes back cannot be appended to it" (src/service/put_media.rs:395-398). But the value that check compares against, `previous`, is a local of `serve` initialised to `None` on every request (src/service/put_media.rs:388), so the rule only holds WITHIN one container. A second `POST /putMedia` session whose first Cluster names an instant earlier than the newest fragment already persisted passes the check and is appended by `ingest_fragment` (src/state.rs:323-328) with a `producer_timestamp` behind its predecessor's — even though this crate holds and exposes the newest fragment's timestamp (`log_by_arn`, src/state.rs:392) and already reads the log on that same path (`received_fragment_number`, src/state.rs:394 call site).

After such an ingest the log's index order and its time order disagree, and two readers that trust the invariant go wrong:

* `GetMedia` with PRODUCER_TIMESTAMP: with producer stamps [t+10s, t], a read at t+5s picks the covering stamp `t` (index 1) and delivers ONLY that chunk — a chunk stamped BEFORE the requested instant — while the chunk stamped after it (index 0) is not delivered, against `StartSelector$StartTimestamp`'s "The `GetMedia` API then starts with the chunk containing the fragment that has the specified timestamp".
* `drop_oldest_beyond_the_ceiling` removes `stream.fragments.remove(0)` as "the oldest" (src/state.rs:682), which is then not the oldest fragment in time.

This is not a new feature: the refusal, its error code and its in-band acknowledgement already exist on the ingest path, and the accessor the check needs already exists; only the state the check is taken against is reset per request. Either end closes it — enforce the stated ordering rule against the persisted log, or stop reading the vector as time-ordered.

## Evidence

The invariant GetMedia resolves its cursor under:

  services/service-com-amazonaws-kinesisvideomedia-awsacuityinletservice/src/state.rs:72-74
    /// The fragments this log still holds, oldest first. `GetMedia` emits a contiguous
    /// run of these, so the index of a fragment in this vector IS the stream cursor.

  services/service-com-amazonaws-kinesisvideomedia-awsacuityinletservice/src/ops/get_media.rs:676-682
    // "PRODUCER_TIMESTAMP or SERVER_TIMESTAMP - Start with the chunk
    // CONTAINING a fragment with the specified timestamp": the chunks that
    // contain the requested instant are the ones carrying the newest stamp
    // that is not after it, and the session starts with the FIRST of them,

The rule this crate states for the log, and the request-scoped state it is enforced against:

  services/service-com-amazonaws-kinesisvideomedia-awsacuityinletservice/src/service/put_media.rs:395-398
    // "A cluster timecode moving backwards" ends the session at that Cluster:
    // the fragment log is ordered by time, so a fragment that goes back cannot
    // be appended to it. The Clusters already taken stay ingested.
    if previous.is_some_and(|held| cluster.timecode < held) {

  services/service-com-amazonaws-kinesisvideomedia-awsacuityinletservice/src/service/put_media.rs:388
    let mut previous: Option<u64> = None;
  (a local of `pub async fn serve(&self, req: &RtRequest, ctx: &OperationContext<'_>) -> RtResponse`, src/service/put_media.rs:294 — so it starts empty for every session, and nothing compares a Cluster against the log)

The state the check could be taken against, already owned by this crate:

  services/service-com-amazonaws-kinesisvideomedia-awsacuityinletservice/src/state.rs:392
    pub fn log_by_arn(&self, arn: &str) -> Option<&MediaStream> {

The other reader of "oldest":

  services/service-com-amazonaws-kinesisvideomedia-awsacuityinletservice/src/state.rs:682
    let dropped = stream.fragments.remove(0);

The model sentence the GetMedia symptom contradicts (smithy_query shape=StartSelector, member StartTimestamp): "A timestamp value. This value is required if you choose the PRODUCER_TIMESTAMP or the SERVER_TIMESTAMP as the startSelectorType. The GetMedia API then starts with the chunk containing the fragment that has the specified timestamp."

Test coverage: the backwards-timecode refusal is pinned only WITHIN one container — tests/all/put_media_ingest.rs:1253-1265, `name: "a cluster timecode that goes back"`, one `Container::of(vec![cluster(0,..), cluster(2000,..), cluster(1000,..), cluster(3000,..)])` with `expected_code: "FRAGMENT_TIMECODE_LESSER_THAN_PREVIOUS"`. The multi-session suite only ever advances: tests/all/put_media_ingest.rs:825 `for (session, timecodes) in [(0usize, vec![0u64, 1000]), (1, vec![2000, 3000, 4000])]` under the rule "successive sessions on one stream EXTEND it" (tests/all/put_media_ingest.rs:814-816), and the GetMedia timestamp-cursor suites seed strictly increasing stamps (tests/all/get_media.rs:113-118 `BASE_MILLIS` + `FRAGMENT_SPACING_MILLIS`). No test submits a session behind the log, so nothing pins the behaviour reported here.

## Steps to Reproduce

1. CreateStream "probe" on the Kinesis Video control plane, then GetDataEndpoint(StreamName=probe, APIName=PUT_MEDIA) for the ingest address and GetDataEndpoint(APIName=GET_MEDIA) for the read address.
2. POST /putMedia at the ingest address with `x-amzn-stream-name: probe`, `x-amzn-fragment-timecode-type: ABSOLUTE`, `x-amz-content-sha256` declared, and a container whose single Cluster carries Timestamp = 1761000010000 (epoch ms). Observe BUFFERING/RECEIVED/PERSISTED for fragment 1.
3. POST /putMedia again, same headers, with a container whose single Cluster carries Timestamp = 1761000000000 — ten seconds BEFORE the fragment already persisted. Observe 200 with PERSISTED for fragment 2 and no FRAGMENT_TIMECODE_LESSER_THAN_PREVIOUS acknowledgement: the log now holds producer stamps [t+10s, t], so `MediaStream::fragments` is no longer "oldest first".
4. POST /getMedia at the read address with {"StreamName":"probe","StartSelector":{"StartSelectorType":"PRODUCER_TIMESTAMP","StartTimestamp":1761000005}} and read the AWS_KINESISVIDEO_FRAGMENT_NUMBER / AWS_KINESISVIDEO_PRODUCER_TIMESTAMP tags out of the payload.
5. Observe only fragment 2 is returned, whose producer timestamp (1761000000000) is BEFORE the requested instant, while fragment 1 (1761000010000, after the requested instant) is returned by no timestamp selector at that instant.
