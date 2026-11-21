Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: batch__ecr__lambda__logs.refarch-batch-fargate-compute-environment-omits-vcpu-floors
Issue-Key: batch__ecr__lambda__logs.refarch-batch-fargate-compute-environment-omits-vcpu-floors|run#13|KeyError|assert est['jq']['jobQueueType'] == 'ECS_FARGATE', (
Issue-Commit: ef5208a2316f6e603f33dcb5e1ac1bc1a8b9d2da

# batch: DescribeJobQueues omits jobQueueType, so a queue over a FARGATE compute environment never reports ECS_FARGATE

> _Note: this issue was identified against the code base at commit `ef5208a2316f6e603f33dcb5e1ac1bc1a8b9d2da`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeJobQueues

## Description
Cause 1 of 1: a job queue read back from DescribeJobQueues carries no jobQueueType member at all.
AWS: Smithy: JobQueueDetail.jobQueueType targets the JobQueueType enum and is documented as "The type of job queue. For service jobs that run on SageMaker Training, this value is SAGEMAKER_TRAINING. For regular container jobs, this value is EKS, ECS, or ECS_FARGATE depending on the compute environment." It is not an input of CreateJobQueue — AWS DERIVES it from the type of the compute environments named in computeEnvironmentOrder — so for a queue whose single compute environment is MANAGED with computeResources.type=FARGATE, DescribeJobQueues reports jobQueueType=ECS_FARGATE.
Emulator: DescribeJobQueues returns the queue (status VALID, state ENABLED, priority and computeEnvironmentOrder as sent) with the jobQueueType member absent, so reading it raises KeyError in a client. The derivation from the attached compute environment's type is simply not performed.
Why: jobQueueType is how a client or operator tells an ECS, ECS_FARGATE, EKS or SAGEMAKER_TRAINING queue apart without walking every attached compute environment. A tool that branches on it (a console-like view, a submit path that picks a queue for a Fargate job definition, a validator that refuses to mix orchestration types) either crashes on the missing key or falls back to a wrong default, and a SAGEMAKER_TRAINING service queue becomes indistinguishable from a container queue.
Fix: the DescribeJobQueues projection / job-queue state in the Batch service — populate jobQueueType when the queue is created or read by deriving it from the orchestration type of the compute environments in computeEnvironmentOrder (FARGATE/FARGATE_SPOT -> ECS_FARGATE, EC2/SPOT -> ECS, EKS environments -> EKS, a service environment -> SAGEMAKER_TRAINING).
Traces: batch__ecr__lambda__logs.refarch-batch-fargate-compute-environment-omits-vcpu-floors.

## Evidence
Check batch__ecr__lambda__logs.refarch-batch-fargate-compute-environment-omits-vcpu-floors, at `assert est['jq']['jobQueueType'] == 'ECS_FARGATE'`:

  KeyError: 'jobQueueType'

The queue was created with CreateJobQueue(priority=1, state='ENABLED', computeEnvironmentOrder=[{'order': 1, 'computeEnvironment': <MANAGED FARGATE CE arn>}]) and read back with DescribeJobQueues(jobQueues=[<arn>]) after it settled to status VALID; expected jobQueueType='ECS_FARGATE', got no such key in the jobQueues[0] structure. Queue/CE names and ARNs are per-run values. Every assertion before this one passed, so the compute-environment side of the read-back is not implicated.

## Steps to Reproduce
1. batch.create_compute_environment(computeEnvironmentName=CE, type='MANAGED', state='ENABLED', computeResources={'type':'FARGATE','maxvCpus':4,'subnets':[...],'securityGroupIds':[...]}) and wait for status VALID.
2. batch.create_job_queue(jobQueueName=JQ, priority=1, state='ENABLED', computeEnvironmentOrder=[{'order':1,'computeEnvironment':<CE arn>}]) and wait for status VALID.
3. batch.describe_job_queues(jobQueues=[<JQ arn>])['jobQueues'][0] — AWS reports jobQueueType='ECS_FARGATE'; the emulator returns no jobQueueType member.
