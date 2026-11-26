// SPDX-License-Identifier: CC0-1.0

//! The secure-remote-password sign-in of a USER: the exchange that proves the
//! caller holds the user's password without that password being sent, and what
//! the sign-in still owes once it is proved.
//!
//! `AuthFlowType`: "`USER_SRP_AUTH` — Username-password authentication with the
//! Secure Remote Password (SRP) protocol" (`InitiateAuth.AuthFlow` in this
//! service's model), whose `AuthParameters` are `USERNAME` and `SRP_A`. The
//! sign-in answers that flow with the `PASSWORD_VERIFIER` challenge — "respond
//! with `PASSWORD_CLAIM_SIGNATURE`, `PASSWORD_CLAIM_SECRET_BLOCK`, and
//! `TIMESTAMP`" (`ChallengeResponses` in this service's model) — and the claim
//! that response carries is what the stored password checks.
//!
//! The continuation of that challenge is the `SECRET_BLOCK` it serves rather
//! than a session token: the response echoes that block, so the exchange a claim
//! is checked against is the one the block names, and a client holding no
//! session answers the challenge all the same.
//!
//! One concern of [`super`]'s store, in its own file: the records and helpers
//! here are reached as `crate::state::…` because the parent re-exports them, so
//! both sign-in operations and both challenge-response operations ask the SAME
//! rule.
//!
//! The exchange is the one both legs of this protocol run, read from the one
//! place that defines it ([`super::srp`]): what is USER-specific is that the
//! verifier is derived from the password this service stored for the user, under
//! `<PoolName><Username>`, and that those two names are what the claim is signed
//! over.

use super::srp::{self, SrpServerHalf, digest, generator, group_prime, padded, random_bytes};
use super::*;
use ::num_bigint::BigUint;

/// The most outstanding user SRP continuations this emulator holds; the ones
/// whose window has closed are swept first and the oldest are evicted past it,
/// so repeating the sign-in cannot grow the store without limit. The same bound
/// the challenge SESSIONS of this service are held under
/// ([`super::MAX_AUTH_SESSIONS`]), since one sign-in starts one of either.
const MAX_USER_SRP_EXCHANGES: usize = super::MAX_AUTH_SESSIONS;

/// The `ChallengeNameType` member that asks the caller for the claim the stored
/// password checks: "`PASSWORD_VERIFIER` — Respond with
/// `PASSWORD_CLAIM_SIGNATURE`, `PASSWORD_CLAIM_SECRET_BLOCK`, and `TIMESTAMP`"
/// (`ChallengeResponses` in this service's model).
pub const PASSWORD_VERIFIER_CHALLENGE: &str = "PASSWORD_VERIFIER";

/// The `ChallengeParameters` entry the continuation of that challenge travels
/// in, read from the one place this service names it ([`super::srp`]): what the
/// challenge serves there is what the response echoes, so it is also what
/// identifies the sign-in attempt a response continues.
pub const SECRET_BLOCK_PARAMETER: &str = super::srp::SECRET_BLOCK_PARAMETER;

/// The `AuthFlowType` member this exchange serves.
pub const USER_SRP_AUTH_FLOW: &str = "USER_SRP_AUTH";

/// The `ExplicitAuthFlows` value that enables [`USER_SRP_AUTH_FLOW`]:
/// "`ALLOW_USER_SRP_AUTH`: Enable SRP-based authentication"
/// (`CreateUserPoolClient.ExplicitAuthFlows` in this service's model), which is
/// also one of the three a client that sets no `ExplicitAuthFlows` gets
/// ([`super::DEFAULT_EXPLICIT_AUTH_FLOWS`]).
pub const USER_SRP_AUTH_FLOWS: [&str; 1] = ["ALLOW_USER_SRP_AUTH"];

/// The `AuthParameters` entry the caller's public value arrives in: "`SRP_A`"
/// is the "initial SRP secret" the SRP flows present (`AuthParameters` and
/// `ChallengeResponses` in this service's model), read from the one place this
/// service names it ([`super::SRP_A_RESPONSE`]).
pub const SRP_A_PARAMETER: &str = super::SRP_A_RESPONSE;

/// The `ChallengeNameType` member a user that still has to choose a password is
/// answered with, as both sign-in operations name it (`ChallengeNameType` in
/// this service's model).
pub const NEW_PASSWORD_REQUIRED_CHALLENGE: &str = "NEW_PASSWORD_REQUIRED";

