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
//! The exchange is SRP-6a over the 3072-bit prime of RFC 3526 — the constant
//! comes from the library that defines the SRP groups rather than from a prime
//! written out here — with Amazon Cognito's own generator and key schedule: the
//! stored verifier is derived from the device password under
//! `<DeviceGroupKey><DeviceKey>`, the session key is the HKDF-SHA256 expansion
//! of the exchange's shared secret salted with its scrambling parameter, and the
//! claim is the HMAC-SHA256 of the device group key, the device key, the secret
//! block the challenge served and the instant the device signed.

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

/// The `ChallengeParameters` entry carrying the server's public value of the
/// exchange.
pub const SRP_B_PARAMETER: &str = "SRP_B";

/// The `ChallengeParameters` entry carrying the salt the device's stored
/// verifier was derived with.
pub const SALT_PARAMETER: &str = "SALT";

/// The `ChallengeParameters` entry carrying the block the device's password
/// claim signs.
pub const SECRET_BLOCK_PARAMETER: &str = "SECRET_BLOCK";

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
    /// The private exponent the served public value was computed from.
    server_secret: Vec<u8>,
    /// The device's public value, as the `SRP_A` it presented.
    client_public: Vec<u8>,
    /// The public value served as `SRP_B`.
    server_public: Vec<u8>,
    /// The bytes served as `SECRET_BLOCK`, which the device's claim signs.
    secret_block: Vec<u8>,
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
/// The server's public value is `B = k·v + g^b (mod N)` over the group below,
/// with `v` the verifier the device registered and `b` a private exponent minted
/// for THIS exchange, so the salt and the public value a caller is served carry
/// no device password and two exchanges of one device share nothing.
///
/// `None` when the presented value is not a usable public value — one that is
/// not hexadecimal, that is spelled in more digits than the group's own prime,
/// or that is zero modulo that prime, which RFC 5054 §2.5.4 aborts the exchange
/// on — or when the randomness this needs could not be read.
fn serve_device_srp(
    username: &str,
    device: &Device,
    client_srp_a: &str,
) -> Option<ServedDeviceChallenge> {
    let (verifier, salt) = device_srp_material(device)?;
    let prime = group_prime();
    // A public value of this group is a number modulo its prime, so one spelled
    // in more digits than the prime itself is no public value of it — and
    // reading one at all is work whose size the caller would otherwise choose.
    if client_srp_a.len() > padded(prime).len().saturating_mul(2) {
        return None;
    }
    let client_public = BigUint::parse_bytes(client_srp_a.as_bytes(), 16)?;
    if &client_public % prime == zero() {
        return None;
    }
    let server_secret = random_bytes::<SERVER_SECRET_BYTES>()?;
    let secret_block = random_bytes::<SECRET_BLOCK_BYTES>()?;
    let exponent = BigUint::from_bytes_be(&server_secret);
    let server_public =
        (multiplier() * &verifier + generator().modpow(&exponent, prime)) % prime;
    if server_public == zero() {
        return None;
    }

    let mut challenge_parameters =
        device_srp_auth_challenge_parameters(username, &device.device_key);
    challenge_parameters.insert(
        SRP_B_PARAMETER.to_string(),
        ::hex::encode(padded(&server_public)),
    );
    challenge_parameters.insert(SALT_PARAMETER.to_string(), ::hex::encode(padded(&salt)));
    challenge_parameters.insert(
        SECRET_BLOCK_PARAMETER.to_string(),
        encode_base64(&secret_block),
    );
    Some(ServedDeviceChallenge {
        challenge_parameters,
        exchange: DeviceSrpExchange {
            device_key: device.device_key.clone(),
            server_secret: server_secret.to_vec(),
            client_public: padded(&client_public),
            server_public: padded(&server_public),
            secret_block: secret_block.to_vec(),
        },
    })
}

/// Whether the claim `presented_signature`, made at `timestamp` over
/// `presented_secret_block`, proves that the device holds the device password
/// the verifier stored for `device` was derived from.
///
/// The device echoes the block the challenge served it, so a response carrying
/// any other block is answering an exchange this one did not serve; the
/// signature itself is checked against the HMAC this exchange's own session key
/// computes, compared in constant time by the library that computes it.
fn device_claim_verifies(
    device_group_key: &str,
    device: &Device,
    exchange: &DeviceSrpExchange,
    presented_secret_block: &str,
    timestamp: &str,
    presented_signature: &str,
) -> bool {
    if presented_secret_block != encode_base64(&exchange.secret_block) {
        return false;
    }
    let Some(signature) = decode_base64(presented_signature) else {
        return false;
    };
    let Some((verifier, _salt)) = device_srp_material(device) else {
        return false;
    };
    let Some(session_key) = exchange.session_key(&verifier) else {
        return false;
    };
    let mut message = Vec::new();
    message.extend_from_slice(device_group_key.as_bytes());
    message.extend_from_slice(exchange.device_key.as_bytes());
    message.extend_from_slice(&exchange.secret_block);
    message.extend_from_slice(timestamp.as_bytes());
    ::ring::hmac::verify(
        &::ring::hmac::Key::new(::ring::hmac::HMAC_SHA256, &session_key),
        &message,
        &signature,
    )
    .is_ok()
}

