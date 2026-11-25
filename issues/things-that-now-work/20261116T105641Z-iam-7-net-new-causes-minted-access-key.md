Issue-Source: trace-sweep-divergence
Issue-Trace: iam-access-key-error-paths
Issue-Step: task#2
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: iam-access-key-error-paths iam-list-access-keys-lifecycle iam-read-only-inspection-of-operator-identity iam-self-user-access-key-not-found iam-bap-simulate-with-bucket-policy iam-ebecs-events-role-run-task-pass-role-simulation iam-err-delete-user-blocked-by-policies iam-role-lifecycle-inline-managed-policy-and-boundary
Issue-Shape-Id: com.amazonaws.iam#AWSIdentityManagementV20100508
Issue-Key: iam|ListAccessKeys|2xx->2xx|$.ListAccessKeysResponse.ListAccessKeysResult.AccessKeyMetadata.member.AccessKeyId
Issue-Key: iam|CreateAccessKey|2xx->2xx|$.CreateAccessKeyResponse.CreateAccessKeyResult.AccessKey.AccessKeyId
Issue-Key: iam|SimulatePrincipalPolicy|2xx->2xx|$.SimulatePrincipalPolicyResponse.SimulatePrincipalPolicyResult.EvaluationResults.member.MissingContextValues
Issue-Key: iam|DeleteUser|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Key: iam|DeleteRole|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Related: 20261103T132715Z-iam-simulateprincipalpolicy-evaluation-results-omit-modeled-detail-members.md
Issue-Related: 20261115T063919Z-iam-simulateprincipalpolicy-evaluation-results-diverge-organizations-and-cross.md
Issue-Commit: cf9fc6ab8267f9d5f0acca19e6a4a41633dc20ed

# iam: 7 net-new causes — minted access-key ids are EMULATOR-prefixed, DeleteConflict refusals pick the wrong sentence, and SimulatePrincipalPolicy context/position details diverge

> _Note: this issue was identified against the code base at commit `cf9fc6ab8267f9d5f0acca19e6a4a41633dc20ed`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListAccessKeys

## Description
Seven net-new root causes in the iam query API, each grounded in its own replayed divergence cluster from simparity-wire-aws-v1. All seven must be closed before this issue is submitted. They group into three faults: the access-key-id minting site (causes 1-2), the SimulatePrincipalPolicy evaluation-result projection (causes 3-5), and the DeleteConflict message-selection site (causes 6-7).

Cause 1 of 7: ListAccessKeys serves an AccessKeyId that is not an AWS IAM access key id.
AWS: Smithy iam#ListAccessKeys -> output AccessKeyMetadata (required, accessKeyMetadataListType); AccessKeyMetadata.AccessKeyId -> iam#accessKeyIdType, @length 16-128, @pattern ^[\w]+$. AWS's IAM unique-identifier prefixes are documented and load-bearing: a long-term IAM user access key id is 20 characters and begins with the literal prefix `AKIA` (a temporary STS credential begins `ASIA`). The recording's value is redacted as an aws-access-key, i.e. the recorded id matched AWS's access-key-id class; the requirement is the id FORM (AKIA + 16 uppercase base32 characters), never one run's literal id, which is per-run state.
Emulator: the listed id is `EMULATOR`-prefixed (one run: 'EMULATORA339E47315BC'); it satisfies the Smithy pattern but is not in AWS's documented access-key class, so it is not recognised as an access key id at all.
Why: credential-classifying tooling (long-term vs temporary, IAM user vs role session, credential scanners, policy linters, SDK presign helpers that branch on the prefix) misclassifies every key the emulator lists, and a client that validates an id against AWS's documented prefix rejects the whole listing.
Fix: the access-key-id allocator behind the IAM user access-key resource, reached by ListAccessKeys' projection of stored key metadata (iam service crate, access-key state + its id minting), not the ListAccessKeys serializer.
Note: the open behavioral issue 20261111T103332Z-iam-7-net-new-causes-deleteaccountpasswordpolicy-is-unrouted.md describes the same EMULATOR prefix on CreateAccessKey as its cause 6, but it carries no wire replay key, so these two clusters are not covered by it.
Traces: iam-access-key-error-paths#task#2 (cluster of 4: also iam-list-access-keys-lifecycle#task#4, iam-read-only-inspection-of-operator-identity#task#4, iam-self-user-access-key-not-found#task#3).