/// The message a sign-in refuses an `SRP_A` that is no public value of this
/// group with.
///
/// A public value of the group is a number modulo its prime, so a value that is
/// not hexadecimal at all, one spelled in more digits than that prime, or one
/// that is zero modulo it — the case RFC 5054 §2.5.4 aborts the exchange on —
/// is none of it. The sentence names the parameter that is wrong and no value
/// from the request. The wording is this emulator's: no recording of this
/// refusal is available, so a later one can correct it.
pub const INVALID_SRP_A_MESSAGE: &str = "Invalid SRP_A value.";

/// How many bytes of salt one exchange derives its verifier with: 16, the width
/// RFC 5054 §2.1 states for the salt of an SRP verifier ("s shall be 16 bytes").
const SALT_BYTES: usize = 16;

/// The server's half of one user SRP exchange, which is the whole of the
/// outstanding `PASSWORD_VERIFIER` challenge it was served with: the sign-in it
/// belongs to, the user it is for and the window it is answerable in.
///
/// The continuation travels in the `SECRET_BLOCK` the challenge serves rather
/// than in a session token, so an exchange is held against that block — the
/// response echoes it in `PASSWORD_CLAIM_SECRET_BLOCK` and signs its claim over
/// it, which is what names the exchange the claim is checked against.
#[derive(Clone)]
pub struct UserSrpExchange {
    /// The pool the sign-in this challenge belongs to runs against.
    pub user_pool_id: String,
    /// The app client that sign-in was addressed to: the continuation belongs to
    /// that app client's sign-in, so no other app client's response answers it.
    pub client_id: String,
    /// The user as STORED, which is the name the challenge served in
    /// `USER_ID_FOR_SRP` and the one the key schedule hashes.
    pub username: String,
    /// When the challenge stops being answerable: the window the app client's
    /// own `AuthSessionValidity` gives a challenge of this sign-in.
    pub expires_at: SystemTime,
    /// The salt the verifier of THIS exchange was derived with, exactly as the
    /// challenge served it.
    salt: Vec<u8>,
    /// The exchange itself, as [`super::srp`] serves and checks one.
    half: SrpServerHalf,
}

impl UserSrpExchange {
    /// The `SECRET_BLOCK` the challenge served this exchange's continuation in,
    /// which is the key a response resolves it by.
    #[must_use]
    pub fn secret_block(&self) -> String {
        self.half.served_secret_block()
    }
}

/// Stated rather than derived: the private exponent and the secret block of one
/// exchange are secrets of the sign-in it belongs to, so nothing renders them.
impl ::std::fmt::Debug for UserSrpExchange {
    fn fmt(&self, formatter: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        formatter
            .debug_struct("UserSrpExchange")
            .field("username", &self.username)
            .finish_non_exhaustive()
    }
}

/// What a sign-in that named the SRP flow serves: the `ChallengeParameters` of
/// the `PASSWORD_VERIFIER` challenge, and the exchange state holding the
/// continuation those parameters carry.
#[derive(Debug, Clone)]
pub struct ServedUserSrpChallenge {
    pub challenge_parameters: HashMap<String, String>,
    pub exchange: UserSrpExchange,
}

/// What a response that ANSWERED the `PASSWORD_VERIFIER` challenge proved: the
/// sign-in the continuation belonged to, and the block that names the
/// continuation it has now spent.
#[derive(Debug, Clone)]
pub struct AnsweredUserSrp {
    pub user_pool_id: String,
    pub username: String,
    pub secret_block: String,
}

/// Why a response to the `PASSWORD_VERIFIER` challenge does not answer it. The
/// operation that carried the response maps each one to a modelled error IT
/// declares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserSrpRefusal {
    /// A `ChallengeResponses` entry the challenge requires and the response did
    /// not carry, named by that entry.
    MissingResponse(&'static str),
    /// The claim does not prove the caller holds the password stored for the
    /// user the challenge was served for: the signature does not verify, the
    /// block it echoes names no exchange this service is holding, that exchange
    /// belongs to another app client or another user, or the user holds no
    /// password at all.
    ClaimNotVerified,
}

