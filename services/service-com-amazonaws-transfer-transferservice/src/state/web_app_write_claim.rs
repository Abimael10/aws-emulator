// SPDX-License-Identifier: CC0-1.0

//! The guard that admits ONE writer of one of a web app's objects at a time.
//!
//! A web app has two independently written objects — its own configuration
//! (`UpdateWebApp`) and its customization (`UpdateWebAppCustomization` /
//! `DeleteWebAppCustomization`) — and every operation that writes one of them models
//! `ConflictException` (HTTP 409). The service applies one write of an object at a time:
//! a request that arrives while another write of the SAME object of the SAME web app is
//! still being applied is refused with that conflict rather than racing it, so two
//! writers that overlap have exactly one winner and what survives is one submitter's
//! values rather than a mixture of both.
//!
//! This is MUTUAL EXCLUSION between calls that are in flight at the same time, not a
//! window the web app sits in afterwards: the claim is taken when a write begins and
//! released as soon as that write is over, so a caller that writes AFTER reading the
//! previous write's answer is served, and only a caller whose call overlaps another's is
//! refused. Nothing here is due to the clock, so nothing is stored for a later call to
//! read.
//!
//! The claim is OWNED, not flagged: [`claim`] hands back a [`WriteClaim`] whose `Drop`
//! releases it, so it is released on every exit a request can take — the write
//! completing, a refusal after it was taken, the future being DROPPED at the await
//! (a caller that disconnects, a request that times out, a cancelled task), or an
//! unwind. A claim can therefore never outlive the request that took it and wedge a web
//! app into answering 409 forever. Nothing in this module holds the state lock across an
//! await, and the release takes that lock, so a caller must not be holding it when the
//! claim is dropped.

use crate::state::{ComAmazonawsTransferTransferserviceState as State, WebApp};

/// Which of a web app's independently written objects a claim is on.
///
/// The two are written by different operations and are never the same write, so a write
/// of one does not refuse a write of the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Object {
    /// The web app's own configuration — its identity provider, access endpoint, units
    /// and endpoint — written by `UpdateWebApp`.
    Configuration,
    /// The web app's customization object, written by `UpdateWebAppCustomization` and
    /// `DeleteWebAppCustomization`.
    Customization,
}

impl Object {
    /// The flag on a stored web app that says whether a write of this object is in
    /// flight. One place per object, so the take and the release cannot disagree.
    fn in_flight(self, web_app: &mut WebApp) -> &mut bool {
        match self {
            Object::Configuration => &mut web_app.configuration_write_in_flight,
            Object::Customization => &mut web_app.customization_write_in_flight,
        }
    }
}

/// Why a write did not get the claim on one of a web app's objects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// Another write of this object of this web app holds it: the modelled
    /// `ConflictException`.
    Conflict,
    /// The store holds no such web app: the modelled `ResourceNotFoundException`.
    NoSuchWebApp,
}

/// The claim one request holds on one object of one web app while it writes it.
///
/// Held by VALUE for as long as the write may still happen, and released when it is
/// dropped — see the module header for why that is what the claim is, rather than a flag
/// a success path clears.
///
/// No `Debug`: the state handle it carries is the whole store, which has none to
/// borrow.
pub struct WriteClaim {
    /// The store the claim lives in, as a handle: the claim has to be releasable without
    /// the request that took it still being there to do it.
    state: ::engine::ServiceState<State>,
    web_app_id: String,
    object: Object,
}

impl Drop for WriteClaim {
    fn drop(&mut self) {
        // Releasing a claim on a web app the store no longer holds is nothing to do: the
        // web app the claim was on is gone, and so is the claim.
        if let Some(web_app) = self.state.lock().web_apps.get_mut(&self.web_app_id) {
            *self.object.in_flight(web_app) = false;
        }
    }
}

/// Take the claim on `object` of `web_app_id`, when the web app exists and no other
/// write of that object holds it.
///
/// The caller must NOT hold the state lock: this takes it, and releases it before
/// answering.
pub fn claim(
    state: &::engine::ServiceState<State>,
    web_app_id: &str,
    object: Object,
) -> Result<WriteClaim, Refusal> {
    {
        let mut guard = state.lock();
        let Some(web_app) = guard.web_apps.get_mut(web_app_id) else {
            return Err(Refusal::NoSuchWebApp);
        };
        let in_flight = object.in_flight(web_app);
        if *in_flight {
            return Err(Refusal::Conflict);
        }
        *in_flight = true;
    }
    Ok(WriteClaim {
        state: state.clone(),
        web_app_id: web_app_id.to_string(),
        object,
    })
}

/// Let the runtime run whatever else is ready while this request holds the claim.
///
/// The claim is only worth taking if a request that is being handled AT THE SAME TIME
/// can be admitted far enough to see it: this is the point in a write where the peer
/// racing it reaches [`claim`], which is what makes one of two overlapping writers the
/// loser instead of both silently winning.
pub async fn admit_a_racing_writer() {
    ::tokio::task::yield_now().await;
}