Cause 2 of 7: CreateAccessKey mints the same non-AWS access key id.
AWS: Smithy iam#CreateAccessKey -> output AccessKey (required); AccessKey.AccessKeyId -> accessKeyIdType, @length 16-128, @pattern ^[\w]+$; AWS mints a 20-character id beginning `AKIA` for an IAM user's long-term key. Recorded response: HTTP 200 with the id redacted as an aws-access-key.
Emulator: answers 200 with an `EMULATOR`-prefixed id (one run: 'EMULATORAKWTKFPNA6Z6'; the suffix is per-run, the prefix is the defect).
Why: the id the client stores and later signs with is unclassifiable, so every downstream consumer that branches on the prefix is wrong from the moment the key is created, and the id the emulator later lists (cause 1) inherits the same defect.
Fix: the CreateAccessKey handler's id minting / the shared access-key id allocator in the iam service crate.
Traces: iam-list-access-keys-lifecycle#task#3.

Cause 3 of 7: SimulatePrincipalPolicy reports a top-level MissingContextValues for a simulation that supplied ResourceArns, mis-computes MatchedStatements StartPosition.Column, and omits EvalDecisionDetails.
AWS: Smithy iam#EvaluationResult.MissingContextValues -> ContextKeyNamesResultListType, documented: the top-level list is used when the simulated resource is "*" (explicitly, or because ResourceArns was blank) and "if you include a list of resources, then any missing context values are instead included under the ResourceSpecificResults section". iam#Statement.StartPosition -> iam#Position{Line, Column}, "the row and column of the beginning of the Statement in an IAM policy" — a position in the policy document as stored, so the same document always yields the same column. iam#EvaluationResult.EvalDecisionDetails -> EvalDecisionDetailsType (map), documented as "present but the response is empty" for a same-account simulation that includes a resource ARN. Recorded response: HTTP 200, no top-level missing-context entry, an EvalDecisionDetails element present-but-empty, and the matched statement's column as stored in the document.
Emulator: HTTP 200, but the top-level MissingContextValues carries aws:SecureTransport (expected '', got {'member': 'aws:SecureTransport'}) — a condition key referenced by the resource policy under test; StartPosition.Column is off by a small, non-constant amount (expected '287', got '288'), repeated identically inside ResourceSpecificResults.member.MatchedStatements.member.StartPosition.Column; EvalDecisionDetails is absent entirely (expected '', got None).
Why: a caller that treats a non-empty top-level MissingContextValues as "inconclusive, supply more context" retries a simulation AWS already answered conclusively, and never finds the key in the per-resource place the SDK documents; tooling that highlights the deciding statement from StartPosition/EndPosition points at the wrong character; a client that distinguishes same-account from cross-account simulations by the presence of EvalDecisionDetails can never see the same-account case.
Fix: the SimulatePrincipalPolicy result builder in the iam service crate — missing context keys must be routed to ResourceSpecificResults whenever the request carried ResourceArns, the statement position must index the stored policy document text (and be reused for the per-resource copy), and EvalDecisionDetails must be emitted for every EvaluationResult.
Note: the open issue 20261115T063919Z-iam-simulateprincipalpolicy-evaluation-results-diverge-organizations-and-cross.md describes the column and top-level-MissingContextValues symptoms, and 20261103T132715Z-iam-simulateprincipalpolicy-evaluation-results-omit-modeled-detail-members.md the EvalDecisionDetails omission, but neither carries this cluster's replay key — the replay reports it owned by no open issue, so it is filed here to bind the key.
Traces: iam-bap-simulate-with-bucket-policy#task#3.

