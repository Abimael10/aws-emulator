// SPDX-License-Identifier: CC0-1.0

//! Device authentication: the proof of possession a remembered device owes
//! before it stands in for the user's second factor.
//!
//! `DeviceConfigurationType.ChallengeRequiredOnNewDevice`: "When true, a
//! remembered device can sign in with device authentication instead of SMS and
//! time-based one-time password (TOTP) factors for multi-factor authentication
//! (MFA)" (this service's model). Device authentication is the exchange the two
//! `ChallengeNameType` members that carry it run — `DEVICE_SRP_AUTH` and then
//! `DEVICE_PASSWORD_VERIFIER` — over the credentials the device registered:
//! `DeviceSecretVerifierConfig` is "a Secure Remote Password (SRP) value that
//! your application generates when you register a user's device"
//! (`ConfirmDevice` in this service's model). So a `DEVICE_KEY` proves nothing
//! by itself — a sign-in hands it out in `NewDeviceMetadata` and every device
//! read reports it — and the device has to answer with a claim signed under the
//! device password its stored verifier was derived from.
//!
//! One concern of [`super`]'s store, in its own file: the records and helpers
//! here are reached as `crate::state::…` because the parent re-exports them, so
//! both sign-in operations and both challenge-response operations ask the SAME
//! rule.
//!
//! The exchange is the one both legs of this protocol run, read from the one
//! place that defines it ([`super::srp`]): what is DEVICE-specific is that the
//! verifier is the one `ConfirmDevice` registered — derived from the device
//! password under `<DeviceGroupKey><DeviceKey>` — and that those two names are
//! what the device's claim is signed over.

use super::srp::{self, SrpServerHalf, decode_base64, group_prime, padded, zero};
use super::*;
use ::num_bigint::BigUint;

/// The `ChallengeNameType` member that asks a device to start the exchange that
/// authenticates it.
pub const DEVICE_SRP_AUTH_CHALLENGE: &str = "DEVICE_SRP_AUTH";

/// The `ChallengeNameType` member that asks the device for the password claim
/// its stored verifier checks.
pub const DEVICE_PASSWORD_VERIFIER_CHALLENGE: &str = "DEVICE_PASSWORD_VERIFIER";

/// The entry that names the device: "Include a `DEVICE_KEY` for device
/// authentication" (`ChallengeResponses` in this service's model), which is also
/// the `AuthParameters` entry a sign-in presents a device through and the
/// `ChallengeParameters` entry a device challenge names it back in.
pub const DEVICE_KEY_ENTRY: &str = "DEVICE_KEY";

/// The `ChallengeResponses` entry the device's public value arrives in:
/// "`PASSWORD_SRP`: Respond with the initial SRP secret as `SRP_A`"
/// (`ChallengeResponses` in this service's model) is the entry the device leg
/// carries it in too.
pub const SRP_A_RESPONSE: &str = "SRP_A";

/// The `ChallengeResponses` entry carrying back the block the challenge served.
pub const PASSWORD_CLAIM_SECRET_BLOCK_RESPONSE: &str = "PASSWORD_CLAIM_SECRET_BLOCK";

/// The `ChallengeResponses` entry carrying the instant the device signed its
/// claim at, which is part of the signed message.
pub const TIMESTAMP_RESPONSE: &str = "TIMESTAMP";

/// The `ChallengeResponses` entry carrying the signature itself.
pub const PASSWORD_CLAIM_SIGNATURE_RESPONSE: &str = "PASSWORD_CLAIM_SIGNATURE";

impl UserPool {
    /// Whether this pool's device-remembering configuration asks a remembered
    /// device to AUTHENTICATE itself in place of the user's second factor:
    /// "When true, a remembered device can sign in with device authentication
    /// instead of SMS and time-based one-time password (TOTP) factors for
    /// multi-factor authentication (MFA)"
    /// (`DeviceConfigurationType.ChallengeRequiredOnNewDevice` in this
    /// service's model). A pool that activated no device remembering at all
    /// carries no such configuration, so it asks for no device authentication.
    pub fn requires_device_authentication(&self) -> bool {
        self.device_configuration
            .as_ref()
            .is_some_and(|configuration| configuration.challenge_required_on_new_device)
    }

