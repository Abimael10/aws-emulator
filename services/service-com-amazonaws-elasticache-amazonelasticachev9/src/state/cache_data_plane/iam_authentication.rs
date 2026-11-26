// SPDX-License-Identifier: CC0-1.0

//! Whether the IAM AUTHENTICATION TOKEN a client presents on a cache's socket is a credential
//! for the cache user it names.
//!
//! An IAM-authenticated user is reached with a token instead of a password: a SigV4 PRESIGNED
//! request to `connect` as that user, which the client signs with its own AWS credentials. So
//! what makes the token a credential is the SIGNATURE on it, and a signature is only a
//! credential against the secret of the key that made it.
//!
//! Nothing on this socket can check a signature: the core hands a protocol handler an accepted
//! byte stream, the secret half of a credential belongs to the authentication layer that minted
//! it, and this emulator's own verification is its front door's. What this module does instead
//! is hold the token to everything about it that CAN be established here, and a token that
//! fails any of it authenticates nobody:
//!
//! * it presigns the request that CONNECTS — `Action=connect` — as THIS user (`User`), and its
//!   credential scope names THIS service, so a presigned request for anything else, for another
//!   user or for another service is not a credential here;
//! * it carries what a SigV4 presigned request carries: the algorithm, a credential scope and a
//!   signature; and
//! * the credential it names is one this emulator ISSUED — the key pair the process publishes at
//!   start, an access key `iam:CreateAccessKey` minted for a user of the account and the account
//!   has not deactivated, or a temporary credential a session mint recorded there which neither
//!   the clock nor a withdrawal has taken.
//!
//! That last one is what makes the token a credential rather than a sentence anyone can write:
//! a cache user's NAME is not secret and nothing else in the token is this emulator's to
//! recognise, so without it mere possession of the name would open a session. What this module
//! never does is invent a verdict on the signature itself — the identity behind the signing key
//! is established by the key being one of the account's, and no further.
//!
//! The credentials of the account are the IAM store's own records, read in the SAME shared
//! engine ([`::service_com_amazonaws_sts_awssecuritytokenservicev20110615::state::iam_state`],
//! the accessor the token service resolves a key id through), and the published pair is asked of
//! the authentication layer that owns it. An engine carrying neither — no IAM store and no
//! published pair — holds no credential for anybody, so an IAM-authenticated user authenticates
//! nobody there: a credential that cannot be shown to be the account's is not one that opens a
//! session, which is how this crate reads every other fact it does not own
//! ([`crate::state::ec2_security_groups`]).
//!
//! LOCK ORDER: the IAM store's guard is taken and RELEASED inside the one call here, with no
//! guard of this service's store held across it.

use ::std::time::SystemTime;

use ::service_com_amazonaws_sts_awssecuritytokenservicev20110615::state as sts_store;

/// The `Action` an IAM authentication token presigns: the one that opens a session on the
/// cache.
const CONNECT_ACTION: &str = "connect";

/// The query fields such a token carries: the action and the user it is made out for, and the
/// SigV4 fields of the signature over them.
const ACTION_FIELD: &str = "Action";
/// See [`ACTION_FIELD`].
const USER_FIELD: &str = "User";
/// See [`ACTION_FIELD`].
const ALGORITHM_FIELD: &str = "X-Amz-Algorithm";
/// See [`ACTION_FIELD`].
const SIGNATURE_FIELD: &str = "X-Amz-Signature";

/// The algorithm a SigV4 presigned request states (`X-Amz-Algorithm`).
const SIGV4_ALGORITHM: &str = "AWS4-HMAC-SHA256";

/// The credentials of the account, as the token service's own view of them: the IAM store in the
/// same shared engine, which holds the access keys `iam:CreateAccessKey` minted and the
/// temporary credentials every session mint records.
pub type AccountCredentials = sts_store::IamState;

/// A handle on those records, or `None` for an engine that vends none.
///
/// Clone-cheap and `'static`, so the RESP server of a cache can keep one beside the socket it
/// serves, which outlives the request that created the cache.
#[must_use]
pub fn account_credentials<Eng: Send + Sync + 'static>(
    engine: &Eng,
) -> Option<::engine::ServiceState<AccountCredentials>> {
    sts_store::iam_state(engine)
}

