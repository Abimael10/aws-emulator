Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: codebuild.phase-durations-equal-their-own-windows-and-the-slow-phase-dominates codebuild.timeout-in-minutes-kills-the-running-build-and-marks-that-phase-timed-out
Issue-Shape-Id: com.amazonaws.codebuild#CodeBuild_20161006
Issue-Key: codebuild.phase-durations-equal-their-own-windows-and-the-slow-phase-dominates|run#20|AssertionError|assert build_dur >= sleep_s, (
Issue-Key: codebuild.timeout-in-minutes-kills-the-running-build-and-marks-that-phase-timed-out|run#17|AssertionError|assert timed_out["buildStatus"] == "FAILED", (
Issue-Commit: fa5a827558c7852c4bea0e628f9a7809957676ef

# CodeBuild build clock is not real: every BuildPhase reports durationInSeconds 0, and timeoutInMinutes is never enforced (2 causes)

> _Note: this issue was identified against the code base at commit `fa5a827558c7852c4bea0e628f9a7809957676ef`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
BatchGetBuilds

## Description
Two net-new root causes in the codebuild emulator's build execution/phase bookkeeping. Both are observed through BatchGetBuilds on a build started with StartBuild against a NO_SOURCE project with an inline buildspec. No open issue carries either one: the closest open items are 20261110T135206Z-codebuild-a-build-is-never-held-queued-behind.md (QUEUED holding and queuedTimeoutInMinutes — the QUEUE timeout, not the EXECUTION timeout) and 20261109T181128Z-codebuild-buildspec-commands-are-not-run-through-a.md (shell execution of commands); neither covers phase duration derivation nor timeoutInMinutes enforcement.

Cause 1 of 2: every BuildPhase comes back with durationInSeconds 0, so no phase reflects the time it actually occupied.
AWS: Smithy: BuildPhase.durationInSeconds (WrapperLong) is documented as "How long, in seconds, between the starting and ending times of the build's phase", alongside BuildPhase.startTime and BuildPhase.endTime (both Timestamp). That is a DEFINITION: for every phase that reports both startTime and endTime, durationInSeconds is round(endTime - startTime) (rounding mode undocumented, so ±1s). Consequently a buildspec whose build phase blocks for N seconds yields a BUILD phase whose durationInSeconds is at least N, strictly greater than the other buildspec phases (INSTALL/PRE_BUILD/POST_BUILD), the phase windows do not overlap (endTime[i] <= startTime[i+1]), the per-phase durations sum to no more than the Build's own round(endTime - startTime) window, and COMPLETED is the last phase and the only one with no durationInSeconds.
Emulator: for a build whose buildspec sleeps 24s in its build phase, every phase reports durationInSeconds 0: {'SUBMITTED': 0, 'QUEUED': 0, 'PROVISIONING': 0, 'DOWNLOAD_SOURCE': 0, 'INSTALL': 0, 'PRE_BUILD': 0, 'BUILD': 0, 'POST_BUILD': 0, 'UPLOAD_ARTIFACTS': 0, 'FINALIZING': 0}. The phase table is therefore not derived from any real start/end window — the emulator stamps each phase with a single instant (startTime == endTime) rather than recording when the phase actually began and ended.
Why: a client that reads the phase table to attribute build time (which phase dominated, whether the build is making progress, CI dashboards and billing/duration estimates, any assertion of the documented duration == endTime - startTime identity) sees a uniformly zero table, so no phase can ever be identified as slow and a long-running build is indistinguishable from an instantaneous one.
Fix: the build runner/phase state machine in the codebuild service crate — the component that advances a build through BuildPhaseType and records each phase's startTime/endTime must record the real window the phase occupied, and the serializer must derive durationInSeconds from that window rather than emitting 0.
Traces: codebuild.phase-durations-equal-their-own-windows-and-the-slow-phase-dominates.

