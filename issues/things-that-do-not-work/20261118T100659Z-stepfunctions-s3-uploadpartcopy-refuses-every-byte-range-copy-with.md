Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: stepfunctions.distributed-map-item-reader-input-file-over-10gb-rejected
Issue-Shape-Id: com.amazonaws.sfn#AWSStepFunctions
Issue-Key: s3|UploadPartCopy|InvalidRequest
Issue-Commit: 7f8f759d74cb6fc509a9ff7e87e7c97fb9625f4c

# S3 UploadPartCopy refuses every byte-range copy with InvalidRequest 'The specified copy source is not supported as a byte-range copy source'

> _Note: this issue was identified against the code base at commit `7f8f759d74cb6fc509a9ff7e87e7c97fb9625f4c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UploadPartCopy

## Description
One net-new root cause, surfaced by a Step Functions distributed-map check whose setup assembles a large S3 object: S3's UploadPartCopy rejects a request that carries CopySourceRange. (The failing check is listed under the '### stepfunctions' heading of the suite report, which is the service token used here; the defect itself is in the S3 handler.)

AWS: UploadPartCopy copies data from an existing object into a part of a multipart upload. When x-amz-copy-source-range is supplied, S3 copies exactly that byte range of the source object; the request succeeds with HTTP 200 and a CopyPartResult carrying the part's ETag, provided the range is within the source object and the resulting part satisfies the multipart part-size rules. Smithy (com.amazonaws.s3#UploadPartCopy): CopySourceRange is a modeled optional input member bound to the header x-amz-copy-source-range; CopySource is required (header x-amz-copy-source, pattern ^\/?.+\/.+$); the operation is PUT /{Bucket}/{Key+} with success code 200 and declares no modeled errors. The documented InvalidRequest for a byte-range copy applies to a source that cannot be range-copied at all — not to an ordinary object in the same bucket.
Emulator: every UploadPartCopy carrying CopySourceRange is refused with the error code InvalidRequest and the message "The specified copy source is not supported as a byte-range copy source", for a plain 5 MiB object in the same bucket that was just written with PutObject and whose requested range (bytes=0-<size-1>) is wholly inside it.
Why: ranged server-side copy is the only way to build a large object without re-uploading its bytes, and it is how callers (and SDK high-level transfer managers) compose, re-chunk and concatenate objects. With it refused, no caller can assemble a multi-GB object server-side at all, and any workflow that depends on a large object — here, exercising the Step Functions distributed-map ItemReader input-size quota — cannot even reach the behaviour it means to test.
Fix: the UploadPartCopy handler in the S3 crate — honour the CopySourceRange header by copying the named byte range out of the resolved copy source and returning CopyPartResult, restricting the InvalidRequest refusal to sources that genuinely cannot be range-copied.
Traces: stepfunctions.distributed-map-item-reader-input-file-over-10gb-rejected (the check errors out in setup, so the distributed-map ItemReader 10 GB quota behaviour it exists to assert is also left unverified).

## Evidence
Behavioral suite simparity-behavioral-aws-v1 replayed against the emulator built from this checkout: 761/955 passed. The check is listed under '### stepfunctions' with verdict [error] and is marked neither ALREADY FILED nor OWNED.

stepfunctions.distributed-map-item-reader-input-file-over-10gb-rejected:
  botocore.errorfactory.InvalidRequest: An error occurred (InvalidRequest) when calling the UploadPartCopy operation: The specified copy source is not supported as a byte-range copy source.

The call that is refused (from the check's _put_giant_object helper) is, in general form:
  PutObject(Bucket=<bucket>, Key=<chunk key>, Body=<5 MiB of CSV rows>)
  CreateMultipartUpload(Bucket=<bucket>, Key=<giant key>) -> UploadId
  UploadPartCopy(Bucket=<bucket>, Key=<giant key>, UploadId=<id>, PartNumber=<n>,
                 CopySource="<bucket>/<chunk key>", CopySourceRange="bytes=0-<chunksize-1>")
Expected (AWS): HTTP 200 with CopyPartResult.ETag for each part, then CompleteMultipartUpload and a HeadObject ContentLength above 10 GiB.
Actual (emulator): the first UploadPartCopy is refused with InvalidRequest / "The specified copy source is not supported as a byte-range copy source", so the object is never assembled and the check cannot proceed to the Step Functions assertion.

Fixed vs. variable parts: the refused message is wholly fixed emulator text; the bucket, keys, upload id, part number and range are the request's values. Nothing here depends on a live account or a per-run id.

Model reference used: com.amazonaws.s3#UploadPartCopy — input members CopySource (required, header x-amz-copy-source) and CopySourceRange (header x-amz-copy-source-range); output CopyPartResult as the HTTP payload; http PUT /{Bucket}/{Key+} with code 200; errors: [] (no modeled error shapes).

## Steps to Reproduce
Boot the emulator from this checkout and replay stepfunctions.distributed-map-item-reader-input-file-over-10gb-rejected. Minimal manual repro: CreateBucket; PutObject a 5 MiB body under key 'chunk'; CreateMultipartUpload for key 'giant'; call UploadPartCopy(Bucket=<bucket>, Key='giant', UploadId=<id>, PartNumber=1, CopySource='<bucket>/chunk', CopySourceRange='bytes=0-5242879'). Expect HTTP 200 and a CopyPartResult ETag; the emulator answers InvalidRequest 'The specified copy source is not supported as a byte-range copy source'. Then CompleteMultipartUpload and HeadObject to confirm the assembled size.
