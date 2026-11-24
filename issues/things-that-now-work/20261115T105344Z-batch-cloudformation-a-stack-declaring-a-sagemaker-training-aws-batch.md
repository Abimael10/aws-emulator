Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: batch__cloudformation.deploy-service-environment-and-quota-share-lifecycle
Issue-Key: batch__cloudformation.deploy-service-environment-and-quota-share-lifecycle|run#7|AssertionError|assert status == "CREATE_COMPLETE", (
Issue-Commit: f7526cbca79906f89a28c5d197946d676b42fe8d

# A stack declaring a SAGEMAKER_TRAINING AWS::Batch::JobQueue fails the JobQueue create and rolls the whole Batch estate back

> _Note: this issue was identified against the code base at commit `f7526cbca79906f89a28c5d197946d676b42fe8d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
One net-new cause.

Cause 1 of 1: AWS::Batch::JobQueue of JobQueueType SAGEMAKER_TRAINING cannot be provisioned, so a stack that also declares ::SchedulingPolicy, ::ServiceEnvironment and ::QuotaShare never reaches CREATE_COMPLETE.

AWS: recorded behavior only for the CloudFormation path — a stack declaring AWS::Batch::SchedulingPolicy (QuotaSharePolicy.IdleResourceAssignmentStrategy=FIFO), AWS::Batch::ServiceEnvironment (ServiceEnvironmentType=SAGEMAKER_TRAINING, CapacityLimits), an AWS::Batch::JobQueue with JobQueueType=SAGEMAKER_TRAINING, Priority, SchedulingPolicyArn={Ref SP} and ServiceEnvironmentOrder=[{Order:1, ServiceEnvironment:{Ref SE}}], and an AWS::Batch::QuotaShare depending on that queue reaches CREATE_COMPLETE, and every resource then reads back through Batch. The shape is modelled: Smithy (batch) CreateJobQueue accepts jobQueueName, priority, state, schedulingPolicyArn, serviceEnvironmentOrder and jobQueueType, JobQueueType's enum values are EKS | ECS | ECS_FARGATE | SAGEMAKER_TRAINING | ECS_MANAGED_INSTANCES, ServiceEnvironmentOrder requires order and serviceEnvironment (name or ARN), and CreateQuotaShare accepts quotaShareName, jobQueue, capacityLimits, resourceSharingConfiguration and preemptionConfiguration; the only client-side refusal modelled is ClientException (400).

Emulator: the SchedulingPolicy and ServiceEnvironment reach CREATE_COMPLETE, then the JobQueue reaches CREATE_FAILED and the stack rolls everything back; the QuotaShare is never attempted at all. The decisive event rows are [('SP','CREATE_COMPLETE'), ('SE','CREATE_COMPLETE'), ('JQ','CREATE_FAILED'), ('JQ','DELETE_COMPLETE'), ('SE','DELETE_COMPLETE'), ('SP','DELETE_COMPLETE'), (<stack>,'ROLLBACK_COMPLETE')] and the stack's terminal status is ROLLBACK_COMPLETE instead of CREATE_COMPLETE.

Why: no template can stand up a SageMaker-training Batch estate. Everything downstream of the queue — the quota share, job submission, and any client that reads the queue's ARN out of stack Outputs — is unreachable, and because the stack rolls back the caller is left with no resources at all rather than with a partially wrong one.

Note: this check is also listed in the open issue 20261109T123922Z-batch-cloudformation-2-net-new-causes-cloudformation-created.md, but that issue's key for it is the much later GetAtt/Ref ARN assertion (`assert (out["SeArnAtt"], out["QsArnAtt"]) == (se_arn, qs_arn)`), which the check can only reach from a CREATE_COMPLETE stack. The create-time failure described here happens before that and is a distinct cause that issue does not cover.

Fix: the AWS::Batch::JobQueue resource handler's create path — specifically its handling of JobQueueType=SAGEMAKER_TRAINING together with ServiceEnvironmentOrder and SchedulingPolicyArn resolved from Ref — and, once the queue creates, the AWS::Batch::QuotaShare handler that currently never gets a create attempt.

Traces: batch__cloudformation.deploy-service-environment-and-quota-share-lifecycle

## Evidence
batch__cloudformation.deploy-service-environment-and-quota-share-lifecycle — AssertionError: stack ended 'ROLLBACK_COMPLETE': [('SP', 'CREATE_COMPLETE'), ('SE', 'CREATE_COMPLETE'), ('JQ', 'CREATE_FAILED'), ('JQ', 'DELETE_COMPLETE'), ('SE', 'DELETE_COMPLETE'), ('SP', 'DELETE_COMPLETE'), ('<stack-name>', 'ROLLBACK_COMPLETE')]

Expected (AWS): stack status CREATE_COMPLETE with SP, SE, JQ and QS all CREATE_COMPLETE.
Actual (emulator): JQ CREATE_FAILED, QS never attempted, stack ROLLBACK_COMPLETE.

## Steps to Reproduce
1. CreateStack with a template containing:
   - SP: AWS::Batch::SchedulingPolicy {Name, QuotaSharePolicy:{IdleResourceAssignmentStrategy: FIFO}}
   - SE: AWS::Batch::ServiceEnvironment {ServiceEnvironmentName, ServiceEnvironmentType: SAGEMAKER_TRAINING, CapacityLimits:[{MaxCapacity:2, CapacityUnit:"ml.m6i.large"}]}
   - JQ: AWS::Batch::JobQueue {JobQueueName, JobQueueType: SAGEMAKER_TRAINING, Priority:1, SchedulingPolicyArn:{Ref:SP}, ServiceEnvironmentOrder:[{Order:1, ServiceEnvironment:{Ref:SE}}]} DependsOn [SE, SP]
   - QS: AWS::Batch::QuotaShare {QuotaShareName, JobQueue:{Ref:JQ}, CapacityLimits:[{MaxCapacity:1, CapacityUnit:"ml.m6i.large"}], ResourceSharingConfiguration:{Strategy:RESERVE}, PreemptionConfiguration:{InSharePreemption:DISABLED}} DependsOn [JQ]
2. Poll DescribeStacks until terminal — the stack settles ROLLBACK_COMPLETE.
3. DescribeStackEvents: the JQ row is CREATE_FAILED and there is no QS create row; read its ResourceStatusReason for the handler's own message.
4. Re-replay batch__cloudformation.deploy-service-environment-and-quota-share-lifecycle.
