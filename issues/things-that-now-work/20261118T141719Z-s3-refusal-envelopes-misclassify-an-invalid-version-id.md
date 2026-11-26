Issue-Source: trace-sweep-divergence
Issue-Trace: s3-csvcols-encryption-header-mistakes
Issue-Step: task#5
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: s3-csvcols-encryption-header-mistakes s3-ovm-traj-version-id-argument-mistakes s3-put-rejects-second-prefix-rule s3-refarch-s3ev-bucket-addressing-and-filter-name-mistakes
Issue-Shape-Id: com.amazonaws.s3#AmazonS3
Issue-Key: s3|GetObject|4xx->4xx|$.Error.ArgumentName
Issue-Key: s3|GetObjectAttributes|4xx->4xx|$.Error.Code
Issue-Key: s3|GetObjectAttributes|4xx->4xx|status
Issue-Key: s3|PutBucketNotificationConfiguration|4xx->4xx|$.Error.ArgumentName
Issue-Related: 20261102T111455Z-s3-refusal-envelopes-omit-aws-contextual-error-members.md
Issue-Commit: 35e32963f89bfa2c86c6d7f7351ac2825588f64a

# s3 refusal envelopes misclassify an invalid version id and drop the contextual ArgumentName/ArgumentValue members

> _Note: this issue was identified against the code base at commit `35e32963f89bfa2c86c6d7f7351ac2825588f64a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetObjectAttributes

## Description
Three net-new causes where S3 refuses the right request but the refusal envelope is not AWS's: the contextual members that name the offending argument are missing or wrong, and on one path the whole error class and HTTP status are wrong. Found by replaying the wire corpus (trace set simparity-wire-aws-v1).

Cause 1 of 3: GetObject refused for an unsupported x-amz-server-side-encryption request header omits Error.ArgumentName.
AWS: an InvalidArgument refusal names the offending argument in the error document — the Error element carries ArgumentName set to the wire name of the rejected input, here the request header 'x-amz-server-side-encryption'. Smithy (com.amazonaws.s3#AmazonS3) models the S3 REST-XML error document; ArgumentName/ArgumentValue are the contextual members S3 adds to an InvalidArgument envelope, so their presence is recorded behavior.
Emulator: status 400 and the code are right, but Error.ArgumentName is absent.
Why: a client that reports which argument S3 objected to (or branches on it) has nothing to read, so the refusal is indistinguishable from any other InvalidArgument.
Fix: the GetObject request-header validation path and the error serializer that builds its InvalidArgument envelope — ArgumentName must carry the rejected header's wire name.
Traces: s3-csvcols-encryption-header-mistakes#task#5.

Cause 2 of 3: GetObjectAttributes with an unparseable versionId is answered 404 NoSuchVersion instead of 400 InvalidArgument.
AWS: a version id that is not a valid version id is an argument fault, not a missing resource: the refusal is 400 with code InvalidArgument, message 'Invalid version id specified' (fixed text, no variable parts), ArgumentName 'versionId' (the wire query parameter) and ArgumentValue set to the version id supplied in the request. Smithy: GetObjectAttributes.VersionId is httpQuery 'versionId'; the operation's only modelled error is NoSuchKey (404), so the InvalidArgument classification is recorded behavior.
Emulator: 404 with code NoSuchVersion and message 'The specified version does not exist.', and neither ArgumentName nor ArgumentValue.
Why: a client distinguishing 'you sent a malformed version id' (fix the request) from 'that version is gone' (fix the state) takes the wrong branch, and retries/404-handling fire on what is a permanent 400.
Fix: GetObjectAttributes' versionId parsing and its error classification (validate the argument before resolving it against object state).
Traces: s3-ovm-traj-version-id-argument-mistakes#task#9.

Cause 3 of 3: PutBucketNotificationConfiguration's InvalidArgument names the wrong argument.
AWS: when a notification filter rule's Name is not an accepted filter-rule name, the refusal's Error.ArgumentName is the dotted path of the offending element, 'FilterRule.Name' — the member actually at fault, not its parent. Smithy: FilterRule.Name targets FilterRuleName, an enum with values ['prefix', 'suffix'], inside NotificationConfigurationFilter -> S3KeyFilter.
Emulator: 400 and the code are right, but ArgumentName is 'FilterRule' — the container, not the member.
Why: a client that points the user at the offending element names the whole rule instead of its Name field, and a tool keyed on the exact argument name does not match.
Fix: PutBucketNotificationConfiguration's filter-rule validation — report the failing member's dotted wire path in ArgumentName.
Traces: s3-put-rejects-second-prefix-rule#task#5 (cluster also covers s3-refarch-s3ev-bucket-addressing-and-filter-name-mistakes#task#7).

Related but not covering these: the open issue '20261102T111455Z-s3-refusal-envelopes-omit-aws-contextual-error-members.md' owns the GetObjectAttributes x-amz-object-attributes and PutBucketEncryption instances of the missing-ArgumentName family, and '20261030T053639Z-s3-refusals-use-the-wrong-error-code-and.md' owns the PutObject envelope instances; neither carries a replay key for the three steps above, so they are filed here with their own fixture bindings.

## Evidence
Cause 1 — GetObject with an x-amz-server-side-encryption request header: status 400 as expected, but $.Error.ArgumentName expected 'x-amz-server-side-encryption', got None.
Cause 2 — GetObjectAttributes with an invalid versionId: expected status 400, got 404; $.Error.Code expected 'InvalidArgument', got 'NoSuchVersion'; $.Error.Message expected 'Invalid version id specified' (fixed text), got 'The specified version does not exist.'; $.Error.ArgumentName expected 'versionId', got None; $.Error.ArgumentValue expected the version id from the request (recording: '0f2d66d2c3f9b43c7c0e5b2c6d0e1bd5' — a per-run value filling the template), got None.
Cause 3 — PutBucketNotificationConfiguration with a bad filter-rule name: $.Error.ArgumentName expected 'FilterRule.Name', got 'FilterRule'.

## Steps to Reproduce
Cause 1: PutObject an object, then GetObject sending an x-amz-server-side-encryption request header; read Error.ArgumentName from the 400 body. Replay fixture s3-csvcols-encryption-header-mistakes#task#5.
Cause 2: on a versioned bucket, call GetObjectAttributes with versionId set to a string that is not a valid version id; expect 400 InvalidArgument 'Invalid version id specified' with ArgumentName 'versionId' and ArgumentValue echoing the supplied id. Replay fixture s3-ovm-traj-version-id-argument-mistakes#task#9.
Cause 3: PutBucketNotificationConfiguration with an S3Key FilterRule whose Name is not 'prefix' or 'suffix'; read Error.ArgumentName. Replay fixtures s3-put-rejects-second-prefix-rule#task#5 and s3-refarch-s3ev-bucket-addressing-and-filter-name-mistakes#task#7.
