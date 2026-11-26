Issue-Source: trace-sweep-divergence
Issue-Trace: bedrock-data-automation-runtime-async-document-extraction-writes-text
Issue-Step: task#8
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: bedrock-data-automation-runtime-async-document-extraction-writes-text
Issue-Shape-Id: com.amazonaws.bedrockdataautomationruntime#AmazonBedrockKeystoneRuntimeService
Issue-Key: s3|GetObject|2xx->4xx|$.document
Issue-Key: s3|GetObject|2xx->4xx|$.metadata
Issue-Related: 20261115T034316Z-s3-getobject-answers-404-nosuchkey-for-the-dms.md
Issue-Related: 20261115T095444Z-s3-getobject-answers-404-nosuchkey-for-the-dataexchange.md
Issue-Commit: e79672e854f5973dd19ff8b089aaca728dc35125

# s3 GetObject answers 404 for the Bedrock Data Automation async document-extraction standard_output result object

> _Note: this issue was identified against the code base at commit `e79672e854f5973dd19ff8b089aaca728dc35125`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetObject

## Description
Cause 1 of 1: the standard-output object that a COMPLETED Bedrock Data Automation Runtime asynchronous document-extraction job reports under its output S3 configuration is never present in the bucket, so reading it back with s3 GetObject fails instead of returning the extraction result.

AWS: recorded behavior only for the job's S3 side effect; the model grounds the read-back. Smithy (s3): GetObject http GET /{Bucket}/{Key+}, success code 200, errors NoSuchKey (httpError 404) and InvalidObjectState (403); output members Body (httpPayload, streaming), AcceptRanges (accept-ranges), LastModified (Last-Modified), ETag, ChecksumCRC32 (x-amz-checksum-crc32), ChecksumType (x-amz-checksum-type), ContentType (Content-Type), ServerSideEncryption (x-amz-server-side-encryption). The recording shows that once the asynchronous invocation reaches a terminal success status, the output prefix of the configured output bucket holds the per-asset standard-output document under <outputPrefix>/<invocationId>/<assetIndex>/standard_output/<subdocumentIndex>/result.json, and GetObject on that key answers 200 with the extracted-document JSON — a `metadata` object (asset_id, logical_subdocument_id, semantic_modality 'DOCUMENT', s3_bucket, s3_key of the input asset, number_of_pages, start_page_index, end_page_index, file_type 'PDF') and a `document` object whose representation.text carries the text extracted from the input PDF — plus accept-ranges 'bytes', Last-Modified, ETag, a CRC32 checksum with x-amz-checksum-type 'FULL_OBJECT', Content-Type binary/octet-stream and x-amz-server-side-encryption 'AES256'. The bucket name, the generated invocation id in the key, the ETag/Last-Modified values, the page counts and the extracted text string are per-run values and are NOT the requirement; the requirement is that the key the job's own output configuration designates is readable with 200 and the extracted-document shape above.

Emulator: GetObject on that key answers 404 with an XML error envelope (Content-Type 'application/xml' where AWS serves 'binary/octet-stream'), no body members at all ($.metadata and $.document both missing) and none of the object-metadata headers (accept-ranges, last-modified, etag, x-amz-checksum-crc32, x-amz-checksum-type, x-amz-server-side-encryption all absent). The object genuinely is not there: the preceding ListObjectsV2 on the same output prefix in the same fixture also returns a listing with no Contents entry for it. So the s3 GetObject handler is reporting a real absence (its 404/NoSuchKey classification for a missing key is correct); what is missing is the Bedrock Data Automation Runtime asynchronous job's S3 side effect — the job settles as finished without ever materializing its standard_output result.json.

Why: a client that submits an asynchronous data-automation job and then does the only thing the API allows — poll until the job reports success, then read the output S3 URI the job reports — gets 404 NoSuchKey forever. The job reports success, so there is no error to handle, and the extraction result is simply unreachable; every downstream consumer of the output prefix (S3 event notifications, Glue/Athena over the output prefix, a Lambda that parses result.json) sees an empty prefix.