Cause 4 of 7: the same spurious top-level MissingContextValues plus absent EvalDecisionDetails on the resource-policy-bearing steps of the same trajectory.
AWS: as cause 3 — EvaluationResult.MissingContextValues must be empty at the top level when ResourceArns was supplied, and EvalDecisionDetails must be present (empty) for a same-account simulation naming a resource ARN. Recorded response: HTTP 200 with an empty top-level MissingContextValues and a present EvalDecisionDetails element.
Emulator: HTTP 200 with MissingContextValues = {'member': 'aws:SecureTransport'} and EvalDecisionDetails missing.
Why: same client consequence as cause 3 — these steps are the simulate-after-fix legs, so a caller driving a remediation loop off MissingContextValues never observes it clear.
Fix: same site as cause 3 (the context-key routing and the EvalDecisionDetails emission in the SimulatePrincipalPolicy result builder); listed separately because it is a distinct replayed cluster that must be re-replayed to confirm the fix.
Traces: iam-bap-simulate-with-bucket-policy#task#4 (cluster of 2: also iam-bap-simulate-with-bucket-policy#task#6).

Cause 5 of 7: SimulatePrincipalPolicy reports NO missing context key where AWS reports one — the opposite direction.
AWS: Smithy iam#EvaluationResult.MissingContextValues documents "a list of context keys that are required by the included input policies but that were not provided by one of the input parameters", reported at the top level and, for a request naming resources, in ResourceSpecificResults. Recorded response: HTTP 200 with MissingContextValues = ecs:cluster both at the top level and in the matching ResourceSpecificResults entry, plus a present EvalDecisionDetails element.
Emulator: HTTP 200 with an empty top-level MissingContextValues (expected {'member': 'ecs:cluster'}, got ''), no ResourceSpecificResults.MissingContextValues member at all (expected {'member': 'ecs:cluster'}, got None), and EvalDecisionDetails absent.
Why: the condition key a policy requires but the simulation did not supply is how a caller learns its answer is conditional. Silently dropping it turns a "you must supply ecs:cluster to know" answer into an unconditional decision, so a pre-flight permission check reports a definite verdict AWS never gave.
Fix: the policy-simulation engine's condition-key evaluation must collect keys referenced by the identity-based policy's Condition blocks that the request's ContextEntries did not supply, and emit them both in the aggregate list and in each ResourceSpecificResults entry.
Traces: iam-ebecs-events-role-run-task-pass-role-simulation#task#11 (cluster of 2: also iam-ebecs-events-role-run-task-pass-role-simulation#task#13).

Cause 6 of 7: DeleteUser's DeleteConflict refusal emits the INLINE-policy sentence for a conflict caused by ATTACHED MANAGED policies.
AWS: Smithy iam#DeleteUser models DeleteConflictException with awsQueryError code 'DeleteConflict', httpResponseCode 409. The model does not state the message text, so this is recorded behavior only: AWS words the refusal per blocking dependency, and for a user that still has MANAGED policies attached the message is exactly 'Cannot delete entity, must detach all policies first.' The other sentence, 'Cannot delete entity, must delete policies first.', is AWS's wording for the different case where an INLINE (embedded) policy still exists. Both sentences are 100% fixed text — no part is substituted from the request or from state, so the whole sentence including the trailing period is the requirement; the defect is sentence SELECTION.
Emulator: HTTP 409 with Code 'DeleteConflict' (both correct) but Message 'Cannot delete entity, must delete policies first.' for the attached-managed-policy conflict.
Why: DeleteConflict is the one IAM refusal a client is expected to act on by remediating the named dependency. A cleanup loop told to "delete policies" deletes inline policies — of which there may be none — and never detaches the managed policy actually blocking the delete, so it never converges; runbooks and message-matching clients diverge even though status and code match.
Fix: the DeleteConflict message construction in iam's DeleteUser handler — select the sentence from the dependency actually detected (inline user policies -> 'Cannot delete entity, must delete policies first.'; attached managed policies -> 'Cannot delete entity, must detach all policies first.') instead of emitting one constant.
Note: the mirror-image defect on DeleteGroup is the resolved issue 20261103T183603Z-iam-deletegroup-deleteconflict-uses-the-managed-policy-sentence.md (it emitted the managed sentence for an inline conflict); a resolved issue covers nothing, and the user/role conflict paths now emit the opposite constant.
Traces: iam-err-delete-user-blocked-by-policies#task#5.

