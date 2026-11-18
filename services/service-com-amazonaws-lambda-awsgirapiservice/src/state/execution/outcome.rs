// SPDX-License-Identifier: CC0-1.0

//! What one RUN of a handler is allowed to spend, and what it came to.
//!
//! An invocation's answer is the same shape whichever language the handler is written in —
//! the value it returned, the error it raised, the bound it ran past — so these are the
//! types both engines ([`super::python`], [`super::node`]) report in and the one
//! [`super::run_invocation`] reads. Keeping them here is what lets the operation's answer
//! be built once for every runtime rather than once per engine.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

/// What one invocation is allowed to spend.
#[derive(Debug, Clone, Copy)]
pub struct Budget {
    /// How long the handler may run for.
    pub wall_clock: Duration,
    /// How many traced events the handler may execute.
    pub steps: u64,
    /// How much memory the run may add to this process.
    pub memory_bytes: u64,
    /// How many bytes of the handler's output are kept.
    pub log_bytes: usize,
    /// How large the handler's response may be.
    pub response_bytes: usize,
    /// How deep the handler may recurse.
    pub recursion_limit: usize,
}

/// Which bound a stopped run ran past.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stop {
    /// The run outlasted its wall clock.
    Deadline,
    /// The run executed more traced events than its budget.
    Steps,
    /// The run grew this process past its ceiling.
    Memory,
    /// The handler's response is larger than one may be.
    Response,
    /// The run took more of THIS HOST's own time than a run bounded against its clock may
    /// ([`super::run_clock::Clock::within`]).
    ///
    /// Not the run's own deadline: it is what bounds the host a run whose own time this
    /// host's clock does not move runs on, so a run stopped here is one this emulator gave up
    /// on rather than one whose own span ran out.
    HostCeiling,
}

impl Stop {
    /// What the stop is reported as, as the reason of the run's failure.
    #[must_use]
    pub fn reason(self) -> &'static str {
        match self {
            Self::Deadline => "the invocation ran past its timeout",
            Self::Steps => "the invocation ran past its step budget",
            Self::Memory => "the invocation ran past its memory ceiling",
            Self::Response => "the response is larger than the invocation may return",
            Self::HostCeiling => "the invocation took more host time than one run may take",
        }
    }
}

/// What the handler came to.
#[derive(Debug, Clone)]
pub enum Completion {
    /// The handler returned, and this is the JSON its value was serialized to.
    Returned(String),
    /// A RESPONSE-STREAMING handler finished, and these are the writes it made to its
    /// response stream, in the order it made them.
    ///
    /// Such a handler's answer is what it WROTE rather than what it returned, so the
    /// boundaries here are the handler's own writes — which is what a streaming caller
    /// reads as the stream's chunks.
    Streamed {
        /// The writes the handler made, in order.
        chunks: Vec<String>,
        /// The failure the handler came to once it had already written, when it failed;
        /// see [`StreamFailure`].
        failure: Option<StreamFailure>,
    },
    /// The handler (or the runtime resolving it) raised.
    Raised {
        /// The exception's type name, or the `Runtime.*` class of a runtime failure.
        error_type: String,
        /// The exception's message.
        error_message: String,
        /// The handler's own stack, one formatted frame per entry.
        stack_trace: Vec<String>,
    },
    /// The run was stopped by one of its bounds.
    Stopped(Stop),
    /// The interpreter itself could not be built or driven. Not a guest failure.
    Internal(String),
}

/// The failure a RESPONSE-STREAMING handler came to AFTER it had already written to its
/// response stream.
///
/// A stream that has carried bytes cannot be withdrawn, so the bytes already written stay
/// the answer and the failure is reported BESIDE them — which is what lets a streaming
/// caller tell a complete response from one cut short. A handler that failed before its
/// first write wrote nothing and carries no failure here: it delivered no bytes, and its
/// error is in the run's log alone.
#[derive(Debug, Clone)]
pub struct StreamFailure {
    /// The exception's type name.
    pub error_type: String,
    /// The exception's message.
    pub error_message: String,
    /// The handler's own stack, one formatted frame per entry.
    pub stack_trace: Vec<String>,
}

