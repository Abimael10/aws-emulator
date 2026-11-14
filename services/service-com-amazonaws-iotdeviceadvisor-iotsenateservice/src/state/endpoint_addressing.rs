// SPDX-License-Identifier: CC0-1.0

//! WHICH host an operation that REPORTS a Device Advisor endpoint reports: the one value
//! `GetEndpoint` vends and a suite run reports and persists for its primary device.
//!
//! `endpoint` is a modelled member of this service's API (`Endpoint`, a hostname of
//! `len 45..75`), so what it carries is the Device Advisor hostname of the account or of
//! the device the request names ([`super::endpoint_for`]) — a value of the account, the
//! region the request is signed for and the device identity alone. The address a client
//! happens to have reached this emulator on is a property of that client's own
//! configuration, not of the resource being reported, so it is never substituted into the
//! member: a caller reading the region or the Device Advisor domain out of the reported
//! host, or validating it against the modelled length, is answered the same form AWS
//! answers whichever address the request arrived on.
//!
//! Both reporting operations come through here, so the host a start call publishes for a
//! device and the host `GetEndpoint` vends for that same device cannot drift.

use super::{DeviceIdentity, endpoint_for};

/// The Device Advisor endpoint reported for `device` in `region`, which is what
/// `GetEndpoint` vends and what a suite run stores and reports for its primary device.
pub fn announced_endpoint(region: &str, device: &DeviceIdentity<'_>) -> String {
    endpoint_for(region, device)
}
