// SPDX-License-Identifier: CC0-1.0

//! The sessions this store holds, and what one needs to be CARRIED.
//!
//! # The rule this module implements
//!
//! `ExecuteCommand` "runs a command remotely on a container within a task", and what it
//! answers for a task whose execute-command agent is connected is a `Session`: an id, the
//! `streamUrl` of the data channel its stream is carried over, and the `tokenValue` that
//! channel admits the connection on. So a session is a record of this store only in as much
//! as the CHANNEL needs one: the connection a client then opens names the session on the
//! data-channel path and authenticates with the token, and everything the session then runs
//! — which container, under which command, over which filesystem and as which role — is read
//! back from here.
//!
//! The token is a SECRET handed to a caller. It is minted by the one component of this
//! emulator that generates secrets, its credential store, and removed from that store again
//! (see [`mint_exec_session_token`]), so it is never a credential anything of this emulator
//! accepts; what this store KEEPS of it is its digest ([`ExecSession::token_value`]), which is
//! all a session needs to recognise the token presented on its channel, and it is in no
//! response but the one that minted the token, no log and nothing the container can read.
//!
//! # Bounds and the clock
//!
//! A session is a record of a stream a client may or may not open, so at most
//! [`MAX_EXEC_SESSIONS`] are held at once and each lapses after
//! [`EXEC_SESSION_IDLE_TIMEOUT`] of inactivity, which is the Session Manager idle timeout a
//! session of a container is carried under. Both are materialised by the CLOCK
//! ([`ComAmazonawsEcsAmazonec2containerservicev20141113State::advance_clock`]) rather than by
//! whoever happens to call, so a session that has lapsed is one no channel admits whether or
//! not anybody looked.

use ::std::collections::BTreeMap;
use ::std::sync::Arc;
use ::std::time::{Duration, Instant, SystemTime};

use crate::state::*;

/// How long a session with no traffic on its channel lives for.
///
/// "Session Manager automatically disconnects an idle session after 20 minutes", which is the
/// idle timeout a session of a container is carried under, so a session nothing has used for
/// that long is over.
pub const EXEC_SESSION_IDLE_TIMEOUT: Duration = Duration::from_secs(20 * 60);

/// How many sessions this store holds at once. THIS NUMBER IS THE EMULATOR'S, not an AWS
/// quota: every session is a record this process holds and a stream it may be asked to carry,
/// so without a ceiling a caller opening sessions without end would grow the store without
/// end. Far above what a debugging scenario opens.
pub const MAX_EXEC_SESSIONS: usize = 64;

/// What every session id this service mints begins with, which is how Amazon ECS names the
/// Session Manager session of an execute-command call.
pub const EXEC_SESSION_ID_PREFIX: &str = "ecs-execute-command-";

/// How many ROWS the terminal of a session has until its client states a size of its own on
/// the channel: the conventional terminal a shell assumes before anything resizes it.
pub const DEFAULT_TERMINAL_ROWS: u32 = 24;

/// How many COLUMNS it has, under [`DEFAULT_TERMINAL_ROWS`]' rule.
pub const DEFAULT_TERMINAL_COLUMNS: u32 = 80;

/// ONE session this store holds.
pub struct ExecSession {
    /// `Session$sessionId`.
    pub session_id: String,
    /// `Session$tokenValue`, as the DIGEST of the secret the channel admits a connection on:
    /// the token itself is reported once, to the caller that opened the session, and this store
    /// keeps only what is needed to recognise it again ([`token_digest`]). Never reported,
    /// never logged — which is why this struct's `Debug` carries no field for it.
    token_value: String,
    /// `ExecuteCommandResponse$clusterArn`'s cluster, as this store keys clusters.
    pub cluster_name: String,
    /// The task the container is part of.
    pub task_arn: String,
    /// The container the command runs on.
    pub container_arn: String,
    /// That container's name, which the response reports.
    pub container_name: String,
    /// `ExecuteCommandRequest$command`, "the command to run on the container" — the program
    /// the session's stream carries.
    pub command: String,
    /// When it was opened.
    pub opened_at: SystemTime,
    /// When it was last used — the instant the clock measures [`EXEC_SESSION_IDLE_TIMEOUT`]
    /// from.
    pub used_at: SystemTime,
    /// Whether a data channel has been opened on it, so a second connection on one token
    /// opens nothing: a session's stream is carried once.
    pub connected: bool,
    /// Whether it is OVER: its shell exited, its channel closed, or the clock found it idle.
    pub ended: bool,
    /// The Region the task was placed in, which is the endpoint the Session Manager session
    /// of this one is held at ([`register_ssm_session`]).
    pub region: String,
}

