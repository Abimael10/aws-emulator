Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2__eks__iam.ekscc-create-cluster-with-nonexistent-security-group-rejected
Issue-Key: ec2__eks__iam.ekscc-create-cluster-with-nonexistent-security-group-rejected|_error#8|AssertionError|raise AssertionError("the call SUCCEEDED; real AWS rejects it, so there is no error code")
Issue-Commit: 384e9c06f8b850c3e6c1129231f41c38ae5339ba

# ec2__eks__iam: EKS CreateCluster accepts a well-formed but nonexistent additional securityGroupId instead of refusing it with InvalidParameterException

> _Note: this issue was identified against the code base at commit `384e9c06f8b850c3e6c1129231f41c38ae5339ba`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateCluster

## Description
Cause 1 of 1: CreateCluster does not verify that the security groups named in resourcesVpcConfig.securityGroupIds exist.
AWS: Smithy — CreateCluster.resourcesVpcConfig (VpcConfigRequest).securityGroupIds names the security groups for the cross-account ENIs EKS creates; modeled errors include InvalidParameterException (400) and ResourceNotFoundException (404). Recorded behavior: a create naming a syntactically valid but nonexistent security group id is refused with InvalidParameterException and EKS's own message 'The security group '<securityGroupId>' does not exist' (fixed text "The security group '" + the id from the request + "' does not exist" — EKS's own sentence, not a relayed EC2 subnet-style text), and the refused create leaves no cluster, so DescribeCluster answers ResourceNotFoundException 'No cluster found for name: <name>.'. The identical call with no extra security group is accepted and starts CREATING in the subnets' VPC.
Emulator: the create with the unknown security group SUCCEEDS (the check reports 'the call SUCCEEDED; real AWS rejects it, so there is no error code'), so a cluster is created referencing a security group that does not exist.
Why: a client validating its network wiring through EKS gets a cluster the real service would never create; the misconfiguration is discovered only in a real account, and no client code path for the 400 refusal is ever exercised.
Fix: CreateCluster's resourcesVpcConfig validation in the eks service crate (services/service-com-amazonaws-eks-awswesleyfrontend/src/ops/create_cluster.rs) — resolve each securityGroupIds entry against EC2 state and refuse an unknown id with EKS's InvalidParameterException sentence, creating no cluster. Related but distinct open issues: 20261104T055008Z-eks-s3-createcluster-accepts-a-securitygroupid-that-belongs.md and 20261103T021956Z-ec2-eks-eks-createcluster-and-updateclusterconfig-accept-vpc-resources-that.md cover security groups/subnets that EXIST but sit in another VPC; neither covers an id that exists nowhere, which needs the existence check and a different message.
Traces: ec2__eks__iam.ekscc-create-cluster-with-nonexistent-security-group-rejected.

## Evidence
ec2__eks__iam.ekscc-create-cluster-with-nonexistent-security-group-rejected:
  AssertionError: the call SUCCEEDED; real AWS rejects it, so there is no error code

Expected: ('InvalidParameterException', "The security group '<sg-id>' does not exist") for CreateCluster with resourcesVpcConfig.securityGroupIds=[<an sg-id that DescribeSecurityGroups returns nothing for>], followed by DescribeCluster on that name answering ('ResourceNotFoundException', 'No cluster found for name: <name>.').
Actual: CreateCluster returned a cluster (no error), so the check could not even read an error code; the control create without the extra security group is accepted as expected.

## Steps to Reproduce
1. Pick a well-formed security group id that does not exist (ec2.describe_security_groups(Filters=[{'Name':'group-id','Values':[<id>]}])['SecurityGroups'] == []).
2. eks.create_cluster(name=<new>, roleArn=<EKS cluster role>, resourcesVpcConfig={'subnetIds': [two AZ-distinct subnets of one VPC], 'securityGroupIds': [<that id>]}).
3. Expect 400 InvalidParameterException "The security group '<id>' does not exist"; observed: the call succeeds and the cluster exists.
4. eks.describe_cluster(name=<new>) must answer ResourceNotFoundException 'No cluster found for name: <new>.' after the refusal.
