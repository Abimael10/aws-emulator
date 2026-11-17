Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: s3.cors-browser-form-post-upload-with-origin-stores-the-bytes-and-grants-the-rule s3.cors-first-matching-rule-governs-presigned-get s3.cors-max-age-absent-from-the-rule-is-not-invented-on-the-preflight s3.cors-preflight-for-presigned-put-with-unlisted-request-header-rejected s3.cors-preflight-on-presigned-put-url-answers-rule-headers s3.cors-preflight-subdomain-scheme-mismatch-rejected-without-cors-headers s3.cors-preflight-without-a-request-method-header-rejected s3.cors-presigned-delete-with-origin-removes-the-object-and-grants-the-rule s3.cors-presigned-get-from-unlisted-origin-rejected-without-cors-headers s3.cors-presigned-get-of-missing-key-rejected-with-cors-headers s3.cors-presigned-get-under-wildcard-origin-rule-answers-star-without-credentials s3.cors-presigned-get-with-tampered-signature-rejected-with-cors-headers s3.cors-presigned-get-without-origin-on-configured-bucket-has-no-cors-headers s3.cors-presigned-put-from-origin-whose-rule-omits-put-rejected-without-cors-headers s3.cors-presigned-put-under-subdomain-wildcard-rule-echoes-the-origin s3.cors-expose-headers-names-only-the-listed-metadata-header s3.cors-presigned-get-from-the-allowed-origin-without-its-port-rejected s3.cors-presigned-head-carries-the-rule-headers-on-a-hit-and-on-a-miss s3.single-put-object-of-thirty-two-mib-round-trips-exactly s3.bucket-metadata-table-v1-surface-closed s3.delete-bucket-policy-denied-leaves-the-policy-a-twin-role-deletes-it s3.put-bucket-encryption-denied-default-unchanged-twin-role-applies-kms s3.multipart-complete-checksum-type-mismatched-from-create-rejected s3.multipart-complete-full-object-against-default-composite-upload-rejected s3.put-bucket-accelerate-configuration-directory-bucket-rejected s3.put-bucket-accelerate-configuration-dotted-bucket-name-rejected s3.copy-object-unrecognized-checksum-algorithm-rejected
Issue-Shape-Id: com.amazonaws.s3#AmazonS3
Issue-Key: s3.cors-browser-form-post-upload-with-origin-stores-the-bytes-and-grants-the-rule|_await_preflight#4|AssertionError|raise AssertionError(
Issue-Key: s3.cors-first-matching-rule-governs-presigned-get|_until_status#5|AssertionError|raise AssertionError(
Issue-Key: s3.cors-max-age-absent-from-the-rule-is-not-invented-on-the-preflight|_await_preflight#7|AssertionError|raise AssertionError(
Issue-Key: s3.cors-preflight-for-presigned-put-with-unlisted-request-header-rejected|_until_status#5|AssertionError|raise AssertionError(
Issue-Key: s3.cors-preflight-on-presigned-put-url-answers-rule-headers|_until_status#4|AssertionError|raise AssertionError(
Issue-Key: s3.cors-preflight-subdomain-scheme-mismatch-rejected-without-cors-headers|_until_status#6|AssertionError|raise AssertionError(
Issue-Key: s3.cors-preflight-without-a-request-method-header-rejected|_await_preflight_grant#7|AssertionError|raise AssertionError(
Issue-Key: s3.cors-presigned-delete-with-origin-removes-the-object-and-grants-the-rule|_await_preflight#5|AssertionError|raise AssertionError(
Issue-Key: s3.cors-presigned-get-from-unlisted-origin-rejected-without-cors-headers|_until_status#5|AssertionError|raise AssertionError(
Issue-Key: s3.cors-presigned-get-of-missing-key-rejected-with-cors-headers|_until_status#5|AssertionError|raise AssertionError(
Issue-Key: s3.cors-presigned-get-under-wildcard-origin-rule-answers-star-without-credentials|_until_status#5|AssertionError|raise AssertionError(
Issue-Key: s3.cors-presigned-get-with-tampered-signature-rejected-with-cors-headers|_until_status#5|AssertionError|raise AssertionError(
Issue-Key: s3.cors-presigned-get-without-origin-on-configured-bucket-has-no-cors-headers|_until_status#5|AssertionError|raise AssertionError(
Issue-Key: s3.cors-presigned-put-from-origin-whose-rule-omits-put-rejected-without-cors-headers|_until_status#4|AssertionError|raise AssertionError(
Issue-Key: s3.cors-presigned-put-under-subdomain-wildcard-rule-echoes-the-origin|_until_status#4|AssertionError|raise AssertionError(
Issue-Key: s3.cors-expose-headers-names-only-the-listed-metadata-header|_await_cors_grant#6|AssertionError|raise AssertionError(
Issue-Key: s3.cors-presigned-get-from-the-allowed-origin-without-its-port-rejected|_await_cors_grant#6|AssertionError|raise AssertionError(
Issue-Key: s3.cors-presigned-head-carries-the-rule-headers-on-a-hit-and-on-a-miss|_await_cors_grant#5|AssertionError|raise AssertionError(
Issue-Key: s3|PutObject|413
Issue-Key: s3.bucket-metadata-table-v1-surface-closed|run#7|AssertionError|assert get_code == "V1APIsNotAllowed", (
Issue-Key: s3.delete-bucket-policy-denied-leaves-the-policy-a-twin-role-deletes-it|_denied#13|AssertionError|raise AssertionError(f"expected the call to be denied, but it succeeded: {result!r}")
Issue-Key: s3.put-bucket-encryption-denied-default-unchanged-twin-role-applies-kms|_denied#13|AssertionError|raise AssertionError(f"expected the call to be denied, but it succeeded: {result!r}")
Issue-Key: s3.multipart-complete-checksum-type-mismatched-from-create-rejected|_error#10|AssertionError|raise AssertionError(f"expected the call to be rejected, but it succeeded: {result!r}")
Issue-Key: s3.multipart-complete-full-object-against-default-composite-upload-rejected|run#13|AssertionError|assert (code, message) == _MISMATCH, (
Issue-Key: s3.put-bucket-accelerate-configuration-directory-bucket-rejected|_error#3|AssertionError|raise AssertionError(f"expected the call to be rejected, but it succeeded: {result!r}")
Issue-Key: s3.put-bucket-accelerate-configuration-dotted-bucket-name-rejected|run#6|AssertionError|assert message == (
Issue-Key: s3|CopyObject|InvalidArgument
Issue-Commit: 898fb31eb69e07df3cb7a6861bbbd7f63697428c

