// SPDX-License-Identifier: CC0-1.0

//! The PROCESSES this emulator keeps standing for deployed JAVASCRIPT bundles,
//! and how one request reaches one.
//!
//! An instance runs the application as ONE long-lived process
//! ([`super::application_process`] states the rule and holds the identity of a
//! process), and for a bundle a Node.js platform starts that process is the
//! embedded JavaScript engine with the bundle's entry point evaluated in it
//! ([`super::application_runtime::JavaScriptProcess`]). Every object of that
//! engine belongs to the thread it was built on, so a process IS a thread here:
//! it is reached by handing it a request, and it answers what the server the
//! entry point started answered.
//!
//! What a standing process holds is bounded by this module: at most
//! [`MAX_STANDING_PROCESSES`] of them stand at once, the one used longest ago
//! retired to make room — which ends its thread and gives the engine's heap back —
//! and a process whose thread has gone is replaced by a new one rather than
//! reported as a failure. The engine's own per-run bounds (its wall clock, its
//! step budget, its heap and its stack) are the runtime module's, and the slots a
//! run is admitted through are the ones every deployed application's run is
//! admitted through ([`super::deployed_application`]).
//!
//! Retiring a process drops the channel its thread takes requests on, which that
//! thread observes when the run it is performing returns — so a run the engine has
//! not stopped yet keeps its thread alive for a moment after the process was
//! forgotten. What that cannot do is grow: the THREADS are counted
//! ([`live_threads`]), a thread counts until it really exits, and no process is
//! started once [`MAX_LIVE_PROCESS_THREADS`] of them are live — so no sequence of
//! requests can hold more than that many engine heaps and stacks of this host.

use super::*;

use ::std::collections::HashMap;
use ::std::sync::mpsc::{channel, Receiver, Sender};
use ::std::sync::{Mutex, OnceLock, PoisonError};

/// How many JavaScript processes this emulator keeps standing at once.
///
/// An EMULATOR bound: each one holds its engine's heap on a thread of its own, so
/// this is what bounds what those processes hold however many environments a
/// client launches. Well above the environments one client drives at a time, and
/// the process used longest ago is the one retired to make room.
const MAX_STANDING_PROCESSES: usize = 8;

/// How many process THREADS may be live in this emulator at once, standing or
/// not.
///
/// The ceiling on what the engines hold of this host, and the one no sequence of
/// requests can get past: a thread counts from the moment it is started until it
/// really exits, which is after the run it is performing returns — so a process
/// this emulator gave up on and forgot still counts while its own run is
/// finishing. Above [`MAX_STANDING_PROCESSES`], so the processes standing for the
/// environments a client is driving are not refused because of a run that is on
/// its way out; once this many are live a request is answered as one this
/// emulator did not run, rather than starting one more.
const MAX_LIVE_PROCESS_THREADS: usize = MAX_STANDING_PROCESSES * 2;

/// The stack the thread one process runs on is given.
///
/// Above the JavaScript stack the engine is given
/// ([`super::application_runtime`]), so deep recursion in a bundle ends that run
/// with the engine's own stack-overflow exception rather than touching the
/// thread's guard page.
const PROCESS_STACK_BYTES: usize = 8 * 1024 * 1024;

/// How long the task that handed a request to a process waits for the answer.
///
/// The span the engine allows one run, plus the grace its stop needs to land: the
/// stop is raised at the next ask the engine makes of its interrupt handler, so a
/// run the engine has stopped answers a moment after the deadline rather than at
/// it. A process that has still not answered by then is given up on — and
/// retired, because nothing can be said about where its thread is.
const PROCESS_HOST_WAIT: Duration = super::application_runtime::JAVASCRIPT_RUN_WALL_CLOCK
    .saturating_add(Duration::from_secs(5));

/// Serve one run in the JavaScript process of `process`: start the process from
/// `entry` with `environment` as its process environment if it is not standing,
/// and hand it `request` when there is one.
///
/// The FIRST run in a process evaluates the entry point — which is what starts the
/// server the application serves with — and every later run reaches that same
/// server, so what the application holds in memory survives from one request to
/// the next.
///
/// `None` for a run this emulator did not perform at all: one it had no run slot
/// or no thread for, and one that had still not answered a grace period past the
/// span the engine allows it.
pub(super) async fn served_by_the_javascript_process(
    process: &ProcessIdentity,
    entry: &str,
    environment: &[(String, String)],
    request: Option<&GuestRequest>,
) -> Option<GuestRun> {
    let key = process.key();
    // ONE request in flight in a process, exactly as for the interpreter the other
    // platform's process runs on: the instance runs one process, which answers
    // one request at a time.
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
    let standing = standing_process(&key, entry, environment)?;
    let (answered, answer) = ::tokio::sync::oneshot::channel();
    let job = Job {
        request: request.cloned(),
        answer: answered,
    };
    if standing.jobs.send(job).is_err() {
        // The process's thread has gone, which it can only have done by panicking:
        // it is forgotten here and the next request starts a new one, so a lost
        // process is invisible to the caller.
        forget_process(&key);
        return None;
    }
    // The permit is held until the answer is in, so what the engines hold at once
    // is bounded by the slots however many requests arrive together.
    let waited = ::tokio::time::timeout(PROCESS_HOST_WAIT, answer).await;
    drop(permit);
    match waited {
        Ok(Ok(run)) => {
            // What the process wrote OUTSIDE the serving of one request — starting
            // the application — is what the instance's standard-output log file
            // carries, so it is kept for as long as the process stands.
            record_process_log(&key, application_log_lines(&run));
            Some(run)
        }
        _ => {
            // Nothing can be said about where that thread is, so the process is
            // not kept: the next request starts one.
            forget_process(&key);
            ::tracing::warn!(
                "the deployed application's own run did not answer inside the span this \
                 emulator allows it",
            );
            None
        }
    }
}

