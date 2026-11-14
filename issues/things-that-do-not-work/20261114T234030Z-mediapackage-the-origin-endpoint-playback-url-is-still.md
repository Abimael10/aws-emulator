Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: mediapackage.ingest-endpoint-wrong-credentials-rejected
Issue-Shape-Id: com.amazonaws.mediapackage#MediaPackage
Issue-Key: mediapackage.ingest-endpoint-wrong-credentials-rejected|run#21|PublishedUrlRejected|target_url, route_headers = ctx.endpoint_route(oe["Url"])
Issue-Commit: 5bc7b474c3edd4c3c4a63267d20aa9334a54ae6b

# mediapackage: the origin-endpoint playback Url is still minted on an AWS-owned host, so the ingest-credentials check cannot read its destination

> _Note: this issue was identified against the code base at commit `5bc7b474c3edd4c3c4a63267d20aa9334a54ae6b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateOriginEndpoint

## Description
ONE cause.

Cause 1 of 1: CreateOriginEndpoint returns an OriginEndpoint.Url whose HOST is a public AWS hostname (`*.mediapackage.<region>.amazonaws.com`) rather than a host the emulator answers on, so a client handed that URL leaves the emulator entirely.

AWS: Smithy mediapackage CreateOriginEndpoint -> POST /origin_endpoints, HTTP 200; output member Url (jsonName "url", target __string) is documented as the URL of the packaged OriginEndpoint for consumption, i.e. the address a player GETs the manifest from, and DescribeOriginEndpoint/ListOriginEndpoints re-report the same value. The ingest/egress wire protocol itself is not modeled, so for the HOST component this is recorded behavior only: the recordings root the advertised URL at the endpoint the client is talking to. The `/out/v1/<endpoint label>/<manifestName>.m3u8` path shape and the generated labels are NOT the requirement; a servable host is. For reference, the sibling control-plane calls in the same check (CreateChannel 200, DescribeChannel 200, the HLS ingest Url and its Username/Password) all pass, so the account, channel and credential minting are healthy and only the advertised playback host is at fault.

Emulator: CreateOriginEndpoint(ChannelId=<channel>, Id=<channel>-oe, HlsPackage={}) answers 200 with Url = `https://<label>.mediapackage.us-west-2.amazonaws.com/out/v1/<endpoint label>/index.m3u8`. The replay harness refuses to dereference it: `simparity.published.PublishedUrlRejected: endpoint_url was given https://<label>.mediapackage.<region>.amazonaws.com/out/v1/<label>/index.m3u8 (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:<port>/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener` (context.py `endpoint_route` -> `_published_target`). The host and port in that text are this run's values; the defect is the `*.mediapackage.<region>.amazonaws.com` host class.

Why: a player or a test given the advertised manifest URL either reaches real AWS or fails name resolution offline, and the 200 on the control-plane call gives it no hint the address is unusable. Concretely for this check, the independent read at the destination — proving that the blanket-403 ingest PUTs landed nothing, i.e. that an origin endpoint on a never-ingested channel answers 404 — can never be performed, so the ingest-credential behavior it guards (correct and wrong Basic-auth credentials drawing the SAME 403 from a closed push listener) is left unverifiable.

Fix: the CreateOriginEndpoint handler's playback-URL minting and the origin-endpoint state record that persists it, plus the describe/list/update serializers that re-serve the stored value; derive the host from the endpoint the request arrived on (at read time too, so records minted before the fix stop serving the public-AWS host).

NOT covered by an existing issue, named here for the fixer: open issue 20261109T160342Z-mediapackage-ingest-and-playback-urls-handed-to-data.md describes the same host defect and OWNS the replay keys of the three origin-endpoint checks and two ingest checks, but its key for THIS check is `mediapackage.ingest-endpoint-wrong-credentials-rejected|run#10|AssertionError|assert not (p := ctx.published_url_mismatch(url)), p` — the ingest-URL assertion, which now passes. This check has moved on to failing at a different site (`endpoint_route` on the origin-endpoint Url) and is therefore not owned. Also related but not covering: 20261109T142008Z-mediapackage-origin-endpoint-playback-url-advertises-an-aws-owned.md owns only wire replay keys, and 20261107T231715Z-mediapackage-origin-endpoint-playback-url-is-unrouted-404.md is about what the URL answers once it is reachable.

