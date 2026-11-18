Issue-Source: trace-sweep-divergence
Issue-Trace: emr-run-job-flow-step-writes-s3-output
Issue-Step: task#24
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: emr-run-job-flow-step-writes-s3-output
Issue-Shape-Id: com.amazonaws.emr#ElasticMapReduce
Issue-Key: s3|GetObject|2xx->2xx|$headers.content-type
Issue-Related: 20261113T191419Z-s3-getobject-serves-an-emr-step-s-s3.md
Issue-Commit: cb1fe1cd01dea9b9b76399bb5958c78b62c26090

# s3 GetObject omits the CRC64NVME checksum header and reports the generic content type for a service-written output object

> _Note: this issue was identified against the code base at commit `cb1fe1cd01dea9b9b76399bb5958c78b62c26090`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetObject

## Description
One root cause, grounded in the single unowned wire divergence cluster of this replay (the replay report lists owned_by as empty for it).

=== Cause 1 of 1: an object materialized by a service write path (here an EMR step's S3 output) is stored with neither a recorded content type nor a full-object checksum, so GetObject answers 200 but reports Content-Type 'application/octet-stream' and emits no x-amz-checksum-crc64nvme ===

AWS: Smithy (services/service-com-amazonaws-s3-amazons3/smithy-model/com-amazonaws-s3-amazons3.json) models GetObject as http {method: GET, uri: /{Bucket}/{Key+}?x-id=GetObject, code: 200} with the object bytes on the httpPayload streaming member Body, and binds response headers ContentType -> httpHeader 'Content-Type' and ChecksumCRC64NVME -> httpHeader 'x-amz-checksum-crc64nvme' (also ChecksumType -> 'x-amz-checksum-type'); the operation carries httpChecksum {requestValidationModeMember: ChecksumMode, responseAlgorithms: [CRC64NVME, CRC32, CRC32C, SHA256, SHA1, SHA512, MD5, XXHASH64, XXHASH3, XXHASH128]}. A GET therefore reports the content type S3 RECORDED for the object at store time and the full-object checksum S3 stored for it. For an upload that supplies no Content-Type — which is how a Hadoop/Spark step's output part files are written — S3's recorded content type is 'binary/octet-stream'; that string is S3's own fixed default, not a value from this run and not the generic HTTP fallback 'application/octet-stream'. S3 also computes a CRC64NVME full-object checksum by default for such an upload and returns it on x-amz-checksum-crc64nvme. The header's VALUE is derived from the stored bytes (the recording happens to show 'exstQHcXHaI=' for its bytes); the requirement is that the header is PRESENT and computed from the stored object, never that it equals that one literal.

Emulator: the GET answers 200 with the correct bytes and every other asserted header matching, but Content-Type comes back as the generic fallback 'application/octet-stream' instead of S3's default 'binary/octet-stream', and x-amz-checksum-crc64nvme is absent entirely even though a CRC64NVME is computable from the stored bytes.

Why: a client that reads a job's output out of S3 keys off exactly these headers. Content-type-driven routing or sniffing mis-classifies the object because it sees a value S3 never reports for a default upload, and a reader that sets x-amz-checksum-mode: ENABLED to verify the integrity of the step's output has no checksum to verify against, so verification either errors or silently degrades to no integrity check — the very guarantee S3's default-checksum behaviour is relied on for.

Fix: S3's object state record and the write path that stores an object with no explicit Content-Type (it must record S3's default 'binary/octet-stream' rather than leaving the field unset for the response serializer to fall back on), plus that write path's checksum computation so a CRC64NVME full-object checksum is stored for the upload. GetObject's response serializer then has values to bind to Content-Type and x-amz-checksum-crc64nvme and needs no change beyond emitting headers for metadata that is present. The EMR step-execution path that materializes the output object must go through that same S3 write path.

Related open issues, none of which the replay report records as owning this cluster: 20261113T191419Z-s3-getobject-serves-an-emr-step-s-s3.md describes this same symptom for this same fixture step but is NOT registered as its owner (its Issue-Key names only the checksum header, not the content-type key, so the cluster replays as unowned and the content-type half is un-gated); 20261113T174343Z-s3-getobject-of-the-appflow-written-destination-object.md covers an AppFlow-written destination; 20261115T160652Z-s3-listobjectsv2-reports-checksumalgorithm-crc32-for-a-service.md covers the listing-side checksum-algorithm report rather than GetObject's response headers. A fixer should close this cluster against both asserted header keys.

Traces: emr-run-job-flow-step-writes-s3-output#task#24

## Evidence
Fixture emr-run-job-flow-step-writes-s3-output, step task#24, service s3, operation GetObject, verdict mismatch, owned_by [] (unowned), stub_path empty. Expected status 200, actual status 200 — status and body match, response headers diverge.

divergence_key: service=s3|GetObject|status=200->200|keys=$headers.content-type,$headers.x-amz-checksum-crc64nvme

  $headers.content-type: value_mismatch (expected 'binary/octet-stream', got 'application/octet-stream')
  $headers.x-amz-checksum-crc64nvme: missing (expected a CRC64NVME value — the recording shows 'exstQHcXHaI=' for these bytes; the requirement is the header's presence and derivation from the stored object, not that literal)

Message/value classification: 'binary/octet-stream' is FIXED text (S3's default recorded content type for an upload with no Content-Type). The checksum header's value is VARIABLE — derived from the stored object's bytes.

Model references: GetObject http {GET, /{Bucket}/{Key+}?x-id=GetObject, 200}; Body = httpPayload streaming blob; ContentType -> httpHeader Content-Type; ChecksumCRC64NVME -> httpHeader x-amz-checksum-crc64nvme; ChecksumType -> httpHeader x-amz-checksum-type; httpChecksum {requestValidationModeMember: ChecksumMode, responseAlgorithms: [CRC64NVME, CRC32, ...]}; errors = [InvalidObjectState(403), NoSuchKey(404)].

## Steps to Reproduce
1. Replay wire fixture emr-run-job-flow-step-writes-s3-output and inspect step task#24.

Manually: create an S3 bucket for job output; RunJobFlow a cluster whose single step writes its output to an s3:// prefix in that bucket (no explicit Content-Type on the write); poll DescribeStep/ListSteps until the step is terminal; then GetObject the written output key. Observe HTTP 200 with the correct bytes but 'Content-Type: application/octet-stream' and no 'x-amz-checksum-crc64nvme' response header, instead of S3's default 'binary/octet-stream' plus the object's stored CRC64NVME full-object checksum. Re-issue the GET with 'x-amz-checksum-mode: ENABLED' to confirm no checksum is available for verification.
