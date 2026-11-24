// SPDX-License-Identifier: CC0-1.0

//! ONE DIRECTORY'S RADIUS MULTI-FACTOR AUTHENTICATION: what `EnableRadius` records for a
//! directory, the status that record reports and the wording each refusal of the enable is
//! stated with.
//!
//! A directory has at most ONE RADIUS configuration — the enable takes the `DirectoryId` and
//! the settings and nothing that identifies a second one, and the reporting operations carry
//! one `RadiusSettings` per directory — so the record lives on the directory it belongs to
//! ([`super::Directory::radius`]) and deleting the directory takes it with it. A directory no
//! enable has ever named holds none, which is why the members that report it are structurally
//! ABSENT for it rather than reported empty.
//!
//! The status is the SERVICE's rather than the caller's: the enable takes no status and
//! answers none, so it records the TRANSIENT first value of the modelled `RadiusStatus` enum
//! ([`RADIUS_STATUS_CREATING`]) and the clock settles it one [`TRANSITION_SPAN`] later, the
//! same way every other transition this service owes is settled
//! ([`super::ComAmazonawsDirectoryserviceDirectoryservice20150416State::advance_clock`]) — so
//! a caller that reads the directory straight after the enable still sees the MFA server
//! connection being brought up.
//!
//! # The shared secret is NOT held
//!
//! `RadiusSettings$SharedSecret` is the credential the directory's servers would authenticate
//! to the RADIUS server with. The model marks it `@sensitive`, Directory Service does not
//! report it back on any read, and nothing in this emulator ever opens a RADIUS conversation,
//! so the value is used to decide the enable ([`RADIUS_SHARED_SECRET_REQUIRED`]) and then
//! dropped: the stored settings carry every other member exactly as the request sent them and
//! that one as absent. It is the same rule the administrator password of a create follows
//! ([`super::account_credentials`]) — state this emulator does not need is state it must not
//! hold — so no read, and no refusal, can disclose a secret a caller submitted.

use super::*;

use crate::ops::types::RadiusSettings;

/// The status a directory's RADIUS configuration carries between `EnableRadius` and
/// [`TRANSITION_SPAN`]: the `Creating` value of the modelled `RadiusStatus` enum
/// (`smithy_query(query="shape",shape="RadiusStatus")`), whose first value it is.
///
/// The enable answers before the MFA server connection is up, so the move to
/// [`RADIUS_STATUS_COMPLETED`] is owed by the clock rather than by any later call.
pub const RADIUS_STATUS_CREATING: &str = "Creating";

/// The status a RADIUS configuration reaches once that transition has fallen due: the
/// `Completed` value of the same modelled enum.
pub const RADIUS_STATUS_COMPLETED: &str = "Completed";

/// The `ClientException` diagnostic an enable is refused with when the directory already
/// carries a RADIUS configuration.
///
/// Directory Service answers a repeated enable in the class it answers a resource a caller
/// asks it to make twice in — the same class an address block the directory already routes is
/// refused in ([`ip_routes_already_exist`]) — rather than in the entity class. The sentence
/// states the act it could not perform, then BOTH positions that refuse it (a configuration
/// that is already enabled, and one still being brought up: the transient
/// [`RADIUS_STATUS_CREATING`] refuses the enable exactly as the settled status does), and
/// closes with the directory the request named, so a caller sweeping many directories can
/// attribute it. Nothing of the stored configuration is disclosed by it. [`refusal`] appends
/// the request id, once.
#[must_use]
pub fn radius_already_enabled(directory_id: &str) -> String {
    format!(
        "Could not enable radius as it is already enabled or currently being configured for \
         directory: {directory_id}"
    )
}

/// The `InvalidParameterException` diagnostic an enable that carries no shared secret is
/// refused with.
///
/// `RadiusSettings$SharedSecret` is optional in the shape — `UpdateRadius` and the creates
/// share it — and its own documentation states the requirement of THIS operation: "Required
/// for enabling RADIUS on the directory". So the enable states the member the model spells,
/// then what it is required for. [`refusal`] appends the request id, once.
pub const RADIUS_SHARED_SECRET_REQUIRED: &str =
    "SharedSecret is required for enabling RADIUS on the directory.";

