Issue-Source: trace-sweep-divergence
Issue-Trace: cloudfront-cfode-distribution-lookup-identifier-mistakes
Issue-Step: task#4
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: cloudfront-cfode-distribution-lookup-identifier-mistakes cloudfront-field-level-encryption-profile-config-get-and-not-found cloudfront-get-public-key-wrong-resource-type-id-rejected cloudfront-public-key-and-key-group-lifecycle
Issue-Shape-Id: com.amazonaws.cloudfront#Cloudfront2020_05_31
Issue-Key: cloudfront|GetDistributionConfig|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Key: cloudfront|GetFieldLevelEncryptionProfileConfig|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Key: cloudfront|GetPublicKey|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Commit: 6bae9ee14af172aae629fb16389ee320a8de1819

# CloudFront reads report a malformed identifier with the resource-absent message instead of 'Id format is invalid'

> _Note: this issue was identified against the code base at commit `6bae9ee14af172aae629fb16389ee320a8de1819`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetDistributionConfig

## Description
Three net-new causes, one shared fault: a CloudFront single-resource read that is handed a SYNTACTICALLY unusable identifier (an id that is not in the resource-id form the path label accepts, e.g. an id belonging to a different resource type) is answered with the handler's resource-absent sentence rather than AWS's syntax-rejection sentence. In all three the HTTP status (404) and the error Code match AWS exactly; only ErrorResponse/Error/Message diverges. The emulator performs no syntactic admission check on the {Id} label at all: it goes straight to the state lookup, misses, and emits the 'does not exist' message, so the malformed-id path and the well-formed-but-absent path are indistinguishable on the wire.

This is the OPPOSITE substitution from the already-open field-level-encryption bundle (20261124T141512Z-cloudfront-field-level-encryption-not-found-refusals-carry.md), which reports well-formed absent ids answered with 'Id format is invalid'. Both substitutions exist because the two branches are not separated at all.

Cause 1 of 3: GetDistributionConfig words a malformed distribution Id as a missing distribution.
AWS: Smithy (cloudfront 2020-05-31) GetDistributionConfig — input member Id is @required and @httpLabel on GET /2020-05-31/distribution/{Id}/config (success 200); modeled errors AccessDenied (httpError 403) and NoSuchDistribution (httpError 404). Recorded behavior: an Id that is not a well-formed CloudFront distribution id is answered 404 in the REST-XML ErrorResponse envelope with the not-found Code and Message 'Id format is invalid'. That message is ENTIRELY FIXED TEXT for this class — AWS interpolates nothing from the request or from state into it, not even the offending id — so the requirement is the sentence itself.
Emulator: answers 404 with Message 'The specified distribution does not exist.' — its state-lookup-miss sentence, for an id that never could have named a distribution.
Why: a client cannot distinguish a request-construction bug (it pasted an id of the wrong resource type, or a truncated/typo id) from a distribution that was deleted or never created. Create-if-absent and idempotent-teardown logic keyed on the not-found message fires on input it must instead reject and surface to the developer, and the genuine absent-id path becomes untestable because both paths emit the same body.
Fix: GetDistributionConfig's input validation in the cloudfront operation handler — validate the Id label's format before the state lookup and keep that refusal's message distinct from the lookup-miss message.
Traces: cloudfront-cfode-distribution-lookup-identifier-mistakes#task#4 (cluster also covers #task#5).

Cause 2 of 3: GetFieldLevelEncryptionProfileConfig words a malformed profile Id as a missing profile.
AWS: Smithy GetFieldLevelEncryptionProfileConfig — Id @required @httpLabel on GET /2020-05-31/field-level-encryption-profile/{Id}/config (success 200); modeled errors AccessDenied (403) and NoSuchFieldLevelEncryptionProfile (404). Recorded behavior: a malformed profile id is answered 404 with Message 'Id format is invalid' — again wholly fixed text, nothing substituted from request or state.
Emulator: answers 404 with Message 'The specified field level encryption profile does not exist.'
Why: same loss of signal on the FLE profile resource. A caller probing for a profile before creating one is told the profile is absent when its id is actually unusable, so it proceeds to a create that will also be rejected, instead of correcting the identifier.
Fix: GetFieldLevelEncryptionProfileConfig's input validation in the cloudfront operation handler, and the shared FLE id-resolution helper the config/profile read paths call, so an unparsable id yields the syntax refusal and only a resolution miss yields the modeled not-found sentence.
Traces: cloudfront-field-level-encryption-profile-config-get-and-not-found#task#4.

Cause 3 of 3: GetPublicKey words an id of the wrong resource type as a missing public key.
AWS: Smithy GetPublicKey — Id @required @httpLabel on GET /2020-05-31/public-key/{Id} (success 200); modeled errors AccessDenied (403) and NoSuchPublicKey (404). Recorded behavior: an Id that is not a CloudFront public-key id (the fixture submits an id of another CloudFront resource type) is answered 404 with Message 'Id format is invalid', fixed text in full.
Emulator: answers 404 with Message 'The specified CloudFront public key does not exist.'
Why: key-rotation and signed-URL setup code reads this message to decide whether to re-create the key or to fix the reference it was handed; told the key is absent, it creates a duplicate key rather than reporting the bad reference. That the same substitution appears on three unrelated resources (distribution, FLE profile, public key) shows it is in the common 'read by id' path, not in any one handler.
Fix: GetPublicKey's input validation in the cloudfront operation handler, plus the shared read-by-id admission check the three handlers go through, so label-format rejection happens before the state lookup across all of them.
Traces: cloudfront-get-public-key-wrong-resource-type-id-rejected#task#3 (cluster also covers cloudfront-public-key-and-key-group-lifecycle#task#10).

