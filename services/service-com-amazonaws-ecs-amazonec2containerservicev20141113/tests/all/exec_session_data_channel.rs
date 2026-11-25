// SPDX-License-Identifier: CC0-1.0

//! The SESSION an `ExecuteCommand` opens, driven end to end: the response it is reported in,
//! the data channel its `streamUrl` leads to, and the container's own session the stream
//! carries.
//!
//! # The rule under test
//!
//! > `ExecuteCommand` on a `RUNNING` task that was launched with `enableExecuteCommand` and
//! > whose `ExecuteCommandAgent` managed agent is RUNNING answers 200 with a `Session` — a
//! > `sessionId`, a `streamUrl` and a `tokenValue` — and that `streamUrl` is a data channel
//! > THIS emulator serves: a client that opens it and presents that session's token types
//! > into the container's own session and reads back what the container wrote, so a file one
//! > session writes is a file the next session of the same container reads, and the terminal
//! > size the client states is the terminal that session runs in. A token is one session's:
//! > another session's token, a token presented twice and a session that is over each open no
//! > stream. `TargetNotConnectedException` is left for a target nothing can carry a stream to.
//!
//! # What the rows span
//!
//! The commands a client types (what each writes, the status it answers, the file it leaves
//! behind, the terminal it reports), which container of a two-container task the session is
//! opened on, the sizes a client resizes its terminal to, and every way a connection can
//! present a token that is not the one its session was opened with.
//!
//! Nothing is asserted off the store: every session is opened through `ExecuteCommand`'s own
//! wire call and every byte is read off a real connection to the announced channel, framed the
//! way a Session Manager client frames it.

use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use service_com_amazonaws_ecs_amazonec2containerservicev20141113::service::exec_data_channel::agent_message::{
    self, AgentMessage,
};
use smithy_server_runtime::MultiServiceRouter;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;

use crate::ecs_wire::{cluster_with_capacity, list, ok, refusal, service, string, ACCOUNT};
use crate::task_lifecycle::container;

/// The cluster every task of this suite runs in.
const CLUSTER: &str = "execsessions";

/// The task-definition family they run, with TWO containers so a session is opened on the
/// container the request names and on no sibling of it.
const FAMILY: &str = "execsessionfamily";

/// The two containers that family declares.
const FIRST_CONTAINER: &str = "app";
const SECOND_CONTAINER: &str = "sidecar";

/// The managed agent the execute command feature runs inside a container
/// (`smithy_query(query="shape",shape="ManagedAgentName")` -> `ExecuteCommandAgent`).
const EXECUTE_COMMAND_AGENT: &str = "ExecuteCommandAgent";

/// The status that agent reports once it is connected.
const RUNNING: &str = "RUNNING";

/// How long a case waits for one frame off the channel before failing: generous for a loaded
/// test machine, and bounded so a case nothing answers fails instead of hanging the suite.
const FRAME_DEADLINE: Duration = Duration::from_secs(30);

/// How long a case keeps re-reading a task it is waiting on, and how long it keeps trying: the
/// start and the agent are clock-owed, so the condition is polled for rather than slept on.
const POLL_INTERVAL: Duration = Duration::from_millis(100);
const POLL_BUDGET: Duration = Duration::from_secs(60);

/// The status a completed opening handshake carries (RFC 6455 §4.1).
const SWITCHING_PROTOCOLS: u16 = 101;

/// The loopback host the core binds a resource listener on by default, and so the only address
/// these cases ever dial.
const LOOPBACK_HOST: &str = "127.0.0.1";

/// The socket a client gets from a completed handshake.
type ClientSocket = tokio_tungstenite::WebSocketStream<TcpStream>;

/// A client of one session's data channel: the socket, and the sequence its own messages stand
/// in — a Session Manager client numbers what it sends.
///
/// Reachable from the suites beside this one, so a case about the sessions of another task
/// shape drives the channel through this one client rather than a second copy of it.
pub(crate) struct Channel {
    socket: ClientSocket,
    written: i64,
}

