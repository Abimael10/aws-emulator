Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: emr.presigned-url-bounded-expiration emr.bootstrap-action-completes-before-the-first-step-runs emr.bootstrap-action-failure-terminates-with-errors
Issue-Shape-Id: com.amazonaws.emr#ElasticMapReduce
Issue-Key: emr.presigned-url-bounded-expiration|_assert_bounded_expiry#5|AssertionError|assert expiry_epoch is not None, f"presigned URL carries no parseable expiry: {url!r}"
Issue-Key: emr.bootstrap-action-completes-before-the-first-step-runs|run#22|AssertionError|assert step["Status"]["State"] == "COMPLETED", (
Issue-Key: emr.bootstrap-action-failure-terminates-with-errors|_poll#12|Timeout|time.sleep(interval_s)
Issue-Commit: 47208604cde63241f79c213c4b593a23b3ff78ba

# emr: 3 net-new causes — app-UI presigned URLs carry no expiry at all, a step on a bootstrap-action cluster never runs its application, and a failing bootstrap action never fails the cluster

> _Note: this issue was identified against the code base at commit `47208604cde63241f79c213c4b593a23b3ff78ba`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetOnClusterAppUIPresignedURL

## Description
Three net-new behavioral root causes in the EMR (elasticmapreduce) data plane and app-UI surface. Each is a separate work item; all three must be closed before this issue is submitted.

