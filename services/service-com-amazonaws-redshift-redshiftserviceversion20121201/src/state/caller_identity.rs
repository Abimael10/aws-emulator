// SPDX-License-Identifier: CC0-1.0

//! WHO a request speaks for: the IAM identity the credential it was signed with
//! belongs to.
//!
//! The ONE place this crate answers that question, so every operation that reports
//! a caller reports the same identity — and the same identity
//! `sts:GetCallerIdentity` answers for that very credential, because the answer is
//! read through the STS service's OWN accessors rather than derived here: the
//! session the request's access key id names, or — for a key that names no
//! session — the IAM user that holds the key. A client that reads its own identity
//! from STS and then reads what Amazon Redshift mapped it to therefore sees one
//! principal.
//!
//! An access key id is a CREDENTIAL identifier, never an identity: two credentials
//! of one identity resolve to that one identity, and a credential is not the name
//! of anything a client can grant to. That is why nothing here derives a value
//! from the key id itself.
//!
//! The IAM store is locked and RELEASED before the session store is, so no two
//! services' stores are ever held at once; call this BEFORE locking this service's
//! own store.

use super::*;

use ::service_com_amazonaws_sts_awssecuritytokenservicev20110615::state as sts_state;

/// The ARN of the IAM identity the credential this request was signed with speaks
/// for — the value `sts:GetCallerIdentity` answers for that same credential.
///
/// Falls back to the identity STS answers for a key nothing holds (its ambient
/// identity) when no session resolves: a credential past its promised lifetime, or
/// an engine carrying no STS store at all.
pub fn caller_identity_arn<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &smithy_server_runtime::OperationContext<'_>,
) -> String {
    let access_key_id = ::service_common::request_access_key_id(ctx.parts());
    let long_term = {
        let iam = sts_state::iam_state(engine);
        match iam.as_ref() {
            Some(iam) => sts_state::long_term_identity(Some(&iam.lock()), &access_key_id),
            None => sts_state::long_term_identity(None, &access_key_id),
        }
    };
    match sts_state::service_state(engine) {
        Some(sts) => {
            let resolved = sts_state::resolve_request_caller(
                &mut sts.lock(),
                &access_key_id,
                SystemTime::now(),
                ctx.credential_store(),
                long_term,
            );
            match resolved {
                Ok(caller) => caller.identity.arn,
                // A credential past its promised lifetime speaks for no session,
                // which is the position a key nothing holds is in.
                Err(_expired) => sts_state::ambient_identity().arn,
            }
        }
        None => long_term.arn,
    }
}