/// Written by hand rather than derived: the token is a secret handed to a caller, so no
/// debug line of this emulator may carry it.
impl ::std::fmt::Debug for ExecSession {
    fn fmt(&self, formatter: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        formatter
            .debug_struct("ExecSession")
            .field("session_id", &self.session_id)
            .field("task_arn", &self.task_arn)
            .field("container_arn", &self.container_arn)
            .field("connected", &self.connected)
            .field("ended", &self.ended)
            .finish()
    }
}

/// The execute-command plane of this store: the sessions it holds, the socket their streams
/// are carried on, and what IAM answered about each task's agent reaching the channel.
#[derive(Default)]
pub struct ExecPlane {
    /// `sessionId` -> the session. An entry exists while the session does: the clock takes
    /// away the ones that have ended or lapsed ([`EXEC_SESSION_IDLE_TIMEOUT`]).
    pub(in crate::state) sessions: BTreeMap<String, ExecSession>,
    /// Monotonic source of session ids, so every session of this store is distinct.
    pub(in crate::state) next_session_seq: u64,
    /// The socket every session's data channel is served on, opened on the first session and
    /// kept for the life of the store ([`super::data_channel`]).
    pub(in crate::state) endpoint: Option<ExecDataChannelEndpoint>,
    /// `taskArn` -> whether that task's execute-command agent can reach the Systems Manager
    /// channel at all, as IAM answered it for the task's role
    /// ([`super::agent_connection`]). An entry is written once per task.
    pub(in crate::state) agent_connections: BTreeMap<String, bool>,
    /// The sessions that have ENDED and whose Session Manager record has not been ended with
    /// them yet, each with the Region it is held at: the SESSION is this store's and the
    /// RECORD is Systems Manager's, and no pass of this store may take that service's lock, so
    /// what is owed is left here for [`settle_exec_session_records`] to carry out. Bounded by
    /// the sessions this store holds ([`MAX_EXEC_SESSIONS`]), since only a session it held can
    /// end.
    pub(in crate::state) ended_elsewhere: Vec<(String, String)>,
}

/// Written by hand rather than derived: the endpoint holds a live socket and carries no
/// `Debug`, and what is diagnosable about this plane is how much of it is held.
impl ::std::fmt::Debug for ExecPlane {
    fn fmt(&self, formatter: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        formatter
            .debug_struct("ExecPlane")
            .field("sessions", &self.sessions.len())
            .field("endpoint", &self.endpoint.as_ref().map(ExecDataChannelEndpoint::authority))
            .field("agent_connections", &self.agent_connections.len())
            .finish()
    }
}

/// What one `ExecuteCommand` asks a session to be opened for, as the handler resolved it off
/// this store.
#[derive(Debug, Clone)]
pub struct ExecSessionRequest {
    /// The cluster the task is in, as this store keys clusters.
    pub cluster_name: String,
    /// The task the container is part of.
    pub task_arn: String,
    /// The container the command runs on.
    pub container_arn: String,
    /// That container's name.
    pub container_name: String,
    /// `ExecuteCommandRequest$command`.
    pub command: String,
    /// The token the channel admits the connection on, minted by the credential store
    /// ([`mint_exec_session_token`]).
    pub token_value: String,
    /// The Region the task was placed in, which is the endpoint the Session Manager record of
    /// this session is held at.
    pub region: String,
}

