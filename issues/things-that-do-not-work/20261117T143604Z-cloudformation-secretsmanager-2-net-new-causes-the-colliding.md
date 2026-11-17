Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__secretsmanager.cfn-secretsmanager-secret-name-collision-on-replacement-rejected-and-rolled-back cloudformation__secretsmanager.cfn-secretsmanager-secret-tag-update-in-place-preserves-the-secret
Issue-Key: cloudformation__secretsmanager.cfn-secretsmanager-secret-name-collision-on-replacement-rejected-and-rolled-back|run#26|AssertionError|assert got == want, f"SecretB's UPDATE_FAILED reason was {got!r}, expected {want!r}"
Issue-Key: cloudformation__secretsmanager.cfn-secretsmanager-secret-tag-update-in-place-preserves-the-secret|_expect#21|AssertionError|raise AssertionError(
Issue-Commit: da5ca9a7bb6657991592368ee3d3c2d494ca2b65

# cloudformation__secretsmanager: 2 net-new causes — the colliding-secret-name handler reason uses HandlerErrorCode InvalidRequest and drops the AWS SDK diagnostic, and an in-place (Tags-only) update of AWS::SecretsManager::Secret answers NotImplemented

> _Note: this issue was identified against the code base at commit `da5ca9a7bb6657991592368ee3d3c2d494ca2b65`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateStack

## Description
Two independent CloudFormation/Secrets Manager defects found by the behavioral replay, both in the AWS::SecretsManager::Secret resource provider path. Cause count: 2.

