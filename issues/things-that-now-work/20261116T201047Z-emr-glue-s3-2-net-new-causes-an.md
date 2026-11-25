Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: emr__glue__s3.emr-cluster-missing-s3-logs-permission-negative emr__glue__s3.cli-emr-cluster-terminate-s3-logs-persist-positive
Issue-Key: emr__glue__s3.emr-cluster-missing-s3-logs-permission-negative|run#20|AssertionError|assert not denied_found, (
Issue-Key: emr__glue__s3.cli-emr-cluster-terminate-s3-logs-persist-positive|run#23|Timeout|time.sleep(2)
Issue-Commit: f8633eecdaeae6109a5f103d84c11b9279b91d9e

# emr__glue__s3: 2 net-new causes — an EC2 instance role with an explicit Deny on s3:PutObject still gets its EMR cluster logs written under LogUri, and the CLI-submitted keep-alive LogUri cluster still never settles step+WAITING+log objects

> _Note: this issue was identified against the code base at commit `f8633eecdaeae6109a5f103d84c11b9279b91d9e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RunJobFlow

## Description
Two net-new root causes in the EMR cluster log-delivery path, both grounded in failing behavioral checks under the suite heading `### emr__glue__s3`. Neither check is marked ALREADY FILED and neither is marked OWNED by an open issue in this replay report, so neither replay key is carried by an existing issue.

=== Cause 1 of 2: EMR cluster log delivery to LogUri ignores the EC2 instance role's S3 permissions — an explicit Deny on s3:PutObject for the log prefix does not suppress the logs ===
Failing check: emr__glue__s3.emr-cluster-missing-s3-logs-permission-negative (verdict `mismatch`).
AWS: EMR writes a cluster's log bundle to `<LogUri>/<clusterId>/` using the EC2 instance profile the cluster runs under (Smithy com.amazonaws.elasticmapreduce#ElasticMapReduce: `RunJobFlow` input member `LogUri` (XmlString, length 0..10280) — "The location in Amazon S3 to write the log files of the job flow"; `Instances.KeepJobFlowAliveWhenNoSteps`; `DescribeCluster` reports `Cluster$Status.State` over `ClusterState`). The model says nothing about log upload authorization, so this is recorded behavior only: when the cluster's instance role carries an explicit `Deny` of `s3:PutObject` on `arn:aws:s3:::<bucket>/<denied-prefix>/*`, the log PUTs are refused by S3 and NO object ever appears under `<denied-prefix>/<clusterId>/`, while a sibling cluster of identical wiring whose instance role is unrestricted does get objects under its own prefix. An explicit Deny is terminal in IAM evaluation and cannot be overridden by any Allow, so the asymmetry is a requirement, not a timing artifact.
Emulator: the positive control now passes — the unrestricted cluster's logs DO appear under `logs-allowed/<clusterId>/` — but the DENIED cluster's logs appear too. The check fails at `check_emr__glue__s3_emr_cluster_missing_s3_logs_permission_negative.py` line 79 with `AssertionError: cluster with an instance role denied s3:PutObject on the log prefix still managed to persist logs there; the emulator is not enforcing the permission boundary`, after holding the negative window for at least 3x the positive control's own measured latency (so the window is sized from the run, never a fixed guess). The log-upload side effect is therefore performed unconditionally, without evaluating the instance role's S3 policy against the target prefix.
Why: a client cannot use the emulator to verify an EMR log-permission boundary, and gets the opposite verdict from AWS. Any test or control that asserts "a cluster whose instance role is denied s3:PutObject on the log prefix writes nothing there" passes against AWS and fails against the emulator; conversely a misconfigured deny looks harmless under the emulator and silently loses all cluster logs in production. Least-privilege reviews built on this signal are inverted.
Fix: the cluster log-delivery side effect of the RunJobFlow cluster state machine in the emr crate must authorize each log PUT as the cluster's EC2 instance profile (JobFlowRole / `Instances.Ec2InstanceProfile`) against the S3 object key it is writing, and skip the write when the effective policy denies `s3:PutObject` on that key — the same evaluation S3's own authorization applies, with an explicit Deny terminal. Note the earlier open issue 20261110T214521Z-emr-glue-s3-the-s3-log-permission-boundary.md describes the PREVIOUS state of this check (no logs uploaded at all, check killed at the harness cap); that state has changed — logs are now uploaded — so the surviving defect is the missing authorization, not the missing upload.
Traces: emr__glue__s3.emr-cluster-missing-s3-logs-permission-negative.

