Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__ec2__ssm.cfn-deploy-ssm-maintenance-window-targets-ec2-positive cloudformation__ec2__ssm.cfn-ssm-maintenance-window-missing-service-role-negative cloudformation__ec2__ssm.cfn-ssm-maintenance-window-registers-ec2-target-positive
Issue-Key: cloudformation__ec2__ssm|CreateStack|ValidationError
Issue-Key: cloudformation__ec2__ssm.cfn-ssm-maintenance-window-registers-ec2-target-positive|run#17|AssertionError|assert resolved_ids == [instance_id], (
Issue-Commit: d3cdd55e8b72332e5c71fd34997111c995fed5da

# cloudformation__ec2__ssm: 2 net-new causes — AWS::SSM::MaintenanceWindowTask is an unrecognized resource type, and SSM DescribeInstanceInformation ignores tag:<key> filters

> _Note: this issue was identified against the code base at commit `d3cdd55e8b72332e5c71fd34997111c995fed5da`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
Two net-new root causes. The resolved issue 20261107T075601Z-cloudformation-ec2-ssm-cloudformation-ssm-aws-ssm-maintenancewindowtarget-unrecognised-ec2-ref.md registered AWS::SSM::MaintenanceWindowTarget and made nodes register; neither cause below is a refusal of anything it claimed to fix — the sibling TASK type is still unregistered and the tag-filter read now answers 200 with an empty list.

Cause 1 of 2: CreateStack refuses a template declaring AWS::SSM::MaintenanceWindowTask as an unknown resource type.
AWS: the type is supported and maps onto ssm RegisterTaskWithMaintenanceWindow; a stack declaring an AWS::SSM::MaintenanceWindow plus an AWS::SSM::MaintenanceWindowTarget and an AWS::SSM::MaintenanceWindowTask creates successfully, and a task whose ServiceRoleArn is missing/invalid is what the negative scenario then exercises (recorded behavior only for the handler; the underlying operation is ssm RegisterTaskWithMaintenanceWindow).
Emulator: CreateStack is refused HTTP 400 ValidationError with message 'Template format error: Unrecognized resource types: [AWS::SSM::MaintenanceWindowTask]' (fixed text 'Template format error: Unrecognized resource types: [' + the unrecognized type list + ']').
Why: no maintenance-window template that registers a task can be deployed at all — the stack never exists, so neither the positive target/task registration nor the missing-service-role refusal can be observed.
Fix: the CloudFormation resource-type registry and provisioner — register AWS::SSM::MaintenanceWindowTask and wire its handler to ssm RegisterTaskWithMaintenanceWindow (note the related open scaffold issue 20261111T143427Z-ssm-registertaskwithmaintenancewindow-stub.md for the SSM operation itself).
Failing checks: cloudformation__ec2__ssm.cfn-deploy-ssm-maintenance-window-targets-ec2-positive, cloudformation__ec2__ssm.cfn-ssm-maintenance-window-missing-service-role-negative.

Cause 2 of 2: SSM DescribeInstanceInformation ignores a 'tag:<key>' filter, so a tagged managed node is never resolved by its tags.
AWS: Smithy — DescribeInstanceInformation.Filters is a list of InstanceInformationStringFilter, and the model documents the valid filter keys for InstanceInformationStringFilter$Key as 'ActivationIds | AgentVersion | AssociationStatus | IamRole | InstanceIds | PingStatus | PlatformTypes | ResourceType | SourceIds | SourceTypes | "tag-key" | "tag:{keyname}"', with the example Key=tag:Purpose,Values=Test. Filtering with Key='tag:<K>', Values=['<V>'] therefore returns exactly the managed nodes carrying tag K=V.
Emulator: the same instance IS a managed node — the preceding poll with Filters=[{'Key':'InstanceIds','Values':[<id>]}] returns it with PingStatus 'Online', and DescribeMaintenanceWindowTargets stores the target as [{'Key':'tag:<K>','Values':['<V>']}] — but DescribeInstanceInformation with Filters=[{'Key':'tag:<K>','Values':['<V>']}] answers 200 with an EMPTY InstanceInformationList.
Why: tag-based targeting is the normal way Run Command, maintenance windows and State Manager select nodes. A client resolving a tag filter gets an empty fleet and cannot tell 'no nodes match' from 'tag filters are not implemented'; a maintenance window whose target is a tag filter therefore matches nothing.
Fix: the DescribeInstanceInformation handler's filter evaluation in the SSM service — support the 'tag:{keyname}' and 'tag-key' filter keys against the managed node's tags.
Failing check: cloudformation__ec2__ssm.cfn-ssm-maintenance-window-registers-ec2-target-positive.

## Evidence
Cause 1 — expected CreateStack to accept the template and the stack to reach CREATE_COMPLETE.
Actual (cfn-deploy-ssm-maintenance-window-targets-ec2-positive and cfn-ssm-maintenance-window-missing-service-role-negative): botocore.exceptions.ClientError: An error occurred (ValidationError) when calling the CreateStack operation: Template format error: Unrecognized resource types: [AWS::SSM::MaintenanceWindowTask]

Cause 2 — expected ssm.describe_instance_information(Filters=[{'Key':'tag:Sim2realMwTarget','Values':[<value>]}])['InstanceInformationList'] to contain exactly the one tagged instance.
Actual (cfn-ssm-maintenance-window-registers-ec2-target-positive): AssertionError: tag:Sim2realMwTarget=<value> should resolve to exactly [<instance id>] via describe_instance_information, got [] — while the same instance is returned by the InstanceIds filter with PingStatus 'Online', and DescribeMaintenanceWindowTargets already stores Targets=[{'Key':'tag:Sim2realMwTarget','Values':[<value>]}].

## Steps to Reproduce
1. Replay the behavioral suite and re-run the three checks in trace_ids.
2. Cause 1: CreateStack a template containing AWS::SSM::MaintenanceWindow + AWS::SSM::MaintenanceWindowTarget + AWS::SSM::MaintenanceWindowTask (and, for the negative scenario, a task whose ServiceRoleArn names a role that cannot be assumed).
3. Cause 2: deploy an EC2 instance with an SSM-core instance profile and a tag K=V, wait until ssm.describe_instance_information(Filters=[{'Key':'InstanceIds','Values':[<id>]}]) reports PingStatus 'Online', then call ssm.describe_instance_information(Filters=[{'Key':'tag:K','Values':['V']}]).
