Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__ec2__ssm.cfn-ssm-document-wrong-instance-tag-key-negative cloudformation__ec2__ssm.cfn-ssm-patch-baseline-wrong-tag-filter-negative
Issue-Key: cloudformation__ec2__ssm.cfn-ssm-document-wrong-instance-tag-key-negative|run#18|AssertionError|assert instance_id in resolved_ids, (
Issue-Key: cloudformation__ec2__ssm.cfn-ssm-patch-baseline-wrong-tag-filter-negative|<lambda>#12|KeyError|lambda e: e is not None and e["ResourceCountByStatus"] != "{}",
Issue-Commit: 6df92bd208b59385c9370bd54f69012d5d2b8998

# cloudformation__ec2__ssm: 2 net-new causes — a successful tag-targeted association execution reports zero AssociationExecutionTargets, and AssociationExecution records omit ResourceCountByStatus

> _Note: this issue was identified against the code base at commit `6df92bd208b59385c9370bd54f69012d5d2b8998`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeAssociationExecutionTargets

## Description
Two net-new root causes in the SSM State Manager association-execution read surface, both observed against associations created by CloudFormation (AWS::SSM::Association with a `tag:<key>` target over an EC2 instance that carries that tag).

=== Cause 1 of 2: DescribeAssociationExecutionTargets returns an empty list for an execution that ran on a resolved target ===

AWS: Smithy com.amazonaws.ssm#DescribeAssociationExecutionTargets takes AssociationId (required) and ExecutionId (required) and returns AssociationExecutionTargets (ssm#AssociationExecutionTargetsList of ssm#AssociationExecutionTarget). AssociationExecutionTarget declares AssociationId, AssociationVersion, ExecutionId, ResourceId ("The resource ID, for example, the managed node ID where the association ran"), ResourceType ("The resource type, for example, EC2"), Status, DetailedStatus, LastExecutionDate and OutputSource. AWS emits ONE element per target the named execution actually resolved: an association whose Targets are [{Key: "tag:<key>", Values: ["<value>"]}] and whose tag matches a managed node yields an element whose ResourceId is that node's instance id. The modeled refusals are AssociationDoesNotExist / AssociationExecutionDoesNotExist (400 class); a known association+execution that resolved targets is never answered with an empty list. The empty list is AWS's answer only for an execution that resolved no target at all — which is exactly the distinction this check rests on.

Emulator: for the positive-control association (the tag KEY the instance really carries) the emulator does get that far — DescribeAssociationExecutions returns at least one AssociationExecution and DescribeAssociation reports Overview.Status "Success" — but DescribeAssociationExecutionTargets(AssociationId=<control id>, ExecutionId=<that execution>) answers HTTP 200 with AssociationExecutionTargets = [] . No per-target record exists for the instance the association matched, so the resolved-target set is empty for a Success execution.

Why: AssociationExecutionTargets is the only per-resource view of a State Manager run. With it always empty, a client cannot tell "the association ran on my fleet" from "the association matched nothing" — the two cases AWS distinguishes by exactly this list — and every per-target Status/DetailedStatus/OutputSource that drives association troubleshooting is lost. Negative-control reasoning inverts: an association deliberately pointed at a tag key no instance carries is indistinguishable from one pointed at a real tag.

Fix: the ssm service's State Manager association-execution state and the DescribeAssociationExecutionTargets handler/projection: when an association execution resolves targets, record one execution-target row per resolved target (ResourceId = the managed node id, ResourceType, Status/DetailedStatus, LastExecutionDate) and project those rows, instead of returning an unconditionally empty list.

Traces: cloudformation__ec2__ssm.cfn-ssm-document-wrong-instance-tag-key-negative.

Related but NOT covering it: open issue 20261114T135329Z-cloudformation-ec2-ssm-6-net-new-causes-ssm.md Cause 2 describes associations that never execute at all and DescribeAssociationExecutions recording nothing. Here an execution record IS returned and reports Success; only the per-target list is empty, and the replay report does not mark this check as owned by that issue.

=== Cause 2 of 2: AssociationExecution records carry no ResourceCountByStatus member ===

AWS: Smithy com.amazonaws.ssm#AssociationExecution declares the member ResourceCountByStatus (target ssm#ResourceCountByStatus, string), documented as "An aggregate status of the resources in the execution based on the status type", on every element of DescribeAssociationExecutionsResult.AssociationExecutions. AWS always populates it as a brace-wrapped per-status aggregate string; the brace-wrapped form is the requirement, the statuses and counts inside come from the execution's resolved target set, and an execution that resolved no target carries the empty aggregate "{}".

Emulator: DescribeAssociationExecutions(AssociationId=<id>) for a CloudFormation-created AWS::SSM::Association returns a non-empty AssociationExecutions list whose elements have no ResourceCountByStatus key at all — reading the modeled member raises KeyError: 'ResourceCountByStatus'.

