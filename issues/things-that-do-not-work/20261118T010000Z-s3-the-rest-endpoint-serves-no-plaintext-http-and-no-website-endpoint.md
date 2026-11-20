Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: s3.s3-ssec-and-endpoint-enforcement-ssec-request-over-plain-http-rejected s3.s3-ssec-and-endpoint-enforcement-website-endpoint-serving-an-ssec-object-rejected
Issue-Shape-Id: com.amazonaws.s3#AmazonS3
Issue-Key: s3.s3-ssec-and-endpoint-enforcement-ssec-request-over-plain-http-rejected|_http#14|BadStatusLine|with _OPENER.open(req, timeout=60) as resp:
Issue-Key: s3.s3-ssec-and-endpoint-enforcement-website-endpoint-serving-an-ssec-object-rejected|_website_base_url#12|CapabilityNotOffered|candidates = ctx.s3_website_urls(
Issue-Commit: 41bd6c476a5fe40a474aadc7e47c434277d677d7

# s3: the endpoint answers nothing over plaintext HTTP and publishes no website endpoint, so neither the SSE-C-over-HTTP refusal nor a website read of an SSE-C object can be observed

> _Note: these two causes were filed as causes 5 and 6 of
> `20261110T210333Z-s3-6-net-new-causes-in-the-sse.md`, whose other four causes (the SSE-C
> admission check on the write path, the copy-source customer key, the part refusal's wording
> and the UploadPartCopy scaffold) are fixed and whose report has moved to
> `issues/things-that-now-work/`. They are re-filed here, with their own replay keys, so the two
> that remain OUTSTANDING stay recorded as not working. Both need the same change to the
> emulator core, which is read-only from a service crate — see "Why this is not fixable from
> `services/`" below._

## Operation
GetObject

## Description
Cause 1 of 2: the S3 endpoint answers nothing over plaintext HTTP, so the SSE-C-over-HTTP refusal cannot exist.
AWS: recorded behavior. The REST endpoint serves plain `http://` requests: a presigned GET of a
non-SSE-C object over `http://` returns HTTP 200 with the object's bytes. A request carrying
SSE-C headers over `http://` is refused HTTP 400 `<Code>InvalidArgument</Code>` with the fixed
message "Requests specifying Server Side Encryption with Customer provided keys must be made
over a secure connection.", leaks no object bytes, and writes nothing (the PUT target reads back
`NoSuchKey`); the byte-identical request over `https://` succeeds. Transport alone decides — no
bucket policy is involved.
Emulator: the `http://` request receives no HTTP response at all; the client fails with
`http.client.BadStatusLine` because the first bytes of the reply are not a status line (the port
speaks only TLS). The non-SSE-C control GET over `http://` already fails this way, so the SSE-C
refusal is never reached.
Why: every plaintext client gets an unparseable protocol error instead of either the object or
S3's `<Error>` document, and no transport-sensitive behaviour (SSE-C over HTTP,
`aws:SecureTransport`) can be exercised.
Fix: serve plaintext HTTP on the data port the emulator publishes, then add the secure-transport
check to the SSE-C request validation in this service's crate.
Traces: s3.s3-ssec-and-endpoint-enforcement-ssec-request-over-plain-http-rejected.

Cause 2 of 2: no S3 website endpoint is published, so the website-endpoint refusal for an SSE-C object is unobservable.
AWS: recorded behavior. A bucket configured for website hosting is reachable at its website
endpoint; a GET of an object stored under SSE-C through that endpoint is refused (the website
endpoint cannot carry customer keys), while non-SSE-C keys are served normally.
Emulator: the run announces no website endpoint at all — the harness aborts with
`CapabilityNotOffered: the emulator offers no S3 website endpoint (the run names no website
endpoint template)`. The website SURFACE itself is implemented:
`services/service-com-amazonaws-s3-amazons3/src/service/website_endpoint.rs` answers a
`<bucket>.s3-website-<region>` Host, and `src/service.rs` routes such a request to it. What is
missing is an ADDRESS a client can dial it on: an AWS website endpoint is an `http://` host, and
the one address this emulator publishes is the TLS data port.
Why: the entire website-hosting surface, including this refusal, does not exist for a client:
static-site hosting cannot be exercised and the refusal that protects SSE-C objects from
anonymous website reads can never be seen.
Fix: publish and serve a website endpoint on an address the emulator answers on, then refuse an
SSE-C object's bytes on that surface.
Traces: s3.s3-ssec-and-endpoint-enforcement-website-endpoint-serving-an-ssec-object-rejected.

## Why this is not fixable from `services/`
Both causes need the emulator's one data port to answer a plaintext `http://` request, and that
port is owned by the read-only core:

- `vendor/server/src/lib.rs` serves it with `axum_server::from_tcp_rustls(data_listener,
  tls_config)` — a single TLS listener, with no sniffing of a plaintext preface.
- the same file computes exactly ONE published address for it ("the one published address of the
  data port: the shared config's `endpoint_url` and the base of every URL the service hands
  out"), so there is nowhere for a service crate to announce a second (website) host.
- a core test in that file asserts that a plaintext request to the data port is NOT served.

A service crate can neither change that listener nor add a published address, and a separate
loopback port does not help: the plaintext check dials the PUBLISHED endpoint with the scheme
swapped, and the website check dials the website template the run names. The core change needed
is a data port that accepts a plaintext HTTP connection (and an announced website host that
resolves to it); the service-side half — the secure-transport refusal and the website
endpoint's own SSE-C refusal — is small once a request reaches a handler.

## Evidence
Cause 1 — "File client.py, line 325, in _read_status / raise BadStatusLine(line) /
http.client.BadStatusLine:  2" on the plain-http presigned GET control; expected HTTP 200 for
the non-SSE-C object and 400 InvalidArgument "Requests specifying Server Side Encryption with
Customer provided keys must be made over a secure connection." for the SSE-C one.
Cause 2 — "simparity.behavioral.context.CapabilityNotOffered: the emulator offers no S3 website
endpoint (the run names no website endpoint template: s3_website_url=,
SIMPARITY_S3_WEBSITE_URL)".

## Steps to Reproduce
1. Presign a GetObject of a plain object, swap the scheme to `http://` and fetch it — expect
   HTTP 200 with the bytes; observe an unparseable reply (BadStatusLine). Then presign an SSE-C
   GET and fetch it over `http://` with the SSE-C headers — expect 400 InvalidArgument
   "Requests specifying Server Side Encryption with Customer provided keys must be made over a
   secure connection."
2. PutBucketWebsite, then resolve the bucket's website endpoint — expect a served website host;
   observe that no website endpoint is announced at all.

## Related reports
- `20261106T072814Z-iam-s3-a-plaintext-http-getobject-to-the.md` — the same plaintext-HTTP
  surface, under a different replay key.
- `20261113T120806Z-s3-the-emulator-publishes-no-s3-website-endpoint.md` and
  `20261117T213222Z-s3-4-net-new-causes-no-static-website.md` — the same unpublished website
  endpoint, under different replay keys.
