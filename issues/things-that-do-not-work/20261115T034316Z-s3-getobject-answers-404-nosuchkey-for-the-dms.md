Issue-Source: trace-sweep-divergence
Issue-Trace: dms-s3-full-load-task-migrates-csv-rows
Issue-Step: task#25
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: dms-s3-full-load-task-migrates-csv-rows
Issue-Shape-Id: com.amazonaws.databasemigrationservice#AmazonDMSv20160101
Issue-Key: s3|GetObject|2xx->4xx|$
Issue-Key: s3|GetObject|2xx->4xx|$headers.accept-ranges
Issue-Key: s3|GetObject|2xx->4xx|$headers.content-type
Issue-Key: s3|GetObject|2xx->4xx|$headers.etag
Issue-Key: s3|GetObject|2xx->4xx|$headers.last-modified
Issue-Key: s3|GetObject|2xx->4xx|$headers.x-amz-server-side-encryption
Issue-Key: s3|GetObject|2xx->4xx|status
Issue-Commit: 2a19625f8ed3d2f7c2b61fce967c4196e2d44d15

# s3 GetObject answers 404 NoSuchKey for the DMS full-load target object

> _Note: this issue was identified against the code base at commit `2a19625f8ed3d2f7c2b61fce967c4196e2d44d15`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetObject

## Description
One net-new wire divergence, from fixture dms-s3-full-load-task-migrates-csv-rows step task#25 (service 's3', operation GetObject). The emulator is serving (emulator_up=true, bootstrap_signal=false, /__admin/health answered 200 and the data listener accepts), so this is a fidelity divergence and not a bootstrap gap. The cluster carries NO stub_path and an EMPTY owned_by: nothing open carries this replay key, so it is filed here. Related but NOT covering it: 20261111T153432Z-s3-listobjectsv2-reports-keycount-0-and-no-contents.md (the sibling ListObjectsV2 at task#24 of this same fixture; it explicitly excludes task#25, which it believed was owned elsewhere), 20261110T181940Z-s3-getobject-returns-404-nosuchkey-for-the-destination.md (same 404 shape but for a DataSync destination object, a different flow), 20261109T161403Z-dms-replication-task-lifecycle-never-completes-task-settings.md (the DMS-side lifecycle, service 'dms', which owns task#21/#22/#26/#27/#31 of this fixture), and 20261102T111455Z-s3-scalar-parameter-refusals-leak-smithy-coercion-wording.md (named in prose by the ListObjectsV2 issue as owning this step, but replay now reports owned_by empty for it).

Cause 1 of 1: a GET of the object a completed DMS S3-target full-load wrote under the endpoint's bucket folder is refused 404 NoSuchKey instead of serving the loaded CSV. Fixture dms-s3-full-load-task-migrates-csv-rows, step task#25 (GetObject).

AWS: Smithy com.amazonaws.s3#GetObject -> GET /{Bucket}/{Key+}, success HTTP 200, output shape GetObjectOutput whose Body is the @httpPayload streaming blob and whose remaining members are bound to response headers: AcceptRanges -> accept-ranges, LastModified -> Last-Modified, ETag -> ETag, ContentType -> Content-Type, ServerSideEncryption -> x-amz-server-side-encryption, ContentLength -> Content-Length. Its only modelled 404 is NoSuchKey (errorKind client, httpError 404), i.e. 'The specified key does not exist.' — which AWS returns only when the key is genuinely absent from the bucket. That the key exists at all after a full load is recorded behavior only as far as the S3 model goes: the recording shows a full-load of one selected table materializing exactly one object at <BucketFolder>/<schema>/<table>/LOAD00000001.csv, whose body is the migrated rows as CSV (one line per row, comma-separated column values, trailing newline) and whose response carries accept-ranges 'bytes', a Last-Modified, an ETag, Content-Type 'application/octet-stream' and x-amz-server-side-encryption 'AES256'. The recording's own key 'tgtdata/hr/employee/LOAD00000001.csv', its body '1,Ada,eng\n2,Linus,ops\n3,Grace,eng\n', its ETag and its timestamp are that run's values filling that template, not requirements in themselves.

Emulator: the same GET answers HTTP 404 with an XML error body — Code 'NoSuchKey', Message 'The specified key does not exist.', plus Key/RequestId/HostId — and Content-Type 'application/xml'; none of accept-ranges, Last-Modified, ETag or x-amz-server-side-encryption is present. In that message, 'The specified key does not exist.' is AWS's fixed text for NoSuchKey and Key is the requested key from the request, so the refusal is correctly WORDED; the defect is that the refusal is served at all, for a key AWS has by this point. Replay detail: "status: expected 200, got 404", "$: type_mismatch (expected '1,Ada,eng\\n2,Linus,ops\\n3,Grace,eng\\n', got {'Error': {'Code': 'NoSuchKey', ...}})", "$headers.accept-ranges: missing (expected 'bytes')", "$headers.last-modified: missing", "$headers.etag: missing", "$headers.content-type: value_mismatch (expected 'application/octet-stream', got 'application/xml')", "$headers.x-amz-server-side-encryption: missing (expected 'AES256')". Hypothesis: the DMS replication-task execution path never performs the target-endpoint write (the same fixture's task never leaves 'running'), so the object store the GetObject handler reads holds no such key and the handler reports its store faithfully — consistent with the sibling ListObjectsV2 at task#24 reporting KeyCount 0.