impl ComAmazonawsEcsAmazonec2containerservicev20141113State {
    /// Open a session for `request` and answer its id, or `None` when this store already
    /// holds as many sessions as it carries at once.
    ///
    /// The id is minted here, from this store's own counter and the instant the session was
    /// opened, so two sessions never share one and nothing of the request is read back out of
    /// it.
    pub fn open_exec_session(
        &mut self,
        request: ExecSessionRequest,
        now: SystemTime,
    ) -> Option<String> {
        self.expire_exec_sessions(now);
        if self.exec.sessions.len() >= MAX_EXEC_SESSIONS {
            // The ceiling is this emulator's, so it is TRACED rather than passed over in
            // silence: the caller is answered as a target no session could be opened on.
            ::tracing::warn!(
                limit = MAX_EXEC_SESSIONS,
                "this emulator is already holding as many execute-command sessions as it \
                 carries at once, so no session is opened"
            );
            return None;
        }
        self.exec.next_session_seq += 1;
        let session_id = exec_session_id(self.exec.next_session_seq, now);
        self.exec.sessions.insert(
            session_id.clone(),
            ExecSession {
                session_id: session_id.clone(),
                token_value: token_digest(&request.token_value),
                cluster_name: request.cluster_name,
                task_arn: request.task_arn,
                container_arn: request.container_arn,
                container_name: request.container_name,
                command: request.command,
                opened_at: now,
                used_at: now,
                connected: false,
                ended: false,
                region: request.region,
            },
        );
        Some(session_id)
    }

    /// The `host:port` every session's data channel is served on, and `None` until a socket
    /// has been opened for it ([`super::data_channel::ensure_exec_data_channel`]).
    pub fn exec_data_channel_authority(&self) -> Option<String> {
        self.exec
            .endpoint
            .as_ref()
            .map(|endpoint| endpoint.authority().to_string())
    }

    /// Whether a connection presenting `token` may carry the session `session_id` names, as
    /// of `now` — and record that it did.
    ///
    /// Four conditions, all of them the SESSION's: this store holds it, it has not ended, it
    /// has not been carried already (a session's stream is carried once), and the token
    /// presented is the one it was opened with. A session this admits is marked connected and
    /// used, so the clock measures its idle timeout from here.
    ///
    /// The token is compared over its DIGEST rather than over its characters
    /// ([`token_digest`]), so the comparison costs the same whatever a presented token shares
    /// with the one the session was opened with.
    pub fn admit_exec_data_channel(&mut self, session_id: &str, token: &str, now: SystemTime) -> bool {
        self.expire_exec_sessions(now);
        let Some(session) = self.exec.sessions.get_mut(session_id) else {
            return false;
        };
        if session.ended || session.connected || session.token_value != token_digest(token) {
            return false;
        }
        session.connected = true;
        session.used_at = now;
        true
    }

    /// Record that the session `session_id` names was used at `now`, which is what its idle
    /// timeout is measured from.
    pub fn touch_exec_session(&mut self, session_id: &str, now: SystemTime) {
        if let Some(session) = self.exec.sessions.get_mut(session_id) {
            session.used_at = now;
        }
    }

    /// Record that the session `session_id` names is OVER: its shell exited or its channel
    /// closed, so nothing opens it again — and that its Systems Manager record is owed the
    /// same end ([`settle_exec_session_records`]).
    pub fn end_exec_session(&mut self, session_id: &str) {
        let Some(session) = self.exec.sessions.get_mut(session_id) else {
            return;
        };
        if session.ended {
            return;
        }
        session.ended = true;
        session.connected = false;
        let owed = (session.region.clone(), session.session_id.clone());
        self.exec.ended_elsewhere.push(owed);
    }

    /// The sessions whose Session Manager record is owed its end, taking them off the list:
    /// each is carried out by [`settle_exec_session_records`], which holds no guard of this
    /// store.
    pub(in crate::state) fn take_ended_exec_sessions(&mut self) -> Vec<(String, String)> {
        ::std::mem::take(&mut self.exec.ended_elsewhere)
    }

