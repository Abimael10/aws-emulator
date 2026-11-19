Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: autoscaling__ec2__ssm.launch-template-resolve-ssm-alias-launch-blocked-while-parameter-is-absent
Issue-Key: autoscaling__ec2__ssm.launch-template-resolve-ssm-alias-launch-blocked-while-parameter-is-absent|run#19|AssertionError|assert launched["ImageId"] == ami_id, (
Issue-Commit: bf5f1b716fd44070c371033dd9201606434af162

# autoscaling__ec2__ssm: a launch-template resolve:ssm: ImageId is never resolved on the launch path — the launched instance's ImageId is the literal alias string

> _Note: this issue was identified against the code base at commit `bf5f1b716fd44070c371033dd9201606434af162`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RunInstances

## Description
Cause 1 of 1: an instance launched (here by an Auto Scaling group) from a launch-template version whose LaunchTemplateData.ImageId is a `resolve:ssm:<parameter-name>` alias carries that alias string as its own ImageId. The alias is stored verbatim and never resolved against the SSM parameter store at launch time. Failing check: autoscaling__ec2__ssm.launch-template-resolve-ssm-alias-launch-blocked-while-parameter-is-absent.

AWS: Smithy com.amazonaws.ec2#RequestLaunchTemplateData member ImageId documents "Alternatively, you can specify a Systems Manager parameter, using one of the following formats. The Systems Manager parameter will resolve to an AMI ID on launch", and lists resolve:ssm:<public-parameter>, resolve:ssm:<parameter-name>, resolve:ssm:<parameter-name>:<version-number>, resolve:ssm:<parameter-name>:<label> and the parameter-ARN forms. Smithy com.amazonaws.ec2#Instance member ImageId is "The ID of the AMI used to launch the instance" — an ami- identifier, never an alias. So the alias lives only in the launch template; every instance launched from it reports in DescribeInstances the ami- id the named parameter held at launch time. Recorded behavior for the negative leg: once the named parameter is deleted, the launch fails and the Auto Scaling activity records StatusCode `Failed` with StatusMessage "The following aliases are invalid: <parameter-name>. Launching EC2 instance failed." — the sentence is fixed text, only <parameter-name> is taken from the launch template — no second instance ever appears, and re-creating the parameter makes the identical scale-out succeed with the resolved ami- id again.

Emulator: CreateLaunchTemplate now accepts the alias and the Auto Scaling group does launch an instance, but DescribeInstances reports that instance's ImageId as the alias string itself, e.g. ImageId 'resolve:ssm:<parameter-name>' where the parameter's value was an 'ami-...' id. Because the alias is never dereferenced on the launch path, the rest of the scenario is unreachable as well: deleting the parameter cannot make a launch fail, so the Failed activity with the "The following aliases are invalid: ... Launching EC2 instance failed." StatusMessage is never recorded, and the repaired scale-out cannot be observed either.

Why: a client that pins an AMI by SSM alias — the documented idiom for keeping a launch template on the latest image — gets an instance whose reported AMI is an unusable string. Anything that reads ImageId back (DescribeInstances consumers, AMI-based inventory, drift detection, golden-image rotation) sees an identifier that resolves to no image, and the failure mode AWS has for a deleted/renamed parameter (a refused launch, not a launch on a bogus image) is inverted: the emulator launches an instance where AWS launches none.

Fix: the RunInstances handler's ImageId resolution in the ec2 service crate — the launch path shared by direct RunInstances and by Auto Scaling scale-out. It must dereference every resolve:ssm: form (name, name:version, name:label, parameter ARN and the ARN variants) against the SSM parameter store at launch time, persist and report the resolved ami- id on the instance, and treat an alias that resolves to nothing as a launch failure surfaced as the Auto Scaling activity StatusMessage above rather than as a successful launch.

Related already-filed work: open issue 20261117T140330Z-autoscaling-ec2-ssm-reopens-createlaunchtemplate-still-refuses-a.md describes CreateLaunchTemplate *refusing* this alias at create time with SsmInvalidParameter. That refusal no longer happens in this replay — the create succeeds and the check now gets as far as the launch — so that issue does not cover this defect; this is the distinct, downstream launch-path cause it predicted.

Traces: autoscaling__ec2__ssm.launch-template-resolve-ssm-alias-launch-blocked-while-parameter-is-absent.

## Evidence
Behavioral suite, service heading `### autoscaling__ec2__ssm`, check launch-template-resolve-ssm-alias-launch-blocked-while-parameter-is-absent.

The check creates an SSM parameter holding a valid AMI id, creates a launch template whose ImageId is the alias, creates an ASG on it with DesiredCapacity=1, waits for the group's first instance and then reads it back:

    ssm.put_parameter(Name=param_name, Value=ami_id, Type="String", DataType="aws:ec2:image")
    ec2.create_launch_template(LaunchTemplateData={"ImageId": f"resolve:ssm:{param_name}", "InstanceType": "t3.micro"}, ...)
    autoscaling.create_auto_scaling_group(LaunchTemplate={"LaunchTemplateId": lt_id}, MinSize=1, MaxSize=2, DesiredCapacity=1, ...)
    launched = ec2.describe_instances(InstanceIds=first_ids)["Reservations"][0]["Instances"][0]
    assert launched["ImageId"] == ami_id

Replay:

    AssertionError: the instance resolve:ssm launched carries ImageId 'resolve:ssm:sim2real-lthoame-param-2fc757983438', expected the parameter's value 'ami-0000000000000004d'

Expected (AWS): DescribeInstances reports the ami- id the parameter held at launch time; the alias string appears only in DescribeLaunchTemplateVersions. The parameter name and the AMI id above are per-run values — the requirement is that Instance.ImageId is the resolved ami- identifier, never the alias.

Not reached because of this cause, and part of the same defect: after `ssm.delete_parameter(Name=param_name)` and `set_desired_capacity(DesiredCapacity=2)`, AWS records an Auto Scaling activity with StatusCode `Failed` and StatusMessage exactly "The following aliases are invalid: <parameter-name>. Launching EC2 instance failed." and launches no second instance; restoring the parameter then launches one whose ImageId is again the resolved ami- id.

## Steps to Reproduce
1. Build and boot the emulator from this checkout.
2. ssm PutParameter Name=<fresh name>, Value=<a valid ami- id from DescribeImages>, Type=String, DataType=aws:ec2:image.
3. ec2 CreateLaunchTemplate with LaunchTemplateData={"ImageId": "resolve:ssm:<that parameter name>", "InstanceType": "t3.micro"}.
4. autoscaling CreateAutoScalingGroup on that launch template id with MinSize=1, MaxSize=2, DesiredCapacity=1 and a subnet in an AZ that offers t3.micro; wait for the group to report one instance.
5. ec2 DescribeInstances on that instance id and read Instances[0].ImageId. Observe the literal 'resolve:ssm:<parameter name>'; AWS reports the ami- id the parameter held.
6. ssm DeleteParameter on the parameter, then autoscaling SetDesiredCapacity DesiredCapacity=2 and poll DescribeScalingActivities. Observe no Failed activity carrying "The following aliases are invalid: <parameter name>. Launching EC2 instance failed."; AWS records exactly that and launches no second instance.
7. Re-replay the check id in trace_ids.
