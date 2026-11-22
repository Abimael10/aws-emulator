Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__ssm-quicksetup.deploy-ssmquicksetup-configuration-manager-host-mgmt-round-trip-and-delete
Issue-Key: cloudformation__ssm-quicksetup.deploy-ssmquicksetup-configuration-manager-host-mgmt-round-trip-and-delete|run#30|AssertionError|assert deployed_sets, f"no ACTIVE stack set is administered by {admin_role_name!r}"
Issue-Commit: 68c71f6585a425304635c9d8c1cc1d9408beb2ea

# cloudformation__ssm-quicksetup: a Quick Setup configuration manager's deployment reports SUCCEEDED while creating no CloudFormation stack set, so nothing the configuration deploys exists

> _Note: this issue was identified against the code base at commit `68c71f6585a425304635c9d8c1cc1d9408beb2ea`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetConfigurationManager

## Description
Cause 1 of 1: the ssm-quicksetup configuration-deployment engine walks a status machine only — it never creates the CloudFormation stack set that implements the deployment, yet it reports the deployment's terminal status as SUCCEEDED.

AWS: Quick Setup implements a configuration definition's deployment through CloudFormation StackSets. For a LOCAL deployment the stack set is administered by the role the definition names, and executed by the per-account role it names (Smithy, com.amazonaws.ssmquicksetup#ConfigurationDefinition: `LocalDeploymentAdministrationRoleArn` — "The ARN of the IAM role used to administrate local configuration deployments" (target IAMRoleArn); `LocalDeploymentExecutionRoleName` @pattern ^[\w+=,.@-]{1,64}$ — "The name of the IAM role used to deploy local configurations"). The model declares no stack-set member, so for the observable consequence this is recorded behavior only: once GetConfigurationManager's `Deployment` StatusSummary reaches `SUCCEEDED` for an `AWSQuickSetupType-SSMHostMgmt` definition, CloudFormation ListStackSets(Status=ACTIVE) reports at least one stack set whose DescribeStackSet `StackSet.AdministrationRoleARN` equals the definition's `LocalDeploymentAdministrationRoleArn`, and those stack sets are gone again after the manager is deleted. That is the only way a caller can see that a SUCCEEDED Quick Setup deployment actually deployed anything; the particular stack-set names, role names and the IAM role ARN's account/partition are values taken from the request and the account, never literals.

Emulator: the deployment's `Deployment` StatusSummary does reach `SUCCEEDED` (the check's poll converges), but CloudFormation knows nothing about it: ListStackSets(Status=ACTIVE) reports no stack set whose AdministrationRoleARN is the templated admin role ARN, so the check fails `AssertionError: no ACTIVE stack set is administered by 'sim2real-qscfn-admin-<id>'`. The ssm-quicksetup crate's deployment engine (services/service-com-amazonaws-ssmquicksetup-quicksetup/src/state.rs) models a deployment purely as per-target `ConfigurationRecord`s plus `StatusSummaryRecord`s advanced on the clock through `DEPLOYMENT_PHASES = [INITIALIZING, DEPLOYING, SUCCEEDED]`; the whole crate contains exactly one mention of CloudFormation (a doc comment about CreatedAt precision at state.rs:2509) and no stack-set creation, so no cross-service effect is ever produced. Correspondingly, no teardown of such stack sets can happen when the manager is deleted.

Why: SUCCEEDED is the signal a client waits for before using what the configuration deployed. A client that creates a Quick Setup configuration manager (directly or via AWS::SSMQuickSetup::ConfigurationManager) and then inspects, operates on, or tears down the CloudFormation stack sets Quick Setup created — the documented way to see which accounts/Regions a configuration reached and whether a target failed — finds nothing at all. Deployment success is therefore unfalsifiable in the emulator: a configuration that deployed correctly and one that deployed nothing are indistinguishable, and any orchestration that reconciles Quick Setup against the stack sets it owns (or that asserts no stack set leaks after the manager is deleted) can never pass.

Fix: the configuration-deployment engine of the ssm-quicksetup service crate (services/service-com-amazonaws-ssmquicksetup-quicksetup/src/state.rs — `deploy_definition`, `fresh_summaries`/`StatusSummaryRecord` phase walking, and the manager-teardown path `deletes_at`/`settle`), plus the cross-service wiring into the CloudFormation emulator's stack-set state: a definition that is deployed must create the stack set(s) that back it, with AdministrationRoleARN set from the definition's LocalDeploymentAdministrationRoleArn and the execution role name from LocalDeploymentExecutionRoleName, ACTIVE while the manager's deployment is SUCCEEDED, and removed when the manager is deleted.

Traces: behavioral check cloudformation__ssm-quicksetup.deploy-ssmquicksetup-configuration-manager-host-mgmt-round-trip-and-delete.

Not covered by anything already filed: open issue 20261110T211706Z-ssm-quicksetup-configuration-deployment-responses-diverge-minted-ids.md is about the SHAPE of the deployment response (minted ids, StatusDetails, AsyncExecutions, post-delete refusal class) and says nothing about stack sets; open issue 20261109T142303Z-cloudformation-ssm-5-net-new-causes-no-resource.md is about unrecognized AWS::SSM* resource types, and here the stack and the AWS::SSMQuickSetup::ConfigurationManager resource DID create successfully.

