Issue-Source: trace-sweep-divergence
Issue-Trace: s3-multipart-part-sizes-and-composite-etag
Issue-Step: task#6
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: s3-multipart-part-sizes-and-composite-etag s3w-complete-multipart-composite-crc32-missing-part-checksum-rejected s3w-complete-multipart-sha256-two-part-missing-second-part-checksum-rejected
Issue-Shape-Id: com.amazonaws.s3#AmazonS3
Issue-Key: s3|ListParts|2xx->2xx|$.ListPartsResult.NextPartNumberMarker
Issue-Key: s3|ListParts|2xx->2xx|$.ListPartsResult.PartNumberMarker
Issue-Key: s3|CompleteMultipartUpload|4xx->2xx|$.Error
Issue-Key: s3|CompleteMultipartUpload|4xx->2xx|status
Issue-Key: s3|HeadObject|4xx->2xx|$headers.content-type
Issue-Key: s3|HeadObject|4xx->2xx|status
Issue-Key: s3|CompleteMultipartUpload|2xx->4xx|$.CompleteMultipartUploadResult
Issue-Key: s3|CompleteMultipartUpload|2xx->4xx|$headers.x-amz-server-side-encryption
Issue-Key: s3|CompleteMultipartUpload|2xx->4xx|status
Issue-Commit: 35e32963f89bfa2c86c6d7f7351ac2825588f64a

# s3 multipart upload plane: ListParts omits the pagination markers and CompleteMultipartUpload never enforces per-part checksums

