Issue-Source: trace-sweep-divergence
Issue-Trace: bedrock-automated-reasoning-policy-lifecycle
Issue-Step: teardown#9
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: bedrock-automated-reasoning-policy-lifecycle bedrock-guardrail-tag-quota-boundary-and-retag
Issue-Shape-Id: com.amazonaws.bedrock#AmazonBedrockControlPlaneService
Issue-Key: bedrock|DeleteAutomatedReasoningPolicy|4xx->2xx|status
Issue-Key: bedrock|TagResource|4xx->4xx|$.message
Issue-Key: bedrock|TagResource|4xx->4xx|$.resourceName
Issue-Commit: 67143cfe72c1eaf0048b7d6ffe4fd5ee2a6ed259

# bedrock resource-refusal paths diverge: DeleteAutomatedReasoningPolicy accepts a delete AWS refuses 400, and the TagResource 50-tag TooManyTagsException is re-worded (2 causes)

> _Note: this issue was identified against the code base at commit `67143cfe72c1eaf0048b7d6ffe4fd5ee2a6ed259`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteAutomatedReasoningPolicy

## Description
Two net-new root causes in the bedrock control plane's resource-management refusal paths: a mutation the service refuses is accepted as a success, and a modelled quota error is served with emulator-authored wording and an extra member. Both were found by replaying the wire corpus simparity-wire-aws-v1; neither is owned by an open issue's replay key. There are 2 causes to close.

=== Cause 1 of 2: DeleteAutomatedReasoningPolicy answers 202 success where AWS refuses the delete with a 400 client error ===

