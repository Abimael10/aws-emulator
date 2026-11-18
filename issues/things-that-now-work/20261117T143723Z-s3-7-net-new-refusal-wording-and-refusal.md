Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: s3.bucket-policy-over-twenty-kb-rejected s3.bucket-tagging-over-fifty-rejected s3.list-object-annotations-max-annotation-results-over-one-thousand-rejected s3.put-bucket-cors-over-100-rules-rejected s3.put-bucket-logging-nonexistent-target-bucket-rejected s3.put-object-acl-canned-and-policy-together-rejected s3.update-object-encryption-deleted-version-rejected s3.update-object-encryption-error-paths
Issue-Shape-Id: com.amazonaws.s3#AmazonS3
Issue-Key: s3.bucket-policy-over-twenty-kb-rejected|run#10|AssertionError|assert got == _POLICY_TOO_LARGE, f"expected {_POLICY_TOO_LARGE!r} for an oversized policy, got {got!r}"
Issue-Key: s3.bucket-tagging-over-fifty-rejected|run#10|AssertionError|assert got == _TOO_MANY_BUCKET_TAGS, f"expected {_TOO_MANY_BUCKET_TAGS!r} for 51 tags, got {got!r}"
Issue-Key: s3.list-object-annotations-max-annotation-results-over-one-thousand-rejected|run#11|AssertionError|assert message == (
Issue-Key: s3.put-bucket-cors-over-100-rules-rejected|run#9|AssertionError|assert message == "The number of CORS rules should not exceed allowed limit of 100 rules.", (
Issue-Key: s3.put-bucket-logging-nonexistent-target-bucket-rejected|run#7|AssertionError|assert message == "The target bucket for logging does not exist", f"unexpected message {message!r}"
Issue-Key: s3.put-object-acl-canned-and-policy-together-rejected|run#13|AssertionError|assert code == "UnexpectedContent", f"expected UnexpectedContent, got {code!r}"
Issue-Key: s3.update-object-encryption-deleted-version-rejected|run#11|AssertionError|assert code == "NoSuchKey", (
Issue-Key: s3.update-object-encryption-error-paths|run#21|AssertionError|assert msg_malformed == MSG_ACCESS_DENIED, (
Issue-Commit: 898fb31eb69e07df3cb7a6861bbbd7f63697428c

# s3: 7 net-new refusal-wording and refusal-class causes — oversized bucket policy, >50 bucket tags, over-range MaxAnnotationResults, >100 CORS rules, missing logging target bucket, PutObjectAcl with both an ACL and a policy, and UpdateObjectEncryption's two error paths