> _Note: this issue was identified against the code base at commit `35e32963f89bfa2c86c6d7f7351ac2825588f64a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListParts

## Description
Six net-new causes in the multipart upload plane, found by replaying the wire corpus (trace set simparity-wire-aws-v1): ListParts does not serialize its pagination markers, and CompleteMultipartUpload does not enforce the per-part checksum a checksum-typed upload requires — which in turn corrupts the rest of the recorded sequence.

Cause 1 of 6: ListParts omits both PartNumberMarker and NextPartNumberMarker from the result.
AWS: Smithy (com.amazonaws.s3#AmazonS3) ListPartsOutput (xmlName ListPartsResult) carries PartNumberMarker and NextPartNumberMarker, and the operation is @paginated with inputToken PartNumberMarker / outputToken NextPartNumberMarker / pageSize MaxParts. S3 always emits PartNumberMarker (the marker the page started at, '0' when none was supplied) and, on a truncated page, NextPartNumberMarker (the part number to resume from).
Emulator: both elements are absent from ListPartsResult.
Why: an SDK paginator reads NextPartNumberMarker to continue; with the member missing it concludes the listing is complete and silently truncates to the first page, and a client echoing PartNumberMarker to show where a page began sees nothing.
Fix: the ListParts output serializer / its pagination state — emit PartNumberMarker for every page and NextPartNumberMarker whenever the page is truncated.
Traces: s3-multipart-part-sizes-and-composite-etag#task#6.

Cause 2 of 6: on a ListParts page requested with an explicit max-parts (truncated page), PartNumberMarker is still omitted.
AWS: PartNumberMarker is returned on every page, carrying '0' when the request supplied no part-number-marker.
Emulator: element absent.
Why: same as cause 1 — the page's starting marker is unreadable, so a resumed listing cannot be verified.
Fix: ListParts output serializer (marker emitted unconditionally, defaulting to 0).
Traces: s3-multipart-part-sizes-and-composite-etag#task#7.

Cause 3 of 6: on the page that follows a marker, NextPartNumberMarker is omitted.
AWS: when IsTruncated is true the result carries NextPartNumberMarker set to the last part number listed.
Emulator: element absent.
Why: pagination terminates one page early.
Fix: ListParts output serializer / truncation bookkeeping.
Traces: s3-multipart-part-sizes-and-composite-etag#task#8.

Cause 4 of 6: CompleteMultipartUpload accepts a complete request that omits a part's checksum on an upload created with a checksum algorithm.
AWS: when the upload was created with a checksum algorithm (COMPOSITE checksum type), every CompletedPart in the complete request must carry that part's checksum; a request missing one is refused 400 with code InvalidRequest and the message 'The upload was created using a <algorithm> checksum. The complete request must include the checksum for each part. It was missing for part <N> in the request.' — fixed text apart from the algorithm name (lower-case, from the CreateMultipartUpload request) and the part number (from the request). Smithy models CompleteMultipartUpload with the per-checksum headers and ChecksumType; the enforcement rule itself is recorded behavior (modelled errors: none).
Emulator: answers 200 and completes the upload.
Why: a client that relies on S3 to detect a dropped part checksum gets a silently assembled object whose composite checksum was never verified; the whole checksum-integrity contract of multipart upload is unenforced.
Fix: CompleteMultipartUpload's part-checksum validation, before the parts are assembled into object state.
Traces: s3w-complete-multipart-composite-crc32-missing-part-checksum-rejected#task#4 (cluster also covers s3w-complete-multipart-sha256-two-part-missing-second-part-checksum-rejected#task#5 — the same gap for a SHA256 upload).

Cause 5 of 6: because the refusal in cause 4 never happens, HeadObject answers 200 for a key that must not exist, with the wrong Content-Type.
AWS: the complete request having been refused, no object exists at the key, so HeadObject answers 404 (NoSuchKey) and the error response carries Content-Type application/xml.
Emulator: answers 200 with Content-Type 'binary/octet-stream' — it serves the object it should never have assembled.
Why: a client's negative check ('the rejected upload left nothing behind') passes against AWS and fails here, and an error response typed binary/octet-stream is not parseable as the XML error document S3 returns.
Fix: follows cause 4 (no object written on a refused complete); separately, the HeadObject/GetObject error path must serve application/xml for error bodies.
Traces: s3w-complete-multipart-composite-crc32-missing-part-checksum-rejected#task#5 (cluster also covers s3w-complete-multipart-sha256-two-part-missing-second-part-checksum-rejected#task#6).

Cause 6 of 6: the corrected complete request (all part checksums supplied) then fails 404 because the upload was already consumed.
AWS: after the malformed complete is refused the upload id is still live, so re-sending the complete request with every part checksum succeeds 200 and returns CompleteMultipartUploadResult with Location, Bucket, Key, ETag, the composite checksum (ChecksumCRC32 of the form '<base64>-<partCount>') and ChecksumType COMPOSITE, plus the x-amz-server-side-encryption response header for a bucket with default SSE-S3 (AES256). Smithy: CompleteMultipartUploadOutput (xmlName CompleteMultipartUploadResult) carries Location/Bucket/Key/ETag/Checksum*/ChecksumType and ServerSideEncryption as the x-amz-server-side-encryption header.
Emulator: answers 404 with no CompleteMultipartUploadResult and no x-amz-server-side-encryption header — the upload id no longer exists, having been consumed by the request that should have been refused.
Why: the documented retry-after-refusal path is unavailable, and the composite-checksum/SSE echo a client reads from the successful completion is never produced.
Fix: CompleteMultipartUpload must leave the upload intact when it refuses (cause 4) and must emit the full result document and SSE header on success.
Traces: s3w-complete-multipart-composite-crc32-missing-part-checksum-rejected#task#6 (cluster also covers s3w-complete-multipart-sha256-two-part-missing-second-part-checksum-rejected#task#7).

## Evidence
Cause 1 — ListParts: $.ListPartsResult.PartNumberMarker expected '0', got None; $.ListPartsResult.NextPartNumberMarker expected '2', got None.
Cause 2 — ListParts (explicit max-parts page): $.ListPartsResult.PartNumberMarker expected '0', got None.
Cause 3 — ListParts (page after a marker): $.ListPartsResult.NextPartNumberMarker expected '2', got None.
Cause 4 — CompleteMultipartUpload with part 1's checksum omitted: expected status 400 with body $.Error = {'Code': 'InvalidRequest', 'Message': 'The upload was created using a crc32 checksum. The complete request must include the checksum for each part. It was missing for part 1 in the request.', ...}; actual status 200 and no Error element. Fixed text = 'The upload was created using a ... checksum. The complete request must include the checksum for each part. It was missing for part ... in the request.'; variable parts = the upload's checksum algorithm and the offending part number.
Cause 5 — HeadObject on the key of the refused upload: expected status 404 and $headers.content-type 'application/xml'; actual status 200 and content-type 'binary/octet-stream'.
Cause 6 — CompleteMultipartUpload re-sent with all part checksums: expected status 200 with CompleteMultipartUploadResult {Location, Bucket, Key, ETag, ChecksumCRC32 '<base64>-<parts>', ChecksumType 'COMPOSITE'} and header x-amz-server-side-encryption 'AES256'; actual status 404, result document missing, header missing. (Bucket/key/ETag values in the recording are per-run.)

## Steps to Reproduce
Causes 1-3: CreateMultipartUpload, UploadPart twice, then ListParts — once with no part-number-marker, once with max-parts=1, once with part-number-marker set — and check ListPartsResult for PartNumberMarker / NextPartNumberMarker. Replay fixture s3-multipart-part-sizes-and-composite-etag (task#6, task#7, task#8).
Causes 4-6: CreateMultipartUpload with ChecksumAlgorithm CRC32 (and separately SHA256), UploadPart with the per-part checksum, then CompleteMultipartUpload omitting one part's checksum (must be 400 InvalidRequest), HeadObject the key (must be 404, Content-Type application/xml), then re-send CompleteMultipartUpload with all part checksums (must be 200 with the composite checksum and the SSE header). Replay fixtures s3w-complete-multipart-composite-crc32-missing-part-checksum-rejected (task#4-#6) and s3w-complete-multipart-sha256-two-part-missing-second-part-checksum-rejected (task#5-#7).