/// The `ClientException` diagnostic a disable is refused with when the directory carries no
/// RADIUS configuration to withdraw.
///
/// The mirror of [`radius_already_enabled`], worded from the request the same way: the act
/// the call could not perform, the fact that refused it and the directory the request named,
/// in the same class the enable's own refusal is answered in. [`refusal`] appends the request
/// id, once.
#[must_use]
pub fn radius_not_enabled(directory_id: &str) -> String {
    format!("Could not disable radius as it is not enabled for directory: {directory_id}")
}

/// One directory's RADIUS configuration.
///
/// The settings are held as the enable passed them — minus the shared secret, see the module
/// docs — because that is what `DirectoryDescription$RadiusSettings` reports: "A
/// RadiusSettings object that contains information about the RADIUS server configured for
/// this directory".
#[derive(Debug, Clone)]
pub struct RadiusConfiguration {
    /// The settings the enable named, as they arrived, with `SharedSecret` dropped.
    pub settings: RadiusSettings,
    /// One of the modelled `RadiusStatus` values
    /// ([`crate::ops::types::RADIUS_STATUS_VALUES`]): [`RADIUS_STATUS_CREATING`] until the
    /// transition the enable owes falls due, [`RADIUS_STATUS_COMPLETED`] from then on.
    pub status: String,
    /// When that transition falls due; `None` once there is none left to make. See
    /// [`TRANSITION_SPAN`].
    pub status_due_at: Option<SystemTime>,
}

impl RadiusConfiguration {
    /// The record an `EnableRadius` at `now` writes: the settings it named with the shared
    /// secret dropped, the transient status the enable answers in, and the instant that
    /// status settles.
    #[must_use]
    pub fn enabling(requested: &RadiusSettings, now: SystemTime) -> Self {
        Self {
            settings: RadiusSettings {
                shared_secret: None,
                ..requested.clone()
            },
            status: RADIUS_STATUS_CREATING.to_string(),
            status_due_at: Some(now + TRANSITION_SPAN),
        }
    }

    /// Materialise the transition the enable owes, when the clock has already made it due at
    /// `now`: the transient [`RADIUS_STATUS_CREATING`] settles on [`RADIUS_STATUS_COMPLETED`].
    ///
    /// Called only from
    /// [`super::ComAmazonawsDirectoryserviceDirectoryservice20150416State::advance_clock`],
    /// which is where every clock-owed write of this service is made. A status this emulator
    /// does not move out of keeps whatever it holds, so the modelled `Failed` — which nothing
    /// here reaches — is never taken for the end of this transition.
    pub fn advance_clock(&mut self, now: SystemTime) {
        let Some(due) = self.status_due_at else {
            return;
        };
        if now < due {
            return;
        }
        if self.status == RADIUS_STATUS_CREATING {
            self.status = RADIUS_STATUS_COMPLETED.to_string();
        }
        self.status_due_at = None;
    }
}

impl super::Directory {
    /// The `RadiusSettings` this directory reports: the settings an enable recorded, and
    /// `None` for a directory that holds no RADIUS configuration — the member is then absent
    /// rather than reported empty, since there is no configuration to describe.
    ///
    /// THE one place that member is derived, so every operation that reports a directory's
    /// RADIUS configuration reports the same settings.
    #[must_use]
    pub fn reported_radius_settings(&self) -> Option<RadiusSettings> {
        self.radius.as_ref().map(|radius| radius.settings.clone())
    }

