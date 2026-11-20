Issue-Severity: CRITICAL
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: codebuild.command-execution-lifecycle-in-sandbox codebuild.command-execution-lifecycle-reaches-terminal-state codebuild.e2e-sandbox-command-execution-after-stop-rejected codebuild.sandbox-lifecycle-active-to-terminated codebuild.sandbox-lifecycle-transitions
Issue-Key: codebuild.command-execution-lifecycle-in-sandbox|_poll_until#16|Timeout|time.sleep(interval_s)
Issue-Key: codebuild.command-execution-lifecycle-reaches-terminal-state|_poll_until#10|Timeout|time.sleep(interval_s)
Issue-Key: codebuild.e2e-sandbox-command-execution-after-stop-rejected|_poll_until#12|Timeout|time.sleep(interval_s)
Issue-Key: codebuild.sandbox-lifecycle-active-to-terminated|_poll_until#11|Timeout|time.sleep(interval_s)
Issue-Key: codebuild.sandbox-lifecycle-transitions|_poll_until#15|Timeout|time.sleep(interval_s)
Issue-Commit: cfbb9a1e24eec7d8c98af7d6e9e62a0e8013f018

# CodeBuild: a sandbox command execution never runs the command, so it never reaches SUCCEEDED or FAILED and carries no exit code or output — the one capability left is a CONTAINED in-process shell

> _Split out of `issues/things-that-now-work/20261025T062553Z-codebuild-3-of-the-6-divergence-causes-remain-open.md`, whose other two causes are fixed (`StartBuildBatch`'s batch now settles into a terminal status the suite can act on, and CloudFormation now provisions `AWS::CodeBuild::Fleet` through CodeBuild's own `CreateFleet`). This report carries the five replay keys that do NOT pass, re-filed here so they are not recorded as working. Everything below is what the two earlier fixes established about them; a later fixer starts from here rather than from scratch._

## Operation
StartCommandExecution

## Description
A command execution started in a `RUNNING` sandbox never reaches a terminal status that
reflects a process, because nothing runs the command.

AWS: a command run in a `RUNNING` sandbox reaches a terminal status that reflects the
process it ran — `echo <marker>` leaves the execution `SUCCEEDED` with `exitCode` '0' and
the marker in `standardOutputContent`; `exit 7` leaves it `FAILED` with `exitCode` '7'.

Emulator: `StoredCommandExecution::settle`
(`services/service-com-amazonaws-codebuild-codebuild-20161006/src/state.rs`) ends every
execution in `COMMAND_EXECUTION_STATUS_FAULT` carrying `COMMAND_NOT_EXECUTED_MESSAGE`, with
no exit code and no output, so the five checks spin on `BatchGetCommandExecutions` until
their budget is gone and never reach their own negative assertions.

Why it is BLOCKED, and on what: the capability needed is RUNNING a caller-supplied shell
command line and reporting its real exit code and output. Served faithfully that needs the
build image's shell and filesystem — a runtime this platform does not provide ("Containers
and external processes are not available to a generated emulator yet",
`vendor/engine/src/lib.rs:13-15`). What was checked and ruled out:

* `brush-core` 0.5.0 (`cargo info brush-core`: MIT, rust-version 1.88.0, dependencies
  include `command-fds`, `nix`, `uzers`, `check_elevation`) is the only maintained
  in-process POSIX/bash shell on the registry, and it is a FULL shell: it resolves and
  executes external commands as host processes against the host filesystem. That breaks
  containment (no process spawn and no host filesystem on request data) and is unfaithful
  anyway — the host is not the CodeBuild image — and it exposes no seam that disables
  external command resolution.
* Writing an evaluator for a shell subset instead is the forbidden hand-written language
  subset: a subset sized to `echo` and `exit` answers every other command wrongly while
  reporting success.

So the honest exit for this cause remains `mark_wont_fix(platform_blocker)` naming
`vendor/engine/src/lib.rs:13-15`, UNLESS a contained in-process shell engine appears — a
shell crate with a seam for external command resolution, or a WASI runtime plus a shell
image the tree may ship. A fixer that finds one implements it ONCE, in a shared place under
`services/`, and the execution's own status, exit code and `standardOutputContent` then all
follow from it. Do not re-word the refusal: answering the call with a different non-success
status, or with a 501, is not a fix.

What is already in place around it, so a later fixer does not redo it: `StartSandbox` /
`BatchGetSandboxes` carry the sandbox through its own lifecycle, and
`StartCommandExecution`'s refusal for a sandbox whose session is not live states
CodeBuild's own sentence ("Command execution cannot be started if sandbox status is
STOPPED", with the status read off the stored record) — so the negative half of the sandbox
checks is in place and waiting only for the positive control above.

## Evidence
Replay of the parent issue's 7 units after the fleet fix: 2 passed
(`codebuild.e2e-build-batch-retry-and-cleanup-flow`,
`codebuild.deploy-fleet-list-reflects-fleet`), 5 failed, and the 5 keys above are still
open with `other_causes` empty — the fleet fix caused none of them. Each failure is
`Check timed out after 60.0s`: the execution poll waits on `{'SUCCEEDED','FAILED'}` and the
emulator settles every execution to `FAULT`.

## Steps to Reproduce
1. Boot the emulator and replay the check ids in Issue-Trace-Ids.
2. `CreateProject` on a NO_SOURCE project, `StartSandbox`, poll `BatchGetSandboxes` until
   `RUNNING`, then `StartCommandExecution(sandboxId=..., command='echo marker',
   type='SHELL')` and poll `BatchGetCommandExecutions` — observe `FAULT` with no `exitCode`
   and no `standardOutputContent` instead of `SUCCEEDED` / '0' / the marker.
3. Repeat with `command='exit 7'` (expect `FAILED` / '7'). The `StopSandbox` refusal that
   follows is already served with AWS's own sentence.
