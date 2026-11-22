// SPDX-License-Identifier: CC0-1.0

//! A GraphQL API's real-time plane as the gauge sampler sees it: where its sampler's claim
//! is kept, and what ONE sample of its two gauges reads.
//!
//! The cadence, the one-sampler-per-API claim and the loop itself are the tree's own
//! ([`crate::service::gauge_sampling`] states why the gauges are observed rather than only
//! written when they change); what this module adds is this plane's answer to each of that
//! contract's four questions, all of them off this service's own store.

use super::PlaneContext;
use crate::service::gauge_sampling::{self, SampledPlane};

impl SampledPlane for PlaneContext {
    fn claim_sampler(&self) -> Option<u64> {
        self.state
            .lock()
            .graphql_data_plane
            .claim_gauge_sampler(&self.api_id)
    }

    fn install_sampler(&self, generation: u64, handle: ::tokio::task::JoinHandle<()>) {
        self.state
            .lock()
            .graphql_data_plane
            .set_gauge_sampler(&self.api_id, generation, handle);
    }

    fn sampling_wanted(&self, generation: u64) -> bool {
        self.state
            .lock()
            .graphql_data_plane
            .gauge_sampling_wanted_or_release(&self.api_id, generation)
    }

    /// Take ONE sample of the API's two gauges and publish it, while the API holds a
    /// connection.
    ///
    /// The counts are READ from the store and published as they stand: nothing is written to
    /// this service's own records, so a sample changes nothing a later caller sees. A plane
    /// that emptied between two samples publishes nothing — its last reading is the one the
    /// closing connection published — so the series carries readings only for the time the
    /// API was connected to.
    fn publish_sample(&self) {
        let counts = self.connection_counts();
        if counts.connections == 0 {
            return;
        }
        self.metrics.sampled(counts);
    }
}

/// Start the gauge sampler of the API `context` serves, if one is not already running.
///
/// Called by the handshake that has just recorded a real-time connection on the API: that is
/// the write which gives the plane a quantity to observe.
pub fn drive_gauge_sampling(context: &PlaneContext) {
    gauge_sampling::drive(context);
}
