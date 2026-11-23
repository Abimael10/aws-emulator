Issue-Source: trace-sweep-divergence
Issue-Trace: bedrock-data-automation-runtime-async-document-extraction-writes-text
Issue-Step: task#8
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: bedrock-data-automation-runtime-async-document-extraction-writes-text
Issue-Shape-Id: com.amazonaws.bedrockdataautomationruntime#AmazonBedrockKeystoneRuntimeService
Issue-Key: s3|GetObject|2xx->4xx|$headers.x-amz-checksum-crc32
Issue-Key: s3|GetObject|2xx->4xx|$headers.x-amz-checksum-type
Issue-Related: 20261115T034316Z-s3-getobject-answers-404-nosuchkey-for-the-dms.md
Issue-Related: 20261120T050654Z-s3-getobject-answers-404-for-the-bedrock-data.md
Issue-Commit: 10a6073fd096e836d3399e1065e42f6a20fdae56

# s3 GetObject never serves the Bedrock Data Automation async job's standard_output result.json

> _Note: this issue was identified against the code base at commit `10a6073fd096e836d3399e1065e42f6a20fdae56`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetObject

## Description
Cause 1 of 1: the per-asset standard-output document that a terminally-successful Bedrock Data Automation Runtime asynchronous document-extraction invocation designates under its own output S3 configuration is never present in the bucket, so the only read-back the API allows — s3 GetObject on <outputPrefix>/<invocationId>/<assetIndex>/standard_output/<subdocumentIndex>/result.json — answers 404 instead of returning the extraction result.

AWS: Smithy (s3, com-amazonaws-s3-amazons3): GetObject http GET /{Bucket}/{Key+}, success code 200; errors NoSuchKey (httpError 404) and InvalidObjectState (403); output members Body (httpPayload, streaming), AcceptRanges (accept-ranges), LastModified (Last-Modified), ETag (ETag), ChecksumCRC32 (x-amz-checksum-crc32), ChecksumType (x-amz-checksum-type, enum), ContentType (Content-Type), ServerSideEncryption (x-amz-server-side-encryption, enum), StorageClass (x-amz-storage-class); httpChecksum responseAlgorithms include CRC32. The S3 side effect of the asynchronous data-automation job itself is recorded behavior only: the recording shows that once the invocation reaches a terminal success status, GetObject on the standard_output result key answers 200 with the extracted-document JSON — a `metadata` object (asset_id, logical_subdocument_id, semantic_modality 'DOCUMENT', s3_bucket, s3_key of the input asset, number_of_pages, start_page_index, end_page_index, file_type 'PDF') and a `document` object whose representation.text carries the text extracted from the input PDF plus a statistics object — together with accept-ranges 'bytes', Last-Modified, ETag, an x-amz-checksum-crc32 value with x-amz-checksum-type 'FULL_OBJECT', Content-Type 'binary/octet-stream' and x-amz-server-side-encryption 'AES256'. The fixed part of the requirement is the 200 status, the member names (metadata/document/representation/text/statistics), the enum values 'DOCUMENT', 'PDF', 'FULL_OBJECT' and 'AES256', the accept-ranges value 'bytes' and the untyped binary/octet-stream content type; the bucket name, the generated invocation id inside the key, the input s3_key, the page indices and count, the extracted text, and the ETag/Last-Modified/CRC32 values are per-run values taken from the request and emulator state and are NOT part of the requirement.

