Issue-Source: trace-sweep-divergence
Issue-Trace: cloudfront-field-level-encryption-errors
Issue-Step: task#6
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: cloudfront-field-level-encryption-errors cloudfront-fle-config-not-found-and-invalid-ifmatch-errors
Issue-Shape-Id: com.amazonaws.cloudfront#Cloudfront2020_05_31
Issue-Key: cloudfront|DeleteFieldLevelEncryptionProfile|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Key: cloudfront|DeleteFieldLevelEncryptionProfile|2xx->4xx|status
Issue-Commit: ed1dc90158d37b546381596a943eb7e01ce78b3f

# CloudFront DeleteFieldLevelEncryptionProfile diverges on malformed-id refusal and If-Match precondition

> _Note: this issue was identified against the code base at commit `ed1dc90158d37b546381596a943eb7e01ce78b3f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteFieldLevelEncryptionProfile

## Description
TWO (2) distinct net-new causes, both on CloudFront's DELETE /2020-05-31/field-level-encryption-profile/{Id} route. They share one fault: the emulator's DeleteFieldLevelEncryptionProfile handler reaches the wrong refusal branch - it answers the state-lookup not-found message where AWS refuses the identifier's FORM, and it answers PreconditionFailed where AWS accepts the If-Match version it itself published. Both causes must be closed before this issue is submitted.

Replay reports both clusters unowned (owned_by empty, stub_path empty). The nearest open issues do NOT cover them: 20261114T142904Z-cloudfront-reopens-field-level-encryption-lookups-report-the.md carries the SAME malformed-id fault but only for GetFieldLevelEncryptionConfig, GetFieldLevelEncryptionProfile, DeleteFieldLevelEncryptionConfig and GetFieldLevelEncryptionProfileConfig - DeleteFieldLevelEncryptionProfile is not among its Issue-Key lines or its traces, so cause 1 is filed here; 20261118T213617Z-cloudfront-public-key-lifecycle-refuses-creates-and-deletes.md describes the analogous 204->412 delete refusal for DeletePublicKey (teardown#8 of the very same fixture) and owns only that public-key cluster, not the field-level-encryption-profile delete in cause 2.

=== Cause 1 of 2: DeleteFieldLevelEncryptionProfile reports an absent profile instead of a malformed id ===
AWS: 404 whose ErrorResponse.Error.Message is the fixed text 'Id format is invalid' - recorded behavior only. The Smithy model supplies the route and the error shape but no message text: DeleteFieldLevelEncryptionProfile, http {method DELETE, uri /2020-05-31/field-level-encryption-profile/{Id}, code 204}; Id is @required @httpLabel; errors AccessDenied (403), FieldLevelEncryptionProfileInUse (409), InvalidIfMatchVersion (400), NoSuchFieldLevelEncryptionProfile (404, doc 'The specified profile for field-level encryption doesn't exist.'), PreconditionFailed (412). The recording shows AWS judging the path identifier's FORM before any state lookup and before the If-Match test, and the message for that error class is ENTIRELY FIXED text: 'Id format is invalid' has no variable part - nothing in it comes from the request or from state - so reproducing it exactly is the whole requirement. The status (404) and the error code already match.
Emulator: 404 with Error.Message 'The specified field level encryption profile does not exist.' - the not-found message that belongs to a well-formed id naming an absent resource.
Why: these are two different client contracts. A caller that distinguishes 'this id cannot exist' (a bug in its own id handling, not retryable) from 'this resource is gone' (treat the delete as already done / recreate) takes the wrong branch, and any test asserting the invalid-id-format wording passes against AWS and fails against the emulator.
Fix: DeleteFieldLevelEncryptionProfile's request validation in the cloudfront field-level-encryption handler - validate the path id's format and refuse with the invalid-id-format message before the state lookup, exactly as the sibling FLE routes must.
Traces: cloudfront-field-level-encryption-errors#task#6 (representative).

=== Cause 2 of 2: DeleteFieldLevelEncryptionProfile answers 412 PreconditionFailed to a delete carrying the ETag the emulator published ===
AWS: 204 with an empty body. Smithy: DeleteFieldLevelEncryptionProfile http code 204, output Unit; IfMatch is bound to the If-Match request header; PreconditionFailed (client, httpError 412, doc 'The precondition in one or more of the request fields evaluated to false.') and InvalidIfMatchVersion (client, 400, doc 'The If-Match version is missing or not valid.'). The rule is that PreconditionFailed is raised ONLY when the presented If-Match value does not equal the resource's CURRENT ETag; when the caller presents the version most recently published for that profile, the delete succeeds with 204 and no body. The recorded teardown deletes the profile with the ETag handed back by the preceding create/read and AWS answers 204.
Emulator: 412 PreconditionFailed - the delete is refused even though the If-Match value is the one the emulator itself published for that profile. Hypothesis: the emulator's field-level-encryption-profile ETag/version is advanced by requests that did not change the resource (this fixture's earlier steps are deliberately-failing not-found and invalid-If-Match probes), so the version the caller holds no longer equals the stored one. The same symptom appears on DeletePublicKey in this fixture's teardown#8, which is filed separately, so a shared version-bookkeeping defect is likely - but this route must be verified on its own.
Why: the profile becomes undeletable. A caller holding the current ETag is told its precondition failed and has no newer version to retry with, so field-level-encryption profiles leak, the public keys and FLE configs that reference them cannot be released either, and the modeled success path of the operation is unreachable. Fixture teardown cannot complete, which corrupts state for anything replayed after it.
Fix: the field-level-encryption-profile state's ETag/version bookkeeping plus DeleteFieldLevelEncryptionProfile's If-Match comparison - an unchanged resource must keep the ETag it last published, and a failed or read-only request must not advance it.
Traces: cloudfront-fle-config-not-found-and-invalid-ifmatch-errors#teardown#7.

The malformed ids, generated profile ids and ETag values in the fixtures are per-run values and are not themselves requirements; the requirements are the branch order (id-format check before lookup), the exact fixed message text of cause 1, and the 204 success status of cause 2.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout (emulator_up=true, bootstrap_signal=false; 1411 steps, 1316 passed, 86 mismatched, score 93.3%). Two unowned mismatch clusters, both DeleteFieldLevelEncryptionProfile, both with stub_path empty and owned_by empty:

Cause 1 - divergence_key service=cloudfront|DeleteFieldLevelEncryptionProfile|status=404->404|keys=$.ErrorResponse.Error.Message; fixture cloudfront-field-level-encryption-errors, step task#6; expected_status 404, actual_status 404; detail: $.ErrorResponse.Error.Message value_mismatch (expected 'The specified field level encryption profile does not exist.', got 'Id format is invalid').
NOTE on direction: the corpus records AWS answering 'Id format is invalid' for this malformed-id request; the replay's expected/actual labels for this cluster read the other way round, and the sibling clusters in the same fixture (GetFieldLevelEncryptionProfile#task#4, GetFieldLevelEncryptionConfig#task#2, DeleteFieldLevelEncryptionConfig#task#5) all show expected 'The specified ... does not exist.' vs got 'Id format is invalid' for the same probe. Either way the two messages are not interchangeable: the emulator and the recording disagree on which branch this route takes, the status (404) and error code agree, and only Error.Message diverges. Compare the recorded response for this step to settle which of the two fixed texts AWS emits on this route and make the emulator emit exactly that one, consistently with the other FLE routes.

Cause 2 - divergence_key service=cloudfront|DeleteFieldLevelEncryptionProfile|status=204->412|keys=status; fixture cloudfront-fle-config-not-found-and-invalid-ifmatch-errors, step teardown#7; expected_status 204, actual_status 412; detail: 'status: expected 204, got 412'. 204 is the success code bound in the Smithy model for this operation; 412 PreconditionFailed is a modeled error whose precondition (If-Match != current ETag) does not hold for this request. The immediately following step of the same fixture, teardown#8 (DeletePublicKey), diverges identically 204->412 and is already owned by 20261118T213617Z-cloudfront-public-key-lifecycle-refuses-creates-and-deletes.md.

## Steps to Reproduce
1. git clone the repo, boot the emulator built from that checkout, and replay the wire corpus (trace set simparity-wire-aws-v1).
2. Cause 1: replay fixture cloudfront-field-level-encryption-errors to step task#6 - DELETE /2020-05-31/field-level-encryption-profile/<id that is not in CloudFront's id form>. Status 404 and the error code match; compare only $.ErrorResponse.Error.Message, and compare it against the recorded response and against the sibling routes exercised by task#2 (GetFieldLevelEncryptionConfig), task#4 (GetFieldLevelEncryptionProfile) and task#5 (DeleteFieldLevelEncryptionConfig) of the same fixture.
3. Cause 2: replay fixture cloudfront-fle-config-not-found-and-invalid-ifmatch-errors IN FULL, including step teardown#7 - DeleteFieldLevelEncryptionProfile for the profile the fixture created, sending If-Match set to the ETag the emulator last published for it. Expect 204 with an empty body; observe 412 PreconditionFailed. The fixture's earlier not-found and invalid-If-Match probe steps must be replayed first, because they are what appears to advance the stored version.
4. Confirm the contract with smithy_query(query="operation", operation="DeleteFieldLevelEncryptionProfile", model_path="services/service-com-amazonaws-cloudfront-cloudfront2020-05-31/smithy-model/com-amazonaws-cloudfront-cloudfront2020-05-31.json"): http code 204, Id @required @httpLabel, IfMatch bound to the If-Match header, and NoSuchFieldLevelEncryptionProfile (404) / InvalidIfMatchVersion (400) / PreconditionFailed (412) as conditional errors rather than default answers.
5. Re-replay both fixtures after the fix; both clusters must pass.
