// SPDX-License-Identifier: CC0-1.0

//! The PROCESS an environment's instance runs the deployed application as, and
//! how one request reaches it.
//!
//! An Elastic Beanstalk instance runs the application as ONE long-lived process:
//! the platform starts the bundle's web command once, that process stays up, and
//! every request the environment's address receives is answered by it. So what
//! the application holds IN MEMORY is there for the next request — a counter, a
//! warm cache, a session, a value it minted when it started — and what changes it
//! is the process being REPLACED: `RestartAppServer` "causes the environment to
//! restart the application container server running on each Amazon EC2 instance",
//! a deployment that lands puts a new process on the instances, and a rebuild or
//! a managed replacement gives the environment new instances to start one on. An
//! update that is ABORTED lands nothing, so the instances keep the process they
//! were running.
//!
//! This module is WHICH process a request is served by ([`ProcessIdentity`]) and
//! how one request reaches it ([`served_by_the_process`]). The process itself is
//! an execution environment of the embedded Python interpreter kept standing for
//! it: the interpreter this workspace embeds is reached through the Lambda service
//! crate, whose warm pool already holds a standing interpreter per environment key
//! and serves request after request in it
//! (`state::execution::warm_pool`) — reused rather than stood up a second time, so
//! an environment's process is held, bounded and torn down by the one mechanism
//! this workspace has for a standing interpreter. The platform's own serving layer
//! runs the web command the FIRST time it is asked in a process and keeps the
//! server that command started for every later request
//! ([`super::python_program`], [`super::python_application`]), which is what makes
//! consecutive requests reach the same process.
//!
//! ONE request is in flight in a process at a time ([`in_flight_lock`]): an
//! instance runs one process, so two requests arriving together are served by that
//! process one after the other rather than by two interpreters of the same
//! instance.
//!
//! Every register here is this PROCESS's, while an environment belongs to one
//! account in one STORE, so every key into them carries both
//! ([`ProcessIdentity`]): a request of one store reaches no process, and reads no
//! process log, of another engine's environments.

use super::*;

use ::std::collections::HashMap;
use ::std::sync::{Arc, Mutex, OnceLock, PoisonError};

use ::service_com_amazonaws_lambda_awsgirapiservice::state::execution::package::PackageModule;
use ::service_com_amazonaws_lambda_awsgirapiservice::state::execution::warm_pool;
use ::service_com_amazonaws_lambda_awsgirapiservice::state::execution_slot::INITIALIZATION_TYPE_ON_DEMAND;
use ::service_com_amazonaws_lambda_awsgirapiservice::state::DEFAULT_ARCHITECTURE;

/// How many processes this module tracks an in-flight request for at once.
///
/// An EMULATOR bound on what this register holds, not on how many processes may
/// stand (which is the pool's). Past it the entries nothing is holding are
/// dropped, so a client that launches and terminates environments in a loop does
/// not grow the register.
const MAX_TRACKED_PROCESSES: usize = 64;

/// How many lines of ONE process's own log file this emulator keeps.
///
/// The instance's `/var/log/web.stdout.log` is what the process has written since
/// it started, which is what the environment information a `RequestEnvironmentInfo`
/// compiles carries ([`super::compiled_environment_information`]). It is held in
/// this process's memory, so it is a TAIL: past this many lines the oldest are
/// dropped, the way a rotated log file keeps the last of what was written.
const MAX_PROCESS_LOG_LINES: usize = 1_000;

/// WHICH process a request is served by: the instance's process, as it stands now.
///
/// Two requests carrying the same identity are served by the SAME process and see
/// what each other left in it; a request carrying a different one is served by a
/// new process, which is what a restart, a landed deployment and a replaced
/// instance each produce.
///
/// WHAT the process runs follows from WHEN it was started, which is why the
/// identity is the environment's instance and the generation rather than a digest
/// of the deployment: a version or a configuration an update has been ASKED to
/// deploy reaches the instances only when the clock completes that update — which
/// is what moves the generation — and one whose update was cancelled reaches them
/// never, so the process a request is served by is the one the instance is running
/// and not whatever the environment record has most recently been told to run.
///
/// WHOSE process it is is part of it too, and has to be: the registers below and
/// the pool that keeps a process standing are this PROCESS's, while an environment
/// belongs to one ACCOUNT in one STORE. Both are in the identity and in every key
/// built from it, so no request can reach a standing interpreter — which holds the
/// application's own in-memory state — or the log of a process belonging to
/// another account or to another engine of this process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessIdentity {
    /// The environment whose instance runs it.
    pub environment_id: String,
    /// The Region that environment is in.
    pub region: String,
    /// The application it belongs to.
    pub application_name: String,
    /// WHOSE it is: the account the environment belongs to, in the store that
    /// holds that environment ([`Self::of`]).
    pub owner: String,
    /// WHICH process of that instance this is
    /// ([`Environment::app_process_generation`]): the number the environment
    /// moves on every time its instances start the application again.
    pub generation: u64,
}

