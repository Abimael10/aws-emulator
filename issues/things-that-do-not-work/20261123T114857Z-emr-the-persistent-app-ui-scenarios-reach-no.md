Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: emr.persistent-app-ui-creation-then-describe-eventually-ready emr.persistent-app-ui-describe-survives-cluster-termination-window
Issue-Shape-Id: com.amazonaws.emr#ElasticMapReduce
Issue-Key: emr.persistent-app-ui-creation-then-describe-eventually-ready|_poll_until#15|Timeout|return last
Issue-Key: emr.persistent-app-ui-describe-survives-cluster-termination-window|_poll_until#13|Timeout|return last
Issue-Commit: cd8f73d475d74adb0ca8a470eb19f8c740cf481a

# emr: the persistent-app-UI scenarios reach no verdict — neither the PersistentAppUIStatus transition past its initial value nor a UI's survival of its cluster's termination is observable inside a client's polling budget

> _Note: this issue was identified against the code base at commit `cd8f73d475d74adb0ca8a470eb19f8c740cf481a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribePersistentAppUI

## Description
Cause 1 of 1: the persistent application user interface lifecycle cannot be driven to a ready state, because the prerequisite every one of its readiness rules is written against — a Spark application that has actually run on the cluster — never completes, so both persistent-app-UI scenarios are cut off with no verdict at all.

AWS:      Smithy (com.amazonaws.emr#ElasticMapReduce) CreatePersistentAppUI returns PersistentAppUIId; DescribePersistentAppUI answers PersistentAppUI with PersistentAppUIStatus (XmlStringMaxLen256, "The status for the persistent application user interface object"), LastModifiedTime and LastStateChangeReason; GetPersistentAppUIPresignedURL answers PresignedURLReady (Boolean) and PresignedURL (XmlString, len 0..10280). Recorded behavior for the lifecycle the model does not spell out: on a cluster that has run a Spark application (a command-runner.jar `spark-submit --class org.apache.spark.examples.SparkPi` step that reaches StepState COMPLETED), CreatePersistentAppUI(TargetResourceArn=<cluster arn>) is followed by DescribePersistentAppUI LEAVING its initial status for an attached/ready one within a short bounded window, and GetPersistentAppUIPresignedURL(PersistentAppUIId, PersistentAppUIType="SHS") then answers PresignedURLReady=true with a non-empty PresignedURL. The UI is persistent off-cluster: after TerminateJobFlows and the cluster settling in TERMINATED, repeated DescribePersistentAppUI calls keep answering 200 with the same PersistentAppUIId rather than refusing it with InvalidRequestException.
Emulator: neither scenario reaches any of those assertions. Both checks are killed by the harness at their budget — "Check timed out after 60.0s" — while still waiting on the prerequisites they share (DescribeCluster reaching WAITING/RUNNING, then DescribeStep on the SparkPi step reaching a terminal COMPLETED). The replay output carries no further detail than that timeout, so the stall is located at the shared prerequisite rather than at a specific app-UI call. Hypothesis: it is the EMR step runner never settling a spark-submit step as COMPLETED (the engine named in 20261119T185618Z-emr-6-net-new-root-causes-in-the.md, 20261120T034602Z-emr-a-spark-submit-step-on-a-keepjobflowalivewhennosteps.md and 20261118T213929Z-emr-cluster-steps-never-execute-their-command-every.md); none of those issues carries this scenario's replay keys or its persistent-app-UI requirements, and 20261116T232636Z-emr-3-net-new-causes-app-ui-presigned.md covers only the URL's missing expiry, so the two checks here are net-new work against that engine plus the persistent-app-UI status clock.
Why:       a client that opens a Spark History Server through a persistent app UI has no observable path to a usable URL: it polls DescribePersistentAppUI and GetPersistentAppUIPresignedURL forever, with no state transition and no refusal to branch on, and it cannot establish the one property the resource exists for — that the UI outlives the cluster whose history it serves. Any tooling that waits for readiness hangs instead of failing, and a UI's post-termination describability (the reason it is called "persistent") is untestable.
Fix:       the EMR step execution engine behind AddJobFlowSteps/DescribeStep in the elasticmapreduce service crate (a spark-submit step must run its application and settle COMPLETED, so a cluster has a finished Spark application for the history server to publish), together with the persistent-app-UI status progression and attachment clock in that crate's state (the record minted by the CreatePersistentAppUI handler with status STARTING and an attachment due time, read by the DescribePersistentAppUI handler and gating PresignedURLReady in the GetPersistentAppUIPresignedURL handler) and the retention of that record past its cluster's termination.
Traces:    emr.persistent-app-ui-creation-then-describe-eventually-ready, emr.persistent-app-ui-describe-survives-cluster-termination-window.

## Evidence
Behavioral replay against the booted emulator, service heading `### emr` (suite simparity-behavioral-aws-v1).

Check emr.persistent-app-ui-creation-then-describe-eventually-ready [mismatch]:
  expected (AWS): cluster reaches WAITING/RUNNING; a SparkPi command-runner.jar step reaches COMPLETED; CreatePersistentAppUI(TargetResourceArn=<cluster arn>) -> PersistentAppUIId; DescribePersistentAppUI's PersistentAppUIStatus leaves its initial value and GetPersistentAppUIPresignedURL(PersistentAppUIId, PersistentAppUIType="SHS") answers PresignedURLReady=true with a non-empty PresignedURL.
  actual (emulator): Check timed out after 60.0s — no status transition and no ready URL is ever observed; the scenario never gets past waiting on the cluster/step prerequisite.

Check emr.persistent-app-ui-describe-survives-cluster-termination-window [mismatch]:
  expected (AWS): after the SparkPi step COMPLETEs and the UI is ready, TerminateJobFlows settles the cluster in TERMINATED and three successive DescribePersistentAppUI calls each answer 200 with the same PersistentAppUIId.
  actual (emulator): Check timed out after 60.0s — the terminated-cluster/persistent-UI pairing is never reached.

The 60 s figure is this suite's per-check budget and the cluster/step/UI identifiers in the recording are per-run values; the requirement is only that the readiness transition and the post-termination describability happen within a bounded window a client can poll for.

## Steps to Reproduce
1. Boot the emulator; create the EMR service role and EC2 instance profile; RunJobFlow a keep-alive cluster with Spark installed and poll DescribeCluster until WAITING/RUNNING.
2. AddJobFlowSteps one command-runner.jar step with Args ["spark-submit", "--class", "org.apache.spark.examples.SparkPi", "/usr/lib/spark/examples/jars/spark-examples.jar", "5"] and poll DescribeStep. Observed: the step does not settle COMPLETED inside the client's budget.
3. CreatePersistentAppUI(TargetResourceArn=<cluster arn>) and record the initial DescribePersistentAppUI PersistentAppUIStatus.
4. Poll DescribePersistentAppUI and GetPersistentAppUIPresignedURL(PersistentAppUIId, PersistentAppUIType="SHS"). Observed: the status never leaves its initial value together with PresignedURLReady=true; expected a bounded transition to a ready UI with a non-empty PresignedURL.
5. TerminateJobFlows the cluster, poll DescribeCluster to TERMINATED, then call DescribePersistentAppUI three times. Expected: each answers 200 with the same PersistentAppUIId; observed: unreachable, the scenario is killed at its budget first.
6. Re-replay the two checks in trace_ids to confirm the fix.
