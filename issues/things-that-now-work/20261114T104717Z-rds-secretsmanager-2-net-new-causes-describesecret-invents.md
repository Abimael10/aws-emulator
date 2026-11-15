Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: rds__secretsmanager.refarch-aur-e2e-review-aurora-production-readiness-lifecycle rds__secretsmanager.refarch-aur-protected-cluster-blocks-stack-delete-then-succeeds-after-unprotect
Issue-Key: rds__secretsmanager.refarch-aur-e2e-review-aurora-production-readiness-lifecycle|phase#4|AssertionError|raise AssertionError(f"[phase {i}/{len(self.phases)} {name}] {exc}") from exc
Issue-Key: rds__secretsmanager.refarch-aur-protected-cluster-blocks-stack-delete-then-succeeds-after-unprotect|_error#33|AssertionError|raise AssertionError(f"expected a rejection, but the call succeeded: {result!r}")
Issue-Commit: 7f016f3356818cac761a999c51caadd7d8cdcae1

# rds__secretsmanager: 2 net-new causes — DescribeSecret invents RotationEnabled on a never-rotated secret, and a stack delete that ends DELETE_FAILED leaves its DELETE_COMPLETE DeletionPolicy=Delete secret merely scheduled instead of removed

> _Note: this issue was identified against the code base at commit `7f016f3356818cac761a999c51caadd7d8cdcae1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeSecret

## Description
Two distinct net-new root causes behind the two rds__secretsmanager failures the replay report does not mark OWNED. Both are reached through secretsmanager.DescribeSecret, but they are separate defects in separate components and must both be closed.

Cause 1 of 2: DescribeSecret reports RotationEnabled for a secret that never had rotation configured.
Failing check ids: rds__secretsmanager.refarch-aur-e2e-review-aurora-production-readiness-lifecycle.
AWS: Smithy secretsmanager DescribeSecretResponse$RotationEnabled is an OPTIONAL boolean with no modeled default, sitting alongside the equally optional RotationLambdaARN, RotationRules, LastRotatedDate and NextRotationDate. For a hand-created secret on which rotation has never been configured (no RotateSecret, no AWS::SecretsManager::RotationSchedule), AWS omits every one of those rotation members from the response rather than returning false/null — boto3 therefore renders RotationEnabled as absent (None), never as False. Recorded behavior confirms the omission: the check grades presence, not value.
Emulator: the DescribeSecret envelope for such a secret carries RotationEnabled: false. The replay fails with "an unrotated secret must report NO RotationEnabled (absent, which boto3 renders as None) -- not False; got False" in phase 2/4 (solve) of the production-readiness lifecycle.
Why: a client that tests member PRESENCE to decide whether rotation was ever configured — drift detection, compliance reporting, "turn rotation on if it was never set up" remediation — concludes "rotation is configured and currently disabled" for a secret rotation never touched, and takes the wrong branch.
Fix: the DescribeSecret serializer / secret-state projection in the Secrets Manager service — leave the rotation members out of the envelope until rotation is actually configured, instead of defaulting them.
Note: the SAME root cause is already filed as Cause 5 of 20261104T044754Z-rds-secretsmanager-7-net-new-root-causes-create.md, whose Issue-Trace-Ids list carries only rds__secretsmanager.refarch-aur-never-rotated-secret-omits-every-rotation-field. That issue does not carry the check above, so this check is not covered by it; closing one should close the other, and this entry exists so the lifecycle check is re-replayed.
Traces: rds__secretsmanager.refarch-aur-e2e-review-aurora-production-readiness-lifecycle (phase 2/4 'solve').

Cause 2 of 2: when a stack delete ends DELETE_FAILED, the AWS::SecretsManager::Secret it did delete (DeletionPolicy=Delete) is only SCHEDULED for deletion, so DescribeSecret still answers 200 for it even though its stack resource reports DELETE_COMPLETE.
Failing check ids: rds__secretsmanager.refarch-aur-protected-cluster-blocks-stack-delete-then-succeeds-after-unprotect.
AWS: CloudFormation deletes the resources a stack delete reached even when a sibling resource blocks the stack (here an AWS::RDS::DBCluster with DeletionProtection=true fails, and the independent secret is still removed). For AWS::SecretsManager::Secret with DeletionPolicy/UpdateReplacePolicy = Delete the handler removes the secret outright, with no recovery window — DeleteSecret models exactly that switch, ForceDeleteWithoutRecovery (BooleanType) alongside RecoveryWindowInDays. Once that resource reports DELETE_COMPLETE the secret and its name are gone, and DescribeSecret on its ARN or name is refused: Smithy DescribeSecret errors include ResourceNotFoundException (client error, httpError 400), and the message AWS words for this error class is the fixed sentence "Secrets Manager can't find the specified secret." — fully fixed text, with no part taken from the request (the SecretId is NOT echoed).
Emulator: DescribeSecret on that secret's ARN answers HTTP 200 with the full secret still described — ARN, Name, Description, Tags, CreatedDate and VersionIdsToStages still mapping its version to AWSCURRENT — plus a DeletedDate member equal to LastChangedDate. That is the shape of a secret scheduled for deletion under a recovery window, so the CloudFormation delete leg is issuing a recovery-window DeleteSecret rather than an immediate one (or DeleteSecret ignores ForceDeleteWithoutRecovery and only flags DeletedDate). Secondarily, the DeletedDate it returns is the moment of the call rather than the end of any recovery window, so even read as a scheduled delete the envelope is self-inconsistent.
Why: a client relying on AWS's behavior treats DELETE_COMPLETE on the secret resource as proof the secret is gone. Against the emulator (a) the name stays taken, so redeploying the same template with the same NamePrefix collides on a name AWS had freed, (b) code that probes DescribeSecret to confirm cleanup after a partially failed stack delete reads 200/present where AWS refuses with 400 ResourceNotFoundException, and (c) credential material the delete should have destroyed stays readable for the whole emulated recovery window.
Fix: the CloudFormation resource handler for AWS::SecretsManager::Secret — its delete leg must perform an immediate, no-recovery-window delete so the secret is purged from Secrets Manager state; if it already requests a force delete, the defect is in the Secrets Manager DeleteSecret handler/state, which flags DeletedDate instead of purging when ForceDeleteWithoutRecovery is set. DescribeSecret must then refuse the ARN and the name with ResourceNotFoundException (400) and the fixed message above.
Note: nearby already-open issues describe adjacent legs of this same family but do not carry this check — 20261108T215646Z-rds-secretsmanager-a-rolled-back-cloudformation-create-leaves.md (the ROLLBACK leg), 20261108T215449Z-secretsmanager-2-net-new-causes-a-stack-created.md and 20261109T043550Z-secretsmanager-5-net-new-causes-cloudformation-stack-delete.md (clean stack delete), 20261109T223435Z-cloudformation-secretsmanager-4-net-new-causes-describesecret-still.md and 20261110T165732Z-cloudformation-secretsmanager-ssm-a-stack-deleted-aws-secretsmanager.md. None of them covers the DELETE_FAILED-stack leg replayed here.
Traces: rds__secretsmanager.refarch-aur-protected-cluster-blocks-stack-delete-then-succeeds-after-unprotect.

## Evidence
Both quoted from the behavioral replay report (suite simparity-behavioral-aws-v1, service group rds__secretsmanager 7/15 passed, 8 failed — the other six failures are marked OWNED and skipped).

Cause 1 — [mismatch] rds__secretsmanager.refarch-aur-e2e-review-aurora-production-readiness-lifecycle:
  AssertionError: [phase 2/4 solve] an unrotated secret must report NO RotationEnabled (absent, which boto3 renders as None) -- not False; got False
Expected (AWS): DescribeSecret on a secret that never had rotation configured returns an envelope with NO RotationEnabled / RotationLambdaARN / RotationRules / LastRotatedDate / NextRotationDate members.
Actual (emulator): the envelope contains RotationEnabled: false.

Cause 2 — [mismatch] rds__secretsmanager.refarch-aur-protected-cluster-blocks-stack-delete-then-succeeds-after-unprotect:
  AssertionError: expected a rejection, but the call succeeded: {'ARN': 'arn:aws:secretsmanager:<region>:<account>:secret:<prefix>-master-<suffix>', 'Name': '<prefix>-master', 'Description': 'sim2real refarch-aur cluster credentials, deliberately unrotated', 'RotationEnabled': False, 'LastChangedDate': <t>, 'DeletedDate': <t>, 'Tags': [{'Key': 'sim2real', 'Value': 'refarch-aur'}], 'VersionIdsToStages': {'<version>': ['AWSCURRENT']}, 'CreatedDate': <t0>, 'ResponseMetadata': {'HTTPStatusCode': 200, ...}}
(the ARN suffix, generated prefix, version id and timestamps are per-run values, not requirements).
The check had already asserted and PASSED that the stack reached DELETE_FAILED naming only the protected cluster, that the cluster survived 'available' with DeletionProtection true, and that the MasterSecret stack resource reported ResourceStatus DELETE_COMPLETE. It then asserts: "the secret must already be gone once its own resource reports DELETE_COMPLETE" — DescribeSecret must raise ResourceNotFoundException (HTTP 400, message "Secrets Manager can't find the specified secret.").
Expected (AWS): 400 ResourceNotFoundException / "Secrets Manager can't find the specified secret."
Actual (emulator): HTTP 200 with the secret described and merely carrying DeletedDate.

## Steps to Reproduce
Build and boot the emulator, then replay the two checks in trace_ids. Manually:

Cause 1:
1. secretsmanager.create_secret(Name=<n>, SecretString='{"username":"dbadmin","password":"..."}') and never configure rotation on it.
2. secretsmanager.describe_secret(SecretId=<n>).
   AWS: the response has no RotationEnabled member at all (boto3 -> key absent).
   Emulator: RotationEnabled: False is present.

Cause 2:
1. CreateStack a template with (a) an AWS::RDS::DBCluster (aurora-postgresql, Serverless v2) with DeletionProtection: true and DeletionPolicy/UpdateReplacePolicy Delete, and (b) an independent AWS::SecretsManager::Secret with DeletionPolicy/UpdateReplacePolicy Delete. Wait for CREATE_COMPLETE and for the cluster to be 'available'.
2. DeleteStack and wait: the stack ends DELETE_FAILED naming only the cluster; describe_stack_resources shows the secret's resource as DELETE_COMPLETE.
3. secretsmanager.describe_secret(SecretId=<the secret ARN from the stack output>).
   AWS: 400 ResourceNotFoundException, "Secrets Manager can't find the specified secret."
   Emulator: HTTP 200 with the full secret plus DeletedDate == LastChangedDate (scheduled, not deleted).
