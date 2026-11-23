Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__ec2__iam__sagemaker.deploy-sagemaker-domain-app-network-access-type-invalid-value-rejected
Issue-Key: cloudformation__ec2__iam__sagemaker.deploy-sagemaker-domain-app-network-access-type-invalid-value-rejected|run#14|AssertionError|assert rows == want_rows, f"validation rows {rows!r}, expected {want_rows!r}"
Issue-Commit: 3ae1846d4c1ce91fa3a8a650fb4cf01548153921

# cloudformation__ec2__iam__sagemaker: an AWS::SageMaker::Domain with an out-of-enum AppNetworkAccessType rolls the stack back but DescribeEvents emits no VALIDATION_ERROR OperationEvent

> _Note: this issue was identified against the code base at commit `3ae1846d4c1ce91fa3a8a650fb4cf01548153921`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeEvents

## Description
One net-new root cause in CloudFormation's early property-validation reporting, seen on an AWS::SageMaker::Domain whose AppNetworkAccessType is outside the resource type's enum.

Cause 1 of 1: the stack is correctly refused and rolled back, but the operation-event record that says WHY is never produced, so DescribeEvents reports no validation error for the rejected property.

AWS: Smithy (com.amazonaws.cloudformation) — DescribeEvents takes StackName/ChangeSetName/OperationId/Filters/NextToken and returns `OperationEvents: list<OperationEvent>` (paginated on NextToken, items OperationEvents). OperationEvent.EventType is an enum whose values are STACK_EVENT, PROGRESS_EVENT, VALIDATION_ERROR, PROVISIONING_ERROR, HOOK_INVOCATION_ERROR, and OperationEvent carries the validation members ValidationName (string), ValidationStatus (enum FAILED|SKIPPED), ValidationFailureMode (HookFailureMode, enum FAIL|WARN), ValidationStatusReason (string) and ValidationPath (string) beside LogicalResourceId. When a template property's value is outside the resource type's schema enum, AWS fails the operation in property validation and records exactly ONE VALIDATION_ERROR OperationEvent for that resource, with LogicalResourceId = the template's logical id, ValidationName = PROPERTY_VALIDATION, ValidationStatus = FAILED, ValidationFailureMode = FAIL, ValidationPath = /Resources/<LogicalId>/Properties/<PropertyName>, and ValidationStatusReason = "<the rejected value> is not a valid enum value. Supported values: [<the resource schema's permitted values, comma-separated>]". In that message the fixed text is " is not a valid enum value. Supported values: [" and the closing "]"; the variable parts are the rejected value from the request's template, the permitted-value list from the resource type's schema, and the logical id plus property name in the path. For this template that reads: ('Domain', 'PROPERTY_VALIDATION', 'FAILED', 'FAIL', 'BadAccessType is not a valid enum value. Supported values: [PublicInternetOnly, VpcOnly]', '/Resources/Domain/Properties/AppNetworkAccessType').

Emulator: the stack does end ROLLBACK_COMPLETE (that part matches), and the domain is correctly absent from ListDomains — but DescribeEvents(StackName=<stack id>) yields NO OperationEvent whose EventType is VALIDATION_ERROR. The projection of (LogicalResourceId, ValidationName, ValidationStatus, ValidationFailureMode, ValidationStatusReason, ValidationPath) over those events is the empty list `[]` where AWS returns the single row above. The property-validation outcome exists only as a rollback, never as an addressable event.

Why: a client diagnosing a refused deployment reads the VALIDATION_ERROR rows of DescribeEvents to learn WHICH property of WHICH resource was wrong and what values are allowed — that is the whole point of the validation members. Against the emulator it gets a bare ROLLBACK_COMPLETE with no path, no reason and no failure mode, cannot distinguish a template property-schema rejection from a downstream provisioning failure, and any CI gate or console view that renders those rows renders nothing.

Fix: CloudFormation's template/property validation stage and the operation-event store DescribeEvents reads — the validation stage must emit a VALIDATION_ERROR OperationEvent (ValidationName/ValidationStatus/ValidationFailureMode/ValidationStatusReason/ValidationPath populated as above) when a resource property value is outside its resource-type schema enum, and the AWS::SageMaker::Domain resource schema must declare AppNetworkAccessType's permitted values so the reason can name them.

Related but not covering this check: open issues 20261115T073242Z-cloudformation-2-net-new-causes-a-failure-wait..., 20261117T005020Z-backup-gateway-cloudformation-a-pattern-violating-aws-backupgateway... and 20261123T033806Z-cloudformation-redshift-an-aws-redshift-eventsubscription-with-a... describe the same missing-VALIDATION_ERROR shape under other service headings; none carries this check's replay key, and the report lists this check as neither already filed nor owned.

Traces: cloudformation__ec2__iam__sagemaker.deploy-sagemaker-domain-app-network-access-type-invalid-value-rejected.

## Evidence
Expected (AWS): the VALIDATION_ERROR OperationEvents of the rolled-back stack project to exactly
  [('Domain', 'PROPERTY_VALIDATION', 'FAILED', 'FAIL', 'BadAccessType is not a valid enum value. Supported values: [PublicInternetOnly, VpcOnly]', '/Resources/Domain/Properties/AppNetworkAccessType')]

Actual (emulator): [] — DescribeEvents returns no OperationEvent with EventType == 'VALIDATION_ERROR'. Replay output:
  [mismatch] cloudformation__ec2__iam__sagemaker.deploy-sagemaker-domain-app-network-access-type-invalid-value-rejected
  File "check_cloudformation__ec2__iam__sagemaker_deploy_sagemaker_domain_app_network_access_type_invalid_value_rejected.py", line 414, in run
    assert rows == want_rows, f"validation rows {rows!r}, expected {want_rows!r}"
  AssertionError: validation rows [], expected [('Domain', 'PROPERTY_VALIDATION', 'FAILED', 'FAIL', 'BadAccessType is not a valid enum value. Supported values: [PublicInternetOnly, VpcOnly]', '/Resources/Domain/Properties/AppNetworkAccessType')]

The preceding assertion in the same check — that the stack status is ROLLBACK_COMPLETE — passed, and the following assertion that the domain is absent from ListDomains was never reached; so the refusal and rollback are right and only the operation event is missing.

## Steps to Reproduce
1. Create a VPC, a subnet, a security group and an execution role.
2. CreateStack with an AWS::SageMaker::Domain whose Properties carry DomainName, AuthMode=IAM, VpcId, SubnetIds, DefaultUserSettings.ExecutionRole and AppNetworkAccessType set to a value outside the resource's enum (e.g. "BadAccessType").
3. Poll DescribeStacks until terminal — the status is ROLLBACK_COMPLETE, as AWS does.
4. Call cloudformation:DescribeEvents(StackName=<stack id>), page all OperationEvents and keep those with EventType == 'VALIDATION_ERROR'.
5. Observe no such event. On AWS there is exactly one, with ValidationName=PROPERTY_VALIDATION, ValidationStatus=FAILED, ValidationFailureMode=FAIL, ValidationPath=/Resources/Domain/Properties/AppNetworkAccessType and ValidationStatusReason naming the rejected value and the supported values.