impl ProcessIdentity {
    /// The identity of the process `environment`'s instance runs now, in the store
    /// whose own scope is `scope`.
    ///
    /// The OWNER is the account this emulator serves
    /// (`engine::emulator_account_id`, the account every ARN this service mints
    /// names) inside that scope: an environment of one store is never an
    /// environment of another, however the two happen to name their environments.
    #[must_use]
    pub fn of(environment: &Environment, scope: &str) -> Self {
        Self {
            environment_id: environment.environment_id.clone(),
            region: environment.region.clone(),
            application_name: environment.application_name.clone(),
            owner: format!("{}/{scope}", ::engine::emulator_account_id()),
            generation: environment.app_process_generation,
        }
    }

    /// The one key this identity is held under, in the registers below and in the
    /// pool that keeps the process standing.
    pub(super) fn key(&self) -> String {
        format!(
            "{}|{}|{}|{}|{}",
            self.owner,
            self.environment_id,
            self.region,
            self.application_name,
            self.generation
        )
    }
}

/// Serve one run in the process `process`: hand the run the platform assembled
/// ([`PlatformRun`]) to the standing interpreter that process IS, with
/// `environment` as its process environment.
///
/// The FIRST run in a process is what starts the application (the platform's
/// serving layer applies the deployment's configuration, runs the web command and
/// keeps the server it started), and every later run reaches that same server —
/// which is what makes the application's own in-memory state survive from one
/// request to the next.
///
/// `None` for a run this emulator did not perform at all: one it had no run slot
/// for inside [`RUN_PERMIT_WAIT`], and one that had still not answered a grace
/// period past the spans the engine allows it
/// ([`PYTHON_APPLICATION_HOST_WAIT`]).
pub async fn served_by_the_process(
    process: &ProcessIdentity,
    run: PlatformRun,
    environment: Vec<(String, String)>,
) -> Option<GuestRun> {
    let key = process.key();
    // ONE request in flight in a process: the instance runs one process, so a
    // second request waits for the one being served rather than standing up a
    // second interpreter for the same instance.
    let serialized = in_flight_lock(&key);
    let _in_flight = serialized.lock().await;
    let permit = ::tokio::time::timeout(RUN_PERMIT_WAIT, application_runs().acquire()).await;
    let permit = match permit {
        Ok(Ok(permit)) => permit,
        _ => {
            ::tracing::warn!(
                "the deployed application was not run: every run slot this emulator holds was \
                 taken",
            );
            return None;
        }
    };
    let process = process.clone();
    // The run happens OFF the async executor: the interpreter is synchronous, and
    // the thread it stands on is the pool's own.
    let performed = ::tokio::task::spawn_blocking(move || {
        // Held for as long as the run lasts, whoever is still waiting for it.
        let _permit = permit;
        run_in_process(&process, &run.supplied, &run.handler, &run.event, &environment)
    });
    match ::tokio::time::timeout(PYTHON_APPLICATION_HOST_WAIT, performed).await {
        Ok(Ok(run)) => {
            // What the process wrote OUTSIDE the serving of one request — starting
            // the application, and anything it writes from a thread of its own —
            // is what the instance's standard-output log file carries, so it is
            // kept for as long as the process stands.
            record_process_log(&key, application_log_lines(&run));
            Some(run)
        }
        // The run has still not answered a grace period past the spans the engine
        // itself allows, which it has by then been stopped on, or the thread it
        // ran on did not finish: it is given up on and reported as an application
        // that produced nothing.
        _ => {
            ::tracing::warn!(
                "the deployed application's own run did not answer inside the spans this \
                 emulator allows it",
            );
            None
        }
    }
}

/// The instance's own standard-output log file, as the process `process` has
/// written it: every line that process wrote outside the serving of one request,
/// in order, up to the tail this emulator keeps ([`MAX_PROCESS_LOG_LINES`]).
///
/// Empty for a process that has written nothing and for one that was never
/// started: a file that holds no line is what an instance running no application
/// of this emulator's engines has.
#[must_use]
pub fn process_log(process: &ProcessIdentity) -> Vec<String> {
    process_logs()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&process.key())
        .cloned()
        .unwrap_or_default()
}

