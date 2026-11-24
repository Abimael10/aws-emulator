Issue-Source: trace-sweep-divergence
Issue-Trace: cloudfront-field-level-encryption-not-found-errors
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: cloudfront-field-level-encryption-not-found-errors cloudfront-fle-config-not-found-and-invalid-ifmatch-errors cloudfront-field-level-encryption-profile-config-get-and-not-found
Issue-Shape-Id: com.amazonaws.cloudfront#Cloudfront2020_05_31
Issue-Key: cloudfront|GetFieldLevelEncryptionConfig|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Key: cloudfront|GetFieldLevelEncryptionProfile|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Key: cloudfront|DeleteFieldLevelEncryptionConfig|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Key: cloudfront|GetFieldLevelEncryptionProfileConfig|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Commit: 84412ef57d49d3e319b2b9326649851b03084832

# CloudFront REOPENS: field-level-encryption lookups report the resource missing for a malformed id

> _Note: this issue was identified against the code base at commit `84412ef57d49d3e319b2b9326649851b03084832`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetFieldLevelEncryptionConfig

## Description
Four causes, one shared fault: when the id in the request path does not have CloudFront's id form, the field-level-encryption handlers go straight to a state lookup and report the resource as ABSENT, instead of reporting that the identifier itself is malformed. The HTTP status (404) and the error code already match in all four clusters; only Error.Message diverges, and for this error class the message is entirely FIXED text - 'Id format is invalid' has no variable part, nothing in it comes from the request or from state - so reproducing it exactly is the whole requirement.

This REOPENS the resolved issue 20261109T120741Z-cloudfront-field-level-encryption-lookups-report-the-resource.md: the same four operations, the same four replay keys and the same expected/actual messages still diverge in this replay, and a resolved issue does not own a live failure. The open issue 20261105T231732Z-cloudfront-path-identifiers-are-not-format-validated-before.md describes the same fault for the distribution/OAC/function routes and owns those clusters (this replay still shows GetOriginAccessControl under it); it does NOT carry the replay keys of the four FLE clusters below, so they are filed here.

Cause 1 of 4: GetFieldLevelEncryptionConfig.
AWS: 404 whose ErrorResponse.Error.Message is the fixed text 'Id format is invalid' - recorded behavior only; the Smithy model supplies the error shape (GetFieldLevelEncryptionConfig errors: NoSuchFieldLevelEncryptionConfig, httpError 404; http {GET /2020-05-31/field-level-encryption/{Id}/config, 200}; Id is a required httpLabel) but no message text, and the recording shows AWS judging the path identifier's FORMAT before any state lookup.
Emulator: 404 with 'The specified field level encryption config does not exist.' - the not-found message that belongs to a well-formed id naming an absent resource.
Why: these are two different client contracts. A caller that distinguishes 'this id cannot exist' (a bug in its own id handling, not retryable) from 'this resource is gone' (recreate/retry) takes the wrong branch, and a test asserting the invalid-id-format message fails against the emulator.
Fix: GetFieldLevelEncryptionConfig's request validation - validate the path id's format before the state lookup and refuse with the invalid-id-format message.
Traces: cloudfront-field-level-encryption-not-found-errors#task#1 (2 affected steps, incl. cloudfront-fle-config-not-found-and-invalid-ifmatch-errors#task#4).

Cause 2 of 4: GetFieldLevelEncryptionProfile.
AWS: 404 with Error.Message 'Id format is invalid' (recorded behavior only; Smithy gives NoSuchFieldLevelEncryptionProfile, httpError 404, for GET /2020-05-31/field-level-encryption-profile/{Id}).
Emulator: 'The specified field level encryption profile does not exist.'
Why/Fix: as cause 1, in GetFieldLevelEncryptionProfile's request validation.
Traces: cloudfront-field-level-encryption-not-found-errors#task#2.

Cause 3 of 4: DeleteFieldLevelEncryptionConfig.
AWS: 404 with Error.Message 'Id format is invalid' (recorded behavior only; Smithy: DELETE /2020-05-31/field-level-encryption/{Id}, errors incl. NoSuchFieldLevelEncryptionConfig 404, InvalidIfMatchVersion 400, PreconditionFailed 412 - the id-format refusal precedes all of them).
Emulator: 'The specified field level encryption config does not exist.'
Why/Fix: as cause 1, in DeleteFieldLevelEncryptionConfig's request validation.
Traces: cloudfront-field-level-encryption-not-found-errors#task#3.

Cause 4 of 4: GetFieldLevelEncryptionProfileConfig.
AWS: 404 with Error.Message 'Id format is invalid' (recorded behavior only; Smithy: GET /2020-05-31/field-level-encryption-profile/{Id}/config, errors NoSuchFieldLevelEncryptionProfile 404).
Emulator: 'The specified field level encryption profile does not exist.'
Why/Fix: as cause 1, in GetFieldLevelEncryptionProfileConfig's request validation.
Traces: cloudfront-field-level-encryption-profile-config-get-and-not-found#task#4.

The malformed ids used by the fixtures are per-run request values; the requirement is the branch (format check before lookup) and the exact fixed message text on all four routes.

## Evidence
Replay of simparity-wire-aws-v1 (emulator_up=true; 1303 steps, 1151 passed). Four unowned mismatch clusters, each expected_status 404 == actual_status 404, failing only $.ErrorResponse.Error.Message (no stub_path, no owned_by on any of them):
1. GetFieldLevelEncryptionConfig 404->404 - cloudfront-field-level-encryption-not-found-errors#task#1: expected 'Id format is invalid', got 'The specified field level encryption config does not exist.' (2 affected steps, incl. cloudfront-fle-config-not-found-and-invalid-ifmatch-errors#task#4).
2. GetFieldLevelEncryptionProfile 404->404 - cloudfront-field-level-encryption-not-found-errors#task#2: expected 'Id format is invalid', got 'The specified field level encryption profile does not exist.'
3. DeleteFieldLevelEncryptionConfig 404->404 - cloudfront-field-level-encryption-not-found-errors#task#3: expected 'Id format is invalid', got 'The specified field level encryption config does not exist.'
4. GetFieldLevelEncryptionProfileConfig 404->404 - cloudfront-field-level-encryption-profile-config-get-and-not-found#task#4: expected 'Id format is invalid', got 'The specified field level encryption profile does not exist.'
The previously resolved issue for these exact four keys (20261109T120741Z-cloudfront-field-level-encryption-lookups-report-the-resource.md) no longer owns them, and the behavior is unchanged from what it recorded.

## Steps to Reproduce
1. git clone the repo, boot the emulator built from that checkout, replay the wire corpus (simparity-wire-aws-v1).
2. Representative step: fixture cloudfront-field-level-encryption-not-found-errors, step task#1 - GET /2020-05-31/field-level-encryption/<id that is not in CloudFront's id form>/config; the emulator answers 404 with 'The specified field level encryption config does not exist.' instead of 'Id format is invalid'.
3. Same fixture task#2 (GetFieldLevelEncryptionProfile) and task#3 (DeleteFieldLevelEncryptionConfig) - same substitution on each route, same wrong message.
4. cloudfront-field-level-encryption-profile-config-get-and-not-found#task#4 (GetFieldLevelEncryptionProfileConfig) - same.
5. In each case the status (404) and the error code already match; compare only Error.Message.
