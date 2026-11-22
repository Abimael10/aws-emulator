// SPDX-License-Identifier: CC0-1.0

//! The MFA devices one IAM user holds, across the KINDS of device the account keeps.
//!
//! One concern, held in one place: `ListMFADevices` "lists the MFA devices for an IAM user"
//! whatever kind each device is, while the account holds the two kinds separately — the
//! virtual MFA devices `CreateVirtualMFADevice` provisions
//! ([`ComAmazonawsIamAwsidentitymanagementv20100508State::virtual_mfa_devices`]) and the FIDO
//! security keys enrolled through the browser's WebAuthn ceremony
//! ([`ComAmazonawsIamAwsidentitymanagementv20100508State::fido_security_keys`]) — so the ONE
//! projection that spans both lives here rather than inside either.

#[allow(unused_imports)]
use super::*;

use crate::ops::types::MFADevice;

/// The marker scope of a page over ONE user's MFA devices, which `ListMFADevices` pages
/// over ([`MARKER_SCOPE_USERS`] states why each listing has its own scope: a position in
/// this collection names no position in the account-wide listing of virtual devices,
/// [`MARKER_SCOPE_VIRTUAL_MFA_DEVICES`]).
pub const MARKER_SCOPE_MFA_DEVICES: &str = "mfa-devices";

/// How many MFA devices ONE IAM user may have enrolled at a time.
///
/// AWS's "IAM and STS quotas" page gives the quota "MFA devices in use per IAM user: 8",
/// which is why `EnableMFADevice` models `LimitExceededException` ("it attempted to create
/// resources beyond the current … account limits. The error message describes the limit
/// exceeded"): the enrolment past it is refused and the device stays unassigned. The quota
/// is over the devices of EVERY kind the user holds, because what it bounds is the devices
/// "in use" by that user ([`ComAmazonawsIamAwsidentitymanagementv20100508State::mfa_devices_held_by`]).
pub const MAX_MFA_DEVICES_PER_USER: usize = 8;

/// The sentence IAM refuses an enrolment past [`MAX_MFA_DEVICES_PER_USER`] with.
///
/// Recorded behaviour, and a DIFFERENT template from the one IAM's other quota refusals
/// carry (`Cannot exceed quota for <quota>: <value>`): this one says `quota limit for` and
/// states the quota's NAME alone, with no value after it. Entirely fixed text — it names
/// neither the user nor the device — and the quota name in it is what identifies WHICH of
/// the operation's several `LimitExceeded` causes refused the call.
pub const MFA_DEVICES_PER_USER_QUOTA_EXCEEDED: &str =
    "Cannot exceed quota limit for MFADevicesPerUser";

impl ComAmazonawsIamAwsidentitymanagementv20100508State {
    /// How many MFA devices the IAM user held under `user_key` has enrolled, of every kind
    /// the account holds — what the per-user quota ([`MAX_MFA_DEVICES_PER_USER`]) is taken
    /// against, and the count `ListMFADevices` would report for that user.
    ///
    /// A READ: it only consults the store.
    pub fn mfa_devices_held_by(&self, user_key: &str) -> usize {
        let virtual_devices = self
            .virtual_mfa_devices
            .values()
            .filter(|record| record.user_key.as_deref() == Some(user_key))
            .count();
        let security_keys = self
            .fido_security_keys
            .values()
            .filter(|record| record.user_key == user_key)
            .count();
        virtual_devices.saturating_add(security_keys)
    }
}

impl ComAmazonawsIamAwsidentitymanagementv20100508State {
    /// The MFA devices the IAM user held under `user_key` has, as `ListMFADevices` reports
    /// them — "a list of MFA devices" of every kind the account holds, each carrying the
    /// user name, the serial it is addressed by and when it was enabled (`MFADevice`) —
    /// ordered by that serial, which is the key both kinds are held under and so the
    /// position a page resumes after.
    ///
    /// `user_name` is the name the devices are reported under: IAM names "are not
    /// distinguished by case", so what a device reports is the spelling the account holds
    /// the user under rather than the one a request happened to use.
    ///
    /// A virtual device the user holds carries its enable instant — `EnableMFADevice` writes
    /// the association and that instant together — and `MFADevice$EnableDate` is required,
    /// so a record carrying no instant is one no reader can report.
    ///
    /// A READ: it only consults the store.
    pub fn mfa_devices_of_user(&self, user_key: &str, user_name: &str) -> Vec<MFADevice> {
        let virtual_devices = self
            .virtual_mfa_devices
            .iter()
            .filter(|(_, record)| record.user_key.as_deref() == Some(user_key))
            .filter_map(|(serial_number, record)| {
                record.enable_date.map(|enable_date| MFADevice {
                    user_name: user_name.to_string(),
                    serial_number: serial_number.clone(),
                    enable_date,
                })
            });
        let security_keys = self
            .fido_security_keys
            .iter()
            .filter(|(_, record)| record.user_key == user_key)
            .map(|(serial_number, record)| MFADevice {
                user_name: user_name.to_string(),
                serial_number: serial_number.clone(),
                enable_date: record.enable_date,
            });
        let mut devices: Vec<MFADevice> = virtual_devices.chain(security_keys).collect();
        // Both kinds are held under their serial, so ordering the union by it is what makes
        // one page order over the two maps.
        devices.sort_by(|one, other| one.serial_number.cmp(&other.serial_number));
        devices
    }
}
