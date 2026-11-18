Issue-Source: trace-sweep-divergence
Issue-Trace: secretsmanager-cancel-rotate-and-version-stage-on-a-missing-secret
Issue-Step: task#4
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: secretsmanager-cancel-rotate-and-version-stage-on-a-missing-secret secretsmanager-resource-policy-operations-on-a-missing-secret
Issue-Shape-Id: com.amazonaws.secretsmanager#secretsmanager
Issue-Key: secretsmanager|UpdateSecretVersionStage|4xx->4xx|$.Message
Issue-Key: secretsmanager|UpdateSecretVersionStage|4xx->4xx|$.__type
Issue-Key: secretsmanager|ValidateResourcePolicy|2xx->4xx|$.PolicyValidationPassed
Issue-Key: secretsmanager|ValidateResourcePolicy|2xx->4xx|$.ValidationErrors
Issue-Key: secretsmanager|ValidateResourcePolicy|2xx->4xx|status
Issue-Commit: ec2ef26d41329427f76ddd397b9730fbe7244eae

# secretsmanager: missing-secret gating is wrong in two operations — UpdateSecretVersionStage and ValidateResourcePolicy (2 causes)

> _Note: this issue was identified against the code base at commit `ec2ef26d41329427f76ddd397b9730fbe7244eae`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateSecretVersionStage

## Description
Two net-new causes, both the emulator deciding a request against a NON-EXISTENT secret with the wrong gate: one operation refuses before it resolves the secret, the other resolves a secret it must not require. Cause count: 2.

=== Cause 1 of 2: UpdateSecretVersionStage applies the MoveToVersionId/RemoveFromVersionId rule before resolving SecretId ===
AWS: Smithy (com.amazonaws.secretsmanager#UpdateSecretVersionStage): input SecretId (SecretIdType, @length 1..2048, required) and VersionStage (SecretVersionStageType, @length 1..256, required); MoveToVersionId and RemoveFromVersionId (SecretVersionIdType, @length 32..64) are both OPTIONAL. Modeled errors: InternalServiceError (500), InvalidParameterException (400), InvalidRequestException (400), LimitExceededException (400), ResourceNotFoundException (400). Recorded behavior: a call whose SecretId names a secret that does not exist is refused HTTP 400 with __type "ResourceNotFoundException" and Message "Secrets Manager can't find the specified secret." even when the call supplies NEITHER version parameter — the secret is resolved first, so a missing secret outranks the either-or parameter rule. That message is entirely fixed text: it names neither the secret nor any request value, so there is nothing per-run in it.
Emulator: answers HTTP 400 __type "InvalidParameterException" with Message "You must specify either MoveToVersionId or RemoveFromVersionId." The presence check sits at services/service-com-amazonaws-secretsmanager-secretsmanager/src/ops/update_secret_version_stage.rs:204-208, ahead of the store lookup that would produce ResourceNotFoundException at :226-233, so the secret is never looked up at all.
Why: the error CLASS is what a client branches on. Cleanup and reconcile loops ("move AWSCURRENT back if the secret still exists, otherwise skip") catch ResourceNotFoundException; against the emulator they get InvalidParameterException and conclude their own request was malformed rather than that the secret is gone — in botocore these are two different exception classes, so the intended except branch never fires and the caller aborts.
Fix: the UpdateSecretVersionStage handler's ordering — resolve SecretId against the store (and refuse ResourceNotFoundException with the fixed not-found message) BEFORE the MoveToVersionId/RemoveFromVersionId presence check, leaving the parameter refusal for a secret that does exist.
Traces: secretsmanager-cancel-rotate-and-version-stage-on-a-missing-secret#task#4

=== Cause 2 of 2: ValidateResourcePolicy requires SecretId to resolve, so a policy cannot be validated for a secret that does not exist ===
AWS: Smithy (com.amazonaws.secretsmanager#ValidateResourcePolicy): SecretId is OPTIONAL (SecretIdType, @length 1..2048) while ResourcePolicy (NonEmptyResourcePolicyType, @length 1..20480) is required; output is PolicyValidationPassed (boolean, default false) and ValidationErrors. Recorded behavior: a request carrying a SecretId that names no secret and a well-formed, non-public policy document is answered HTTP 200 with PolicyValidationPassed=true and ValidationErrors=[] — the operation reports on the DOCUMENT, and SecretId is attribution only. ResourceNotFoundException "Secrets Manager can't find the specified secret." is reserved for the operations that must resolve the secret (GetResourcePolicy, PutResourcePolicy, DeleteResourcePolicy, CancelRotateSecret), all of which matched AWS in this same fixture.
Emulator: answers HTTP 400 and no body members at all (replay: status expected 200, got 400; $.PolicyValidationPassed missing, $.ValidationErrors missing). The only refusal on that path is the secret resolution at services/service-com-amazonaws-secretsmanager-secretsmanager/src/ops/validate_resource_policy.rs:215-232, which answers ResourceNotFoundException with the fixed message "Secrets Manager can't find the specified secret." — the handler resolves SecretId as though it were required, after its syntax checks have already passed.
Why: the operation exists to validate a document BEFORE it is attached — CI linting, console-style pre-flight, or validating the policy of a secret a template has not created yet. Against the emulator every such call fails as a missing resource and there is no way to obtain a syntax verdict without first creating the secret.
Fix: the ValidateResourcePolicy handler — treat SecretId as optional attribution and do not resolve it against the secret store; run the document's syntax/public-access checks and answer 200 with PolicyValidationPassed and ValidationErrors regardless of whether the id names a live secret.
Note: open issue 20261117T143604Z-secretsmanager-validateresourcepolicy-is-refused-resourcenotfoundexception-when-secretid-names.md describes this same defect from the behavioral suite, but it carries no wire replay key, so this cause is filed here to bind it to the failing fixture step (and must be re-replayed from it).
Traces: secretsmanager-resource-policy-operations-on-a-missing-secret#task#2

## Evidence
Wire corpus simparity-wire-aws-v1, replayed against the emulator built from this checkout. The two clusters bundled here, verbatim from the replay:

1) UpdateSecretVersionStage, fixture secretsmanager-cancel-rotate-and-version-stage-on-a-missing-secret, step task#4, expected 400 / actual 400:
   $.__type: value_mismatch (expected 'ResourceNotFoundException', got 'InvalidParameterException')
   $.Message: value_mismatch (expected "Secrets Manager can't find the specified secret.", got 'You must specify either MoveToVersionId or RemoveFromVersionId.')

   Expected (AWS): 400 ResourceNotFoundException, Message "Secrets Manager can't find the specified secret." (wholly fixed text).
   Actual (emulator): 400 InvalidParameterException, Message "You must specify either MoveToVersionId or RemoveFromVersionId."

2) ValidateResourcePolicy, fixture secretsmanager-resource-policy-operations-on-a-missing-secret, step task#2, expected 200 / actual 400:
   status: expected 200, got 400
   $.PolicyValidationPassed: missing (expected True, got None)
   $.ValidationErrors: missing (expected [], got None)

   Expected (AWS): 200 with PolicyValidationPassed=true and ValidationErrors=[].
   Actual (emulator): 400 — the handler's secret resolution (ops/validate_resource_policy.rs:224-232) refuses with ResourceNotFoundException "Secrets Manager can't find the specified secret."; sibling resource-policy calls on the same missing name in the same fixture passed, so only this operation's precondition diverges.

