Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: ac0d009460b15386fb7f532991bd4fb2bd9b5be8

# A non-UTF-8 request body is handed to the revision's code as a lossy U+FFFD transcription, although the plane's own doc says such bytes are not carried and that "nothing here pretends otherwise"

_Filed: 2026-11-26T15:16:27Z_
_Operation: CreateWebFunction | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `ac0d009460b15386fb7f532991bd4fb2bd9b5be8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`CreateWebFunction` (and `CreateWebFunctionEndpoint`) publishes an endpoint data plane by calling `crate::state::open_endpoint_plane`; every request that address answers is built into the guest-facing `HandlerRequest` by `handler_request` in the same state module. Two doc comments in this crate state the contract of that seam: a request body that is not UTF-8 is NOT carried through to the revision's code, and the layer does not pretend otherwise.

The code under those comments does the opposite. `handler_request` transcodes the body with `String::from_utf8_lossy`, which carries every byte through after replacing each invalid sequence with U+FFFD. So a client that POSTs a PNG, a gzip body, or any other non-text payload to the published `domainName` is not refused and is not told the layer cannot carry it: the revision's JavaScript is handed a silently CORRUPTED body and whatever it answers is served to the client as a normal 2xx. The emulator claims the request worked while the bytes the function received are not the bytes the client sent.

A second, mechanical consequence of the same line: the request's own headers are passed through verbatim just above (lines 589-598), so the guest sees the client's original `content-length` next to a body string whose byte length differs (each replaced sequence becomes a 3-byte U+FFFD). A Node framework that buffers or validates against `content-length` therefore reads a self-inconsistent request that no real HTTP server would hand it.

The fix is not binary-body support (that would be a new feature the module explicitly defers): it is to stop pretending — either carry the bytes faithfully, or report the case the way the comment says this layer reports it, instead of handing the guest a mangled transcription and answering as if nothing was lost.

## Evidence

The function's OWN doc comment, services/service-com-amazonaws-lambdaweb-lambdaweb/src/state/endpoint_plane.rs:578-583:
```
/// One request as the revision's code is handed it: the method, the path and query, the
/// headers lowercased as a Node server reports them, and the body as text.
///
/// The body reaches the code as TEXT, which is what this layer carries (see
/// [`super::web_runtime`]): the bytes of a request body that is not UTF-8 are not carried
/// through to the code.
```
…and the line under it that carries them anyway, services/service-com-amazonaws-lambdaweb-lambdaweb/src/state/endpoint_plane.rs:603-605:
```
        // The body as the TEXT the code reads. A request this address accepts is already
        // bounded by the core's per-connection ceiling before it reaches here.
        body: String::from_utf8_lossy(req.body()).to_string(),
```
`String::from_utf8_lossy` does not drop a non-UTF-8 body; it rewrites each invalid sequence as U+FFFD and returns the rest, so the guest is handed a transcription of the bytes rather than nothing.

The module header the comment points at, services/service-com-amazonaws-lambdaweb-lambdaweb/src/state/web_runtime.rs:80-86:
```
//! # What this layer carries
//!
//! A request's body reaches the code as TEXT and the body it answers with is the text it
//! wrote: the bytes of a request that is not UTF-8 are not carried through this seam, and
//! the guest is given no byte-buffer type to write one with. So this layer serves the text
//! HTTP a web function framework answers with — HTML, JSON, plain text — and a binary
//! request or response is not yet carried; nothing here pretends otherwise.
```

The headers that are passed through unchanged beside the rewritten body, services/service-com-amazonaws-lambdaweb-lambdaweb/src/state/endpoint_plane.rs:589-598:
```
    let headers = req
        .headers()
        .iter()
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.as_str().to_lowercase(), value.to_string()))
        })
        .collect();
```
(`content-length` is ASCII, so it survives and is reported to the guest alongside the re-encoded body.)

The handler that publishes the plane this seam belongs to, services/service-com-amazonaws-lambdaweb-lambdaweb/src/ops/create_web_function.rs:364-372:
```
        let endpoint_plane = match (&input.endpoint_config, &input.revision_config) {
            (Some(config), Some(_)) => Some(
                crate::state::open_endpoint_plane(
                    &*self.engine,
                    &state,
                    &input.function_name,
                    &config.endpoint_name,
                )
```

Test coverage: NONE pins this. Every data-plane request in tests/all/endpoint_data_plane.rs is built with an empty body — `let request = builder.body(Vec::new()).expect("a well-formed request");` (tests/all/endpoint_data_plane.rs:220) — and the raw-socket cases at lines 802-1360 send request lines with no payload. `grep -n 'from_utf8_lossy|binary|non-UTF' tests/` matches only response-side body readers (tests/all/endpoint_data_plane.rs:235, 878), so no test sends a request body to a published address at all, let alone a non-UTF-8 one.

## Steps to Reproduce

1. Start the emulator; PutObject a Node.js .zip deployment package whose handler echoes the request body back (for example `res.end(req.body)` or a `Buffer.byteLength(body)` report) and create an execution role and bucket for it
2. `PUT /2025-03-07/web-functions` with `revisionConfig` pointing at that object and `endpointConfig` `{"endpointName":"ep","endpointType":"HomeRegion","authType":"ApplicationManaged"}`
3. Poll `GET /2025-03-07/web-functions/{functionName}/endpoints/ep` until `state` is `Active` and read `domainName`
4. POST to that address with a body of raw non-UTF-8 bytes (e.g. the four bytes `FF D8 FF E0`, a JPEG SOI) and `Content-Type: application/octet-stream`
5. Observe a 200 computed from a body the function never received: the guest's body is the U+FFFD transcription produced by `String::from_utf8_lossy` (4 bytes in, 12 bytes of replacement characters out), while the `content-length` header the guest reads still says 4 — instead of the layer reporting that it does not carry this request, which is what its own doc comment and `web_runtime`'s "nothing here pretends otherwise" state