## Evidence
Behavioral suite (simparity-behavioral-aws-v1) against the emulator built from this checkout: 22/23 passed; the single failure is check cloudformation__ssm-quicksetup.deploy-ssmquicksetup-configuration-manager-host-mgmt-round-trip-and-delete.

The check gets all the way through the CloudFormation create and the ssm-quicksetup read-back: the stack reaches CREATE_COMPLETE, ListConfigurationManagers/GetConfigurationManager agree with the template (ManagerArn == the resource's Ref, Name, Description, CreatedAt, ConfigurationDefinitions, tags), and _wait_manager_deployment_terminal(qs, manager_arn) observes the `Deployment` StatusSummary reach 'SUCCEEDED'.

It then fails at the very next assertion:

    deployed_sets = _active_stack_sets_of(cfn, admin_arn)
    assert deployed_sets, f"no ACTIVE stack set is administered by {admin_role_name!r}"
    AssertionError: no ACTIVE stack set is administered by 'sim2real-qscfn-admin-ddf2eaa0a84d'

where _active_stack_sets_of paginates CloudFormation ListStackSets(Status='ACTIVE') and keeps the names whose DescribeStackSet(StackSetName=n)['StackSet']['AdministrationRoleARN'] equals arn:<partition>:iam::<account>:role/<admin role name from the template>. Expected: a non-empty list of ACTIVE stack sets administered by that role (Quick Setup's own). Actual: the empty list — CloudFormation holds no stack set for the deployment at all. (The role name in the message is a per-run generated value from ctx.unique_name and is not part of the requirement; the requirement is that at least one ACTIVE stack set is administered by whatever role ARN the definition named.)

The check's remaining steps (delete the stack, then assert GetConfigurationManager answers ResourceNotFoundException and that Quick Setup's stack sets go ACTIVE->gone within 300 s) are never reached, so the teardown side of the same cause is unexercised.

Model (smithy_query, services/service-com-amazonaws-ssmquicksetup-quicksetup/smithy-model/com-amazonaws-ssmquicksetup-quicksetup.json): ConfigurationDefinition { Type required @pattern ^[a-zA-Z0-9_\-.:/]{3,200}$, Parameters required, TypeVersion, LocalDeploymentExecutionRoleName @pattern ^[\w+=,.@-]{1,64}$ "The name of the IAM role used to deploy local configurations", LocalDeploymentAdministrationRoleArn (IAMRoleArn) "The ARN of the IAM role used to administrate local configuration deployments", Id }.

Code: services/service-com-amazonaws-ssmquicksetup-quicksetup/src/state.rs — DEPLOYMENT_PHASES/ASYNC_EXECUTION_PHASES + TRANSITION_SPAN drive the whole deployment lifecycle on the clock; a search of the crate for 'stack'/'StackSet'/'cloudformation' yields a single hit, the CreatedAt-precision doc comment at state.rs:2509. Nothing in the crate creates, updates or deletes a CloudFormation stack set.

## Steps to Reproduce
1. Clone this checkout, build and boot the emulator, then replay the behavioral suite; check cloudformation__ssm-quicksetup.deploy-ssmquicksetup-configuration-manager-host-mgmt-round-trip-and-delete fails.
2. Manually: with signed credentials, CloudFormation CreateStack (Capabilities=['CAPABILITY_NAMED_IAM']) for a template that creates two named IAM roles (an admin role assumable by cloudformation.amazonaws.com and ssm-quicksetup.amazonaws.com, and an execution role assumable by it) plus one AWS::SSMQuickSetup::ConfigurationManager whose single ConfigurationDefinitions entry has Type 'AWSQuickSetupType-SSMHostMgmt', Parameters TargetType=Tags / TargetTagKey / TargetTagValue / TargetAccounts={Ref AWS::AccountId} / TargetRegions={Ref AWS::Region} (plus the type's remaining host-management parameters), LocalDeploymentAdministrationRoleArn = the admin role's Arn and LocalDeploymentExecutionRoleName = the execution role. Wait for CREATE_COMPLETE.
3. Take the manager ARN from the stack's Ref output and poll ssm-quicksetup GetConfigurationManager(ManagerArn=...) until the StatusSummaries entry with StatusType 'Deployment' reports Status 'SUCCEEDED'.
4. Call CloudFormation ListStackSets(Status='ACTIVE') and, for each summary, DescribeStackSet(StackSetName=...). Observe that NO stack set reports StackSet.AdministrationRoleARN == the admin role ARN the definition named (the ACTIVE list holds no Quick Setup stack set at all), where AWS reports at least one.
5. Then DeleteStack and poll ListStackSets(Status='ACTIVE')/DescribeStackSet again: AWS removes the Quick Setup stack sets administered by that role along with the manager, which cannot be observed while step 4 creates none.