/// One finished run.
#[derive(Debug, Clone)]
pub struct Finished {
    /// What the handler came to.
    pub completion: Completion,
    /// Everything the handler wrote to its output streams, capped.
    pub log: String,
    /// That same output as the RECORDS the run wrote it in, in order: the text of one write,
    /// the break that closed it included ([`Capture::records`]).
    ///
    /// A record is what the run wrote in ONE call, so text carrying line breaks of its own —
    /// the stack of an error written to the console — is one record and not several: that is
    /// what keeps one log event per thing the handler reported ([`super::log_delivery`]).
    ///
    /// These are the records of the INVOKE phase alone; what the initialization wrote is
    /// [`Finished::init_records`].
    pub log_records: Vec<String>,
    /// What the INITIALIZATION phase wrote, in order and in the same form: the records the
    /// module-level code of the package left while the execution environment was being built,
    /// before this invocation was bound to it ([`Capture::begin_invocation`]).
    ///
    /// Kept apart from the invocation's own records because output written during init
    /// precedes the invocation's opening frame in the function's log, which is how a client
    /// attributes module-scope output to the cold start.
    pub init_records: Vec<String>,
    /// Whether this run paid the INITIALIZATION phase — whether the execution environment it
    /// ran in was built for it rather than already standing.
    pub initialized: bool,
    /// How long that initialization took, for a run that paid it and reached its handler's
    /// own phase — what the run's report states as its `Init Duration`.
    pub init_duration: Option<Duration>,
    /// The log stream the EXECUTION ENVIRONMENT that served this run writes to, when that
    /// environment decided one of its own ([`super::warm_pool`]), and `None` for a run whose
    /// environment was built for it alone — whose stream is the one its caller composed.
    pub log_stream: Option<String>,
    /// How long the run took.
    pub elapsed: Duration,
    /// The MOST memory this run was measured holding, in bytes, or `None` for a run nothing
    /// measured because nothing ran.
    ///
    /// Measured, never assumed: it is the greatest reading the engine running the handler
    /// took of what the run holds of the host, from the baseline set before a line of the
    /// run's own code was read ([`super::python`], [`super::node`]) — so it is the same
    /// quantity the run's memory ceiling is judged on. `Some(0)` is a run that ran and faulted
    /// no page of its own beyond that baseline, which is what a warm environment serving a
    /// handler that allocates nothing comes to.
    pub max_memory_bytes: Option<u64>,
}

/// What the handler wrote to its output streams, capped at the run's log budget.
#[derive(Debug)]
pub(super) struct Capture {
    /// The text kept so far.
    pub(super) text: String,
    /// The RECORDS that text was written in, each AS IT WAS WRITTEN — the break that closed
    /// it included, and no break at all for a record the run never closed.
    records: Vec<String>,
    /// Whether the last of them is still open — written by a call that did not end on a line
    /// break, so the next write continues it.
    open: bool,
    /// How many records had been written when the INVOKE phase began, for a capture the
    /// initialization wrote into as well, and `None` while no invoke phase has begun on it.
    initialized: Option<usize>,
    /// How many bytes are kept at most.
    limit: usize,
}

impl Capture {
    /// A capture that keeps at most `limit` bytes.
    pub(super) fn new(limit: usize) -> Self {
        Self {
            text: String::new(),
            records: Vec::new(),
            open: false,
            initialized: None,
            limit,
        }
    }

    /// Mark the instant the INVOKE phase begins: every record written so far belongs to the
    /// initialization of the execution environment, and every record after it to the
    /// invocation being served.
    ///
    /// One-shot, so the boundary is the FIRST invocation's own: a capture is never written
    /// into by two invocations.
    pub(super) fn begin_invocation(&mut self) {
        if self.initialized.is_none() {
            self.initialized = Some(self.records.len());
            // A write the initialization never closed is a record of the initialization: what
            // the invocation writes next is its own record rather than a continuation of one
            // written before it was bound.
            self.open = false;
        }
    }

    /// How many of the records belong to the initialization: none until an invoke phase has
    /// begun on this capture, because a run whose invoke phase never began wrote everything it
    /// wrote as the run it is.
    fn initialized(&self) -> usize {
        self.initialized.unwrap_or(0).min(self.records.len())
    }

    /// The records the INITIALIZATION wrote, in order.
    pub(super) fn init_records(&self) -> Vec<String> {
        self.records[..self.initialized()].to_vec()
    }

    /// The records the INVOCATION itself wrote, in order.
    pub(super) fn invocation_records(&self) -> Vec<String> {
        self.records[self.initialized()..].to_vec()
    }

    /// Append `text`, stopping at the cap. Past it the capture keeps what it already has.
    pub(super) fn write(&mut self, text: &str) {
        if self.text.len() >= self.limit {
            return;
        }
        let room = self.limit - self.text.len();
        // Cut on a character boundary, so the kept log is still text.
        let mut end = text.len().min(room);
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        let kept = &text[..end];
        if kept.is_empty() {
            return;
        }
        self.text.push_str(kept);
        self.recorded(kept);
    }

    /// Add `kept` to those records: it continues the one still open, and a write that ends on
    /// a line break CLOSES one — which is what makes the record the unit the run wrote in
    /// rather than the line the text breaks into. The break stays part of the record, because
    /// the record is the text the run wrote.
    fn recorded(&mut self, kept: &str) {
        if self.open {
            if let Some(last) = self.records.last_mut() {
                last.push_str(kept);
            }
        } else {
            self.records.push(kept.to_string());
            self.open = true;
        }
        if kept.ends_with('\n') {
            self.open = false;
        }
    }
}