Why: a client that drives a migration and then reads its output through S3 — the canonical consumer of a DMS S3 target — cannot read a single migrated row. The 404 NoSuchKey is indistinguishable from 'the migration produced nothing', so SDK retries do not help, an 'object exists' precondition fails, and any loader, crawler or ETL step keyed on reading <BucketFolder>/<schema>/<table>/LOAD00000001.csv aborts with a hard client error rather than processing the table. Because the body never arrives, the downstream content assertions (byte content, checksum, SSE reporting) are unreachable too.

Fix: the write side of the S3 target plane for this flow — the component that materializes a DMS S3 target endpoint's full-load output into the S3 object store (the DMS replication-task execution path's target writer) — so that one object per loaded table is PUT under the endpoint's bucket folder with its bytes, size, ETag, content type and SSE state set the way a normal single-part PUT sets them. The GetObject handler's own 404/NoSuchKey path and its header projection need no change once the object is present; if the writer lands the object but without those members, the remaining gap belongs in the GetObject serializer's response-header binding.

Traces: dms-s3-full-load-task-migrates-csv-rows#task#25.

## Evidence
Replay of trace set simparity-wire-aws-v1: 12 fixtures replayed, 156 steps total, 57 passed, 97 mismatched, 2 blocked, 0 errored, 7 divergence clusters, score 36.5%. emulator_up=true, bootstrap_signal=false, emulator_detail '/__admin/health answered 200 and the :46207 data listener accepts'. Service token 's3' is bootstrapped (pending is empty).

[mismatch] dms-s3-full-load-task-migrates-csv-rows#task#25 GetObject — expected HTTP 200, got HTTP 404.
  divergence_key: service=s3|GetObject|status=200->404|keys=$,$headers.accept-ranges,$headers.content-type,$headers.etag,$headers.last-modified,$headers.x-amz-server-side-encryption,status
  stub_path: (empty)   owned_by: (empty)   affected_count: 1   affected: dms-s3-full-load-task-migrates-csv-rows#task#25

  Expected (recorded AWS): HTTP 200; body = the migrated rows as CSV (recorded '1,Ada,eng\n2,Linus,ops\n3,Grace,eng\n'); headers accept-ranges 'bytes', last-modified <shaped-not-asserted>, etag <shaped-not-asserted>, content-type 'application/octet-stream', x-amz-server-side-encryption 'AES256'.
  Actual (emulator): HTTP 404; body = {'Error': {'Code': 'NoSuchKey', 'Message': 'The specified key does not exist.', 'Key': 'tgtdata/hr/employee/LOAD00000001.csv', 'RequestId': ..., 'HostId': ...}}; content-type 'application/xml'; no accept-ranges, last-modified, etag or x-amz-server-side-encryption.
  Message anatomy: 'The specified key does not exist.' is NoSuchKey's fixed text; 'Key' is the requested key taken from the request; RequestId/HostId are per-run values and are NOT part of the finding.

Corroborating (NOT filed here — owned by 20261111T153432Z-s3-listobjectsv2-reports-keycount-0-and-no-contents.md): step task#24 ListObjectsV2 over the same prefix answers KeyCount 0 with no Contents, i.e. the object genuinely is not in the store rather than merely unreadable, so both steps share the missing target write.

Model consulted: smithy_query query=operation operation=GetObject on services/service-com-amazonaws-s3-amazons3/smithy-model/com-amazonaws-s3-amazons3.json — GetObject is GET /{Bucket}/{Key+} with success code 200; output members Body (@httpPayload, streaming), AcceptRanges (accept-ranges), LastModified (Last-Modified), ETag (ETag), ContentType (Content-Type), ServerSideEncryption (x-amz-server-side-encryption); errors InvalidObjectState (403) and NoSuchKey (404).

## Steps to Reproduce
1. git_clone the repo and boot the emulator built from that checkout.
2. Replay wire fixture dms-s3-full-load-task-migrates-csv-rows from trace set simparity-wire-aws-v1; step task#25 (GetObject) diverges with HTTP 404 NoSuchKey where the recording has HTTP 200 and the CSV body.
3. Shorter repro:
   a. CreateBucket for the migration target bucket.
   b. Stand up the DMS side: CreateReplicationSubnetGroup over two subnets in two AZs, CreateReplicationInstance and wait for ReplicationInstanceStatus 'available', then CreateEndpoint twice with EngineName 's3' (EndpointType 'source' and 'target'; the target's S3Settings naming that bucket and a BucketFolder).
   c. CreateReplicationTask with MigrationType 'full-load' and one selection rule for a single schema/table, StartReplicationTask, and wait for the task to finish.
   d. GET /<bucket>/<BucketFolder>/<schema>/<table>/LOAD00000001.csv : the emulator answers HTTP 404 with <Error><Code>NoSuchKey</Code><Message>The specified key does not exist.</Message><Key><that key></Key>...</Error> and Content-Type application/xml, where AWS answers HTTP 200 with the migrated rows as CSV and headers accept-ranges 'bytes', Last-Modified, ETag, Content-Type 'application/octet-stream' and x-amz-server-side-encryption 'AES256'.
   e. GET /<bucket>?list-type=2&prefix=<BucketFolder>/<schema>/<table>/ to confirm the cause: KeyCount 0 and no Contents, so the full-load target object was never written.
