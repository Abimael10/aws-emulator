// SPDX-License-Identifier: CC0-1.0

//! The calls one emulated service makes to another ON THE CALLER'S BEHALF — in flight, for
//! the service that has to authorize them.
//!
//! Some of the requests a service serves cannot be served without a call to a SECOND
//! service: Secrets Manager asking KMS to wrap a data key so a secret value can be sealed is
//! the shape. That second call is made for the caller, not by the caller, and the service
//! serving it has to be able to tell the two apart — because a resource policy may be
//! written to admit the call that arrives THROUGH a service while refusing the identical one
//! a principal makes itself. KMS's `kms:ViaService` is exactly such a discriminator: the
//! value it carries is the regional endpoint name of the service the request arrived
//! through, and a request a caller made directly carries no value for it at all.
//!
//! WHICH credential such a call presents is what says so, the same way it says so for a call
//! a service makes as its own service principal ([`crate::service_principal_calls`]): the
//! calling service mints a credential in the emulator's own credential store
//! (`OperationContext::credential_store`) for the ONE call, signs that call with it, and
//! records HERE, under that credential's access key id, which service the call is arriving
//! through AND which credential the ORIGINATING request was signed with. The service SERVING
//! the call reads the record for the access key id the call's SIGNATURE was verified against —
//! the one thing about a request a client cannot choose for itself — so no client can present
//! a value for that condition key by sending a header, and the call is decided for the
//! principal that asked for it rather than for the service that relayed it. Held once in the
//! shared engine, like every other fact one service writes and another reads.
//!
//! Two things keep a record to the one call it was written for: its key is a credential the
//! EMULATOR minted, so presenting it takes the secret the emulator minted with it, and it
//! lives only while the call is in flight — the caller forgets it, and deletes the
//! credential, as soon as the call returns ([`InFlightSession`]), and the clock drops a
//! record whose deadline has passed. A call whose credential this directory holds no record
//! for is therefore decided as a DIRECT call, which is the narrower answer.
//!
//! Bounded like every other shared store: [`MAX_IN_FLIGHT_SESSIONS`] records at most, and a
//! record whose deadline the clock has passed is dropped. The cap REFUSES a new record
//! rather than evicting a live one, so a caller that never forgets its own records cannot
//! make this grow without end and cannot take the identity off a call already in flight
//! either.

use std::collections::HashMap;
use std::time::{Duration, SystemTime};

/// How many in-flight records the directory holds at once.
///
/// An EMULATOR ceiling, not an AWS quota: a record is written per in-flight cross-service
/// call, so the directory needs a declared cap.
pub const MAX_IN_FLIGHT_SESSIONS: usize = 1_024;

/// How long a record of one in-flight call stays readable.
///
/// What ENDS a record is [`InFlightSession`] being dropped, which happens on every path out
/// of the call it was opened for; this deadline is the floor that bounds a record nothing
/// dropped. It sits well past one in-process call, so a record can never expire while the
/// serving handler is still deciding the call.
const RECORD_SLACK: Duration = Duration::from_secs(60);

/// One call an emulated service is making to another on the behalf of the caller it is
/// serving.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForwardAccessSession {
    /// The regional endpoint name of the service the call is arriving THROUGH
    /// (`secretsmanager.us-east-1.amazonaws.com`), which is the value a resource policy
    /// scoped to one service's path to the resource is written against.
    pub via_service: String,
    /// The access key id the ORIGINATING request — the caller's own call, the one this call is
    /// being made for — was signed with, so the serving service decides this call for THAT
    /// caller's principal. A forward access session changes how a request arrived and nothing
    /// about who it speaks for.
    pub caller_access_key_id: String,
    /// The instant past which the record decides nothing: the calling service's own deadline
    /// for the call, so a record outlives neither the call nor the clock.
    pub expires_at: SystemTime,
}

/// The calls in flight right now, keyed by the access key id of the credential each is
/// signed with.
#[derive(Debug, Default)]
pub struct ForwardAccessSessions {
    /// At most [`MAX_IN_FLIGHT_SESSIONS`] records.
    sessions: HashMap<String, ForwardAccessSession>,
}

impl ForwardAccessSessions {
    /// Remember the call signed with `access_key_id`, for as long as it is in flight.
    ///
    /// Prunes first: a record whose deadline the clock has passed can decide nothing, so it
    /// is dropped — owed at this instant whether or not anybody is calling. `false` when the
    /// directory is at [`MAX_IN_FLIGHT_SESSIONS`] live records and nothing was written: the
    /// cap never takes the identity off a call already in flight, so the new call is left to
    /// be decided as a direct one.
    pub fn remember(
        &mut self,
        access_key_id: impl Into<String>,
        session: ForwardAccessSession,
        now: SystemTime,
    ) -> bool {
        self.sessions.retain(|_, held| now < held.expires_at);
        let key = access_key_id.into();
        if self.sessions.len() >= MAX_IN_FLIGHT_SESSIONS && !self.sessions.contains_key(&key) {
            return false;
        }
        self.sessions.insert(key, session);
        true
    }

