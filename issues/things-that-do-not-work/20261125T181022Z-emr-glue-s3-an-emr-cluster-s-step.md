Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: emr__glue__s3.cli-emr-cluster-terminate-s3-logs-persist-positive
Issue-Key: emr__glue__s3.cli-emr-cluster-terminate-s3-logs-persist-positive|run#23|Timeout|time.sleep(2)
Issue-Commit: 3ba50c32d06f7606ecc26210a28cec377b010e0f

# emr__glue__s3: an EMR cluster's step/cluster lifecycle and its S3 log flush never progress, so the terminate-and-logs-persist check reaches no verdict

> _Note: this issue was identified against the code base at commit `3ba50c32d06f7606ecc26210a28cec377b010e0f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeCluster

## Description
Cause 1 of 1: the EMR cluster lifecycle the check polls never reaches the states AWS reports, and no log object is ever flushed to the LogUri prefix, so every bounded poll in the check runs to the end of the run budget without a verdict.

AWS: Smithy (com-amazonaws-emr-elasticmapreduce) models StepState with values PENDING, CANCEL_PENDING, RUNNING, COMPLETED, CANCELLED, FAILED, INTERRUPTED, and ClusterState with STARTING, BOOTSTRAPPING, RUNNING, WAITING, TERMINATING, TERMINATED, TERMINATED_WITH_ERRORS. For a keep-alive cluster created with a single CUSTOM_JAR command-runner step, real AWS advances ListSteps[0].Status.State to COMPLETED, then settles DescribeCluster Cluster.Status.State at WAITING; after terminate-clusters it advances to TERMINATED. With --log-uri s3://<bucket>/logs/, EMR flushes cluster logs under logs/<ClusterId>/ in that bucket (periodically and on termination), and those objects remain readable after the cluster is terminated. Recorded behavior only for the flush timing: there is no promptness SLA, which is why the check polls rather than asserting immediately.

Emulator: the check ends with "Check timed out after 60.0s" — no state assertion is ever reached and no response is recorded for the step/cluster transition or for list_objects_v2 under logs/<ClusterId>/. Its first gate is a poll of ListSteps for a terminal StepState; nothing after that gate executed. Hypothesis: the submitted step is never run and the cluster never leaves its initial state, so the step never becomes COMPLETED, the cluster never reports WAITING or TERMINATED, and no log object is written under the LogUri prefix.

Why: a client that submits a step and waits on ListSteps/DescribeCluster — the documented way to drive an EMR cluster — waits forever instead of seeing COMPLETED/WAITING/TERMINATED, and a client that reads cluster logs from the LogUri after termination finds the prefix empty. Both the lifecycle contract and the "S3-persisted logs outlive the terminated cluster" guarantee are unobservable.

Fix: the EMR RunJobFlow/AddJobFlowSteps step runner and the cluster-state machine behind DescribeCluster/ListSteps must advance a submitted step to a terminal StepState and the keep-alive cluster through WAITING to TERMINATED on TerminateJobFlows, and the cluster's log delivery must write objects under the configured LogUri prefix (logs/<ClusterId>/) and leave them in place after termination.

Traces: the single failing check in trace_ids, under the emr__glue__s3 heading. Related open issues describe EMR steps not reaching terminal states for other scenarios (e.g. 20261105T074335Z-emr-glue-s3-emr-cluster-steps-never-reach.md, 20261108T165124Z-emr-glue-s3-an-emr-step-is-reported.md) but carry none of this check's replay key, and none covers the LogUri flush-and-persist half.

## Evidence
Expected (AWS): after RunJobFlow with a single CUSTOM_JAR command-runner step and --log-uri s3://<bucket>/logs/,
  - ListSteps(ClusterId).Steps[0].Status.State becomes COMPLETED;
  - DescribeCluster(ClusterId).Cluster.Status.State settles at WAITING for a keep-alive cluster;
  - after terminate-clusters it becomes TERMINATED;
  - list_objects_v2(Bucket, Prefix="logs/<ClusterId>/") returns KeyCount > 0, and still does on repeated reads over the following 30 s.

Actual (emulator): emr__glue__s3.cli-emr-cluster-terminate-s3-logs-persist-positive
  Error: Check timed out after 60.0s
The check's first gate is _poll_until(lambda: emr.list_steps(ClusterId=cluster_id)["Steps"], ... state in (COMPLETED, FAILED, CANCELLED, INTERRUPTED)); the budget is exhausted inside that bounded poll, so no terminal StepState was ever reported, and the later asserts on WAITING, TERMINATED and KeyCount > 0 never ran. The 60 s figure is this run's budget, a per-run value; the requirement is that the states advance and the log objects appear at all.

## Steps to Reproduce
1. Create an S3 bucket and the EMR service role and instance profile.
2. Run `aws emr create-cluster --release-label emr-6.15.0 --applications Name=Hadoop --instance-type m5.xlarge --instance-count 1 --log-uri s3://<bucket>/logs/ --steps '[{"Name":"...","Type":"CUSTOM_JAR","ActionOnFailure":"CONTINUE","Jar":"command-runner.jar","Args":["bash","-c","echo hello"]}]'` (keep-alive).
3. Poll ListSteps for the step's Status.State: it never becomes COMPLETED.
4. Poll DescribeCluster for Cluster.Status.State: it never settles at WAITING, and after `aws emr terminate-clusters` it never becomes TERMINATED.
5. List s3://<bucket>/logs/<ClusterId>/: KeyCount stays 0.
6. Re-replay the check id in trace_ids to confirm the fix.
