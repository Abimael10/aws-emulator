Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch__ecs__ssm__xray.e2e-diagnose-xray-latency-cause-from-alarm-config-lifecycle cloudwatch__ecs__ssm__xray.fargate-task-role-without-xray-put-trace-segments-is-denied-the-frame-trace
Issue-Key: cloudwatch__ecs__ssm__xray.e2e-diagnose-xray-latency-cause-from-alarm-config-lifecycle|phase#12|AssertionError|raise AssertionError(f"[phase {i}/{len(self.phases)} {name}] {exc}") from exc
Issue-Key: cloudwatch__ecs__ssm__xray.fargate-task-role-without-xray-put-trace-segments-is-denied-the-frame-trace|_poll#31|AssertionError|raise AssertionError(f"{description} not met within {timeout_s}s; last {last!r}")
Issue-Commit: cd6d73564e5f4cf53934bca1327f08de31f2ee5f

# ecs: a container's declared program is ABANDONED when it uses a shell form the container session performs none of, so a containerised publisher makes none of the AWS calls it is written round

> _Split out of `issues/things-that-now-work/20261110T232635Z-cloudwatch-ecs-ssm-xray-a-cloudformation-created-aws-cloudwatch-alarm-is-invisible.md`, whose cause 1 (a CloudFormation-created `AWS::CloudWatch::Alarm` invisible to `DescribeAlarms`) is fixed and whose cause 2's RULE is implemented and tested there. This report carries the replay keys that do NOT pass, re-filed here so they are not recorded as working. Everything below is what that fix established about them; a later fixer starts from here rather than from scratch._

## Operation
RunTask

## Description
ONE open cause, and it is the step BEFORE the one the parent report fixed.

What the parent fix established, so a later fixer does not redo it: the AWS CLI a container's
session provides now really performs the command groups an instrumented workload is written round —
`aws cloudwatch put-metric-data`, `aws xray put-trace-segments` and `aws ssm get-parameter`, beside
the `aws s3`, `aws s3api` and `aws sns publish` it already had — each call made against this
account's own services over the in-process hop and SIGNED AS THE TASK ROLE, so a task role that
does not carry an action is refused exactly that call and the command exits with the status the CLI
answers a service error with. That is pinned end to end by
`core/router/tests/all/ecs_container_publishes_its_telemetry.rs` (one Fargate task whose role
carries every action reads its parameter, submits its segment, publishes `Result=ok` and has its
trace answered by `GetTraceSummaries`; one whose role withholds exactly `xray:PutTraceSegments` is
refused, branches, publishes `Result=denied` and leaves no trace; one with no task role locates no
credential and calls nothing) and by
`services/service-com-amazonaws-ecs-amazonec2containerservicev20141113/tests/all/container_published_telemetry.rs`.

Cause 1 of 1: a container's program is ABANDONED, with none of it performed, when it uses a shell
form the ECS container session performs none of — so a program that uses one makes none of its
calls, and the polled counters of both checks below stay at zero however complete the CLI surface
is (hypothesis for WHICH form the recorded programs use: the replay reports only its own barrier,
so what is established is that no counter moved and no log event was delivered, which is the
signature of a run abandoned before its first command rather than of a call that failed).
AWS: recorded behavior only (no Smithy member describes container execution). On AWS the container
runs the argument vector its definition states (`ContainerDefinition$entryPoint` / `$command`)
through a real shell, so every form that shell admits runs.
Emulator: `crate::state::container_shell` is this service's OWN session, and it performs a SUBSET
of the forms the workspace's shared session (`services/common/src/posix_shell_session.rs`) performs
— it has no here-document redirection (`<<EOF`, which the shared session's redirection arm handles)
and no clock program (`date`, which the shared session's environment programs provide through
`service-com-amazonaws-ec2-amazonec2`'s `state::guest_clock`) — and ONE form it declines abandons
the WHOLE run: the container reports no exit code and the detail
"This emulator performs a container's declared program itself and performed none of the program
this container declared" (`PROGRAM_FORM_NOT_PERFORMED_DETAIL`). A publisher program that builds a
segment document in a here-document, or stamps what it publishes with `date +%s`, therefore
produces nothing at all rather than running up to the form it uses.
Why: every container whose program uses a form outside that subset is silently a container that
does nothing, which is the direction that hides breakage — the control plane reports a healthy task
and no log line, metric or trace ever appears, so no workload behaviour and no permission
differential built on one is observable.
Fix: the migration `crate::state::container_shell`'s own module docs name — that session onto
`service_common::posix_shell_session`, whose forms, bounds and catalogs are already the shared
ones — or, short of it, the missing forms added to this session (here-documents and here-strings,
and the clock program) with a test per form that drives a container's program through it.
What is NOT the cause, ruled out by the parent fix: the CLI surface (the three command groups above
are performed and tested), the task-role credential (vended and presented), the image reference
(`public.ecr.aws/aws-cli/aws-cli` pulls and its entry point is applied), and the services the calls
reach (CloudWatch, X-Ray and Systems Manager each serve and authorize them).
Traces: cloudwatch__ecs__ssm__xray.fargate-task-role-without-xray-put-trace-segments-is-denied-the-frame-trace,
cloudwatch__ecs__ssm__xray.e2e-diagnose-xray-latency-cause-from-alarm-config-lifecycle.

## Evidence
cloudwatch__ecs__ssm__xray.fargate-task-role-without-xray-put-trace-segments-is-denied-the-frame-trace:
AssertionError: the control tracing its frames and the denied task reporting refusals not met
within 420s; last (0, 0, 0) — the polled triple (allowed metric writes, denied metric writes,
traced segments for the allowed task) never moves, and no log event from either task is delivered
either, which is what a run abandoned before its first command looks like.

cloudwatch__ecs__ssm__xray.e2e-diagnose-xray-latency-cause-from-alarm-config-lifecycle:
`Check timed out after 60.0s` (the harness ceiling, a per-run value) in a setup poll — its phase
1/6 alarm assertion, which the parent report was filed for, now passes; what it then blocks on is
the same publisher program.

## Steps to Reproduce
1. `ecs:RegisterTaskDefinition` with `entryPoint: ["bash", "-c"]` and a `command` whose program
   uses a here-document (`SEG=$(cat <<EOF … EOF)`) or `date +%s`, and a `logConfiguration` with the
   `awslogs` driver.
2. `ecs:RunTask` it and poll `ecs:DescribeTasks`: the task stops with no `exitCode` on its
   container and the `reason` above, and `logs:GetLogEvents` on its stream holds nothing — not even
   the lines the program wrote before the form it uses.
3. Replace that one form with a form the session performs (a quoted word, a literal instant) and
   run it again: the same program runs to its end and its `aws` commands publish, submit and read,
   as `core/router/tests/all/ecs_container_publishes_its_telemetry.rs` asserts.
4. Re-replay the two check ids in Issue-Trace-Ids to confirm the fix.