    /// The sessions this store is still carrying, each with the Region its Session Manager
    /// record is held at — what [`settle_exec_session_records`] compares against that record,
    /// so a session ENDED there is one this store carries no further.
    pub(in crate::state) fn carried_exec_sessions(&self) -> Vec<(String, String)> {
        self.exec
            .sessions
            .values()
            .filter(|session| !session.ended)
            .map(|session| (session.region.clone(), session.session_id.clone()))
            .collect()
    }

    /// Whether the session `session_id` names is still being carried: the channel reads this
    /// between frames, so a session the clock has found idle, or whose task is gone, ends the
    /// stream it was carrying.
    pub fn exec_session_is_live(&self, session_id: &str) -> bool {
        self.exec
            .sessions
            .get(session_id)
            .is_some_and(|session| !session.ended)
    }

    /// Take away the sessions that are over: the ones the clock has found idle past
    /// [`EXEC_SESSION_IDLE_TIMEOUT`], the ones whose channel ended, and the ones whose task
    /// this store no longer holds running.
    ///
    /// The idle lapse is a transition the CLOCK owes a session — the timeout was already up at
    /// the same instant whether or not anybody called — which is why this runs from
    /// `advance_clock` as well as from the two calls above.
    pub(in crate::state) fn expire_exec_sessions(&mut self, now: SystemTime) {
        let tasks = &self.tasks;
        let mut lapsed: Vec<(String, String)> = Vec::new();
        self.exec.sessions.retain(|_, session| {
            let idle = now
                .duration_since(session.used_at)
                .unwrap_or_default()
                >= EXEC_SESSION_IDLE_TIMEOUT;
            // A session is opened ON a container of a running task: a task that is gone, or
            // one that has stopped, carries no container for a stream to reach.
            let running = tasks
                .get(&session.task_arn)
                .is_some_and(|task| task.last_status != STATUS_STOPPED);
            let kept = !session.ended && !idle && running;
            if !kept && !session.ended {
                // A session the clock took away is one whose Systems Manager record is owed
                // the same end ([`settle_exec_session_records`]); one that had already ended
                // is owed it from where it ended.
                lapsed.push((session.region.clone(), session.session_id.clone()));
            }
            kept
        });
        self.exec.ended_elsewhere.extend(lapsed);
        self.exec
            .agent_connections
            .retain(|task_arn, _| tasks.contains_key(task_arn));
    }

    /// What the session `session_id` names needs to be CARRIED, read off this store under its
    /// lock: which container the stream runs in, the program it runs, the environment and
    /// filesystem it runs over, and the role its AWS calls are made as.
    ///
    /// `None` for a session this store does not hold, one whose task or container is gone, and
    /// one whose container holds no run — a container nothing is running in is a container no
    /// session can be opened inside.
    pub(in crate::state) fn exec_session_parts(&self, session_id: &str) -> Option<ExecSessionParts> {
        let session = self.exec.sessions.get(session_id)?;
        let task = self.tasks.get(&session.task_arn)?;
        let container = task
            .containers
            .iter()
            .find(|container| container.container_arn == session.container_arn)?;
        let definition = self.revision_of_arn(&task.task_definition_arn)?;
        let name = container.name.clone()?;
        let stored = definition
            .container_definitions
            .iter()
            .find(|candidate| candidate.name.as_deref() == Some(name.as_str()))?;
        let region = super::super::request_arn::RequestArn::parse(&task.task_arn)
            .map(|arn| arn.region.to_string())
            .unwrap_or_default();
        let mut environment: BTreeMap<String, String> = BTreeMap::new();
        for pair in stored.environment.iter().flatten() {
            if let Some(key) = pair.name.clone() {
                environment.insert(key, pair.value.clone().unwrap_or_default());
            }
        }
        let overrides = task
            .overrides
            .as_ref()
            .and_then(|overrides| overrides.container_overrides.as_ref())
            .and_then(|overrides| {
                overrides
                    .iter()
                    .find(|candidate| candidate.name.as_deref() == Some(name.as_str()))
            });
        for pair in overrides
            .and_then(|over| over.environment.as_deref())
            .unwrap_or_default()
        {
            if let Some(key) = pair.name.clone() {
                environment.insert(key, pair.value.clone().unwrap_or_default());
            }
        }
        Some(ExecSessionParts {
            command: session.command.clone(),
            environment,
            mounts: super::super::container_volumes::mount_requests(definition, stored, task),
            working_directory: stored.working_directory.clone(),
            layer: self.container_layer(&session.container_arn)?,
            process: ContainerProcess::of(Some(stored), task),
            credential: task
                .task_role_credentials
                .clone()
                .unwrap_or(TaskRoleCredential::Absent),
            address: task
                .attachments
                .iter()
                .find_map(AttachmentRecord::private_ipv4_address)
                .map(str::to_string),
            region,
        })
    }
}

