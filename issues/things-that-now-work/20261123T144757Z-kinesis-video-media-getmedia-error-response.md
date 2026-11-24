Issue-Severity: MEDIUM
Issue-Gap-Class: error-response
Issue-Commit: b743c8ed5789be1c53f63ef71d8598fcef7945b3

# GetMedia detects a wrong-endpoint call but answers 404 ResourceNotFoundException instead of the modelled InvalidEndpointException

_Filed: 2026-11-23T14:47:57Z_
_Operation: GetMedia | Class: error-response_

> _Note: this issue was identified against the code base at commit `b743c8ed5789be1c53f63ef71d8598fcef7945b3`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The handler implements the per-stream endpoint rule — a read that reached the media endpoint announced for ANOTHER stream is refused — but it answers that condition with `ResourceNotFoundException` (404) and the fixed sentence "The requested stream is not found or not active.", i.e. it tells the caller the stream does not exist even though the control-plane registry resolved it a few lines earlier. The model declares a distinct error for exactly this condition, `InvalidEndpointException` (400), whose own documentation states both the trigger and the remedy (re-run `GetDataEndpoint` and retry `GetMedia` at the returned endpoint). A client that keys on the refusal therefore deletes/re-creates the stream, or gives up, instead of re-discovering the endpoint, which is the action the modelled error exists to prompt.

This is also an internal contradiction inside the handler: its module doc says the wrong-endpoint behaviour is NOT modelled here and was "filed as [its] own gap issue" (no such issue exists in the backlog — `list_existing_issues(query="InvalidEndpoint")` returns none), while the code 500 lines below does implement a wrong-endpoint decision and routes it into a different modelled error. `InvalidEndpointException` is constructible in this crate today (the generated shape and the `GetMediaErrorModeled::InvalidEndpointException` variant exist) and is never returned from any path.

Note the read-side scoping branch has no test of its own; only the ingest sibling's equivalent rule is pinned (`tests/all/ingest_stream_scoping.rs:150`), so nothing in this crate pins the read route's choice of error code.

## Evidence

Smithy (`smithy_query(query="doc", shape="InvalidEndpointException")`), quoted verbatim: "Status Code: 400, Caller used wrong endpoint to write data to a stream. On receiving such an exception, the user must call GetDataEndpoint with AccessMode set to \"READ\" and use the endpoint Kinesis Video returns in the next GetMedia call." The operation declares it: `GetMedia` errors include `InvalidEndpointException (HTTP 400)` (OPERATIONS.md:32).

The handler detects the condition and answers a different error — services/service-com-amazonaws-kinesisvideomedia-awsacuityinletservice/src/ops/get_media.rs:623-636:
```
// And the stream has to be the one the ADDRESS serves: an announced data
// endpoint is the endpoint of the stream it was discovered for, so a read at
// it naming another stream asked an endpoint for a stream it does not serve —
// as unreadable as a stream that is not there, and the same sentence.
if !crate::service::read_plane::serves_the_addressed_stream(
    &*self.engine, ctx.parts(), &stream.arn,
) {
    return Err(resource_not_found(
        ::service_com_amazonaws_kinesisvideoarchivedmedia_awsacuityreader::state::STREAM_NOT_FOUND_MESSAGE
            .to_string(),
    ));
}
```
The same file's own doc admits the gap — src/ops/get_media.rs:31-35:
```
//! hands back the token that resumes there. The throttling
//! (`ClientLimitExceededException`, `ConnectionLimitExceededException`) and wrong-endpoint
//! (`InvalidEndpointException`) behaviours, and the stream-level/expired-token refusal the
//! modelled `NotAuthorizedException` (401) carries, are not modelled here and are filed as
//! their own gap issues.
```
And the helper it calls states the rule it is enforcing is the one that error carries — src/service/read_plane.rs:11-14: "this service's own `InvalidEndpointException` states the rule from the data side (\"Caller used wrong endpoint to write data to a stream. On receiving such an exception, the user must call `GetDataEndpoint` with `AccessMode` set to \\\"READ\\\" ...\")", and src/service/read_plane.rs:60-69: "Whether the address a request arrived on is the media endpoint OF the stream whose ARN is `stream_arn` ... a read there naming another stream asked an endpoint for a stream it does not serve."

The modelled variant exists and is unused: src/ops/get_media.rs:153 `InvalidEndpointException(InvalidEndpointException),` — grep for `InvalidEndpointException` across the crate's `src/` finds only the enum variant, the generated shape (src/ops/types/get_media_types.rs:56-58) and doc comments; no construction site.

Test coverage: the read-side per-stream branch is untested (`grep serves_the_addressed_stream tests/` matches nothing); only the ingest plane's analogous rule is pinned, at tests/all/ingest_stream_scoping.rs:150-156 ("RULE: an announced ingest endpoint serves the ONE stream it was discovered for — a session naming any other stream is refused 404 `ResourceNotFoundException`").

## Steps to Reproduce

1. Boot the emulator and create two streams through the control plane: `CreateStream(StreamName="stream-a")` and `CreateStream(StreamName="stream-b")`; poll `DescribeStream` until both are ACTIVE.
2. Discover stream-a's live-media endpoint: `GetDataEndpoint(APIName="GET_MEDIA", StreamName="stream-a")` -> endpoint_a.
3. POST /getMedia at endpoint_a with body {"StreamName": "stream-b", "StartSelector": {"StartSelectorType": "EARLIEST"}} (stream-b exists and resolves in the registry; only the endpoint is the wrong one).
4. Observe HTTP 404 `ResourceNotFoundException` with message "The requested stream is not found or not active." from src/ops/get_media.rs:632-635, instead of the 400 `InvalidEndpointException` the model declares for a caller that "used wrong endpoint" and that tells the caller to re-run GetDataEndpoint.
5. Confirm the same call at stream-b's own announced GET_MEDIA endpoint is served 200, proving the refusal is about the endpoint and not about the stream.
