Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: secretsmanager.resource-policy-and-rotation-calls-on-a-missing-secret-rejected
Issue-Shape-Id: com.amazonaws.secretsmanager#secretsmanager
Issue-Key: secrets-manager|ValidateResourcePolicy|ResourceNotFoundException
Issue-Commit: da5ca9a7bb6657991592368ee3d3c2d494ca2b65

# secretsmanager: ValidateResourcePolicy is refused ResourceNotFoundException when SecretId names no existing secret, instead of returning a syntax verdict

> _Note: this issue was identified against the code base at commit `da5ca9a7bb6657991592368ee3d3c2d494ca2b65`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ValidateResourcePolicy

## Description
REOPENS 20261024T190718Z-secretsmanager-validateresourcepolicy-stub.md: its fix left the call refused. Cause count: 1.

=== Cause 1 of 1: ValidateResourcePolicy requires SecretId to resolve ===
AWS: Smithy (com.amazonaws.secretsmanager#ValidateResourcePolicy): input SecretId targets SecretIdType (@length 1..2048) and is NOT required, while ResourcePolicy (NonEmptyResourcePolicyType, @length 1..20480) IS required; output is PolicyValidationPassed (boolean, default false) and ValidationErrors. Modeled errors: InternalServiceError (500), InvalidParameterException (400), InvalidRequestException (400), MalformedPolicyDocumentException (400), ResourceNotFoundException (400). Because SecretId is optional and the operation only checks the document's syntax and policy rules, a request carrying a SecretId that names no secret still gets a verdict: AWS answers HTTP 200 with PolicyValidationPassed=true for a well-formed, non-public policy (recorded behavior; the model states only that SecretId is optional). ResourceNotFoundException is reserved for operations that must resolve the secret — GetResourcePolicy, PutResourcePolicy, DeleteResourcePolicy and CancelRotateSecret on a name no secret ever used are each refused 400 ResourceNotFoundException with the fixed message "Secrets Manager can't find the specified secret."
Emulator: ValidateResourcePolicy(SecretId=<a name no secret ever used>, ResourcePolicy=<a well-formed policy>) is refused with ResourceNotFoundException "An error occurred (ResourceNotFoundException) when calling the ValidateResourcePolicy operation: Secrets Manager can't find the specified secret." — the handler resolves SecretId as if it were required. The four sibling refusals immediately before it in the same check (GetResourcePolicy, PutResourcePolicy, DeleteResourcePolicy, CancelRotateSecret on the same missing name) matched AWS exactly, so only this operation's secret-resolution precondition diverges.
Why: the operation exists so a caller can validate a policy document BEFORE attaching it — CI linting, a console-style pre-flight check, or validating a policy for a secret that has not been created yet (e.g. a template about to be deployed). Against the emulator every such call fails with a not-found error the client must mis-handle as a missing resource, and there is no way to get a syntax verdict without first creating the secret. The uncaught refusal also aborts the rest of the check, so its positive-control half (PutResourcePolicy/GetResourcePolicy/ValidateResourcePolicy/DeleteResourcePolicy/CancelRotateSecret on an existing secret) was never exercised and must be re-replayed with the fix.
Fix: the ValidateResourcePolicy handler in the secretsmanager service crate — treat SecretId as optional attribution and do not resolve it against the secret store; run the document's syntax/public-policy validation and answer 200 with PolicyValidationPassed and ValidationErrors regardless of whether the id names a live secret.
Traces: secretsmanager.resource-policy-and-rotation-calls-on-a-missing-secret-rejected

## Evidence
Behavioral suite simparity-behavioral-aws-v1, check [error] secretsmanager.resource-policy-and-rotation-calls-on-a-missing-secret-rejected:

  File "client.py", line 1094, in _make_api_call
    raise error_class(parsed_response, operation_name)
  botocore.errorfactory.ResourceNotFoundException: An error occurred (ResourceNotFoundException) when calling the ValidateResourcePolicy operation: Secrets Manager can't find the specified secret.

The call that raised: validate_resource_policy(SecretId=<a generated name no secret was ever created under>, ResourcePolicy='{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"AWS":"arn:aws:iam::<account>:root"},"Action":"secretsmanager:GetSecretValue","Resource":"*"}]}').

Expected (AWS): HTTP 200 with PolicyValidationPassed=true (SecretId is an optional attribution field; the document is well formed and not public).
Actual (emulator): HTTP 400 ResourceNotFoundException "Secrets Manager can't find the specified secret." — an unhandled refusal that ends the check.

Passed in the same check immediately before (so this is specific to ValidateResourcePolicy): GetResourcePolicy, PutResourcePolicy, DeleteResourcePolicy and CancelRotateSecret on the same missing name each returned ('ResourceNotFoundException', "Secrets Manager can't find the specified secret.") as AWS does.

(The secret name and account above are per-run values and are not part of the requirement.)

## Steps to Reproduce
1. Boot the emulator from this checkout and point a secretsmanager client at it.
2. Pick a name N that no secret was ever created under.
3. Confirm the sibling behavior still matches AWS: get_resource_policy(SecretId=N), put_resource_policy(SecretId=N, ResourcePolicy=<policy>), delete_resource_policy(SecretId=N) and cancel_rotate_secret(SecretId=N) each raise ResourceNotFoundException "Secrets Manager can't find the specified secret."
4. Call validate_resource_policy(SecretId=N, ResourcePolicy='{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"AWS":"arn:aws:iam::<account>:root"},"Action":"secretsmanager:GetSecretValue","Resource":"*"}]}').
   Observed: ResourceNotFoundException "Secrets Manager can't find the specified secret."
   Expected: HTTP 200 with PolicyValidationPassed=true (and no ValidationErrors).
5. Also re-run the check's control half, which this refusal prevented: create a secret, then PutResourcePolicy / GetResourcePolicy (policy reads back) / ValidateResourcePolicy (PolicyValidationPassed=true) / DeleteResourcePolicy (policy cleared) / CancelRotateSecret (returns the secret's ARN).