=== Cause 2 of 2: a CLI-created keep-alive cluster with a LogUri never presents a terminal step, a WAITING cluster and log objects, so the log-persistence property is unreachable ===
Failing check: emr__glue__s3.cli-emr-cluster-terminate-s3-logs-persist-positive (verdict `mismatch`; detail `Error: Check timed out after 60.0s`).
AWS: a keep-alive cluster (`Instances.KeepJobFlowAliveWhenNoSteps=true`) created with `LogUri=s3://<bucket>/logs/` runs its single submitted step to a terminal `StepState` — `ListSteps` reports `StepSummary$Status.State` over StepState (PENDING, CANCEL_PENDING, RUNNING, COMPLETED, CANCELLED, FAILED, INTERRUPTED), and a trivial `command-runner.jar` / `bash -c echo ...` step reaches COMPLETED — then settles in `WAITING` (`DescribeCluster` -> `Cluster$Status.State` over ClusterState: STARTING, BOOTSTRAPPING, RUNNING, WAITING, TERMINATING, TERMINATED, TERMINATED_WITH_ERRORS). `TerminateJobFlows` then drives it to `TERMINATED`, and the cluster's logs under `<LogUri>/<clusterId>/` OUTLIVE the terminated cluster: repeated `ListObjectsV2` on that prefix keeps returning a non-zero `KeyCount`. AWS states no promptness SLA for the log flush, which is why the client polls a bounded window rather than asserting immediately.
Emulator: the check reaches no comparison at all. Every wait in it is a bounded poll over an OBSERVABLE emulator response (ListSteps until the one step is terminal; DescribeCluster until the state leaves STARTING/BOOTSTRAPPING/RUNNING and equals WAITING; DescribeCluster after terminate-clusters until TERMINATED; ListObjectsV2 on `logs/<clusterId>/` until KeyCount > 0), and the only fixed-duration loop (the 30 s persistence re-read) is itself capped by `ctx.time_left()`, so it cannot be what overran. One of those polled conditions is never satisfied, so the client hangs instead of receiving either a verdict or an error to branch on. Hypothesis (the report shows only the timeout, not which poll): the submitted step never advances to a terminal state and/or the keep-alive cluster never settles in WAITING, so the log flush that follows is never reached. The 60 s figure is this environment's per-check budget and is not itself a requirement.
Why: this sequence IS the operational contract for EMR post-mortem — submit a step, wait for it to finish, terminate the cluster, then read its logs out of S3. Against the emulator such a client blocks on the first wait forever and the property it verifies (S3-persisted EMR logs survive the cluster that produced them) is unobservable.
Fix: the EMR step execution and cluster state machine plus the log-delivery side effect in the emr crate — a submitted step must advance to a terminal StepState, a keep-alive cluster with no pending steps must settle in WAITING and reach TERMINATED after TerminateJobFlows, and the cluster's logs must be written under `<LogUri>/<clusterId>/` and remain after termination. Related open issues that do NOT carry this check's current replay key: 20261113T102950Z-emr-glue-s3-the-emr-cluster-step-log.md, 20261108T104909Z-emr-glue-s3-2-net-new-causes-cluster.md, 20261105T074335Z-emr-glue-s3-emr-cluster-steps-never-reach.md and 20261029T184212Z-emr-clusters-accept-steps-but-never-execute-them.md; closing the step/log gaps they describe should also close this check, which is filed so this check id is re-replayed.
Traces: emr__glue__s3.cli-emr-cluster-terminate-s3-logs-persist-positive.

## Evidence
Suite report simparity-behavioral-aws-v1, service heading '### emr__glue__s3' (25/47 passed, 22 failed). The two checks below are the only failures of this service not marked ALREADY FILED and not marked OWNED by an open issue.

