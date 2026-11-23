Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecs.exec-interactive-stdout-round-trip ecs.execute-command-session-credentials-round-trip
Issue-Shape-Id: com.amazonaws.ecs#AmazonEC2ContainerServiceV20141113
Issue-Key: ecs.exec-interactive-stdout-round-trip|_wait_for_exec_agent#9|Timeout|time.sleep(5)
Issue-Key: ecs.execute-command-session-credentials-round-trip|_wait_for_exec_agent_ready#18|Timeout|time.sleep(interval_s)

# ecs: ExecuteCommand opens no data channel, so an exec-enabled task's session carries no container output

> _Filed while fixing `20261113T124512Z-ecs-8-net-new-causes-container-dependson-health.md` (its Cause 4 of 8). That change implements the other causes of that issue — the container `dependsOn` give-up and the CloudWatch alarms a rollout is watched by — and leaves this one untouched, because serving it is a change of its own rather than a part of either of those rules. These two checks' replay keys are carried here so nothing records them as working._

## Operation
ExecuteCommand

## Description
AWS: `ExecuteCommand` on a `RUNNING` task launched with `enableExecuteCommand=true`, whose
`ExecuteCommandAgent` managed agent is running, answers 200 with a `Session` — `sessionId`,
`streamUrl` and `tokenValue` — and that `streamUrl` ("a URL to the managed agent on the container
that the SSM Session Manager client uses to send commands and receive output", a
`wss://ssmmessages.<region>.amazonaws.com/v1/data-channel/<session-id>` address) opens a WORKING
data channel: the client types into the container's PTY, reads its standard output and standard
error back, and reads the session's own credentials round trip.

Emulator: the store already holds everything the SESSION needs — the flag the launch recorded and
the agent the clock brought up with the container (`src/state/execute_command_agent.rs`) — and the
handler resolves the cluster, task and container from it, but the call then earns the modelled
`TargetNotConnectedException` rather than a session
(`src/ops/execute_command.rs`, the last branch of `handle`), because this emulator serves nothing
that could carry the stream: answering a `session` instead would hand the caller an address nothing
serves and a token for a stream nothing carries.

## Why it matters
`ecs exec` is the primary debugging path into a task. Both recorded checks spend their whole budget
without reaching a single stdout or credential assertion, and a client cannot tell "the agent is not
up" from "the channel is not served", because both conditions answer with the same one error.

## What a fix needs
The data plane, not another branch in the handler:

1. A SOCKET the announced `streamUrl` leads to. The emulator's front door serves one buffered
   request per call and the core HTTP listener refuses a WebSocket upgrade, so this needs a
   core-owned listener of the kind a native-protocol resource gets
   (`engine.listeners().tcp_plaintext(handler)`), with the opening handshake and the frames carried
   by a maintained crate — the pattern
   `services/service-com-amazonaws-apigatewayv2-apigatewayv2/src/service/websocket/transport.rs`
   already establishes in this tree (in-band TLS through `service_common::in_band_tls`, frames
   through `tokio_tungstenite`).
2. The PROTOCOL that socket then speaks: the Systems Manager message-gateway agent-message framing
   and its handshake, which is what a session-manager client sends and expects, so that
   `input_stream_data` reaching the emulator is what the container's session reads and what that
   session writes leaves as `output_stream_data`. Anything else is an endpoint the real client
   cannot use.
3. The BRIDGE from that channel to the container's own session (`src/state/container_shell.rs`),
   whose output today goes to the log driver its definition configures: an exec session needs that
   output delivered to the channel instead, and needs a session that stays open across the lines a
   client types rather than one program run to its end.
4. The session's own identity: `sessionId`, and a `tokenValue` minted through the credential store
   (`ctx.credential_store().create(&AWS_SESSION)`) rather than constructed, with the `streamUrl`
   built from the listener's own address (`ctx.endpoint()`-style) so it leads back to this emulator.

Adjacent open issues describing neighbouring symptoms of the same gap, neither carrying these two
checks' replay keys: `20261104T062603Z-ecs-exec-the-executecommandagent-managed-agent-never-reaches.md`
and `20261120T154211Z-ecs-2-net-new-causes-container-dependson-conditions-never.md` (its Cause 2).

## Evidence
`('TargetNotConnectedException', 'The execute command failed due to an internal error. Wait and try
again.')` where AWS answers 200 with a `session`; both checks then end with
`Error: Check timed out after 60.0s` before any stdout or credential assertion is reached.

## Steps to Reproduce
1. `CreateCluster`; `RegisterTaskDefinition` with a task role that may call Systems Manager.
2. `RunTask` with `enableExecuteCommand=true` and poll `DescribeTasks` until the container's
   `ExecuteCommandAgent` managed agent reports `RUNNING`.
3. `ExecuteCommand` with `interactive=true` and a command of `/bin/sh` → HTTP 400
   `TargetNotConnectedException`, where AWS returns a `session` whose `streamUrl` the client then
   opens to type into the container and read its output back.
