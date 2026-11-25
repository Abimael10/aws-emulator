Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: backup-gateway__cloudformation.deploy-backupgateway-hypervisor-invalid-name-rejected
Issue-Key: backup-gateway__cloudformation.deploy-backupgateway-hypervisor-invalid-name-rejected|run#9|AssertionError|assert rows == [
Issue-Commit: 6ccffbe251dc34c011571ac1370edc0cb588a460

# backup-gateway__cloudformation: a pattern-violating AWS::BackupGateway::Hypervisor Name produces no VALIDATION_ERROR OperationEvent, so DescribeEvents reports no validation row for the rolled-back stack

> _Note: this issue was identified against the code base at commit `6ccffbe251dc34c011571ac1370edc0cb588a460`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeEvents

## Description
One net-new root cause found replaying the backup-gateway__cloudformation deploy checks. The failing check is marked neither ALREADY FILED nor OWNED, and no open issue carries its replay key.

Cause 1 of 1: CloudFormation records no VALIDATION_ERROR operation event when a resource property fails its resource-type @pattern validation, so DescribeEvents returns no validation rows for the create it rolled back.

AWS: Smithy — cloudformation DescribeEvents is paginated over OperationEvents and returns OperationEvent rows; OperationEvent models EventType (enum STACK_EVENT | PROGRESS_EVENT | VALIDATION_ERROR | PROVISIONING_ERROR | HOOK_INVOCATION_ERROR) together with LogicalResourceId and the validation members ValidationName, ValidationStatus (enum FAILED | SKIPPED), ValidationFailureMode, ValidationStatusReason and ValidationPath ("The path within the resource where the validation was applied"). backup-gateway Smithy — Hypervisor$Name targets Name, @length(1,100) @pattern("^[a-zA-Z0-9-]*$"), so a Name containing underscores is outside the resource type's schema. Recorded behavior for the event payload: a CreateStack whose AWS::BackupGateway::Hypervisor carries such a Name is refused during validation, the stack settles ROLLBACK_COMPLETE, and DescribeEvents reports exactly one row whose EventType is VALIDATION_ERROR, with LogicalResourceId = the offending resource's logical id, ValidationName "PROPERTY_VALIDATION", ValidationStatus "FAILED", ValidationFailureMode "FAIL", ValidationStatusReason "Property value [<REDACTED>] does not match pattern: <pattern>" (fixed text "Property value [<REDACTED>] does not match pattern: " — the offending value itself is redacted by CloudFormation, and the trailing pattern is the resource type's pattern for that property, here ^[a-zA-Z0-9-]*$) and ValidationPath the JSON pointer of the property in the template ("/Resources/<LogicalId>/Properties/Name").

Emulator: the stack outcome is right — the create is refused and the stack reaches ROLLBACK_COMPLETE, and ListHypervisors correctly does not list the refused hypervisor — but DescribeEvents returns no OperationEvent with EventType VALIDATION_ERROR at all: the collected validation rows are []. Nothing names the resource, the property path, or the reason. (The positive control is never reached, so the check fails at the validation-row assertion: "AssertionError: unexpected validation rows []".)

Why: a client (and the console) diagnoses a refused deploy from the VALIDATION_ERROR operation events. With none emitted, a rolled-back create is indistinguishable from any other failure, and automation cannot report which property was rejected or what pattern it had to satisfy; a 200 with an empty OperationEvents list reads exactly like a healthy stack.

Fix: the cloudformation operation-event recording path behind DescribeEvents — the template/resource property-validation stage must record a VALIDATION_ERROR OperationEvent (ValidationName PROPERTY_VALIDATION, ValidationStatus FAILED, ValidationFailureMode FAIL, the pattern-mismatch reason text with the value redacted, and the property's JSON pointer) for the resource whose property it refused, and DescribeEvents must serve it. This likely also requires the AWS::BackupGateway::Hypervisor resource-type schema to carry the Name pattern so the validation stage has a rule to apply.

Related open issues (neither carries this check's replay key, and both are about different validation stages/resource types): 20261115T190121Z-appsync-cloudformation-a-property-validation-failure-invalid-aws.md (enum validation on AWS::AppSync::SourceApiAssociation MergeType during an update) and 20261115T073242Z-cloudformation-2-net-new-causes-a-failure-wait.md (its Cause 2 is a NAME_CONFLICT_VALIDATION row for a stack-set instance). This one is a @pattern property validation on a create of AWS::BackupGateway::Hypervisor.

Traces: backup-gateway__cloudformation.deploy-backupgateway-hypervisor-invalid-name-rejected.

## Evidence
backup-gateway__cloudformation.deploy-backupgateway-hypervisor-invalid-name-rejected:
  The stack with the pattern-violating Name correctly reaches ROLLBACK_COMPLETE (that assertion passes).
  expected (rows from DescribeEvents OperationEvents where EventType == 'VALIDATION_ERROR', as (LogicalResourceId, ValidationName, ValidationStatus, ValidationFailureMode, ValidationStatusReason, ValidationPath)):
    [('Hyp', 'PROPERTY_VALIDATION', 'FAILED', 'FAIL', 'Property value [<REDACTED>] does not match pattern: ^[a-zA-Z0-9-]*$', '/Resources/Hyp/Properties/Name')]
  actual:
    File "check_backup_gateway__cloudformation_deploy_backupgateway_hypervisor_invalid_name_rejected.py", line 200, in run
      assert rows == [...]
    AssertionError: unexpected validation rows []
  Fixed vs variable parts of the expected row: the VALIDATION_ERROR EventType itself, ValidationName 'PROPERTY_VALIDATION', ValidationStatus 'FAILED', ValidationFailureMode 'FAIL' and the reason's fixed text 'Property value [<REDACTED>] does not match pattern: ' are requirements; the trailing pattern is the resource type's pattern for the property (^[a-zA-Z0-9-]*$ for AWS::BackupGateway::Hypervisor Name, per backup-gateway Smithy Name @pattern); 'Hyp' is the template's logical id and so is the ValidationPath '/Resources/Hyp/Properties/Name' — both come from the request. The per-run generated hypervisor name is not part of the message (AWS redacts the value).

## Steps to Reproduce
1. Clone the repo and run the behavioral suite (service backup-gateway__cloudformation), or re-run the check in trace_ids.
2. CreateStack with a template containing an AWS::SecretsManager::Secret (GenerateSecretString for the password) and an AWS::BackupGateway::Hypervisor 'Hyp' with Host 10.0.0.5, Username svc-backup-admin, Password resolved from that secret, and a Name that violates ^[a-zA-Z0-9-]*$ (e.g. one containing underscores).
3. Poll DescribeStacks until terminal: the stack reaches ROLLBACK_COMPLETE (correct).
4. Call cloudformation DescribeEvents(StackName=<that stack id>) across every page and keep the OperationEvents whose EventType is 'VALIDATION_ERROR': the list is empty, instead of the single ('Hyp', 'PROPERTY_VALIDATION', 'FAILED', 'FAIL', 'Property value [<REDACTED>] does not match pattern: ^[a-zA-Z0-9-]*$', '/Resources/Hyp/Properties/Name') row.
