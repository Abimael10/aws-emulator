Issue-Severity: LOW
Issue-Gap-Class: maintainability
Issue-Commit: 44d4304dbd56e0f97705f387ff3fc6793140eb84

# The "does this address serve the addressed stream?" rule is implemented twice in this crate, once per route of the one endpoint

_Filed: 2026-11-23T06:09:03Z_
_Operation: GetMedia | Class: maintainability_

> _Note: this issue was identified against the code base at commit `44d4304dbd56e0f97705f387ff3fc6793140eb84`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

This crate serves two routes of one media endpoint — the modelled `POST /getMedia` (the `GetMedia` handler) and the recorded `POST /putMedia` ingest beside it — and both must answer the same question before they read or write a stream's media: is the address this request arrived on the endpoint announced for THIS stream? That one rule (read the `Host` header, hand the URI, the host and the stream's ARN to the control plane's `endpoint_serves_stream`) is written out twice, in two modules of this same crate, as two functions with the same name, the same three parameters and the same two-line body. The two copies already differ in how they present the URI to the shared resolver: the read copy stringifies `parts.uri.to_string()` while the ingest copy passes `req.uri()` through, and the read copy extracts the `Host` header through `::http::header::HOST` while the ingest copy uses `HOST.as_str()`. Nothing keeps them in step, yet `read_plane.rs` asserts in its own doc that they cannot drift — which is exactly the invariant a single shared helper would make true instead of claimed. The same shape repeats for the plane check beside it (`read_plane::read_plane_served_at` versus the ingest plane's own `ingest_served_at`) and for the "exactly one of the two stream identifiers" branch (`ops/get_media.rs:498-510` versus `service/put_media.rs:727-741`), so this is the crate's one addressing rule copied per route rather than stated once. A fix is a single helper both routes call; every response and every existing test stays byte-for-byte as it is.

## Evidence

Copy 1 — the read route, services/service-com-amazonaws-kinesisvideomedia-awsacuityinletservice/src/service/read_plane.rs:70-80:
```rust
pub fn serves_the_addressed_stream<Eng: Send + Sync + 'static>(
    engine: &Eng,
    parts: &::http::request::Parts,
    stream_arn: &str,
) -> bool {
    let host = parts
        .headers
        .get(::http::header::HOST)
        .and_then(|value| value.to_str().ok());
    control_plane::endpoint_serves_stream(engine, &parts.uri.to_string(), host, stream_arn)
}
```

Copy 2 — the ingest route, services/service-com-amazonaws-kinesisvideomedia-awsacuityinletservice/src/service/put_media.rs:707-714:
```rust
fn serves_the_addressed_stream<Eng: Send + Sync + 'static>(
    engine: &Eng,
    req: &RtRequest,
    stream_arn: &str,
) -> bool {
    let host = req.headers().get(::http::header::HOST.as_str());
    control_plane::endpoint_serves_stream(engine, req.uri(), host, stream_arn)
}
```

The claim the duplication contradicts, read_plane.rs:66-69: "The sibling [`super::put_media`] scopes the ingest route by the same reading, against the same record of which stream an address was announced for, so the two routes of this one endpoint cannot drift on whose media they serve." The symmetric claim sits on the ingest side too, put_media.rs:56-60.

The caller that makes this `GetMedia`'s: src/ops/get_media.rs:627-636 — `if !crate::service::read_plane::serves_the_addressed_stream(&*self.engine, ctx.parts(), &stream.arn) { return Err(resource_not_found(...)) }`; the ingest caller is put_media.rs:347 — `if !serves_the_addressed_stream(&*self.engine, req, &stream_arn) { return stream_not_found(&session).response(); }`.

The same rule copied a second time, as the "exactly one identifier" branch: src/ops/get_media.rs:498-510 (`(Some(_), Some(_)) => return Err(invalid_argument("Only one of StreamName and StreamARN may be specified."))`) against src/service/put_media.rs:730-741 (`(Some(_), Some(_)) => return Err(invalid_argument(format!("Only one of {STREAM_NAME_HEADER} and {STREAM_ARN_HEADER} may be specified.")))`), whose own comment names the copy it mirrors: "The stream is named by EXACTLY one of the two headers, as it is by exactly one of the two members on the read plane" (put_media.rs:727-729).

Test coverage that must stay unchanged: tests/all/ingest_stream_scoping.rs:9-15 and :216 pin the ingest copy ("asked an endpoint for a stream it does not serve: refused HTTP 404 `ResourceNotFoundException` ... and nothing persisted to either stream"); tests/all/read_plane_addressing.rs:260-264 and tests/all/media_endpoint_grouping.rs:6-16 pin where the read route is served; tests/all/get_media.rs:743-759 pins the identifier branch's status and error code for the "neither" and "both" rows.

## Steps to Reproduce

1. Open services/service-com-amazonaws-kinesisvideomedia-awsacuityinletservice/src/service/read_plane.rs and read `serves_the_addressed_stream` (lines 70-80), together with the doc above it (lines 66-69) claiming the two routes "cannot drift on whose media they serve".
2. Open services/service-com-amazonaws-kinesisvideomedia-awsacuityinletservice/src/service/put_media.rs and read `serves_the_addressed_stream` (lines 707-714): the same name, the same three parameters, the same `Host`-header-plus-`endpoint_serves_stream` body, already spelled differently (`parts.uri.to_string()` vs `req.uri()`, `HOST` vs `HOST.as_str()`).
3. Note the two callers that make each copy live: src/ops/get_media.rs:627 (GetMedia) and src/service/put_media.rs:347 (the ingest route).
4. Note the second copy of the same addressing rule: the "exactly one of StreamName/StreamARN" branch at src/ops/get_media.rs:498-510 and at src/service/put_media.rs:727-741, the latter's comment pointing at the former.
5. Confirm a single shared helper changes no wire behaviour: `cargo test -p service-com-amazonaws-kinesisvideomedia-awsacuityinletservice` (tests/all/ingest_stream_scoping.rs, read_plane_addressing.rs, media_endpoint_grouping.rs, get_media.rs) must pass untouched.
