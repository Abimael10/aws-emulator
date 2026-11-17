// SPDX-License-Identifier: CC0-1.0

//! WHERE this service performs a caller-supplied shell program: the thread one gets, the
//! claim that keeps one program performed once, and the permits that bound how many this
//! process performs at a time.
//!
//! Two concerns of this crate hand a caller's own program to the one bounded shell session
//! this workspace has ([`::service_common::posix_shell_session`]): the command a
//! `command-runner.jar` step submits ([`super::command_step_run`]) and the script a cluster's
//! bootstrap action names ([`super::bootstrap_run`]). WHAT such a program may reach is those
//! modules' and that session's — a parse, a fixed allow-list of session-local utilities and
//! the AWS CLI's Amazon S3 transfers, with NO host command, process or shell started
//! anywhere. This module is only about WHERE one runs, which is the same for both, so it is
//! stated once:
//!
//! * on a THREAD of its own, because the session is synchronous and the transfers its
//!   program's own program makes are calls into this emulator — the thread is not one of the
//!   async runtime's workers, so those calls are driven by blocking it rather than by making
//!   the session asynchronous;
//! * under a CLAIM ([`claimed_program`]), so one owed program is performed once at a time
//!   however many callers report the resource that owes it: its transfers are real effects on
//!   the object store, and neither a second caller nor a call that has given up waiting makes
//!   them twice;
//! * holding one of [`MAX_CONCURRENT_PROGRAM_RUNS`] permits, with a bounded wait for one, so
//!   the number of such threads this process carries follows from that bound rather than from
//!   how many callers call at once.
//!
//! The claim and the permit are both MOVED into the thread, so both are given up when that
//! thread really ends — including a program the caller has already stopped waiting for, which
//! is still making its calls until then.

use super::*;

/// How many caller-supplied programs this process performs AT ONCE.
///
/// CHOSEN, the sibling of the Spark path's own ceiling
/// ([`super::spark_step_run::MAX_CONCURRENT_APPLICATION_RUNS`]): one program is a session, a
/// thread and the calls the two make, so the number of them this process carries follows from
/// this bound rather than from how many callers report a cluster at once.
pub const MAX_CONCURRENT_PROGRAM_RUNS: usize = 4;

/// How long a program waits for one of [`MAX_CONCURRENT_PROGRAM_RUNS`] to come free before
/// nothing is performed for it. CHOSEN: a bounded wait, so the operation that performs it
/// answers either way; a program that found no free slot stays owed an answer and the next
/// call performs it.
pub const PROGRAM_PERMIT_WAIT: Duration = Duration::from_secs(2);

/// How long after its budget is spent a program's thread is still waited for. CHOSEN: the
/// room a call that has just been given up on needs to come back, so a program that answered
/// at its bound is still read rather than being reported as one nothing answered for.
pub const PROGRAM_ABANDON_GRACE: Duration = Duration::from_secs(2);

/// The permits [`MAX_CONCURRENT_PROGRAM_RUNS`] hands out, for the whole process.
///
/// Held as an owned permit MOVED INTO the thread that performs the program, so it is given
/// back when that thread really ends — including a program the caller has already given up
/// waiting for, which would otherwise take this bound past its count while its thread was
/// still going.
static PROGRAM_RUN_PERMITS: ::std::sync::LazyLock<::std::sync::Arc<::tokio::sync::Semaphore>> =
    ::std::sync::LazyLock::new(|| {
        ::std::sync::Arc::new(::tokio::sync::Semaphore::new(MAX_CONCURRENT_PROGRAM_RUNS))
    });

/// The programs being performed RIGHT NOW, by the key their caller claims them under.
///
/// A program's transfers are real effects on the object store, so one owed program is
/// performed once at a time however many callers report the resource that owes it: a second
/// call leaves it owed and the answer the first produces is the one that resource takes.
static PROGRAMS_IN_FLIGHT: ::std::sync::LazyLock<
    ::std::sync::Mutex<::std::collections::BTreeSet<String>>,
> = ::std::sync::LazyLock::new(|| ::std::sync::Mutex::new(::std::collections::BTreeSet::new()));

/// A claim on performing ONE program, given up when the THREAD that performs it really ends —
/// including a program the caller has already stopped waiting for, which is still making its
/// calls until then.
#[derive(Debug)]
pub struct InFlight {
    key: String,
}

impl Drop for InFlight {
    fn drop(&mut self) {
        held_programs().remove(&self.key);
    }
}

/// The in-flight set, recovering a guard a panicking program left poisoned: one caller's
/// program must not stop every later one from being performed.
fn held_programs() -> ::std::sync::MutexGuard<'static, ::std::collections::BTreeSet<String>> {
    PROGRAMS_IN_FLIGHT
        .lock()
        .unwrap_or_else(::std::sync::PoisonError::into_inner)
}

/// Claim the program `key` names for this call, or `None` when it is already being performed.
///
/// The key is the caller's: it names the ONE piece of work being performed — a step's command,
/// a cluster's bootstrap action — so two callers reporting the same resource claim the same
/// key and only the first performs it.
#[must_use]
pub fn claimed_program(key: &str) -> Option<InFlight> {
    held_programs()
        .insert(key.to_string())
        .then(|| InFlight { key: key.to_string() })
}

/// Perform `program` on a thread of its own and answer what it came to, or `None` when
/// nothing came back at all.
///
/// `thread_name` names that thread, `budget` is what the program's own calls may spend
/// ([`super::step_command_transfers::TRANSFER_BUDGET`] for every program this crate performs)
/// and the caller's wait for it is that budget plus [`PROGRAM_ABANDON_GRACE`]. `claim` is this
/// program's place in [`PROGRAMS_IN_FLIGHT`] and the permit is its share of
/// [`MAX_CONCURRENT_PROGRAM_RUNS`]; both are MOVED into that thread, so both are given up when
/// the thread really ends rather than when this call stops waiting. A program that found no
/// free slot inside [`PROGRAM_PERMIT_WAIT`] is performed by nothing here and stays owed an
/// answer.
pub async fn performed_on_its_own_thread<T: Send + 'static>(
    thread_name: &str,
    budget: Duration,
    claim: InFlight,
    program: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    let permit = ::tokio::time::timeout(
        PROGRAM_PERMIT_WAIT,
        ::std::sync::Arc::clone(&PROGRAM_RUN_PERMITS).acquire_owned(),
    )
    .await
    .ok()?
    .ok()?;
    let (sender, receiver) = ::tokio::sync::oneshot::channel();
    let spawned = ::std::thread::Builder::new()
        .name(thread_name.to_string())
        .spawn(move || {
            // Both held for as long as the program is being performed, whoever is still
            // waiting for it.
            let _claim = claim;
            let _permit = permit;
            let _ = sender.send(program());
        });
    if spawned.is_err() {
        return None;
    }
    match ::tokio::time::timeout(budget.saturating_add(PROGRAM_ABANDON_GRACE), receiver).await {
        Ok(Ok(answered)) => Some(answered),
        // The program was given up on, or its thread ended without reporting: nothing is
        // known about what it came to, so nothing is claimed about it.
        Ok(Err(_)) | Err(_) => None,
    }
}
