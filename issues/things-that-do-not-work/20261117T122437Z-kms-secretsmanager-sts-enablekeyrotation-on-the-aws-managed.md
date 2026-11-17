Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: kms__secretsmanager__sts.enable-key-rotation-refuses-the-aws-managed-secretsmanager-key
Issue-Key: kms__secretsmanager__sts.enable-key-rotation-refuses-the-aws-managed-secretsmanager-key|run#28|AssertionError|assert code == "AccessDeniedException", (
Issue-Commit: 745763357b0410355460567821d75d30e5935144

# kms__secretsmanager__sts: EnableKeyRotation on the AWS managed alias/aws/secretsmanager key answers UnsupportedOperationException with an empty message instead of AccessDeniedException naming the policy denial

> _Note: this issue was identified against the code base at commit `745763357b0410355460567821d75d30e5935144`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
EnableKeyRotation

## Description
Cause 1 of 1: the refusal for an AWS managed key is classified as unsupported rather than denied by its key policy.
  AWS: recorded behavior only for the specific wording; the mechanism is KMS policy evaluation — an AWS managed key's policy is fixed by the owning service and grants kms:EnableKeyRotation to no principal, so EnableKeyRotation on the key behind alias/aws/secretsmanager is refused with AccessDeniedException and the message 'User: <callerArn> is not authorized to perform: kms:EnableKeyRotation on resource: <keyArn> because no resource-based policy allows the kms:EnableKeyRotation action' — fixed text with the caller ARN (from sts:GetCallerIdentity) and the key ARN as variables. The key's rotation status is unchanged afterwards, and the identical call on a customer managed key succeeds.
  Emulator: the call is refused 'UnsupportedOperationException' with an EMPTY message.
  Why: a client distinguishing 'this key forbids rotation to me' (an authorization problem, remediable by policy) from 'this operation is not supported' takes the wrong branch, and the denial message it logs or shows an operator is empty.
  Fix: the EnableKeyRotation handler must evaluate the target key's resource policy (AWS managed keys included) and emit KMS's AccessDeniedException with the authorization message, instead of a blanket unsupported-operation refusal.
  Traces: kms__secretsmanager__sts.enable-key-rotation-refuses-the-aws-managed-secretsmanager-key.

## Evidence
enable-key-rotation-refuses-the-aws-managed-secretsmanager-key — AssertionError: EnableKeyRotation on the AWS managed key alias/aws/secretsmanager must be refused with AccessDeniedException -- its key policy, fixed by the service, grants the action to no principal; got 'UnsupportedOperationException': ''. Expected message 'User: <caller arn> is not authorized to perform: kms:EnableKeyRotation on resource: <managed key arn> because no resource-based policy allows the kms:EnableKeyRotation action', and GetKeyRotationStatus unchanged.

## Steps to Reproduce
1. Create a secret so the account's alias/aws/secretsmanager managed key exists; resolve it with kms.describe_key(KeyId='alias/aws/secretsmanager') and note its Arn; note sts.get_caller_identity()['Arn'].
2. kms.get_key_rotation_status(KeyId=<managed key>) -> note KeyRotationEnabled.
3. kms.enable_key_rotation(KeyId=<managed key>) -> expect AccessDeniedException with the message above; re-read GetKeyRotationStatus -> unchanged.
4. The same call on a customer managed key succeeds (control).
