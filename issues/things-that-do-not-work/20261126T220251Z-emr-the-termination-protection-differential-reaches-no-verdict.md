Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: emr.termination-protection-blocks-terminate-job-flows
Issue-Shape-Id: com.amazonaws.emr#ElasticMapReduce
Issue-Key: emr.termination-protection-blocks-terminate-job-flows|_held#12|Timeout|time.sleep(interval_s)
Issue-Commit: 1b03d2a36dce0f217fa0fa1a851fd548a0788911

# emr: the termination-protection differential reaches no verdict — the protected-cluster TerminateJobFlows scenario never settles within the harness budget

> _Note: this issue was identified against the code base at commit `1b03d2a36dce0f217fa0fa1a851fd548a0788911`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
TerminateJobFlows

## Description
Cause 1 of 1: the whole termination-protection lifecycle differential (protect a WAITING cluster, ask to terminate it, prove the cluster did NOT go terminal, clear protection, terminate for real) never completes against the emulator: the check is cut off at the harness time budget (`Check timed out after 60.0s`) with no assertion reached, so none of the three graded properties below is ever observed. Every other emr failure in this run is already filed/owned; this is the only surviving one.

AWS: `TerminateJobFlows` and `SetTerminationProtection` model no client error (Smithy: TerminateJobFlows.JobFlowIds XmlStringList @required, only error InternalServerError/500; SetTerminationProtection.JobFlowIds + TerminationProtected @required, only error InternalServerError/500), so the behavior below is recorded behavior only, as the check encodes it:
  (a) `SetTerminationProtection(JobFlowIds=[id], TerminationProtected=true)` must make `DescribeCluster.Cluster.TerminationProtected` read `true`;
  (b) `TerminateJobFlows` on that cluster must shut nothing down. If it is refused, the refusal uses one of EMR's cluster-validation codes — `ValidationException` or `InvalidRequestException` — with the message, whose text is entirely FIXED (no request- or state-derived parts, and plural because the whole batch is judged at once): `Could not shut down one or more job flows since they are termination protected.` The cluster must stay `WAITING` for the whole observation window, `TerminationProtected` must still be `true` afterwards, and `Status.StateChangeReason.Code` must NOT become `USER_REQUEST`;
  (c) after `SetTerminationProtection(... TerminationProtected=false)` the IDENTICAL `TerminateJobFlows` call must succeed and the cluster must reach `TERMINATED` with `Status.StateChangeReason` `Code=USER_REQUEST`, `Message=Terminated by user request` (both fixed strings). A cluster carrying `AutoTerminationPolicy.IdleTimeout` must not be auto-terminated while protection holds.

Emulator: the replay never gets far enough to grade any of (a)-(c). One of the scenario's waits never settles, so the check is killed by the harness: provisioning to `WAITING`, the `TerminationProtected=true` round-trip through `DescribeCluster`, or the control termination reaching a terminal state. The two handlers do contain protection logic (ops/terminate_job_flows.rs refuses on `termination_protection_holds()` via `crate::state::termination_protected_message()`; ops/set_termination_protection.rs writes `cluster.instances.termination_protected` and records `termination_unlocked_at`), so Hypothesis: the stall is in the cluster/termination clock in `state.rs` — a cluster that is protected, or one asked to terminate right after being unprotected, is never advanced to the next state the poll waits for — rather than in the refusal branch itself. That is a hypothesis, not something the replay output states; the one fact the output establishes is that the scenario produces no verdict.

Why: a client that relies on termination protection gets no usable answer here. If the protected-cluster path hangs, callers (and the suite's own teardown, which clears protection and then terminates) block indefinitely on a cluster that never settles; and because the differential is never graded, a protected cluster could in principle be shut down — exactly the data-loss the flag exists to prevent — or an unprotected one could become un-terminable, and neither would be caught.

Fix: the EMR cluster lifecycle/termination clock in the service state (`crate::state` cluster state advance: `begin_termination`, `termination_protection_holds`, `termination_unlocked_at`, and the auto-termination idle timer) plus the two operation handlers that read/write it — `TerminateJobFlows` and `SetTerminationProtection` — and `DescribeCluster`'s reporting of `TerminationProtected` and `Status.StateChangeReason`. Every wait in the scenario must settle: protect -> describe reports true; terminate -> refused with the fixed message above and no state change; unprotect -> terminate -> `TERMINATED` with `USER_REQUEST` / `Terminated by user request`.

Traces: emr.termination-protection-blocks-terminate-job-flows (mismatch, `Check timed out after 60.0s`). Related but NOT covering it: 20261123T183657Z-emr-3-net-new-causes-a-core-instance.md and 20261120T034602Z-emr-a-spark-submit-step-on-a-keepjobflowalivewhennosteps.md describe other non-settling EMR lifecycle paths (instance-group resize, step-driven self-termination); neither carries this check's replay key.

## Evidence
Failing check: `emr.termination-protection-blocks-terminate-job-flows` — verdict `mismatch`, detail `Error: Check timed out after 60.0s`. No assertion in the check body was reached, so the emulator produced NO verdict for any of: (1) `DescribeCluster.Cluster.TerminationProtected == true` after `SetTerminationProtection(TerminationProtected=true)`; (2) `TerminateJobFlows` being refused with code in {ValidationException, InvalidRequestException} and message exactly `Could not shut down one or more job flows since they are termination protected.`, the cluster holding `WAITING` for the observation window, `TerminationProtected` still `true`, and no `USER_REQUEST` StateChangeReason; (3) after clearing protection, `TerminateJobFlows` reaching `TERMINATED` with `StateChangeReason.Code == USER_REQUEST` and `StateChangeReason.Message == Terminated by user request`.
Expected (AWS): all of (1)-(3) hold and the scenario completes.
Actual (emulator): the scenario hangs on one of its waits and is cut off at the harness budget; cluster id, role names, subnet id and timestamps are per-run values and are not part of the requirement.

## Steps to Reproduce
1. Create an EMR service role (trust elasticmapreduce.amazonaws.com, AmazonElasticMapReduceRole) and an EC2 role + instance profile (trust ec2.amazonaws.com, AmazonElasticMapReduceforEC2Role).
2. `RunJobFlow` a 1-node MASTER-only cluster (ReleaseLabel emr-6.15.0, m5.xlarge, Applications=[Hadoop], KeepJobFlowAliveWhenNoSteps=true, TerminationProtected=false, AutoTerminationPolicy={IdleTimeout:60}) in a private subnet, and poll `DescribeCluster` until the cluster is `WAITING`.
3. `SetTerminationProtection(JobFlowIds=[cluster], TerminationProtected=true)`; poll `DescribeCluster` until `Cluster.TerminationProtected` is `true`.
4. Call `TerminateJobFlows(JobFlowIds=[cluster])`. Expect either success-with-no-effect or a refusal with code `ValidationException`/`InvalidRequestException` and message `Could not shut down one or more job flows since they are termination protected.`
5. Observe `DescribeCluster` repeatedly for the observation window: state must stay `WAITING`, `TerminationProtected` must stay `true`, and `Status.StateChangeReason.Code` must not be `USER_REQUEST`.
6. `SetTerminationProtection(..., TerminationProtected=false)`, then `TerminateJobFlows` again and poll to `TERMINATED` with `StateChangeReason` `USER_REQUEST` / `Terminated by user request`.
Observed: the sequence never completes — one of the waits in steps 2-6 never settles and the check is killed at the harness budget (60 s), so no step past the stall is graded.