/// Append `lines` to the log of the process `key`.
pub(super) fn record_process_log(key: &str, lines: Vec<&str>) {
    if lines.is_empty() {
        return;
    }
    let mut held = process_logs()
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    if held.len() >= MAX_TRACKED_PROCESSES && !held.contains_key(key) {
        // The logs of the processes this emulator is serving requests for, not of
        // every process it has ever run: one of the processes already held gives
        // its file up to make room for this one's.
        if let Some(dropped) = held.keys().next().cloned() {
            held.remove(&dropped);
        }
    }
    let log = held.entry(key.to_string()).or_default();
    log.extend(lines.into_iter().map(str::to_string));
    // A TAIL: the file keeps the last of what was written.
    let excess = log.len().saturating_sub(MAX_PROCESS_LOG_LINES);
    if excess > 0 {
        log.drain(..excess);
    }
}

/// The log files the processes of this emulator have written.
fn process_logs() -> &'static Mutex<HashMap<String, Vec<String>>> {
    static LOGS: OnceLock<Mutex<HashMap<String, Vec<String>>>> = OnceLock::new();
    LOGS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// [`served_by_the_process`] once the run slot is held and the executor is left:
/// the standing interpreter the process IS serves the run.
///
/// SYNCHRONOUS, and called with no state guard held.
fn run_in_process(
    process: &ProcessIdentity,
    supplied: &[PackageModule],
    handler: &str,
    event: &str,
    environment: &[(String, String)],
) -> GuestRun {
    let budget = application_budget();
    let key = warm_pool::EnvironmentKey {
        // WHICH instance's process: the environment's own id, and which process of
        // it.
        function_name: process.environment_id.clone(),
        executed_version: process.generation.to_string(),
        region: process.region.clone(),
        // WHAT it runs: the application whose version the instance was deployed
        // from, so a process of one application never serves another's request.
        code_revision: process.application_name.clone(),
        // The engine the platform starts the application on, and the serving layer
        // it is started through.
        runtime: PROCESS_RUNTIME.to_string(),
        handler: handler.to_string(),
        // What the process may hold, which is what the pool keeps a standing
        // interpreter against: the memory one run of a deployed application is
        // given ([`application_budget`]), in whole megabytes.
        memory_size: i32::try_from(budget.memory_bytes / BYTES_PER_MEGABYTE).unwrap_or(i32::MAX),
        architecture: DEFAULT_ARCHITECTURE.to_string(),
        initialization_type: INITIALIZATION_TYPE_ON_DEMAND,
        // WHOSE it is: a standing interpreter holds the application's own
        // in-memory state, so the pool is told which account's environment — in
        // which store of this process — it belongs to, and hands it to no request
        // of another ([`ProcessIdentity`]).
        tenant_id: Some(process.owner.clone()),
    };
    let served = warm_pool::Served {
        modules: Arc::new(supplied.to_vec()),
        handler: handler.to_string(),
        environment: environment.iter().cloned().collect(),
        architecture: DEFAULT_ARCHITECTURE.to_string(),
        event: Some(event.to_string()),
        // An environment's application is no invocation of a function: it is
        // handed no request id, no function ARN, no client context and no SDK seam
        // of this emulator's. WHOSE process serves it is the identity's
        // ([`ProcessIdentity`]), so the invocation names the owner the key
        // selects the standing interpreter by.
        request_id: String::new(),
        invoked_function_arn: String::new(),
        client_context: None,
        tenant_id: Some(process.owner.clone()),
        budget,
        aws: None,
        durable: None,
    };
    guest_run(warm_pool::serve(&key, served))
}

/// The name the process's own engine is held under in the pool — the interpreter
/// the platform starts a Python deployment's web command on.
const PROCESS_RUNTIME: &str = "python";

/// How many bytes a megabyte is, for the memory the pool holds a standing
/// interpreter against.
const BYTES_PER_MEGABYTE: u64 = 1024 * 1024;

/// The lock that keeps ONE request in flight in the process `key`.
///
/// Held per process rather than per environment, so a request for the process an
/// instance runs NOW never waits for a run in a process a restart has already
/// replaced.
pub(super) fn in_flight_lock(key: &str) -> Arc<::tokio::sync::Mutex<()>> {
    static IN_FLIGHT: OnceLock<Mutex<HashMap<String, Arc<::tokio::sync::Mutex<()>>>>> =
        OnceLock::new();
    let mut held = IN_FLIGHT
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    if held.len() >= MAX_TRACKED_PROCESSES {
        // Every entry nothing is waiting on or serving with is dropped: the
        // register holds the processes requests are arriving for, not every
        // process this emulator has ever run.
        held.retain(|tracked, lock| tracked == key || Arc::strong_count(lock) > 1);
    }
    Arc::clone(held.entry(key.to_string()).or_default())
}