    /// The device of `user` that `presented_device_key` names and that CAN
    /// authenticate itself: one the user confirmed and remembers
    /// ([`User::remembered_device`]) whose confirmation registered the SRP
    /// material the exchange runs over. A key naming any other device — another
    /// user's, one nothing minted, one not confirmed or no longer remembered,
    /// or one confirmed without usable credentials — names no device this pool
    /// can ask for a proof.
    pub fn authenticating_device<'u>(
        &self,
        user: &'u User,
        presented_device_key: Option<&str>,
    ) -> Option<&'u Device> {
        let device = user.remembered_device(presented_device_key?)?;
        device_srp_material(device).is_some().then_some(device)
    }

    /// Whether `presented_device_key` names NO device this pool has registered
    /// to `user` — a key that names a device which does not exist.
    ///
    /// A presented `DEVICE_KEY` names the device the caller is signing in from:
    /// `AuthParameters` is where a caller "add[s] `DEVICE_KEY` if you want to
    /// bypass multi-factor authentication with a remembered device", and
    /// `ChallengeResponses` is where it "include[s] a `DEVICE_KEY` for device
    /// authentication" (this service's model). So it has to name one of the
    /// devices this pool holds for that user — the device a sign-in registered
    /// a key for, whether or not a `ConfirmDevice` has confirmed it since — and
    /// a key naming none of them states a device of nobody: one no sign-in ever
    /// minted, one minted for another user, or one a forget has removed.
    ///
    /// Asked only of a pool that ACTIVATED device remembering
    /// ([`UserPool::remembers_devices`]): a pool that remembers no devices
    /// registers none, so it holds no device for any key to name and a
    /// presented key states nothing about the sign-in.
    pub fn registers_no_presented_device(
        &self,
        user: &User,
        presented_device_key: Option<&str>,
    ) -> bool {
        self.remembers_devices()
            && presented_device_key
                .is_some_and(|device_key| !user.devices.contains_key(device_key))
    }
}

/// The `ChallengeParameters` of a `DEVICE_SRP_AUTH` challenge: the user it is
/// outstanding for — "In SRP challenges, Amazon Cognito returns the username
/// attribute in `USER_ID_FOR_SRP`" (this service's model) — and the device it
/// asks to authenticate.
#[must_use]
pub fn device_srp_auth_challenge_parameters(
    username: &str,
    device_key: &str,
) -> HashMap<String, String> {
    let mut parameters = HashMap::new();
    parameters.insert(super::USER_ID_FOR_SRP.to_string(), username.to_string());
    parameters.insert(DEVICE_KEY_ENTRY.to_string(), device_key.to_string());
    parameters
}

/// The server's half of one device-authentication exchange, bound to the session
/// that carries the `DEVICE_PASSWORD_VERIFIER` challenge it was served with: the
/// claim is answerable exactly while that session lasts, so this is held against
/// the session token rather than against the device.
#[derive(Clone)]
pub struct DeviceSrpExchange {
    /// The device this exchange asks for a proof of possession of.
    pub device_key: String,
    /// The exchange itself, as [`super::srp`] serves and checks one.
    half: SrpServerHalf,
}

/// Stated rather than derived: the private exponent and the secret block of one
/// exchange are secrets of the sign-in it belongs to, so nothing renders them.
impl ::std::fmt::Debug for DeviceSrpExchange {
    fn fmt(&self, formatter: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        formatter
            .debug_struct("DeviceSrpExchange")
            .field("device_key", &self.device_key)
            .finish_non_exhaustive()
    }
}