    /// The `RadiusStatus` this directory reports: the status of the configuration an enable
    /// recorded, and `None` for a directory that holds none.
    #[must_use]
    pub fn reported_radius_status(&self) -> Option<String> {
        self.radius.as_ref().map(|radius| radius.status.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The settings a row below enables with: every member the shape carries, so what the
    /// record keeps and what it drops are both visible.
    fn requested() -> RadiusSettings {
        RadiusSettings {
            radius_servers: Some(vec!["radius.example.com".to_string()]),
            radius_servers_ipv6: None,
            radius_port: Some(1812),
            radius_timeout: Some(30),
            radius_retries: 3,
            shared_secret: Some("a-shared-secret".to_string()),
            authentication_protocol: Some("MS-CHAPv2".to_string()),
            display_label: Some("label".to_string()),
            use_same_username: true,
        }
    }

    /// RULE: the enable answers in the TRANSIENT status and the clock settles it one span
    /// later, and nothing settles before that instant.
    #[test]
    fn the_enable_settles_one_span_after_the_call() {
        let started = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let mut record = RadiusConfiguration::enabling(&requested(), started);
        assert_eq!(
            record.status, RADIUS_STATUS_CREATING,
            "case {:?}: expected the transient {RADIUS_STATUS_CREATING}, got {:?}",
            "fresh enable", record.status
        );
        record.advance_clock(started + TRANSITION_SPAN - Duration::from_millis(1));
        assert_eq!(
            record.status, RADIUS_STATUS_CREATING,
            "case {:?}: expected {RADIUS_STATUS_CREATING} before the transition falls due, \
             got {:?}",
            "an instant before it is due", record.status
        );
        record.advance_clock(started + TRANSITION_SPAN);
        assert_eq!(
            record.status, RADIUS_STATUS_COMPLETED,
            "case {:?}: expected {RADIUS_STATUS_COMPLETED} once it has, got {:?}",
            "the due instant", record.status
        );
        assert!(
            record.status_due_at.is_none(),
            "case {:?}: expected no transition left to make, got {:?}",
            "the due instant",
            record.status_due_at
        );
        // Settling is idempotent: a later pass of the clock finds nothing to do.
        record.advance_clock(started + TRANSITION_SPAN + Duration::from_secs(60));
        assert_eq!(
            record.status, RADIUS_STATUS_COMPLETED,
            "case {:?}: expected a settled configuration to stay {RADIUS_STATUS_COMPLETED}, \
             got {:?}",
            "a later pass", record.status
        );
    }

    /// RULE: both statuses a configuration of this service carries are values the model
    /// declares.
    #[test]
    fn the_statuses_recorded_are_modelled() {
        for status in [RADIUS_STATUS_CREATING, RADIUS_STATUS_COMPLETED] {
            assert!(
                crate::ops::types::RADIUS_STATUS_VALUES.contains(&status),
                "case {status:?}: expected a modelled RadiusStatus one of {:?}",
                crate::ops::types::RADIUS_STATUS_VALUES
            );
        }
    }

    /// RULE: the record keeps every setting the request sent and drops the shared secret, so
    /// no read can report a secret back.
    #[test]
    fn the_record_keeps_every_setting_but_the_shared_secret() {
        let asked = requested();
        let record = RadiusConfiguration::enabling(&asked, SystemTime::UNIX_EPOCH);
        let kept = &record.settings;
        assert!(
            kept.shared_secret.is_none(),
            "case {:?}: expected no shared secret in the stored settings, got {:?}",
            "a full settings block", kept.shared_secret
        );
        assert_eq!(
            (
                kept.radius_servers.clone(),
                kept.radius_servers_ipv6.clone(),
                kept.radius_port,
                kept.radius_timeout,
                kept.radius_retries,
                kept.authentication_protocol.clone(),
                kept.display_label.clone(),
                kept.use_same_username,
            ),
            (
                asked.radius_servers.clone(),
                asked.radius_servers_ipv6.clone(),
                asked.radius_port,
                asked.radius_timeout,
                asked.radius_retries,
                asked.authentication_protocol.clone(),
                asked.display_label.clone(),
                asked.use_same_username,
            ),
            "case {:?}: expected every other member kept as the request sent it, got {kept:?}",
            "a full settings block"
        );
    }
}