/// Whether `token` is an IAM authentication token that authenticates `user` at the instant
/// `now` — the whole reading this module's own documentation states.
///
/// `credentials` is the account's own credential records, and `None` an engine that holds none.
#[must_use]
pub fn token_authenticates(
    token: &str,
    user: &crate::state::UserRecord,
    credentials: Option<&::engine::ServiceState<AccountCredentials>>,
    now: SystemTime,
) -> bool {
    // The token is a request's URL, so its fields are its query's: read through the tree's one
    // reader of a raw SigV4 query string, which compares a field name as written.
    let query = token.split_once('?').map_or(token, |(_, query)| query);
    let field = |name: &str| ::service_common::query_parameter(query, name);
    let connects = field(ACTION_FIELD).is_some_and(|action| action.eq_ignore_ascii_case(CONNECT_ACTION));
    // ElastiCache requires an IAM-authenticated user's id and name to be identical, so either
    // is the name the token may carry.
    let names_the_user = field(USER_FIELD).is_some_and(|named| {
        named.eq_ignore_ascii_case(&user.user_name) || named.eq_ignore_ascii_case(&user.user_id)
    });
    let signed = field(ALGORITHM_FIELD).is_some_and(|algorithm| algorithm == SIGV4_ALGORITHM)
        && field(SIGNATURE_FIELD).is_some_and(|signature| !signature.is_empty());
    if !(connects && names_the_user && signed) {
        return false;
    }
    // WHICH SERVICE the credential was signed for, and WHICH credential made it: both are its
    // scope's, read through the tree's one reading of a presigned credential rather than by a
    // second parse of that grammar here.
    let addressed: ::http::Uri = match format!("/?{query}").parse() {
        Ok(uri) => uri,
        Err(_) => return false,
    };
    if ::service_common::presigned_url_credential::signed_for(&addressed).as_deref()
        != Some(super::SIGNING_NAME)
    {
        return false;
    }
    let presented = ::service_common::request_access_key_id(&request_of(&addressed));
    issued_credential(presented.as_str(), credentials, now)
}

/// The token's URL as the request it presigns, so the one reader of the credential a request
/// presents ([`::service_common::request_access_key_id`]) reads this one too.
///
/// A token carries its credential in the query and no header at all, which is the presigned
/// scheme that reader's second tier is for.
fn request_of(addressed: &::http::Uri) -> ::http::request::Parts {
    let mut request = ::http::Request::new(());
    *request.uri_mut() = addressed.clone();
    request.into_parts().0
}

/// Whether `access_key_id` names a credential THIS EMULATOR issued.
///
/// The published pair is asked of the authentication layer that OWNS it rather than of a copy,
/// so the answer cannot drift from the credential the front door verifies requests against; a
/// process that published none (authentication off) has no published credential for anything to
/// be signed with. The rest are the account's own records: an access key of one of its users,
/// usable only while its `AccessKey$Status` is active ("Inactive means that the key cannot be
/// used"), and a temporary credential of a session, which speaks for nobody once the clock has
/// passed its expiry or a withdrawal has taken it.
fn issued_credential(
    access_key_id: &str,
    credentials: Option<&::engine::ServiceState<AccountCredentials>>,
    now: SystemTime,
) -> bool {
    if ::emulator_server::SigV4Authenticator::minted_credentials()
        .is_some_and(|published| published.access_key_id == access_key_id)
    {
        return true;
    }
    let Some(credentials) = credentials else {
        return false;
    };
    let stamp = ::aws_smithy_types::DateTime::from(now);
    let store = credentials.lock();
    if store.credential_is_withdrawn(access_key_id, stamp) {
        return false;
    }
    let holds_access_key = store.access_keys.contains_key(access_key_id)
        && store.access_key_is_usable(access_key_id);
    holds_access_key || store.temporary_credential(access_key_id, stamp).is_some()
}