/// What answering a `DEVICE_SRP_AUTH` challenge serves: the
/// `ChallengeParameters` of the `DEVICE_PASSWORD_VERIFIER` challenge that
/// follows it, and the exchange state to bind to the session carrying it.
#[derive(Debug, Clone)]
pub struct ServedDeviceChallenge {
    pub challenge_parameters: HashMap<String, String>,
    pub exchange: DeviceSrpExchange,
}

/// Why a response to a device-authentication challenge answers it. The operation
/// that carried the response maps each one to a modelled error IT declares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceAuthRefusal {
    /// A `ChallengeResponses` entry the challenge requires and the response did
    /// not carry, named by that entry.
    MissingResponse(&'static str),
    /// The response answers for no device this pool can authenticate for the
    /// user its session was started for
    /// ([`UserPool::authenticating_device`]).
    UnknownDevice,
    /// The device did not prove it holds the device password its stored verifier
    /// was derived from: the claim does not verify, or the exchange it was made
    /// against is not one this service is holding.
    ClaimNotVerified,
}

impl ComAmazonawsCognitoidentityproviderAwscognitoidentityproviderserviceState {
    /// Answer the `DEVICE_SRP_AUTH` challenge `session` is waiting on: serve the
    /// `DEVICE_PASSWORD_VERIFIER` challenge the device's stored credentials are
    /// checked through.
    ///
    /// A PURE read: the exchange this computes is bound to the session the
    /// calling operation starts for the next challenge
    /// ([`Self::start_device_auth_exchange`]), so a response that answers
    /// nothing writes nothing.
    ///
    /// The response has to name the device it is answering for — "Include a
    /// `DEVICE_KEY` for device authentication" (`ChallengeResponses` in this
    /// service's model) — and carry the device's public value, and the device it
    /// names has to be one of THIS session's user that can authenticate itself.
    pub fn answer_device_srp_auth(
        &self,
        session: &AuthSession,
        responses: &HashMap<String, String>,
    ) -> Result<ServedDeviceChallenge, DeviceAuthRefusal> {
        let client_public = presented(responses, SRP_A_RESPONSE)?;
        let device_key = presented(responses, DEVICE_KEY_ENTRY)?;
        let pool = self
            .pool(&session.user_pool_id)
            .ok_or(DeviceAuthRefusal::UnknownDevice)?;
        let user = pool
            .user(&session.username)
            .ok_or(DeviceAuthRefusal::UnknownDevice)?;
        let device = pool
            .authenticating_device(user, Some(device_key))
            .ok_or(DeviceAuthRefusal::UnknownDevice)?;
        serve_device_srp(&user.username, device, client_public)
            .ok_or(DeviceAuthRefusal::ClaimNotVerified)
    }

