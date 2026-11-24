Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecs__logs.deep-ecs-guest-identity-awsvpc-task-reachable-on-its-eni-ip-only-while-its-sg-allows-it
Issue-Key: ecs__logs.deep-ecs-guest-identity-awsvpc-task-reachable-on-its-eni-ip-only-while-its-sg-allows-it|_poll#3|Timeout|time.sleep(interval_s)

# ECS task data plane: the probe the ENI-reachability check makes from inside a task never runs, because a container's session runs no caller-supplied program in a language and `ExecuteCommand` opens no session

> _Filed while fixing `20261105T143800Z-ecs-logs-4-net-new-causes-awslogs-delivery.md` (Cause 2 of 4). That
> issue's other three causes are resolved and its cause-2 RULE — a task answers at the private address of
> its own elastic network interface, and only while a security group on that interface admits the client —
> is now implemented and covered end to end
> (`services/service-com-amazonaws-ecs-amazonec2containerservicev20141113/tests/all/awsvpc_task_eni_reachability.rs`).
> What is NOT resolved is the path the RECORDED check drives to make its probe, so this check is tracked
> here on its own rather than closed with that issue._

## Operation
RunTask

## Description
What already works, driven through the front door and asserted in the suite named above: an `awsvpc` task's
container publishes its server at the private IPv4 address its attachment reports; another task of the
account reaches that address with `curl`/`wget` exactly when an inbound rule of a security group on the
destination's interface names the packet (a source security group or an address range, on the port the rule
names); a packet no rule names is DROPPED, so the client reports a request that timed out (curl exit 28)
rather than a refused connection; revoking the rule naming one client stops that client while a client
another rule still names keeps being answered, and authorizing it again restores delivery. The probe
idioms both work: the document, and `-o /dev/null -w '%{http_code}'` (`200` / `000`).

The recorded check still spends its whole 60 s budget in its first `_poll` and reaches no assertion, and it
does so unchanged by all of that — so the client it probes from is not a container command this emulator
performs. Two paths are left, and the emulator has neither:

1. `ExecuteCommand` opens no session at all. `src/state/exec_sessions.rs` states it: the session IS the
   stream carried over the Systems Manager data channel the `streamUrl` names, which a client opens as a
   WebSocket, and this emulator answers no WebSocket upgrade — so a container with a running
   execute-command agent is answered the modelled `TargetNotConnectedException` and nothing can run a
   command inside a task from outside it.
2. A container's session runs no caller-supplied program in a language. The session
   (`src/state/container_shell.rs`) performs a catalog of shell built-ins, the file commands and the text
   filters, and of an interpreter it performs only `-m http.server`
   (`services/common/src/static_web_servers::served_module`). So a task whose container is an image's
   language runtime running a guest script — the shape the `deep-ecs-guest-*` checks use, per the sibling
   issue `20261118T145001Z` ("the AWS Lambda python base image running a guest script") — runs nothing, is
   reported with no exit code, and writes nothing to its `awslogs` stream. The workspace HAS a Python
   engine and one precedent for wiring it into a guest session
   (`services/service-com-amazonaws-ssm-amazonssm/src/state/guest_python.rs`, over
   `…lambda…::state::execution::python`), but that engine carries no network module, so a guest script that
   makes the request itself would still make none.

## Evidence
Replay of `ecs__logs.deep-ecs-guest-identity-awsvpc-task-reachable-on-its-eni-ip-only-while-its-sg-allows-it`:
`Error: Check timed out after 60.0s`, first divergence in the fixture's own `_poll` helper
(`_poll#3|Timeout|time.sleep(interval_s)`), unchanged by each of: making a container reach another
workload's address at all, gating that reach by the destination's security groups, writing the
`-w`/`--write-out` format a status-only probe asks for, and reading the interpreter's own options in front
of `-m http.server`. The sibling check of the same fixture
(`ecs__logs.deep-ecs-guest-identity-task-role-is-the-guests-credential-source-and-identity`) fails in the
same helper, which is consistent with the fixture's guest program — not the network — being what does not
run.

## What a fix needs
Either:
* the execute-command SESSION: a data channel the `streamUrl` names that a client can open, the stream that
  carries a command's output back, and the `ExecuteCommand` response that names them — the core's own
  listener work, since `src/state/exec_sessions.rs` says no upgrade is answered; or
* a container's session running a guest PROGRAM in a language: the Python engine the workspace already
  locks, wired into `src/state/container_shell.rs` the way `…ssm…/state/guest_python.rs` wires it into an
  instance guest (the container's own filesystem, environment, streams and bounds), WITH a network binding
  for the request such a program makes — resolved through the shared served-address register and gated by
  `src/state/task_reachability.rs`, so the rule already implemented decides it there too.

## Steps to Reproduce
1. Build and boot the emulator from this checkout and replay suite `simparity-behavioral-aws-v1`, check
   `ecs__logs.deep-ecs-guest-identity-awsvpc-task-reachable-on-its-eni-ip-only-while-its-sg-allows-it`.
2. `RunTask` a Fargate `awsvpc` task whose container runs a guest script in a language (not a shell
   program), with an `awslogs` `logConfiguration`; poll `DescribeTasks` and `GetLogEvents` — the container
   reports no exit code and its stream stays empty.
3. `ExecuteCommand` against a task run with `enableExecuteCommand` and a running agent — the call is
   answered `TargetNotConnectedException`, so no command runs inside the task from outside it.