Fix: the bedrock-data-automation-runtime asynchronous invocation completion path — the job-completion logic in services/service-com-amazonaws-bedrockdataautomationruntime-amazonbedrockkeystoneruntimeservice/src/ops (and its state) must write the per-asset standard_output result.json into the configured output bucket/prefix, with the extracted-document body shape and the object metadata (CRC32 full-object checksum, SSE AES256, untyped/binary content type) an S3 PutObject from a service produces, before the job is reported terminal. The s3 GetObject handler itself needs no change for this cause.

Traces: bedrock-data-automation-runtime-async-document-extraction-writes-text#task#8. The replay reported this cluster with an EMPTY owned_by and no blocking stub, so no open issue carries its replay key. Related but NON-covering open issues (same symptom, different producing service, so none of them covers this one): 20261109T171442Z-s3-getobject-answers-404-for-the-patient-insights.md (connecthealth output), 20261109T183045Z-s3-getobject-answers-404-nosuchkey-for-the-emr.md (EMR Serverless output), 20261115T034316Z-s3-getobject-answers-404-nosuchkey-for-the-dms.md (DMS full load), 20261115T095444Z-s3-getobject-answers-404-nosuchkey-for-the-dataexchange.md (DataExchange export). The sibling ListObjectsV2 step of this same fixture (task#7) was reported as owned by 20261118T173242Z-s3-listobjectsv2-lists-no-object-for-an-explicit.md and is therefore not re-filed here, but it is the same root cause observed one step earlier.

## Evidence
Replayed against the emulator built from this checkout (trace set simparity-wire-aws-v1; emulator_up true, bootstrap_signal false, score 92.9%).

Fixture bedrock-data-automation-runtime-async-document-extraction-writes-text, step task#8, service s3, operation GetObject, expected status 200, actual status 404:
  status: expected 200, got 404
  $.metadata: missing — expected {asset_id '0', logical_subdocument_id '0', semantic_modality 'DOCUMENT', s3_bucket <output bucket>, s3_key <input asset key>, number_of_pages 1, start_page_index 0, end_page_index 0, file_type 'PDF'}, got None
  $.document: missing — expected {representation: {text: <text extracted from the input PDF>}, statistics: {}}, got None
  $headers.accept-ranges: missing — expected 'bytes', got None
  $headers.last-modified: missing (shape-asserted), got None
  $headers.etag: missing (shape-asserted), got None
  $headers.x-amz-checksum-crc32: missing (shape-asserted), got None
  $headers.x-amz-checksum-type: missing — expected 'FULL_OBJECT', got None
  $headers.content-type: value_mismatch — expected 'binary/octet-stream', got 'application/xml'
  $headers.x-amz-server-side-encryption: missing — expected 'AES256', got None

Requested key shape: <outputPrefix>/<invocationId>/<assetIndex>/standard_output/<subdocumentIndex>/result.json in the job's configured output bucket. Fixed text of the requirement: the 200 status, the accept-ranges/checksum-type/server-side-encryption header values ('bytes', 'FULL_OBJECT', 'AES256'), the binary/octet-stream content type, and the member names metadata/document/representation/text/statistics with semantic_modality 'DOCUMENT' and file_type 'PDF'. Variable parts taken from the request and emulator state: the bucket name, the invocation id inside the key, the input s3_key, the page indices/count, the extracted text, and the per-run ETag, Last-Modified and CRC32 value. Corroborating: step task#7 (ListObjectsV2 on the same output prefix) returns a 200 listing with no Contents entry for that key, confirming the object was never written rather than being mis-served.

## Steps to Reproduce
1. Call git_clone(), then replay_wire_corpus() so the emulator is built and booted from this checkout.
2. Replay fixture bedrock-data-automation-runtime-async-document-extraction-writes-text: it creates the input/output S3 bucket, uploads the input PDF asset, submits an asynchronous Bedrock Data Automation document-extraction invocation with an output S3 configuration, and polls the invocation until it reports a terminal success status.
3. Step task#7 lists the output prefix with s3 ListObjectsV2; step task#8 does s3 GetObject on <outputPrefix>/<invocationId>/0/standard_output/0/result.json.
4. Observe that GetObject answers 404 with an application/xml error envelope and none of the object metadata headers, where AWS answers 200 with the extracted-document JSON (metadata + document.representation.text), accept-ranges 'bytes', a CRC32 FULL_OBJECT checksum, Content-Type binary/octet-stream and x-amz-server-side-encryption 'AES256'. The ListObjectsV2 step shows the key was never created.