# s3: 8 net-new data-plane and validation causes — OPTIONS preflight answers 501, presigned/origin GETs are refused 403, a 32 MiB PutObject answers 413, GetBucketMetadataTableConfiguration answers NotImplemented, policy-denied bucket writes succeed, multipart checksum-type mismatches are accepted, PutBucketAccelerateConfiguration is accepted on a directory bucket, and CopyObject's unsupported-checksum refusal is the generic enum error

> _Note: this issue was identified against the code base at commit `898fb31eb69e07df3cb7a6861bbbd7f63697428c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutBucketCors

## Description
Eight distinct root causes in the S3 data plane, its authorization path and its request validation.

Cause 1 of 8: a CORS preflight (HTTP OPTIONS on an object or presigned URL) is answered 501 NotImplemented, so no CORS rule can ever be evaluated.
AWS: Smithy (s3) models PutBucketCors/GetBucketCors with CORSConfiguration.CORSRules (AllowedOrigins, AllowedMethods, AllowedHeaders, ExposeHeaders, MaxAgeSeconds). A browser preflight is an OPTIONS request carrying Origin and Access-Control-Request-Method (and optionally Access-Control-Request-Headers); S3 answers 200 with the matching rule's Access-Control-Allow-Origin/-Methods/-Headers (and Access-Control-Max-Age only when the rule sets MaxAgeSeconds), answers 403 with code AccessForbidden when no rule matches the origin, method or a requested header, and answers 400 when Access-Control-Request-Method is absent.
Emulator: every preflight answers HTTP 501 with code 'NotImplemented' and the message 'operation OPTIONS /<bucket>/<key> is not implemented in this emulator' — the OPTIONS route does not exist, so allowed, disallowed, wildcard, subdomain-wildcard, first-matching-rule, max-age and after-DeleteBucketCors behaviour are all unobservable.
Why: no browser-origin workload can work: the preflight that gates every cross-origin GET/PUT/DELETE fails closed, and a client cannot distinguish 'rule does not allow this' from 'the endpoint has no CORS at all'.
Fix: the S3 request router and the CORS evaluator — OPTIONS on a bucket/object path must be routed and answered from the bucket's stored CORS configuration. Open issue 20261109T220008Z-s3-7-net-new-causes-cors-preflight-answers.md names this same 501 for other checks but does not carry the ones listed here.
Traces: s3.cors-browser-form-post-upload-with-origin-stores-the-bytes-and-grants-the-rule, s3.cors-first-matching-rule-governs-presigned-get, s3.cors-max-age-absent-from-the-rule-is-not-invented-on-the-preflight, s3.cors-preflight-for-presigned-put-with-unlisted-request-header-rejected, s3.cors-preflight-on-presigned-put-url-answers-rule-headers, s3.cors-preflight-subdomain-scheme-mismatch-rejected-without-cors-headers, s3.cors-preflight-without-a-request-method-header-rejected, s3.cors-presigned-delete-with-origin-removes-the-object-and-grants-the-rule, s3.cors-presigned-get-from-unlisted-origin-rejected-without-cors-headers, s3.cors-presigned-get-of-missing-key-rejected-with-cors-headers, s3.cors-presigned-get-under-wildcard-origin-rule-answers-star-without-credentials, s3.cors-presigned-get-with-tampered-signature-rejected-with-cors-headers, s3.cors-presigned-get-without-origin-on-configured-bucket-has-no-cors-headers, s3.cors-presigned-put-from-origin-whose-rule-omits-put-rejected-without-cors-headers, s3.cors-presigned-put-under-subdomain-wildcard-rule-echoes-the-origin.

Cause 2 of 8: a query-string (presigned) GET/HEAD is refused 403 AccessDenied instead of serving the object, so no CORS response header is ever carried on the real request either.
AWS: a SigV4 query-string presigned GET/HEAD within its expiry is served exactly like the header-signed request: 200 (or 404 for a missing key), and when the request carries an Origin matching a CORS rule the response carries Access-Control-Allow-Origin and only the ExposeHeaders the rule lists.
Emulator: the request is answered 403 with ('AccessDenied', 'Sign the request with the emulator credentials') and no Access-Control-Allow-Origin — 'after 20s the GET answered 403 with Allow-Origin None, expected 'https://reader.example.org''. The emulator refuses query-string-signed requests outright.
Why: presigned URLs — the standard way browsers and third parties read S3 objects — do not work, and the Expose-Headers/allow-origin contract on them cannot hold.
Fix: the S3 authentication path's query-string SigV4 (presigned) verification, and the CORS response-header decoration on an authorized object GET/HEAD. Related open issue 20261109T220008Z-smoke-query-string-presigned-sigv4-requests-are-refused.md covers the smoke check, not these.
Traces: s3.cors-expose-headers-names-only-the-listed-metadata-header, s3.cors-presigned-get-from-the-allowed-origin-without-its-port-rejected, s3.cors-presigned-head-carries-the-rule-headers-on-a-hit-and-on-a-miss.

Cause 3 of 8: a single PutObject of 32 MiB is refused with HTTP 413.
AWS: Smithy (s3) PutObject takes a streaming Body with no 32 MiB limit — a single PUT carries up to 5 GiB — so a 32 MiB body must be stored and read back byte-exact.
Emulator: 'botocore.exceptions.ClientError: An error occurred (413) when calling the PutObject operation: Request Entity Too Large'. An invented request-size ceiling refuses a valid write.
Why: any client uploading an ordinary-sized object (logs, images, archives) is refused with a status AWS never returns for it, and 413 carries no S3 error code a client can branch on.
Fix: the PutObject request-body size limit in the S3 data-plane handler. Open issue 20261111T165323Z-s3-emulator-invented-storage-ceilings-refuse-valid-writes.md records the same family of invented ceilings for other checks but does not carry this one.
Traces: s3.single-put-object-of-thirty-two-mib-round-trips-exactly.

Cause 4 of 8: GetBucketMetadataTableConfiguration answers NotImplemented where AWS answers the V1-surface refusal.
AWS: Smithy (s3) binds GetBucketMetadataTableConfiguration; on an account where the V1 metadata-table surface is closed, AWS refuses it with error code 'V1APIsNotAllowed' — a modeled refusal, not a missing operation.
Emulator: the error code is 'NotImplemented' — 'AssertionError: GetBucketMetadataTableConfiguration on an account where the V1 surface is closed answered 'NotImplemented', expected V1APIsNotAllowed'.
Why: a client cannot tell a closed V1 surface (migrate to V2) from an unimplemented endpoint (retry/report), so it takes the wrong remediation.
Fix: the GetBucketMetadataTableConfiguration handler's refusal classification.
Traces: s3.bucket-metadata-table-v1-surface-closed.

Cause 5 of 8: a bucket-level write that the caller's policy denies is performed instead of being denied.
AWS: DeleteBucketPolicy and PutBucketEncryption are authorized against the caller's identity and the bucket policy; a principal denied s3:DeleteBucketPolicy / s3:PutEncryptionConfiguration is refused with AccessDenied (403) and the existing policy / default encryption is left unchanged, while a twin role that is permitted succeeds.
Emulator: both calls succeed for the denied principal — 'AssertionError: expected the call to be denied, but it succeeded' (HTTP 204 for DeleteBucketPolicy, HTTP 200 for PutBucketEncryption), and the state is mutated.
Why: least-privilege separation does not hold on the bucket-configuration surface: a role that must not be able to remove a bucket policy or change default encryption can do both, and the emulator reports success, so a security control validated here would be reported as working.
Fix: the authorization check in the DeleteBucketPolicy and PutBucketEncryption handlers (identity policy + bucket policy evaluation before the state write).
Traces: s3.delete-bucket-policy-denied-leaves-the-policy-a-twin-role-deletes-it, s3.put-bucket-encryption-denied-default-unchanged-twin-role-applies-kms.

Cause 6 of 8: CompleteMultipartUpload does not validate the checksum type against the one the upload was created with.
AWS: Smithy (s3) models CompleteMultipartUpload's ChecksumType header (and CreateMultipartUpload's); completing with a ChecksumType other than the upload's is refused with ('InvalidRequest', 'The upload was created using the COMPOSITE checksum mode. The complete request must use the same checksum mode.') — fixed text in which only the mode name comes from the upload's state. A complete whose checksum type contradicts the create must not store an object.
Emulator: one such complete is accepted outright — 'AssertionError: expected the call to be rejected, but it succeeded' with HTTP 200, an ETag and ChecksumType 'FULL_OBJECT' — and the other is refused with the wrong class and message, ('BadDigest', 'The crc32 you specified did not match the calculated checksum.'), as if the value were wrong rather than the mode.
Why: a client gets an object whose checksum semantics differ from what it created the upload with, with no signal; and when it is refused, the error points at a digest mismatch, so the caller retries the same wrong mode forever.
Fix: CompleteMultipartUpload's checksum-mode validation against the upload's recorded ChecksumType, and the InvalidRequest message it raises.
Traces: s3.multipart-complete-checksum-type-mismatched-from-create-rejected, s3.multipart-complete-full-object-against-default-composite-upload-rejected.

Cause 7 of 8: PutBucketAccelerateConfiguration is accepted on a directory bucket, and its refusal message for a dotted bucket name is generic.
AWS: Smithy (s3) models PutBucketAccelerateConfiguration PUT /{Bucket}?accelerate. Transfer Acceleration does not exist for directory buckets: the call is refused ('InvalidArgument', 'AccelerateConfiguration is not valid, expected CreateBucketConfiguration') and GetBucketAccelerateConfiguration on a directory bucket is refused ('MethodNotAllowed', 'The specified method is not allowed against this resource.'). For a general-purpose bucket whose name contains a period, it is refused ('InvalidRequest', 'S3 Transfer Acceleration is not supported for buckets with periods (.) in their names'). All three messages are fixed text with no variable parts.
Emulator: the directory-bucket call succeeds (HTTP 200, 'AssertionError: expected the call to be rejected, but it succeeded'); the dotted-name call is refused but with the generic sentence "A parameter or header in your request isn't valid. For details, see the description of this API operation."
Why: a client believes acceleration is enabled on a bucket class that has no such feature, and on the dotted-name path it gets no clue which parameter or rule it violated.
Fix: PutBucketAccelerateConfiguration's bucket-class and bucket-name validation and the messages it raises.
Traces: s3.put-bucket-accelerate-configuration-directory-bucket-rejected, s3.put-bucket-accelerate-configuration-dotted-bucket-name-rejected.

Cause 8 of 8: CopyObject with an unrecognised checksum algorithm is refused by the generic enum validator rather than by S3's own rule.
AWS: Smithy (s3) models CopyObject's ChecksumAlgorithm as the x-amz-checksum-algorithm header bound to the ChecksumAlgorithm enum, but S3's refusal for an unsupported value is ('InvalidRequest', 'Checksum algorithm provided is unsupported. Please try again with any of the valid types: [CRC32, CRC32C, CRC64NVME, MD5, SHA1, SHA256, SHA512, XXHASH128, XXHASH3, XXHASH64]') — fixed text whose bracketed list is the supported-algorithm set.
Emulator: ('InvalidArgument', "Value 'SHA3-256' at 'checksumAlgorithm' failed to satisfy constraint: Member must satisfy enum value set: [CRC32, CRC32C, SHA1, SHA256, CRC64NVME, SHA512, MD5, XXHASH64, XXHASH3, XXHASH128]") — the generic smithy enum-constraint refusal, with a different code, a different sentence and a differently ordered list.
Why: a client branching on InvalidRequest vs InvalidArgument mis-classifies the failure, and the message does not read as S3's documented guidance.
Fix: CopyObject's checksum-algorithm validation — S3's own InvalidRequest refusal must take precedence over the generic enum-constraint error.
Traces: s3.copy-object-unrecognized-checksum-algorithm-rejected.

## Evidence
Cause 1 — Expected: OPTIONS with Origin + Access-Control-Request-Method answers 200 with the rule's Access-Control-Allow-* headers (403 AccessForbidden when nothing matches; 400 when the request-method header is absent). Actual: HTTP 501 '<Error><Code>NotImplemented</Code><Message>operation OPTIONS /<bucket>/<key> is not implemented in this emulator</Message>' for every preflight.
Cause 2 — Expected: a presigned GET/HEAD within expiry serves the object and carries Access-Control-Allow-Origin for a matching Origin. Actual: HTTP 403 ('AccessDenied', 'Sign the request with the emulator credentials'), Allow-Origin None.
Cause 3 — Expected: PutObject of 32 MiB returns 200 and reads back byte-exact. Actual: '(413) when calling the PutObject operation: Request Entity Too Large'.
Cause 4 — Expected error code 'V1APIsNotAllowed'. Actual 'NotImplemented'.
Cause 5 — Expected: 403 AccessDenied with the bucket policy / default encryption unchanged. Actual: DeleteBucketPolicy HTTP 204 and PutBucketEncryption HTTP 200 for the denied principal.
Cause 6 — Expected ('InvalidRequest', 'The upload was created using the COMPOSITE checksum mode. The complete request must use the same checksum mode.'). Actual: one complete succeeded with HTTP 200 / ChecksumType 'FULL_OBJECT'; the other gave ('BadDigest', 'The crc32 you specified did not match the calculated checksum.').
Cause 7 — Expected ('InvalidArgument', 'AccelerateConfiguration is not valid, expected CreateBucketConfiguration') on a directory bucket and ('InvalidRequest', 'S3 Transfer Acceleration is not supported for buckets with periods (.) in their names') on a dotted name. Actual: HTTP 200 on the directory bucket; "A parameter or header in your request isn't valid. For details, see the description of this API operation." on the dotted name.
Cause 8 — Expected ('InvalidRequest', 'Checksum algorithm provided is unsupported. Please try again with any of the valid types: [CRC32, CRC32C, CRC64NVME, MD5, SHA1, SHA256, SHA512, XXHASH128, XXHASH3, XXHASH64]'). Actual ('InvalidArgument', "Value 'SHA3-256' at 'checksumAlgorithm' failed to satisfy constraint: Member must satisfy enum value set: [...]").

## Steps to Reproduce
Cause 1: PutBucketCors with a rule allowing an origin and method; send OPTIONS to /<bucket>/<key> with Origin and Access-Control-Request-Method.
Cause 2: PutObject an object, generate a presigned GET URL, fetch it (with and without an Origin header).
Cause 3: PutObject a 32 MiB body in a single call.
Cause 4: call GetBucketMetadataTableConfiguration on a bucket in an account whose V1 metadata-table surface is closed.
Cause 5: PutBucketPolicy denying s3:DeleteBucketPolicy (resp. s3:PutEncryptionConfiguration) to a role, then call DeleteBucketPolicy (resp. PutBucketEncryption) as that role, and again as a permitted twin role.
Cause 6: CreateMultipartUpload with the default COMPOSITE checksum mode, UploadPart, then CompleteMultipartUpload with ChecksumType FULL_OBJECT.
Cause 7: CreateBucket a directory bucket and call PutBucketAccelerateConfiguration on it; separately, create a general-purpose bucket whose name contains a period and call PutBucketAccelerateConfiguration on that.
Cause 8: CopyObject with x-amz-checksum-algorithm: SHA3-256.