    /// The call `access_key_id` signed, or `None` when this directory holds none that the
    /// clock has not already passed.
    #[must_use]
    pub fn session(&self, access_key_id: &str, now: SystemTime) -> Option<&ForwardAccessSession> {
        self.sessions
            .get(access_key_id)
            .filter(|session| now < session.expires_at)
    }

    /// Forget the call signed with `access_key_id` — called by the service that remembered
    /// it as soon as that call returns, so a record never outlives the call it describes.
    pub fn forget(&mut self, access_key_id: &str) {
        self.sessions.remove(access_key_id);
    }

    /// How many records the directory holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    /// Whether the directory holds no record at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }
}

/// The shared directory of in-flight calls, in the one shared engine: written by the service
/// making such a call and read by the service serving it.
///
/// `None` for an engine carrying no such directory, which is evidence of nothing: a call is
/// then made, and decided, as a direct one.
#[must_use]
pub fn in_flight_sessions<Eng: Send + Sync + 'static>(
    engine: &Eng,
) -> Option<::engine::ServiceState<ForwardAccessSessions>> {
    (engine as &dyn ::std::any::Any)
        .downcast_ref::<::engine::InMemoryEngine>()
        .map(::engine::InMemoryEngine::state::<ForwardAccessSessions>)
}

/// The forward access session the request signed with `access_key_id` is arriving in, or
/// `None` for a request a caller made directly — which is every request whose access key id
/// this directory holds no live record of.
///
/// This is the whole read side: a service deciding a request asks it for the one record, reads
/// off it the service the call came through and resolves the principal from the credential the
/// ORIGINATING request presented; a request this answers `None` for is a direct call.
///
/// LOCKING: it takes the directory's lock and no other, so a handler may ask it while holding
/// its own store's guard.
#[must_use]
pub fn session_of<Eng: Send + Sync + 'static>(
    engine: &Eng,
    access_key_id: &str,
    now: SystemTime,
) -> Option<ForwardAccessSession> {
    let directory = in_flight_sessions(engine)?;
    let held = directory.lock();
    held.session(access_key_id, now).cloned()
}

/// One in-flight call's own credential and record, held for as long as that call is being
/// made.
///
/// Dropping it forgets the record and DELETES the credential, so the identity the call was
/// made under exists only while the serving service is answering it: a credential nothing
/// holds can sign nothing, and a record nothing holds decides nothing.
pub struct InFlightSession<'ctx> {
    directory: ::engine::ServiceState<ForwardAccessSessions>,
    credentials: &'ctx ::emulator_server::CredentialStore,
    /// The access key id the call presents, which the directory holds its record under.
    access_key_id: String,
    /// The secret that key signs with.
    secret_access_key: String,
}

impl InFlightSession<'_> {
    /// The access key id the call is signed with — the key the record is held under.
    #[must_use]
    pub fn access_key_id(&self) -> &str {
        &self.access_key_id
    }

    /// The secret that key signs with. It travels in the call's `Authorization` and nowhere
    /// else — never in a response, a log or a file.
    #[must_use]
    pub fn secret_access_key(&self) -> &str {
        &self.secret_access_key
    }
}

impl ::std::fmt::Debug for InFlightSession<'_> {
    /// Names the credential the call presents and never the secret it signs with.
    fn fmt(&self, formatter: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        formatter
            .debug_struct("InFlightSession")
            .field("access_key_id", &self.access_key_id)
            .finish_non_exhaustive()
    }
}

impl Drop for InFlightSession<'_> {
    fn drop(&mut self) {
        self.directory.lock().forget(&self.access_key_id);
        let _ = self.credentials.delete(&self.access_key_id);
    }
}

