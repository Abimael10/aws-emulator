Issue-Severity: CRITICAL
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecs.exec-disabled-task-session-rejected-while-enabled-streams ecs.exec-file-write-visible-to-second-session ecs.exec-interactive-stdout-round-trip ecs.exec-session-resize-propagates-to-pty ecs.exec-targets-the-named-container-not-a-sibling ecs.execute-command-session-credentials-round-trip ecs.execute-command-succeeds-when-enabled-at-runtime
Issue-Shape-Id: com.amazonaws.ecs#AmazonEC2ContainerServiceV20141113
Issue-Key: ecs.exec-disabled-task-session-rejected-while-enabled-streams|_wait_for_exec_agent#11|AssertionError|raise AssertionError(
Issue-Key: ecs.exec-file-write-visible-to-second-session|_wait_for_exec_agent#10|AssertionError|raise AssertionError(
Issue-Key: ecs.exec-interactive-stdout-round-trip|_wait_for_exec_agent#9|AssertionError|raise AssertionError(
Issue-Key: ecs.exec-session-resize-propagates-to-pty|_wait_for_exec_agent#7|AssertionError|raise AssertionError(
Issue-Key: ecs.exec-targets-the-named-container-not-a-sibling|_wait_for_exec_agents#9|AssertionError|raise AssertionError(
Issue-Key: ecs.execute-command-session-credentials-round-trip|_wait_for_exec_agent_ready#18|AssertionError|raise AssertionError(f"task stopped before becoming ready for exec: {last_task.get('stoppedReason')}")
Issue-Key: ecs.execute-command-succeeds-when-enabled-at-runtime|run#7|AssertionError|raise AssertionError(f"task stopped before exec agent connected: {task.get('stoppedReason')}")

# ECS Exec: no session STREAM is served for a task that is execable, so ExecuteCommand answers TargetNotConnectedException

> _Filed while fixing `20261111T145804Z-ecs-task-data-plane-container-commands-never-execute.md` (Cause 2 of 7). That issue's other causes are resolved; this one is NOT, and it is tracked here on its own rather than closed with them._

## Operation
ExecuteCommand

## Description
A task launched with `enableExecuteCommand=true` now stays `RUNNING` and its containers report the
`ExecuteCommandAgent` managed agent reaching `lastStatus` `RUNNING`
(`src/state/execute_command_agent.rs`), so the condition `ExecuteCommand` is decided by is met —
what is missing is the SESSION itself.

AWS: `ExecuteCommandResponse$session` is "the details for the execute command session": a session
id, a `streamUrl` — "a URL to the managed agent on the container that the SSM Session Manager
client uses to send commands and receive output from the container" — and a `tokenValue` the client
authenticates that connection with. The client then opens that URL as a WebSocket and speaks the
Systems Manager data-channel protocol over it: an interactive session's standard output comes back
over the channel, a second session on the same container sees the first session's filesystem
writes, a resize propagates to the pty, and a session asked for against a task launched WITHOUT the
flag is refused.

Emulator: `src/ops/execute_command.rs` answers `TargetNotConnectedException` with
`EXECUTE_COMMAND_TARGET_NOT_CONNECTED_MESSAGE` for exactly this case
(`src/state/exec_sessions.rs`), because nothing serves the stream. Returning a session id and a
`streamUrl` without serving it would be an address nothing answers, so the refusal is honest — but
it means no exec workflow can be emulated at all.

## Why it matters
ECS Exec is the standard way to get a shell into a running task. A client cannot open, stream,
target, resize or correctly have refused a session, so no interactive-debugging workflow can be
emulated, and the negative case (exec against a task launched without the flag) cannot be
exercised either.

## What a fix needs
The emulator's front door and its `listeners().http(..)` resource listeners both refuse a WebSocket
upgrade (`vendor/server/src/listeners.rs:549`), so the channel cannot be served there. A service
CAN serve WebSockets over a core-owned socket: `service-com-amazonaws-apigatewayv2-apigatewayv2`'s
`src/service/websocket/transport.rs` completes the RFC 6455 handshake and carries frames over
`engine.listeners().tcp_plaintext(..)` with `tokio-tungstenite`, with in-band TLS
(`service_common::in_band_tls`), which is the pattern this needs. On top of that socket the fix has
to carry the Systems Manager data-channel framing the SSM Session Manager client speaks, hand each
session's input to the container's own session (`src/state/container_shell.rs`) so what the command
writes comes back on the channel, and report the session's `streamUrl`/`tokenValue` for that
listener. That is a data plane of its own, which is why it is tracked separately here.

## Steps to Reproduce
1. `RegisterTaskDefinition` with one essential container and a long-lived command (`sleep 3600`).
2. `RunTask` it with `enableExecuteCommand=true`.
3. Poll `DescribeTasks` until `containers[0].managedAgents` carries `ExecuteCommandAgent` with
   `lastStatus` `RUNNING` -> it does.
4. Call `ExecuteCommand` with `interactive=true`, the task's ARN, the container's name and
   `command=/bin/sh` -> `TargetNotConnectedException`, instead of a `session` whose `streamUrl`
   carries the interactive stream.