impl Channel {
    /// Send one `input_stream_data` message carrying `payload` of `payload_type`, as a client
    /// sends its keystrokes, its terminal size and its handshake answer.
    async fn send(&mut self, case: &str, payload_type: u32, payload: Vec<u8>) {
        let sequence = self.written;
        self.written += 1;
        let message = AgentMessage::new(
            agent_message::INPUT_STREAM_DATA,
            sequence,
            payload_type,
            payload,
            [u8::try_from(sequence % 251).unwrap_or_default(); 16],
            0,
            if sequence == 0 { agent_message::FLAG_FIRST } else { 0 },
        );
        self.socket
            .send(Message::Binary(message.encode().into()))
            .await
            .unwrap_or_else(|error| {
                panic!("case {case:?}: the client's message must be sent, got {error}")
            });
    }

    /// Type one command LINE into the session, as a client types it: the characters and the
    /// return that ends the line.
    pub(crate) async fn type_line(&mut self, case: &str, line: &str) {
        self.send(
            case,
            agent_message::PAYLOAD_TYPE_OUTPUT,
            format!("{line}\n").into_bytes(),
        )
        .await;
    }

    /// State the size of the client's terminal, as a client does when its window changes.
    pub(crate) async fn resize(&mut self, case: &str, rows: u32, columns: u32) {
        self.send(
            case,
            agent_message::PAYLOAD_TYPE_SIZE,
            json!({ "cols": columns, "rows": rows }).to_string().into_bytes(),
        )
        .await;
    }

    /// The next agent message the channel delivers, or `None` when none arrives within
    /// [`FRAME_DEADLINE`].
    pub(crate) async fn next_message(&mut self, case: &str) -> Option<AgentMessage> {
        match tokio::time::timeout(FRAME_DEADLINE, self.socket.next()).await {
            Ok(Some(Ok(Message::Binary(bytes)))) => Some(
                AgentMessage::decode(&bytes)
                    .unwrap_or_else(|| panic!("case {case:?}: a frame is not an agent message")),
            ),
            Ok(Some(Ok(Message::Close(_)))) | Ok(None) => None,
            Ok(Some(Ok(other))) => panic!("case {case:?}: unexpected frame {other:?}"),
            Ok(Some(Err(error))) => panic!("case {case:?}: the channel failed: {error}"),
            Err(_elapsed) => None,
        }
    }

    /// Read the stream until everything it has delivered so far CONTAINS `wanted`, and answer
    /// what was read. The output of a line is written as the line runs, so a case reads until
    /// what it is waiting for is there rather than assuming one message carries it.
    pub(crate) async fn read_until(&mut self, case: &str, wanted: &str) -> String {
        let mut read = String::new();
        while !read.contains(wanted) {
            let Some(message) = self.next_message(case).await else {
                panic!(
                    "case {case:?}: expected the stream to carry {wanted:?}, got {read:?} \
                     before it ended"
                );
            };
            if message.message_type == agent_message::OUTPUT_STREAM_DATA
                && message.payload_type == agent_message::PAYLOAD_TYPE_OUTPUT
            {
                read.push_str(message.payload_text().unwrap_or_default());
            }
        }
        read
    }
}

/// A container definition of a family whose task stays RUNNING while its sessions are carried:
/// the harness's minimal definition plus a program that runs for longer than a suite takes.
///
/// Reachable from the suites beside this one, which launch tasks of the same shape.
pub(crate) fn long_running_container(name: &str) -> Value {
    let mut definition = container(name, "public.ecr.aws/nginx/nginx:latest");
    definition["command"] = json!(["sleep", "600"]);
    definition
}

/// A router holding one cluster with capacity and one revision of [`FAMILY`] declaring a task
/// role — an exec-enabled launch needs one.
async fn fixture() -> MultiServiceRouter {
    let router = service();
    cluster_with_capacity(&router, CLUSTER, "fixture").await;
    ok(
        &router,
        "RegisterTaskDefinition",
        json!({
            "family": FAMILY,
            "taskRoleArn": format!("arn:aws:iam::{ACCOUNT}:role/execSessionTaskRole"),
            "containerDefinitions": [
                long_running_container(FIRST_CONTAINER),
                long_running_container(SECOND_CONTAINER),
            ],
        }),
        "fixture",
    )
    .await;
    router
}

