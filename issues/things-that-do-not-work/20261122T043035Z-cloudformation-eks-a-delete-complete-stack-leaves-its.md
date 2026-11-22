Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__eks.deploy-cfn-cluster-single-az-subnets-rejected
Issue-Key: cloudformation__eks.deploy-cfn-cluster-single-az-subnets-rejected|expect_error#19|AssertionError|raise AssertionError(f"expected {code} but the call succeeded: {result!r}")
Issue-Commit: 622c214bd0a84106899ec4445dc3396f8892f16d

# cloudformation__eks: a DELETE_COMPLETE stack leaves its AWS::EKS::Cluster describable and stuck in 'DELETING', so DescribeCluster answers 200 instead of ResourceNotFoundException

> _Note: this issue was identified against the code base at commit `622c214bd0a84106899ec4445dc3396f8892f16d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeCluster

## Description
One net-new root cause: the AWS::EKS::Cluster delete path reports the stack as DELETE_COMPLETE while the cluster itself is only moved to 'DELETING' and is never removed from EKS state, so EKS keeps answering reads for a cluster the stack says is gone. Failing check: cloudformation__eks.deploy-cfn-cluster-single-az-subnets-rejected

AWS:      Smithy (eks): DescribeCluster GET /clusters/{name} -> 200 with `cluster`, and models ResourceNotFoundException with httpError 404. DeleteCluster DELETE /clusters/{name} is asynchronous — the cluster passes through 'DELETING' and then stops existing; once it is gone DescribeCluster answers 404 ResourceNotFoundException with Message `No cluster found for name: <cluster name>.` Fixed text: `No cluster found for name: ` and the trailing `.`; the variable part is the cluster name copied from the request. When CloudFormation reports a stack DELETE_COMPLETE, every resource the stack owned has finished deleting, so a read of the deleted cluster by name is already a 404 at that point.
Emulator: after DeleteStack settles DELETE_COMPLETE, DescribeCluster(name=<cluster>) still answers HTTP 200 with the full cluster envelope and `status` = 'DELETING'. The cluster never leaves DELETING and never becomes not-found.
Why:      the stack's DELETE_COMPLETE is not truthful: a client that deletes a stack and then recreates a cluster under the same name, or that polls DescribeCluster/ListClusters to confirm teardown, sees the old cluster forever. Teardown waiters never finish, name reuse collides with a tombstone that never clears, and absence can never be observed — DescribeCluster is a 200 where AWS has a 404 ResourceNotFoundException, so error-class branching ('gone' vs 'still there') always takes the wrong branch.
Fix:      the AWS::EKS::Cluster delete handler in CloudFormation's resource provider and the EKS DeleteCluster state transition it drives — the stack resource must not report DELETE_COMPLETE until the cluster is removed from EKS state, and a cluster that finished deleting must stop being returned by DescribeCluster/ListClusters.
Traces:   cloudformation__eks.deploy-cfn-cluster-single-az-subnets-rejected — the failure is on the check's positive control teardown (line 244), before the single-AZ rejection assertions are reached, so the single-AZ refusal itself is not graded by this run. The open issue 20261117T115837Z-cloudformation-eks-the-aws-eks-cluster-resource-handler.md covers the AWS::EKS::Cluster handler ignoring ComputeConfig and does not carry this check's replay key or this delete-completion cause.

## Evidence
Check cloudformation__eks.deploy-cfn-cluster-single-az-subnets-rejected [mismatch]

Sequence: CreateStack (IAM cluster role + AWS::EKS::Cluster with two subnets spanning two AZs) -> CREATE_COMPLETE -> DescribeCluster matches the stack outputs (status ACTIVE) -> delete_stack(...) reaches DELETE_COMPLETE -> DescribeCluster(name=<cluster>).

expected: ClientError ResourceNotFoundException, Message 'No cluster found for name: <cluster name>.'
actual:   HTTP 200, {'cluster': {'name': '<cluster name>', 'arn': 'arn:aws:eks:<region>:<account>:cluster/<cluster name>', ..., 'status': 'DELETING', ...}}

AssertionError at check line 244 via expect_error: "expected ResourceNotFoundException but the call succeeded". The cluster name, ARN, endpoint host/port, timestamps and certificate data in the actual envelope are per-run values and are not themselves the requirement; the requirement is that the read is a 404 ResourceNotFoundException once the stack is DELETE_COMPLETE.

## Steps to Reproduce
1. CreateStack (Capabilities=[CAPABILITY_IAM]) with an AWS::IAM::Role trusted by eks.amazonaws.com and an AWS::EKS::Cluster whose ResourcesVpcConfig.SubnetIds span two Availability Zones, EndpointPublicAccess=false, EndpointPrivateAccess=true.
2. Wait for CREATE_COMPLETE; eks.DescribeCluster(name=<cluster>) reports status ACTIVE (this part matches).
3. DeleteStack and poll DescribeStacks until DELETE_COMPLETE.
4. Call eks.DescribeCluster(name=<cluster>): the emulator answers 200 with status 'DELETING'; AWS answers 404 ResourceNotFoundException 'No cluster found for name: <cluster>.'. ListClusters likewise still lists it.