Cause 7 of 7: DeleteRole's DeleteConflict refusal has the same wrong sentence selection.
AWS: Smithy iam#DeleteRole models DeleteConflictException with awsQueryError code 'DeleteConflict', httpResponseCode 409. Recorded behavior only for the text: a role that still has managed policies attached is refused with the fixed sentence 'Cannot delete entity, must detach all policies first.'; the inline-policy conflict uses 'Cannot delete entity, must delete policies first.' Neither sentence has variable parts.
Emulator: HTTP 409, Code 'DeleteConflict' (correct), Message 'Cannot delete entity, must delete policies first.' for the attached-managed-policy conflict.
Whay a client gets wrong — Why: an operator or teardown routine that reads the message detaches nothing and retries forever on a role whose only blocker is an attached managed policy (and, with a permissions boundary in play, cannot tell which attachment class is at fault).
Fix: the same per-dependency message selection in iam's DeleteRole handler (and the shared DeleteConflict message helper the user/role/group paths use).
Traces: iam-role-lifecycle-inline-managed-policy-and-boundary#task#16.

## Evidence
Replay of simparity-wire-aws-v1 against the emulator built from this checkout: 228 fixtures replayed, 1695 steps, 1568 passed, 126 mismatched, 1 blocked, score 92.5%, emulator_up true, bootstrap_signal false. 58 divergence clusters, all service=iam; 51 are already owned by an open issue or blocked by a filed stub. The 7 clusters below are the ones the replay reports owned by nothing.

Cause 1 — iam-access-key-error-paths#task#2, ListAccessKeys, expected 200 / actual 200, verdict mismatch, affected_count 4:
  $.ListAccessKeysResponse.ListAccessKeysResult.AccessKeyMetadata.member.AccessKeyId: value_mismatch (expected '[REDACTED:aws-access-key]', got 'EMULATORA339E47315BC')
  divergence_key: service=iam|ListAccessKeys|status=200->200|keys=$.ListAccessKeysResponse.ListAccessKeysResult.AccessKeyMetadata.member.AccessKeyId
  The expected value is redacted precisely because the recorded id was recognised as an AWS access key id; the emulator's id was not. The AKIA form is the requirement, not either literal.

Cause 2 — iam-list-access-keys-lifecycle#task#3, CreateAccessKey, expected 200 / actual 200:
  $.CreateAccessKeyResponse.CreateAccessKeyResult.AccessKey.AccessKeyId: value_mismatch (expected '[REDACTED:aws-access-key]', got 'EMULATORAKWTKFPNA6Z6')

Cause 3 — iam-bap-simulate-with-bucket-policy#task#3, SimulatePrincipalPolicy, expected 200 / actual 200:
  $...EvaluationResults.member.EvalDecisionDetails: missing (expected '', got None)
  $...EvaluationResults.member.MatchedStatements.member.StartPosition.Column: value_mismatch (expected '287', got '288')
  $...EvaluationResults.member.MissingContextValues: type_mismatch (expected '', got {'member': 'aws:SecureTransport'})
  $...EvaluationResults.member.ResourceSpecificResults.member.MatchedStatements.member.StartPosition.Column: value_mismatch (expected '287', got '288')

Cause 4 — iam-bap-simulate-with-bucket-policy#task#4, SimulatePrincipalPolicy, expected 200 / actual 200, affected_count 2 (also #task#6):
  $...EvaluationResults.member.EvalDecisionDetails: missing (expected '', got None)
  $...EvaluationResults.member.MissingContextValues: type_mismatch (expected '', got {'member': 'aws:SecureTransport'})

Cause 5 — iam-ebecs-events-role-run-task-pass-role-simulation#task#11, SimulatePrincipalPolicy, expected 200 / actual 200, affected_count 2 (also #task#13):
  $...EvaluationResults.member.EvalDecisionDetails: missing (expected '', got None)
  $...EvaluationResults.member.MissingContextValues: type_mismatch (expected {'member': 'ecs:cluster'}, got '')
  $...EvaluationResults.member.ResourceSpecificResults.member.MissingContextValues: missing (expected {'member': 'ecs:cluster'}, got None)

