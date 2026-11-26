Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2__ssm.run-instances-empty-placement-availability-zone-treated-as-unset
Issue-Key: ec2__ssm.run-instances-empty-placement-availability-zone-treated-as-unset|run#13|AssertionError|assert refused == want, (
Issue-Commit: f214ea914d97c777aba1c48734a44751ff321c53

# ec2 RunInstances re-words the InvalidParameterValue refusal for an unknown Placement.AvailabilityZone

> _Note: this issue was identified against the code base at commit `f214ea914d97c777aba1c48734a44751ff321c53`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RunInstances

## Description
Cause 1 of 1: RunInstances correctly refuses a Placement.AvailabilityZone naming a zone the region does not have, with the correct HTTP 400 / InvalidParameterValue, but the message is the emulator's own sentence rather than EC2's (failing check: ec2__ssm.run-instances-empty-placement-availability-zone-treated-as-unset).

AWS: Smithy (ec2, awsQuery/ec2Query protocol): RunInstances input member Placement -> Placement structure, member AvailabilityZone targets String with no enum, pattern or length constraint, documented "The Availability Zone of the instance. On input, you can specify AvailabilityZone or AvailabilityZoneId, but not both. If you specify neither one, Amazon EC2 automatically selects an Availability Zone for you." RunInstances models no error shapes at all (refusals are protocol-level ec2Query errors), so the refusal's code and message are recorded behavior only. Recorded behavior: a DryRun RunInstances whose Placement.AvailabilityZone names a zone that does not exist in the request's region is refused HTTP 400 with Code "InvalidParameterValue" and Message "Invalid availability zone: [<zone>]" — the fixed text is "Invalid availability zone: [" and the closing "]", and the single variable part is the AvailabilityZone value copied verbatim from the request; the region is NOT named in the sentence. The two control legs of the same recording are already correct in the emulator and are not part of this cause: Placement.AvailabilityZone set to the subnet's own zone, and Placement.AvailabilityZone set to the empty string (treated as unset), both answer the DryRun refusal ("DryRunOperation", "Request would have succeeded, but DryRun flag is set."), and nothing launches.

Emulator: same status and same code, but Message "Invalid value '<zone>' for availabilityZone. Region '<region>' has no such Availability Zone" — entirely different fixed text ("Invalid value '", "' for availabilityZone. Region '", "' has no such Availability Zone", no bracketed form), and it carries a second variable part, the region, that AWS's sentence does not contain.

Why: a client that matches, logs or surfaces the service's refusal text — the only informative part of this refusal, since RunInstances models no error shape to discriminate on — sees wording EC2 never emits. Launch-time remediation and retry logic keyed on AWS's documented sentence (for example picking another zone when the message reports an invalid availability zone) does not fire, and test suites that assert the AWS message fail against the emulator.

Fix: the RunInstances handler's placement validation, in the arm that rejects an unknown Availability Zone — the refusal value's message builder must emit AWS's sentence with the requested zone in brackets and no region. The code (InvalidParameterValue), the 400 status and the surrounding behavior (empty string treated as unset, DryRun short-circuit, nothing launched) are already right and must not change. Related but different: open issue 20261116T190237Z-ec2-region-availability-zone-and-vpc-endpoint-service.md reports the region's zone CATALOG being incomplete; this is the wording of the refusal for a zone that is correctly judged absent, and that issue does not carry this check's replay key.

Traces: ec2__ssm.run-instances-empty-placement-availability-zone-treated-as-unset (third assertion; its first two assertions pass).

## Evidence
Failing check ec2__ssm.run-instances-empty-placement-availability-zone-treated-as-unset:

AssertionError: Placement.AvailabilityZone 'us-west-2-nozone': expected ('InvalidParameterValue', 'Invalid availability zone: [us-west-2-nozone]'), got ('InvalidParameterValue', "Invalid value 'us-west-2-nozone' for availabilityZone. Region 'us-west-2' has no such Availability Zone")

Expected: HTTP 400, Code InvalidParameterValue, Message "Invalid availability zone: [<requested zone>]".
Actual: HTTP 400, Code InvalidParameterValue, Message "Invalid value '<requested zone>' for availabilityZone. Region '<region>' has no such Availability Zone".

The zone string ('us-west-2-nozone') and the region ('us-west-2') shown here are this run's values; the requirement is the template, not these literals. The control legs in the same check passed: Placement={'AvailabilityZone': <subnet's zone>} and Placement={'AvailabilityZone': ''} both returned ('DryRunOperation', 'Request would have succeeded, but DryRun flag is set.').

## Steps to Reproduce
1. Clone the repo and boot the emulator built from it (the behavioral suite does this).
2. ec2 CreateVpc(CidrBlock='10.78.0.0/16'); create one subnet in a zone that offers t3.micro; note that zone <az>.
3. ssm GetParameter(Name='/aws/service/ami-amazon-linux-latest/al2023-ami-kernel-default-x86_64') for <ami>.
4. Control A: ec2 RunInstances(DryRun=True, ImageId=<ami>, InstanceType='t3.micro', MinCount=1, MaxCount=1, SubnetId=<subnet>, Placement={'AvailabilityZone': <az>}) — expect DryRunOperation (already correct).
5. Control B: the same call with Placement={'AvailabilityZone': ''} — expect DryRunOperation (already correct).
6. Divergence: the same call with Placement={'AvailabilityZone': '<region>-nozone'} — the error Code is InvalidParameterValue as AWS returns, but compare the Message with "Invalid availability zone: [<region>-nozone]". The emulator instead answers "Invalid value '<region>-nozone' for availabilityZone. Region '<region>' has no such Availability Zone".
7. ec2 DescribeInstances filtered on the run's marker tag — still empty, as required.