Why: ResourceCountByStatus is the aggregate a caller uses to decide whether a patch-baseline/RunPatchBaseline association resolved any instance. Omitting it makes "matched the Patch Group" and "matched nothing" indistinguishable and crashes any SDK-typed consumer that indexes a member the API guarantees.

Fix: the DescribeAssociationExecutions handler in the ssm service (the AssociationExecution projection that builds each list element) must populate ResourceCountByStatus from the execution's per-status resolved-target counts, emitting the empty aggregate when nothing was resolved.

Traces: cloudformation__ec2__ssm.cfn-ssm-patch-baseline-wrong-tag-filter-negative.

Already open for the SAME root cause under a different replay key: 20261117T084005Z-cloudformation-ec2-ssm-ssm-describeassociationexecutions-returns-associationexecution-records.md (grounded in cloudformation__ec2__ssm.cfn-change-set-preview-ec2-ssm-association-update-positive). The replay report does not mark this check as owned by it, so the check is recorded here; fixing that issue should close this cause too, and this check is the second replay key to re-run.

## Evidence
Cause 1 — failing check cloudformation__ec2__ssm.cfn-ssm-document-wrong-instance-tag-key-negative [mismatch]:

  File "check_cloudformation__ec2__ssm_cfn_ssm_document_wrong_instance_tag_key_negative.py", line 75, in run
    assert instance_id in resolved_ids, (
  AssertionError: positive control association never resolved the instance as an execution target: targets=[]

The assertion is reached only after ssm.describe_association(AssociationId=<control>)["AssociationDescription"]["Overview"]["Status"] == "Success" and after ssm.describe_association_executions(AssociationId=<control>)["AssociationExecutions"] came back non-empty, so the emulator did report a Success execution for the association.
Expected (AWS): ssm.describe_association_execution_targets(AssociationId=<control>, ExecutionId=<latest execution>)["AssociationExecutionTargets"] contains an element whose ResourceId is the tagged instance's id (one element per resolved target).
Actual (emulator): the same call returns AssociationExecutionTargets == [] (HTTP 200, empty list).

Cause 2 — failing check cloudformation__ec2__ssm.cfn-ssm-patch-baseline-wrong-tag-filter-negative [error]:

  File "check_cloudformation__ec2__ssm_cfn_ssm_patch_baseline_wrong_tag_filter_negative.py", line 162, in <lambda>
    lambda e: e is not None and e["ResourceCountByStatus"] != "{}",
  KeyError: 'ResourceCountByStatus'

The predicate short-circuits on `e is not None`, so the KeyError proves the emulator returned an AssociationExecution record that simply lacks the modeled member.
Expected (AWS): each element of DescribeAssociationExecutionsResult.AssociationExecutions carries ResourceCountByStatus as a brace-wrapped per-status aggregate string ("{}" only when no target was resolved).
Actual (emulator): the element has no ResourceCountByStatus key.

## Steps to Reproduce
Cause 1:
1. Clone the repo and run the behavioral suite so the emulator is serving.
2. CreateStack with an IAM role + instance profile carrying AmazonSSMManagedInstanceCore, one AWS::EC2::Instance tagged with a real key (e.g. Sim2realRealKey=<value>), and two AWS::SSM::Association resources running AWS-RunShellScript: one targeting [{Key: "tag:Sim2realRealKey", Values: [<value>]}] (control) and one targeting a tag key no instance carries. Wait for CREATE_COMPLETE and read both association ids from the Outputs.
3. Poll ssm.describe_instance_information until the instance is PingStatus Online, then poll ssm.describe_association(AssociationId=<control>) until Overview.Status is Success.
4. Call ssm.describe_association_executions(AssociationId=<control>), take the last ExecutionId, and call ssm.describe_association_execution_targets(AssociationId=<control>, ExecutionId=<that id>).
5. Emulator: AssociationExecutionTargets is []. AWS: it holds one AssociationExecutionTarget whose ResourceId is the tagged instance id.

Cause 2:
1. With the emulator serving, CreateStack with an EC2 instance tagged "Patch Group"=<value>, an AWS::SSM::PatchBaseline for AMAZON_LINUX_2023 with PatchGroups [<value>], and an AWS::SSM::Association running AWS-RunPatchBaseline with Targets [{Key: "tag:Patch Group", Values: [<value>]}]. Wait for CREATE_COMPLETE and read the association id.
2. Wait until DescribeInstanceInformation reports the instance Online, then call ssm.describe_association_executions(AssociationId=<association id>).
3. Read ResourceCountByStatus off the first element. Emulator: KeyError — the member is absent. AWS: a brace-wrapped per-status aggregate string.