Cause 6 — iam-err-delete-user-blocked-by-policies#task#5, DeleteUser, expected 409 / actual 409 (status and Error.Code 'DeleteConflict' both match):
  $.ErrorResponse.Error.Message: value_mismatch (expected 'Cannot delete entity, must detach all policies first.', got 'Cannot delete entity, must delete policies first.')

Cause 7 — iam-role-lifecycle-inline-managed-policy-and-boundary#task#16, DeleteRole, expected 409 / actual 409 (status and code match):
  $.ErrorResponse.Error.Message: value_mismatch (expected 'Cannot delete entity, must detach all policies first.', got 'Cannot delete entity, must delete policies first.')

Model grounding — smithy_query on services/service-com-amazonaws-iam-awsidentitymanagementv20100508/smithy-model/com-amazonaws-iam-awsidentitymanagementv20100508.json:
- CreateAccessKey: output AccessKey (required); errors LimitExceeded(409), NoSuchEntity(404), ServiceFailure(500).
- ListAccessKeys: output AccessKeyMetadata (required, list), IsTruncated, Marker; paginated Marker/MaxItems.
- accessKeyIdType: string, @length 16-128, @pattern ^[\w]+$.
- DeleteUser / DeleteRole: errors include DeleteConflictException, awsQueryError code 'DeleteConflict', httpResponseCode 409; DeleteConflictException carries only `message` (deleteConflictMessage) and is documented "attempted to delete a resource that has attached subordinate entities. The error message describes these entities."
- SimulatePrincipalPolicy: output SimulatePolicyResponse.EvaluationResults (EvaluationResultsListType).
- EvaluationResult members: MissingContextValues (ContextKeyNamesResultListType; top-level list used when the resource is "*", otherwise missing values go to ResourceSpecificResults), EvalDecisionDetails (map; "present but the response is empty" for a same-account simulation naming a resource ARN), MatchedStatements (StatementListType), ResourceSpecificResults.
- Statement.StartPosition -> Position{Line, Column}: "the row and column of the beginning of the Statement in an IAM policy".

## Steps to Reproduce
1. git_clone the repo, build and boot the emulator from the checkout, then replay the wire corpus simparity-wire-aws-v1.
2. Cause 1: replay fixture iam-access-key-error-paths, step task#2 — CreateUser + CreateAccessKey, then ListAccessKeys for that user and inspect AccessKeyMetadata.member.AccessKeyId: it is 'EMULATOR'-prefixed instead of a 20-character AKIA-prefixed id.
3. Cause 2: replay iam-list-access-keys-lifecycle step task#3 — call CreateAccessKey and inspect AccessKey.AccessKeyId's prefix.
4. Cause 3: replay iam-bap-simulate-with-bucket-policy step task#3 — a SimulatePrincipalPolicy over an s3 action with an explicit ResourceArns entry and a bucket ResourcePolicy carrying an aws:SecureTransport condition; check that the top-level MissingContextValues is empty (any missing key belongs under ResourceSpecificResults), that EvalDecisionDetails is present (empty), and that MatchedStatements.member.StartPosition.Column equals the recorded column (287) both at the top level and inside ResourceSpecificResults.
5. Cause 4: replay iam-bap-simulate-with-bucket-policy steps task#4 and task#6 — same checks on the top-level MissingContextValues and EvalDecisionDetails.
6. Cause 5: replay iam-ebecs-events-role-run-task-pass-role-simulation steps task#11 and task#13 — a SimulatePrincipalPolicy whose identity policy conditions on ecs:cluster with no matching ContextEntries; MissingContextValues must list ecs:cluster at the top level and in the matching ResourceSpecificResults entry.
7. Cause 6: replay iam-err-delete-user-blocked-by-policies step task#5 — CreateUser, AttachUserPolicy (managed attachment only, no inline policy), then DeleteUser; the 409 DeleteConflict Message must be 'Cannot delete entity, must detach all policies first.' For contrast, with only PutUserPolicy (inline) in place AWS answers 'Cannot delete entity, must delete policies first.'
8. Cause 7: replay iam-role-lifecycle-inline-managed-policy-and-boundary step task#16 — a role with an attached managed policy (and a permissions boundary) refused by DeleteRole; same sentence requirement as cause 6.
9. Re-replay the corpus and confirm all seven listed fixture steps pass.