    /// Check the password claim answering the `DEVICE_PASSWORD_VERIFIER`
    /// challenge the session `session_token` carries, as of `now`, and answer
    /// with the device that proved itself.
    ///
    /// The claim is the HMAC of the exchange's own session key over the device
    /// group key, the device key, the secret block THIS exchange served and the
    /// instant the response carries, so a signature computed from any other
    /// device password does not verify. A refused claim writes nothing and
    /// leaves the challenge outstanding, exactly as a refused one-time password
    /// does ([`Self::answer_mfa_challenge`]); an accepted one is an
    /// authentication by that device, which is what moves its last-authenticated
    /// instant (`DeviceType.DeviceLastAuthenticatedDate` is "the date when the
    /// device was last authenticated", this service's model).
    pub fn answer_device_password_verifier(
        &mut self,
        session_token: &str,
        session: &AuthSession,
        responses: &HashMap<String, String>,
        now: SystemTime,
    ) -> Result<String, DeviceAuthRefusal> {
        let timestamp = presented(responses, TIMESTAMP_RESPONSE)?.to_string();
        let secret_block = presented(responses, PASSWORD_CLAIM_SECRET_BLOCK_RESPONSE)?.to_string();
        let signature = presented(responses, PASSWORD_CLAIM_SIGNATURE_RESPONSE)?.to_string();
        // The exchange the claim is checked against is the one THIS session was
        // served with: a session carrying no exchange of this service has no
        // served secret block or public value to check a claim against at all.
        let exchange = self
            .device_auth_exchange(session_token)
            .cloned()
            .ok_or(DeviceAuthRefusal::ClaimNotVerified)?;
        // A response that names a device answers for THAT device, so one naming
        // another device answers this exchange's challenge for nothing.
        if responses
            .get(DEVICE_KEY_ENTRY)
            .is_some_and(|named| *named != exchange.device_key)
        {
            return Err(DeviceAuthRefusal::UnknownDevice);
        }
        let pool = self
            .pool(&session.user_pool_id)
            .ok_or(DeviceAuthRefusal::UnknownDevice)?;
        let user = pool
            .user(&session.username)
            .ok_or(DeviceAuthRefusal::UnknownDevice)?;
        let device = pool
            .authenticating_device(user, Some(&exchange.device_key))
            .ok_or(DeviceAuthRefusal::UnknownDevice)?;
        if !device_claim_verifies(
            &pool.device_group_key,
            device,
            &exchange,
            &secret_block,
            &timestamp,
            &signature,
        ) {
            return Err(DeviceAuthRefusal::ClaimNotVerified);
        }

        let device_key = exchange.device_key.clone();
        let user_pool_id = session.user_pool_id.clone();
        let username = session.username.clone();
        // The claim is spent: this exchange authenticated the device it was
        // served for, and a second response cannot reuse it.
        self.device_auth_exchanges.remove(session_token);
        if let Some(device) = self
            .pool_mut(&user_pool_id)
            .and_then(|pool| pool.user_mut(&username))
            .and_then(|user| user.confirmed_device_mut(&device_key))
        {
            device.last_authenticated_date = Some(now);
        }
        Ok(device_key)
    }

    /// Bind `exchange` to the session token `session`, which is the session
    /// carrying the `DEVICE_PASSWORD_VERIFIER` challenge it was served with.
    ///
    /// The exchanges of sessions this service no longer holds are dropped first:
    /// a challenge is answerable exactly while its session lasts, so an exchange
    /// outlives nothing and this map stays inside the bound on the stored
    /// sessions themselves ([`Self::start_session`]).
    pub fn start_device_auth_exchange(&mut self, session: &str, exchange: DeviceSrpExchange) {
        let sessions = &self.auth_sessions;
        self.device_auth_exchanges
            .retain(|token, _| sessions.contains_key(token));
        self.device_auth_exchanges
            .insert(session.to_string(), exchange);
    }

    /// The exchange bound to the session token `session`, or `None` when this
    /// service holds none for it.
    fn device_auth_exchange(&self, session: &str) -> Option<&DeviceSrpExchange> {
        self.device_auth_exchanges.get(session)
    }

    /// Issue the token triple of a sign-in a DEVICE authenticated: identical to
    /// [`Self::issue_tokens`] but for the `device_key` claim its access token
    /// carries, which states the device whose proof of possession completed this
    /// sign-in.
    #[allow(clippy::too_many_arguments)]
    pub fn issue_tokens_for_device(
        &mut self,
        user_pool_id: &str,
        client_id: &str,
        username: &str,
        device_key: &str,
        now: SystemTime,
        credentials: &::emulator_server::CredentialStore,
        overrides: &lambda_triggers::TokenOverrides,
    ) -> Option<AuthenticationResultType> {
        let refresh_token = mint_refresh_token(credentials, self.refresh_token_key())?;
        self.mint_tokens(
            user_pool_id,
            client_id,
            username,
            now,
            Some(refresh_token),
            None,
            overrides,
            Some(device_key),
        )
    }
}