> _Note: this issue was identified against the code base at commit `898fb31eb69e07df3cb7a6861bbbd7f63697428c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutBucketPolicy

## Description
Seven distinct causes where S3 refuses the right request but with wording or a class AWS does not use. In every one of these the emulator substitutes its own sentence (often naming internal member paths or counts) for AWS's fixed text, so a client matching on the message, or branching on the code, diverges.

Cause 1 of 7: PutBucketPolicy over the 20,480-byte limit.
AWS: the refusal is ('MalformedPolicy', 'Normalized policy document exceeds the maximum allowed size of 20480 bytes') — fixed text; 20480 is the documented limit, not a per-request value.
Emulator: ('MalformedPolicy', 'A bucket policy carries at most 20480 bytes; the request carries 41159') — the emulator's own sentence with the request's byte count interpolated.
Fix: PutBucketPolicy's size validation message.
Traces: s3.bucket-policy-over-twenty-kb-rejected.

Cause 2 of 7: PutBucketTagging with more than 50 tags.
AWS: ('BadRequest', 'Bucket tag count cannot be greater than 50') — fixed text.
Emulator: ('BadRequest', 'Bucket tags cannot be greater than 50') — 'tag count' replaced by 'tags'.
Fix: PutBucketTagging's tag-count validation message.
Traces: s3.bucket-tagging-over-fifty-rejected.

Cause 3 of 7: ListObjectAnnotations with MaxAnnotationResults above 1000.
AWS: ('InvalidArgument', 'Argument max-annotation-results must be an integer between 1 and 1000') — fixed text naming the wire parameter max-annotation-results and the documented bounds.
Emulator: ('InvalidArgument', "Value '1001' at 'maxAnnotationResults' failed to satisfy constraint: Member must have value less than or equal to 1000") — the generic smithy range-constraint sentence naming the modeled member instead of the wire parameter.
Fix: ListObjectAnnotations' MaxAnnotationResults validation — S3's own InvalidArgument message must take precedence over the generic range-constraint error.
Traces: s3.list-object-annotations-max-annotation-results-over-one-thousand-rejected.

Cause 4 of 7: PutBucketCors with more than 100 rules.
AWS: ('InvalidRequest', 'The number of CORS rules should not exceed allowed limit of 100 rules.') — fixed text; the rejected PUT must also leave the bucket with no CORS configuration (GetBucketCors -> NoSuchCORSConfiguration).
Emulator: ('InvalidRequest', 'CORSConfiguration.CORSRules carries 101 rules, more than the 100 rules one cors configuration may hold') — the emulator's own sentence naming the member path and the submitted count.
Fix: PutBucketCors' rule-count validation message.
Traces: s3.put-bucket-cors-over-100-rules-rejected.

Cause 5 of 7: PutBucketLogging naming a target bucket that does not exist.
AWS: ('InvalidTargetBucketForLogging', 'The target bucket for logging does not exist') — fixed text, no variable parts.
Emulator: the code is right but the message is the longer variant 'The target bucket for logging does not exist, is not owned by you, or does not have the appropriate grants for the log-delivery group.'
Fix: PutBucketLogging's target-bucket validation message.
Traces: s3.put-bucket-logging-nonexistent-target-bucket-rejected.

Cause 6 of 7: PutObjectAcl carrying both a canned ACL header and an AccessControlPolicy body.
AWS: Smithy (s3) models PutObjectAcl with both the x-amz-acl header (ACL) and the AccessControlPolicy payload; supplying both is refused with error code 'UnexpectedContent'.
Emulator: the code is 'InvalidRequest'.
Why: a client branching on the code cannot distinguish 'you sent a body you should not have' from any other bad request.
Fix: PutObjectAcl's mutually-exclusive-input validation and the error code it raises.
Traces: s3.put-object-acl-canned-and-policy-together-rejected.

Cause 7 of 7: UpdateObjectEncryption's two error paths both diverge.
AWS: for a version id that has been permanently deleted the refusal is 'NoSuchKey'; for a KMS key owned by another account the refusal is AccessDenied with the fixed message 'The KMS key must be from the same account as the bucket'.
Emulator: the deleted version gives ('NoSuchVersion', 'The specified version does not exist.'), and the cross-account key gives AccessDenied with a different sentence: 'The Amazon Web Services KMS key in the request must be owned by the same account as the bucket. Modify the request to specify a KMS key from the same account, and then try again.'
Why: a client that distinguishes a missing key from a missing version takes the wrong branch, and the cross-account-key guidance it surfaces is not AWS's.
Fix: UpdateObjectEncryption's version-resolution error classification and its cross-account-KMS AccessDenied message.
Traces: s3.update-object-encryption-deleted-version-rejected, s3.update-object-encryption-error-paths.

## Evidence
Cause 1 — Expected ('MalformedPolicy', 'Normalized policy document exceeds the maximum allowed size of 20480 bytes'); actual ('MalformedPolicy', 'A bucket policy carries at most 20480 bytes; the request carries 41159') (41159 is this run's body size).
Cause 2 — Expected ('BadRequest', 'Bucket tag count cannot be greater than 50'); actual ('BadRequest', 'Bucket tags cannot be greater than 50').
Cause 3 — Expected ('InvalidArgument', 'Argument max-annotation-results must be an integer between 1 and 1000'); actual ('InvalidArgument', "Value '1001' at 'maxAnnotationResults' failed to satisfy constraint: Member must have value less than or equal to 1000").
Cause 4 — Expected ('InvalidRequest', 'The number of CORS rules should not exceed allowed limit of 100 rules.'); actual ('InvalidRequest', 'CORSConfiguration.CORSRules carries 101 rules, more than the 100 rules one cors configuration may hold').
Cause 5 — Expected ('InvalidTargetBucketForLogging', 'The target bucket for logging does not exist'); actual message 'The target bucket for logging does not exist, is not owned by you, or does not have the appropriate grants for the log-delivery group.'
Cause 6 — Expected code 'UnexpectedContent'; actual 'InvalidRequest'.
Cause 7 — Expected code 'NoSuchKey' for a permanently deleted version id; actual ('NoSuchVersion', 'The specified version does not exist.'). Expected AccessDenied message 'The KMS key must be from the same account as the bucket'; actual 'The Amazon Web Services KMS key in the request must be owned by the same account as the bucket. Modify the request to specify a KMS key from the same account, and then try again.'

## Steps to Reproduce
Cause 1: PutBucketPolicy with a policy document larger than 20480 bytes after normalisation.
Cause 2: PutBucketTagging with 51 tags.
Cause 3: ListObjectAnnotations with MaxAnnotationResults=1001.
Cause 4: PutBucketCors with 101 CORSRules, then GetBucketCors.
Cause 5: PutBucketLogging naming a TargetBucket that does not exist.
Cause 6: PutObjectAcl sending both x-amz-acl and an AccessControlPolicy body.
Cause 7: on a versioned bucket, PutObject then DeleteObject with the VersionId (permanent delete) and call UpdateObjectEncryption for that version id; separately call UpdateObjectEncryption naming a KMS key owned by another account.
