Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2__ecs__elbv2.e2e-ecsnt-diagnose-ecs-cluster-no-traffic-lifecycle
Issue-Key: ec2__ecs__elbv2.e2e-ecsnt-diagnose-ecs-cluster-no-traffic-lifecycle|phase#11|AssertionError|raise AssertionError(f"[phase {i}/{len(self.phases)} {name}] {exc}") from exc
Issue-Commit: 23a6305fb4194ac652f562ce813f73dafa581013

# ec2__ecs__elbv2: DeleteStack on a Fargate ECS + internal ALB estate never reaches DELETE_COMPLETE — the stack stays describable and its ALB, cluster and target groups are never released

> _Note: this issue was identified against the code base at commit `23a6305fb4194ac652f562ce813f73dafa581013`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteStack

## Description
Cause 1 of 1: DeleteStack on a CloudFormation stack that owns a Fargate ECS estate fronted by an internal Application Load Balancer (VPC + two security groups + IAM roles + ECS cluster + two task definitions + internal ALB + two target groups + two listeners + two ECS services + two log groups) never converges: the stack neither disappears nor reports DELETE_COMPLETE, so none of its resources is released.

AWS:      Smithy (services/service-com-amazonaws-cloudformation-cloudformation/smithy-model/com-amazonaws-cloudformation-cloudformation.json) declares DeleteStack(StackName @required, RetainResources, RoleARN, ClientRequestToken, DeletionMode, DeploymentConfig) -> Unit with TokenAlreadyExistsException as its only modelled error, and DescribeStacks(StackName, NextToken) -> Stacks with no modelled errors. The convergence contract is recorded behavior only: after DeleteStack, AWS deletes the stack's resources in reverse dependency order — the ECS services drain, the listeners and target groups go before the ALB, the ALB's network interfaces are released before its subnets and security groups, the cluster goes after its services — and the stack reaches DELETE_COMPLETE. DescribeStacks / DescribeStackResources on the name then refuse with HTTP 400 Code `ValidationError`, Message `Stack with id <StackName> does not exist` (fixed text `Stack with id ` and ` does not exist`; the name comes from the request); a second DeleteStack is a clean no-op; the stack's ECS cluster reads status `INACTIVE`; DescribeLoadBalancers on the deleted ALB refuses with Code `LoadBalancerNotFound`, Message `One or more load balancers not found`; and the stack's task definitions stay describable as `INACTIVE` (a stack delete DEREGISTERS a revision, only DeleteTaskDefinitions removes it).

Emulator: after DeleteStack the stack never reaches a deleted state. The check polls DescribeStacks(StackName=<stack>) every 10 s for 1200 s and accepts either the stack being gone (DescribeStacks refusing with `... does not exist`) or StackStatus `DELETE_FAILED`; neither is reached in that budget, so the stack is left in a non-terminal status while DescribeStacks keeps answering 200 with the stack present. Everything before the teardown passed — the stack reached CREATE_COMPLETE, the cluster read ACTIVE with capacity providers FARGATE and FARGATE_SPOT, the ALB read scheme `internal` in the stack's VPC, the frozen nine-hop diagnosis trajectory derived exactly the deployed estate, and the residue sweep found no tasks, no targets and no log streams — so the defect is confined to the delete path.

Why:      A client cannot tear the estate down. `stack_delete_complete` waiters, `cdk destroy`, `terraform destroy` over the stack and CI jobs that delete their stack between runs all block until their own timeout and then report a failed destroy. Because the delete never releases resources, the ALB, its listeners and target groups, the two ECS services, the cluster, the log groups and the whole VPC net stay live and leak into the account; the next deploy of the same template collides with that residue, and no post-delete assertion (cluster INACTIVE, ALB LoadBalancerNotFound, revisions INACTIVE, a second delete being a no-op) can ever be reached.

Fix:      The DeleteStack delete-progression in the CloudFormation stack engine, specifically the dependency-ordered release of this estate's resource types (AWS::ECS::Service, AWS::ECS::Cluster, AWS::ECS::TaskDefinition, AWS::ElasticLoadBalancingV2::Listener / TargetGroup / LoadBalancer, AWS::Logs::LogGroup and the VPC resources beneath them): every resource must reach a terminal delete verdict and the stack must then move to DELETE_COMPLETE, or to DELETE_FAILED naming the resource and reason that blocked it — never remain in a non-terminal status indefinitely.

Traces:   ec2__ecs__elbv2.e2e-ecsnt-diagnose-ecs-cluster-no-traffic-lifecycle (phase 4/4 `destroy`).

## Evidence
Failing check `ec2__ecs__elbv2.e2e-ecsnt-diagnose-ecs-cluster-no-traffic-lifecycle` (verdict: mismatch).

Replay output:
  File "check_ec2__ecs__elbv2_e2e_ecsnt_diagnose_ecs_cluster_no_traffic_lifecycle.py", line 234, in phase
    raise AssertionError(f"[phase {i}/{len(self.phases)} {name}] {exc}") from exc
  AssertionError: [phase 4/4 destroy] stack deletion did not reach DELETE_COMPLETE

Phases 1-3 (deploy, solve, cleanup) all passed: the stack reached CREATE_COMPLETE, DescribeClusters reported status ACTIVE with capacity providers [FARGATE, FARGATE_SPOT], DescribeLoadBalancers reported (Scheme, VpcId) = (internal, <stack VpcId>), the frozen nine-operation trajectory derived the deployed estate exactly (two services, desired 0, running 0, 0 IGWs, 0 NAT gateways, 0 VPC endpoints, 0 default routes, one internal LB), and the residue sweep found no RUNNING/STOPPED tasks, no target-group targets and no log streams.

Expected (recorded AWS): DeleteStack(StackName=<stack>) converges; DescribeStackResources(StackName=<stack>) then refuses with Code `ValidationError`, Message `Stack with id <stack> does not exist`; a second DeleteStack is a clean no-op; DescribeClusters on the stack's cluster ARN reads status `INACTIVE`; DescribeLoadBalancers on the stack's ALB ARN refuses with Code `LoadBalancerNotFound`, Message `One or more load balancers not found`; the stack's task definitions read `INACTIVE`.

Actual (emulator): polling DescribeStacks(StackName=<stack>) every 10 s for 1200 s after DeleteStack never observes the stack gone and never observes StackStatus `DELETE_FAILED` — the stack never reaches a terminal deleted state, so none of the post-delete assertions is reachable.

## Steps to Reproduce
1. Boot the emulator with cloudformation, ecs, elbv2, ec2 and logs.
2. CreateStack with a template provisioning an isolated VPC (no IGW, no NAT, no VPC endpoints), two security groups, task/execution IAM roles, an AWS::ECS::Cluster with FARGATE and FARGATE_SPOT capacity providers, two AWS::ECS::TaskDefinition revisions, an internal AWS::ElasticLoadBalancingV2::LoadBalancer with two target groups and two listeners, two AWS::ECS::Service resources with desiredCount 0, and two AWS::Logs::LogGroup resources with retentionInDays 7. Wait for CREATE_COMPLETE.
3. DeleteStack(StackName=<stack>).
4. Poll DescribeStacks(StackName=<stack>) every 10 s for up to 1200 s.
5. Observe that the stack never becomes gone (DescribeStacks never refuses with `Stack with id <stack> does not exist`) and never reports DELETE_COMPLETE or DELETE_FAILED; DescribeLoadBalancers on the stack's ALB and DescribeClusters on its cluster still answer with live resources.