/// What a sign-in that has PROVED the user's password still owes
/// ([`ComAmazonawsCognitoidentityproviderAwscognitoidentityproviderserviceState::sign_in_after_password`]).
#[derive(Debug, Clone)]
pub enum PasswordVerified {
    /// Another challenge stands between this sign-in and its tokens: the one
    /// the caller is answered with, what it reports about itself, and the
    /// one-time password it delivered, for a factor delivered as a message.
    Challenge {
        challenge_name: String,
        challenge_parameters: HashMap<String, String>,
        code: Option<VerificationCode>,
    },
    /// Nothing does: the tokens are what the caller gets.
    Complete,
}

impl UserPoolClient {
    /// Whether `AuthFlow=USER_SRP_AUTH` is enabled for this client.
    pub fn allows_user_srp_auth(&self) -> bool {
        self.allows_any_of(&USER_SRP_AUTH_FLOWS)
    }
}

impl UserPool {
    /// The pool NAME the SRP key schedule hashes: the part of a `UserPoolId`
    /// after the Region that prefixes it, which is what every client library of
    /// this exchange derives its verifier under. A pool id carrying no such
    /// prefix is hashed whole.
    pub fn srp_pool_name(&self) -> &str {
        self.id
            .split_once('_')
            .map_or(self.id.as_str(), |(_region, name)| name)
    }
}

/// Serve the `PASSWORD_VERIFIER` challenge of `user` in `pool`, against the
/// public value `client_srp_a` the sign-in presented.
///
/// The exchange itself is the one [`super::srp`] serves, over the verifier this
/// call derives from the password stored for the user, so the salt and the
/// public value a caller is served carry no password and two exchanges of one
/// user share nothing.
///
/// The salt belongs to the exchange rather than to the user: this service
/// stores the password itself, so each exchange derives the verifier from that
/// password and the salt it served, and the two always describe one key
/// schedule.
///
/// The challenge is answerable until `expires_at`, the window the app client
/// that asked for the sign-in gives a challenge of it, and belongs to that app
/// client's sign-in: `client_id` is what a response has to come through for its
/// claim to answer this challenge.
///
/// `None` when the user holds no password for a verifier to be derived from,
/// when the randomness this needs could not be read, or when the presented
/// public value is none the group can use ([`srp::serve`], which
/// [`INVALID_SRP_A_MESSAGE`] is the refusal of).
pub fn serve_user_srp(
    pool: &UserPool,
    user: &User,
    client_id: &str,
    client_srp_a: &str,
    expires_at: SystemTime,
) -> Option<ServedUserSrpChallenge> {
    let password = user.password.as_deref()?;
    // The salt is reported, and hashed, as these bytes: the key schedule hashes
    // exactly what the challenge served, so both sides derive the verifier over
    // one value.
    let salt = padded(&BigUint::from_bytes_be(&random_bytes::<SALT_BYTES>()?));
    let verifier = password_verifier(pool.srp_pool_name(), &user.username, password, &salt);
    let half = srp::serve(&verifier, client_srp_a)?;

    let mut challenge_parameters = HashMap::new();
    // "In SRP challenges, Amazon Cognito returns the username attribute in
    // `USER_ID_FOR_SRP`" (this service's model), which is the name the claim is
    // signed over and the name the response carries back as its `USERNAME`.
    challenge_parameters.insert(super::USER_ID_FOR_SRP.to_string(), user.username.clone());
    challenge_parameters.insert(CHALLENGE_USERNAME.to_string(), user.username.clone());
    half.report(&mut challenge_parameters, &salt);
    Some(ServedUserSrpChallenge {
        challenge_parameters,
        exchange: UserSrpExchange {
            user_pool_id: pool.id.clone(),
            client_id: client_id.to_string(),
            username: user.username.clone(),
            expires_at,
            salt,
            half,
        },
    })
}

/// The SRP verifier `v = g^x (mod N)` of the password `password` stored for
/// `username` in the pool `pool_name`, derived with `salt`.
///
/// `x = H(salt | H(<PoolName><Username>:<Password>))`, the private key Amazon
/// Cognito's SRP derives a credential under — the same derivation the device
/// leg runs under its own two names.
fn password_verifier(pool_name: &str, username: &str, password: &str, salt: &[u8]) -> BigUint {
    let credential = digest(&[
        pool_name.as_bytes(),
        username.as_bytes(),
        b":",
        password.as_bytes(),
    ]);
    let private_key = BigUint::from_bytes_be(&digest(&[salt, &credential]));
    generator().modpow(&private_key, group_prime())
}

