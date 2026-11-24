Issue-Source: trace-sweep-divergence
Issue-Trace: dms-s3-full-load-task-migrates-csv-rows
Issue-Step: task#25
Issue-Severity: LOW
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: dms-s3-full-load-task-migrates-csv-rows
Issue-Shape-Id: com.amazonaws.databasemigrationservice#AmazonDMSv20160101
Issue-Key: s3|GetObject|2xx->2xx|$headers.content-type
Issue-Commit: b304949cbcbc21b926a1298c720fbfae51fb7fc9

# s3 GetObject serves Content-Type binary/octet-stream for an untyped service-written object instead of application/octet-stream

> _Note: this issue was identified against the code base at commit `b304949cbcbc21b926a1298c720fbfae51fb7fc9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetObject

## Description
One cause (Cause 1 of 1).

Cause 1 of 1: GetObject announces the wrong default MIME type for an object stored without an explicit Content-Type.

AWS: Smithy: GetObject -> GetObjectOutput.ContentType, bound to the HTTP response header `Content-Type`, documented only as "A standard MIME type describing the format of the object data." The model states no default, so this is recorded behavior only: when an object is written with no caller-supplied Content-Type (here the CSV object a DMS full-load task writes into its S3 target bucket), AWS answers the subsequent GetObject with HTTP 200 and `Content-Type: application/octet-stream`. That is the generic-binary MIME type S3 reports for an untyped object on the REST data plane.

Emulator: the same GetObject answers HTTP 200 but with `Content-Type: binary/octet-stream`. The status, body and every other asserted field match; only the header value diverges. `binary/octet-stream` is the legacy S3-console upload default, not a registered MIME type and not what the data plane reports for an untyped object. Hypothesis: the emulator's object store seeds a literal `binary/octet-stream` as its fallback content type (either where a service-side writer such as the DMS S3 target persists object metadata, or where the GetObject/HeadObject response serializer fills in a missing stored value).

Why: a client that branches on the response Content-Type -- an SDK transfer manager choosing a decoder, a browser or CDN deciding whether to render or download, a downstream pipeline that whitelists `application/octet-stream` for opaque payloads -- sees an unregistered type it does not recognise. Code that round-trips the header (CopyObject, a re-upload, a cache entry, a signed-response comparison) also propagates the wrong value, so the divergence spreads past the single read. Fidelity tests that assert the documented generic-binary type fail against the emulator while passing against AWS.

Fix: the s3 object store's default/fallback content type and the GetObject (and HeadObject, which shares the binding) response serializer: when no Content-Type was supplied at write time, the stored and served value must be `application/octet-stream`. An explicitly supplied Content-Type must still be echoed verbatim and must not be rewritten to the default.

Message/header rule: the fixed requirement is the header NAME `Content-Type` and the literal default value `application/octet-stream`; there is no variable part in this default. When the writer did supply a type, the value is a variable taken from the request. The recorded object key, bucket name and request id in this fixture are per-run values and are not part of the requirement.

Traces: dms-s3-full-load-task-migrates-csv-rows#task#25 (GetObject, expected 200/`application/octet-stream`, got 200/`binary/octet-stream`).

## Evidence
Fixture dms-s3-full-load-task-migrates-csv-rows, step task#25, service s3, operation GetObject.

Expected (recorded AWS): status 200, header `content-type: application/octet-stream`.
Actual (emulator):       status 200, header `content-type: binary/octet-stream`.

Replay detail, verbatim:
  $headers.content-type: value_mismatch (expected 'application/octet-stream', got 'binary/octet-stream')

Divergence key: service=s3|GetObject|status=200->200|keys=$headers.content-type
Verdict: mismatch. affected_count: 1 (dms-s3-full-load-task-migrates-csv-rows#task#25). No stub_path, no owning open issue.

Smithy evidence: GetObjectOutput.ContentType is documented "A standard MIME type describing the format of the object data." and carries no modeled default, so the required default value comes from the recording: application/octet-stream.

Searched the open-issue set for an s3 Content-Type owner: the existing content-type issues cover docdb-elastic, geo-maps, kinesis-video-archived-media, neptunedata, appsync, sagemaker-runtime, connect-campaigns, ecr-public, cloudfront-edge responses and a DynamoDB error reply; none of them covers the s3 GetObject default MIME type, and none of the open s3 issues (e.g. 20261117T143723Z-s3-8-net-new-data-plane-and-validation.md) names this header.

## Steps to Reproduce
1. Clone the repo and boot the emulator built from it.
2. Replay wire fixture `dms-s3-full-load-task-migrates-csv-rows` and let it run through step `task#25`.
   Equivalently, by hand:
   a. Create an S3 bucket.
   b. Have an object written into it WITHOUT a caller-supplied Content-Type (in the fixture, a DMS full-load replication task writes its CSV output to the S3 target bucket; a plain PutObject that omits the Content-Type header reproduces the same path).
   c. GET that object: `aws s3api get-object --bucket <bucket> --key <key> /tmp/out --debug`, or issue a signed `GET /<bucket>/<key>` and inspect the response headers.
3. Read the `Content-Type` response header.
   Expected: `application/octet-stream` (status 200).
   Observed: `binary/octet-stream` (status 200).
4. Repeat with an explicit `--content-type text/csv` at write time to confirm an explicitly supplied type is still echoed verbatim and only the no-type-supplied default is wrong.
