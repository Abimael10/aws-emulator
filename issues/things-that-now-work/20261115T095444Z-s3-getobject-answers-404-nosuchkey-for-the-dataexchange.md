Issue-Source: trace-sweep-divergence
Issue-Trace: dataexchange-import-from-s3-then-export-round-trips-asset
Issue-Step: task#12
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: dataexchange-import-from-s3-then-export-round-trips-asset
Issue-Shape-Id: com.amazonaws.dataexchange#DataExchange
Issue-Key: s3|GetObject|2xx->4xx|$headers.x-amz-checksum-crc32
Issue-Key: s3|GetObject|2xx->4xx|$headers.x-amz-checksum-type
Issue-Related: 20261115T034316Z-s3-getobject-answers-404-nosuchkey-for-the-dms.md
Issue-Commit: 5d93654b0bd14cd350333b75279398fadc574ee6

# s3 GetObject answers 404 NoSuchKey for the DataExchange export destination object

> _Note: this issue was identified against the code base at commit `5d93654b0bd14cd350333b75279398fadc574ee6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetObject

## Description
One cause: the object a DataExchange EXPORT_ASSETS_TO_S3 job is supposed to write into its destination bucket never exists in the S3 object store, so the read-back of that key is refused 404 instead of serving the exported asset bytes with the object's metadata headers.

Cause 1 of 1: GetObject of the export destination key answers 404 NoSuchKey instead of 200 with the exported object.

AWS: Smithy (com.amazonaws.s3#AmazonS3): GetObject -> GET /{Bucket}/{Key+}, success http code 200, modeled errors NoSuchKey (404) and InvalidObjectState (403). Its output binds Body as the httpPayload and the object's metadata to response headers: AcceptRanges -> accept-ranges, LastModified -> Last-Modified, ETag -> ETag, ContentType -> Content-Type, ChecksumCRC32 -> x-amz-checksum-crc32, ChecksumType -> x-amz-checksum-type, ServerSideEncryption -> x-amz-server-side-encryption. Recorded behavior only for the end-to-end rule: on AWS a DataExchange export job whose Details name a destination bucket/key actually writes the asset's bytes under that key, so the subsequent GetObject of that key returns 200, Body = the exported asset's bytes (byte-for-byte the imported source object), accept-ranges 'bytes', Content-Type = the stored object's content type for the exported asset (a CSV asset is served as 'text/csv', not a generic fallback), x-amz-server-side-encryption 'AES256' for a default-encrypted bucket, x-amz-checksum-type 'FULL_OBJECT' with the matching x-amz-checksum-* header for the algorithm the object was stored with, plus ETag and Last-Modified. The ETag, Last-Modified value, the concrete checksum digest and the object bytes' length are per-run/per-environment values, not requirements; what is required is that the key EXISTS and that these headers are present and describe the stored object.

Emulator: the request is answered HTTP 404 with an S3 error envelope - Code 'NoSuchKey', Message 'The specified key does not exist.', plus Key (the requested key), RequestId and HostId - and Content-Type 'application/xml'. No object body is served and accept-ranges, last-modified, etag, x-amz-checksum-crc32, x-amz-checksum-type and x-amz-server-side-encryption are all absent. The error's fixed text ('The specified key does not exist.') and its 404 status are AWS's own wording for a genuinely missing key, and the Key member correctly echoes the requested key - the defect is NOT the message, it is that the key is missing at all: nothing ever wrote the export destination object, so a read AWS serves 200 is refused. (The requested key, the request id and the host id in the recording are that run's values and are not requirements.)

Why: a client that imports data into a DataExchange revision, exports it to S3 and then reads the exported object back gets a hard 404 at the last step. The round trip silently loses the data instead of failing where it broke, every reader keyed on the exported object (a downstream ETL, a checksum verification, a 'wait for the export then process it' loop) breaks, and code that treats NoSuchKey as 'the export has not finished yet' retries forever.

Fix: the DataExchange export-job executor's write into the S3 object store - a completing EXPORT_ASSETS_TO_S3 job must materialize one S3 object per exported asset at the bucket/key from the job's Details, carrying the asset's bytes, a content type derived from the asset and the bucket's default encryption, so S3's own GetObject handler then finds it. S3's GetObject handler/serializer is the reporting side here: once the object exists it must emit the stored object's Content-Type (not application/xml or a generic fallback) together with accept-ranges, ETag, Last-Modified, the x-amz-checksum-* / x-amz-checksum-type pair and x-amz-server-side-encryption.

Related open issues (neither covers this step): 20261110T013717Z-dataexchange-import-assets-from-s3-job-fails-instead.md owns the upstream import failure at task#7/task#8 of this same fixture and only names those two steps as its replay keys; 20261102T111455Z-s3-scalar-parameter-refusals-leak-smithy-coercion-wording.md is about GetObject refusals caused by query/header parameter coercion and does not carry this fixture or step. The replay reports this cluster with owned_by empty and no stub, so it is net-new.

Traces: dataexchange-import-from-s3-then-export-round-trips-asset#task#12.

## Evidence
Replay of fixture dataexchange-import-from-s3-then-export-round-trips-asset (trace set simparity-wire-aws-v1), step task#12, GetObject.
divergence_key: service=s3|GetObject|status=200->404|keys=$,$headers.accept-ranges,$headers.content-type,$headers.etag,$headers.last-modified,$headers.x-amz-checksum-crc32,$headers.x-amz-checksum-type,$headers.x-amz-server-side-encryption,status
verdict: mismatch; expected_status 200, actual_status 404; owned_by: [] ; stub_path: ""

detail, verbatim:
  status: expected 200, got 404
  $: type_mismatch (expected 'order_id,sku,qty\n1001,A-17,3\n1002,B-42,1\n1003,A-17,5\n', got {'Error': {'Code': 'NoSuchKey', 'Message': 'The specified key does not exist.', 'Key': 'export/orders.csv', 'RequestId': '0000LX6PF4F2CUCE', 'HostId': 'evVUTffH3Zjo+iX/yHkp75mHzzro1o/xR0Uz7ejLeAQDaL1OjrrK/d8MjYnPU0hd'}})
  $headers.accept-ranges: missing (expected 'bytes', got None)
  $headers.last-modified: missing (expected '<shaped-not-asserted>', got None)
  $headers.etag: missing (expected '<shaped-not-asserted>', got None)
  $headers.x-amz-checksum-crc32: missing (expected '0xW3Yw==', got None)
  $headers.x-amz-checksum-type: missing (expected 'FULL_OBJECT', got None)
  $headers.content-type: value_mismatch (expected 'text/csv', got 'application/xml')
  $headers.x-amz-server-side-encryption: missing (expected 'AES256', got None)

Expected: HTTP 200 with the exported asset's bytes as the body, accept-ranges 'bytes', Content-Type 'text/csv' for the exported CSV asset, x-amz-checksum-type 'FULL_OBJECT' with its matching x-amz-checksum-crc32 digest, x-amz-server-side-encryption 'AES256', and ETag / Last-Modified present. The CSV text, the CRC32 digest value, the ETag and the Last-Modified value in the recording are that run's values (shape-asserted only for last-modified/etag); the requirement is the 200 with a body and the presence of those headers describing the stored object.
Actual:   HTTP 404, body an S3 error envelope with Code 'NoSuchKey' and Message 'The specified key does not exist.', Content-Type 'application/xml', and none of the object headers. The key was never written by the preceding DataExchange export job.

Model references used: GetObject operation signature (http binding GET /{Bucket}/{Key+}, code 200; errors NoSuchKey 404 / InvalidObjectState 403) and its output header bindings AcceptRanges, LastModified, ETag, ContentType, ChecksumCRC32, ChecksumType, ServerSideEncryption, Body(httpPayload/streaming), plus PutObject's corresponding checksum/SSE output bindings, from the repo's com.amazonaws.s3#AmazonS3 Smithy model.

## Steps to Reproduce
1. git_clone() the repo and boot the emulator built from it.
2. Replay the wire corpus (replay_wire_corpus) for trace set simparity-wire-aws-v1 and look at fixture dataexchange-import-from-s3-then-export-round-trips-asset.
3. The fixture imports an S3 object into a DataExchange revision, then creates and starts an EXPORT_ASSETS_TO_S3 job that writes the asset to a destination bucket/key, then at step task#12 issues GET /{bucket}/{export key} (s3 GetObject) for that destination key.
4. Observe the response is HTTP 404 with Code 'NoSuchKey' / Message 'The specified key does not exist.' and Content-Type application/xml, instead of 200 with the exported bytes - the destination object was never created by the export job.
5. After a fix, re-replay step task#12: it must answer 200 with the exported asset's bytes, accept-ranges 'bytes', the stored object's Content-Type (text/csv for this CSV asset), x-amz-checksum-type FULL_OBJECT with its x-amz-checksum-crc32 digest, x-amz-server-side-encryption AES256, and ETag / Last-Modified present.