/// What one session needs to be carried, taken out of the store under its lock — the exec
/// counterpart of the launch a container's own run is started from
/// ([`super::super::container_runtime`]).
#[derive(Debug)]
pub(in crate::state) struct ExecSessionParts {
    /// The program the session runs.
    command: String,
    /// The environment that container's definition and the launch's override give it.
    environment: BTreeMap<String, String>,
    /// The volumes mounted into it, resolved where the session is started.
    mounts: Vec<MountRequest>,
    /// `ContainerDefinition$workingDirectory`.
    working_directory: Option<String>,
    /// The container's OWN writable layer, so what the session writes is what the container
    /// holds and what another session of it reads.
    layer: ContainerLayer,
    /// What the process the session is runs as.
    process: ContainerProcess,
    /// What its AWS calls present: the session of the task's role.
    credential: TaskRoleCredential,
    /// The address of the interface the task holds, which the client its session provides
    /// reaches the task's own servers at.
    address: Option<String>,
    /// The Region the task was placed in, which its calls are addressed in.
    region: String,
}

/// One session ready to be carried: the shell it runs in, where its output is collected, and
/// what the channel reports about the container it is inside.
pub struct ExecSessionShell {
    /// The session the stream runs in. Its variables, its filesystem and its traps are kept
    /// between the lines a client types, as a shell reading its input keeps them.
    pub shell: ContainerShell,
    /// Where the lines that session writes are collected, which the channel reads between
    /// lines ([`CollectedOutput`]).
    pub output: Arc<CollectedOutput>,
    /// The program `ExecuteCommand` named, which is the shell the session runs.
    pub command: String,
}

/// Written by hand rather than derived: a [`ContainerShell`] holds the session's own state
/// and what is diagnosable here is the program it runs.
impl ::std::fmt::Debug for ExecSessionShell {
    fn fmt(&self, formatter: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        formatter
            .debug_struct("ExecSessionShell")
            .field("command", &self.command)
            .finish()
    }
}

/// Build the session the data channel carries for `session_id`, or `None` when this store
/// holds nothing it could be carried in (see
/// [`ComAmazonawsEcsAmazonec2containerservicev20141113State::exec_session_parts`]).
///
/// Called with NO guard of this store held, from the connection that carries the channel: the
/// volumes a container's `mountPoints` mount are resolved here, which reads the store of the
/// service that holds each of them, exactly as a container's own run resolves them
/// ([`super::super::container_runtime::settle_container_runtime`]).
pub fn exec_session_shell<Eng: Send + Sync + 'static>(
    engine: &Eng,
    callback: &::smithy_server_runtime::EmbeddedEmulator,
    session_id: &str,
    lifetime: Duration,
) -> Option<ExecSessionShell> {
    let state = service_state(engine)?;
    let parts = state.lock().exec_session_parts(session_id)?;
    let filesystem = ContainerFilesystem::on_layer(
        parts.layer.clone(),
        parts.working_directory.as_deref(),
        super::super::container_volumes::mounted_volumes(engine, &state, &parts.mounts),
    );
    // Where the lines the session writes are collected: an exec session's output goes to the
    // CLIENT that typed the command rather than to the container's log driver, which delivers
    // what the container's own program writes.
    let output = Arc::new(CollectedOutput::default());
    let mut shell = ContainerShell::new(
        parts.environment,
        Arc::new(ContainerOutput::collecting(Arc::clone(&output))),
        // The session answers no signal of the container's: what ends it is the client
        // closing the channel, its shell exiting, or the clock finding it idle.
        Arc::new(ContainerSignals::new()),
        Instant::now() + lifetime,
        filesystem,
        None,
    )
    .with_process(parts.process.clone())
    // The AWS CLI the session reaches the account's services through, signing as the TASK
    // role — a session runs inside the container, so its calls are the container's
    // ([`super::super::container_aws_cli`]).
    .with_aws_cli(ContainerAwsCli::new(
        callback.clone(),
        &parts.region,
        parts.credential.clone(),
    ));
    // And the client it asks the servers of its own task for a document with, for a task
    // reached at an address of its own.
    if let Some(http) = parts
        .address
        .clone()
        .zip(::service_common::served_http_addresses::ServedAddresses::of(engine))
        .map(|(address, addresses)| ContainerHttpClient::new(address, addresses))
    {
        shell = shell.with_http_client(http);
    }
    Some(ExecSessionShell {
        shell,
        output,
        command: parts.command.clone(),
    })
}

