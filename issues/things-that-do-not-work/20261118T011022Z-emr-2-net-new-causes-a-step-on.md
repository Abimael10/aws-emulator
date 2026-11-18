Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: emr.bootstrap-action-failure-terminates-with-errors emr.emr-bootstrap-action-runs-once-on-every-provisioned-node
Issue-Shape-Id: com.amazonaws.emr#ElasticMapReduce
Issue-Key: emr.bootstrap-action-failure-terminates-with-errors|run#24|AssertionError|assert steps[0]["Status"]["State"] == "CANCELLED", (
Issue-Key: emr.emr-bootstrap-action-runs-once-on-every-provisioned-node|run#12|AssertionError|assert state == "WAITING", (
Issue-Commit: 719a7d873c2ead99db4a3b531125e6f7ff90e604

# emr: 2 net-new causes — a step on a never-ready cluster is INTERRUPTED instead of CANCELLED, and a bootstrap script the session cannot perform kills the cluster with emulator prose

> _Note: this issue was identified against the code base at commit `719a7d873c2ead99db4a3b531125e6f7ff90e604`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RunJobFlow

## Description
Two independent behavioral root causes in the EMR cluster bring-up / bootstrap-action plane, each reached by its own failing check. THIS ISSUE BUNDLES 2 CAUSES (Cause 1 of 2, Cause 2 of 2). Both must be closed before this issue is submitted.

