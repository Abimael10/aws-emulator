Issue-Source: trace-sweep-divergence
Issue-Trace: cloudfront-field-level-encryption-not-found-errors
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: cloudfront-field-level-encryption-not-found-errors cloudfront-fle-config-not-found-and-invalid-ifmatch-errors
Issue-Shape-Id: com.amazonaws.cloudfront#Cloudfront2020_05_31
Issue-Key: cloudfront|GetFieldLevelEncryptionConfig|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Key: cloudfront|GetFieldLevelEncryptionProfile|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Key: cloudfront|DeleteFieldLevelEncryptionConfig|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Key: cloudfront|DeleteFieldLevelEncryptionProfile|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Commit: 503762756f25d78271c850b842bfd982771d79b2

# CloudFront field-level encryption reads and deletes answer an unreadable identifier with the absent-resource message

> _Note: this issue was identified against the code base at commit `503762756f25d78271c850b842bfd982771d79b2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetFieldLevelEncryptionConfig

## Description
Four net-new causes, one shared fault: the four field-level-encryption operations addressed by an {Id} path label (the configuration's read and delete, the profile's read and delete) no longer distinguish an identifier value CloudFront refuses as UNREADABLE for the addressed resource kind from a well-formed identifier that names nothing. Every such value is now carried through to the state lookup, misses, and is answered with that resource's absent-record sentence. The HTTP status (404) and the error Code match AWS exactly in all four causes; only $.ErrorResponse.Error.Message diverges, and it diverges the SAME way on all four, which places the fault in the shared FLE id-admission path rather than in any one handler.

This is the OPPOSITE substitution from the already-RESOLVED bundle 20261124T141512Z-cloudfront-field-level-encryption-not-found-refusals-carry.md, which reported these same four operations answering a well-formed ABSENT id with 'Id format is invalid'. That resolution replaced a width-only test with a rule on the identifier alphabet and the kind's namespace letter; the four probes recorded here are values that rule now carries through to the store, while AWS still refuses them as unreadable. A resolved issue covers nothing, and the replay reports all four clusters below unowned. The closest OPEN issue, 20261125T014453Z-cloudfront-reads-report-a-malformed-identifier-with-the.md, reports exactly this direction of substitution but on GetDistributionConfig, GetFieldLevelEncryptionProfileConfig and GetPublicKey -- not on the four operations here, and it does not carry their replay keys. 20261109T120741Z-cloudfront-field-level-encryption-configs-lose-contenttypeprofileconfig-and.md is about FLE config payload round-tripping, not error wording.

Cause 1 of 4: GetFieldLevelEncryptionConfig words an unreadable configuration id as a missing configuration.
AWS: Smithy (cloudfront 2020-05-31) GetFieldLevelEncryptionConfig -- Id @required @httpLabel on GET /2020-05-31/field-level-encryption/{Id}/config, success 200; modeled errors AccessDenied (httpError 403) and NoSuchFieldLevelEncryptionConfig (httpError 404). Recorded behavior: a value that could not be an identifier CloudFront issued for the field-level-encryption configuration kind is answered 404 in the REST-XML ErrorResponse envelope with Message 'Id format is invalid'. That sentence is entirely FIXED TEXT for this class -- AWS interpolates nothing from the request or from state into it, not even the offending value -- so the requirement is the sentence itself and no recorded id is part of it.
Emulator: answers 404 with Message 'The specified field level encryption config does not exist.' -- the absent-record sentence, for a value that can never have named a configuration.
Why: a client cannot tell a request-construction bug (a truncated id, an id of another resource kind, an id pasted from another service) from a configuration that was deleted or never created. Create-if-absent and idempotent-teardown logic keyed on the absent-record sentence fires on input AWS requires it to reject and surface to the developer, and the genuine absent-id path becomes untestable because both paths emit the same body.
Fix: the shared FLE id-admission check the configuration read path calls, and GetFieldLevelEncryptionConfig's input validation in the cloudfront operation handler -- the unreadable-identifier branch must be decided before the state lookup and must keep its own message.
Traces: cloudfront-field-level-encryption-not-found-errors#task#1 (cluster also covers cloudfront-fle-config-not-found-and-invalid-ifmatch-errors#task#4).

Cause 2 of 4: GetFieldLevelEncryptionProfile words an unreadable profile id as a missing profile.
AWS: Smithy GetFieldLevelEncryptionProfile -- Id @required @httpLabel on GET /2020-05-31/field-level-encryption-profile/{Id}, success 200; modeled errors AccessDenied (403) and NoSuchFieldLevelEncryptionProfile (404). Recorded behavior: an unreadable profile id is answered 404 with Message 'Id format is invalid', wholly fixed text, nothing substituted from request or state.
Emulator: answers 404 with Message 'The specified field level encryption profile does not exist.'
Why: same loss of signal on the profile resource. A caller probing for a profile before creating one is told the profile is absent when its identifier is actually unusable, so it proceeds to a create that will also be refused instead of correcting the reference it was handed.
Fix: the same shared FLE id-admission check on the profile read path, plus GetFieldLevelEncryptionProfile's input validation in the cloudfront operation handler.
Traces: cloudfront-field-level-encryption-not-found-errors#task#2.

Cause 3 of 4: DeleteFieldLevelEncryptionConfig words an unreadable configuration id as a missing configuration.
AWS: Smithy DeleteFieldLevelEncryptionConfig -- Id @required @httpLabel, IfMatch @httpHeader('If-Match') on DELETE /2020-05-31/field-level-encryption/{Id}, success 204; modeled errors AccessDenied (403), FieldLevelEncryptionConfigInUse (409), InvalidIfMatchVersion (400), NoSuchFieldLevelEncryptionConfig (404), PreconditionFailed (412). Recorded behavior: deleting by an unreadable configuration id is answered 404 with Message 'Id format is invalid' -- fixed text in full.
Emulator: answers 404 with Message 'The specified field level encryption config does not exist.'
Why: idempotent-teardown code treats a not-found delete as success and anything else as a real failure; told the configuration is simply absent, cleanup silently completes on a request AWS rejects as malformed, hiding the bad identifier from the operator until a later operation fails on it.
Fix: the delete path's id admission in the cloudfront operation handler, sharing the configuration's id-admission check with the read path so both branches agree.
Traces: cloudfront-field-level-encryption-not-found-errors#task#3.

Cause 4 of 4: DeleteFieldLevelEncryptionProfile words an unreadable profile id as a missing profile.
AWS: Smithy DeleteFieldLevelEncryptionProfile -- Id @required @httpLabel, IfMatch @httpHeader('If-Match') on DELETE /2020-05-31/field-level-encryption-profile/{Id}, success 204; modeled errors AccessDenied (403), FieldLevelEncryptionProfileInUse (409), InvalidIfMatchVersion (400), NoSuchFieldLevelEncryptionProfile (404), PreconditionFailed (412). Recorded behavior: deleting by an unreadable profile id is answered 404 with Message 'Id format is invalid', fixed text in full.
Emulator: answers 404 with Message 'The specified field level encryption profile does not exist.'
Why: same as Cause 3 on the profile resource. That all four entry points -- two kinds, read and delete of each -- substitute the same way confirms the decision is made once, in the common FLE id-resolution path, and that the resolved bundle's rule now falls on the wrong side for this class of value.
Fix: the shared FLE id-resolution helper all four handlers call, so a value that cannot be an identifier of the addressed kind yields the unreadable-identifier refusal and only a resolution miss yields the kind's modeled absent-record sentence; the two branches must not be allowed to collapse into one again in either direction.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout (score 91.4%, 36 divergence clusters; these four are the only clusters the replay reports with no owning open issue and no blocking stub). In all four the HTTP status (404) and the error Code match AWS; only $.ErrorResponse.Error.Message diverges. The envelope's RequestId is per-run and not asserted.

Cause 1 -- fixture cloudfront-field-level-encryption-not-found-errors, step task#1, GetFieldLevelEncryptionConfig (cluster also affects cloudfront-fle-config-not-found-and-invalid-ifmatch-errors#task#4):
  expected: HTTP 404, $.ErrorResponse.Error.Message = 'Id format is invalid'
  actual:   HTTP 404, $.ErrorResponse.Error.Message = 'The specified field level encryption config does not exist.'
  divergence_key: service=cloudfront|GetFieldLevelEncryptionConfig|status=404->404|keys=$.ErrorResponse.Error.Message

Cause 2 -- fixture cloudfront-field-level-encryption-not-found-errors, step task#2, GetFieldLevelEncryptionProfile:
  expected: HTTP 404, $.ErrorResponse.Error.Message = 'Id format is invalid'
  actual:   HTTP 404, $.ErrorResponse.Error.Message = 'The specified field level encryption profile does not exist.'
  divergence_key: service=cloudfront|GetFieldLevelEncryptionProfile|status=404->404|keys=$.ErrorResponse.Error.Message

Cause 3 -- fixture cloudfront-field-level-encryption-not-found-errors, step task#3, DeleteFieldLevelEncryptionConfig:
  expected: HTTP 404, $.ErrorResponse.Error.Message = 'Id format is invalid'
  actual:   HTTP 404, $.ErrorResponse.Error.Message = 'The specified field level encryption config does not exist.'
  divergence_key: service=cloudfront|DeleteFieldLevelEncryptionConfig|status=404->404|keys=$.ErrorResponse.Error.Message

Cause 4 -- fixture cloudfront-field-level-encryption-not-found-errors, step task#4, DeleteFieldLevelEncryptionProfile:
  expected: HTTP 404, $.ErrorResponse.Error.Message = 'Id format is invalid'
  actual:   HTTP 404, $.ErrorResponse.Error.Message = 'The specified field level encryption profile does not exist.'
  divergence_key: service=cloudfront|DeleteFieldLevelEncryptionProfile|status=404->404|keys=$.ErrorResponse.Error.Message

Model evidence (services/service-com-amazonaws-cloudfront-cloudfront2020-05-31/smithy-model/com-amazonaws-cloudfront-cloudfront2020-05-31.json):
  GetFieldLevelEncryptionConfig -- GET /2020-05-31/field-level-encryption/{Id}/config, 200; Id @required @httpLabel; errors AccessDenied(403), NoSuchFieldLevelEncryptionConfig(404).
  GetFieldLevelEncryptionProfile -- GET /2020-05-31/field-level-encryption-profile/{Id}, 200; Id @required @httpLabel; errors AccessDenied(403), NoSuchFieldLevelEncryptionProfile(404).
  DeleteFieldLevelEncryptionConfig -- DELETE /2020-05-31/field-level-encryption/{Id}, 204; Id @required @httpLabel, IfMatch @httpHeader If-Match; errors AccessDenied(403), FieldLevelEncryptionConfigInUse(409), InvalidIfMatchVersion(400), NoSuchFieldLevelEncryptionConfig(404), PreconditionFailed(412).
  DeleteFieldLevelEncryptionProfile -- DELETE /2020-05-31/field-level-encryption-profile/{Id}, 204; Id @required @httpLabel, IfMatch @httpHeader If-Match; errors AccessDenied(403), FieldLevelEncryptionProfileInUse(409), InvalidIfMatchVersion(400), NoSuchFieldLevelEncryptionProfile(404), PreconditionFailed(412).

Fixed vs variable message text: 'Id format is invalid' is fixed text in full for all four operations -- no id, name, value, limit or member name from the request or from emulator state is interpolated into it, so no recorded value is part of the requirement. The emulator's two 'does not exist' sentences are likewise fixed text, and are the correct wording only for a readable identifier that names nothing.

Regression context: the resolved bundle 20261124T141512Z-cloudfront-field-level-encryption-not-found-refusals-carry.md asserted the reverse mapping on these same four operations and its keys were graded fixed; the four causes above show the admission rule introduced there now classifies this class of value on the absent-record side, so the two branches must be separated rather than re-swapped.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay trace set simparity-wire-aws-v1 (replay_wire_corpus).
2. Cause 1: replay fixture cloudfront-field-level-encryption-not-found-errors and observe step task#1 -- GET /2020-05-31/field-level-encryption/{Id}/config with an Id value CloudFront cannot read as an identifier of the field-level-encryption configuration kind. Expect 404 with Message 'Id format is invalid'; the emulator answers 404 with 'The specified field level encryption config does not exist.' Fixture cloudfront-fle-config-not-found-and-invalid-ifmatch-errors#task#4 reproduces it.
3. Cause 2: same fixture, step task#2 -- GET /2020-05-31/field-level-encryption-profile/{Id} with an unreadable profile id. Expect 404 with 'Id format is invalid'; the emulator answers 404 with 'The specified field level encryption profile does not exist.'
4. Cause 3: same fixture, step task#3 -- DELETE /2020-05-31/field-level-encryption/{Id} with an unreadable configuration id. Expect 404 with 'Id format is invalid'; the emulator answers the configuration's absent-record sentence.
5. Cause 4: same fixture, step task#4 -- DELETE /2020-05-31/field-level-encryption-profile/{Id} with an unreadable profile id. Expect 404 with 'Id format is invalid'; the emulator answers the profile's absent-record sentence.
6. In each case compare only $.ErrorResponse.Error.Message: the status (404) and the error Code already match, so the fix must change the message emitted on the unreadable-identifier branch WITHOUT regressing the readable-but-absent branch -- re-replay the fixtures of the resolved bundle 20261124T141512Z (cloudfront-field-level-encryption-errors, cloudfront-fle-profile-lifecycle) to confirm both branches now hold.
