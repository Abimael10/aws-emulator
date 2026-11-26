Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__ec2__iam.cdk-three-region-ec2-estate-deploys-serves-and-destroys cloudformation__ec2__iam.deploy-ec2-spot-fleet-wrong-trust-policy-rejected-and-valid-role-succeeds
Issue-Key: cloudformation__ec2__iam.cdk-three-region-ec2-estate-deploys-serves-and-destroys|_run_cli#13|AssertionError|assert result.returncode == 0, (
Issue-Key: cloudformation__ec2__iam.deploy-ec2-spot-fleet-wrong-trust-policy-rejected-and-valid-role-succeeds|run#26|AssertionError|assert got == (INSTANCE_TYPE, outputs["SubnetId"], ami, True), f"fleet instance reads {got!r}"
Issue-Commit: 34fefd7aecead9141157dc0abcdfe1d4823d27aa

# cloudformation__ec2__iam: 2 net-new causes — a failed stack's rollback ends ROLLBACK_FAILED on deleting its Custom::AWS resource, and SpotFleet-launched instances carry none of the launch template's instance TagSpecifications

> _Note: this issue was identified against the code base at commit `34fefd7aecead9141157dc0abcdfe1d4823d27aa`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteStack

## Description
Two independent net-new root causes under the cloudformation__ec2__iam suite. A fixer must close BOTH before submitting.

=== Cause 1 of 2: a stack whose resource create fails cannot finish its rollback — it ends ROLLBACK_FAILED because the Custom::AWS resource fails to DELETE ===
Failing check: cloudformation__ec2__iam.cdk-three-region-ec2-estate-deploys-serves-and-destroys
AWS:      recorded behavior only. When a resource in a CREATE_IN_PROGRESS stack fails, CloudFormation rolls the stack back and reaches a terminal ROLLBACK_COMPLETE: every resource it had begun creating is deleted, and a custom resource (here an `AWS::CloudFormation::CustomResource`/`Custom::AWS` backed by the CDK AwsCustomResource provider, logical id `CreateAmi`) whose CREATE never produced a physical id is removed without its on-delete SDK call being dispatched against a nonexistent resource. In the recording the whole three-region estate deploys instead: `cdk deploy --all` exits 0 and each of the four stacks reads CREATE_COMPLETE.
Emulator: `cdk deploy --all` exits 1. The Workload stack fails creation and then cannot roll back: DeploymentError reports "The stack named <prefix>-Workload failed creation, it may need to be manually deleted from the AWS console: ROLLBACK_FAILED (The following resource(s) failed to delete: [CreateAmi7BE3E6A5]. )". The stack is left in the non-terminal-for-the-client ROLLBACK_FAILED state with the custom resource undeleted.
          The create-side trigger is the EC2 CreateImage gap the suite already files for you (UNIMPLEMENTED `CreateImage`, services/service-com-amazonaws-ec2-amazonec2/src/ops/create_image.rs); THIS cause is the separate rollback defect: whatever makes the custom resource's create fail, CloudFormation must still drive the rollback to ROLLBACK_COMPLETE rather than stalling on the DELETE of that resource.
Why:      a client — the CDK CLI here, but equally `aws cloudformation create-stack` + waiter — is left with a stack it can neither use nor delete from its own tooling, and the real diagnosis (the failed CREATE) is replaced by a delete failure. ROLLBACK_FAILED requires manual continue-update-rollback/console intervention, so every automated deploy pipeline that hits any create failure in a stack holding a custom resource wedges instead of cleaning up and reporting the cause.
Fix:      the CloudFormation stack rollback engine and its custom-resource delete path — a resource whose CREATE did not complete must be deleted (or skipped) during rollback so the stack reaches ROLLBACK_COMPLETE, and a custom resource with no physical id must not be driven through its on-delete call.
Traces:   cloudformation__ec2__iam.cdk-three-region-ec2-estate-deploys-serves-and-destroys