AWS: Smithy (services/service-com-amazonaws-bedrock-amazonbedrockcontrolplaneservice/smithy-model/com-amazonaws-bedrock-amazonbedrockcontrolplaneservice.json) models DeleteAutomatedReasoningPolicy as DELETE /automated-reasoning-policies/{policyArn}, success code 202 with an empty output shape, input policyArn (@httpLabel, @required, @length 1..2048, @pattern ^arn:aws(-[^:]+)?:bedrock:[a-z0-9-]{1,20}:[0-9]{12}:automated-reasoning-policy/[a-z0-9]{12}(:([1-9][0-9]{0,11}))?$) plus force (@httpQuery force, @default false), and @idempotent. Its client-error set includes three 400-class errors — ConflictException (400, "Error occurred because of a conflict while performing an operation"), ResourceInUseException (400, documented as "Thrown when attempting to delete or modify a resource that is currently being used by other resources or operations. For example, trying to delete an Automated Reasoning policy that is referenced by an active guardrail") and ValidationException (400) — alongside ResourceNotFoundException (404), AccessDeniedException (403), ThrottlingException (429) and InternalServerException (500). For this recorded step AWS answered HTTP 400: the delete of this policy was REFUSED, not accepted. The fixture asserts the status only and no body member, so beyond "a 400-class modelled client error" the specific error code and message here are recorded behavior only and are not asserted by the replay. Hypothesis: the policy still carried the version that the same fixture created earlier (task#6 CreateAutomatedReasoningPolicyVersion, POST /automated-reasoning-policies/{policyArn}/versions), and AWS refuses deleting a policy that still has dependents/versions unless the caller opts out with force=true — which is exactly what the modelled `force` query member (@default false) is for. The requirement filed here is the one the recording states: this delete is refused with HTTP 400, not accepted.

Emulator: the same DELETE is ACCEPTED — HTTP 202 with the empty success body. Replay detail for the cluster: "status: expected 400, got 202". The handler therefore answers the modelled 202 unconditionally: it neither gates on the target policy's state (existing versions / in-use references) nor honours the force=false default by refusing, so no 400-class refusal is reachable from this path at all. Note that in this same fixture the emulator had already failed task#6 (CreateAutomatedReasoningPolicyVersion, 400 instead of 200 — owned by 20261108T070006Z-bedrock-automated-reasoning-policy-definitionhash-is-derived-differently.md), so its policy had no version; the divergence to fix is still that the delete path performs no state gating and cannot produce the refusal, and it must be re-replayed once the version-creation path works.

Why: a client that uses the refusal as its guard loses it entirely. Teardown and reconciliation code that deletes a policy only after clearing its versions, or that retries with force=true when the unforced delete is rejected, never sees the rejection against the emulator, so it believes a protected policy was removed; a policy still referenced by an active guardrail reads as gone while the reference survives, and the 400 branch (ConflictException / ResourceInUseException / ValidationException) of every such caller is dead code that is never exercised.

Fix: DeleteAutomatedReasoningPolicy's handler must resolve policyArn against Automated Reasoning policy state and evaluate the delete's preconditions BEFORE mutating — refusing with the modelled 400 client error when the policy cannot be deleted as asked (dependent versions / in-use references and force not set) and only answering 202 when the delete is actually performed. This belongs in that operation's handler and the policy state layer it reads, beside the read path that already resolves the ARN correctly.

Related but NOT covering this: open issue 20261115T054942Z-bedrock-deleteautomatedreasoningpolicy-accepts-a-deleted-policy-arn-with.md covers the behavioral check for a SECOND delete of an already-deleted ARN (AWS: 404 ResourceNotFoundException "Policy was not found"); it is a behavioral-corpus issue, it carries no wire replay key, and its expectation is a 404 on a dead ARN — not this step's 400 refusal of a live policy. 20261108T070006Z-bedrock-automated-reasoning-policy-definitionhash-is-derived-differently.md concerns definitionHash derivation.

Traces: bedrock-automated-reasoning-policy-lifecycle#teardown#9 (expected 400, got 202).

=== Cause 2 of 2: TagResource's 50-tag TooManyTagsException carries emulator wording and serializes resourceName AWS leaves out ===

AWS: Smithy models TagResource as POST /tagResource, success 200 with an empty output, input resourceARN (@required, @length 20..1011) and tags (@required, TagList @length 0..200), with TooManyTagsException (@httpError 400) in its error set. TooManyTagsException has members message (NonBlankString) and resourceName (TaggableResourcesArn, documented "The name of the resource with too many tags"), and the shape itself documents the rule: "The request contains more tags than can be associated with a resource (50 tags per resource). The maximum number of tags includes both existing tags and those included in your current request." For a tag request whose resulting set would exceed the 50-user-tag limit, the recorded response is HTTP 400 TooManyTagsException with the message "The resultant tag set must not have more than 50 user tags." — wholly FIXED text, with no variable part: no ARN, resource name, tag key, count or limit value is interpolated into it. The recording also leaves resourceName UNSET on the wire (the member is optional and absent).

Emulator: the status and error class are right, but the message is re-worded to "You can assign a maximum of 50 tags to a resource" and resourceName is serialized with the tagged resource's ARN. Replay detail: "$.message: value_mismatch (expected 'The resultant tag set must not have more than 50 user tags.', got 'You can assign a maximum of 50 tags to a resource'); $.resourceName: type_mismatch (expected None, got <the guardrail ARN under test>)". The ARN value itself is per-environment and is not the requirement — the requirement is that the member is absent.

Why: the message is part of the error contract. A client that matches, logs or surfaces AWS's sentence (the fixed text above, which it can match literally precisely because nothing is interpolated) does not recognise the emulator's, so tag-quota handling, user-facing messaging and assertions on the refusal all diverge; and a client that reads TooManyTagsException.resourceName sees a populated member against the emulator where AWS gives it nothing, so code that branches on its presence takes the wrong path.

Fix: in the TagResource handler's validation / error construction for the per-resource tag quota, emit TooManyTagsException with AWS's fixed message text "The resultant tag set must not have more than 50 user tags." and do not populate resourceName. The quota itself must stay evaluated against the RESULTANT set (existing tags plus the request's), as the shape documents.

Traces: bedrock-guardrail-tag-quota-boundary-and-retag#task#4 (status 400 on both sides; message and resourceName diverge).

## Evidence
Wire corpus simparity-wire-aws-v1 replayed against the emulator built from this checkout: emulator_up=true, bootstrap_signal=false, 33 fixtures replayed, 189 steps (156 passed, 33 mismatched, 0 errored, 0 blocked), score 82.5%, 18 divergence clusters. 16 of the 18 are already owned by an open issue's replay key or filed as unimplemented-scaffold stubs; the two below are the only unowned ones, and both are service=bedrock.

[mismatch] bedrock / DeleteAutomatedReasoningPolicy — fixture bedrock-automated-reasoning-policy-lifecycle, step teardown#9
  divergence_key: service=bedrock|DeleteAutomatedReasoningPolicy|status=400->202|keys=status
  expected: HTTP 400 (modelled 400-class client error: ConflictException / ResourceInUseException / ValidationException) — the delete is refused
  actual:   HTTP 202 with the modelled empty success body
  detail:   "status: expected 400, got 202"
  affected: bedrock-automated-reasoning-policy-lifecycle#teardown#9 (1 step)
  context in the same fixture: task#6 CreateAutomatedReasoningPolicyVersion also diverges (200 -> 400) and is owned by 20261108T070006Z-bedrock-automated-reasoning-policy-definitionhash-is-derived-differently.md, so the policy the teardown deletes had no version in the emulator.

[mismatch] bedrock / TagResource — fixture bedrock-guardrail-tag-quota-boundary-and-retag, step task#4
  divergence_key: service=bedrock|TagResource|status=400->400|keys=$.message,$.resourceName
  expected: HTTP 400 TooManyTagsException, message == "The resultant tag set must not have more than 50 user tags." (fixed text, nothing interpolated), resourceName ABSENT
  actual:   HTTP 400, message == "You can assign a maximum of 50 tags to a resource", resourceName == the guardrail ARN under test (a per-environment value; its presence is the defect, not its value)
  detail:   "$.message: value_mismatch (...); $.resourceName: type_mismatch (expected None, got 'arn:aws:bedrock:<region>:<account>:guardrail/<id>')"
  affected: bedrock-guardrail-tag-quota-boundary-and-retag#task#4 (1 step)

Model facts used (services/service-com-amazonaws-bedrock-amazonbedrockcontrolplaneservice/smithy-model/com-amazonaws-bedrock-amazonbedrockcontrolplaneservice.json): DeleteAutomatedReasoningPolicy DELETE /automated-reasoning-policies/{policyArn}, http code 202, @idempotent, input policyArn @required @httpLabel + force @httpQuery @default false, empty output, errors ConflictException 400 / ResourceInUseException 400 / ValidationException 400 / ResourceNotFoundException 404 / AccessDeniedException 403 / ThrottlingException 429 / InternalServerException 500; ResourceInUseException doc "Thrown when attempting to delete or modify a resource that is currently being used by other resources or operations...". TagResource POST /tagResource, http code 200, input resourceARN @required + tags @required (TagList @length 0..200), errors TooManyTagsException 400 / ValidationException 400 / ResourceNotFoundException 404 / AccessDeniedException 403 / ThrottlingException 429 / InternalServerException 500; TooManyTagsException members {message, resourceName(optional)}, shape doc "The request contains more tags than can be associated with a resource (50 tags per resource). The maximum number of tags includes both existing tags and those included in your current request."

## Steps to Reproduce
Boot the emulator from this checkout and replay the wire corpus simparity-wire-aws-v1; the two clusters are fixtures bedrock-automated-reasoning-policy-lifecycle (step teardown#9) and bedrock-guardrail-tag-quota-boundary-and-retag (step task#4). By hand:

Cause 1 (DeleteAutomatedReasoningPolicy, fixture bedrock-automated-reasoning-policy-lifecycle#teardown#9):
1. CreateAutomatedReasoningPolicy(name=<unique>, policyDefinition=<definition>) -> note policyArn and definitionHash.
2. UpdateAutomatedReasoningPolicy on that policyArn, then CreateAutomatedReasoningPolicyVersion(policyArn, lastUpdatedDefinitionHash=<the hash returned by the update>) -> AWS returns 200 with version "1" (against the emulator this step currently 400s; that part is owned by the definitionHash issue).
3. DeleteAutomatedReasoningPolicy(policyArn=<the base policy ARN>) with force unset (default false) -> expect HTTP 400, a modelled client error (ConflictException / ResourceInUseException / ValidationException) refusing the delete; observe HTTP 202 and the empty success body.

Cause 2 (TagResource, fixture bedrock-guardrail-tag-quota-boundary-and-retag#task#4):
1. CreateGuardrail(...) -> note the guardrail ARN.
2. TagResource(resourceARN=<guardrail ARN>, tags=[...]) so that the RESULTANT tag set (tags already on the resource plus those in the request) exceeds 50 user tags.
3. Expect HTTP 400 TooManyTagsException with message exactly "The resultant tag set must not have more than 50 user tags." and NO resourceName member in the body; observe message "You can assign a maximum of 50 tags to a resource" and a resourceName member carrying the guardrail ARN.
