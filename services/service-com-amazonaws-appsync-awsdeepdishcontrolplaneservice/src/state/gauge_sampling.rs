// SPDX-License-Identifier: CC0-1.0

//! The CLAIM one data plane's gauge sampler is held under: who runs it, which claim that
//! runner holds, and the task it runs in.
//!
//! Both of this service's real-time planes report gauges — how many connections are open on
//! an API and how many subscriptions those connections hold — and both observe them on a
//! cadence rather than only when they change
//! ([`crate::service::gauge_sampling`] states why). The bookkeeping that keeps exactly ONE
//! sampler per API is the same for either plane, so it lives here once: a flag saying a
//! sampler runs, a GENERATION so a sampler that has stood down cannot release a claim a
//! later one took, and the task handle, held so the sampler an API's first connection
//! started goes when that API's records go.

/// The single gauge sampler of one API: whether one runs, which claim it holds, and its
/// task.
#[derive(Debug, Default)]
pub struct GaugeSampling {
    /// Whether a sampler is running for this API ([`Self::claim`]).
    running: bool,
    /// Which claim the running sampler holds, so a sampler that has stood down cannot
    /// release a claim a later one took.
    generation: u64,
    /// The running sampler's task, held so the task a connection started stays stoppable.
    sampler: Option<::tokio::task::JoinHandle<()>>,
}

impl GaugeSampling {
    /// Claim the sampler: the claim's GENERATION when the caller may start one, `None` when
    /// one already runs.
    pub fn claim(&mut self) -> Option<u64> {
        if self.running {
            return None;
        }
        self.running = true;
        self.generation = self.generation.wrapping_add(1);
        Some(self.generation)
    }

    /// Keep the running sampler's task handle, aborting the one it replaces — and only while
    /// `generation` is still the current claim AND that claim is still held, so a starter
    /// whose own task has already stood down installs nothing and aborts nothing.
    pub fn install(&mut self, generation: u64, handle: ::tokio::task::JoinHandle<()>) {
        if !self.running || self.generation != generation {
            handle.abort();
            return;
        }
        if let Some(previous) = self.sampler.replace(handle) {
            previous.abort();
        }
    }

    /// Whether the sampler holding `generation` is still wanted — and, when `wanted` says
    /// it is not, RELEASE that claim.
    ///
    /// Asking and standing down happen in ONE call, which is what makes the hand-over
    /// race-free under the store's lock: a connection records itself BEFORE it claims a
    /// sampler, so either this call sees that connection and the running sampler keeps it, or
    /// the new claim finds the flag cleared and starts a sampler of its own.
    pub fn wanted_or_release(&mut self, generation: u64, wanted: bool) -> bool {
        if !wanted && self.generation == generation {
            self.running = false;
        }
        wanted
    }

    /// Stop the running sampler — what an API whose records have just gone does to it: a
    /// sampler reads those records, so one whose API is no longer held has nothing left to
    /// observe.
    pub fn abort(self) {
        if let Some(sampler) = self.sampler {
            sampler.abort();
        }
    }
}