=== Cause 2 of 2: an instance launched by a SpotFleet from a launch template carries none of the template's instance TagSpecifications ===
Failing check: cloudformation__ec2__iam.deploy-ec2-spot-fleet-wrong-trust-policy-rejected-and-valid-role-succeeds
AWS:      recorded behavior only. A `AWS::EC2::SpotFleet` whose LaunchTemplateConfigs name an `AWS::EC2::LaunchTemplate` whose LaunchTemplateData carries TagSpecifications for ResourceType "instance" (and "volume") launches instances that carry those tags: DescribeInstances on the fleet's active instance lists the launch template's instance tag among its Tags, alongside the template's InstanceType, the override's SubnetId and the resolved ImageId.
Emulator: the fleet does launch one instance with the right InstanceType, SubnetId and ImageId, but the launch template's instance TagSpecifications are dropped — DescribeInstances reports Tags without the template's tag, so the tuple reads ('t3.micro', '<subnet>', '<ami>', False) where the fourth element is "the launch template's instance tag is present on the instance".
Why:      tags applied through a launch template are how fleet-launched instances are attributed, cost-allocated and swept by cleanup/garbage-collection tooling. A client that tags instances only via the launch template (the only place a SpotFleet can tag the instances it launches) sees untagged instances: tag-filtered DescribeInstances returns nothing, cost allocation misses them, and tag-scoped teardown leaks them.
Fix:      the EC2 spot-fleet fulfilment path that launches instances from a LaunchTemplateConfig — it must apply the resolved launch template version's TagSpecifications (ResourceType "instance", and "volume" for the created volumes) to the launched instance, the same way a direct RunInstances from that template does.
Traces:   cloudformation__ec2__iam.deploy-ec2-spot-fleet-wrong-trust-policy-rejected-and-valid-role-succeeds

## Evidence
Cause 1 — cloudformation__ec2__iam.cdk-three-region-ec2-estate-deploys-serves-and-destroys:
  Expected: `cdk deploy --all` exits 0; each stack (<prefix>-AppRoles, <prefix>-Workload, <prefix>-EC2Edge-<region> x2) reads CREATE_COMPLETE.
  Actual:   AssertionError: `cdk deploy --all` exited 1, expected 0; output tail:
            "...\u274c  <prefix>-Workload failed to deploy
             \u2023 DeploymentError: The stack named <prefix>-Workload failed creation, it may need to be manually deleted from the AWS console: ROLLBACK_FAILED (The following resource(s) failed to delete: [CreateAmi7BE3E6A5]. )"
  The named resource is the stack's CDK AwsCustomResource (Custom::AWS, logical id CreateAmi) whose on_create is ec2:createImage and on_delete is ec2:deregisterImage.

Cause 2 — cloudformation__ec2__iam.deploy-ec2-spot-fleet-wrong-trust-policy-rejected-and-valid-role-succeeds:
  The stack reached CREATE_COMPLETE, DescribeSpotFleetRequests matched the expected config (active, TargetCapacity 1, IamFleetRole, lowestPrice, maintain, TerminateInstancesWithExpiration true, LaunchTemplateConfigs) and DescribeSpotFleetInstances reported one active instance.
  Expected: (InstanceType, SubnetId, ImageId, launch-template instance tag present) == ('t3.micro', <stack's SubnetId>, <resolved AMI>, True)
  Actual:   ('t3.micro', 'subnet-000000000000000c3', 'ami-00000000000000053', False)
            AssertionError: fleet instance reads ('t3.micro', 'subnet-000000000000000c3', 'ami-00000000000000053', False)
  i.e. only the tag assertion fails: {"Key": "sim2real-res", "Value": <run tag value>} declared in the launch template's TagSpecifications (ResourceType "instance") is absent from the instance's Tags.

Related, already filed for you (do not re-file): UNIMPLEMENTED `CreateImage` (services/service-com-amazonaws-ec2-amazonec2/src/ops/create_image.rs) is what makes the CreateAmi custom resource's CREATE fail in Cause 1; the rollback defect is separate.

## Steps to Reproduce
Cause 1: deploy any CloudFormation stack that contains a Custom::AWS / AWS::CloudFormation::CustomResource whose create call fails (the CDK app in the check declares `cr.AwsCustomResource(self, "CreateAmi", on_create=ec2:createImage, on_delete=ec2:deregisterImage)` beside a VPC, instances, a launch template and an instance profile), then observe the stack's terminal status and DescribeStackEvents: it ends ROLLBACK_FAILED naming the custom resource under "failed to delete" instead of ROLLBACK_COMPLETE.

Cause 2: CreateStack with AWS::EC2::VPC + AWS::EC2::Subnet + AWS::EC2::LaunchTemplate whose LaunchTemplateData carries TagSpecifications [{ResourceType: instance, Tags: [{Key: sim2real-res, Value: <v>}]}, {ResourceType: volume, ...}] + AWS::EC2::SpotFleet whose SpotFleetRequestConfigData has IamFleetRole (a role trusting spotfleet.amazonaws.com), TargetCapacity 1, Type maintain, LaunchTemplateConfigs referencing the template with Overrides [{SubnetId}]. Wait CREATE_COMPLETE, DescribeSpotFleetInstances for the fleet, wait the instance running, then DescribeInstances on it and inspect Tags — the launch template's instance tag is missing.
