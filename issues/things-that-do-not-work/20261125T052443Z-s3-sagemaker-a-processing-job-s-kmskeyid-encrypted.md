Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: s3__sagemaker.sagemaker-processing-job-kms-encrypted-s3-output-positive
Issue-Key: s3__sagemaker.sagemaker-processing-job-kms-encrypted-s3-output-positive|run#17|AssertionError|assert head.get("ServerSideEncryption") == "aws:kms", head.get("ServerSideEncryption")
Issue-Commit: 3ce962b3772a5f276bd697ccb3d5b119b45b10c0

# s3__sagemaker: a processing job's KmsKeyId-encrypted output object is written with SSE-S3, so HeadObject reports AES256 instead of aws:kms

> _Note: this issue was identified against the code base at commit `3ce962b3772a5f276bd697ccb3d5b119b45b10c0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateProcessingJob

## Description
Cause 1 of 1: the processing job's S3 output object ignores the declared output KMS key and is stored with SSE-S3 instead of SSE-KMS.

AWS: Smithy (com-amazonaws-sagemaker-sagemaker) documents ProcessingOutputConfig.KmsKeyId as "The Amazon Web Services Key Management Service (Amazon Web Services KMS) key that Amazon SageMaker uses to encrypt the processing job output. KmsKeyId can be an ID of a KMS key, ARN of a KMS key, or alias of a KMS key. The KmsKeyId is applied to all outputs." So every object the job writes under the output S3Uri is encrypted with that customer key, and an S3 HeadObject/GetObject on it reports ServerSideEncryption = "aws:kms" with x-amz-server-side-encryption-aws-kms-key-id naming the key the job declared.

Emulator: the output object is written with SSE-S3 — HeadObject reports ServerSideEncryption = "AES256" — so the declared KmsKeyId is dropped and the object is not encrypted with the customer key at all.

Why: a client that declares an output KMS key to satisfy an encryption requirement, then verifies it by heading the output object (or by relying on a KMS key policy / grant to gate who can read the result), is told the object is SSE-S3. Compliance checks that assert aws:kms fail, and a downstream reader that expects to need kms:Decrypt on the declared key sees an object readable without it — the opposite of the guarantee the request asked for.

Fix: the SageMaker CreateProcessingJob handler (the part that materialises the job's declared outputs into S3) must carry ProcessingOutputConfig.KmsKeyId through to the output PutObject as SSE-KMS with that key id, so S3 state records aws:kms and the key id for the object, and HeadObject/GetObject report them.

Note: open issue 20261029T211510Z-s3-sagemaker-sagemaker-jobs-never-execute-and-are-never-validated.md owns several neighbouring processing-job checks (including the sibling sagemaker-processing-job-kms-encrypted-output-positive) but does not carry this check's replay key.

Traces: the failing check in trace_ids.

## Evidence
Expected (AWS): after a processing job with ProcessingOutputConfig.KmsKeyId = <key> completes, S3 HeadObject on the output object returns ServerSideEncryption = "aws:kms" (header x-amz-server-side-encryption: aws:kms) and names that key in SSEKMSKeyId.

Actual (emulator): HeadObject returns ServerSideEncryption = "AES256" — "AssertionError: AES256" on the assertion head.get("ServerSideEncryption") == "aws:kms". The key id and object key are per-run values; the requirement is that the declared KmsKeyId be applied to all outputs.

## Steps to Reproduce
1. Create a KMS key and an S3 bucket.
2. Call sagemaker CreateProcessingJob with ProcessingOutputConfig.KmsKeyId set to that key and an S3 output destination in the bucket.
3. Wait for the job to complete and locate the output object.
4. Call s3 HeadObject on it: ServerSideEncryption is "AES256" instead of "aws:kms", and no SSE-KMS key id is reported.
5. Re-replay the check id in trace_ids to confirm.
