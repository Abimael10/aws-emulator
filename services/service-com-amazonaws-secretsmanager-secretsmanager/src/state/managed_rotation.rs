// SPDX-License-Identifier: CC0-1.0

//! The rotation of a secret the service that OWNS it manages — MANAGED ROTATION, where
//! there is no rotation function of the caller's at all.
//!
//! A secret another Amazon Web Services service created and manages
//! ([`super::service_owned`]) holds a credential of one of that service's own resources, and
//! rotating it means re-staking that credential on the resource as well as writing the
//! secret. Only the owning service can do that, which is why such a secret uses managed
//! rotation: "For secrets that use managed rotation, omit this field"
//! (`RotateSecretRequest$RotationLambdaARN`, this service's Smithy model) — the rotation is
//! configured and performed by the managing service, not by a rotation function this service
//! invokes.
//!
//! The owning service RECORDS how to ask it for that rotation, under the ARN of the secret it
//! created ([`::service_common::managed_secret_rotations`], the one register in the shared
//! engine both sides read). This module resolves that record for a secret this service holds,
//! and only ever as the OWNER's own: a record is read only when its owning service is the one
//! the secret itself reports, so nothing else can make this service hand a secret's rotation
//! anywhere.
//!
//! Two answers come out of one record. A secret under managed rotation has rotation
//! CONFIGURED — by the service that manages it — which is what `RotationEnabled` reports
//! ("whether automatic rotation is turned on for this secret",
//! `DescribeSecretResponse$RotationEnabled`). And a `RotateSecret` call that asks for nothing
//! but the rotation of such a secret is SERVED by handing it to that service, rather than
//! refused as a write of a secret the caller does not own.

use std::sync::Arc;

use ::service_common::managed_secret_rotations::{ManagedRotation, ManagedRotations};

/// How the secret at `arn` is rotated by the service that owns it, or `None` when it is under
/// no managed rotation — which is every secret a caller created for itself.
///
/// `owning_service` is what the secret itself reports as its `OwningService`, and a record is
/// resolved only when it is that service's own: the register is written by whichever service
/// created a secret, and this reads a record only for the owner the secret names.
#[must_use]
pub fn managed_rotation<Eng: Send + Sync + 'static>(
    engine: &Eng,
    arn: &str,
    owning_service: Option<&str>,
) -> Option<Arc<dyn ManagedRotation>> {
    let owner = owning_service?;
    let rotation = ManagedRotations::of(engine)?.managed(arn)?;
    (rotation.owning_service() == owner).then_some(rotation)
}

/// Whether the secret at `arn` has rotation configured by the service that owns it.
///
/// This is what a READ of such a secret reports as `RotationEnabled`: the configuration lives
/// with the managing service, so a secret under managed rotation reports rotation as turned
/// on even though no `RotateSecret` call ever configured it here. Nothing is written: the
/// answer is read out of the register the owning service keeps.
#[must_use]
pub fn under_managed_rotation<Eng: Send + Sync + 'static>(
    engine: &Eng,
    arn: &str,
    owning_service: Option<&str>,
) -> bool {
    managed_rotation(engine, arn, owning_service).is_some()
}

/// `RotationEnabled`, `RotationRules` and `NextRotationDate` as a read of one secret reports
/// them.
///
/// Each is the secret's OWN where a `RotateSecret` call configured one, and otherwise comes
/// from the schedule the service that manages the secret publishes — which is where the
/// rotation configuration of a managed-rotation secret lives. A secret that is neither
/// configured nor managed reports none of the three, which is how "rotation was never
/// configured" is told from "rotation was configured and turned off".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RotationView {
    /// "Whether automatic rotation is turned on for this secret."
    pub enabled: Option<bool>,
    /// The rotation schedule in force for the secret.
    pub rules: Option<crate::state::StoredRotationRules>,
    /// "The last date and time that Secrets Manager rotated the secret."
    pub last_rotated: Option<std::time::SystemTime>,
    /// "The next rotation is scheduled to occur on or before this date."
    pub next_rotation: Option<std::time::SystemTime>,
}

