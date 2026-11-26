Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: emr.emr-run-step-spark
Issue-Shape-Id: com.amazonaws.emr#ElasticMapReduce
Issue-Key: emr.emr-run-step-spark|_poll_until#21|Timeout|return last
Issue-Commit: 36c52b4dca18359d29d13b5f35377729e4798d85

# emr: a spark-submit step on a KeepJobFlowAliveWhenNoSteps=false cluster never settles and the cluster never self-terminates with ALL_STEPS_COMPLETED, so emr-run-step-spark is cut off at the harness budget

> _Note: this issue was identified against the code base at commit `36c52b4dca18359d29d13b5f35377729e4798d85`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeStep

## Description
One net-new root cause, grounded in the single failing check of the ### emr heading that this report marks neither ALREADY FILED nor OWNED.

Cause 1 of 1: an accepted spark-submit step never leaves its initial state, produces no S3 side effect, and the auto-terminating cluster that carries it never shuts itself down (failing check: emr.emr-run-step-spark).

The check creates an emr-6.15.0 cluster with Applications=[Spark], one MASTER m5.xlarge in a private subnet and Instances.KeepJobFlowAliveWhenNoSteps=false, submitting one step through RunJobFlow.Steps whose HadoopJarStep is command-runner.jar with Args [spark-submit, --deploy-mode, client, s3://<bucket>/job/sum_squares.py, 100, s3://<bucket>/out]. It then polls, in order: (a) DescribeCluster until Status.State is RUNNING/WAITING/TERMINATED, (b) DescribeStep until Status.State is terminal, (c) ListObjectsV2 for the job's output under the out/ prefix, (d) DescribeCluster until TERMINATED, and finally asserts AddJobFlowSteps on the dead cluster is refused. It reaches none of its assertions: the runner kills it at the 60 s per-check budget while still polling, so at least one of these never advances. Every wait in the check is on an EMR state transition, never on wall-clock time, and 205 checks of this same suite complete inside the same budget against the same emulator, so the cut-off is a state that does not move, not slow provisioning.

AWS: Smithy (com.amazonaws.emr#ElasticMapReduce) models StepState = PENDING|CANCEL_PENDING|RUNNING|COMPLETED|CANCELLED|FAILED|INTERRUPTED, so a submitted step leaves PENDING on its own and settles in a terminal StepState; StepStatus carries StateChangeReason/FailureDetails/Timeline for that settlement. ClusterState = STARTING|BOOTSTRAPPING|RUNNING|WAITING|TERMINATING|TERMINATED|TERMINATED_WITH_ERRORS and ClusterStateChangeReasonCode includes ALL_STEPS_COMPLETED: a cluster whose Instances.KeepJobFlowAliveWhenNoSteps is false runs its steps and then self-terminates, reaching ClusterStatus.State=TERMINATED with StateChangeReason.Code=ALL_STEPS_COMPLETED, and leaves the live ListClusters ClusterStates (STARTING/BOOTSTRAPPING/RUNNING/WAITING) set. The data-plane effect is recorded behavior only: a spark-submit step that runs the script writes the job's part file and the Hadoop _SUCCESS marker under the output prefix the step names, and the part file holds exactly what the job computed. Once the cluster is terminated, AddJobFlowSteps against it is refused with HTTP 400, error code ValidationException and the message "A job flow that is shutting down, terminated, or finished may not be modified." — that sentence is fixed text with no variable parts.

Emulator: no polled condition is ever satisfied. The step is accepted but never reaches a terminal StepState, so the step's output objects (part file and _SUCCESS) are never written under the output prefix, and the KeepJobFlowAliveWhenNoSteps=false cluster never transitions to TERMINATED/ALL_STEPS_COMPLETED or drops out of the live cluster listing; the refusal of AddJobFlowSteps on a terminated cluster is never reached at all. Hypothesis: the step record is stored without any execution or state-advancement step behind DescribeStep, and the cluster state machine has no "last step done -> self-terminate" transition for KeepJobFlowAliveWhenNoSteps=false.

Why: a client that submits a step and waits for it — the only way to use EMR's step API — never receives a terminal status, so neither a success nor a failure path can terminate; it reads no output from S3 for a job it has no reason to believe did not run; and an auto-terminating cluster stays live forever, so cost-control and teardown logic keyed on TERMINATED/ALL_STEPS_COMPLETED never fires and ListClusters keeps reporting a cluster that should be gone.

Fix: the step runner behind AddJobFlowSteps/RunJobFlow.Steps and DescribeStep (state progression into a terminal StepState plus the step's S3 side effect), and the cluster state machine's self-termination path for Instances.KeepJobFlowAliveWhenNoSteps=false, including StateChangeReason.Code=ALL_STEPS_COMPLETED and removal from the live ListClusters states; the terminated-cluster refusal in the AddJobFlowSteps handler's validation is behind it.

Related but not covering: the open issue 20261110T142808Z-emr-3-net-new-root-causes-the-spark.md describes this same scenario, and 20261029T184212Z-emr-clusters-accept-steps-but-never-execute-them.md and 20261118T213929Z-emr-cluster-steps-never-execute-their-command-every.md describe neighbouring step-execution gaps, but this report does not mark emr.emr-run-step-spark as OWNED by any of them, so its replay key is not carried by an open issue.

Traces: emr.emr-run-step-spark ("Check timed out after 60.0s").

## Evidence
Suite report (behavioral, simparity-behavioral-aws-v1), service heading `### emr`:

  #### [mismatch] emr.emr-run-step-spark
  Error: Check timed out after 60.0s

This is the only failing check in the whole report carrying no ALREADY FILED and no OWNED marker. Overall run: 205/283 passed; emr 169/220 with 51 failures, all others marked already filed or owned.

Expected (AWS): inside the budget, DescribeStep moves the submitted spark-submit step out of PENDING to a terminal StepState (Smithy StepState = PENDING|CANCEL_PENDING|RUNNING|COMPLETED|CANCELLED|FAILED|INTERRUPTED) and reports COMPLETED for a job that succeeds; ListObjectsV2 on the step's output prefix shows a part-NNNNN object and a _SUCCESS marker, and the part object holds the value the job computed; DescribeCluster then reports Status.State=TERMINATED with Status.StateChangeReason.Code=ALL_STEPS_COMPLETED (Smithy ClusterStateChangeReasonCode) for Instances.KeepJobFlowAliveWhenNoSteps=false; the cluster id is absent from ListClusters(ClusterStates=[STARTING,BOOTSTRAPPING,RUNNING,WAITING]); and AddJobFlowSteps on it then fails with HTTP 400, Error.Code=ValidationException and the exact message "A job flow that is shutting down, terminated, or finished may not be modified."

Actual (emulator): none of those conditions is ever observed — the check is killed at 60 s still polling, so the step stays in its non-terminal state, no output object or _SUCCESS marker appears under the output prefix, the cluster never reaches TERMINATED/ALL_STEPS_COMPLETED and never leaves the live cluster listing, and the AddJobFlowSteps refusal is never exercised. The emulator serves these reads from memory, and 205 checks of the same suite finish inside the same 60 s budget, so the timeout is a non-advancing state rather than slow provisioning.

## Steps to Reproduce
1. Create an IAM service role trusting elasticmapreduce.amazonaws.com with AmazonElasticMapReduceRole, and an EC2 role/instance profile trusting ec2.amazonaws.com with AmazonElasticMapReduceforEC2Role.
2. Create a bucket and PutObject a PySpark script at job/sum_squares.py that sums the squares 1..N over 4 partitions and saves the result as text to the output URI given as its second argument.
3. RunJobFlow with ReleaseLabel=emr-6.15.0, Applications=[{Name: Spark}], Instances={InstanceCount:1, MasterInstanceType:m5.xlarge, KeepJobFlowAliveWhenNoSteps:false, TerminationProtected:false, Ec2SubnetId:<subnet>}, ServiceRole/JobFlowRole from step 1, and Steps=[{Name:..., ActionOnFailure:CONTINUE, HadoopJarStep:{Jar:"command-runner.jar", Args:["spark-submit","--deploy-mode","client","s3://<bucket>/job/sum_squares.py","100","s3://<bucket>/out"]}}].
4. ListSteps(ClusterId) to take the step id, then poll DescribeCluster until Status.State is RUNNING/WAITING/TERMINATED.
5. Poll DescribeStep(ClusterId, StepId): it never reports a terminal StepState.
6. ListObjectsV2(Bucket, Prefix="out/"): no part-* object and no _SUCCESS marker ever appear.
7. Poll DescribeCluster: Status.State never becomes TERMINATED with StateChangeReason.Code=ALL_STEPS_COMPLETED, and the cluster id stays in ListClusters(ClusterStates=[STARTING,BOOTSTRAPPING,RUNNING,WAITING]).
8. AddJobFlowSteps(JobFlowId=<cluster>, Steps=[<same step>]) once the cluster should be terminated: the expected HTTP 400 / ValidationException / "A job flow that is shutting down, terminated, or finished may not be modified." is never reachable because the cluster never terminates.