Traces: mediapackage.ingest-endpoint-wrong-credentials-rejected.

## Evidence
Suite simparity-behavioral-aws-v1: 54/60 passed; per-service line `mediapackage 54/58 (93%) 4 failed`. Three of the four mediapackage failures and both cloudformation__mediapackage failures are marked OWNED by an open issue and were skipped; this check is NOT marked ALREADY FILED or OWNED.

[mismatch] mediapackage.ingest-endpoint-wrong-credentials-rejected
  File "context.py", line 603, in endpoint_route -> line 617 in _published_target:
  simparity.published.PublishedUrlRejected: endpoint_url was given https://be07297cf53abd5e9f21bf454fd80cd7.mediapackage.us-west-2.amazonaws.com/out/v1/7992b88615eebcbc5e40b2ab5b88fa9b/index.m3u8 (published by the emulator or built by the check): its host is not one of the emulator's own hosts; a customer of the emulator needs http(s)://127.0.0.1:41269/<resource path>, or http(s)://<own host>:<port>/<path> on a per-resource listener

The rejected value is `oe["Url"]` from `mp.create_origin_endpoint(ChannelId=cid, Id=cid+"-oe", HlsPackage={}, Tags={"sim2real": "true"})` (check source lines 649-652). Everything before it in the same check succeeded: CreateChannel 200 with usable HlsIngest Username/Password, `ctx.published_url_mismatch(<ingest url>)` clean, DescribeChannel echoing the same Id and ingest Username, and both Basic-auth PUTs to the ingest URL answering the same 403 body for wrong and correct passwords. The divergence is confined to the HOST component of the advertised playback URL.

The generated labels (`be07297c…`, `7992b886…`), the region and the port 41269 are one run's values and are not part of the requirement; `*.mediapackage.<region>.amazonaws.com` as the host class is the defect.

Model evidence: services/service-com-amazonaws-mediapackage-mediapackage/smithy-model/com-amazonaws-mediapackage-mediapackage.json — CreateOriginEndpoint POST /origin_endpoints code 200, output member Url (jsonName "url") documented as the URL of the packaged OriginEndpoint for consumption; errors ForbiddenException 403, NotFoundException 404, UnprocessableEntityException 422, TooManyRequestsException 429, InternalServerErrorException 500, ServiceUnavailableException 503 (none of them raised here — the call succeeded with 200).

## Steps to Reproduce
1. Build and boot the emulator from this checkout and replay the simparity-behavioral-aws-v1 suite; mediapackage.ingest-endpoint-wrong-credentials-rejected fails with PublishedUrlRejected on the origin-endpoint Url.
2. Reproduce directly with boto3 pointed at the emulator (region us-west-2):
   a. ch = mp.create_channel(Id="ch1", Tags={"sim2real": "true"})  -> 200.
   b. oe = mp.create_origin_endpoint(ChannelId="ch1", Id="ch1-oe", HlsPackage={}, Tags={"sim2real": "true"})  -> 200; read oe["Url"].
   c. Observed: host `<label>.mediapackage.us-west-2.amazonaws.com` (path `/out/v1/<label>/index.m3u8`). Expected: a host the emulator answers on, derived from the endpoint the request arrived on (e.g. 127.0.0.1:<port>, or <own host>:<port> on a per-resource listener).
   d. HTTP GET of oe["Url"] never reaches the emulator (name resolution failure offline, or real AWS online). Once the host is emulator-served, the GET on a channel that never accepted an ingest PUT must answer HTTP 404.
   e. mp.describe_origin_endpoint(Id="ch1-oe")["Url"] and the Url in mp.list_origin_endpoints() re-report the same unservable host, so the stored record and the read serializers must be fixed too.
3. Pass criterion: every OriginEndpoint.Url (create, describe, list, update, and records minted before the fix) carries a host the emulator answers on, so a GET of the advertised manifest reaches the emulator and answers 404 before any ingest; generated labels and ports may differ freely, but the `/out/v1/<endpoint label>/<manifestName>.m3u8` path shape and its per-resource uniqueness must be preserved. Then re-replay mediapackage.ingest-endpoint-wrong-credentials-rejected, which must get past the destination read and assert the blanket-403 ingest behavior.