/// Read the capture, which a panic in guest-driven code cannot poison out of use: what it
/// already holds is still the only output there is.
pub(super) fn captured(capture: &Arc<Mutex<Capture>>) -> MutexGuard<'_, Capture> {
    capture.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The log a run writes is kept up to the budget's cap and no further — one row per
    /// size class around it.
    #[test]
    fn the_capture_keeps_at_most_its_budget() {
        let cases: &[(usize, usize, usize)] = &[
            (16, 0, 0),
            (16, 8, 8),
            (16, 16, 16),
            (16, 17, 16),
            (16, 4_096, 16),
        ];
        for (limit, written, expected) in cases {
            let capture = Arc::new(Mutex::new(Capture::new(*limit)));
            captured(&capture).write(&"L".repeat(*written));
            let actual = captured(&capture).text.len();
            assert_eq!(
                actual, *expected,
                "case a {limit}-byte cap with {written} bytes written: expected {expected} \
                 bytes kept, got {actual}"
            );
        }
    }

    /// A run's output is kept as the RECORDS it was written in: one per write the run closed
    /// with a line break, each carrying the break that closed it, and a line break INSIDE one
    /// write stays inside its record.
    #[test]
    fn the_capture_keeps_one_record_per_write_the_run_closed() {
        // (case, the writes the run made, the records they leave)
        let cases: &[(&str, &[&str], &[&str])] = &[
            ("one line", &["hello\n"], &["hello\n"]),
            (
                "two lines, each written and closed",
                &["a\n", "b\n"],
                &["a\n", "b\n"],
            ),
            (
                "a write carrying line breaks of its own",
                &["TypeError: boom\n    at handler\n"],
                &["TypeError: boom\n    at handler\n"],
            ),
            (
                "a line written in pieces, as a print writes its break apart",
                &["hello", "\n"],
                &["hello\n"],
            ),
            // A record the run never closed carries no break, because the run wrote none —
            // which is how a runtime writes a report of several lines as ONE record.
            ("a write the run never closed", &["half"], &["half"]),
            ("a line of its own with nothing on it", &["\n"], &["\n"]),
            ("nothing written at all", &[], &[]),
        ];
        for (case, writes, expected) in cases {
            let capture = Arc::new(Mutex::new(Capture::new(4_096)));
            for written in *writes {
                captured(&capture).write(written);
            }
            let actual = captured(&capture).invocation_records();
            let expected: Vec<String> = expected.iter().map(|held| (*held).to_string()).collect();
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
            // The records ARE the log: they are the text the run wrote, in the pieces it
            // wrote it in.
            let rebuilt: String = actual.concat();
            let text = captured(&capture).text.clone();
            assert_eq!(
                rebuilt, text,
                "case {case:?}: expected the records to rebuild the log, got {rebuilt:?} \
                 against {text:?}"
            );
        }
    }

    /// RULE: the records written before the INVOKE phase began are the initialization's, and
    /// every record after it the invocation's — which is what lets the output of the init
    /// phase precede the invocation's own opening frame in the function's log.
    #[test]
    fn the_capture_tells_the_initializations_records_from_the_invocations() {
        // (case, the writes before the phase began, the writes after it)
        let cases: &[(&str, &[&str], &[&str])] = &[
            ("a module that wrote nothing", &[], &["handled\n"]),
            ("a module that wrote one line", &["imported\n"], &["handled\n"]),
            (
                "a module that wrote several",
                &["one\n", "two\n"],
                &["handled\n", "and more\n"],
            ),
            (
                "a module whose last write it never closed",
                &["half"],
                &["handled\n"],
            ),
            ("an invocation that wrote nothing of its own", &["init\n"], &[]),
        ];
        for (case, before, after) in cases {
            let capture = Arc::new(Mutex::new(Capture::new(4_096)));
            for written in *before {
                captured(&capture).write(written);
            }
            captured(&capture).begin_invocation();
            for written in *after {
                captured(&capture).write(written);
            }
            let init = captured(&capture).init_records();
            let invocation = captured(&capture).invocation_records();
            assert_eq!(
                init.len(),
                before.len(),
                "case {case:?}: expected {} initialization record(s), got {init:?}",
                before.len()
            );
            assert_eq!(
                invocation.len(),
                after.len(),
                "case {case:?}: expected {} record(s) of the invocation, got {invocation:?}",
                after.len()
            );
            // Together they are the whole log, in the order the run wrote it.
            let rebuilt: String = init.iter().chain(invocation.iter()).cloned().collect();
            let text = captured(&capture).text.clone();
            assert_eq!(
                rebuilt, text,
                "case {case:?}: expected the two halves to rebuild the log, got {rebuilt:?} \
                 against {text:?}"
            );
        }
    }

    /// A multi-byte character is never cut in half by the cap: what is kept is still text.
    #[test]
    fn the_capture_cuts_on_a_character_boundary() {        for limit in [0_usize, 1, 2, 3, 4, 5] {
            let capture = Arc::new(Mutex::new(Capture::new(limit)));
            // Three bytes per character, so every cap but a multiple of three has to cut
            // short of it.
            captured(&capture).write("☃☃");
            let kept = captured(&capture).text.clone();
            assert!(
                kept.chars().all(|character| character == '☃'),
                "case a {limit}-byte cap: expected whole characters, got {kept:?}"
            );
            assert!(
                kept.len() <= limit,
                "case a {limit}-byte cap: expected at most {limit} bytes, got {}",
                kept.len()
            );
        }
    }
}
