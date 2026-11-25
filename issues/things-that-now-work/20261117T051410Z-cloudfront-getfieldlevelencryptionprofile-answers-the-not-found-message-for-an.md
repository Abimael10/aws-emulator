Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudfront.get-field-level-encryption-profile-type-confusion-rejected
Issue-Shape-Id: com.amazonaws.cloudfront#Cloudfront2020_05_31
Issue-Key: cloudfront.get-field-level-encryption-profile-type-confusion-rejected|run#12|AssertionError|assert message == "Id format is invalid", (
Issue-Commit: 2c138c8eaec3f3a9f26bdccb5a666b1a3044732b

# GetFieldLevelEncryptionProfile answers the not-found message for an id of another resource type instead of 'Id format is invalid'

> _Note: this issue was identified against the code base at commit `2c138c8eaec3f3a9f26bdccb5a666b1a3044732b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetFieldLevelEncryptionProfile

## Description
One net-new cause.

Cause 1 of 1: GetFieldLevelEncryptionProfile does not refuse an Id that is not a field-level-encryption-profile id on its FORM; it goes straight to a state lookup and reports the profile as absent. The id used here is not garbage — it is a real, existing PublicKey id created in the same account moments earlier (GetPublicKey resolves it), i.e. a well-formed CloudFront id carrying ANOTHER resource type's shape. The HTTP status (404) and the error code (NoSuchFieldLevelEncryptionProfile) already match; only Error.Message diverges.

AWS: 404 / NoSuchFieldLevelEncryptionProfile whose ErrorResponse.Error.Message is the fixed text 'Id format is invalid' — recorded behavior only. The Smithy model supplies only the error shape and route (GetFieldLevelEncryptionProfile: http {GET /2020-05-31/field-level-encryption-profile/{Id}, 200}; Id is a required httpLabel of type string with no modeled pattern; errors AccessDenied httpError 403, NoSuchFieldLevelEncryptionProfile httpError 404) and no message text. The recording shows AWS judging the identifier's form against the FLE-profile id type before any state lookup, so an id belonging to a different resource type (PublicKey, per GetPublicKey: GET /2020-05-31/public-key/{Id}, errors NoSuchPublicKey 404) is refused on format rather than resolved or reported absent. The message for this error class is entirely FIXED text: 'Id format is invalid' has no variable part — nothing in it comes from the request or from state — so reproducing it exactly is the whole requirement. The id in the replay is a per-run generated value and is not part of the requirement.

Emulator: 404 / NoSuchFieldLevelEncryptionProfile with Error.Message 'The specified field level encryption profile does not exist.' — the absent-resource message that belongs to a well-formed FLE-profile id naming a resource that is not there.

Why: these are two different client contracts. A caller that distinguishes 'this id can never name a profile' (its own id-plumbing bug or a cross-resource-type mix-up — not retryable, no recreate) from 'this profile is gone' (recreate/retry) takes the wrong branch, and cross-resource-type isolation can no longer be asserted: the emulator makes a PublicKey id look merely missing as a profile instead of invalid as one. Any test asserting AWS's invalid-id-format message fails against the emulator.

Fix: GetFieldLevelEncryptionProfile's request validation — validate the path id's form against the field-level-encryption-profile id type before the state lookup, and refuse with the invalid-id-format message; do not fall through to the not-found message.

Related, does NOT cover this: open issue 20261114T142904Z-cloudfront-reopens-field-level-encryption-lookups-report-the.md describes the same fault for MALFORMED ids on four FLE routes (and carries the wire replay key cloudfront|GetFieldLevelEncryptionProfile|4xx->4xx|$.ErrorResponse.Error.Message), but it does not carry this behavioral check and its causes are stated for ids that are not in CloudFront's id form at all; this check exercises a well-formed id of a different, existing resource type. It is filed here so the type-confusion case is replayed; a fixer closing both should expect one shared validation branch.

Traces: cloudfront.get-field-level-encryption-profile-type-confusion-rejected.

## Evidence
Behavioral suite simparity-behavioral-aws-v1, 510/632 passed; check cloudfront.get-field-level-encryption-profile-type-confusion-rejected [mismatch], the only failing check in the run not already owned by an open issue or blocked by an unimplemented stub.

Failure detail:
  check_cloudfront_get_field_level_encryption_profile_type_confusion_rejected.py, line 51, in run
  assert message == "Id format is invalid"
  AssertionError: expected AWS's exact message, got 'The specified field level encryption profile does not exist.'

Sequence in the check: CreatePublicKey(PublicKeyConfig{CallerReference, Name, EncodedKey=<RSA 2048 PEM>, Comment}) -> PublicKey.Id; positive control GetPublicKey(Id=<that id>) echoes the id (passes, so the id is real and well-formed); then GetFieldLevelEncryptionProfile(Id=<that same PublicKey id>).

Expected (AWS): error code NoSuchFieldLevelEncryptionProfile (HTTP 404) with Error.Message exactly 'Id format is invalid' — fixed text, no variable parts.
Actual (emulator): error code NoSuchFieldLevelEncryptionProfile (HTTP 404) — code and status match — with Error.Message 'The specified field level encryption profile does not exist.'
The check's code assertion passed; only the message assertion failed.

## Steps to Reproduce
1. Clone the repo and boot the emulator built from that checkout.
2. Replay the behavioral suite (simparity-behavioral-aws-v1) and run check cloudfront.get-field-level-encryption-profile-type-confusion-rejected, or by hand:
   a. cf.create_public_key(PublicKeyConfig={CallerReference: <unique>, Name: <unique>, EncodedKey: <RSA 2048 public key PEM>, Comment: ...}) and keep PublicKey.Id.
   b. cf.get_public_key(Id=<that id>) — succeeds, confirming the id is real and well-formed.
   c. cf.get_field_level_encryption_profile(Id=<that same PublicKey id>).
3. Inspect the refusal: status and Error.Code (404 / NoSuchFieldLevelEncryptionProfile) already match AWS; compare only Error.Message. The emulator returns 'The specified field level encryption profile does not exist.' where AWS returns 'Id format is invalid'.
4. Clean up with cf.get_public_key(Id=...)['ETag'] then cf.delete_public_key(Id=..., IfMatch=<etag>).