Cause 1 — [mismatch] emr__glue__s3.emr-cluster-missing-s3-logs-permission-negative, replay detail verbatim:
  File "check_emr__glue__s3_emr_cluster_missing_s3_logs_permission_negative.py", line 79, in run
    assert not denied_found, (
  AssertionError: cluster with an instance role denied s3:PutObject on the log prefix still managed to persist logs there; the emulator is not enforcing the permission boundary
Expected (AWS): with two clusters of identical wiring and the same trivial step, KeepJobFlowAliveWhenNoSteps=false — one under an unrestricted instance profile with LogUri=s3://<bucket>/logs-allowed/, one under a profile whose role carries {"Effect":"Deny","Action":"s3:PutObject","Resource":"arn:aws:s3:::<bucket>/logs-denied/*"} with LogUri=s3://<bucket>/logs-denied/ — ListObjectsV2 on logs-allowed/<clusterId>/ returns objects, and ListObjectsV2 on logs-denied/<clusterId>/ returns NONE for at least 3x the positive control's measured latency.
Actual (emulator): the positive control is satisfied (logs-allowed/<clusterId>/ has objects — this is a change from the previously recorded state of this check, where no logs were uploaded at all), and logs-denied/<clusterId>/ ALSO has objects. The log write is performed without authorizing it as the cluster's EC2 instance profile, so the explicit Deny has no effect.
The bucket name, cluster ids, role/profile names and the measured latency are per-run values and are not part of the requirement; the requirement is the asymmetry: denied prefix empty, allowed prefix non-empty.

Cause 2 — [mismatch] emr__glue__s3.cli-emr-cluster-terminate-s3-logs-persist-positive, replay detail verbatim:
  Error: Check timed out after 60.0s
What the client was waiting on, in order, all bounded polls over emulator responses:
  1. emr.list_steps(ClusterId=...) until the single step is COMPLETED/FAILED/CANCELLED/INTERRUPTED, then assert it is COMPLETED.
  2. emr.describe_cluster(...)["Cluster"]["Status"]["State"] until it leaves STARTING/BOOTSTRAPPING/RUNNING, then assert WAITING (a keep-alive cluster whose only step finished must be WAITING).
  3. `aws emr terminate-clusters --cluster-ids <id>`, then poll describe_cluster until TERMINATED.
  4. s3.list_objects_v2(Bucket=<bucket>, Prefix="logs/<clusterId>/") until KeyCount > 0, then the same non-zero count on every re-read for 30 s (that loop is itself capped by ctx.time_left()).
Expected: steps 1-4 are each answered and the run ends on the persistence assertion.
Actual: no verdict inside the harness budget — the sequence never advanced past the step/cluster-state polls. The `aws` CLI was present (require_tools("aws") did not skip the check) and the reads are served from memory, so neither a missing tool nor the budget is the constraint: 25 checks of this same service and budget completed.
Smithy citations used: RunJobFlow.LogUri (XmlString, 0..10280), RunJobFlow.Instances (JobFlowInstancesConfig, required), ListSteps -> StepSummaryList with StepSummary$Status.State over StepState, DescribeCluster -> Cluster$Status.State over ClusterState. Log-bundle layout and log authorization are recorded behavior only; the model does not describe them.

## Steps to Reproduce
Cause 1 (emr-cluster-missing-s3-logs-permission-negative):
1. CreateBucket. Create an EMR service role trusting elasticmapreduce.amazonaws.com; create two EC2 instance profiles whose roles trust ec2.amazonaws.com — one unrestricted, one carrying an inline policy {"Effect":"Deny","Action":"s3:PutObject","Resource":"arn:aws:s3:::<bucket>/logs-denied/*"}.
2. RunJobFlow twice with the same trivial step (command-runner.jar, ["bash","-c","echo emr-permission-check"]) and Instances.KeepJobFlowAliveWhenNoSteps=false: one with the denied profile and LogUri=s3://<bucket>/logs-denied/, one with the unrestricted profile and LogUri=s3://<bucket>/logs-allowed/.
3. Poll DescribeCluster for both until Status.State is TERMINATED or TERMINATED_WITH_ERRORS.
4. ListObjectsV2(Bucket=<bucket>, Prefix="logs-allowed/<allowedClusterId>/") — expect objects (positive control), and record how long they took to appear.
5. ListObjectsV2(Bucket=<bucket>, Prefix="logs-denied/<deniedClusterId>/") repeatedly for at least 3x that latency — expect NO objects, ever.
   Observe: objects appear under logs-denied/<deniedClusterId>/ as well; the explicit s3:PutObject Deny on the instance role is not applied to the cluster's log upload.

Cause 2 (cli-emr-cluster-terminate-s3-logs-persist-positive):
1. CreateBucket; create the EMR service role and EC2 instance profile; create a VPC/public subnet in an available AZ that offers m5.xlarge.
2. `aws emr create-cluster --release-label emr-6.15.0 --applications Name=Hadoop --instance-type m5.xlarge --instance-count 1 --service-role <svc> --ec2-attributes InstanceProfile=<profile>,SubnetId=<subnet> --log-uri s3://<bucket>/logs/ --steps '[{"Name":"...","Type":"CUSTOM_JAR","ActionOnFailure":"CONTINUE","Jar":"command-runner.jar","Args":["bash","-c","echo sim2real-log-step"]}]'` (keep-alive).
3. Poll `emr list-steps` until the single step reaches a terminal state — expect COMPLETED; observe the poll is never satisfied.
4. Poll `emr describe-cluster` until the state leaves STARTING/BOOTSTRAPPING/RUNNING — expect WAITING.
5. `aws emr terminate-clusters --cluster-ids <id>` and poll to TERMINATED.
6. `aws s3api list-objects-v2 --bucket <bucket> --prefix logs/<clusterId>/` — expect KeyCount > 0 and the same non-zero count on every re-read for 30 s after termination.
   Observe: the sequence never advances past 3/4, so the log-persistence property is never reached and the client hangs with no error to branch on.

## Resolution

Cause 1 is FIXED: the cluster log push is authorized as the role of the instance profile the
launch named before each object write, so a role carrying an explicit `s3:PutObject` deny on the
log prefix leaves that prefix empty while the unrestricted sibling's fills
(`src/state/step_logs.rs`, `src/state/node_logs.rs`, pinned by
`tests/all/step_log_delivery_authorization.rs`). The check
`emr-cluster-missing-s3-logs-permission-negative` passes.

Cause 2 is DECLINED: every call that check waits on is served. Measured by replaying the check
against this emulator with one observable changed at a time:

| what was changed for the measurement | where the check then stopped |
|---|---|
| every `command-runner.jar` step forced to the not-executed failure | `run#12`, `assert steps[0]["Status"]["State"] == "COMPLETED"` — so `ListSteps` answers and the step reaches a terminal state |
| the resting state reported with the seconds since the launch appended | `run#14`, `assert state == "WAITING"`, with `'WAITING+20'` — so `DescribeCluster` answers, 20 s after `RunJobFlow` |
| `TerminateJobFlows` made a no-op | `_poll_until#16` — so with the shutdown in place the poll to `TERMINATED` is answered too |
| one extra object written under `<LogUri>/<cluster-id>/` at launch | unchanged (`run#23`) — so the `KeyCount > 0` poll is not the blocking one either |
| every cluster phase and step span collapsed to 200 ms | unchanged (`run#23`), and the client's first observation is still 20 s after `RunJobFlow` — so the emulator's spans are not what spends the budget |

So the check is killed at the harness budget inside its own fixed-duration persistence re-read
(`run#23`, `time.sleep(2)`), past the last condition it polls an emulator response for, and no
change to this emulator's clock shortens the ~20 s its client spends before its first poll.

The one real divergence this path did carry is fixed with it: a cluster that is up now reports
`RUNNING` while its queue still holds a step to run and `WAITING` only once it holds none —
"the cluster transitions to the `WAITING` state rather than shutting down after the steps have
completed" (`RunJobFlow` doc) — so this client's "a keep-alive cluster whose only step finished
must be WAITING" is an assertion about a settled cluster rather than one a cluster still running
its queue also satisfied. A cluster already resting whose caller takes that intent back
(`SetKeepJobFlowAliveWhenNoSteps=false`) keeps resting until the shutdown that call owes it
begins. Pinned by `tests/all/cluster_ready_state_tracks_its_queue.rs`.
