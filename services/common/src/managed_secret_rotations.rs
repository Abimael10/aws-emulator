// SPDX-License-Identifier: CC0-1.0

//! The secrets whose ROTATION another Amazon Web Services service manages, and how Secrets
//! Manager asks that service to rotate one.
//!
//! A secret an AWS service creates and manages for one of its own resources — the master
//! user password an RDS DB instance has RDS generate is the shape — rotates under MANAGED
//! ROTATION: there is no rotation function of the caller's for Secrets Manager to invoke
//! ("For secrets that use managed rotation, omit this field",
//! `RotateSecretRequest$RotationLambdaARN`), because the credential is the OWNING service's
//! to re-stake. The owning service is the only side that can do it: it holds the resource
//! the credential logs in to, and only it may write the secret it owns.
//!
//! So the owning service RECORDS here, under the ARN of the secret it created, how to ask it
//! for that rotation, and Secrets Manager — which cannot see that service's store and does
//! not know its API — asks it through the record. Two things follow from one record:
//!
//! * the secret's rotation is CONFIGURED, by the service that manages it rather than by a
//!   `RotateSecret` call, which is what `RotationEnabled` reports;
//! * a caller's own `RotateSecret` on it is served by handing the rotation to that service,
//!   rather than refused as a write of a secret the caller does not own.
//!
//! A secret no record is held for is one no service rotates: it keeps the rotation state its
//! own `RotateSecret` calls configured, and a caller's write of it is refused exactly as
//! before.
//!
//! Engine-scoped, like every other store two services on either side of a hand-off share:
//! the records live in the SAME shared engine as the services' own, so the secret one service
//! created is the one the other resolves, and two engines in one process share nothing. An
//! engine this emulator did not build vends no store, and then nothing is under managed
//! rotation — the position every secret was in before a service recorded one.
//!
//! LOCK ORDER: these records are taken LAST and are never held across a call. A writer takes
//! them after its own store's guard is released (a secret is recorded once its owner has
//! created it), and a reader CLONES the record out before it asks the owning service for the
//! rotation, so no store's guard is held while that service serves the request.

use std::collections::BTreeMap;
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// How many managed-rotation records the register holds at once.
///
/// An EMULATOR ceiling, not an AWS quota: the records live in memory for the lifetime of the
/// process, so without a bound a caller creating managed resources in a loop would grow the
/// register without end. It is set far above any ordinary workflow, and past it the OLDEST
/// record is dropped — never a newer one — so the secret a running workflow just had minted
/// is always still rotatable.
pub const MAX_MANAGED_ROTATIONS: usize = 10_000;

/// One secret's rotation, as the service that MANAGES that secret performs it.
///
/// Implemented by the owning service, which is the side that knows its own API and holds the
/// resource the credential belongs to. Secrets Manager only asks.
pub trait ManagedRotation: std::fmt::Debug + Send + Sync + 'static {
    /// The id of the service that manages the secret — the value the secret reports as its
    /// `OwningService`, so a reader can check the record is the owner's own.
    fn owning_service(&self) -> &str;

    /// How often that service rotates the credential, in DAYS, as the SCHEDULE it publishes
    /// for the secrets it manages; `None` for a service that publishes none, whose secrets
    /// are then rotated only when something asks for it.
    ///
    /// It is the managing service's own schedule, which is why it is stated by the owner
    /// rather than by Secrets Manager: a secret under managed rotation has no rotation rules
    /// of a caller's for Secrets Manager to read.
    fn rotation_interval_days(&self) -> Option<i64>;

    /// Rotate the credential the secret holds, as the service that manages it: re-stake a new
    /// credential on the resource and write it to the secret.
    ///
    /// The call is made over the in-process callback, so nothing leaves this process. `Err`
    /// carries what went wrong as text, for a caller that reports a rotation it could not
    /// have performed; the credential is then unchanged.
    fn rotate<'a>(
        &'a self,
        callback: &'a ::smithy_server_runtime::EmbeddedEmulator,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>>;
}