/// One request handed to a process, with where its answer goes.
struct Job {
    /// The request to serve, and `None` for a run that only starts the process.
    request: Option<GuestRequest>,
    /// Where what the run came to is sent back.
    answer: ::tokio::sync::oneshot::Sender<GuestRun>,
}

/// A standing process, reached by handing it a job.
#[derive(Clone)]
struct Standing {
    /// Where its thread takes requests. Dropping every clone of this ends that
    /// thread, which drops the engine and gives its heap back.
    jobs: Sender<Job>,
}

/// The process of `key`, standing it up from `entry` when none is standing.
///
/// `None` when no thread could be started for it at all.
fn standing_process(
    key: &str,
    entry: &str,
    environment: &[(String, String)],
) -> Option<Standing> {
    let mut held = processes()
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    if let Some(standing) = held.get_mut(key) {
        standing.used = ::std::time::Instant::now();
        return Some(standing.standing.clone());
    }
    while held.len() >= MAX_STANDING_PROCESSES {
        // The process used longest ago makes room: dropping it ends its thread.
        let Some(retired) = held
            .iter()
            .min_by_key(|(_, held)| held.used)
            .map(|(key, _)| key.clone())
        else {
            break;
        };
        held.remove(&retired);
    }
    let standing = start_process(entry, environment)?;
    held.insert(
        key.to_string(),
        Held {
            standing: standing.clone(),
            used: ::std::time::Instant::now(),
        },
    );
    Some(standing)
}

/// Forget the process of `key`: whatever is left of it is dropped, which ends its
/// thread.
fn forget_process(key: &str) {
    processes()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .remove(key);
}

/// Start a process: a thread that builds the engine, evaluates `entry` in it and
/// then serves request after request in it.
///
/// `None` when no thread may be started: [`MAX_LIVE_PROCESS_THREADS`] of them are
/// live already, or the host would not start one.
fn start_process(entry: &str, environment: &[(String, String)]) -> Option<Standing> {
    // Counted BEFORE the thread exists and given back by the thread itself as it
    // exits ([`LiveThread`]), so a thread that is still finishing a run it was
    // given up on goes on counting against the ceiling.
    let live = LiveThread::claimed()?;
    let (jobs, incoming) = channel();
    let entry = entry.to_string();
    let environment = environment.to_vec();
    let spawned = ::std::thread::Builder::new()
        .name("eb-javascript-process".to_string())
        .stack_size(PROCESS_STACK_BYTES)
        .spawn(move || {
            // Held for the whole life of the thread: dropping it is what gives the
            // claim back.
            let _live = live;
            serve_jobs(&entry, &environment, incoming);
        });
    if let Err(error) = spawned {
        ::tracing::warn!(
            detail = %error,
            "the thread a deployed application's process runs on could not be started",
        );
        return None;
    }
    Some(Standing { jobs })
}

/// One live process thread, counted against [`MAX_LIVE_PROCESS_THREADS`] for as
/// long as it exists.
struct LiveThread;

impl LiveThread {
    /// Claim one of the live-thread slots, or `None` when they are all taken.
    fn claimed() -> Option<Self> {
        let mut live = live_threads()
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if *live >= MAX_LIVE_PROCESS_THREADS {
            ::tracing::warn!(
                live = *live,
                "the deployed application was not run: every process thread this emulator \
                 holds is live",
            );
            return None;
        }
        *live += 1;
        Some(Self)
    }
}

impl Drop for LiveThread {
    fn drop(&mut self) {
        let mut live = live_threads()
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        *live = live.saturating_sub(1);
    }
}

/// How many process threads are live in this emulator now.
fn live_threads() -> &'static Mutex<usize> {
    static LIVE: OnceLock<Mutex<usize>> = OnceLock::new();
    LIVE.get_or_init(|| Mutex::new(0))
}

/// One process's whole life, on its own thread: the engine is built and the entry
/// point evaluated ONCE, and every job that arrives is served in it.
fn serve_jobs(entry: &str, environment: &[(String, String)], jobs: Receiver<Job>) {
    let Some((process, start)) = JavaScriptProcess::start(entry, environment) else {
        // No engine could be stood up, so nothing of this process ever answers: the
        // jobs already queued are dropped, and their callers report a run this
        // emulator did not perform.
        return;
    };
    let mut start = Some(start);
    while let Ok(job) = jobs.recv() {
        let run = match job.request.as_ref() {
            Some(request) => process.serve(request),
            // A run with nothing to serve asked for the process to be STARTED,
            // which is what building it was: the first such run is answered with
            // what starting it produced, and a later one with nothing, because
            // nothing started again.
            None => start.take().unwrap_or_default(),
        };
        // A caller that gave up on its run is not waiting for the answer; the
        // process goes on standing either way, because what it holds is the
        // application's and not that request's.
        let _ = job.answer.send(run);
    }
}

/// A standing process and when it was last used, which is what decides which one
/// is retired to make room.
struct Held {
    standing: Standing,
    used: ::std::time::Instant,
}

/// The JavaScript processes standing in this emulator.
fn processes() -> &'static Mutex<HashMap<String, Held>> {
    static PROCESSES: OnceLock<Mutex<HashMap<String, Held>>> = OnceLock::new();
    PROCESSES.get_or_init(|| Mutex::new(HashMap::new()))
}
