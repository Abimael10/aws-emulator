// SPDX-License-Identifier: CC0-1.0

//! The SESSION an `ExecuteCommand` asks for: what one IS, and the sentences a call that
//! cannot be given one carries.
//!
//! `ExecuteCommandResponse$session` is "the details for the execute command session": an id,
//! a `streamUrl` — "a URL to the managed agent on the container that the SSM Session Manager
//! client uses to send commands and receive output from the container" — and a `tokenValue`
//! the client authenticates that connection with. So a session is not a record: it IS the
//! stream, carried over the Systems Manager data channel that URL names, which a client
//! opens as a WebSocket.
//!
//! This emulator answers no WebSocket upgrade, so nothing of it can carry that stream, and
//! there is therefore no session to hand a caller — an id and a URL on their own would be an
//! address nothing answers. What a call is told instead is decided by the task's own state
//! ([`super::execute_command_agent`]), in the two conditions Amazon ECS models for it: a
//! container with no running execute-command agent is the `InvalidParameterException` below,
//! and a container that HAS one is a target the channel does not reach, which is
//! `TargetNotConnectedException`.

/// `InvalidParameterException$message` for an `ExecuteCommand` a session cannot be opened
/// for because no execute-command agent of the task is there to open it on.
///
/// ONE fixed sentence states BOTH conditions that leave a task without such an agent —
/// the task was not run with `enableExecuteCommand`, or its agent is not running — so a
/// caller is told what to change without being told which of the two held, and the
/// sentence carries nothing of the request.
pub const EXECUTE_COMMAND_NOT_AVAILABLE_MESSAGE: &str =
    "The execute command failed because execute command was not enabled when the task was \
     run or the execute command agent isn't running. Wait and try again or run a new task \
     with execute command enabled and try again.";

/// `InvalidParameterException$message` for an `ExecuteCommand` asking for a session of a kind
/// Amazon ECS does not open. The rule and its wording are the member's own:
/// `ExecuteCommandRequest$interactive` — "Use this flag to run your command in interactive
/// mode. Amazon ECS only supports initiating interactive sessions, so you must specify true
/// for this value." The sentence is fixed text about the member and carries nothing of the
/// request.
pub const EXECUTE_COMMAND_INTERACTIVE_ONLY_MESSAGE: &str =
    "Amazon ECS only supports initiating interactive sessions, so you must specify true for \
     this value.";

/// `TargetNotConnectedException$message` for an `ExecuteCommand` whose target container IS
/// running its execute-command agent, so what is left is the channel the session's stream is
/// carried over (see this module's own documentation).
///
/// `TargetNotConnectedException` is the error class for exactly that: "The execute command
/// cannot run. This error can be caused by any of the following configuration issues: …The
/// SSM agent is not installed or is not running… There is an interface Amazon VPC endpoint
/// for Amazon ECS, but there is not one for Systems Manager Session Manager." The sentence is
/// the one AWS publishes for this error class in its Amazon ECS Exec troubleshooting
/// guidance: fixed text, carrying nothing of the request.
pub const EXECUTE_COMMAND_TARGET_NOT_CONNECTED_MESSAGE: &str =
    "The execute command failed due to an internal error. Wait and try again.";
