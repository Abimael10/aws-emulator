// SPDX-License-Identifier: CC0-1.0

//! The periodic SAMPLING of the two `AWS/AppSync` gauges a real-time plane reports: how
//! many connections are open on an API, and how many subscriptions those connections hold.
//!
//! A gauge is not a count of events. Each of these two series measures a quantity that HAS
//! a value at every instant, so a reader laying periods over it expects a reading of the
//! quantity IN each period — including a period after it fell, which reads the lower value,
//! and every later period while it stays there. A series written only when the quantity
//! CHANGES cannot answer that: the last change is the last datapoint, so a reader windowing
//! any period after it finds no data at all and the value it last saw looks like the value
//! the quantity still has. A subscriber that unsubscribed on a connection it deliberately
//! holds open is exactly that case — the subscriptions fall to zero while the connections
//! do not — and an alarm or dashboard built on the subscription gauge would latch on the
//! reading taken before the drop.
//!
//! So the gauges are OBSERVED on a cadence ([`SAMPLE_INTERVAL`]) for as long as the API
//! holds a connection, which is what makes every period of that time carry a reading of
//! what the plane holds then. Three properties of it, each of them observable:
//!
//! * IT IS OFF THE RESPONSE PATH. No client call produces these datapoints: they are what
//!   an operator reads, so they exist whether or not anybody is calling. The sampler is
//!   claimed by the write that gives the API something to sample — a real-time connection
//!   being opened ([`drive`]) — and nothing on a read path starts, advances or stops it.
//! * NOTHING IS SAMPLED FOR A PLANE NOTHING IS CONNECTED TO. The sampler stands down once
//!   the API holds no connection ([`SampledPlane::sampling_wanted`]), so an API nobody has
//!   connected to carries no datapoint in the window rather than a run of zeros, and the
//!   final reading of a plane that emptied is the one the closing connection published.
//! * THE READINGS ARE THE STORE'S. Every sample is the count the store holds AT that moment
//!   ([`SampledPlane::publish_sample`]); nothing here keeps a count of its own, so a gauge
//!   and the plane it measures cannot drift.
//!
//! BOTH of this service's real-time planes report these two gauges — an Event API's
//! ([`super::event_api::gauges`]) and a GraphQL API's ([`super::graphql_api::gauges`]) — so the cadence,
//! the one-sampler-per-API claim and the loop they are observed in are written once here
//! and each plane states only what its own store holds.

use ::std::time::Duration;

/// How often the two gauges are observed while an API holds a connection.
///
/// An EMULATOR-CHOSEN cadence — AppSync publishes the series, not a sampling frequency —
/// and deliberately shorter than the one-minute resolution the datapoints are stored at
/// ([`super::event_api::metrics`]), so every minute of a held-open connection carries several
/// readings and the minute after a change reads what the plane holds rather than nothing. It
/// is non-zero, so the sampler hands the runtime back between samples instead of spinning.
pub const SAMPLE_INTERVAL: Duration = Duration::from_secs(5);

/// One API's real-time plane as the sampler sees it: the claim its single sampler is held
/// under, and the one sample it publishes.
///
/// Implemented by each plane's own socket context, which is what carries the store the
/// counts are read from and the publisher they are reported through.
pub trait SampledPlane: Clone + Send + Sync + 'static {
    /// Claim the single sampler of the API this plane serves: the claim's GENERATION when
    /// the caller may start one, `None` when one already runs or the API holds no socket.
    fn claim_sampler(&self) -> Option<u64>;

    /// Keep the running sampler's task handle under the claim `generation`.
    fn install_sampler(&self, generation: u64, handle: ::tokio::task::JoinHandle<()>);

    /// Whether the sampler holding `generation` still has gauges to observe, releasing that
    /// claim when it has not.
    fn sampling_wanted(&self, generation: u64) -> bool;

    /// Publish ONE sample of the two gauges, as the store holds them at this moment.
    fn publish_sample(&self);
}

/// Start `plane`'s gauge sampler, if one is not already running.
///
/// Called by the handshake that has just recorded a real-time connection on the API: that is
/// the write which gives the plane a quantity to observe. At most one sampler runs per API
/// (the store carries the claim), it samples for as long as the API holds a connection, and
/// with no async runtime to spawn onto there is no sampler — the readings are then the ones
/// the connection's own lifecycle publishes.
pub fn drive<Plane: SampledPlane>(plane: &Plane) {
    let Ok(runtime) = ::tokio::runtime::Handle::try_current() else {
        return;
    };
    let Some(generation) = plane.claim_sampler() else {
        return;
    };
    let sampling = plane.clone();
    let handle = runtime.spawn(async move {
        // The sampler ends when the API holds no connection left to observe, which it sees
        // — and releases its claim in — inside one critical section.
        while sampling.sampling_wanted(generation) {
            // Every sample waits first, which is what hands the runtime back between them.
            // No store lock is held across it.
            ::tokio::time::sleep(SAMPLE_INTERVAL).await;
            sampling.publish_sample();
        }
    });
    plane.install_sampler(generation, handle);
}