/// One task of [`FAMILY`] launched with execute command enabled, waited for until it is
/// `RUNNING` with the agent of both its containers connected — the condition a session is
/// opened on.
async fn running_task(router: &MultiServiceRouter, case: &str) -> String {
    let body = ok(
        router,
        "RunTask",
        json!({
            "cluster": CLUSTER,
            "taskDefinition": FAMILY,
            "count": 1,
            "enableExecuteCommand": true,
        }),
        case,
    )
    .await;
    let tasks = list(&body, "tasks", case);
    let arn = string(&tasks[0], "taskArn", case);
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        let task = describe_task(router, &arn, case).await;
        let connected = [FIRST_CONTAINER, SECOND_CONTAINER]
            .iter()
            .all(|name| agent_status(&task, name, case).as_deref() == Some(RUNNING));
        if task["lastStatus"].as_str() == Some(RUNNING) && connected {
            return arn;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the task to reach RUNNING with its agents connected, got \
             {task}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One task read back through `DescribeTasks`.
async fn describe_task(router: &MultiServiceRouter, task_arn: &str, case: &str) -> Value {
    let body = ok(
        router,
        "DescribeTasks",
        json!({ "cluster": CLUSTER, "tasks": [task_arn] }),
        case,
    )
    .await;
    list(&body, "tasks", case)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: expected the task back, got {body}"))
}

/// The `lastStatus` the execute-command agent of `container_name` reports in a described task.
pub(crate) fn agent_status(task: &Value, container_name: &str, case: &str) -> Option<String> {
    let container = list(task, "containers", case)
        .into_iter()
        .find(|container| container["name"].as_str() == Some(container_name))?;
    list(&container, "managedAgents", case)
        .into_iter()
        .find(|agent| agent["name"].as_str() == Some(EXECUTE_COMMAND_AGENT))
        .map(|agent| agent["lastStatus"].as_str().unwrap_or_default().to_string())
}

/// `ExecuteCommand` on one container of `task_arn`, answered 200: the whole response.
async fn execute_command(
    router: &MultiServiceRouter,
    task_arn: &str,
    container_name: Option<&str>,
    command: &str,
    case: &str,
) -> Value {
    let mut request = json!({
        "cluster": CLUSTER,
        "task": task_arn,
        "command": command,
        "interactive": true,
    });
    if let Some(name) = container_name {
        request["container"] = json!(name);
    }
    ok(router, "ExecuteCommand", request, case).await
}

/// The port of an announced `streamUrl`, having checked its host is [`LOOPBACK_HOST`] — so the
/// dial below names that literal and reaches nothing else — and the path it carries.
fn dialled(case: &str, stream_url: &str) -> (u16, String) {
    let rest = stream_url
        .strip_prefix("wss://")
        .unwrap_or_else(|| panic!("case {case:?}: expected a wss:// URL, got {stream_url:?}"));
    let (authority, path) = rest
        .split_once('/')
        .unwrap_or_else(|| panic!("case {case:?}: expected a path, got {stream_url:?}"));
    let (host, port) = authority
        .rsplit_once(':')
        .unwrap_or_else(|| panic!("case {case:?}: expected a host:port, got {authority:?}"));
    assert_eq!(
        host, LOOPBACK_HOST,
        "case {case:?}: a session's channel must be served on this emulator's own loopback \
         listener, got {authority:?}"
    );
    let port = port
        .parse()
        .unwrap_or_else(|_| panic!("case {case:?}: expected a port, got {authority:?}"));
    (port, format!("/{path}"))
}

/// Open the data channel `stream_url` names and present `token` on it, as a Session Manager
/// client does: the established channel, or `None` when the channel closed instead of
/// carrying the session.
pub(crate) async fn dial(case: &str, stream_url: &str, token: &str) -> Option<Channel> {
    let (port, path) = dialled(case, stream_url);
    // The emulator's own loopback port, named as a literal: a case dials this address and
    // nothing else ([`dialled`] has already held the announced host to it).
    let stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap_or_else(|error| {
            panic!("case {case:?}: the announced channel must accept a connection, got {error}")
        });
    let (socket, response) =
        tokio_tungstenite::client_async(format!("ws://{LOOPBACK_HOST}:{port}{path}"), stream)
            .await
            .unwrap_or_else(|error| {
                panic!("case {case:?}: the opening handshake must complete, got {error}")
            });
    assert_eq!(
        response.status().as_u16(),
        SWITCHING_PROTOCOLS,
        "case {case:?}: a completed handshake must answer 101"
    );
    let mut channel = Channel { socket, written: 0 };
    // The opening message a Session Manager client sends on a data channel, carrying the
    // token the call that minted the session answered.
    channel
        .socket
        .send(Message::Text(
            json!({
                "MessageSchemaVersion": "1.0",
                "RequestId": "00000000-0000-0000-0000-000000000001",
                "TokenValue": token,
                "ClientId": "00000000-0000-0000-0000-000000000002",
                "ClientVersion": "1.2.0.0",
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap_or_else(|error| {
            panic!("case {case:?}: the opening message must be sent, got {error}")
        });
    // The agent states the session it is about to carry; a client answers that handshake and
    // reads the completion, after which the stream itself starts.
    let first = channel.next_message(case).await?;
    if first.message_type == agent_message::CHANNEL_CLOSED {
        return None;
    }
    assert_eq!(
        (first.message_type.as_str(), first.payload_type),
        (
            agent_message::OUTPUT_STREAM_DATA,
            agent_message::PAYLOAD_TYPE_HANDSHAKE_REQUEST
        ),
        "case {case:?}: expected the agent's handshake request first, got {first:?}"
    );
    channel
        .send(
            case,
            agent_message::PAYLOAD_TYPE_HANDSHAKE_RESPONSE,
            json!({
                "ClientVersion": "1.2.0.0",
                "ProcessedClientActions": [{ "ActionType": "SessionType", "ActionStatus": 1 }],
            })
            .to_string()
            .into_bytes(),
        )
        .await;
    Some(channel)
}

/// The session members of an `ExecuteCommand` response: its id, the channel it is carried on
/// and the token that channel admits.
pub(crate) fn session_of(answer: &Value, case: &str) -> (String, String, String) {
    let session = &answer["session"];
    (
        string(session, "sessionId", case),
        string(session, "streamUrl", case),
        string(session, "tokenValue", case),
    )
}

#[tokio::test]
async fn an_exec_session_runs_what_the_client_types_inside_the_container() {
    let router = fixture().await;
    let task_arn = running_task(&router, "a task run with execute command").await;
    let case = "the session of an exec-enabled task";
    let answer = execute_command(&router, &task_arn, Some(FIRST_CONTAINER), "/bin/sh", case).await;

    // The response is the session and the resources it was opened on, all read back off the
    // call rather than composed by the client.
    assert_eq!(
        answer["containerName"].as_str(),
        Some(FIRST_CONTAINER),
        "case {case:?}: expected the container the request named, got {answer}"
    );
    assert_eq!(
        answer["taskArn"].as_str(),
        Some(task_arn.as_str()),
        "case {case:?}: expected the task the request named, got {answer}"
    );
    assert_eq!(
        answer["interactive"].as_bool(),
        Some(true),
        "case {case:?}: Amazon ECS only initiates interactive sessions, got {answer}"
    );
    let (session_id, stream_url, token) = session_of(&answer, case);
    assert!(
        stream_url.contains(&session_id),
        "case {case:?}: expected the channel to name its session, got {stream_url:?}"
    );
    assert!(
        !token.is_empty() && !token.contains(&session_id),
        "case {case:?}: expected a minted token, got {token:?}"
    );

    let mut channel = dial(case, &stream_url, &token)
        .await
        .unwrap_or_else(|| panic!("case {case:?}: the session's channel must carry it"));

    // The rows span what a client types: a command that WRITES, one whose output is computed
    // from the container's own environment, one that leaves a FILE behind, one that reports
    // the terminal the session runs in, and one whose status the next command reads.
    let rows: [(&str, &str, &str); 6] = [
        ("a command that writes a line", "echo hello-exec", "hello-exec"),
        (
            "a command whose output is formatted",
            "printf 'one %s\\n' two",
            "one two",
        ),
        (
            "a command that writes a file",
            "echo marker > /tmp/exec-written && cat /tmp/exec-written",
            "marker",
        ),
        (
            "a command that reads the file back",
            "cat /tmp/exec-written",
            "marker",
        ),
        (
            "a command reporting the terminal of the session",
            "stty size",
            "24 80",
        ),
        (
            "a command whose status the next one reads",
            "false; echo status-$?",
            "status-1",
        ),
    ];
    for (row, typed, expected) in rows {
        let case = format!("{case}: {row}");
        channel.type_line(&case, typed).await;
        let read = channel.read_until(&case, expected).await;
        assert!(
            read.contains(expected),
            "case {case:?}: expected the stream to carry {expected:?}, got {read:?}"
        );
    }

    // The terminal the session runs in is the one the CLIENT states, so a resize is read back
    // by the next command that reports it. The rows span the sizes a client resizes to.
    for (rows_, columns) in [(40u32, 120u32), (12, 200)] {
        let case = format!("{case}: a terminal of {rows_} by {columns}");
        channel.resize(&case, rows_, columns).await;
        channel.type_line(&case, "stty size").await;
        let wanted = format!("{rows_} {columns}");
        let read = channel.read_until(&case, &wanted).await;
        assert!(
            read.contains(&wanted),
            "case {case:?}: expected the resized terminal {wanted:?}, got {read:?}"
        );
    }

    // A SECOND session of the same container reads what the first one wrote: both run over the
    // container's own filesystem.
    let second = execute_command(
        &router,
        &task_arn,
        Some(FIRST_CONTAINER),
        "/bin/sh",
        "a second session of the same container",
    )
    .await;
    let (second_id, second_url, second_token) =
        session_of(&second, "a second session of the same container");
    assert_ne!(
        second_id, session_id,
        "expected every session to carry an id of its own"
    );
    assert_ne!(
        second_token, token,
        "expected every session to carry a token of its own"
    );
    let mut second_channel = dial(
        "a second session of the same container",
        &second_url,
        &second_token,
    )
    .await
    .expect("the second session's channel must carry it");
    second_channel
        .type_line("a second session of the same container", "cat /tmp/exec-written")
        .await;
    let read = second_channel
        .read_until("a second session of the same container", "marker")
        .await;
    assert!(
        read.contains("marker"),
        "expected the second session to read the file the first wrote, got {read:?}"
    );

    // A session opened on the SIBLING container reads none of it: the two containers are two
    // filesystems, and a session runs in the one the request named.
    let sibling = execute_command(
        &router,
        &task_arn,
        Some(SECOND_CONTAINER),
        "/bin/sh",
        "a session of the sibling container",
    )
    .await;
    assert_eq!(
        sibling["containerName"].as_str(),
        Some(SECOND_CONTAINER),
        "expected the session to be opened on the container the request named, got {sibling}"
    );
    let (_, sibling_url, sibling_token) = session_of(&sibling, "a session of the sibling container");
    let mut sibling_channel = dial(
        "a session of the sibling container",
        &sibling_url,
        &sibling_token,
    )
    .await
    .expect("the sibling session's channel must carry it");
    sibling_channel
        .type_line(
            "a session of the sibling container",
            "cat /tmp/exec-written; echo sibling-done",
        )
        .await;
    let read = sibling_channel
        .read_until("a session of the sibling container", "sibling-done")
        .await;
    assert!(
        !read.contains("marker"),
        "expected the sibling container's session to read none of the other's files, got \
         {read:?}"
    );
}

#[tokio::test]
async fn a_channel_carries_only_the_session_whose_token_it_presents() {
    let router = fixture().await;
    let task_arn = running_task(&router, "a task run with execute command").await;
    let first = execute_command(
        &router,
        &task_arn,
        Some(FIRST_CONTAINER),
        "/bin/sh",
        "the first session",
    )
    .await;
    let second = execute_command(
        &router,
        &task_arn,
        Some(SECOND_CONTAINER),
        "/bin/sh",
        "the second session",
    )
    .await;
    let (_, first_url, first_token) = session_of(&first, "the first session");
    let (_, second_url, second_token) = session_of(&second, "the second session");

    // The rows span every way a connection can present a token that is not the one its session
    // was opened with.
    let rows: [(&str, &str, &str); 3] = [
        ("another session's token", first_url.as_str(), second_token.as_str()),
        ("a token of no session at all", first_url.as_str(), "not-a-token"),
        ("an empty token", second_url.as_str(), ""),
    ];
    for (row, url, token) in rows {
        assert!(
            dial(row, url, token).await.is_none(),
            "case {row:?}: expected the channel to carry no session"
        );
    }

    // The session's OWN token carries it, and a second connection presenting the same token
    // carries nothing: a session's stream is carried once.
    let carried = dial("the session's own token", &first_url, &first_token)
        .await
        .expect("a session's own token must carry it");
    assert!(
        dial("the same token twice", &first_url, &first_token)
            .await
            .is_none(),
        "expected a second connection on one token to carry no session"
    );
    drop(carried);
}

#[tokio::test]
async fn a_session_that_the_shell_exited_carries_nothing_further() {
    let router = fixture().await;
    let task_arn = running_task(&router, "a task run with execute command").await;
    let case = "a session whose shell exits";
    let answer = execute_command(&router, &task_arn, Some(FIRST_CONTAINER), "/bin/sh", case).await;
    let (_, stream_url, token) = session_of(&answer, case);
    let mut channel = dial(case, &stream_url, &token)
        .await
        .unwrap_or_else(|| panic!("case {case:?}: the session's channel must carry it"));
    channel.type_line(case, "echo before-exit").await;
    let read = channel.read_until(case, "before-exit").await;
    assert!(
        read.contains("before-exit"),
        "case {case:?}: expected the line to run, got {read:?}"
    );

    // The shell EXITS, so the session it was is over: the channel says so, and the token that
    // opened it opens nothing again.
    channel.type_line(case, "exit").await;
    let mut closed = false;
    while let Some(message) = channel.next_message(case).await {
        if message.message_type == agent_message::CHANNEL_CLOSED {
            closed = true;
            break;
        }
    }
    assert!(
        closed,
        "case {case:?}: expected the channel to say the session is over"
    );
    assert!(
        dial(case, &stream_url, &token).await.is_none(),
        "case {case:?}: expected a session that is over to carry nothing further"
    );
}

#[tokio::test]
async fn a_task_launched_without_execute_command_is_refused_a_session() {
    let router = fixture().await;
    let body = ok(
        &router,
        "RunTask",
        json!({
            "cluster": CLUSTER,
            "taskDefinition": FAMILY,
            "count": 1,
            "enableExecuteCommand": false,
        }),
        "a task run without execute command",
    )
    .await;
    let task_arn = string(
        &list(&body, "tasks", "a task run without execute command")[0],
        "taskArn",
        "a task run without execute command",
    );
    // The task has to be RUNNING for the refusal to be about the agent rather than about the
    // task, and the start is clock-owed.
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        let task = describe_task(&router, &task_arn, "a task run without execute command").await;
        if task["lastStatus"].as_str() == Some(RUNNING) {
            assert!(
                agent_status(&task, FIRST_CONTAINER, "no agent").is_none(),
                "expected a task launched without the flag to carry no agent, got {task}"
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "expected the task to reach RUNNING, got {task}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    let (code, message) = refusal(
        &router,
        "ExecuteCommand",
        json!({
            "cluster": CLUSTER,
            "task": task_arn,
            "command": "/bin/sh",
            "interactive": true,
        }),
        "a task run without execute command",
    )
    .await;
    assert_eq!(
        (code.as_str(), message.as_str()),
        (
            "InvalidParameterException",
            "The execute command failed because execute command was not enabled when the task \
             was run or the execute command agent isn't running. Wait and try again or run a new \
             task with execute command enabled and try again."
        ),
        "expected the one sentence that states both agent conditions, got {code:?} with \
         {message:?}"
    );
}
