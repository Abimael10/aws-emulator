// SPDX-License-Identifier: CC0-1.0

//! How much memory ONE run holds, as the HOST measures it.
//!
//! What a run is reported to have used is a reading, never an assumption, and it is the same
//! reading for every engine: the memory the run's own thread has faulted in since the instant
//! the host marked as this run's own — after the engine the handler runs in is standing and
//! before a line of the run's own code is read. So the figure covers the handler's work and
//! not the environment this emulator built to run it in, and it is comparable across the
//! engines ([`super::python`], [`super::node`]) because neither engine's own accounting is
//! what it comes from.
//!
//! The count of faulted pages never falls, so what is kept is an UPPER bound on what the run
//! holds at the instant it is read — a page given back and touched again is counted twice —
//! which is the side to err on for a figure a memory bound is judged on.
//!
//! What the run put on a STORE this process keeps for it that is not the function's memory is
//! left out of that reading ([`Held::discount`]): the files a run writes to its function's
//! ephemeral storage are a disk on the real runtime and are held in this process's memory
//! here, so counting them would report a function as holding memory it is not. How much may be
//! left out is the HOST's own decision, bounded per run and for this process together
//! ([`super::python::Bounds::wrote_to_ephemeral_storage`]).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// The size of a page of this process's memory, which the faulted-page count is scaled by.
const PAGE_BYTES: u64 = 4_096;

/// How many bytes of memory THIS thread has faulted in, as the host reports it.
pub(super) fn faulted_bytes() -> u64 {
    match ::nix::sys::resource::getrusage(::nix::sys::resource::UsageWho::RUSAGE_THREAD) {
        Ok(usage) => {
            let pages = usage.minor_page_faults().saturating_add(usage.major_page_faults());
            u64::try_from(pages).unwrap_or(0).saturating_mul(PAGE_BYTES)
        }
        Err(_) => 0,
    }
}

/// What ONE run holds of the host: the baseline the host took for it, and the greatest
/// reading taken since.
///
/// Every reading is this THREAD's, so both the baseline and every sample are taken on the
/// thread the run happens on. The baseline is taken by the HOST and by no guest-reachable
/// call: a run that could re-take its own baseline could hold the ceiling's worth of memory
/// again after every call.
#[derive(Debug)]
pub(super) struct Held {
    /// What the run's thread had faulted in when the host marked the run's own start.
    baseline: AtomicU64,
    /// The greatest reading of what the run holds beyond that baseline.
    peak: AtomicU64,
    /// Whether that instant has been marked yet, which is the instant from which a reading
    /// says something about THIS RUN.
    measured: AtomicBool,
    /// How much of what this thread has faulted in since that baseline is NOT the run's
    /// memory: the bytes the run put on a store this process keeps for it that is not memory
    /// at all ([`Self::discount`]).
    discounted: AtomicU64,
}

impl Held {
    /// A measurement of a run that has not reached its own code yet: the baseline stands at
    /// what this thread has faulted in now, and no reading of the run has been taken.
    pub(super) fn new() -> Self {
        Self {
            baseline: AtomicU64::new(faulted_bytes()),
            peak: AtomicU64::new(0),
            measured: AtomicBool::new(false),
            discounted: AtomicU64::new(0),
        }
    }

    /// Measure this run from HERE: what this thread has faulted in now is the baseline every
    /// later reading is taken against.
    pub(super) fn measure_from_here(&self) {
        self.baseline.store(faulted_bytes(), Ordering::Relaxed);
        // Last, so no reading is ever judged against a baseline that is still the one taken
        // when this measurement was built.
        self.measured.store(true, Ordering::SeqCst);
    }

    /// Hold `bytes` of what this thread has faulted in since the baseline to be something
    /// OTHER than the run's own memory, which is what every reading below then leaves out.
    ///
    /// The figure is an ABSOLUTE one and is only ever raised, because the faulted-page count
    /// it is taken out of only ever rises as well: pages this run touched for such a store
    /// stay counted after the run has given the store's bytes back, so a reading that stopped
    /// leaving them out would report them as the run's memory. How much may be left out is
    /// decided and bounded by the HOST before this is called
    /// ([`super::python::Bounds::wrote_to_ephemeral_storage`]), so no guest can discount
    /// memory of its own.
    pub(super) fn discount(&self, bytes: u64) {
        self.discounted.fetch_max(bytes, Ordering::Relaxed);
    }

    /// Read what the run holds beyond its baseline, keep it if it is the greatest reading so
    /// far, and report it.
    ///
    /// Before the run's own start was marked there is no reading that speaks for the run, so
    /// none is kept.
    pub(super) fn sample(&self) -> u64 {
        let held = faulted_bytes()
            .saturating_sub(self.baseline.load(Ordering::Relaxed))
            .saturating_sub(self.discounted.load(Ordering::Relaxed));
        if self.measured.load(Ordering::Relaxed) {
            self.peak.fetch_max(held, Ordering::Relaxed);
        }
        held
    }

    /// The MOST memory the run was measured holding, in bytes: the greatest reading taken of
    /// it, including one taken here — so a run whose last stretch of work was not sampled is
    /// still reported on a reading that covers it.
    pub(super) fn max_bytes(&self) -> u64 {
        self.sample();
        self.peak.load(Ordering::Relaxed)
    }
}