=== Cause 1 of 2: a step submitted with the launch is started while the cluster is still BOOTSTRAPPING, so a bootstrap-failed cluster reports it INTERRUPTED instead of CANCELLED ===
AWS:      Smithy (com.amazonaws.emr#ElasticMapReduce): StepState = PENDING | CANCEL_PENDING | RUNNING | COMPLETED | CANCELLED | FAILED | INTERRUPTED; ClusterState = STARTING | BOOTSTRAPPING | RUNNING | WAITING | TERMINATING | TERMINATED | TERMINATED_WITH_ERRORS; ClusterStateChangeReasonCode includes BOOTSTRAP_FAILURE; RunJobFlow$BootstrapActions is documented as "a list of bootstrap actions to run BEFORE Hadoop starts on the cluster nodes". So a cluster whose bootstrap action fails never becomes ready to run steps, and a step submitted with that launch therefore never starts: ListSteps/DescribeStep report StepStatus.State = CANCELLED with NO Status.Timeline.StartDateTime (recorded behavior for the terminal value; the model supplies the vocabulary). INTERRUPTED is reserved for a step that WAS running when its cluster was cut short — the distinction between "never ran" and "cut short mid-run" is the whole content of those two enum values.
Emulator: the step on the bootstrap-failed cluster settles in StepState INTERRUPTED (replay: `AssertionError: a step on a bootstrap-failed cluster must be CANCELLED, never run; got 'INTERRUPTED'`), i.e. the emulator had already moved it to RUNNING while the cluster sat in BOOTSTRAPPING and had never published a ReadyDateTime. The mechanism is visible in the state: for a cluster in STARTING/BOOTSTRAPPING the step queue's start instant is answered as `status.ready_due_at` (a FIXED instant that has already passed once the bootstrapping span elapsed), while the cluster itself is held in BOOTSTRAPPING because `bootstrap_actions_have_run()` is false; the queue therefore starts the step from that past instant, and the cluster's shutdown then maps a RUNNING step to INTERRUPTED rather than a PENDING one to CANCELLED.
Why:      a client that launches a cluster with a bootstrap action and reads the step's terminal state is told its work was interrupted mid-run when it never ran at all. Retry/idempotency logic keyed on CANCELLED (safe to resubmit, nothing executed) vs INTERRUPTED (may have had partial effects) takes the wrong branch, and the invariant "a cluster that never reported ReadyDateTime ran no step" — plus the absent StartDateTime that proves it — is broken.
Fix:      the cluster step queue's readiness gate — `ClusterRecord::steps_runnable_from` / `ClusterRecord::advance_steps` in services/service-com-amazonaws-emr-elasticmapreduce/src/state.rs (lines 3043-3106): a cluster in STARTING/BOOTSTRAPPING must not hand the queue a start instant in the past; the queue may only move from the instant the cluster REALLY became ready (ready_date_time set, every bootstrap action succeeded). With that gate correct, `StepRecord::stop_with_cluster` in services/service-com-amazonaws-emr-elasticmapreduce/src/state/step_records.rs (lines 201-223) already yields CANCELLED, and the step must also carry no Status.Timeline.StartDateTime.
Traces:   emr.bootstrap-action-failure-terminates-with-errors

=== Cause 2 of 2: a well-formed bootstrap action is never performed, and the cluster is driven to BOOTSTRAP_FAILURE carrying an emulator-authored message ===
AWS:      recorded behavior (the model describes BootstrapActionConfig/ScriptBootstrapActionConfig but not execution): the script named by ScriptBootstrapAction.Path is fetched and run ONCE on every provisioned node (primary + core) during BOOTSTRAPPING, with the node's own facilities available to it — instance metadata (IMDSv2) and the EMR-documented /mnt/var/lib/info/instance.json — so a script that exits 0 leaves its per-node side effects in place and the cluster goes on to ClusterState WAITING (keep-alive) or RUNNING with Status.Timeline.ReadyDateTime set. Only a script that really exits non-zero fails the cluster, and then AWS words ClusterStateChangeReason.Message with its own fixed text — "On the master instance (<primary Ec2InstanceId>), bootstrap action <position> returned a non-zero return code" (fixed text; the variable parts are the primary node's instance id and the action's 1-based position) — under Code BOOTSTRAP_FAILURE. AWS has no message for "the script was not performed": there is no such outcome.
Emulator: the script is not performed at all, and that non-outcome is published AS a bootstrap failure. DescribeCluster reports ClusterState TERMINATING with Status.StateChangeReason {Code: 'BOOTSTRAP_FAILURE', Message: 'Bootstrap action 1 did not run: this emulator performed nothing of its script.'} — an emulator-authored sentence AWS never emits — for a script whose commands merely fall outside the session's small catalog (echo/cat/true/false/:/exit/export plus `aws s3` transfers). In code: `performed_script` answers `BootstrapActionAnswer::NotPerformed` when the session gives the "could not run" status, `BootstrapActionRun::failure_message` turns that into the sentence above, and `owed_bootstrap_actions`/`begin_bootstrap_failure` then take the cluster down (services/service-com-amazonaws-emr-elasticmapreduce/src/state/bootstrap_run.rs, lines 138-170, 242-270, 482-501). The cluster consequently never reaches WAITING and the "ran once per node" contract is unobservable.
Why:      every client whose cluster is configured by a bootstrap action (install packages, write config, start an agent, anything reading instance metadata) gets a cluster that dies at launch with a failure reason that blames the script, so correct input is reported as a customer error; and the message is in no AWS vocabulary, so a client parsing or logging the reason gets emulator prose. A false BOOTSTRAP_FAILURE is also indistinguishable from a real one, so bootstrap-failure handling can never be exercised honestly.
Fix:      bootstrap-action execution — `performed_script`/`performed_bootstrap_action` in services/service-com-amazonaws-emr-elasticmapreduce/src/state/bootstrap_run.rs together with the session catalog it borrows from ::service_common::posix_shell_session: the session must perform the node-side facilities a bootstrap action uses (instance metadata, /mnt/var/lib/info/instance.json, the text utilities a script pipes through) and the action must be performed once per provisioned node so its per-node side effects exist. Separately, `BootstrapActionAnswer::NotPerformed` must never be published as a BOOTSTRAP_FAILURE whose Message is an emulator sentence — only a script that really exited non-zero (or whose object could not be fetched) may fail the cluster, with AWS's own wording.
          Related open issues that do NOT carry these checks' current replay keys and do not describe this behavior: 20261114T075942Z-emr-4-net-new-root-causes-in-the.md (its cause 3 recorded this check only as a 60 s timeout with nothing run at all) and 20261116T232636Z-emr-3-net-new-causes-app-ui-presigned.md (its cause 3 recorded the failing-bootstrap check as never reaching a terminal state). The emulator now DOES reach a terminal state and DOES publish a reason, so both causes above are net-new work items.
Traces:   emr.emr-bootstrap-action-runs-once-on-every-provisioned-node

## Evidence
Behavioral replay against the booted emulator; both checks are listed under the suite heading `### emr` (emr: 147/220 passed).

Cause 1 — [mismatch] emr.bootstrap-action-failure-terminates-with-errors:
  expected: RunJobFlow with BootstrapActions=[{ScriptBootstrapAction:{Path: s3://<bucket>/<script that exits non-zero>}}] plus one step -> the step is reported StepState CANCELLED with no Status.Timeline.StartDateTime (it never ran, because the cluster never became ready).
  actual:   File "check_emr_bootstrap_action_failure_terminates_with_errors.py", line 349, in run
            assert steps[0]["Status"]["State"] == "CANCELLED", (
            AssertionError: a step on a bootstrap-failed cluster must be CANCELLED, never run; got 'INTERRUPTED'

Cause 2 — [mismatch] emr.emr-bootstrap-action-runs-once-on-every-provisioned-node:
  expected: RunJobFlow (1 primary + 2 core, emr-6.15.0) with a BootstrapActions entry whose script identifies its node from IMDSv2 and reads its EMR role from /mnt/var/lib/info/instance.json, then writes one S3 object per node -> the cluster reaches ClusterState WAITING with ReadyDateTime set, and one object exists per provisioned node.
  actual:   File "check_emr_emr_bootstrap_action_runs_once_on_every_provisioned_node.py", line 277, in run
            assert state == "WAITING", (
            AssertionError: the cluster must finish its bootstrap action and reach WAITING; got 'TERMINATING' ({'Code': 'BOOTSTRAP_FAILURE', 'Message': 'Bootstrap action 1 did not run: this emulator performed nothing of its script.'})
  The quoted Message is emulator-authored; the only AWS message in this class is "On the master instance (<instance id>), bootstrap action <n> returned a non-zero return code", whose variable parts are the primary node's instance id and the action position (the id/position values of any single run are not part of the requirement).

## Steps to Reproduce
Cause 1:
1. Boot the emulator, create the EMR service role and EC2 instance profile, create a bucket and upload a bootstrap script that exits non-zero (e.g. `#!/bin/bash\nexit 17`).
2. RunJobFlow a keep-alive emr-6.15.0 cluster with BootstrapActions naming that script AND one step (command-runner.jar, Args ["bash","-c","exit 0"], ActionOnFailure CONTINUE).
3. Poll DescribeCluster to a terminal state (TERMINATED_WITH_ERRORS / BOOTSTRAP_FAILURE), then call ListSteps: expect the step in StepState CANCELLED with no Status.Timeline.StartDateTime; observe INTERRUPTED.
4. Re-replay emr.bootstrap-action-failure-terminates-with-errors.

Cause 2:
5. Upload a bootstrap script that reads IMDSv2 for its instance id and /mnt/var/lib/info/instance.json for its EMR role, then `aws s3 cp` one object per node under nodes/<instance-id>.
6. RunJobFlow a keep-alive cluster with 1 primary + 2 core nodes and that BootstrapActions entry.
7. Poll DescribeCluster: expect the cluster to reach WAITING with Timeline.ReadyDateTime set and one S3 object per provisioned node; observe TERMINATING with StateChangeReason.Code BOOTSTRAP_FAILURE and Message "Bootstrap action 1 did not run: this emulator performed nothing of its script." and no per-node objects.
8. Re-replay emr.emr-bootstrap-action-runs-once-on-every-provisioned-node.