=== Cause 1 of 3: the app-UI presigned URL carries no expiry, so the credential in it is unbounded ===
AWS:      Smithy (com.amazonaws.emr#ElasticMapReduce) GetOnClusterAppUIPresignedURL and GetPersistentAppUIPresignedURL both return PresignedURLReady (Boolean) and PresignedURL (XmlString, len 0..10280); the model says nothing about the URL's internals, so recorded behavior only for those. EMR's app-UI proxy URL has the form `https://<proxy-host>/<ui-path>/?authToken=<JWT>`, where authToken is a three-part JWT (header.payload.signature) whose base64url payload carries `iat` and `exp` claims; the window is bounded (the recorded value of exp-iat is 43200 s / 12 h, and the requirement is a bounded future window, not that exact number). The requirement is that an expiry is discoverable from the URL, that it is in the future, and that the window is bounded (<= 24 h) rather than indefinite. The same holds for the persistent-app-UI URL, which may instead carry the SigV4 query form (X-Amz-Date + X-Amz-Expires).
Emulator: the URL is `https://<cluster-id>.emrappui-prod.<region>.amazonaws.com/<ui-path>/?authToken=<opaque>` where authToken is a flat random opaque string (the replay shows a 64-character alphanumeric value with no '.' separators), not a JWT, and the URL carries no X-Amz-Date/X-Amz-Expires/Expires parameter either. Nothing in the URL states when the credential stops working, so the check fails with `AssertionError: presigned URL carries no parseable expiry`.
Why:       a client cannot tell when to re-mint the URL, and anything that caches or forwards one has no way to know it is handing on a credential; an app-UI token with no expiry is a credential that never ages out, which is the opposite of what "presigned" means. Tooling that parses the token's exp to schedule a refresh gets nothing to parse.
Fix:       the token minting used by both presigned-URL handlers — `crate::state::mint_presigned_url_token` and `ClusterRecord::on_cluster_app_ui_url` in services/service-com-amazonaws-emr-elasticmapreduce/src/state.rs, reached from services/service-com-amazonaws-emr-elasticmapreduce/src/ops/get_on_cluster_app_ui_presigned_url.rs (lines 269-281) and the GetPersistentAppUIPresignedURL handler. The minted token must be a JWT whose payload carries iat (mint time) and exp (iat + a bounded TTL), so both operations' URLs expose a bounded future expiry.
Traces:    emr.presigned-url-bounded-expiration

=== Cause 2 of 3: a step dispatched after a bootstrap action is FAILED with emulator prose instead of running its application ===
AWS:      Smithy ListSteps/DescribeStep report StepStatus.State from StepState (PENDING | CANCEL_PENDING | RUNNING | COMPLETED | CANCELLED | FAILED | INTERRUPTED). Recorded behavior: a cluster launched with a BootstrapActions entry runs that script on every provisioned node BEFORE the first step is dispatched, so a following step whose HadoopJarStep is command-runner.jar with Args ["bash","-c","cat /tmp/<file the bootstrap action wrote>"] finds the file, the command exits 0, and the step reaches StepState COMPLETED (with Status.Timeline.StartDateTime and EndDateTime set). A step that runs and succeeds carries no Status.FailureDetails and no StateChangeReason blaming the service.
Emulator: the step is walked through the lifecycle but its application is never executed: it ends in StepState FAILED with StateChangeReason.Message `This emulator runs the step lifecycle but executes no Hadoop jar, so the step ended without running its application.` — an emulator-authored sentence AWS never emits. Because no jar runs, the bootstrap-before-step ordering this check exists to observe can never be observed either.
Why:       every client that submits work to EMR and reads the step's terminal state sees FAILED for a step AWS completes, so an EMR-driven pipeline can never make progress and its error branch fires on correct input. The emulator-authored reason also makes the failure unparseable against AWS's own reason vocabulary.
Fix:       the step execution engine behind AddJobFlowSteps/RunJobFlow — the step state machine in services/service-com-amazonaws-emr-elasticmapreduce/src/state.rs and the step-dispatch path of the RunJobFlow/AddJobFlowSteps handlers — must actually execute the step's HadoopJarStep (at minimum command-runner.jar) against the cluster's node filesystem and derive the terminal StepState from the command's exit status. The open issue 20261108T151103Z-emr-4-net-new-root-causes-every-job.md covers the same engine for other checks but does not carry this check's replay key or the bootstrap-ordering requirement, so this cause is net-new work item against that engine.
Traces:    emr.bootstrap-action-completes-before-the-first-step-runs

=== Cause 3 of 3: a bootstrap action that exits non-zero never fails the cluster — no TERMINATED_WITH_ERRORS, no BOOTSTRAP_FAILURE, and the step is not CANCELLED ===
AWS:      Smithy ClusterState enum is STARTING | BOOTSTRAPPING | RUNNING | WAITING | TERMINATING | TERMINATED | TERMINATED_WITH_ERRORS, and ClusterStateChangeReasonCode enum includes BOOTSTRAP_FAILURE; ClusterTimeline carries CreationDateTime / ReadyDateTime ("The date and time when the cluster was ready to run steps") / EndDateTime. Recorded behavior: a ScriptBootstrapAction whose script exits non-zero fails the whole cluster. The cluster reaches ClusterState TERMINATED_WITH_ERRORS; Status.StateChangeReason.Code is BOOTSTRAP_FAILURE with a non-empty human-readable Message; Status.Timeline has NO ReadyDateTime (the cluster never became ready to run steps) and DOES have EndDateTime; and any step submitted with the launch is CANCELLED — StepState CANCELLED with no Status.Timeline.StartDateTime — never run. ListBootstrapActions echoes the submitted ScriptPath verbatim.
Emulator: the bootstrap action is recorded but never executed and its exit status is never evaluated, so none of that is reachable: the cluster does not converge on TERMINATED_WITH_ERRORS/BOOTSTRAP_FAILURE and the check exhausts the harness budget (`Check timed out after 60.0s`) still polling DescribeCluster for a terminal state. The step submitted alongside the failing bootstrap action is likewise never CANCELLED.
Why:       a client that launches a cluster whose bootstrap script fails waits forever instead of being told the cluster died bootstrapping; it can neither classify the failure (BOOTSTRAP_FAILURE vs STEP_FAILURE vs INSTANCE_FAILURE) nor rely on the invariant that a bootstrap-failed cluster never reports ReadyDateTime and never runs its steps. Cost-control and retry logic keyed on that classification never runs.
Fix:       bootstrap-action execution and its effect on the cluster lifecycle — the RunJobFlow handler's BootstrapActions path plus the cluster state machine in services/service-com-amazonaws-emr-elasticmapreduce/src/state.rs: run each ScriptBootstrapAction during the BOOTSTRAPPING span, and on a non-zero exit drive the cluster to TERMINATED_WITH_ERRORS with StateChangeReason.Code=BOOTSTRAP_FAILURE, leave ReadyDateTime unset, set Timeline.EndDateTime, and transition every pending step to CANCELLED with no StartDateTime.
Traces:    emr.bootstrap-action-failure-terminates-with-errors

## Evidence
Behavioral replay against the booted emulator, service heading `### emr`.

Cause 1 — check emr.presigned-url-bounded-expiration [mismatch]:
  expected: GetOnClusterAppUIPresignedURL(ClusterId=<running cluster>, OnClusterAppUIType="SparkHistoryServer") -> PresignedURL whose authToken is a JWT with iat/exp claims; exp in the future and exp-iat bounded (0 < ttl <= 86400). Same for GetPersistentAppUIPresignedURL(PersistentAppUIId=<id>, PersistentAppUIType="SHS").
  actual:   AssertionError: presigned URL carries no parseable expiry: 'https://j-0000000000001.emrappui-prod.us-west-2.amazonaws.com/sparkhistoryserver/?authToken=fY8MajURyDSLfYU4VdhlwQuqVzhB12oPv9UE3ByQlMPhonNfSjmvHZMYFkdoMmrh'  (the token is opaque, not a JWT; no X-Amz-Date/X-Amz-Expires/Expires parameter either). Host/id/token values above are per-run; the requirement is only that a bounded expiry be present.

Cause 2 — check emr.bootstrap-action-completes-before-the-first-step-runs [mismatch]:
  expected: step (command-runner.jar, Args ["bash","-c","cat /tmp/sim2real-bootstrap-<token>"]) -> Status.State COMPLETED, proving the bootstrap action had already run when the step was dispatched.
  actual:   AssertionError: the step must COMPLETE — a FAILED step here means `cat /tmp/sim2real-bootstrap-...` found nothing, i.e. the bootstrap action had not run when the step was dispatched. Got 'FAILED' (reason {'Message': 'This emulator runs the step lifecycle but executes no Hadoop jar, so the step ended without running its application.'})

Cause 3 — check emr.bootstrap-action-failure-terminates-with-errors [mismatch]:
  expected: RunJobFlow with BootstrapActions=[{ScriptBootstrapAction:{Path: s3://<bucket>/bootstrap/fail.sh}}] (script `exit 17`) and one step -> cluster converges on ClusterState TERMINATED_WITH_ERRORS, Status.StateChangeReason.Code == "BOOTSTRAP_FAILURE" with a non-empty Message, Status.Timeline without ReadyDateTime and with EndDateTime, and the single submitted step in StepState CANCELLED with no Status.Timeline.StartDateTime.
  actual:   Check timed out after 60.0s — the cluster never leaves its non-terminal state, so no terminal state, no BOOTSTRAP_FAILURE reason and no CANCELLED step is ever observable.

## Steps to Reproduce
Cause 1:
1. Boot the emulator, provision the EMR service role and EC2 instance profile, RunJobFlow a keep-alive emr-6.15.0 cluster and wait for RUNNING/WAITING.
2. Call GetOnClusterAppUIPresignedURL(ClusterId=<id>, OnClusterAppUIType="SparkHistoryServer") until PresignedURLReady is true, then parse the URL's query: observe a flat opaque `authToken` with no '.' separators (not a JWT), and no X-Amz-Date/X-Amz-Expires/Expires parameter — i.e. no discoverable expiry.
3. CreatePersistentAppUI(TargetResourceArn=<cluster arn>), wait for a terminal PersistentAppUIStatus, then GetPersistentAppUIPresignedURL(PersistentAppUIId=<id>, PersistentAppUIType="SHS") and confirm the same absence of an expiry.

Cause 2:
4. Create an S3 bucket and put a bootstrap script that writes a token file under /tmp.
5. RunJobFlow a cluster with that BootstrapActions entry plus a step command-runner.jar / ["bash","-c","cat /tmp/<token file>"].
6. Poll ListSteps: observe Status.State FAILED with StateChangeReason.Message "This emulator runs the step lifecycle but executes no Hadoop jar, so the step ended without running its application." instead of COMPLETED.

Cause 3:
7. Put a bootstrap script `#!/bin/bash\necho ... >&2\nexit 17\n` in S3.
8. RunJobFlow a keep-alive cluster with that bootstrap action AND one step (command-runner.jar, ["bash","-c","exit 0"], ActionOnFailure CONTINUE).
9. Confirm ListBootstrapActions echoes the submitted ScriptPath, then poll DescribeCluster: observe the cluster never reaches TERMINATED_WITH_ERRORS, Status.StateChangeReason.Code is never BOOTSTRAP_FAILURE, ReadyDateTime is not withheld, and ListSteps never shows the step CANCELLED.
10. Re-replay the three checks named in trace_ids to confirm the fix.