/// Whether the claim `presented_signature`, made at `timestamp` over
/// `presented_secret_block`, proves that the caller holds `password` — the
/// password stored for the user `exchange` was served for, in the pool
/// `pool_name`.
///
/// The one check both legs of this protocol run
/// ([`SrpServerHalf::claim_verifies`]), over the verifier this exchange's own
/// salt derives from that password and the two names a USER signs its claim
/// over: the pool name and the username.
fn claim_verifies(
    pool_name: &str,
    password: &str,
    exchange: &UserSrpExchange,
    presented_secret_block: &str,
    timestamp: &str,
    presented_signature: &str,
) -> bool {
    let verifier = password_verifier(pool_name, &exchange.username, password, &exchange.salt);
    exchange.half.claim_verifies(
        &verifier,
        [pool_name, &exchange.username],
        presented_secret_block,
        timestamp,
        presented_signature,
    )
}

/// The value of the `ChallengeResponses` entry `entry`, or the refusal naming
/// it as missing — an entry carrying no text carries no value, so it is the
/// same missing entry as one that is not there.
fn presented<'r>(
    responses: &'r HashMap<String, String>,
    entry: &'static str,
) -> Result<&'r str, UserSrpRefusal> {
    responses
        .get(entry)
        .map(String::as_str)
        .filter(|value| !value.is_empty())
        .ok_or(UserSrpRefusal::MissingResponse(entry))
}

impl ComAmazonawsCognitoidentityproviderAwscognitoidentityproviderserviceState {
    /// Hold `exchange` as the outstanding `PASSWORD_VERIFIER` challenge of the
    /// sign-in that served it, keyed by the `SECRET_BLOCK` it was served in.
    ///
    /// The continuation of this exchange is the block itself, so that is what a
    /// response resolves it by ([`Self::answer_password_verifier`]) and what
    /// this is held against — no session token names it. Exchanges whose window
    /// the clock has already closed are swept first — the expiry was owed at
    /// this instant whether or not anybody called — and the oldest are evicted
    /// if the store is still at [`MAX_USER_SRP_EXCHANGES`], so repeating the
    /// sign-in cannot grow this map without limit.
    pub fn start_user_srp_exchange(&mut self, now: SystemTime, exchange: UserSrpExchange) {
        self.user_srp_exchanges
            .retain(|_, held| held.expires_at > now);
        evict_oldest_until(
            &mut self.user_srp_exchanges,
            MAX_USER_SRP_EXCHANGES.saturating_sub(1),
            |held| held.expires_at,
        );
        self.user_srp_exchanges
            .insert(exchange.secret_block(), exchange);
    }

    /// Spend the continuation `secret_block` names: a challenge is answered
    /// once, so the exchange goes once the response that ANSWERED it completed
    /// or advanced the sign-in.
    ///
    /// A claim that was refused spends nothing, which is what leaves the
    /// challenge outstanding for a corrected response while its window lasts.
    pub fn end_user_srp_exchange(&mut self, secret_block: &str) {
        self.user_srp_exchanges.remove(secret_block);
    }