/// Mint the credential ONE call a service makes on its caller's behalf is signed with, and
/// record, under its access key id, that the call is arriving through `via_service` for the
/// caller whose own request `ctx` is serving.
///
/// `None` where the call cannot be marked at all — an engine carrying no directory, a store
/// that minted no secret, a directory already holding as many in-flight calls as it records —
/// in which case the caller makes its ordinary in-process call instead and the serving
/// service decides it as a direct one.
#[must_use]
pub fn open<'ctx, Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &'ctx ::smithy_server_runtime::OperationContext<'_>,
    via_service: &str,
) -> Option<InFlightSession<'ctx>> {
    let directory = in_flight_sessions(engine)?;
    let minted = ctx
        .credential_store()
        .create(&::emulator_server::aws_credentials::AWS_ACCESS_KEY);
    let secret_access_key = minted
        .secret(::emulator_server::aws_credentials::SECRET_ACCESS_KEY)
        .map(str::to_string);
    let now = SystemTime::now();
    let recorded = secret_access_key.is_some()
        && directory.lock().remember(
            minted.id.clone(),
            ForwardAccessSession {
                via_service: via_service.to_string(),
                caller_access_key_id: ::engine::extract_akid(&ctx.parts().headers),
                expires_at: now + RECORD_SLACK,
            },
            now,
        );
    match secret_access_key.filter(|_| recorded) {
        Some(secret_access_key) => Some(InFlightSession {
            directory,
            credentials: ctx.credential_store(),
            access_key_id: minted.id.clone(),
            secret_access_key,
        }),
        None => {
            let _ = ctx.credential_store().delete(&minted.id);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A record answers for the one credential it was written for and only while that call is
    /// in flight: the rows differ along the access key id asked about, whether the record was
    /// forgotten, and whether the clock has passed its deadline.
    #[test]
    fn a_record_answers_for_its_own_credential_and_window() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        let mut directory = ForwardAccessSessions::default();
        let one = ("secretsmanager.us-east-1.amazonaws.com", "AKIDTHEFIRSTCALLER");
        let two = ("s3.us-west-2.amazonaws.com", "AKIDTHESECONDCALLER");
        for (key, (via_service, caller)) in [("EMULATORAKONE", one), ("EMULATORAKTWO", two)] {
            assert!(
                directory.remember(
                    key,
                    ForwardAccessSession {
                        via_service: via_service.to_string(),
                        caller_access_key_id: caller.to_string(),
                        expires_at: now + Duration::from_secs(30),
                    },
                    now,
                ),
                "case {key:?}: the directory must accept the record"
            );
        }

        for (case, key, expected) in [
            ("the first call's credential", "EMULATORAKONE", Some(one)),
            ("the second call's credential", "EMULATORAKTWO", Some(two)),
            ("a credential nothing remembered", "EMULATORAKNONE", None),
        ] {
            let found = directory.session(key, now).map(|session| {
                (
                    session.via_service.as_str(),
                    session.caller_access_key_id.as_str(),
                )
            });
            assert_eq!(
                found, expected,
                "case {case:?}: expected {expected:?}, got {found:?}"
            );
        }

        directory.forget("EMULATORAKONE");
        assert!(
            directory.session("EMULATORAKONE", now).is_none(),
            "case a forgotten call: expected no record, got {:?}",
            directory.session("EMULATORAKONE", now)
        );
        let past = now + Duration::from_secs(31);
        assert!(
            directory.session("EMULATORAKTWO", past).is_none(),
            "case a call the clock has passed: expected no record, got {:?}",
            directory.session("EMULATORAKTWO", past)
        );
    }

    /// The directory never grows past its cap, and the cap refuses the NEW record rather than
    /// evicting a live one — so a call already in flight keeps the value it arrived with.
    #[test]
    fn the_cap_refuses_a_new_record_and_keeps_every_live_one() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        let mut directory = ForwardAccessSessions::default();
        let first = "secretsmanager.eu-west-1.amazonaws.com";
        directory.remember(
            "EMULATORAKFIRST",
            ForwardAccessSession {
                via_service: first.to_string(),
                caller_access_key_id: "AKIDTHEFIRSTCALLER".to_string(),
                expires_at: now + Duration::from_secs(300),
            },
            now,
        );
        for index in 0..MAX_IN_FLIGHT_SESSIONS + 64 {
            directory.remember(
                format!("EMULATORAK{index}"),
                ForwardAccessSession {
                    via_service: format!("sqs.us-east-{index}.amazonaws.com"),
                    caller_access_key_id: format!("AKIDCALLER{index}"),
                    expires_at: now + Duration::from_secs(300),
                },
                now,
            );
            assert!(
                directory.len() <= MAX_IN_FLIGHT_SESSIONS,
                "case record {index}: expected at most {MAX_IN_FLIGHT_SESSIONS} records, got {}",
                directory.len()
            );
        }
        let kept = directory
            .session("EMULATORAKFIRST", now)
            .map(|session| session.via_service.as_str());
        assert_eq!(
            kept,
            Some(first),
            "case the record held before the cap was reached: expected {first:?}, got {kept:?}"
        );
    }
}