/// The value of the `ChallengeResponses` entry `entry`, or the refusal naming it
/// as missing — an entry carrying no text carries no value, so it is the same
/// missing entry as one that is not there.
fn presented<'r>(
    responses: &'r HashMap<String, String>,
    entry: &'static str,
) -> Result<&'r str, DeviceAuthRefusal> {
    responses
        .get(entry)
        .map(String::as_str)
        .filter(|value| !value.is_empty())
        .ok_or(DeviceAuthRefusal::MissingResponse(entry))
}

/// The SRP material `ConfirmDevice` registered for `device`, as the two numbers
/// the exchange runs over: the password verifier and the salt it was derived
/// with, each read from the base64 the device registered it as.
///
/// `None` for a device whose confirmation registered neither, or registered
/// something that is no SRP material of this group: a value of the group is a
/// number modulo its prime, so material spelled in more base64 than that prime
/// takes ([`max_material_base64_chars`]) is none of it, and neither is a
/// verifier that is zero modulo the prime — which no `g^x` is. Both are decided
/// on the LENGTH before anything is decoded, so what a confirmation registered
/// cannot choose the size of the work a later device sign-in does.
fn device_srp_material(device: &Device) -> Option<(BigUint, BigUint)> {
    let configuration = device.secret_verifier.as_ref()?;
    let registered_verifier = configuration.password_verifier.as_deref()?;
    let registered_salt = configuration.salt.as_deref()?;
    let bound = max_material_base64_chars();
    if registered_verifier.len() > bound || registered_salt.len() > bound {
        return None;
    }
    let verifier = decode_base64(registered_verifier)?;
    let salt = decode_base64(registered_salt)?;
    if verifier.is_empty() || salt.is_empty() {
        return None;
    }
    let verifier = BigUint::from_bytes_be(&verifier);
    if &verifier % group_prime() == zero() {
        return None;
    }
    Some((verifier, BigUint::from_bytes_be(&salt)))
}

/// The most base64 a value of this group can be registered as: the group's prime
/// is the widest number in it, and base64 spells three bytes in four characters,
/// with the padding of a last part-block on top.
fn max_material_base64_chars() -> usize {
    padded(group_prime())
        .len()
        .div_ceil(3)
        .saturating_mul(4)
        .saturating_add(4)
}

/// Serve the `DEVICE_PASSWORD_VERIFIER` challenge of `device` to `username`,
/// against the public value `client_srp_a` the device presented.
///
/// The exchange itself is the one [`super::srp`] serves, over the verifier the
/// device registered, so the salt and the public value a caller is served carry
/// no device password and two exchanges of one device share nothing. `None`
/// when the device registered no usable material, or when that exchange could
/// not be served ([`srp::serve`]).
fn serve_device_srp(
    username: &str,
    device: &Device,
    client_srp_a: &str,
) -> Option<ServedDeviceChallenge> {
    let (verifier, salt) = device_srp_material(device)?;
    let half = srp::serve(&verifier, client_srp_a)?;
    let mut challenge_parameters =
        device_srp_auth_challenge_parameters(username, &device.device_key);
    half.report(&mut challenge_parameters, &padded(&salt));
    Some(ServedDeviceChallenge {
        challenge_parameters,
        exchange: DeviceSrpExchange {
            device_key: device.device_key.clone(),
            half,
        },
    })
}

/// Whether the claim `presented_signature`, made at `timestamp` over
/// `presented_secret_block`, proves that the device holds the device password
/// the verifier stored for `device` was derived from.
///
/// The one check both legs of this protocol run ([`SrpServerHalf::claim_verifies`]),
/// over the verifier the device registered and the two names a DEVICE signs its
/// claim over: the device group key and the device key.
fn device_claim_verifies(
    device_group_key: &str,
    device: &Device,
    exchange: &DeviceSrpExchange,
    presented_secret_block: &str,
    timestamp: &str,
    presented_signature: &str,
) -> bool {
    let Some((verifier, _salt)) = device_srp_material(device) else {
        return false;
    };
    exchange.half.claim_verifies(
        &verifier,
        [device_group_key, &exchange.device_key],
        presented_secret_block,
        timestamp,
        presented_signature,
    )
}