impl DeviceSrpExchange {
    /// The key this exchange's claim is signed under, for a device whose
    /// registered verifier is `verifier`.
    ///
    /// The scrambling parameter is `u = H(PAD(A) | PAD(B))` and the shared
    /// secret is `S = (A · v^u)^b (mod N)`, which is the same secret the device
    /// computes from its own private exponent and its device password; the key
    /// is the first [`SESSION_KEY_BYTES`] of the HKDF-SHA256 expansion of that
    /// secret, salted with the scrambling parameter and labelled
    /// [`DERIVED_KEY_LABEL`].
    fn session_key(&self, verifier: &BigUint) -> Option<[u8; SESSION_KEY_BYTES]> {
        let prime = group_prime();
        let client_public = BigUint::from_bytes_be(&self.client_public);
        let server_public = BigUint::from_bytes_be(&self.server_public);
        let exponent = BigUint::from_bytes_be(&self.server_secret);
        let scrambler = BigUint::from_bytes_be(&digest(&[
            &padded(&client_public),
            &padded(&server_public),
        ]));
        if scrambler == zero() {
            return None;
        }
        let shared = (client_public * verifier.modpow(&scrambler, prime) % prime)
            .modpow(&exponent, prime);
        let mut session_key = [0_u8; SESSION_KEY_BYTES];
        ::hkdf::Hkdf::<::sha2::Sha256>::new(Some(&padded(&scrambler)), &padded(&shared))
            .expand(DERIVED_KEY_LABEL, &mut session_key)
            .ok()?;
        Some(session_key)
    }
}

/// The prime of the group the exchange runs over: the 3072-bit group of RFC
/// 3526, read from the library that defines the SRP groups (RFC 5054 takes its
/// 3072-bit group from there) so the constant has one source rather than a prime
/// written out here.
fn group_prime() -> &'static BigUint {
    &::srp::groups::G_3072.n
}

/// The generator Amazon Cognito's SRP runs over that prime: 2, which is the
/// generator every client library of this exchange computes its public value
/// with, rather than the one RFC 5054 pairs with the 3072-bit prime.
fn generator() -> BigUint {
    BigUint::from(2_u32)
}

/// The multiplier parameter of SRP-6a over that group: `k = H(PAD(N) | PAD(g))`.
fn multiplier() -> BigUint {
    BigUint::from_bytes_be(&digest(&[&padded(group_prime()), &padded(&generator())]))
}

/// Zero, the value a public value of the exchange may not be modulo the group's
/// prime.
fn zero() -> BigUint {
    BigUint::from(0_u32)
}

/// The SHA-256 digest of `parts` concatenated, the hash of this key schedule.
fn digest(parts: &[&[u8]]) -> [u8; 32] {
    use ::sha2::Digest as _;
    let mut hasher = ::sha2::Sha256::new();
    for part in parts {
        hasher.update(part);
    }
    hasher.finalize().into()
}

/// `value` as the big-endian bytes this key schedule hashes and reports it as:
/// its shortest representation, with a leading zero byte where the first byte's
/// top bit is set, so every value is read as a positive number by both sides of
/// the exchange.
fn padded(value: &BigUint) -> Vec<u8> {
    let bytes = value.to_bytes_be();
    if bytes.first().is_some_and(|first| first & 0x80 != 0) {
        let mut padded = Vec::with_capacity(bytes.len().saturating_add(1));
        padded.push(0);
        padded.extend_from_slice(&bytes);
        return padded;
    }
    bytes
}

/// `N` bytes of the randomness this service mints secrets from
/// (`ring::rand::SystemRandom`), or `None` when it could not be read.
fn random_bytes<const N: usize>() -> Option<[u8; N]> {
    let mut bytes = [0_u8; N];
    ::ring::rand::SecureRandom::fill(&::ring::rand::SystemRandom::new(), &mut bytes).ok()?;
    Some(bytes)
}

/// `bytes` in base64, the encoding the served secret block travels in.
fn encode_base64(bytes: &[u8]) -> String {
    use ::base64::Engine as _;
    ::base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// The bytes `text` carries as base64, padded or not: a value a caller
/// registered or signed is read whichever way its library wrote the padding.
fn decode_base64(text: &str) -> Option<Vec<u8>> {
    use ::base64::Engine as _;
    ::base64::engine::general_purpose::STANDARD
        .decode(text)
        .ok()
        .or_else(|| {
            ::base64::engine::general_purpose::STANDARD_NO_PAD
                .decode(text)
                .ok()
        })
}

/// The private exponent of one exchange: 256 bits, the floor RFC 5054 §2.5.4
/// states for the server's private value.
const SERVER_SECRET_BYTES: usize = 32;

/// The block a device's password claim signs, minted per exchange.
const SECRET_BLOCK_BYTES: usize = 64;

/// The size of the key the claim is signed under, which is what this key
/// schedule expands its shared secret to.
const SESSION_KEY_BYTES: usize = 16;

/// The label this key schedule expands the shared secret under.
const DERIVED_KEY_LABEL: &[u8] = b"Caldera Derived Key";