Emulator: GetObject on that key answers 404 with an XML refusal envelope — Content-Type 'application/xml' where AWS serves 'binary/octet-stream' — carrying no body members at all ($.metadata and $.document both missing) and none of the object-metadata headers (accept-ranges, last-modified, etag, x-amz-checksum-crc32, x-amz-checksum-type, x-amz-server-side-encryption all absent). The absence is real rather than a mis-serve: the sibling ListObjectsV2 on the same output prefix in the same fixture (task#7) returns a 200 listing whose Contents has no entry for the key, so the s3 GetObject handler's 404/NoSuchKey classification of a genuinely missing key is itself correct. What is missing is the producing side effect: the asynchronous invocation is reported terminal/successful without ever materializing its standard_output result.json.

Why: a client that submits an asynchronous data-automation job and then does the only thing the API supports — poll until the job reports success, then read the output S3 URI the job itself reports — gets 404 NoSuchKey forever, with no error anywhere to handle because the job reported success. Every downstream consumer of the output prefix (S3 event notifications, Glue/Athena over the prefix, a Lambda parsing result.json) sees an empty prefix.

Fix: the bedrock-data-automation-runtime asynchronous-invocation completion path — the job-completion handler and its state under services/service-com-amazonaws-bedrockdataautomationruntime-amazonbedrockkeystoneruntimeservice/src — must write the per-asset standard_output result.json into the configured output bucket/prefix, with the extracted-document body shape and the object metadata a service-side S3 PutObject produces (CRC32 FULL_OBJECT checksum, SSE AES256, untyped binary content type), before the invocation is reported terminal. The s3 GetObject handler and its serializer need no change for this cause.

Traces: bedrock-data-automation-runtime-async-document-extraction-writes-text#task#8. The replay reported this cluster with an EMPTY owned_by and no blocking stub, so no open issue carries its replay key. Non-covering but closely related open issue, named here per instruction: 20261120T050654Z-s3-getobject-answers-404-for-the-bedrock-data.md describes the same symptom on the same fixture step but does not own this cluster's replay key in the replay report, so this divergence was re-reported as net-new; the two should be reconciled/closed together. Other related-but-different-producer open issues: 20261118T173242Z-s3-listobjectsv2-lists-no-object-for-an-explicit.md (owns the sibling ListObjectsV2 step task#7 of this fixture and is therefore NOT re-filed here), 20261120T164159Z-s3-getobject-serves-a-placeholder-summary-object-and.md, 20261121T075817Z-s3-the-s3files-mount-write-through-never-materialises.md.

## Evidence
Replayed against the emulator built from this checkout (trace set simparity-wire-aws-v1; emulator_up true, bootstrap_signal false, steps_total 42, passed 39, mismatched 2, score 92.9%).

Fixture bedrock-data-automation-runtime-async-document-extraction-writes-text, step task#8, service s3, operation GetObject, verdict mismatch, expected status 200, actual status 404:
  status: expected 200, got 404
  $.metadata: missing (expected {asset_id '0', logical_subdocument_id '0', semantic_modality 'DOCUMENT', s3_bucket <output bucket>, s3_key <input asset key>, number_of_pages 1, start_page_index 0, end_page_index 0, file_type 'PDF'}, got None)
  $.document: missing (expected {representation: {text: <text extracted from the input PDF>}, statistics: {}}, got None)
  $headers.accept-ranges: missing (expected 'bytes', got None)
  $headers.last-modified: missing (shape-asserted, got None)
  $headers.etag: missing (shape-asserted, got None)
  $headers.x-amz-checksum-crc32: missing (shape-asserted, got None)
  $headers.x-amz-checksum-type: missing (expected 'FULL_OBJECT', got None)
  $headers.content-type: value_mismatch (expected 'binary/octet-stream', got 'application/xml')
  $headers.x-amz-server-side-encryption: missing (expected 'AES256', got None)

divergence_key: service=s3|GetObject|status=200->404|keys=$.document,$.metadata,$headers.accept-ranges,$headers.content-type,$headers.etag,$headers.last-modified,$headers.x-amz-checksum-crc32,$headers.x-amz-checksum-type,$headers.x-amz-server-side-encryption,status

Corroboration that the object was never written rather than mis-served: step task#7 of the same fixture (s3 ListObjectsV2 over the same output prefix) answers 200 with no Contents entry for output/<invocationId>/0/standard_output/0/result.json (that cluster is owned by 20261118T173242Z-s3-listobjectsv2-lists-no-object-for-an-explicit.md and is not re-filed).

Which parts of the expected response are requirements: the 200 status; the header values 'bytes' (accept-ranges), 'FULL_OBJECT' (x-amz-checksum-type), 'AES256' (x-amz-server-side-encryption) and 'binary/octet-stream' (Content-Type); the body member names metadata/document/representation/text/statistics with semantic_modality 'DOCUMENT' and file_type 'PDF'. Which parts are per-run values drawn from the request and emulator state: the bucket name, the invocation id embedded in the key, the input s3_key, the page count/indices, the extracted text string, and the ETag, Last-Modified and CRC32 values.

## Steps to Reproduce
1. Call git_clone(), then replay_wire_corpus() so the emulator is built from this checkout and booted.
2. Replay fixture bedrock-data-automation-runtime-async-document-extraction-writes-text: it creates the input/output S3 bucket, uploads the input PDF asset, submits an asynchronous Bedrock Data Automation document-extraction invocation with an output S3 configuration, and polls the invocation until it reports a terminal success status.
3. Step task#7 lists the configured output prefix with s3 ListObjectsV2; step task#8 performs s3 GetObject on <outputPrefix>/<invocationId>/0/standard_output/0/result.json in the output bucket.
4. Observe step task#8: the emulator answers 404 with an application/xml error envelope and none of the object-metadata headers, where AWS answers 200 with the extracted-document JSON (metadata + document.representation.text + statistics), accept-ranges 'bytes', an x-amz-checksum-crc32 value with x-amz-checksum-type 'FULL_OBJECT', Content-Type 'binary/octet-stream' and x-amz-server-side-encryption 'AES256'. Step task#7 shows the key was never created, confirming the missing producing side effect rather than a GetObject lookup fault.
