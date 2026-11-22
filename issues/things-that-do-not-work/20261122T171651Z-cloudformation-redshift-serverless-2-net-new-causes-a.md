Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__redshift-serverless.deploy-redshiftserverless-namespace-round-trip-then-delete cloudformation__redshift-serverless.deploy-redshiftserverless-namespace-invalid-name-rejected
Issue-Key: cloudformation__redshift-serverless.deploy-redshiftserverless-namespace-round-trip-then-delete|run#25|AssertionError|assert re.fullmatch(secret_re, secret_arn), (
Issue-Key: cloudformation__redshift-serverless.deploy-redshiftserverless-namespace-invalid-name-rejected|run#15|AssertionError|assert rows == [
Issue-Commit: 53ca3190ddccd906197b0d4d5e8474d7add13c5d

# cloudformation__redshift-serverless: 2 net-new causes — a CFN-provisioned namespace with ManageAdminPassword carries no adminPasswordSecretArn, and a property-validation rollback emits no VALIDATION_ERROR operation event

> _Note: this issue was identified against the code base at commit `53ca3190ddccd906197b0d4d5e8474d7add13c5d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
Two independent root causes on the CloudFormation -> Redshift Serverless namespace path. Both stacks now reach their expected terminal status (CREATE_COMPLETE / ROLLBACK_COMPLETE), so these are post-provisioning divergences, not the resource-type refusal tracked by the open issue 20261109T124343Z-cloudformation-redshift-serverless-aws-redshiftserverless-namespace-is-not.md.

Cause 1 of 2: a namespace provisioned by AWS::RedshiftServerless::Namespace with ManageAdminPassword=true never gets a managed admin-password secret, so the namespace carries no adminPasswordSecretArn.
AWS: Smithy (redshift-serverless): CreateNamespace accepts manageAdminPassword (Boolean) and adminPasswordSecretKmsKeyId, and the Namespace output shape declares adminPasswordSecretArn — "The Amazon Resource Name (ARN) for the namespace's admin user credentials secret" — returned by GetNamespace (output member namespace, required). Recorded behavior: with ManageAdminPassword=true and no admin password supplied, the namespace settles carrying adminPasswordSecretArn of the form arn:<partition>:secretsmanager:<region>:<account>:secret:<name>-<suffix>, i.e. a Secrets Manager secret of the same account and region (the partition, region, account and secret name/suffix all come from the request and the emulator's own state; no literal value from one run is part of the requirement).
Emulator: the stack reaches CREATE_COMPLETE and GetNamespace on the created namespace returns a namespace with no usable adminPasswordSecretArn at all (the check reads it as the empty string), so no secret was provisioned and nothing identifies the managed credentials. All earlier beats passed: Ref, GetAtt Namespace.NamespaceArn/NamespaceId/AdminUsername/DbName and ListNamespaces all agree.
Why: a template that hands password management to the service gets a namespace whose credentials are unreachable — there is no ARN to pass to Secrets Manager for retrieval, rotation, or IAM/KMS policy — and a client cannot distinguish "managed password" from "explicit password" namespaces at all.
Fix: the redshift-serverless CreateNamespace handler plus the namespace state/serializer (provision the backing service-owned secret on manageAdminPassword=true and project its ARN into Namespace.adminPasswordSecretArn for CreateNamespace/GetNamespace/ListNamespaces), and the AWS::RedshiftServerless::Namespace resource handler so the CloudFormation path exercises it. The same root cause is already reported against the native API in the open issue 20261117T115004Z-redshift-serverless-secretsmanager-3-net-new-causes-manageadminpassword.md (Cause 1); that issue does not carry this check's replay key, so this CloudFormation-path failure is filed here.
Traces: cloudformation__redshift-serverless.deploy-redshiftserverless-namespace-round-trip-then-delete.

Cause 2 of 2: a stack whose AWS::RedshiftServerless::Namespace NamespaceName violates the modelled pattern rolls back, but CloudFormation publishes no property-validation event for it.
AWS: Smithy (redshift-serverless): NamespaceName is @length(min 3, max 64) @pattern "^[a-z0-9-]+$" on CreateNamespace/GetNamespace and on Namespace.namespaceName, so an uppercase name is refused by CloudFormation's early property validation. Smithy (cloudformation): DescribeEvents returns OperationEvents, and OperationEvent declares EventType (enum, VALIDATION_ERROR), ValidationName, ValidationStatus (enum), ValidationFailureMode, ValidationStatusReason and ValidationPath. Recorded behavior: the rejected stack ends ROLLBACK_COMPLETE and DescribeEvents reports exactly one VALIDATION_ERROR operation event for the offending resource, with ValidationName PROPERTY_VALIDATION, ValidationStatus FAILED, ValidationFailureMode FAIL, ValidationPath "/Resources/<LogicalId>/Properties/NamespaceName" and ValidationStatusReason "Property value [<the submitted name>] does not match pattern: ^[a-z0-9-]+$" — fixed text "Property value [", "] does not match pattern: " plus the value from the request and the pattern from the model.
Emulator: the stack does end ROLLBACK_COMPLETE, but DescribeEvents(StackName=<stack>) yields no OperationEvents with EventType VALIDATION_ERROR at all — the validation-row list is empty — so the reason, the failing property path and the failure mode are never reported.
Why: a client (or the console/CLI) that explains a rollback from the validation events gets nothing to show: it cannot tell WHICH property was rejected, at what path, or why, and cannot distinguish an early property-validation refusal from a provisioning failure. Tooling that gates on ValidationStatus/ValidationFailureMode sees an empty event stream and reads the rollback as unexplained.
Fix: CloudFormation's template/property validation stage and its operation-event recorder — emit a VALIDATION_ERROR OperationEvent (ValidationName PROPERTY_VALIDATION, ValidationStatus FAILED, ValidationFailureMode FAIL, ValidationPath of the offending property, ValidationStatusReason rendered from the member's pattern/length constraint) when a resource property fails pre-provision validation, and surface it through DescribeEvents.
Traces: cloudformation__redshift-serverless.deploy-redshiftserverless-namespace-invalid-name-rejected.

## Evidence
Cause 1 — cloudformation__redshift-serverless.deploy-redshiftserverless-namespace-round-trip-then-delete [mismatch]:
  File "check_cloudformation__redshift_serverless_deploy_redshiftserverless_namespace_round_trip_then_delete.py", line 240, in run
    assert re.fullmatch(secret_re, secret_arn), (...)
  AssertionError: ManageAdminPassword: adminPasswordSecretArn '' is not a secret ARN
Expected: GetNamespace(namespaceName=<ns>)['namespace']['adminPasswordSecretArn'] matching arn:<partition>:secretsmanager:<region>:<account>:secret:<name>. Actual: absent/empty. Ref, the four GetAtt values and ListNamespaces all matched before this assertion.

Cause 2 — cloudformation__redshift-serverless.deploy-redshiftserverless-namespace-invalid-name-rejected [mismatch]:
  File "check_cloudformation__redshift_serverless_deploy_redshiftserverless_namespace_invalid_name_rejected.py", line 202, in run
    assert rows == [...]
  AssertionError: validation rows []
Expected (one row, from DescribeEvents OperationEvents filtered to EventType == VALIDATION_ERROR, read as (LogicalResourceId, ValidationName, ValidationStatus, ValidationFailureMode, ValidationStatusReason, ValidationPath)):
  ("Namespace", "PROPERTY_VALIDATION", "FAILED", "FAIL", "Property value [<submitted NamespaceName>] does not match pattern: ^[a-z0-9-]+$", "/Resources/Namespace/Properties/NamespaceName")
Actual: [] — no VALIDATION_ERROR operation event exists, although the stack correctly ended ROLLBACK_COMPLETE and the lowercase control stack reached CREATE_COMPLETE.

## Steps to Reproduce
Cause 1: CreateStack with a template containing one AWS::RedshiftServerless::Namespace {NamespaceName: <lowercase unique name>, AdminUsername: sim2realadmin, DbName: sim2realdb, ManageAdminPassword: true}; wait CREATE_COMPLETE; call redshift-serverless GetNamespace(namespaceName=<name>) — the returned namespace has no adminPasswordSecretArn, and no corresponding Secrets Manager secret exists.

Cause 2: CreateStack with the same template but an uppercase NamespaceName (e.g. ...-BAD-...); wait for ROLLBACK_COMPLETE; call cloudformation DescribeEvents(StackName=<stack-id>) and page OperationEvents — there is no event with EventType VALIDATION_ERROR, so no ValidationName/ValidationStatus/ValidationStatusReason/ValidationPath for /Resources/Namespace/Properties/NamespaceName. A lowercase control stack of the same template reaches CREATE_COMPLETE.