Secret names/ARNs in these fixtures are per-run values and are not part of the requirement; the statuses, the __type codes and the message texts are.

## Steps to Reproduce
1. Boot the emulator from this checkout and point a secretsmanager client at it.
2. Pick a name N no secret was ever created under.
3. update_secret_version_stage(SecretId=N, VersionStage='AWSCURRENT') with neither MoveToVersionId nor RemoveFromVersionId.
   Observed: 400 InvalidParameterException "You must specify either MoveToVersionId or RemoveFromVersionId."
   Expected: 400 ResourceNotFoundException "Secrets Manager can't find the specified secret."
   Then confirm the parameter rule still applies to a secret that DOES exist: create a secret and repeat the call with neither version parameter — that one must still be InvalidParameterException "You must specify either MoveToVersionId or RemoveFromVersionId."
4. validate_resource_policy(SecretId=N, ResourcePolicy='{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"AWS":"arn:aws:iam::<account>:root"},"Action":"secretsmanager:GetSecretValue","Resource":"*"}]}').
   Observed: 400 ResourceNotFoundException "Secrets Manager can't find the specified secret."
   Expected: 200 with PolicyValidationPassed=true and ValidationErrors=[].
5. Check the siblings still match AWS on the same missing name: get_resource_policy / put_resource_policy / delete_resource_policy / cancel_rotate_secret(SecretId=N) must each still raise ResourceNotFoundException "Secrets Manager can't find the specified secret."
6. Re-replay secretsmanager-cancel-rotate-and-version-stage-on-a-missing-secret#task#4 and secretsmanager-resource-policy-operations-on-a-missing-secret#task#2 to confirm both causes are closed.
