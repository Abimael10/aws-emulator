Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ssm.compliance-summary-server-computed-aggregation
Issue-Shape-Id: com.amazonaws.ssm#AmazonSSM
Issue-Key: ssm.compliance-summary-server-computed-aggregation|_wait_for_agent_online#2|AssertionError|raise AssertionError(
Issue-Commit: f4f47df5aa8b9a19bc159acc32ad01036b03b7fe

# ssm: an EC2 instance launched with an AmazonSSMManagedInstanceCore instance profile never becomes an Online managed node — DescribeInstanceInformation returns an empty InstanceInformationList

> _Note: this issue was identified against the code base at commit `f4f47df5aa8b9a19bc159acc32ad01036b03b7fe`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeInstanceInformation

## Description
Cause 1 of 1: SSM never registers an EC2 instance that was launched with an instance profile carrying arn:aws:iam::aws:policy/AmazonSSMManagedInstanceCore, so DescribeInstanceInformation never reports that instance at all and no caller can ever observe PingStatus=Online for it. Every behavioral flow that first has to stand up a managed node is blocked behind this, which is why this check never reaches the ListComplianceSummaries behavior it is actually about.

AWS: Smithy (services/service-com-amazonaws-ssm-amazonssm/smithy-model/com-amazonaws-ssm-amazonssm.json) models DescribeInstanceInformation -> DescribeInstanceInformationResult.InstanceInformationList of InstanceInformation, whose PingStatus member targets the enum PingStatus with values Online | ConnectionLost | Inactive and is documented as "Connection status of SSM Agent". InstanceInformation.ResourceType is documented "The type of instance. Instances are either EC2 instances or managed instances." On real AWS, an EC2 instance running SSM Agent whose attached instance profile grants AmazonSSMManagedInstanceCore registers itself with Systems Manager shortly after boot; DescribeInstanceInformation(Filters=[{Key: InstanceIds, Values: [<instance-id>]}]) then returns exactly one InstanceInformation for that instance, with ResourceType=EC2Instance, PingStatus=Online, a LastPingDateTime that advances, a RegistrationDate, and AgentVersion/PlatformType/PlatformName populated. The call itself is a 200 in both cases — the divergence is in the response body, not in a status code or error code. Recorded behavior only for the registration latency (real AWS takes tens of seconds; the suite polls).

Emulator: the launch path succeeds — iam.CreateRole/AttachRolePolicy/CreateInstanceProfile/AddRoleToInstanceProfile, ssm.GetParameter for /aws/service/ami-amazon-linux-latest/amzn2-ami-hvm-x86_64-gp2, and ec2.RunInstances(IamInstanceProfile={Name: <profile>}, SubnetId=<subnet>) all return 200 and yield an instance id — but the subsequent ssm.DescribeInstanceInformation polls filtered on that instance id keep returning an InstanceInformationList with no entry for it, so PingStatus is never Online and the check aborts with "instance <instance-id> never reached PingStatus=Online ... (SSM Agent never registered)". No managed-node record is ever materialized for an instance-profile-equipped EC2 instance.

Why: a client that follows AWS's documented bootstrap (attach AmazonSSMManagedInstanceCore to the instance profile, launch, wait for the node to appear Online) waits forever. Nothing downstream of registration is reachable: Run Command, State Manager associations, Session Manager, Patch Manager and Compliance all address a managed node by instance id, so every one of those planes is unusable even though their own handlers may be correct. This is a whole-data-plane outage, not a field-level mismatch.

Fix: the SSM managed-node inventory/state that backs DescribeInstanceInformation (services/service-com-amazonaws-ssm-amazonssm) must gain a registration path for EC2 instances: when an instance is running and its attached instance profile's role grants the SSM core permissions, a managed-node record must be created with ResourceType=EC2Instance, PingStatus=Online, LastPingDateTime/RegistrationDate set and AgentVersion/PlatformType/PlatformName/ComputerName/IPAddress populated from the instance, and it must be removed/aged out when the instance stops or terminates. Related open issues describe the same symptom reached from other services and are NOT this one (they carry different service tokens and different replay keys): 20261116T030341Z-codedeploy-the-minimumhealthyhosts-concurrency-check-still-cannot-start.md, 20261121T222346Z-codedeploy-the-deployment-failure-auto-rollback-check-cannot.md, 20261121T113443Z-ec2-guest-data-plane-aws-runshellscript-stdout-is.md, 20261114T143448Z-ec2-ssm-guest-execution-never-terminates-describeinstanceinformation-reports.md (which reports the opposite skew — Online for an egress-less instance). None of them covers the plain `ssm` service token or this check's replay key.

Traces: ssm.compliance-summary-server-computed-aggregation — _launch_ssm_managed_instance -> _wait_for_agent_online asserts on ssm.describe_instance_information. Note the sibling check ssm.compliance-summary-aggregates-by-status is separately blocked by the unimplemented ListComplianceSummaries stub and is NOT part of this issue.

## Evidence
Expected (real AWS): after RunInstances with IamInstanceProfile={Name: <profile whose role has arn:aws:iam::aws:policy/AmazonSSMManagedInstanceCore>}, a poll of
  ssm.describe_instance_information(Filters=[{"Key": "InstanceIds", "Values": ["<instance-id>"]}])
returns HTTP 200 with InstanceInformationList == [ { InstanceId: "<instance-id>", PingStatus: "Online", ResourceType: "EC2Instance", LastPingDateTime: <timestamp>, RegistrationDate: <timestamp>, AgentVersion: <x.y.z>, PlatformType: "Linux", PlatformName: <os name>, ... } ].

Actual (emulator): the same call returns HTTP 200 with an InstanceInformationList that contains no entry for the launched instance, for the entire poll window, so PingStatus is never "Online". The check fails in check_ssm_compliance_summary_server_computed_aggregation.py:
  File "check_ssm_compliance_summary_server_computed_aggregation.py", line 212, in _launch_ssm_managed_instance
    _wait_for_agent_online(ctx, instance_id)
  File "check_ssm_compliance_summary_server_computed_aggregation.py", line 135, in _wait_for_agent_online
    raise AssertionError(
  AssertionError: instance i-00000000000000001 never reached PingStatus=Online within 1s (SSM Agent never registered)

The instance id (i-00000000000000001), the generated role/profile names, the subnet id and the poll deadline printed in that message are per-run values, not requirements; the requirement is that a registered EC2 managed node appears in InstanceInformationList with PingStatus=Online.

## Steps to Reproduce
1. iam.CreateRole with an ec2.amazonaws.com sts:AssumeRole trust policy; iam.AttachRolePolicy(PolicyArn="arn:aws:iam::aws:policy/AmazonSSMManagedInstanceCore").
2. iam.CreateInstanceProfile and iam.AddRoleToInstanceProfile for that role.
3. ssm.GetParameter(Name="/aws/service/ami-amazon-linux-latest/amzn2-ami-hvm-x86_64-gp2") to get an ImageId.
4. ec2.RunInstances(ImageId=<that image id>, InstanceType="t3.micro", MinCount=1, MaxCount=1, IamInstanceProfile={"Name": <profile name>}, SubnetId=<a subnet in the test VPC>) and take InstanceId from the response.
5. Poll ssm.DescribeInstanceInformation(Filters=[{"Key": "InstanceIds", "Values": [<InstanceId>]}]).
6. Observe that InstanceInformationList stays empty and no record with PingStatus="Online" (or any PingStatus) ever appears for the instance. On AWS the instance appears as an EC2Instance managed node with PingStatus="Online".

Replay: run_behavioral_suite and re-replay check ssm.compliance-summary-server-computed-aggregation.