Cause 2 of 2: the project's timeoutInMinutes (execution timeout) is never enforced — a build that runs past it completes SUCCEEDED instead of being killed.
AWS: Smithy: Build.timeoutInMinutes is "How long, in minutes, from 5 to 2160 (36 hours), for CodeBuild to wait before timing out this build if it does not get marked as completed". When that execution timeout elapses, AWS ends the build: Build.buildStatus becomes FAILED (StatusType includes FAILED, FAULT, IN_PROGRESS, STOPPED, SUCCEEDED, TIMED_OUT) and the phase that was running at the moment of the timeout carries phaseStatus TIMED_OUT, with the explanation in that phase's contexts. TIMED_OUT as the scalar buildStatus is the QUEUE-timeout outcome (the build never leaves QUEUED), not the execution-timeout outcome. The phase context text is recorded behavior only; the requirement asserted here is the status pair (buildStatus FAILED + the running phase's phaseStatus TIMED_OUT, with a non-empty context on that phase).
Emulator: a build on a project with timeoutInMinutes=5 whose buildspec sleeps 3600s returns buildStatus 'SUCCEEDED' with every phase SUCCEEDED — phases=[('SUBMITTED','SUCCEEDED'),('QUEUED','SUCCEEDED'),('PROVISIONING','SUCCEEDED'),('DOWNLOAD_SOURCE','SUCCEEDED'),('INSTALL','SUCCEEDED'),('BUILD','SUCCEEDED'),('UPLOAD_ARTIFACTS','SUCCEEDED'),('FINALIZING','SUCCEEDED'),('COMPLETED',None)] — and contexts=[None]*9. No phase is marked TIMED_OUT and no timeout is applied at all.
Why: a client relying on the execution timeout as its safety net (CI pipelines that bound build cost/wall time, retry logic keyed on FAILED + a TIMED_OUT phase, alarms on timed-out builds) is told the runaway build succeeded. Worse, the build's reported success is a false positive: a buildspec that never finished is reported as having completed every phase, so a pipeline gated on buildStatus proceeds on a build that in AWS would have been killed.
Fix: the build runner in the codebuild service crate — enforce the effective execution timeout (project timeoutInMinutes, overridden by StartBuild's timeoutInMinutesOverride) in the component that drives phase progression: on expiry, terminate the build, set the currently running phase's phaseStatus to TIMED_OUT with its context, and set Build.buildStatus to FAILED (buildComplete true).
Traces: codebuild.timeout-in-minutes-kills-the-running-build-and-marks-that-phase-timed-out.

## Evidence
Cause 1 — check codebuild.phase-durations-equal-their-own-windows-and-the-slow-phase-dominates:
  Expected: BUILD phase durationInSeconds >= 24 (the buildspec's sleep), strictly greater than INSTALL/PRE_BUILD/POST_BUILD, and for every phase with both timestamps durationInSeconds == round(endTime - startTime) (±1s).
  Actual: AssertionError: the build phase sleeps 24s, so its durationInSeconds must be at least that; reported 0. Measured durations: {'SUBMITTED': 0, 'QUEUED': 0, 'PROVISIONING': 0, 'DOWNLOAD_SOURCE': 0, 'INSTALL': 0, 'PRE_BUILD': 0, 'BUILD': 0, 'POST_BUILD': 0, 'UPLOAD_ARTIFACTS': 0, 'FINALIZING': 0}

Cause 2 — check codebuild.timeout-in-minutes-kills-the-running-build-and-marks-that-phase-timed-out:
  Expected: buildStatus FAILED, with the phase that was running (BUILD) carrying phaseStatus TIMED_OUT and a context explaining the timeout.
  Actual: AssertionError: a build sleeping 3600s under a 5-minute EXECUTION timeout must reach buildStatus FAILED (the TIMED_OUT status lives on the phase, not the scalar), got 'SUCCEEDED'; phases=[('SUBMITTED','SUCCEEDED'),('QUEUED','SUCCEEDED'),('PROVISIONING','SUCCEEDED'),('DOWNLOAD_SOURCE','SUCCEEDED'),('INSTALL','SUCCEEDED'),('BUILD','SUCCEEDED'),('UPLOAD_ARTIFACTS','SUCCEEDED'),('FINALIZING','SUCCEEDED'),('COMPLETED',None)]; contexts=[None,None,None,None,None,None,None,None,None]

Smithy citations: BuildPhase.durationInSeconds ("How long, in seconds, between the starting and ending times of the build's phase"), BuildPhase.startTime/endTime; Build.timeoutInMinutes ("How long, in minutes, from 5 to 2160 (36 hours), for CodeBuild to wait before timing out this build if it does not get marked as completed"); StatusType = [SUCCEEDED, FAILED, FAULT, TIMED_OUT, IN_PROGRESS, STOPPED]; BuildPhaseType = [SUBMITTED, QUEUED, PROVISIONING, DOWNLOAD_SOURCE, INSTALL, PRE_BUILD, BUILD, POST_BUILD, UPLOAD_ARTIFACTS, FINALIZING, COMPLETED].

## Steps to Reproduce
Cause 1 (phase durations):
1. CreateProject with source {type: NO_SOURCE, buildspec: inline} whose build phase runs `sleep N` for some N in 15..25, artifacts NO_ARTIFACTS, cache NO_CACHE, environment LINUX_CONTAINER/aws/codebuild/standard:7.0/BUILD_GENERAL1_SMALL, and commands in install/pre_build/build/post_build.
2. StartBuild on that project and poll BatchGetBuilds until buildComplete.
3. Read builds[0].phases: every phase reports durationInSeconds 0 (and startTime == endTime), instead of BUILD >= N and durationInSeconds == round(endTime - startTime) per phase.

Cause 2 (execution timeout):
1. CreateProject as above with timeoutInMinutes=5 and a buildspec whose build phase runs `sleep 3600`.
2. StartBuild and poll BatchGetBuilds past the 5-minute timeout.
3. The build reports buildStatus SUCCEEDED with all phases SUCCEEDED, instead of buildStatus FAILED with the running BUILD phase's phaseStatus TIMED_OUT and a context on that phase.
