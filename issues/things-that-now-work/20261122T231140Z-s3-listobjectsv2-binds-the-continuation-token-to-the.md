Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: s3.s3-list-objects-v2-malformed-continuation-token-rejected
Issue-Shape-Id: com.amazonaws.s3#AmazonS3
Issue-Key: s3|ListObjectsV2|InvalidArgument
Issue-Commit: a9b1f91879b3ed1957cfd9e0361182ba83602a4e

# S3 ListObjectsV2 binds the continuation token to the bucket that minted it, so a genuine token replayed against another bucket is refused InvalidArgument

> _Note: this issue was identified against the code base at commit `a9b1f91879b3ed1957cfd9e0361182ba83602a4e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListObjectsV2

## Description
Cause 1 of 1: a ListObjectsV2 continuation token is minted and validated against the requesting bucket name, so a token issued by one bucket's listing is rejected when it is handed back on a listing of a different bucket.

AWS: Smithy (services/service-com-amazonaws-s3-amazons3/smithy-model/com-amazonaws-s3-amazons3.json) documents ListObjectsV2Request$ContinuationToken as "ContinuationToken indicates to Amazon S3 that the list is being continued on this bucket with a token. ContinuationToken is obfuscated and is not a real key. You can use this ContinuationToken for pagination of the list results." It is an opaque cursor over the key sort order; the model states no binding between a token and the bucket that issued it, and ListObjectsV2 models only NoSuchBucket as an error. Recorded behavior (the failing check's grounding): a token minted by a listing of bucket A, replayed with Bucket=B, is ACCEPTED with HTTP 200 and resumes at whatever key of B sorts after the cursor the token encodes — with IsTruncated false when nothing in B sorts past it. A forged, never-issued token IS refused: HTTP 400 InvalidArgument with the message "The continuation token provided is incorrect" (fixed text; the only variable part of that error document is the ArgumentName/ArgumentValue naming the parameter the token arrived on).

Emulator: the cross-bucket replay is refused instead of served. The call `list_objects_v2(Bucket=<second bucket>, ContinuationToken=<token minted by the first bucket>)` answers HTTP 400 `InvalidArgument` / "The continuation token provided is incorrect" — botocore raises `ClientError: An error occurred (InvalidArgument) when calling the ListObjectsV2 operation: The continuation token provided is incorrect`, which aborts the check. Same-bucket resume and the forged-token refusal both behave correctly, so the defect is specifically the bucket in the token's identity: the handler decodes with the request's bucket as part of the token key (services/service-com-amazonaws-s3-amazons3/src/ops/list_objects_v2.rs: `crate::state::decode_continuation_token(&page_tokens, LISTING, &input.bucket, token)`) and mints it the same way (`crate::state::encode_continuation_token(&page_tokens, LISTING, &input.bucket, key)`).

Why: a client relying on AWS's behavior gets a valid pagination request refused. Any caller that carries a cursor across buckets — a tool that stores a page cursor and re-applies it to a sibling/mirror bucket, a differ that walks two buckets from one saved position, or any code that treats the token as the opaque sort cursor AWS documents — receives a 400 InvalidArgument where AWS returns a 200 page. Because the emulator's refusal uses the same code and message AWS reserves for a genuinely malformed token, the caller cannot tell "you forged this token" from "this token is fine but came from another bucket", and retry logic that re-lists from the start will loop or silently skip keys.

Fix: the continuation-token encode/decode used by the ListObjectsV2 handler — the token must encode only the resume position (the cursor key) under this store's token key, not the bucket, so any bucket's listing can decode a token this service issued; the refusal path must stay for tokens this service never issued. Belongs in the ListObjectsV2 handler's resume decoding and the shared token codec in the s3 service state.

Traces: s3.s3-list-objects-v2-malformed-continuation-token-rejected.

## Evidence
Failing check: [error] s3.s3-list-objects-v2-malformed-continuation-token-rejected

Expected (AWS, as the check states it): after `first = list_objects_v2(Bucket=bucket1, MaxKeys=1)` truncates and yields NextContinuationToken, and after the same-bucket resume returns the second key, `list_objects_v2(Bucket=bucket2, ContinuationToken=token)` returns HTTP 200 listing the one key of bucket2 that sorts after the cursor ("yourdocuments/sample.txt") with IsTruncated false.

Actual (emulator): the call raises
  botocore.exceptions.ClientError: An error occurred (InvalidArgument) when calling the ListObjectsV2 operation: The continuation token provided is incorrect

The forged-token assertion immediately before it (ContinuationToken="yourdocuments/" on bucket1 must be InvalidArgument / "The continuation token provided is incorrect") passed, so the emulator does produce that exact error document — it just also produces it for a genuine token from another bucket.

Source evidence in the checkout: services/service-com-amazonaws-s3-amazons3/src/ops/list_objects_v2.rs passes `&input.bucket` into both `crate::state::encode_continuation_token(...)` (mint) and `crate::state::decode_continuation_token(...)` (resume), so the bucket name is part of the token's identity.

## Steps to Reproduce
1. Create two buckets, B1 and B2.
2. Put two objects in B1: "mydocuments/sample.txt" and "yourdocuments/sample.txt". Put one object in B2: "yourdocuments/sample.txt".
3. token = list_objects_v2(Bucket=B1, MaxKeys=1)["NextContinuationToken"]  (IsTruncated is True).
4. list_objects_v2(Bucket=B1, MaxKeys=1, ContinuationToken=token) — returns "yourdocuments/sample.txt" (works today).
5. list_objects_v2(Bucket=B2, ContinuationToken=token) — AWS returns 200 with Contents == ["yourdocuments/sample.txt"] and IsTruncated false; the emulator raises HTTP 400 InvalidArgument "The continuation token provided is incorrect".
