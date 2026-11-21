Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: batch__ecr__lambda__logs.refarch-batch-fargate-compute-environment-omits-vcpu-floors
Issue-Key: batch__ecr__lambda__logs.refarch-batch-fargate-compute-environment-omits-vcpu-floors|run#11|KeyError|assert est['ce']['serviceRole'].endswith('/AWSServiceRoleForBatch'), (
Issue-Commit: 02a4fcf1b47c41876a5d40faa140e945c54f7cde

# batch: DescribeComputeEnvironments omits serviceRole for a MANAGED compute environment created without one (Batch must fill in its service-linked role)

> _Note: this issue was identified against the code base at commit `02a4fcf1b47c41876a5d40faa140e945c54f7cde`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeComputeEnvironments

## Description
Cause 1 of 1: a MANAGED compute environment created with no `serviceRole` is stored and described with no serviceRole at all, instead of the Batch service-linked role AWS substitutes.

AWS: Smithy (com-amazonaws-batch-awsbatchv20160810) CreateComputeEnvironmentRequest.serviceRole documents: "If your account already created the Batch service-linked role, that role is used by default for your compute environment unless you specify a different role here. If the Batch service-linked role doesn't exist in your account, and no role is specified here, the service attempts to create the Batch service-linked role in your account. This automatic service-linked role creation only applies to MANAGED compute environments." ComputeEnvironmentDetail.serviceRole is "The service role that's associated with the compute environment that allows Batch to make calls to Amazon Web Services API operations on your behalf." So after CreateComputeEnvironment(type=MANAGED, computeResources.type=FARGATE, maxvCpus, subnets, securityGroupIds) with serviceRole omitted, DescribeComputeEnvironments returns a ComputeEnvironmentDetail whose serviceRole is PRESENT and is the service-linked role ARN, shaped arn:aws:iam::<account-id>:role/aws-service-role/batch.amazonaws.com/AWSServiceRoleForBatch — the fixed parts are the ARN partition/service/path and the final role name AWSServiceRoleForBatch; the account id is the caller's account, so it is a value from state, not a literal from any one run. CreateComputeEnvironment itself still answers 200 with computeEnvironmentName/computeEnvironmentArn.

Emulator: the described compute environment has no `serviceRole` key at all. The check reads est['ce']['serviceRole'] on the DescribeComputeEnvironments detail and raises KeyError: 'serviceRole'. The same describe response does carry the other derived control-plane fields the check already asserted (type=MANAGED, state=ENABLED, computeResources.type=FARGATE, maxvCpus=4, instanceTypes=[], ec2Configuration=[]), so this is specifically the service-linked-role default that is missing, not a wholesale describe failure.

Why: an operator or tool auditing which principal Batch assumes for a compute environment gets no answer — the trust story of the environment is invisible. Code that reads serviceRole to decide whether an environment uses the service-linked role or a customer role (IAM reviews, drift detection, CDK/Terraform refresh) sees an absent member and either crashes with a key error, as this check does, or concludes the environment has no service role, which is not a state real Batch can report for a MANAGED environment.

Fix: CreateComputeEnvironment's handler/state for MANAGED environments should default serviceRole to the account's Batch service-linked role ARN when the request omits it, and persist it on the compute environment so DescribeComputeEnvironments serializes the member. UNMANAGED environments must keep requiring an explicit serviceRole.

Note: this is NOT the already-open ecsClusterArn gap (20261104T041055Z-batch-ecs-describecomputeenvironments-reports-no-ecsclusterarn-for-a.md) — the check aborts on serviceRole before it ever reaches its ecsClusterArn assertion — nor the CloudFormation-side issue 20261109T123922Z-batch-cloudformation-iam-a-compute-environment-with-no.md, which is about a stack whose ServiceRole-less CE is accepted rather than about the describe representation.

Traces: batch__ecr__lambda__logs.refarch-batch-fargate-compute-environment-omits-vcpu-floors.

## Evidence
Expected (AWS): DescribeComputeEnvironments on a MANAGED/FARGATE environment created without serviceRole returns computeEnvironments[0].serviceRole present, ending in '/AWSServiceRoleForBatch' (full shape arn:aws:iam::<account-id>:role/aws-service-role/batch.amazonaws.com/AWSServiceRoleForBatch).

Actual (emulator): computeEnvironments[0] has no 'serviceRole' member. Replay output:
  File "check_batch__ecr__lambda__logs_refarch_batch_fargate_compute_environment_omits_vcpu_floors.py", line 42, in run
    assert est['ce']['serviceRole'].endswith('/AWSServiceRoleForBatch'), (
  KeyError: 'serviceRole'
The surrounding assertions on the same describe response passed (type=MANAGED, state=ENABLED, computeResources.type=FARGATE, maxvCpus=4, no minvCpus/desiredvCpus, instanceTypes=[], ec2Configuration=[]), so only the service-linked serviceRole default diverges.

## Steps to Reproduce
1. CreateComputeEnvironment(computeEnvironmentName=<unique>, type='MANAGED', state='ENABLED', computeResources={'type':'FARGATE','maxvCpus':4,'subnets':[<subnet ids>],'securityGroupIds':[<sg id>]}) — do NOT send serviceRole.
2. Poll DescribeComputeEnvironments(computeEnvironments=[<arn>]) until status is VALID.
3. Read computeEnvironments[0]['serviceRole'].
Expected: present, ending '/AWSServiceRoleForBatch'. Actual: the key is absent (KeyError).
Equivalently: run behavioral check batch__ecr__lambda__logs.refarch-batch-fargate-compute-environment-omits-vcpu-floors.