=== Cause 1 of 2: a stack update whose replacement CreateSecret collides on an existing secret name reports HandlerErrorCode InvalidRequest and a failure reason stripped of the AWS SDK diagnostic suffix ===
AWS: CreateSecret with a Name an active secret already holds is refused with HTTP 400 and Error.Code ResourceExistsException. Smithy (com.amazonaws.secretsmanager#CreateSecret): modeled error ResourceExistsException (client, httpError 400, doc "A resource with the ID you requested already exists."); the service message for that class is the fixed sentence "The operation failed because the secret <name> already exists." where <name> is the Name taken verbatim from the request (recorded behavior for the sentence; the 400 and the error shape are modeled). CloudFormation wraps that service refusal into the failing resource's StackEvent.ResourceStatusReason as: Resource handler returned message: "<service message> (Service: SecretsManager, Status Code: 400, Request ID: <request id>) (SDK Attempt Count: <n>)" (RequestToken: <token>, HandlerErrorCode: AlreadyExists). Fixed text: the "Resource handler returned message:" envelope, the "(Service: <service>, Status Code: <status>, Request ID: <id>) (SDK Attempt Count: <n>)" SDK diagnostic trailer, and the HandlerErrorCode AlreadyExists that CloudFormation assigns to an already-exists service refusal. Variable parts: the secret name (from the template), the status code and service name (from the wrapped refusal), and the per-run request id / request token / attempt count.
Emulator: the event's reason parses as a resource-handler failure, but (a) the wrapped message is the bare sentence "The operation failed because the secret <name> already exists." with the whole "(Service: SecretsManager, Status Code: 400, Request ID: ...) (SDK Attempt Count: ...)" trailer missing, and (b) HandlerErrorCode is InvalidRequest instead of AlreadyExists — i.e. the handler classified the name collision as InvalidRequestException rather than ResourceExistsException. Everything else in the check passed: the stack reached UPDATE_ROLLBACK_COMPLETE, exactly one SecretB UPDATE_FAILED event was recorded, both physical ids survived and both secrets kept their values.
Why: a client cannot tell a name collision from any other bad-request failure. Deploy tooling, CI gates and operators branch on HandlerErrorCode (AlreadyExists is the retry/rename signal, InvalidRequest is not), and the dropped SDK diagnostic removes the service, status code and request id an operator needs to correlate the stack failure with the underlying Secrets Manager call.
Fix: the AWS::SecretsManager::Secret CFN resource handler's create path — map a duplicate-name refusal to ResourceExistsException/HandlerErrorCode AlreadyExists — and the shared wrapper that formats a wrapped service refusal into ResourceStatusReason, which must append the SDK diagnostic trailer. Open issue 20261113T173814Z-secretsmanager-the-aws-secretsmanager-resourcepolicy-create-failed-reason.md reports the same dropped trailer for AWS::SecretsManager::ResourcePolicy and 20261113T030808Z-cloudformation-secretsmanager-2-net-new-causes-the-conflicting.md covers RotationSchedule reason text; neither carries this check's replay key nor the AlreadyExists mis-classification, so this is filed separately and may close together if the wrapper is fixed centrally.
Traces: cloudformation__secretsmanager.cfn-secretsmanager-secret-name-collision-on-replacement-rejected-and-rolled-back

=== Cause 2 of 2: an in-place property update (Tags only) of AWS::SecretsManager::Secret is refused with emulator-authored NotImplemented prose, so the stack rolls back ===
AWS: Tags on AWS::SecretsManager::Secret is an updatable property, so a template change that only edits Tags is an update with no interruption: CloudFormation invokes the Secret resource handler's update (TagResource/UntagResource against the existing secret), the resource records UPDATE_COMPLETE keeping the SAME PhysicalResourceId, and the stack reaches UPDATE_COMPLETE. The secret itself is preserved — same ARN, same Name, same current version id and same SecretString — and DescribeSecret then reports the new tag set. (CloudFormation resource reference for AWS::SecretsManager::Secret; no Smithy statement covers resource-provider update behavior, so this is recorded behavior.)
Emulator: the resource answers UPDATE_FAILED with the invented reason "NotImplemented: this emulator does not change the properties of an existing AWS::SecretsManager::Secret in place; the resource was left as it is.", so the stack ends UPDATE_ROLLBACK_COMPLETE with StackStatusReason "The following resource(s) failed to update: [Secret]. ". No in-place update path exists for this resource type at all, so no property of a template-managed secret (Tags, Description, SecretString) can ever be edited through its stack.
Why: the normal way a stack owns a secret is broken. Every `cfn deploy`/`cdk deploy` that edits a secret's tags or description fails and rolls back, and the reason text is prose AWS never emits, so a client cannot even classify the failure. There is no workaround short of replacing the secret, which changes its ARN.
Fix: the AWS::SecretsManager::Secret resource provider in the CloudFormation resource-handler registry — implement the update (no-interruption) path for the updatable properties and record UPDATE_IN_PROGRESS/UPDATE_COMPLETE against the same physical id. Open issues report the identically shaped NotImplemented refusal for OTHER resource types (e.g. 20261111T003921Z-cloudformation-ecr-an-in-place-update-of-aws.md, 20261117T084005Z-cloudformation-ec2-an-in-place-update-of-aws.md, 20261111T111216Z-apigateway-cloudformation-in-place-updates-of-aws-apigateway-deployment-method.md); none names AWS::SecretsManager::Secret, and 20261111T001055Z-cloudformation-secretsmanager-a-replacement-update-of-aws-secretsmanager.md is about the REPLACEMENT path of this resource, not the in-place path. If the fix lands in a shared update engine these may close together.
Traces: cloudformation__secretsmanager.cfn-secretsmanager-secret-tag-update-in-place-preserves-the-secret

## Evidence
Behavioral suite simparity-behavioral-aws-v1.

Cause 1 — [mismatch] cloudformation__secretsmanager.cfn-secretsmanager-secret-name-collision-on-replacement-rejected-and-rolled-back:
  File "check_cloudformation__secretsmanager_cfn_secretsmanager_secret_name_collision_on_replacement_rejected_and_rolled_back.py", line 101, in run
    assert got == want, f"SecretB's UPDATE_FAILED reason was {got!r}, expected {want!r}"
  AssertionError: SecretB's UPDATE_FAILED reason was ('The operation failed because the secret sim2real-cfnsmcol-a-15a14283ff74 already exists.', 'InvalidRequest'), expected ('The operation failed because the secret sim2real-cfnsmcol-a-15a14283ff74 already exists. (Service: SecretsManager, Status Code: 400, Request ID: <id>) (SDK Attempt Count: <n>)', 'AlreadyExists')

(the check masks the request id and attempt count to <id>/<n> before comparing, so those per-run values are not part of the requirement; the secret name shown is this run's generated name and fills the template's variable slot. The requirements are the SDK diagnostic trailer and HandlerErrorCode AlreadyExists.)

Expected (AWS), ResourceStatusReason of the SecretB UPDATE_FAILED event:
  Resource handler returned message: "The operation failed because the secret <name> already exists. (Service: SecretsManager, Status Code: 400, Request ID: <id>) (SDK Attempt Count: <n>)" (RequestToken: <token>, HandlerErrorCode: AlreadyExists)
Actual (emulator):
  Resource handler returned message: "The operation failed because the secret <name> already exists." (RequestToken: <token>, HandlerErrorCode: InvalidRequest)

Cause 2 — [mismatch] cloudformation__secretsmanager.cfn-secretsmanager-secret-tag-update-in-place-preserves-the-secret:
  File "check_cloudformation__secretsmanager_cfn_secretsmanager_secret_tag_update_in_place_preserves_the_secret.py", line 93, in run
    _expect(cfn, _settle(ctx, cfn, ref["stack"]), "UPDATE_COMPLETE")
  AssertionError: stack ended 'UPDATE_ROLLBACK_COMPLETE' ('The following resource(s) failed to update: [Secret]. '), expected 'UPDATE_COMPLETE'; failed events: [('Secret', 'UPDATE_FAILED', 'NotImplemented: this emulator does not change the properties of an existing AWS::SecretsManager::Secret in place; the resource was left as it is.')]

Expected (AWS): the Tags-only UpdateStack reaches UPDATE_COMPLETE, the Secret resource keeps its PhysicalResourceId, and the secret keeps its ARN/Name/value while DescribeSecret reports the new tags.
Actual (emulator): the Secret resource is UPDATE_FAILED with emulator-authored NotImplemented prose and the stack rolls back.

## Steps to Reproduce
Cause 1:
1. CreateStack with two AWS::SecretsManager::Secret resources, SecretA (Name=NA, SecretString=VA) and SecretB (Name=NB, SecretString=VB); wait for CREATE_COMPLETE and confirm both values read back through GetSecretValue.
2. Note the current event ids, then UpdateStack with the same template except SecretB's Name is changed to NA (Name is create-only, so this forces a replacement whose CreateSecret collides with the live SecretA).
3. Wait for the stack to settle (UPDATE_ROLLBACK_COMPLETE) and read DescribeStackEvents; take the single SecretB UPDATE_FAILED event's ResourceStatusReason.
   Observed: wrapped message "The operation failed because the secret NA already exists." with no "(Service: SecretsManager, Status Code: 400, Request ID: ...) (SDK Attempt Count: ...)" trailer, and HandlerErrorCode: InvalidRequest.
   Expected: the same sentence followed by the SDK diagnostic trailer, and HandlerErrorCode: AlreadyExists.

Cause 2:
1. CreateStack with one AWS::SecretsManager::Secret { Name: N, SecretString: V, Tags: [{Key: k1, Value: v1}] }; wait for CREATE_COMPLETE.
2. UpdateStack with the identical template except Tags: [{Key: k1, Value: v2}] (or an added tag) — a Tags-only, no-interruption change.
3. Wait for the stack to settle.
   Observed: the Secret resource records UPDATE_FAILED with reason "NotImplemented: this emulator does not change the properties of an existing AWS::SecretsManager::Secret in place; the resource was left as it is." and the stack ends UPDATE_ROLLBACK_COMPLETE.
   Expected: UPDATE_COMPLETE, the same PhysicalResourceId, and DescribeSecret(SecretId=N) reporting the updated tag set with the secret's ARN, value and current version unchanged.
