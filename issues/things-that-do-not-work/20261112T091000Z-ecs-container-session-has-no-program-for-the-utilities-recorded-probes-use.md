Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecs.capprov-cluster-default-strategy-governs-a-task-that-names-no-provider ecs.capprov-zero-capacity-provider-rejects-task-and-no-container-runs ecs.container-workdir-user-and-ulimit-apply-inside-the-running-container ecs.ephemeral-storage-request-is-provisioned-inside-the-container ecs.container-depends-on-success-gates-dependent-start ecs.container-depends-on-healthy-waits-for-the-health-check-to-pass
Issue-Shape-Id: com.amazonaws.ecs#AmazonEC2ContainerServiceV20141113
Issue-Key: ecs.capprov-cluster-default-strategy-governs-a-task-that-names-no-provider|run#18|AssertionError|assert container.get("exitCode") == 0, (
Issue-Key: ecs.capprov-zero-capacity-provider-rejects-task-and-no-container-runs|run#26|AssertionError|assert container.get("exitCode") == 0, (
Issue-Key: ecs.container-workdir-user-and-ulimit-apply-inside-the-running-container|run#21|AssertionError|assert probe.get("exitCode") == _BASE, (
Issue-Key: ecs.ephemeral-storage-request-is-provisioned-inside-the-container|_measured_gib#24|AssertionError|assert code != _DF_FAILED, (
Issue-Key: ecs.container-depends-on-success-gates-dependent-start|run#22|AssertionError|assert app_gated.get("exitCode") is None, (
Issue-Key: ecs.container-depends-on-healthy-waits-for-the-health-check-to-pass|_delta#11|AssertionError|assert code != _NO_START_FILE, (

# ECS container session: a program that names a utility outside the session's catalog reports no exit code, and `awk` is one of them

> _Filed while fixing `20261111T145804Z-ecs-task-data-plane-container-commands-never-execute.md` (the part of Cause 1 and Cause 7 that the runtime-settings fix does not reach). Those causes' own rules are implemented — a container's `workingDirectory`, `user` and `ulimits` and the task's ephemeral storage are now applied and reported by `pwd`, `id`, `ulimit` and `df` (`src/state/container_process.rs`) — but the recorded probes still report no status, because their programs name utilities the session has no program for._

## Operation
RunTask

## Description
A container's program runs in the session `src/state/container_shell.rs` describes, which PERFORMS
a fixed catalog of utilities over the real shell grammar and FAILS CLOSED for everything else: a
run that reaches a command the session has no program for claims nothing about the container
(`ContainerOutcome::Abandoned`), so `DescribeTasks` reports the container with NO `exitCode`. That
is the right answer for a program nothing ran — but it means any probe written with a utility
outside the catalog is invisible, whatever the rule it was probing.

AWS: `Container$exitCode` is "the exit code returned from the container", i.e. the status of the
program the task definition's `command` names, as that program's own utilities computed it. A
container image carries a userland; a task definition's command is routinely a one-liner over it.

Emulator catalogs today: `BUILT_IN_COMMANDS` (`echo`, `true`, `false`, `:`, `exit`, `export`,
`unset`, `trap`, `sleep`, `wait`, `test`, `[`, `set`, `exec`, `hostname`, and now `pwd`, `id`,
`ulimit`, `df`), `service_common::posix_shell_files::FILE_COMMANDS` (`cat`, `ls`, `mkdir`, `touch`,
`rm`, `sync`), `service_common::posix_shell_text_filters::TEXT_FILTER_COMMANDS` (`wc`, `cut`,
`head`, `sha256sum`, `sha512sum`, `base64`, `grep`) and the AWS CLI's `s3`/`s3api` transfers.
Everything else — `awk`, `sed`, `tr`, `sort`, `uniq`, `tail`, `printf`, `cd`, `basename`,
`dirname`, `date`, `env`, `expr`, `seq` — is a command that did not run.

One instance is known exactly: the ephemeral-storage probe measures its filesystem by piping `df`
through `awk`, so it reports no measurement even though `df` now answers the task's provisioned
size. The other units listed above also end with no `exitCode` at all, which is the same signature.

## Why it matters
Every behavioural check that reads `containers[].exitCode` to decide whether something inside the
container worked is silently unobservable, so rules that are implemented (capacity-provider
placement, `dependsOn` gating, the runtime settings, the ephemeral storage) cannot be confirmed
from inside the container.

## What a fix needs
A rule rather than another catalog entry per probe: a container's program computes over text with
the utilities its image's userland provides, and the ones whose whole effect is inside the
container's own session — its streams, its status, its variables and its own filesystem — are
performed in process. `awk` is a LANGUAGE, so it needs a maintained interpreter crate rather than a
hand-written subset (the same decision `brush-parser` already embodies for the shell grammar);
the plain text utilities (`sed` is a language too; `tr`, `sort`, `uniq`, `tail`, `printf`,
`basename`, `dirname` are not) belong beside the existing ones in
`services/common/src/posix_shell_text_filters.rs`, so every service with a caller-supplied script
gets them once. `cd` belongs beside `pwd` in `src/state/container_process.rs`'s rule.

## Steps to Reproduce
1. `RegisterTaskDefinition` with one essential container whose command is
   `['sh','-c','df -k / | awk \'NR==2 {print int($2/1048576)}\'']` (or any line naming `sed`, `tr`,
   `sort`, `printf`, `cd`, `date`).
2. `RunTask` it and poll `DescribeTasks` until the task is `STOPPED`.
3. Read `containers[0].exitCode` -> absent, instead of the status the program ended with, and the
   task carries no `stopCode` for an essential container that exited.