/// The id of the `sequence`-th session this store opens, opened at `now`: the prefix Amazon
/// ECS names an execute-command session with and a token of hexadecimal digits under it.
///
/// Amazon ECS generates the id, so what a client can rely on is its SHAPE; the counter under
/// it is what makes every session of this store distinct, and the instant is what keeps two
/// stores of one process apart.
fn exec_session_id(sequence: u64, now: SystemTime) -> String {
    let millis = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let millis = u64::try_from(millis).unwrap_or(u64::MAX);
    format!("{EXEC_SESSION_ID_PREFIX}{millis:016x}{sequence:016x}")
}

/// The DIGEST one session's token is recognised by: the SHA-256 of the secret, spelled in
/// hexadecimal.
///
/// What a session needs of its token is to recognise the one presented on its channel, and a
/// digest is enough for that — so the secret itself is not what this store keeps, and a
/// comparison over two digests costs the same whatever the two tokens share.
fn token_digest(token: &str) -> String {
    use ::sha2::Digest as _;
    let mut hasher = ::sha2::Sha256::new();
    hasher.update(token.as_bytes());
    ::hex::encode(hasher.finalize())
}

/// The `tokenValue` one session reports: "an encrypted token value containing session and
/// caller information", which a client authenticates the data channel with.
///
/// The value is a secret handed to a caller, but it is NOT a credential of this emulator and
/// must never be accepted as one, so it is minted by the one component that generates secrets
/// — the emulator's credential store — and then removed from that store again. Nothing here
/// constructs a secret.
///
/// `None` when the store minted nothing, which leaves the operation to refuse rather than
/// report a token it made up.
pub fn mint_exec_session_token(store: &::emulator_server::CredentialStore) -> Option<String> {
    let minted = store.create(&::emulator_server::aws_credentials::AWS_SESSION);
    let token = minted
        .secret(::emulator_server::aws_credentials::SESSION_TOKEN)
        .unwrap_or_default()
        .to_string();
    // The session token is not a credential of this emulator; see above.
    let _ = store.delete(&minted.id);
    (!token.is_empty()).then_some(token)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two sessions of one store never share an id, and every id carries the prefix Amazon
    /// ECS names such a session with. The rows span the counter and the instant it was minted
    /// at, which are the two things an id is made of.
    #[test]
    fn every_session_id_is_distinct_and_carries_the_prefix() {
        let rows: [(u64, u64); 4] = [(1, 0), (2, 0), (1, 1_700_000_000_000), (u64::MAX, 1)];
        let mut seen: Vec<String> = Vec::new();
        for (sequence, millis) in rows {
            let case = format!("session {sequence} at {millis}");
            let at = SystemTime::UNIX_EPOCH + Duration::from_millis(millis);
            let minted = exec_session_id(sequence, at);
            assert!(
                minted.starts_with(EXEC_SESSION_ID_PREFIX),
                "case {case:?}: expected the session prefix, got {minted:?}"
            );
            assert!(
                !seen.contains(&minted),
                "case {case:?}: expected a distinct id, got {minted:?} again"
            );
            seen.push(minted);
        }
    }
}