/// What a read of `content` reports about its rotation.
///
/// Nothing is written: the managed half of the answer is read out of the register the owning
/// service keeps, and the secret's own half is what its `RotateSecret` calls stored.
#[must_use]
pub fn rotation_view<Eng: Send + Sync + 'static>(
    engine: &Eng,
    content: &crate::state::Secret,
) -> RotationView {
    let managed = managed_rotation(engine, &content.arn, content.owning_service.as_deref());
    let interval = managed
        .as_ref()
        .and_then(|rotation| rotation.rotation_interval_days());
    let managed_rules = interval.map(|days| crate::state::StoredRotationRules {
        automatically_after_days: Some(days),
        duration: None,
        schedule_expression: None,
    });
    // The service that manages a secret STAKES the credential when it creates it — that is
    // what the secret holds from the start — so for such a secret the last rotation is its
    // creation until a later rotation of it completes: "The last date and time that Secrets
    // Manager rotated the secret" (`DescribeSecretResponse$LastRotatedDate`, this service's
    // Smithy model).
    let last_rotated = content
        .last_rotated_date
        .or_else(|| managed.is_some().then_some(content.created_date));
    // "In DescribeSecret and ListSecrets, this value is calculated from the rotation schedule
    // after every successful rotation" (`RotationRulesType$AutomaticallyAfterDays`, this
    // service's Smithy model): for a managed schedule the rotation it is counted from is the
    // last one, which for a secret nothing has rotated since its creation is that creation.
    let next_managed = managed_rules.as_ref().and_then(|rules| {
        crate::state::parse_rotation_rules(&rules.to_modeled())
            .ok()
            .flatten()
            .and_then(|schedule| {
                schedule.next_after(last_rotated.unwrap_or(content.created_date))
            })
    });
    RotationView {
        enabled: content.rotation_enabled.or(managed.is_some().then_some(true)),
        rules: content.rotation_rules.clone().or(managed_rules),
        last_rotated,
        next_rotation: content.next_rotation_date.or(next_managed),
    }
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Arc;
    use std::time::{Duration, SystemTime};

    use super::*;

    /// A managing service's record for the rows below, publishing the schedule `interval`.
    #[derive(Debug)]
    struct Managing {
        service_id: &'static str,
        interval: Option<i64>,
    }

    impl ManagedRotation for Managing {
        fn owning_service(&self) -> &str {
            self.service_id
        }

        fn rotation_interval_days(&self) -> Option<i64> {
            self.interval
        }

        fn rotate<'a>(
            &'a self,
            _callback: &'a ::smithy_server_runtime::EmbeddedEmulator,
        ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>> {
            Box::pin(async { Ok(()) })
        }
    }

    /// A secret created at `created`, owned by `owner`.
    fn secret(arn: &str, owner: Option<&str>, created: SystemTime) -> crate::state::Secret {
        let mut secret = crate::state::Secret::new(
            "a-secret".to_string(),
            arn.to_string(),
            "us-east-1".to_string(),
            created,
        );
        secret.owning_service = owner.map(str::to_string);
        secret
    }

    /// RULE: a secret's reported rotation view is its own where a call configured one, and
    /// otherwise the schedule the service that manages it publishes; a secret neither
    /// configured nor managed reports no rotation view at all.
    ///
    /// The rows span what the reading generalises over: a secret under a managing service's
    /// schedule before and after a rotation of it, one owned by a service that publishes no
    /// schedule, one a caller configured itself, and one that is neither.
    #[test]
    fn a_managed_secret_reports_the_schedule_its_owner_publishes() {
        let engine = ::engine::InMemoryEngine::new();
        let rotations = ManagedRotations::of(&engine).expect("the engine vends the register");
        let managed_arn = "arn:aws:secretsmanager:us-east-1:111:secret:rds!db-one-AbCdEf";
        let unscheduled_arn = "arn:aws:secretsmanager:us-east-1:111:secret:events!two-GhIjKl";
        rotations.managing(
            managed_arn,
            Arc::new(Managing {
                service_id: "rds",
                interval: Some(7),
            }),
        );
        rotations.managing(
            unscheduled_arn,
            Arc::new(Managing {
                service_id: "events",
                interval: None,
            }),
        );
        let created = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let week = Duration::from_secs(7 * 24 * 60 * 60);

        let managed = secret(managed_arn, Some("rds"), created);
        let view = rotation_view(&engine, &managed);
        assert_eq!(
            (view.enabled, view.last_rotated, view.next_rotation),
            (Some(true), Some(created), Some(created + week)),
            "case \"a managed secret\": expected rotation on, the credential staked at its \
             creation and the next rotation a week out, got {view:?}"
        );
        assert_eq!(
            view.rules.as_ref().and_then(|rules| rules.automatically_after_days),
            Some(7),
            "case \"a managed secret\": expected the owner's schedule, got {view:?}"
        );

        // After a rotation the schedule is counted from that rotation.
        let mut rotated = managed.clone();
        let at = created + Duration::from_secs(60);
        rotated.last_rotated_date = Some(at);
        let view = rotation_view(&engine, &rotated);
        assert_eq!(
            (view.last_rotated, view.next_rotation),
            (Some(at), Some(at + week)),
            "case \"a rotated managed secret\": expected the next rotation a week past it, got \
             {view:?}"
        );

        // An owner that publishes no schedule: rotation is still that service's, so it is on,
        // but nothing states when the next one is due.
        let unscheduled = secret(unscheduled_arn, Some("events"), created);
        let view = rotation_view(&engine, &unscheduled);
        assert_eq!(
            (view.enabled, view.rules.clone(), view.next_rotation),
            (Some(true), None, None),
            "case \"an owner publishing no schedule\": expected rotation on and no schedule, \
             got {view:?}"
        );

        // A secret the register holds nothing for: its own state, and nothing when it has
        // none. A caller's secret that claims an owning service resolves no record either,
        // because the register is keyed by the ARN its owner created.
        let mut configured = secret(
            "arn:aws:secretsmanager:us-east-1:111:secret:a-callers-own-MnOpQr",
            None,
            created,
        );
        let view = rotation_view(&engine, &configured);
        assert_eq!(
            view,
            RotationView::default(),
            "case \"a never-configured secret\": expected no rotation view, got {view:?}"
        );
        configured.rotation_enabled = Some(false);
        configured.rotation_rules = Some(crate::state::StoredRotationRules {
            automatically_after_days: Some(30),
            duration: None,
            schedule_expression: None,
        });
        let view = rotation_view(&engine, &configured);
        assert_eq!(
            (
                view.enabled,
                view.rules.as_ref().and_then(|rules| rules.automatically_after_days)
            ),
            (Some(false), Some(30)),
            "case \"a secret configured by a caller\": expected its own state, got {view:?}"
        );
    }

    /// An engine this emulator did not build vends no register, so no secret is under managed
    /// rotation rather than taking the read down.
    #[test]
    fn an_engine_that_vends_no_register_manages_nothing() {
        struct Elsewhere;
        assert!(
            !under_managed_rotation(&Elsewhere, "arn:aws:secretsmanager:::secret:x", Some("rds")),
            "expected no managed rotation for an engine of another kind"
        );
    }

    /// A secret no owning service is reported for is nobody's to rotate, whatever the register
    /// holds.
    #[test]
    fn a_callers_own_secret_is_under_no_managed_rotation() {
        let engine = ::engine::InMemoryEngine::new();
        assert!(
            !under_managed_rotation(&engine, "arn:aws:secretsmanager:::secret:x", None),
            "expected no managed rotation for a secret reporting no owning service"
        );
    }
}
