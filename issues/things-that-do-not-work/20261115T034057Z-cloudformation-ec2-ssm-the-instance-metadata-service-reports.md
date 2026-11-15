Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__ec2__ssm.cfn-ec2-instance-profile-enables-ssm-agent-registration-positive
Issue-Key: cloudformation__ec2__ssm.cfn-ec2-instance-profile-enables-ssm-agent-registration-positive|run#10|AssertionError|assert imds_role == role_name, (
Issue-Commit: 2bc939698b9a959721d42e691c2ca676c39ad8ba

# cloudformation__ec2__ssm: the instance metadata service reports an empty IAM role for an instance whose CloudFormation-attached instance profile names one

> _Note: this issue was identified against the code base at commit `2bc939698b9a959721d42e691c2ca676c39ad8ba`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RunInstances

## Description
=== Cause 1 of 1: IMDS serves no role name for an instance that carries an IAM instance profile ===
AWS: recorded behavior only. An instance launched with an IamInstanceProfile — here one CloudFormation created and attached together with its role — serves that role under the instance metadata service: a read of `/latest/meta-data/iam/security-credentials/` lists exactly the role name the instance profile names, and `/latest/meta-data/iam/info` names the profile's ARN. The name served is the role the profile contains, taken from the account's state, not a literal.
Emulator: the metadata read comes back empty — the reported role name is `''` while the profile attached by the stack names a real role. The instance therefore appears to carry no role at all from inside the guest.
Why: this is how every AWS SDK and the SSM agent inside an instance obtain credentials. A client that provisions an instance profile through its stack and expects the agent (or any in-guest SDK) to pick up the role gets no identity: the SSM agent cannot register the node, and in-guest calls fall back to no credentials, so the whole instance-profile mechanism is silently inert even though the control plane reports the profile attached.
Fix: the instance metadata service in the ec2 data plane must resolve the instance's attached IAM instance profile to its role and serve that role name (and the profile ARN) under the `iam/security-credentials/` and `iam/info` metadata paths.
Traces: cloudformation__ec2__ssm.cfn-ec2-instance-profile-enables-ssm-agent-registration-positive.

## Evidence
cloudformation__ec2__ssm.cfn-ec2-instance-profile-enables-ssm-agent-registration-positive: `AssertionError: instance metadata service reports IAM role '', expected '<stack-created-role-name>' (the role CFN attached via the instance profile)`. The role name in the recording is a per-run, stack-generated value; the requirement is that IMDS serve the name of the role the attached instance profile contains.

## Steps to Reproduce
1. Clone the repo and run the behavioral suite so the emulator is serving.
2. CreateStack with an IAM role, an AWS::IAM::InstanceProfile naming it, and an AWS::EC2::Instance whose IamInstanceProfile is that profile; wait for CREATE_COMPLETE.
3. From inside the instance, read `http://169.254.169.254/latest/meta-data/iam/security-credentials/` — it is empty instead of listing the stack's role name.
