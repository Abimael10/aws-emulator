Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: s3.s3-itc-put-rejected-for-a-config-id-over-sixty-four-characters
Issue-Shape-Id: com.amazonaws.s3#AmazonS3
Issue-Key: s3.s3-itc-put-rejected-for-a-config-id-over-sixty-four-characters|run#11|AssertionError|assert message == _INVALID_ID_MSG, (
Issue-Commit: 73ee5db70c54544c27a351cd5968d4682ba0e095

# s3: the InvalidConfigId refusal for an over-long Intelligent-Tiering Id carries emulator prose instead of AWS's sentence

> _Note: this issue was identified against the code base at commit `73ee5db70c54544c27a351cd5968d4682ba0e095`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutBucketIntelligentTieringConfiguration

## Description
One net-new root cause in the s3 behavioral suite. Every other failing s3 check in this replay is already OWNED by an open issue or marked ALREADY FILED as blocked by an unimplemented operation; this one is not covered by any of them.

Cause 1 of 1: PutBucketIntelligentTieringConfiguration refuses an Id longer than the allowed length with the right HTTP status and the right error code, but with the emulator's own wording instead of the sentence AWS returns for the InvalidConfigId class.

AWS: Smithy (com.amazonaws.s3#AmazonS3, PutBucketIntelligentTieringConfiguration) binds Id to the IntelligentTieringId shape as the required `id` query parameter and models no operation errors, so the refusal and its text are recorded behavior only. The recording shows that an Id outside the permitted length is refused with HTTP 400, Error Code `InvalidConfigId`, and the message

    Config Id should be more than 1 character and less than 64 characters in length.

That whole sentence is FIXED TEXT for this error class — it has no variable part taken from the request or from state: it does not interpolate the offending Id, the bucket, or the actual length. The length rule it states is itself part of the requirement, and the recording also shows the boundary AWS actually enforces: a 64-character Id is ACCEPTED and reads back verbatim, while a 65-character Id is refused, so the sentence's "less than 64" wording is AWS's prose and must be reproduced as-is rather than rewritten to match the enforced boundary. A refused Put must also store nothing: ListBucketIntelligentTieringConfigurations must still list only the previously accepted configuration.

Emulator: the status (400) and the code (`InvalidConfigId`) match, and the positive 64-character control and the "stores nothing" assertion are reached, but the message served is

    Config Id should not be more than 1 char and less than 64 chars in length

which differs from AWS's sentence in three ways: it inserts "not" (inverting the stated rule), abbreviates "character"/"characters" to "char"/"chars", and drops the terminating period. The check fails on exactly that comparison:

    AssertionError: the InvalidConfigId refusal must carry the exact message 'Config Id should be more than 1 character and less than 64 characters in length.'; got 'Config Id should not be more than 1 char and less than 64 chars in length'

Why: an error message is part of the contract a client sees. Callers, SDK error-message assertions, log scrapers and operator runbooks that match AWS's sentence (or merely read it) get a different, and semantically inverted, statement of the rule — the emulator's text says an Id must NOT be more than 1 character, which asserts the opposite of the constraint being enforced. Anyone diagnosing a rejected Intelligent-Tiering configuration against this emulator is told the wrong rule, and any test that pins AWS's wording fails against the emulator while passing against AWS.

Fix: the PutBucketIntelligentTieringConfiguration handler's Id validation in the s3 service crate — the branch that classifies an out-of-range Id as InvalidConfigId. It must emit AWS's fixed sentence verbatim (including the final period) for this class, while keeping the already-correct HTTP 400, the `InvalidConfigId` code, the accepted 64-character boundary, and the no-store-on-refusal behaviour.

Related open issue that does NOT cover this one, named only so the fixer can reuse its work: 20261102T111455Z-s3-intelligent-tiering-validation-and-absence-reads-diverge.md covers MalformedXML/NoSuchConfiguration/MalformedContinuationToken classification for the intelligent-tiering sub-resource and its replay keys are wire fixtures; it says nothing about the InvalidConfigId message text, and this behavioral check is not among its traces.

Traces: s3.s3-itc-put-rejected-for-a-config-id-over-sixty-four-characters.

## Evidence
Failing check: s3.s3-itc-put-rejected-for-a-config-id-over-sixty-four-characters — verdict [mismatch].

Replay detail, verbatim:
    File "check_s3_s3_itc_put_rejected_for_a_config_id_over_sixty_four_characters.py", line 36, in run
        assert message == _INVALID_ID_MSG, (
    AssertionError: the InvalidConfigId refusal must carry the exact message 'Config Id should be more than 1 character and less than 64 characters in length.'; got 'Config Id should not be more than 1 char and less than 64 chars in length'

Expected (AWS): PutBucketIntelligentTieringConfiguration with an Id longer than the permitted length is refused with
  - HTTP status: 400
  - Error.Code: InvalidConfigId
  - Error.Message: "Config Id should be more than 1 character and less than 64 characters in length." — entirely fixed text for this class, with no values substituted from the request or from emulator state.
And the refusal stores nothing, so ListBucketIntelligentTieringConfigurations continues to list only the previously accepted Id.

Actual (emulator): HTTP 400 and Error.Code InvalidConfigId are correct; Error.Message is "Config Id should not be more than 1 char and less than 64 chars in length" — an inverted ("should not be more than 1") and abbreviated ("char"/"chars") variant with no terminating period.

Scope note: the check's positive control (a 64-character Id accepted and read back verbatim by GetBucketIntelligentTieringConfiguration) and its "a refused Put must store nothing" listing assertion are both reached without failing, so this divergence is confined to the message text of the InvalidConfigId refusal and not to the surrounding validation or storage behaviour. Nothing in the diverging text is a per-run value (no request id, clock time, bucket name or generated id is involved), so the comparison is environment-independent.

## Steps to Reproduce
1. Build and boot the emulator from this checkout and point an AWS SDK/CLI at it.
2. s3.create_bucket(Bucket=<bucket>) (in us-east-1 omit CreateBucketConfiguration; elsewhere pass CreateBucketConfiguration={"LocationConstraint": <region>}).
3. Positive control: s3.put_bucket_intelligent_tiering_configuration(Bucket=<bucket>, Id="c"*64, IntelligentTieringConfiguration={"Id": "c"*64, "Status": "Enabled", "Tierings": [{"Days": 90, "AccessTier": "ARCHIVE_ACCESS"}]}) — expected to succeed, and s3.get_bucket_intelligent_tiering_configuration(Bucket=<bucket>, Id="c"*64) must read it back verbatim.
4. s3.put_bucket_intelligent_tiering_configuration(Bucket=<bucket>, Id="d"*65, IntelligentTieringConfiguration={"Id": "d"*65, "Status": "Enabled", "Tierings": [{"Days": 90, "AccessTier": "ARCHIVE_ACCESS"}]}).
   Observed: ClientError with HTTPStatusCode 400, Error.Code 'InvalidConfigId', Error.Message 'Config Id should not be more than 1 char and less than 64 chars in length'.
   Expected: HTTPStatusCode 400, Error.Code 'InvalidConfigId', Error.Message 'Config Id should be more than 1 character and less than 64 characters in length.' (exact, including the final period).
5. s3.list_bucket_intelligent_tiering_configurations(Bucket=<bucket>) must still list only the 64-character control Id (this part already holds).
