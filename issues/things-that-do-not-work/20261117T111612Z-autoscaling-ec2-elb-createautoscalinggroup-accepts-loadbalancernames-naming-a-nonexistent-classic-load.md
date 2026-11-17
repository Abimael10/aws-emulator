Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: autoscaling__ec2__elb.create-auto-scaling-group-with-nonexistent-classic-load-balancer-rejected
Issue-Key: autoscaling__ec2__elb.create-auto-scaling-group-with-nonexistent-classic-load-balancer-rejected|_error#10|AssertionError|raise AssertionError(f"expected the call to be rejected, but it succeeded: {result!r}")
Issue-Commit: 9d0149c39ab5c73582ec7b48950d23789498e457

# CreateAutoScalingGroup accepts LoadBalancerNames naming a nonexistent Classic Load Balancer (HTTP 200) instead of refusing with ValidationError

> _Note: this issue was identified against the code base at commit `9d0149c39ab5c73582ec7b48950d23789498e457`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateAutoScalingGroup

## Description
Cause 1 of 1: CreateAutoScalingGroup performs no existence check on the Classic Load Balancer names supplied in LoadBalancerNames, so a group referencing a load balancer that does not exist is created instead of refused. Failing check: autoscaling__ec2__elb.create-auto-scaling-group-with-nonexistent-classic-load-balancer-rejected.

AWS: CreateAutoScalingGroup.LoadBalancerNames is modelled as a list of Classic Load Balancer names (Smithy: services/service-com-amazonaws-autoscaling-autoscaling-2011-01-01, CreateAutoScalingGroupType.LoadBalancerNames -> LoadBalancerNames, documented "A list of Classic Load Balancers associated with this Auto Scaling group"). AWS resolves every supplied name against ELB Classic before creating anything, and when any name has no matching Classic Load Balancer in the account/region it refuses the whole request. The awsQuery error is a sender fault with HTTP 400 and Error.Code "ValidationError"; the message is AWS's fixed sentence pair, with no variable parts at all: "Provided Load Balancers may not be valid. Please ensure they exist and try again." (The model's explicitly listed errors for this operation are AlreadyExists/LimitExceeded/ResourceContention/ServiceLinkedRoleFailure; ValidationError is the protocol-common sender fault this class of refusal uses — recorded behavior confirms code and message.) The refusal is also atomic: nothing is created, so a subsequent DescribeAutoScalingGroups for that group name returns an empty AutoScalingGroups list.

Emulator: the identical request returns HTTP 200 with an empty CreateAutoScalingGroup result (ResponseMetadata.HTTPStatusCode 200, content-type text/xml) and creates the group. The replay records: "AssertionError: expected the call to be rejected, but it succeeded: {'ResponseMetadata': {... 'HTTPStatusCode': 200 ...}}". No ValidationError, no 400, and the group named in the request is left behind, so the "rejected create left behind a group" invariant that follows cannot hold either.

Why: a client that relies on this refusal to catch a typo'd or deleted Classic Load Balancer name gets a silent success. The group is created wired to a load balancer that does not exist, so instances it launches are never registered anywhere and never become InService behind the intended load balancer, and the failure surfaces much later (or never) as traffic that is simply not served — instead of as a 400 ValidationError at create time. Negative-path tests and IaC preflight checks that assert this rejection pass against the emulator and fail against AWS.

Fix: CreateAutoScalingGroup's request validation in the autoscaling handler. Before any state mutation, resolve each name in LoadBalancerNames against the ELB Classic (elasticloadbalancing) load-balancer store for the request's account/region; if any name does not resolve, return the awsQuery sender fault ValidationError with HTTP 400 and AWS's fixed message, and create no group (the validation must precede group insertion so the failed create leaves no state). Names that do resolve must continue to be accepted and stored on the group, as the check's positive control exercises. Related but distinct open issues (do not cover this one): 20261114T211421Z-autoscaling-auto-scaling-2-net-new-causes-attachloadbalancertargetgroups-accepts.md and 20261113T064920Z-autoscaling-attachloadbalancertargetgroups-skips-target-type-validation-group-target.md both concern AttachLoadBalancerTargetGroups and ELBv2 target-group ARNs, not CreateAutoScalingGroup's Classic LoadBalancerNames; 20261110T160612Z-autoscaling-ec2-describeautoscalinggroups-omits-loadbalancernames-targetgrouparns-and-suspendedprocesses.md is about serializing that member on reads, not validating it on create.

Traces: autoscaling__ec2__elb.create-auto-scaling-group-with-nonexistent-classic-load-balancer-rejected (group resource-binding, axis:rejection).

## Evidence
Expected (AWS): CreateAutoScalingGroup with LoadBalancerNames=[<name with no matching Classic Load Balancer>] -> HTTP 400, Error.Code "ValidationError", Error.Message exactly "Provided Load Balancers may not be valid. Please ensure they exist and try again." (entirely fixed text; no request- or state-derived substitutions). No group is created: DescribeAutoScalingGroups(AutoScalingGroupNames=[<group>]) then returns AutoScalingGroups == [].

Actual (emulator): HTTP 200 with an empty successful CreateAutoScalingGroup response and the group created. Replay detail: `AssertionError: expected the call to be rejected, but it succeeded: {'ResponseMetadata': {'RequestId': ..., 'HTTPStatusCode': 200, 'HTTPHeaders': {'content-type': 'text/xml', 'content-length': '279', ...}, 'RetryAttempts': 0}}` (RequestId, date and content-length are per-run values, not part of the requirement). Since the call succeeded, no Error.Code or Error.Message was produced at all.

## Steps to Reproduce
1. ec2.create_launch_template(LaunchTemplateName=<unique>, LaunchTemplateData={ImageId: <al2023 AMI from SSM parameter /aws/service/ami-amazon-linux-latest/al2023-ami-kernel-default-x86_64>, InstanceType: 't3.micro'}) and note LaunchTemplateId.
2. Pick a Classic Load Balancer name that does not exist (<=32 chars), e.g. 'sim2real-absent-clb-<unique>'.
3. autoscaling.create_auto_scaling_group(AutoScalingGroupName=<unique>, LaunchTemplate={'LaunchTemplateId': <id>}, MinSize=0, MaxSize=0, DesiredCapacity=0, VPCZoneIdentifier=<subnet-id>, LoadBalancerNames=[<nonexistent name>]).
   AWS: ClientError, HTTP 400, Code 'ValidationError', Message 'Provided Load Balancers may not be valid. Please ensure they exist and try again.'
   Emulator: HTTP 200, no error raised.
4. autoscaling.describe_auto_scaling_groups(AutoScalingGroupNames=[<same name>]) -> AWS: AutoScalingGroups == []. Emulator: the group created in step 3 is listed.
5. Positive control (must keep working): elb.create_load_balancer(LoadBalancerName=<real>, Listeners=[{Protocol TCP, LoadBalancerPort 80, InstanceProtocol TCP, InstancePort 80}], Subnets=[<subnet-id>]), then repeat step 3 with LoadBalancerNames=[<real>] -> succeeds, and the described group carries LoadBalancerNames == [<real>].
