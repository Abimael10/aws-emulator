Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__ec2__iam__sagemaker.deploy-sagemaker-domain-app-network-access-type-invalid-value-rejected
Issue-Key: cloudformation__ec2__iam__sagemaker.deploy-sagemaker-domain-app-network-access-type-invalid-value-rejected|run#11|AssertionError|assert bad_status == "ROLLBACK_COMPLETE", f"rejected stack ended {bad_status!r}"
Issue-Commit: 7dc215237948fb0f525a17841803e21ef74f8c7d

# CloudFormation accepts an off-enum AWS::SageMaker::Domain AppNetworkAccessType: the stack reaches CREATE_COMPLETE with no PROPERTY_VALIDATION event instead of rolling back

> _Note: this issue was identified against the code base at commit `7dc215237948fb0f525a17841803e21ef74f8c7d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
One net-new behavioral root cause under this service heading. The failing check is marked neither ALREADY FILED nor OWNED by the replay.

Cause 1 of 1: a stack whose AWS::SageMaker::Domain declares an AppNetworkAccessType outside the modeled enum is provisioned and reported CREATE_COMPLETE, and CloudFormation emits no property-validation event for it.

AWS: Smithy (services/service-com-amazonaws-sagemaker-sagemaker/smithy-model/com-amazonaws-sagemaker-sagemaker.json): CreateDomain/DescribeDomain member AppNetworkAccessType targets enum com.amazonaws.sagemaker#AppNetworkAccessType, whose only values are PublicInternetOnly and VpcOnly. A value outside that set is refused, and through CloudFormation that refusal surfaces as early property validation: DescribeEvents returns an OperationEvents row with EventType VALIDATION_ERROR carrying LogicalResourceId of the offending resource, ValidationName "PROPERTY_VALIDATION", ValidationStatus "FAILED", ValidationFailureMode "FAIL", ValidationPath "/Resources/<LogicalId>/Properties/AppNetworkAccessType" and ValidationStatusReason "<value> is not a valid enum value. Supported values: [PublicInternetOnly, VpcOnly]"; the stack then reaches terminal StackStatus "ROLLBACK_COMPLETE" and no domain of that name exists in SageMaker afterwards (ListDomains returns no entry for it). In that reason message the fixed text is " is not a valid enum value. Supported values: [PublicInternetOnly, VpcOnly]" — the bracketed set is the enum's declared value list, a service fact, not a per-run value — and the leading token is the rejected value taken verbatim from the submitted template (the recording used "BadAccessType"). The check's positive control is the same template with AppNetworkAccessType "VpcOnly" plus a DomainSettings, which does deploy to CREATE_COMPLETE and reads back through DescribeDomain.

Emulator: CreateStack is accepted and DescribeStacks(StackName=<stack>)["Stacks"][0]["StackStatus"] polls to terminal "CREATE_COMPLETE" for the off-enum template. The out-of-enum AppNetworkAccessType is rejected neither by the AWS::SageMaker::Domain provisioner nor by CreateDomain behind it, so no CREATE_FAILED resource event and no VALIDATION_ERROR OperationEvents row is produced, no rollback is driven, and an invalid domain is left provisioned.

Why: a client validating templates against the emulator gets a green CREATE_COMPLETE for a Studio Domain configuration real SageMaker refuses, so the misconfiguration is discovered only against AWS; and because the refusal never happens, CloudFormation's all-or-nothing rollback never runs, leaving the caller a half-valid domain (and whatever the stack created around it) with no CREATE_FAILED event, no ResourceStatusReason and no VALIDATION_ERROR row to explain it.

Fix: enum validation of AppNetworkAccessType in the CreateDomain handler's input validation (so every caller, not only CloudFormation, is refused), plus the CloudFormation property-validation pass that must emit the VALIDATION_ERROR OperationEvents row and the stack resource lifecycle that must turn the refusal into a CREATE_FAILED resource event and drive the stack to ROLLBACK_COMPLETE.

Related but distinct open issues, none of which carry this check's replay key: 20261109T134927Z-cloudformation-ec2-iam-sagemaker-cloudformation-sagemaker-domain-space-stacks-2-net-new.md covers the same failure class for the Domain's AuthMode member only; 20261116T061927Z-cloudformation-securityhub-cloudformation-accepts-an-off-enum-aws-securityhub-hub.md covers an off-enum AWS::SecurityHub::Hub property; 20261115T190121Z-appsync-cloudformation-a-property-validation-failure-invalid-aws.md covers the missing VALIDATION_ERROR OperationEvent for an AWS::AppSync::SourceApiAssociation property.

Traces: cloudformation__ec2__iam__sagemaker.deploy-sagemaker-domain-app-network-access-type-invalid-value-rejected.

## Evidence
Cause 1 — cloudformation__ec2__iam__sagemaker.deploy-sagemaker-domain-app-network-access-type-invalid-value-rejected [mismatch]:

  File "check_cloudformation__ec2__iam__sagemaker_deploy_sagemaker_domain_app_network_access_type_invalid_value_rejected.py", line 402, in run
    assert bad_status == "ROLLBACK_COMPLETE", f"rejected stack ended {bad_status!r}"
  AssertionError: rejected stack ended 'CREATE_COMPLETE'

Expected: DescribeStacks(StackName=<the off-enum stack>)["Stacks"][0]["StackStatus"] == "ROLLBACK_COMPLETE"; DescribeEvents(StackName=<stack>) yields exactly one EventType=="VALIDATION_ERROR" OperationEvents row equal to
  ("Domain", "PROPERTY_VALIDATION", "FAILED", "FAIL",
   "BadAccessType is not a valid enum value. Supported values: [PublicInternetOnly, VpcOnly]",
   "/Resources/Domain/Properties/AppNetworkAccessType")
and ListDomains contains no domain with the template's DomainName.

Actual: the stack polls to terminal StackStatus "CREATE_COMPLETE"; the run never reaches the event/absence assertions because the status assertion fails first.

In the quoted reason, "BadAccessType" is the rejected value echoed from the request and the logical id "Domain" is the template's own logical id — both template-supplied. "[PublicInternetOnly, VpcOnly]" is the modeled enum value set and is fixed. Stack name/id, VPC and subnet ids, security-group id, role ARN and the generated domain name in the recording are per-run values and are not part of the requirement.

## Steps to Reproduce
1. Build and boot the emulator and re-replay the check in trace_ids.
2. Create a disposable VPC + subnet, a security group in that VPC, and an IAM role trusted by sagemaker.amazonaws.com.
3. CreateStack with a template whose single resource is AWS::SageMaker::Domain with Properties: DomainName=<name>, AuthMode="IAM", VpcId=<vpc>, SubnetIds=[<subnet>], HomeEfsFileSystemCreation="Disabled", DefaultUserSettings.ExecutionRole=<role arn>, DomainSettings={SecurityGroupIds:[<sg>], ExecutionRoleIdentityConfig:"USER_PROFILE_NAME"}, and AppNetworkAccessType set to a value outside {PublicInternetOnly, VpcOnly} (e.g. "BadAccessType").
4. Poll DescribeStacks(StackName=<StackId>) to a terminal status. AWS reports "ROLLBACK_COMPLETE"; the emulator reports "CREATE_COMPLETE".
5. Call DescribeEvents(StackName=<StackId>) and keep the OperationEvents rows with EventType "VALIDATION_ERROR". AWS returns the single PROPERTY_VALIDATION/FAILED/FAIL row at path /Resources/Domain/Properties/AppNetworkAccessType whose ValidationStatusReason is "<rejected value> is not a valid enum value. Supported values: [PublicInternetOnly, VpcOnly]"; the emulator returns none.
6. Paginate ListDomains: AWS lists no domain with the template's DomainName; the emulator lists the domain it provisioned.
7. Call CreateDomain directly with the same out-of-enum AppNetworkAccessType: AWS refuses it, the emulator accepts it.
8. Positive control: the same template with AppNetworkAccessType="VpcOnly" must still deploy to CREATE_COMPLETE, and DescribeDomain must read back AppNetworkAccessType "VpcOnly", DomainSettings.SecurityGroupIds [<sg>] and DomainSettings.ExecutionRoleIdentityConfig "USER_PROFILE_NAME".