    /// Check the password claim a response to the `PASSWORD_VERIFIER` challenge
    /// of the app client `client_id` carries, and answer with the sign-in it
    /// proved its password for.
    ///
    /// The continuation is carried by the response itself: the exchange checked
    /// is the one whose `SECRET_BLOCK` the response echoes, which is also the
    /// block the claim is signed over, so the challenge is answerable by a
    /// caller holding no session token at all. The claim is the HMAC of that
    /// exchange's own session key over the pool name, the username, the block
    /// and the instant the response carries, so a signature computed from any
    /// other password does not verify.
    ///
    /// A PURE check: a refused claim writes nothing and leaves the challenge
    /// outstanding, exactly as a refused one-time password does
    /// ([`Self::answer_mfa_challenge`]), and an accepted one leaves the exchange
    /// standing for the response that ANSWERS the challenge to spend with it
    /// ([`Self::end_user_srp_exchange`]) — so a sign-in whose claim verified but
    /// which the stored user's own state refuses can be presented again once
    /// that state allows it.
    pub fn answer_password_verifier(
        &mut self,
        client_id: &str,
        responses: &HashMap<String, String>,
        now: SystemTime,
    ) -> Result<AnsweredUserSrp, UserSrpRefusal> {
        let timestamp = presented(responses, super::TIMESTAMP_RESPONSE)?.to_string();
        let secret_block =
            presented(responses, super::PASSWORD_CLAIM_SECRET_BLOCK_RESPONSE)?.to_string();
        let signature =
            presented(responses, super::PASSWORD_CLAIM_SIGNATURE_RESPONSE)?.to_string();
        // Continuations the clock has already closed are swept first: the expiry
        // was owed at this instant whether or not anybody called.
        self.user_srp_exchanges
            .retain(|_, held| held.expires_at > now);
        // The exchange the claim is checked against is the one whose block the
        // response echoes, and it answers the sign-in of ITS OWN app client: a
        // block naming no exchange of this service, and one naming an exchange
        // another app client's sign-in served, have no served public value to
        // check a claim against.
        let exchange = self
            .user_srp_exchanges
            .get(&secret_block)
            .filter(|held| held.client_id == client_id)
            .cloned()
            .ok_or(UserSrpRefusal::ClaimNotVerified)?;
        let pool = self
            .pool(&exchange.user_pool_id)
            .ok_or(UserSrpRefusal::ClaimNotVerified)?;
        // A response names the user it answers for, and the claim it carries
        // was signed over the user the challenge was served for: a response
        // naming any other user answers this challenge for nobody.
        if responses
            .get(CHALLENGE_USERNAME)
            .is_none_or(|named| {
                pool.user_storage_key(named) != pool.user_storage_key(&exchange.username)
            })
        {
            return Err(UserSrpRefusal::ClaimNotVerified);
        }
        let user = pool
            .user(&exchange.username)
            .ok_or(UserSrpRefusal::ClaimNotVerified)?;
        let password = user
            .password
            .as_deref()
            .ok_or(UserSrpRefusal::ClaimNotVerified)?;
        if !claim_verifies(
            pool.srp_pool_name(),
            password,
            &exchange,
            &secret_block,
            &timestamp,
            &signature,
        ) {
            return Err(UserSrpRefusal::ClaimNotVerified);
        }
        Ok(AnsweredUserSrp {
            user_pool_id: exchange.user_pool_id.clone(),
            username: user.username.clone(),
            secret_block,
        })
    }

    /// What the sign-in of `username` in the pool `user_pool_id` still owes now
    /// that the user's PASSWORD is proved, as of `now`, for a request that
    /// presented `presented_device_key`.
    ///
    /// The same two questions the password sign-in asks once the stored
    /// password matched, in the same order: a user Amazon Cognito is waiting on
    /// a new password from owes that challenge, and every other user owes the
    /// SECOND factor its pool and its own activated factors decide
    /// ([`UserPool::second_factor`]). So a sign-in that proves the password
    /// through the SRP exchange reaches the same challenge as one that sent the
    /// password directly.
    ///
    /// `None` when the names given are of a user this service no longer holds,
    /// or of a factor nothing can report.
    pub fn sign_in_after_password(
        &self,
        user_pool_id: &str,
        username: &str,
        presented_device_key: Option<&str>,
        now: SystemTime,
    ) -> Option<PasswordVerified> {
        let pool = self.pool(user_pool_id)?;
        let user = pool.user(username)?;
        if user.status == UserStatus::ForceChangePassword {
            return Some(PasswordVerified::Challenge {
                challenge_name: NEW_PASSWORD_REQUIRED_CHALLENGE.to_string(),
                challenge_parameters: new_password_required_challenge_parameters(
                    &user.username,
                    &pool.missing_required_attributes(user),
                    &user.attributes,
                ),
                // This challenge is answered with a password the user chooses,
                // not with a code anything delivered.
                code: None,
            });
        }
        match pool.second_factor(user, presented_device_key, now) {
            SecondFactor::Complete => Some(PasswordVerified::Complete),
            factor => {
                let (challenge_name, challenge_parameters, code) =
                    second_factor_challenge(&user.username, &factor)?;
                Some(PasswordVerified::Challenge {
                    challenge_name,
                    challenge_parameters,
                    code,
                })
            }
        }
    }
}
