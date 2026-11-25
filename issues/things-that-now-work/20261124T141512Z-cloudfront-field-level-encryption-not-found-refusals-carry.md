Issue-Source: trace-sweep-divergence
Issue-Trace: cloudfront-field-level-encryption-errors
Issue-Step: task#2
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: cloudfront-field-level-encryption-errors cloudfront-fle-profile-lifecycle
Issue-Shape-Id: com.amazonaws.cloudfront#Cloudfront2020_05_31
Issue-Key: cloudfront|GetFieldLevelEncryptionConfig|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Key: cloudfront|GetFieldLevelEncryptionProfile|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Key: cloudfront|DeleteFieldLevelEncryptionConfig|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Key: cloudfront|DeleteFieldLevelEncryptionProfile|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Commit: 7a3560cb2ad6221574700506c0d4ad6cbe570c9e

# CloudFront field-level encryption not-found refusals carry the malformed-id message

> _Note: this issue was identified against the code base at commit `7a3560cb2ad6221574700506c0d4ad6cbe570c9e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetFieldLevelEncryptionConfig

## Description
Four causes, one shared fault: the field-level-encryption read and delete handlers conflate "this id is syntactically unusable" with "this id is well-formed but names nothing". A well-formed id that no resource carries is answered with the correct HTTP status and error code for a missing resource, but the message body is the emulator's syntax-rejection sentence 'Id format is invalid' instead of AWS's not-found sentence for that resource type. Status and code are right in all four; the Message member is wrong in all four.

Cause 1 of 4: GetFieldLevelEncryptionConfig's not-found refusal is worded as a malformed id.
AWS: Smithy: GetFieldLevelEncryptionConfig (GET /2020-05-31/field-level-encryption/{Id}/config, success 200) models NoSuchFieldLevelEncryptionConfig @httpError 404, documented "The specified configuration for field-level encryption doesn't exist." For a well-formed id that names no configuration AWS answers 404 in the REST-XML ErrorResponse envelope with Code 'NoSuchFieldLevelEncryptionConfig' and the recorded message "The specified field level encryption config does not exist." That sentence is entirely fixed text for this error class — it interpolates nothing from the request, not even the id.
Emulator: answers 404 with the right code but Message 'Id format is invalid' — a sentence about syntax, authored by the emulator, which AWS uses only when the id cannot be parsed as an id of that resource type at all.
Why: a caller cannot tell a typo-shaped id from a deleted or never-created configuration. Clients and operators branch on the message to decide whether to fix the request or to create the resource; told its id is malformed, a caller retries nothing and reports a client bug where AWS reports a missing resource. It also makes the genuine syntax refusal indistinguishable from the not-found path, so neither can be tested.
Fix: the GetFieldLevelEncryptionConfig handler's lookup-failure mapping — the absent-resource branch must emit the modeled NoSuchFieldLevelEncryptionConfig message, and the syntax-validation branch must stay distinct from it.
Traces: cloudfront-field-level-encryption-errors#task#2 (404/404, Message mismatch).

Cause 2 of 4: GetFieldLevelEncryptionProfile's not-found refusal is worded as a malformed id.
AWS: Smithy: GetFieldLevelEncryptionProfile (GET /2020-05-31/field-level-encryption-profile/{Id}, success 200) models NoSuchFieldLevelEncryptionProfile @httpError 404, documented "The specified profile for field-level encryption doesn't exist." A well-formed id naming no profile is answered 404 with Code 'NoSuchFieldLevelEncryptionProfile' and the fixed message "The specified field level encryption profile does not exist." — again wholly fixed text, nothing substituted from the request.
Emulator: answers 404 with Message 'Id format is invalid'.
Why: same loss of signal on the profile resource, and it also breaks the common create-if-absent pattern: a caller that probes for a profile before creating one is told its id is syntactically wrong rather than that the profile is missing, so it never proceeds to create.
Fix: the GetFieldLevelEncryptionProfile handler's lookup-failure mapping.
Traces: cloudfront-field-level-encryption-errors#task#4 (404/404, Message mismatch); also observed in fixture cloudfront-fle-profile-lifecycle#task#3.

Cause 3 of 4: DeleteFieldLevelEncryptionConfig's not-found refusal is worded as a malformed id.
AWS: Smithy: DeleteFieldLevelEncryptionConfig (DELETE /2020-05-31/field-level-encryption/{Id}, success 204) models NoSuchFieldLevelEncryptionConfig @httpError 404 alongside InvalidIfMatchVersion, PreconditionFailed and FieldLevelEncryptionConfigInUse. Deleting a well-formed id that names no configuration is answered 404 with Code 'NoSuchFieldLevelEncryptionConfig' and the same fixed sentence "The specified field level encryption config does not exist."
Emulator: answers 404 with Message 'Id format is invalid'.
Why: idempotent-teardown code treats a not-found delete as success and anything else as a real failure; a message claiming the id is malformed reads as a request defect, so cleanup paths log or raise instead of completing.
Fix: the DeleteFieldLevelEncryptionConfig handler's lookup-failure mapping, which should share the config-not-found error construction with the Get path.
Traces: cloudfront-field-level-encryption-errors#task#5 (404/404, Message mismatch).

Cause 4 of 4: DeleteFieldLevelEncryptionProfile's not-found refusal is worded as a malformed id.
AWS: Smithy: DeleteFieldLevelEncryptionProfile (DELETE /2020-05-31/field-level-encryption-profile/{Id}, success 204) models NoSuchFieldLevelEncryptionProfile @httpError 404. A well-formed absent id is answered 404 with Code 'NoSuchFieldLevelEncryptionProfile' and the fixed sentence "The specified field level encryption profile does not exist."
Emulator: answers 404 with Message 'Id format is invalid'.
Why: same as Cause 3 on the profile resource; that all four read and delete entry points share the wording shows the substitution is in the common not-found construction for these resources rather than in any one handler.
Fix: the DeleteFieldLevelEncryptionProfile handler's lookup-failure mapping, plus the shared FLE id-resolution helper the four handlers call, so a resolution miss yields the resource's modeled not-found message and only an unparsable id yields the syntax refusal.
Traces: cloudfront-field-level-encryption-errors#task#6 (404/404, Message mismatch).

Related open issues, which do NOT cover these four: 20261117T051410Z-cloudfront-getfieldlevelencryptionprofile-answers-the-not-found-message-for-an.md reports the opposite substitution (an id belonging to another resource type answered with the not-found sentence where AWS says 'Id format is invalid'); 20261115T235211Z-cloudfront-getdistributionconfig-and-getoriginaccesscontrol-report-absent-ids-as.md and 20261123T232502Z-cloudfront-identifier-and-arn-syntax-refusals-carry-the.md cover the same family of confusion on distribution, origin-access-control and ARN surfaces, not the field-level-encryption config/profile read and delete operations.

## Evidence
Replay of simparity-wire-aws-v1 against the emulator built from this checkout. In all four the HTTP status (404) and the error Code match AWS; only ErrorResponse/Error/Message diverges.

1) cloudfront-field-level-encryption-errors#task#2 GetFieldLevelEncryptionConfig
   $.ErrorResponse.Error.Message: expected 'The specified field level encryption config does not exist.', got 'Id format is invalid'

2) cloudfront-field-level-encryption-errors#task#4 GetFieldLevelEncryptionProfile
   $.ErrorResponse.Error.Message: expected 'The specified field level encryption profile does not exist.', got 'Id format is invalid'
   (same cluster also affects cloudfront-fle-profile-lifecycle#task#3)

3) cloudfront-field-level-encryption-errors#task#5 DeleteFieldLevelEncryptionConfig
   $.ErrorResponse.Error.Message: expected 'The specified field level encryption config does not exist.', got 'Id format is invalid'

4) cloudfront-field-level-encryption-errors#task#6 DeleteFieldLevelEncryptionProfile
   $.ErrorResponse.Error.Message: expected 'The specified field level encryption profile does not exist.', got 'Id format is invalid'

Model evidence (services/service-com-amazonaws-cloudfront-cloudfront2020-05-31 Smithy model): GetFieldLevelEncryptionConfig and DeleteFieldLevelEncryptionConfig both declare NoSuchFieldLevelEncryptionConfig (httpError 404, doc "The specified configuration for field-level encryption doesn't exist."); GetFieldLevelEncryptionProfile and DeleteFieldLevelEncryptionProfile both declare NoSuchFieldLevelEncryptionProfile (httpError 404, doc "The specified profile for field-level encryption doesn't exist.").

Fixed vs variable message text: for both not-found classes the entire sentence is fixed text — AWS interpolates no id, name or value into it, so the requirement is the sentence itself, not any recorded id. The envelope's RequestId is per-run and not asserted.

## Steps to Reproduce
1. Replay the wire corpus against the emulator (replay_wire_corpus on this checkout).
2. Fixture cloudfront-field-level-encryption-errors, step task#2: call GetFieldLevelEncryptionConfig with a well-formed field-level-encryption config id that no configuration carries. Observe 404 with Code NoSuchFieldLevelEncryptionConfig but Message 'Id format is invalid' instead of 'The specified field level encryption config does not exist.'
3. Step task#4: call GetFieldLevelEncryptionProfile with a well-formed absent profile id. Observe Message 'Id format is invalid' instead of 'The specified field level encryption profile does not exist.'
4. Step task#5: call DeleteFieldLevelEncryptionConfig with a well-formed absent config id. Observe the same substituted message.
5. Step task#6: call DeleteFieldLevelEncryptionProfile with a well-formed absent profile id. Observe the same substituted message.
6. Fixture cloudfront-fle-profile-lifecycle, step task#3, reproduces the profile read case after a delete.

## Resolution

All four causes are closed; the replay reports all four of this issue's keys `fixed` and both
graded fixtures passing end to end.

What separates the two sentences the one not-found code carries is whether the submitted value
could be an identifier CloudFront ISSUED FOR THE ADDRESSED KIND
(`state::id_format_refused_for`). Only two things about a value say it could not: it is written
outside the alphabet an identifier is written in (digits and uppercase letters, no separator and
no lower case), or it is written exactly the way an identifier is issued — that alphabet at the
width every identifier of this mint is issued at — under the namespace letter of another kind
this service issues identifiers under, which makes it that kind's identifier. Every other value
is carried through to the store, and the record the store holds none under is the record reported
ABSENT: the kind's own letter at any width, a letter no kind of this service issues an identifier
under, and any value of the alphabet at a width no identifier is issued at, since the width this
service issues at is one width CloudFront writes an identifier in rather than the only one.

The condition was previously read as the WIDTH alone, which answered the unreadable-identifier
sentence for every value of the alphabet written at another width — the shape all four recorded
probes of `cloudfront-field-level-encryption-errors` take — and answered the absent-record
sentence for another kind's issued identifier, which is the opposite substitution reported by
`20261117T051410Z-cloudfront-getfieldlevelencryptionprofile-answers-the-not-found-message-for-an.md`.
Both halves now follow from the one rule, on every operation addressed by one of these
identifiers: the configuration's `/config` read, the profile's two reads, and the update and the
delete of each. The configuration's RESOURCE read (`GetFieldLevelEncryption`) keeps its own
recorded contract of reading no form at all.
