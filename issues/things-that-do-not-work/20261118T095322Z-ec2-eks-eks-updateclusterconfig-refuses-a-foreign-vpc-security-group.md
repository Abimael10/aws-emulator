Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2__eks.ekscc-update-cluster-config-security-group-from-another-vpc-rejected
Issue-Key: ec2__eks.ekscc-update-cluster-config-security-group-from-another-vpc-rejected|run#21|AssertionError|assert code == "InvalidRequestException", (
Issue-Commit: cd94cc265dda0020a7368311398ddefc098e50a1

# EKS UpdateClusterConfig refuses a foreign-VPC security group with InvalidParameterException instead of InvalidRequestException

> _Note: this issue was identified against the code base at commit `cd94cc265dda0020a7368311398ddefc098e50a1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateClusterConfig

## Description
Cause 1 of 1: UpdateClusterConfig that sets resourcesVpcConfig.securityGroupIds to a security group from another VPC than the cluster's subnets is refused with the wrong error code; the message already matches AWS.

AWS: UpdateClusterConfig rejects this with InvalidRequestException (HTTP 400). CreateCluster uses InvalidParameterException for the same cross-VPC check, but the update path does not - the two operations classify it differently. Smithy: eks UpdateClusterConfig declares BOTH InvalidParameterException and InvalidRequestException (each client, httpError 400) among its errors, so the distinction is modelled and the emulator is picking the wrong one of two available shapes. The message AWS serves - 'Security group(s) [<the security group ids from the request>] are not (in/associated to) the same VPC as the subnets. Please specify a security group that is associated with the VPC: <the cluster's vpc id>' - is what the emulator already returns.

Emulator: answers InvalidParameterException with that same message, i.e. the CreateCluster classification, for UpdateClusterConfig. The cluster's security groups are otherwise left unchanged, as AWS does.

Why: an SDK client branches on the exception shape, not the prose: code that catches InvalidRequestException on UpdateClusterConfig (per the modelled error set) never sees it and the failure escapes as an unexpected InvalidParameterException; retry/classification logic that distinguishes the two is inverted for this path.

Fix: the UpdateClusterConfig handler's resourcesVpcConfig validation - raise InvalidRequestException for the foreign-VPC security-group case, leaving CreateCluster's InvalidParameterException as it is.

Traces: ec2__eks.ekscc-update-cluster-config-security-group-from-another-vpc-rejected.

## Evidence
Expected: Error.Code 'InvalidRequestException' (HTTP 400) with message 'Security group(s) [<sg ids>] are not (in/associated to) the same VPC as the subnets. Please specify a security group that is associated with the VPC: <cluster vpc id>'.

Actual (replay): AssertionError: UpdateClusterConfig must refuse a security group from another VPC with InvalidRequestException (CreateCluster uses InvalidParameterException for the same check); got 'InvalidParameterException': 'Security group(s) [sg-...0a6] are not (in/associated to) the same VPC as the subnets. Please specify a security group that is associated with the VPC: vpc-...08d'. The sg/vpc ids are per-run values; the code is the divergence.

## Steps to Reproduce
1. CreateCluster with subnets from VPC A (reaching ACTIVE).
2. Create a security group in a different VPC B.
3. UpdateClusterConfig(name=<cluster>, resourcesVpcConfig={'securityGroupIds': [<sg in VPC B>]}) and read Error.Code: it is InvalidParameterException where AWS returns InvalidRequestException.
4. DescribeCluster shows the security groups unchanged (as on AWS).
