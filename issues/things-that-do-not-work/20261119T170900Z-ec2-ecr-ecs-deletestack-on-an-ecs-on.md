Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2__ecr__ecs.e2e-ecsnrt-diagnose-ecs-service-no-running-tasks-lifecycle
Issue-Key: ec2__ecr__ecs.e2e-ecsnrt-diagnose-ecs-service-no-running-tasks-lifecycle|phase#12|AssertionError|raise AssertionError(f"[phase {i}/{len(self.phases)} {name}] {exc}") from exc
Issue-Commit: 23a6305fb4194ac652f562ce813f73dafa581013

# ec2__ecr__ecs: DeleteStack on an ECS-on-EC2 capacity-provider estate never reaches DELETE_COMPLETE — the stack never becomes gone and DescribeStacks keeps answering

> _Note: this issue was identified against the code base at commit `23a6305fb4194ac652f562ce813f73dafa581013`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteStack

## Description
Cause 1 of 1: DeleteStack on a CloudFormation stack that owns an ECS-on-EC2 estate (ECS cluster + capacity provider + Auto Scaling group + ECS service + task definition on top of a VPC) never converges: the stack neither disappears nor reports DELETE_COMPLETE, so the teardown of the estate never finishes.

AWS:      Smithy (services/service-com-amazonaws-cloudformation-cloudformation/smithy-model/com-amazonaws-cloudformation-cloudformation.json) declares DeleteStack(StackName @required, RetainResources, RoleARN, ClientRequestToken, DeletionMode, DeploymentConfig) -> Unit, with TokenAlreadyExistsException its only modelled error; DescribeStacks(StackName, NextToken) -> Stacks with no modelled errors (the not-found refusal is the ec2Query/awsQuery-style ValidationError the recording shows). The convergence contract is recorded behavior only: after DeleteStack, AWS deletes the stack's resources in reverse dependency order and the stack reaches DELETE_COMPLETE, after which DescribeStacks(StackName=<name>) answers HTTP 400 Code `ValidationError` with Message `Stack with id <StackName> does not exist` — fixed text `Stack with id ` and ` does not exist`, with the stack name taken from the request. A second DeleteStack on the same name is a clean no-op. This holds for this estate's resource mix: an ECS service whose desiredCount is 0, its task definition (which a stack delete DEREGISTERS to INACTIVE rather than removing), an ECS capacity provider bound to an Auto Scaling group, the Auto Scaling group itself, and the VPC/subnet/security-group net underneath.

Emulator: after DeleteStack the stack never reaches a deleted state. The check polls DescribeStacks(StackName=<stack>) every 10 s for 1200 s and accepts either the stack being gone (DescribeStacks refusing with `... does not exist`) or StackStatus `DELETE_FAILED`; neither is reached inside that budget, so the stack is left either permanently in DELETE_IN_PROGRESS or in some other non-terminal status while DescribeStacks keeps answering 200 with the stack present. Everything before the teardown passed — the stack reached CREATE_COMPLETE with all its outputs, the ECR seeding was idempotent, the full diagnosis trajectory (DescribeServices -> DescribeTaskDefinition -> ListImages -> ListContainerInstances) derived exactly the deployed fault, and the repository cleanup verified RepositoryNotFoundException — so the defect is confined to the delete path.

Why:      A client cannot tear the estate down. Every consumer of CloudFormation treats "stack reached DELETE_COMPLETE / is gone" as the end of teardown: the `stack_delete_complete` waiter, `cdk destroy`, `terraform destroy` over a CFN stack, and CI jobs that delete their stack between runs all hang until their own timeout and then report a failed destroy. Because the stack never releases its resources, the ECS cluster, capacity provider, Auto Scaling group and the VPC net stay live and leak into the account; a redeploy of the same template under the same name then collides with the residue instead of starting clean, and no follow-on assertion (task definition reading INACTIVE, a second delete being a no-op) can ever be reached.

Fix:      The DeleteStack delete-progression in the CloudFormation stack engine: the per-resource delete of this estate's resource types (AWS::ECS::Service, AWS::ECS::TaskDefinition, AWS::ECS::CapacityProvider, AWS::AutoScaling::AutoScalingGroup and the VPC resources beneath them) must reach a terminal verdict for every resource and then drive the stack to DELETE_COMPLETE (or to DELETE_FAILED naming the resource that blocked), rather than leaving the stack in a non-terminal status forever. A resource whose delete cannot complete must surface as DELETE_FAILED with the resource's reason, not as an unbounded wait.

Traces:   ec2__ecr__ecs.e2e-ecsnrt-diagnose-ecs-service-no-running-tasks-lifecycle (phase 5/5 `destroy`).

## Evidence
Failing check `ec2__ecr__ecs.e2e-ecsnrt-diagnose-ecs-service-no-running-tasks-lifecycle` (verdict: mismatch).

Replay output:
  File "check_ec2__ecr__ecs_e2e_ecsnrt_diagnose_ecs_service_no_running_tasks_lifecycle.py", line 269, in phase
    raise AssertionError(f"[phase {i}/{len(self.phases)} {name}] {exc}") from exc
  AssertionError: [phase 5/5 destroy] stack deletion did not reach DELETE_COMPLETE

Phases 1-4 (deploy, setup, solve, cleanup) all passed: the stack reached CREATE_COMPLETE and produced AppClusterName, ServiceName, TaskDefinitionArn, AutoScalingGroupName and CapacityProviderName; DescribeClusters reported the capacity provider attached with an empty default strategy and 0 registered container instances; the frozen diagnosis trajectory derived the deployed fault exactly; and the ECR repository delete was verified with RepositoryNotFoundException.

Expected (recorded AWS): DeleteStack(StackName=<stack>) converges, and DescribeStacks(StackName=<stack>) then refuses with HTTP 400 Code `ValidationError`, Message `Stack with id <stack> does not exist`; DescribeStackResources(StackName=<stack>) refuses the same way; a second DeleteStack is a clean no-op; DescribeTaskDefinition on the stack's revision reads status `INACTIVE`.

Actual (emulator): polling DescribeStacks(StackName=<stack>) every 10 s for 1200 s after DeleteStack never observes the stack gone and never observes StackStatus `DELETE_FAILED` — the stack never reaches a terminal deleted state, so none of the post-delete assertions is reachable.

## Steps to Reproduce
1. Boot the emulator with cloudformation, ecs, ecr and ec2.
2. CreateRepository in ECR and push one image tag into it.
3. CreateStack with a template provisioning a VPC + subnet + security group, an AWS::ECS::Cluster, an AWS::AutoScaling::AutoScalingGroup, an AWS::ECS::CapacityProvider bound to that group, an AWS::ECS::TaskDefinition whose container images reference tags absent from the repository, and an AWS::ECS::Service with desiredCount 0 on that cluster (Capabilities=[CAPABILITY_IAM]). Wait for CREATE_COMPLETE.
4. DeleteRepository(force=true) on the ECR repository.
5. DeleteStack(StackName=<stack>).
6. Poll DescribeStacks(StackName=<stack>) every 10 s for up to 1200 s.
7. Observe that the stack never becomes gone (DescribeStacks never refuses with `Stack with id <stack> does not exist`) and never reports DELETE_COMPLETE or DELETE_FAILED.
