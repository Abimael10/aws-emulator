Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: batch.deploy-compute-environment-cloudformation-behaves batch.e2e-fair-share-quota-lifecycle-affects-dispatch-and-teardown batch.service-job-managed-execution-reaches-succeeded
Issue-Shape-Id: com.amazonaws.batch#AWSBatchV20160810
Issue-Key: batch.deploy-compute-environment-cloudformation-behaves|run#14|AssertionError|assert stack["StackStatus"] == "CREATE_COMPLETE", f"stack did not reach CREATE_COMPLETE: {stack}"
Issue-Key: batch.e2e-fair-share-quota-lifecycle-affects-dispatch-and-teardown|_poll_until#58|Timeout|return last
Issue-Key: batch.service-job-managed-execution-reaches-succeeded|_delete_trial_components#34|Timeout|time.sleep(5)
Issue-Commit: e21d6e6f6ee0013c77eca32e21a13d15f059c07a

# batch: 2 net-new causes — a CloudFormation-deployed SAGEMAKER_TRAINING Batch estate rolls back on the JobQueue, and the SAGEMAKER_TRAINING service-job plane never converges

> _Note: this issue was identified against the code base at commit `e21d6e6f6ee0013c77eca32e21a13d15f059c07a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SubmitServiceJob

## Description
Two net-new root causes, both in the SAGEMAKER_TRAINING (service environment / quota share / service job) half of Batch. Neither failing check is marked ALREADY FILED or OWNED by the replay report.

Cause 1 of 2: a CloudFormation stack that declares the SAGEMAKER_TRAINING Batch estate fails the AWS::Batch::JobQueue create and rolls the whole stack back.
AWS: recorded behavior only for the CloudFormation path. A stack declaring AWS::Batch::SchedulingPolicy (QuotaSharePolicy.IdleResourceAssignmentStrategy=FIFO), AWS::Batch::ServiceEnvironment (ServiceEnvironmentType=SAGEMAKER_TRAINING with CapacityLimits), an AWS::Batch::JobQueue with JobQueueType=SAGEMAKER_TRAINING, Priority, SchedulingPolicyArn={Ref SP} and ServiceEnvironmentOrder=[{Order:1, ServiceEnvironment:{Ref SE}}], and an AWS::Batch::QuotaShare depending on that queue reaches StackStatus CREATE_COMPLETE, and the queue then reads back VALID/ENABLED through Batch. The underlying shape is modelled: Smithy (batch) CreateJobQueue takes jobQueueName, priority (required), state, schedulingPolicyArn, serviceEnvironmentOrder and jobQueueType, and JobQueueType's enum values are EKS | ECS | ECS_FARGATE | SAGEMAKER_TRAINING | ECS_MANAGED_INSTANCES; the only modelled client-side refusal is ClientException (400).
Emulator: the scheduling policy and service environment reach CREATE_COMPLETE, the JobQueue logical resource reaches CREATE_FAILED, every created resource is deleted, and the stack settles StackStatus='ROLLBACK_COMPLETE' with StackStatusReason 'The following resource(s) failed to create: [JQueue]. ' — the fixed text is 'The following resource(s) failed to create: [', then the failing logical id(s) from the template, then ']. '. The QuotaShare is never attempted.
Why: no template can stand up a SageMaker-training Batch estate at all. Because the stack rolls back, a client is left with nothing — no queue ARN in Outputs, no quota share, and no queue to submit a service job to — rather than with a partially wrong resource it could inspect.
Fix: the AWS::Batch::JobQueue resource handler's create path in the CloudFormation resource-provider layer, specifically JobQueueType=SAGEMAKER_TRAINING together with ServiceEnvironmentOrder and SchedulingPolicyArn resolved from Ref, and the AWS::Batch::QuotaShare handler that currently never receives a create attempt.
Related open issue (does NOT cover this check): 20261115T105344Z-batch-cloudformation-a-stack-declaring-a-sagemaker-training-aws-batch.md carries the same shape under the batch__cloudformation heading for check batch__cloudformation.deploy-service-environment-and-quota-share-lifecycle; it does not carry this check's replay key.
Traces: batch.deploy-compute-environment-cloudformation-behaves

Cause 2 of 2: a SAGEMAKER_TRAINING service job submitted to a SAGEMAKER_TRAINING job queue never converges, so every end-to-end service-job check hangs in its status poll until the replay window expires.
AWS: Smithy (batch) SubmitServiceJob accepts jobName, jobQueue, serviceJobType (ServiceJobType), serviceRequestPayload, quotaShareName, schedulingPriority, retryStrategy and timeoutConfig and answers 200 with jobId/jobArn; DescribeServiceJob reports status of enum ServiceJobStatus = SUBMITTED | PENDING | RUNNABLE | SCHEDULED | STARTING | RUNNING | SUCCEEDED | FAILED, plus startedAt/stoppedAt, attempts and latestAttempt. Recorded behavior: a submitted SAGEMAKER_TRAINING service job is orchestrated server-side onto the service environment's capacity and advances through RUNNING to a terminal SUCCEEDED (attempts carrying the SageMaker TrainingJobArn), and while one quota share's job holds the sole unit of capacity a second share's job stays in SUBMITTED/PENDING/RUNNABLE and is dispatched — without re-submission — as soon as TerminateServiceJob releases the unit; GetJobQueueSnapshot lists the withheld share under frontOfQuotaShares.quotaShares.
Emulator: neither check ever observes the required transition — both run out the replay window with the error 'Check timed out after 60.0s' while polling DescribeServiceJob (and, before it, DescribeServiceEnvironments/DescribeJobQueues/DescribeQuotaShare for status=='VALID'), so no service job is ever seen RUNNING, no terminal SUCCEEDED is ever reported, and startedAt/stoppedAt are never populated.
Why: the whole SAGEMAKER_TRAINING service-job surface is unusable. A client that submits a service job and waits on DescribeServiceJob waits forever: it can neither observe a successful training run nor observe capacity contention between quota shares, and preemption/teardown behavior built on terminal status can never be exercised.
Fix: the batch crate's service-job plane — the SubmitServiceJob handler and the scheduler/state transition that must move a service job from SUBMITTED through RUNNING to a terminal status against the service environment's capacityLimits and the quota share's reservation, plus the DescribeServiceJob serializer members (status, startedAt, stoppedAt, attempts/latestAttempt) that depend on it; also the status progression of CreateServiceEnvironment / CreateJobQueue / CreateQuotaShare to VALID that these polls gate on.
Traces: batch.e2e-fair-share-quota-lifecycle-affects-dispatch-and-teardown, batch.service-job-managed-execution-reaches-succeeded

## Evidence
Cause 1 — batch.deploy-compute-environment-cloudformation-behaves [mismatch]:
AssertionError: stack did not reach CREATE_COMPLETE: {... 'StackStatus': 'ROLLBACK_COMPLETE', 'StackStatusReason': 'The following resource(s) failed to create: [JQueue]. ' ...}
Expected (AWS): DescribeStacks reports StackStatus 'CREATE_COMPLETE' with the SchedulingPolicy, ServiceEnvironment, JobQueue (JobQueueType=SAGEMAKER_TRAINING) and QuotaShare all CREATE_COMPLETE.
Actual (emulator): the JobQueue resource is CREATE_FAILED, the QuotaShare is never attempted, and the stack is ROLLBACK_COMPLETE.

Cause 2 — batch.e2e-fair-share-quota-lifecycle-affects-dispatch-and-teardown [mismatch] and batch.service-job-managed-execution-reaches-succeeded [mismatch]:
Error: Check timed out after 60.0s
Expected (AWS): DescribeServiceJob on the submitted SAGEMAKER_TRAINING job reports status 'RUNNING' while it holds the service environment's sole capacity unit and finally status 'SUCCEEDED' with jobId echoed, a server-assigned createdAt, startedAt/stoppedAt longs and an attempt naming the SageMaker TrainingJobArn; a second quota share's job stays SUBMITTED/PENDING/RUNNABLE and appears under GetJobQueueSnapshot frontOfQuotaShares.quotaShares until TerminateServiceJob on the holder releases the unit, after which it is dispatched with no re-submission.
Actual (emulator): no such transition is ever observed — both checks exhaust the replay window inside their status polls, so the service job never reaches RUNNING or a terminal status.

## Steps to Reproduce
Cause 1:
1. CreateStack with a template containing SP: AWS::Batch::SchedulingPolicy {QuotaSharePolicy:{IdleResourceAssignmentStrategy: FIFO}}, SENV: AWS::Batch::ServiceEnvironment {ServiceEnvironmentType: SAGEMAKER_TRAINING, CapacityLimits:[{MaxCapacity, CapacityUnit}]}, JQueue: AWS::Batch::JobQueue {JobQueueType: SAGEMAKER_TRAINING, Priority: 1, SchedulingPolicyArn:{Ref SP}, ServiceEnvironmentOrder:[{Order:1, ServiceEnvironment:{Ref SENV}}]}, QS: AWS::Batch::QuotaShare {JobQueue:{Ref JQueue}, CapacityLimits, ResourceSharingConfiguration, PreemptionConfiguration}.
2. Poll DescribeStacks until terminal: the stack settles ROLLBACK_COMPLETE with StackStatusReason naming [JQueue] instead of CREATE_COMPLETE.
3. DescribeStackEvents and read the JQueue row's ResourceStatusReason for the handler's own message; note there is no QS create row.
4. Re-replay batch.deploy-compute-environment-cloudformation-behaves.

Cause 2:
1. CreateSchedulingPolicy with quotaSharePolicy={idleResourceAssignmentStrategy: FIFO}.
2. CreateServiceEnvironment with serviceEnvironmentType=SAGEMAKER_TRAINING and capacityLimits=[{maxCapacity:1, capacityUnit:<instance type>}]; poll DescribeServiceEnvironments for status VALID.
3. CreateJobQueue with jobQueueType=SAGEMAKER_TRAINING, priority=1, schedulingPolicyArn=<policy>, serviceEnvironmentOrder=[{order:1, serviceEnvironment:<env>}]; poll DescribeJobQueues for status VALID.
4. CreateQuotaShare against that queue; poll DescribeQuotaShare for status VALID.
5. SubmitServiceJob with serviceJobType=SAGEMAKER_TRAINING, quotaShareName=<share> and a SageMaker training serviceRequestPayload, then poll DescribeServiceJob: the status never reaches RUNNING or a terminal SUCCEEDED/FAILED, and startedAt/stoppedAt/attempts are never reported.
6. Submit a second job on a different quota share and call GetJobQueueSnapshot: the withheld share never appears under frontOfQuotaShares.quotaShares.
7. Re-replay batch.e2e-fair-share-quota-lifecycle-affects-dispatch-and-teardown and batch.service-job-managed-execution-reaches-succeeded.