/// The register itself: secret ARN -> the rotation its owning service performs.
///
/// Held once per engine.
#[derive(Default)]
pub struct ManagedSecretRotations {
    /// The records, by the ARN of the secret each is the rotation of.
    managed: BTreeMap<String, Arc<dyn ManagedRotation>>,
    /// Registration order, so the ceiling drops the oldest record first.
    order: VecDeque<String>,
}

/// Written by hand rather than derived: a record is a live rotation and carries no `Debug`.
impl std::fmt::Debug for ManagedSecretRotations {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ManagedSecretRotations")
            .field("managed", &self.managed.len())
            .finish()
    }
}

/// A handle on one engine's [`ManagedSecretRotations`].
///
/// Clone-cheap (an `Arc` inside) and `'static`, so a service can keep one beside the record it
/// wrote.
#[derive(Clone)]
pub struct ManagedRotations(::engine::ServiceState<ManagedSecretRotations>);

/// Written by hand rather than derived: the state handle inside carries no `Debug`, and what
/// is diagnosable about this handle is how many secrets are under managed rotation — never a
/// secret's own identity in a log line.
impl std::fmt::Debug for ManagedRotations {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ManagedRotations")
            .field("managed", &self.0.lock().managed.len())
            .finish()
    }
}

impl ManagedRotations {
    /// The records of the SAME shared engine the services' own stores live in, or `None` for
    /// an engine that vends none.
    #[must_use]
    pub fn of<Eng: Send + Sync + 'static>(engine: &Eng) -> Option<Self> {
        (engine as &dyn ::std::any::Any)
            .downcast_ref::<::engine::InMemoryEngine>()
            .map(|engine| Self(engine.state::<ManagedSecretRotations>()))
    }

    /// Record that `rotation` is how the secret at `secret_arn` is rotated: the owning service
    /// writes this once it has created that secret.
    ///
    /// Recording an ARN again REPLACES its record — a secret's owner is the one side that
    /// writes here, so this is that owner correcting what it recorded.
    pub fn managing(&self, secret_arn: &str, rotation: Arc<dyn ManagedRotation>) {
        let mut held = self.0.lock();
        if held.managed.insert(secret_arn.to_string(), rotation).is_none() {
            held.order.push_back(secret_arn.to_string());
        }
        while held.order.len() > MAX_MANAGED_ROTATIONS {
            if let Some(oldest) = held.order.pop_front() {
                held.managed.remove(&oldest);
            }
        }
    }

    /// Forget the secret at `secret_arn`: its owner has deleted it, so nothing rotates it any
    /// more and a call that asks for its rotation must be answered as one asking about a
    /// secret under no managed rotation.
    pub fn released(&self, secret_arn: &str) {
        let mut held = self.0.lock();
        held.managed.remove(secret_arn);
        held.order.retain(|held| held != secret_arn);
    }

    /// How the secret at `secret_arn` is rotated, or `None` for a secret under no managed
    /// rotation at all.
    ///
    /// The record is CLONED out, so the caller asks the owning service for the rotation with
    /// no guard of this register held.
    #[must_use]
    pub fn managed(&self, secret_arn: &str) -> Option<Arc<dyn ManagedRotation>> {
        self.0.lock().managed.get(secret_arn).cloned()
    }

    /// Whether the secret at `secret_arn` is under the managed rotation of the service whose
    /// id is `owning_service` — the question a reader holding the secret's own
    /// `OwningService` asks, so a record is only ever read as the owner's own.
    #[must_use]
    pub fn managed_by(&self, secret_arn: &str, owning_service: &str) -> bool {
        self.managed(secret_arn)
            .is_some_and(|rotation| rotation.owning_service() == owning_service)
    }

    /// How many secrets are under managed rotation, for a caller that reports on the register
    /// itself.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.lock().managed.len()
    }

    /// Whether no secret is under managed rotation at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// A rotation for the rows below: it counts the times it was asked to rotate, which is how
    /// "the owner performed it" is told from "the register answered".
    #[derive(Debug)]
    struct Counting {
        service_id: &'static str,
        asked: AtomicUsize,
    }

    impl ManagedRotation for Counting {
        fn owning_service(&self) -> &str {
            self.service_id
        }

        fn rotation_interval_days(&self) -> Option<i64> {
            Some(7)
        }

        fn rotate<'a>(
            &'a self,
            _callback: &'a ::smithy_server_runtime::EmbeddedEmulator,
        ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>> {
            self.asked.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(()) })
        }
    }

    fn counting(service_id: &'static str) -> Arc<Counting> {
        Arc::new(Counting {
            service_id,
            asked: AtomicUsize::new(0),
        })
    }

    /// RULE: a secret its owner recorded resolves to that owner's rotation, and only under the
    /// owner's own id; a secret nobody recorded, and one whose record has been withdrawn,
    /// resolve to nothing.
    ///
    /// The rows span what the reading generalises over: a recorded secret asked for under its
    /// owner's id and under another service's, a secret of a second owning service on the same
    /// engine, and an ARN no record was ever written for.
    #[test]
    fn a_recorded_secret_resolves_to_its_owners_rotation() {
        let engine = ::engine::InMemoryEngine::new();
        let rotations = ManagedRotations::of(&engine).expect("the engine vends the records");
        let rds = counting("rds");
        let redshift = counting("redshift");
        rotations.managing("arn:aws:secretsmanager:us-east-1:111:secret:rds!db-one-AbCdEf", rds);
        rotations.managing(
            "arn:aws:secretsmanager:us-east-1:111:secret:redshift!two-GhIjKl",
            redshift,
        );
        let rows: &[(&str, &str, &str, bool)] = &[
            (
                "the owner's own id",
                "arn:aws:secretsmanager:us-east-1:111:secret:rds!db-one-AbCdEf",
                "rds",
                true,
            ),
            (
                "another service's id",
                "arn:aws:secretsmanager:us-east-1:111:secret:rds!db-one-AbCdEf",
                "redshift",
                false,
            ),
            (
                "a second owning service's secret",
                "arn:aws:secretsmanager:us-east-1:111:secret:redshift!two-GhIjKl",
                "redshift",
                true,
            ),
            (
                "an ARN no record was written for",
                "arn:aws:secretsmanager:us-east-1:111:secret:a-callers-own-MnOpQr",
                "rds",
                false,
            ),
        ];
        for (case, arn, owner, expected) in rows {
            let managed = rotations.managed_by(arn, owner);
            assert_eq!(
                managed, *expected,
                "case {case:?}: expected managed_by({arn:?}, {owner:?}) == {expected}, got \
                 {managed}"
            );
        }

        // A handle built again over the same engine reads the same records, which is what lets
        // the owning service and Secrets Manager be in two crates.
        let read_again = ManagedRotations::of(&engine).expect("the engine vends the records");
        assert_eq!(
            read_again.len(),
            2,
            "case \"a second handle on the same engine\": expected both records, got \
             {read_again:?}"
        );

        // Withdrawn with the secret: nothing rotates it again, and the other owner's record
        // stays.
        rotations.released("arn:aws:secretsmanager:us-east-1:111:secret:rds!db-one-AbCdEf");
        assert!(
            rotations
                .managed("arn:aws:secretsmanager:us-east-1:111:secret:rds!db-one-AbCdEf")
                .is_none(),
            "case \"a secret whose record is withdrawn\": expected nothing, got {rotations:?}"
        );
        assert_eq!(
            rotations.len(),
            1,
            "case \"another owner's record\": expected it to stay, got {rotations:?}"
        );
    }

    /// An engine this emulator did not build vends no records, so no secret is under managed
    /// rotation rather than taking the caller down.
    #[test]
    fn an_engine_that_vends_no_records_manages_nothing() {
        struct Elsewhere;
        assert!(
            ManagedRotations::of(&Elsewhere).is_none(),
            "expected no records for an engine of another kind"
        );
    }
}
