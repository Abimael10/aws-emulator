Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: batch__ecs.batch-propagate-tags-copies-definition-and-job-tags-to-ecs-task-job-tag-wins
Issue-Key: batch__ecs.batch-propagate-tags-copies-definition-and-job-tags-to-ecs-task-job-tag-wins|run#29|AssertionError|assert not described.get("failures"), (
Issue-Commit: 47cc4ef4df4e13f0c580047fd2f3a66085f63081

# batch__ecs: the ECS task a placed Batch job reports in attempts[].container.taskArn is unknown to ecs:DescribeTasks in the compute environment's cluster (failures reason MISSING), so propagateTags is never observable

> _Note: this issue was identified against the code base at commit `47cc4ef4df4e13f0c580047fd2f3a66085f63081`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeJobs

## Description
One net-new behavioral root cause, grounded in the single failing check of the batch__ecs service group (0/1 passed).

Cause 1 of 1 — the Batch job's ECS task ARN does not resolve in ECS.
AWS: Smithy com.amazonaws.batch#AttemptContainerDetail.taskArn is "The Amazon Resource Name (ARN) of the Amazon ECS task that's associated with the job attempt. Each container attempt receives a task ARN when they reach the STARTING status", and com.amazonaws.batch#JobDetail.propagateTags is "Specifies whether to propagate the tags from the job or job definition to the corresponding Amazon ECS task. ... For tags with the same name, job tags are given priority over job definitions tags." On AWS the ARN Batch publishes is a REAL ECS task in the compute environment's own ECS cluster: ecs:DescribeTasks(cluster=<ce ecsClusterArn>, tasks=[<that taskArn>], include=["TAGS"]) resolves it with an empty `failures` list, reports clusterArn equal to that cluster, and — when the job definition or the job sets propagateTags — carries the merged job-definition + job tags, the job's value winning on a shared key. A `failures` entry with reason MISSING is ECS's answer for a task ARN that does not exist in the named cluster, which is precisely what must NOT happen for an ARN Batch itself minted.
Emulator: Batch mints and publishes a task ARN of the form arn:aws:ecs:<region>:<account>:task/AWSBatch-<ce-name>-<id>/<32-hex> for the placed job, but that task is never registered in the ECS cluster's task store. DescribeTasks on the CE's cluster answers 200 with an empty `tasks` list and one `failures` entry per requested ARN, each {"arn": "<the taskArn Batch published>", "reason": "MISSING"} (fixed text: the literal reason "MISSING"; the variable part is the task ARN taken from the request). Because the task object does not exist, nothing carries the propagated tags either, so the propagateTags=true vs. twin-without-the-flag differential can never be read.
Why: a client that reads attempts[].container.taskArn (or container.taskArn) from DescribeJobs and then hands it to ECS — which is the documented way to inspect a Batch job's task, its tags, its network interfaces and its stop reason — gets MISSING for an ARN Batch just gave it. The Batch and ECS views of the same workload disagree, so tag-propagation audits, cost-allocation tagging and any ECS-side diagnosis of a Batch job are impossible; tooling cannot even tell a mis-propagated tag from a nonexistent task.
Fix: the Batch job placement path and its ECS integration — when a job is placed, the backing ECS task must be created in the compute environment's cluster (registered in ECS's task state, launchType FARGATE for a FARGATE CE, clusterArn = the CE's ecsClusterArn) and tagged with the merged job-definition/job tags when propagateTags is set, so the ARN Batch publishes resolves through ecs:DescribeTasks; the ARN must not be a free-standing string minted only inside the batch crate's state.
Traces: batch__ecs.batch-propagate-tags-copies-definition-and-job-tags-to-ecs-task-job-tag-wins.

## Evidence
Behavioral replay (simparity-behavioral-aws-v1) against this checkout.

[mismatch] batch__ecs.batch-propagate-tags-copies-definition-and-job-tags-to-ecs-task-job-tag-wins
AssertionError: ECS must resolve both task ARNs inside cluster 'arn:aws:ecs:<region>:<account>:cluster/AWSBatch-<ce-name>-<id>'; DescribeTasks reported failures [{'arn': 'arn:aws:ecs:<region>:<account>:task/AWSBatch-<ce-name>-<id>/<task-id-1>', 'reason': 'MISSING'}, {'arn': 'arn:aws:ecs:<region>:<account>:task/AWSBatch-<ce-name>-<id>/<task-id-2>', 'reason': 'MISSING'}]
(the cluster ARN, task ARNs and ids are per-run values; the requirement is the empty `failures` list, not these strings.)

The two ARNs came from DescribeJobs attempts[0].container.taskArn for two jobs the emulator had already placed, so Batch asserts the tasks exist while ECS asserts they do not.

Expected (AWS): DescribeTasks(cluster=<CE ecsClusterArn>, tasks=[both ARNs], include=["TAGS"]) -> 200 with failures=[] and two tasks, each clusterArn = the CE's cluster, and the task of the propagateTags=true leg carrying the job-definition tags plus the job tags with the job's value winning on the shared key; the twin leg's task carrying neither set.
Actual (emulator): 200 with tasks=[] and one failures entry per ARN with reason MISSING.

## Steps to Reproduce
1. Boot the emulator from this checkout.
2. Create an ECS-tasks execution role and a MANAGED FARGATE compute environment; wait for status VALID and note its ecsClusterArn.
3. Create an ENABLED job queue in front of that compute environment.
4. Register two FARGATE job definitions that differ only in propagateTags (one true, one unset), each with definition-level tags.
5. aws batch submit-job for each definition with job-level tags that share one key with the definition tags.
6. Poll aws batch describe-jobs --jobs <jobId> until attempts[0].container.taskArn is present; collect both task ARNs.
7. aws ecs describe-tasks --cluster <ecsClusterArn> --tasks <arn1> <arn2> --include TAGS.
   Observed: tasks=[] and failures=[{arn:<arn1>,reason:MISSING},{arn:<arn2>,reason:MISSING}].
   AWS: failures=[] and both tasks returned in that cluster, the propagating leg's task carrying the merged tags with the job tag winning.
8. Re-replay the check id in trace_ids.