Related open issues, which do NOT cover these three (the replay reports all three clusters as unowned, so none of them carries these replay keys): 20261117T051410Z-cloudfront-getfieldlevelencryptionprofile-answers-the-not-found-message-for-an.md reports exactly this substitution on GetFieldLevelEncryptionProfile (not on its Config variant, not on GetDistributionConfig, not on GetPublicKey); 20261118T131652Z-cloudfront-getoriginaccesscontrolconfig-reports-a-well-formed-absent-id.md and 20261124T141512Z-cloudfront-field-level-encryption-not-found-refusals-carry.md report the reverse substitution on other operations; 20261123T232502Z-cloudfront-identifier-and-arn-syntax-refusals-carry-the.md covers GetAnycastIpList and ListDistributionsByOwnedResource, where the status and Code diverge too.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout. In all three causes the HTTP status (404) and the error Code match AWS; only $.ErrorResponse.Error.Message diverges. The envelope's RequestId is per-run and not asserted.

Cause 1 — fixture cloudfront-cfode-distribution-lookup-identifier-mistakes, step task#4, GetDistributionConfig (cluster also affects #task#5):
  expected: HTTP 404, $.ErrorResponse.Error.Message = 'Id format is invalid'
  actual:   HTTP 404, $.ErrorResponse.Error.Message = 'The specified distribution does not exist.'
  divergence_key: service=cloudfront|GetDistributionConfig|status=404->404|keys=$.ErrorResponse.Error.Message

Cause 2 — fixture cloudfront-field-level-encryption-profile-config-get-and-not-found, step task#4, GetFieldLevelEncryptionProfileConfig:
  expected: HTTP 404, $.ErrorResponse.Error.Message = 'Id format is invalid'
  actual:   HTTP 404, $.ErrorResponse.Error.Message = 'The specified field level encryption profile does not exist.'
  divergence_key: service=cloudfront|GetFieldLevelEncryptionProfileConfig|status=404->404|keys=$.ErrorResponse.Error.Message

Cause 3 — fixture cloudfront-get-public-key-wrong-resource-type-id-rejected, step task#3, GetPublicKey (cluster also affects cloudfront-public-key-and-key-group-lifecycle#task#10):
  expected: HTTP 404, $.ErrorResponse.Error.Message = 'Id format is invalid'
  actual:   HTTP 404, $.ErrorResponse.Error.Message = 'The specified CloudFront public key does not exist.'
  divergence_key: service=cloudfront|GetPublicKey|status=404->404|keys=$.ErrorResponse.Error.Message

Model evidence (services/service-com-amazonaws-cloudfront-cloudfront2020-05-31/smithy-model/com-amazonaws-cloudfront-cloudfront2020-05-31.json):
  GetDistributionConfig — GET /2020-05-31/distribution/{Id}/config, 200; Id @required @httpLabel; errors AccessDenied(403), NoSuchDistribution(404).
  GetFieldLevelEncryptionProfileConfig — GET /2020-05-31/field-level-encryption-profile/{Id}/config, 200; Id @required @httpLabel; errors AccessDenied(403), NoSuchFieldLevelEncryptionProfile(404).
  GetPublicKey — GET /2020-05-31/public-key/{Id}, 200; Id @required @httpLabel; errors AccessDenied(403), NoSuchPublicKey(404).

Fixed vs variable message text: 'Id format is invalid' is fixed text in full for all three operations — no id, name, value or limit from the request or from emulator state is interpolated into it, so no recorded value is part of the requirement. The emulator's three 'does not exist' sentences are likewise fixed text, and are the correct wording only for a well-formed id that names nothing.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay trace set simparity-wire-aws-v1 (replay_wire_corpus).
2. Cause 1: replay fixture cloudfront-cfode-distribution-lookup-identifier-mistakes and observe step task#4 — GET /2020-05-31/distribution/{Id}/config with an Id that is not a well-formed CloudFront distribution id. Expect 404 with Message 'Id format is invalid'; the emulator answers 404 with 'The specified distribution does not exist.' Step task#5 of the same fixture reproduces it.
3. Cause 2: replay fixture cloudfront-field-level-encryption-profile-config-get-and-not-found and observe step task#4 — GET /2020-05-31/field-level-encryption-profile/{Id}/config with a malformed profile id. Expect 404 with 'Id format is invalid'; the emulator answers 404 with 'The specified field level encryption profile does not exist.'
4. Cause 3: replay fixture cloudfront-get-public-key-wrong-resource-type-id-rejected and observe step task#3 — GET /2020-05-31/public-key/{Id} with an id belonging to another CloudFront resource type. Expect 404 with 'Id format is invalid'; the emulator answers 404 with 'The specified CloudFront public key does not exist.' Fixture cloudfront-public-key-and-key-group-lifecycle#task#10 reproduces it.
5. In each case compare only $.ErrorResponse.Error.Message: the status (404) and error Code already match, so the fix must change the message emitted on the malformed-id branch without changing the well-formed-but-absent branch.
