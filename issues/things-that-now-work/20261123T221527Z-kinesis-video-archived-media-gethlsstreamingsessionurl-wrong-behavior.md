Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 04b7737d40b7fda90f20de8a2ba64e1a389f2093

# The minted streaming-session URL resolves to no route this emulator serves, and the recorded session (ContainerFormat, DiscontinuityMode, DisplayFragmentTimestamp, playlist snapshot) is write-only state no call can ever observe

_Filed: 2026-11-23T22:15:27Z_
_Operation: GetHLSStreamingSessionURL | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `04b7737d40b7fda90f20de8a2ba64e1a389f2093`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

Both streaming-session handlers build a session URL of the form `https://<host>/hls/v1/getHLSMasterPlaylist.m3u8?SessionToken=<token>` (`/dash/v1/getMPDManifest.mpd?...` for MPEG-DASH) and record a `StreamingSession` carrying everything the request asked for — the playback mode, `ContainerFormat`, `DiscontinuityMode`, `DisplayFragmentTimestamp`, `DisplayFragmentNumber`, the `Expires` window and the playlist/manifest snapshot of fragment numbers.

Nothing in the repository serves either of those two paths, and nothing ever reads `state.sessions`:

* this crate's `can_handle` claims a request only when its FIRST URI path segment is one the model-derived `ROUTE_PATH_SEGMENTS` declares (`getClip`, `getHLSStreamingSessionURL`, `listFragments`, ...), so a request for `/hls/v1/...` or `/dash/v1/...` is not even claimed by this service, and there is no `override_operation` that would route it;
* a repo-wide grep for `getHLSMasterPlaylist` / `getMPDManifest` / `hls/v1` finds only the two literals in `state.rs` that BUILD the URL plus two test assertions on its prefix — no handler, no custom route module (compare `services/service-com-amazonaws-appstream-photonadminproxyservice/src/service/streaming_session.rs`, which is how a non-modelled playback route is served elsewhere in the tree);
* `state.sessions` is inserted into by `record_session` and read by no handler, so every documented parameter the two operations accept beyond the stream identifier (ContainerFormat, DiscontinuityMode, DisplayFragmentTimestamp, DisplayFragmentNumber, MaxMediaPlaylistFragmentResults/MaxManifestFragmentResults) has no observable effect on any later call.

That contradicts this crate's own statements: `session_url`'s doc says the URL "deliberately names no real AWS host: this emulator serves the playback session, so a player or test that follows the URL must come back here", and the handler's module doc says it "returns the URL that resolves to it". A caller that follows the URL comes back here and is answered by the front door, not by a playlist. `GetDASHStreamingSessionURL` has the identical gap through the same shared helper; this issue covers both (the helper is `crate::state::session_url`).

## Evidence

The claim, in this crate's own state module:

* `services/.../src/state.rs:504-514` — "/// The URL a minted streaming session is reported in: the manifest or playlist\n/// resource of `protocol`, carrying `token`, on the endpoint the CALLER reached\n/// this emulator on. ... and deliberately names no real AWS host: this emulator serves the playback session, so a player or test that follows the URL must come back here."
* `services/.../src/ops/get_hls_streaming_session_url.rs:4-5` — "//! Mints an HLS streaming session over one stream's archive and returns the URL\n//! that resolves to it."

The URL that is actually built:

* `services/.../src/state.rs:528-532` — `format!("https://{host}/{}/v1/{}?SessionToken={token}", protocol.url_prefix(), protocol.manifest_resource())`
* `services/.../src/state.rs:578-583` — `Self::Hls => "getHLSMasterPlaylist.m3u8", Self::Dash => "getMPDManifest.mpd",`

The other side — nothing serves or reads it:

* `services/.../src/service.rs:80-83` (the claim rule) — "the request's routing key must match this service's signing name AND its first URI path segment must be one the generated, model-derived `ROUTE_PATH_SEGMENTS` declares, by exact segment equality", applied at `service.rs:100-113` / `service.rs:143-152`; `hls` and `dash` are not modelled path segments of this service (`OPERATIONS.md:28,35,42,49,57,64` lists the six routes: `/getClip`, `/getDASHStreamingSessionURL`, `/getHLSStreamingSessionURL`, `/getImages`, `/getMediaForFragmentList`, `/listFragments`).
* `services/.../src/service.rs:154-164` — `override_operation` is `let _ = req; None`, so no path is routed by hand.
* repo-wide grep for `getHLSMasterPlaylist|getMPDManifest|hls/v1` matches only `src/state.rs:580-581` (the literals) and the two prefix assertions in `tests/all/get_dash_streaming_session_url.rs:187` and `tests/all/archived_media_read_path.rs:1430`.
* crate-wide grep for `sessions` shows the map is written at `src/state.rs:957-973` (`record_session`, called from `get_hls_streaming_session_url.rs:501` and `get_dash_streaming_session_url.rs:491`) and read nowhere in `src/` — only from the test files, which reach into `service_state(...)` directly.

Test coverage that stays unchanged: `tests/all/archived_media_read_path.rs:1402-1473` asserts the URL's prefix and then reads the session back out of the store in-process (`state.sessions`), and `tests/all/get_dash_streaming_session_url.rs:38-51,187` does the same for MPEG-DASH — no existing test fetches the URL over HTTP, and `tests/all/route_claims.rs:40-84` pins no case for an `/hls` or `/dash` path.

## Steps to Reproduce

1. Seed a stream in the Kinesis Video Streams registry with retention > 0 and ingest fragments into this service's archive (as `tests/all/archived_media_read_path.rs::seeded` does).
2. POST /getHLSStreamingSessionURL with {"StreamName": "<stream>", "PlaybackMode": "ON_DEMAND", "HLSFragmentSelector": {"FragmentSelectorType": "PRODUCER_TIMESTAMP", "TimestampRange": {"StartTimestamp": T0, "EndTimestamp": T0+1000}}} and read the returned HLSStreamingSessionURL — it is https://<the host you reached>/hls/v1/getHLSMasterPlaylist.m3u8?SessionToken=<token>.
3. GET that URL against the same emulator: no service claims the path (`service.rs:100-113` matches only the six modelled first segments) and no handler exists for it, so the request is answered by the front door instead of by the master playlist the URL names.
4. Repeat with /getDASHStreamingSessionURL and the returned DASHStreamingSessionURL (/dash/v1/getMPDManifest.mpd?SessionToken=...) for the same result.
5. Vary ContainerFormat / DiscontinuityMode / DisplayFragmentTimestamp / MaxMediaPlaylistFragmentResults between two calls: the only difference observable through any API call is the opaque token in the URL, because the recorded session those members land in (`state.rs:957-973`) is never read by a handler.
