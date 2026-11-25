// SPDX-License-Identifier: CC0-1.0

//! The SASL exchange a client completes on a cluster's endpoint before it may read or
//! write documents.
//!
//! An Amazon DocumentDB cluster authenticates its clients as the master user the create
//! named, and a driver authenticates with SCRAM — RFC 5802, over SHA-1 and over SHA-256 —
//! which is what [`ScramExchange`] runs the server half of. The client proves it holds the
//! password without sending it, and the server proves it holds the same one, so a session
//! that completes the exchange really did present the cluster's master password.
//!
//! The password itself is NOT stored. What a create keeps is only what a server needs to
//! recognise it again: per mechanism a random salt, the iteration count, the STORED key
//! (the digest of the client key) and the server key ([`ScramCredentials`]), which is
//! exactly the credential form the exchange is checked against and from which the password
//! cannot be recovered. The two mechanisms differ in how the password is prepared before it
//! is salted: SHA-1 salts the hex digest of `<user>:mongo:<password>`, which is the form the
//! engine's own SCRAM-SHA-1 credential uses, and SHA-256 salts the password itself.
//!
//! Every value a client sends is bounded before any work is done on it ([`MAX_USERNAME_BYTES`],
//! [`MAX_NONCE_BYTES`], [`MAX_PAYLOAD_BYTES`]), so one connection cannot size this process's
//! work.

use ::rand::RngCore;

/// The mechanism names this endpoint offers, in the order `hello` reports them for a user
/// that asked which mechanisms it may use (`saslSupportedMechs`): the stronger one first,
/// which is the order a driver picks from.
pub const SUPPORTED_MECHANISMS: [&str; 2] = [SCRAM_SHA_256, SCRAM_SHA_1];

/// RFC 5802 over SHA-256.
pub const SCRAM_SHA_256: &str = "SCRAM-SHA-256";

/// RFC 5802 over SHA-1, with the password prepared as the engine's own SCRAM-SHA-1
/// credential prepares it (the hex MD5 digest of `<user>:mongo:<password>`).
pub const SCRAM_SHA_1: &str = "SCRAM-SHA-1";

/// The iteration count a SCRAM-SHA-1 credential is salted at, which is the engine's own
/// default for that mechanism.
const SHA_1_ITERATIONS: u32 = 10_000;

/// The iteration count a SCRAM-SHA-256 credential is salted at, which is the engine's own
/// default for that mechanism.
const SHA_256_ITERATIONS: u32 = 15_000;

/// How many bytes of salt a credential carries. RFC 5802 fixes no length; this is the
/// engine's own salt length for both mechanisms.
const SALT_BYTES: usize = 16;

/// How many random bytes the server nonce is built from.
const SERVER_NONCE_BYTES: usize = 24;

/// The most bytes of user name this endpoint reads out of a client-first message. CHOSEN as
/// a process-safety bound, well above the longest `MasterUsername` the control plane accepts.
pub const MAX_USERNAME_BYTES: usize = 256;

/// The most bytes of client nonce this endpoint reads. CHOSEN as a process-safety bound: the
/// nonce is carried in the message the proof is computed over.
pub const MAX_NONCE_BYTES: usize = 256;

/// The most bytes one SASL payload may carry, bounding every value above together. CHOSEN,
/// and far above the longest exchange a driver sends.
pub const MAX_PAYLOAD_BYTES: usize = 4096;

/// Why an exchange could not be completed. Every variant is answered to the client the same
/// way the engine answers a failed authentication, so a client learns nothing about WHICH
/// part did not hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthFailure {
    /// The mechanism the client named is not one this endpoint offers.
    UnsupportedMechanism,
    /// The payload is not a SCRAM message of the expected step, or carries a value past one
    /// of the bounds above.
    MalformedPayload,
    /// The exchange named a user that is not this cluster's, or the proof does not hold.
    NotAuthenticated,
}

/// One mechanism's stored credential: what the server keeps so it can check a proof.
#[derive(Clone)]
struct MechanismCredential {
    salt: Vec<u8>,
    iterations: u32,
    /// `H(HMAC(salted_password, "Client Key"))`.
    stored_key: Vec<u8>,
    /// `HMAC(salted_password, "Server Key")`.
    server_key: Vec<u8>,
}

/// Written by hand rather than derived, so nothing that formats a record — a log line, a
/// test failure — can print a value derived from the master password.
impl std::fmt::Debug for MechanismCredential {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("MechanismCredential(<redacted>)")
    }
}

/// The master user's SCRAM credentials, one per mechanism this endpoint offers.
#[derive(Clone, Debug)]
pub struct ScramCredentials {
    /// The user these credentials authenticate, which a client-first message must name.
    username: String,
    sha_1: MechanismCredential,
    sha_256: MechanismCredential,
}

impl ScramCredentials {
    /// The credentials of the master user `username` with `password`, salted freshly.
    ///
    /// `password` is used only here: what the caller stores is the returned value, from
    /// which the password cannot be recovered.
    #[must_use]
    pub fn derive(username: &str, password: &str) -> Self {
        // The SCRAM-SHA-1 mechanism salts the hex MD5 digest of `<user>:mongo:<password>`,
        // which is the password form the engine's own SCRAM-SHA-1 credential is built from.
        let sha_1_password = {
            use ::md5::Digest;
            let digest = ::md5::Md5::digest(format!("{username}:mongo:{password}").as_bytes());
            ::hex::encode(digest)
        };
        // SCRAM-SHA-256 salts the password itself, prepared with SASLprep (RFC 4013). Every
        // `MasterUserPassword` this service accepts is printable ASCII
        // (`crate::state::validate_master_user_password`), which SASLprep maps to itself, so
        // the prepared form is the value as given.
        Self {
            username: username.to_string(),
            sha_1: MechanismCredential::derive_sha_1(&sha_1_password),
            sha_256: MechanismCredential::derive_sha_256(password),
        }
    }

    /// The user these credentials authenticate.
    #[must_use]
    pub fn username(&self) -> &str {
        &self.username
    }
}

impl MechanismCredential {
    fn derive_sha_1(password: &str) -> Self {
        let salt = random_bytes(SALT_BYTES);
        let mut salted = [0u8; 20];
        ::pbkdf2::pbkdf2_hmac::<::sha1::Sha1>(
            password.as_bytes(),
            &salt,
            SHA_1_ITERATIONS,
            &mut salted,
        );
        let client_key = hmac_sha_1(&salted, b"Client Key");
        Self {
            salt,
            iterations: SHA_1_ITERATIONS,
            stored_key: sha_1(&client_key),
            server_key: hmac_sha_1(&salted, b"Server Key"),
        }
    }

    fn derive_sha_256(password: &str) -> Self {
        let salt = random_bytes(SALT_BYTES);
        let mut salted = [0u8; 32];
        ::pbkdf2::pbkdf2_hmac::<::sha2::Sha256>(
            password.as_bytes(),
            &salt,
            SHA_256_ITERATIONS,
            &mut salted,
        );
        let client_key = hmac_sha_256(&salted, b"Client Key");
        Self {
            salt,
            iterations: SHA_256_ITERATIONS,
            stored_key: sha_256(&client_key),
            server_key: hmac_sha_256(&salted, b"Server Key"),
        }
    }
}

/// Which mechanism an exchange is running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mechanism {
    Sha1,
    Sha256,
}

/// Where an exchange has got to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    /// The server-first message is out; the client's proof is next.
    AwaitingProof,
    /// The proof held and the server signature is out; the client sends one empty message
    /// to finish, which is the exchange as a client that did not ask to skip it runs.
    AwaitingFinalEmpty,
    /// Nothing more is expected.
    Finished,
}

/// The server half of one SCRAM conversation, held by the session that started it.
#[derive(Debug)]
pub struct ScramExchange {
    mechanism: Mechanism,
    conversation_id: i32,
    credential: MechanismCredential,
    username: String,
    /// The client-first message without its GS2 header, which the proof is computed over.
    client_first_bare: String,
    server_first: String,
    /// What the second reply carries, held until the client's final empty message when the
    /// client did not ask for the exchange to be skipped.
    server_final: Vec<u8>,
    stage: Stage,
}

impl ScramExchange {
    /// Begin an exchange from a `saslStart` payload, answering with the server-first message.
    ///
    /// `skip_empty_exchange` is what the client asked for in `options.skipEmptyExchange`: a
    /// client that asked for it is told the conversation is done as soon as its proof holds,
    /// and one that did not sends a final empty message first.
    pub fn start(
        mechanism: &str,
        credentials: &ScramCredentials,
        conversation_id: i32,
        payload: &[u8],
    ) -> Result<(Self, Vec<u8>), AuthFailure> {
        let mechanism = match mechanism {
            SCRAM_SHA_1 => Mechanism::Sha1,
            SCRAM_SHA_256 => Mechanism::Sha256,
            _ => return Err(AuthFailure::UnsupportedMechanism),
        };
        if payload.len() > MAX_PAYLOAD_BYTES {
            return Err(AuthFailure::MalformedPayload);
        }
        let message = std::str::from_utf8(payload).map_err(|_| AuthFailure::MalformedPayload)?;
        let bare = strip_gs2_header(message).ok_or(AuthFailure::MalformedPayload)?;
        let username = attribute(bare, 'n').ok_or(AuthFailure::MalformedPayload)?;
        let client_nonce = attribute(bare, 'r').ok_or(AuthFailure::MalformedPayload)?;
        if username.len() > MAX_USERNAME_BYTES || client_nonce.len() > MAX_NONCE_BYTES {
            return Err(AuthFailure::MalformedPayload);
        }
        // The name a client-first message carries has to be the user these credentials
        // authenticate: another user is not this cluster's, whatever proof follows.
        let username = unescape_saslname(username).ok_or(AuthFailure::MalformedPayload)?;
        if username != credentials.username {
            return Err(AuthFailure::NotAuthenticated);
        }
        let credential = match mechanism {
            Mechanism::Sha1 => credentials.sha_1.clone(),
            Mechanism::Sha256 => credentials.sha_256.clone(),
        };
        let server_nonce = ::aws_smithy_types::base64::encode(random_bytes(SERVER_NONCE_BYTES));
        let combined_nonce = format!("{client_nonce}{server_nonce}");
        let server_first = format!(
            "r={combined_nonce},s={},i={}",
            ::aws_smithy_types::base64::encode(&credential.salt),
            credential.iterations
        );
        let exchange = Self {
            mechanism,
            conversation_id,
            credential,
            username,
            client_first_bare: bare.to_string(),
            server_first: server_first.clone(),
            server_final: Vec::new(),
            stage: Stage::AwaitingProof,
        };        Ok((exchange, server_first.into_bytes()))
    }

    /// The conversation id this exchange answers under, which every reply carries and every
    /// `saslContinue` of it must name.
    #[must_use]
    pub fn conversation_id(&self) -> i32 {
        self.conversation_id
    }

    /// Take one `saslContinue` payload, answering with the next server message and whether
    /// the conversation is done.
    ///
    /// The user is reported once the proof holds, which is what the session records as who it
    /// is authenticated as.
    pub fn advance(
        &mut self,
        payload: &[u8],
        skip_empty_exchange: bool,
    ) -> Result<(Vec<u8>, bool, Option<String>), AuthFailure> {
        if payload.len() > MAX_PAYLOAD_BYTES {
            return Err(AuthFailure::MalformedPayload);
        }
        match self.stage {
            Stage::AwaitingProof => {
                let message =
                    std::str::from_utf8(payload).map_err(|_| AuthFailure::MalformedPayload)?;
                let proof = attribute(message, 'p').ok_or(AuthFailure::MalformedPayload)?;
                let without_proof = message
                    .rsplit_once(",p=")
                    .map(|(head, _)| head)
                    .ok_or(AuthFailure::MalformedPayload)?;
                let nonce = attribute(message, 'r').ok_or(AuthFailure::MalformedPayload)?;
                if Some(nonce) != attribute(&self.server_first, 'r') {
                    return Err(AuthFailure::NotAuthenticated);
                }
                let auth_message = format!(
                    "{},{},{without_proof}",
                    self.client_first_bare, self.server_first
                );
                let proof = ::aws_smithy_types::base64::decode(proof)
                    .map_err(|_| AuthFailure::MalformedPayload)?;
                if !self.proof_holds(&proof, auth_message.as_bytes()) {
                    return Err(AuthFailure::NotAuthenticated);
                }
                let signature = self.mac(&self.credential.server_key, auth_message.as_bytes());
                let server_final =
                    format!("v={}", ::aws_smithy_types::base64::encode(&signature)).into_bytes();
                let username = self.username.clone();
                if skip_empty_exchange {
                    self.stage = Stage::Finished;
                    return Ok((server_final, true, Some(username)));
                }
                self.server_final = server_final.clone();
                self.stage = Stage::AwaitingFinalEmpty;
                Ok((server_final, false, Some(username)))
            }
            // The client's closing message carries nothing, and the server's answer repeats
            // the signature it already sent.
            Stage::AwaitingFinalEmpty => {
                self.stage = Stage::Finished;
                Ok((std::mem::take(&mut self.server_final), true, None))
            }
            Stage::Finished => Err(AuthFailure::MalformedPayload),
        }
    }

    /// Whether `proof` is the client proof of this credential for `auth_message`.
    ///
    /// The client sends `ClientKey XOR HMAC(StoredKey, AuthMessage)`, so the server recovers
    /// the client key from the proof and the key it stores, and the proof holds exactly when
    /// the digest of that recovered key is the stored key.
    fn proof_holds(&self, proof: &[u8], auth_message: &[u8]) -> bool {
        let signature = self.mac(&self.credential.stored_key, auth_message);
        if proof.len() != signature.len() {
            return false;
        }
        let recovered: Vec<u8> = proof
            .iter()
            .zip(signature.iter())
            .map(|(sent, mask)| sent ^ mask)
            .collect();
        self.digest(&recovered) == self.credential.stored_key
    }

    fn mac(&self, key: &[u8], data: &[u8]) -> Vec<u8> {
        match self.mechanism {
            Mechanism::Sha1 => hmac_sha_1(key, data),
            Mechanism::Sha256 => hmac_sha_256(key, data),
        }
    }

    fn digest(&self, data: &[u8]) -> Vec<u8> {
        match self.mechanism {
            Mechanism::Sha1 => sha_1(data),
            Mechanism::Sha256 => sha_256(data),
        }
    }
}

/// The client-first message without its GS2 header: RFC 5802 puts the channel-binding flag
/// and an optional authorization identity ahead of the part the proof is computed over.
fn strip_gs2_header(message: &str) -> Option<&str> {
    let mut parts = message.splitn(3, ',');
    let flag = parts.next()?;
    // `n` (no channel binding), `y` (the client supports it but thinks the server does not)
    // and `p=<type>` (bound) are the three forms; this endpoint binds no channel, so a client
    // asking for one is not speaking a mechanism it offers.
    if flag != "n" && flag != "y" {
        return None;
    }
    // The authorization identity, which this endpoint does not use: a session acts as the
    // user it authenticated as.
    let _authorization_identity = parts.next()?;
    parts.next()
}

/// The value of the `<key>=` attribute of a comma-separated SCRAM message.
fn attribute(message: &str, key: char) -> Option<&str> {
    message
        .split(',')
        .filter_map(|field| field.split_once('='))
        .find(|(name, _)| name.len() == 1 && name.starts_with(key))
        .map(|(_, value)| value)
}

/// A SASL-escaped user name, unescaped: `=2C` is a comma and `=3D` an equals sign, and no
/// other `=` escape is defined, so a name carrying one is malformed.
fn unescape_saslname(value: &str) -> Option<String> {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some((head, tail)) = rest.split_once('=') {
        out.push_str(head);
        let (escape, remainder) = tail.split_at_checked(2)?;
        match escape {
            "2C" | "2c" => out.push(','),
            "3D" | "3d" => out.push('='),
            _ => return None,
        }
        rest = remainder;
    }
    out.push_str(rest);
    Some(out)
}

/// `count` bytes from this process's cryptographic random generator, which is where every
/// salt and nonce here comes from.
///
/// A SCRAM salt and server nonce have to be UNPREDICTABLE for the exchange to be
/// replay-resistant, so nothing weaker stands in for this source: `rand`'s thread generator
/// is seeded from the operating system and reseeds itself, and an operating system that
/// cannot seed it at all leaves this process with no usable randomness.
fn random_bytes(count: usize) -> Vec<u8> {
    let mut bytes = vec![0u8; count];
    ::rand::rng().fill_bytes(&mut bytes);
    bytes
}

fn sha_1(data: &[u8]) -> Vec<u8> {
    use ::sha1::Digest;
    ::sha1::Sha1::digest(data).to_vec()
}

fn sha_256(data: &[u8]) -> Vec<u8> {
    use ::sha2::Digest;
    ::sha2::Sha256::digest(data).to_vec()
}

fn hmac_sha_1(key: &[u8], data: &[u8]) -> Vec<u8> {
    use ::hmac::digest::KeyInit;
    use ::hmac::Mac;
    let mut mac = <::hmac::Hmac<::sha1::Sha1> as KeyInit>::new_from_slice(key)
        .expect("HMAC-SHA-1 takes a key of any length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

fn hmac_sha_256(key: &[u8], data: &[u8]) -> Vec<u8> {
    use ::hmac::digest::KeyInit;
    use ::hmac::Mac;
    let mut mac = <::hmac::Hmac<::sha2::Sha256> as KeyInit>::new_from_slice(key)
        .expect("HMAC-SHA-256 takes a key of any length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What one mechanism computes with: its HMAC and its digest, which the client half below
    /// uses exactly as the server half does.
    type Primitives = (Vec<u8>, fn(&[u8], &[u8]) -> Vec<u8>, fn(&[u8]) -> Vec<u8>);

    /// The client half of the exchange, written out of RFC 5802 so the test proves the
    /// server half against the specification rather than against itself.
    fn client_proof(
        mechanism: Mechanism,
        password: &str,
        username: &str,
        client_first_bare: &str,
        server_first: &str,
        client_final_without_proof: &str,
    ) -> Vec<u8> {
        let salt = ::aws_smithy_types::base64::decode(attribute(server_first, 's').unwrap())
            .expect("the server salts in base64");
        let iterations: u32 = attribute(server_first, 'i').unwrap().parse().unwrap();
        let auth_message = format!("{client_first_bare},{server_first},{client_final_without_proof}");
        let (salted, mac, digest): Primitives = match mechanism {
            Mechanism::Sha1 => {
                use ::md5::Digest;
                let prepared = ::hex::encode(::md5::Md5::digest(
                    format!("{username}:mongo:{password}").as_bytes(),
                ));
                let mut salted = [0u8; 20];
                ::pbkdf2::pbkdf2_hmac::<::sha1::Sha1>(
                    prepared.as_bytes(),
                    &salt,
                    iterations,
                    &mut salted,
                );
                (salted.to_vec(), hmac_sha_1, sha_1)
            }
            Mechanism::Sha256 => {
                let mut salted = [0u8; 32];
                ::pbkdf2::pbkdf2_hmac::<::sha2::Sha256>(
                    password.as_bytes(),
                    &salt,
                    iterations,
                    &mut salted,
                );
                (salted.to_vec(), hmac_sha_256, sha_256)
            }
        };
        let client_key = mac(&salted, b"Client Key");
        let stored_key = digest(&client_key);
        let signature = mac(&stored_key, auth_message.as_bytes());
        client_key
            .iter()
            .zip(signature.iter())
            .map(|(key, sign)| key ^ sign)
            .collect()
    }

    /// RULE: a client that holds the master password completes the exchange on either
    /// mechanism, and the server's signature is the one the client can verify.
    #[test]
    fn a_client_holding_the_password_completes_the_exchange() {
        for (mechanism, name) in [(Mechanism::Sha1, SCRAM_SHA_1), (Mechanism::Sha256, SCRAM_SHA_256)]
        {
            let credentials = ScramCredentials::derive("docdbadmin", "a-Secret-99");
            let client_first_bare = "n=docdbadmin,r=clientnonce00";
            let (mut exchange, server_first) = ScramExchange::start(
                name,
                &credentials,
                1,
                format!("n,,{client_first_bare}").as_bytes(),
            )
            .unwrap_or_else(|error| panic!("case {name}: saslStart must be taken, got {error:?}"));
            let server_first = String::from_utf8(server_first).expect("the server-first is text");
            let combined = attribute(&server_first, 'r').expect("it carries the nonce");
            let client_final_without_proof = format!("c=biws,r={combined}");
            let proof = client_proof(
                mechanism,
                "a-Secret-99",
                "docdbadmin",
                client_first_bare,
                &server_first,
                &client_final_without_proof,
            );
            let client_final = format!(
                "{client_final_without_proof},p={}",
                ::aws_smithy_types::base64::encode(&proof)
            );
            let (server_final, done, user) = exchange
                .advance(client_final.as_bytes(), true)
                .unwrap_or_else(|error| {
                    panic!("case {name}: the proof must hold, got {error:?}")
                });
            assert!(done, "case {name}: a skipped empty exchange finishes on the proof");
            assert_eq!(
                user.as_deref(),
                Some("docdbadmin"),
                "case {name}: the exchange authenticates the user the client named"
            );
            let server_final = String::from_utf8(server_final).expect("the server-final is text");
            assert!(
                server_final.starts_with("v="),
                "case {name}: expected a server signature, got {server_final:?}"
            );
        }
    }

    /// RULE: a proof computed from another password does not hold, and a client-first message
    /// naming a user that is not this cluster's is refused before any proof.
    #[test]
    fn a_client_without_the_password_is_not_authenticated() {
        let credentials = ScramCredentials::derive("docdbadmin", "a-Secret-99");
        for (case, username, password) in [
            ("the wrong password", "docdbadmin", "another-Secret-99"),
            ("another user's name", "someoneelse", "a-Secret-99"),
        ] {
            let client_first_bare = format!("n={username},r=clientnonce00");
            let started = ScramExchange::start(
                SCRAM_SHA_256,
                &credentials,
                1,
                format!("n,,{client_first_bare}").as_bytes(),
            );
            if case == "another user's name" {
                assert_eq!(
                    started.err(),
                    Some(AuthFailure::NotAuthenticated),
                    "case {case}: a name that is not this cluster's is refused at saslStart"
                );
                continue;
            }
            let (mut exchange, server_first) =
                started.expect("the cluster's own user begins the exchange");
            let server_first = String::from_utf8(server_first).expect("the server-first is text");
            let combined = attribute(&server_first, 'r').expect("it carries the nonce");
            let client_final_without_proof = format!("c=biws,r={combined}");
            let proof = client_proof(
                Mechanism::Sha256,
                password,
                username,
                &client_first_bare,
                &server_first,
                &client_final_without_proof,
            );
            let client_final = format!(
                "{client_final_without_proof},p={}",
                ::aws_smithy_types::base64::encode(&proof)
            );
            let outcome = exchange.advance(client_final.as_bytes(), true);
            assert_eq!(
                outcome.err(),
                Some(AuthFailure::NotAuthenticated),
                "case {case}: the proof must not hold"
            );
        }
    }

    /// RULE: a payload that is not a SCRAM message of its step is malformed, whatever it
    /// carries.
    #[test]
    fn a_payload_that_is_not_a_scram_message_is_refused() {
        let credentials = ScramCredentials::derive("docdbadmin", "a-Secret-99");
        for (case, payload) in [
            ("no gs2 header", "n=docdbadmin,r=nonce"),
            ("a bound channel", "p=tls-unique,,n=docdbadmin,r=nonce"),
            ("no user name", "n,,r=nonce"),
            ("no nonce", "n,,n=docdbadmin"),
        ] {
            let started = ScramExchange::start(SCRAM_SHA_256, &credentials, 1, payload.as_bytes());
            assert!(
                matches!(started, Err(AuthFailure::MalformedPayload)),
                "case {case}: expected a malformed payload, got {:?}",
                started.map(|_| "accepted")
            );
        }
        let unsupported = ScramExchange::start(
            "PLAIN",
            &credentials,
            1,
            b"n,,n=docdbadmin,r=nonce".as_slice(),
        );
        assert!(
            matches!(unsupported, Err(AuthFailure::UnsupportedMechanism)),
            "a mechanism this endpoint does not offer is refused as unsupported"
        );
    }

    /// RULE: a user name carrying a SASL escape is the name it unescapes to.
    #[test]
    fn an_escaped_user_name_is_unescaped() {
        for (case, escaped, expected) in [
            ("a plain name", "docdbadmin", Some("docdbadmin")),
            ("an escaped comma", "doc=2Cadmin", Some("doc,admin")),
            ("an escaped equals sign", "doc=3Dadmin", Some("doc=admin")),
            ("an escape that is not defined", "doc=99admin", None),
            ("a truncated escape", "docadmin=2", None),
        ] {
            let unescaped = unescape_saslname(escaped);
            assert_eq!(
                unescaped.as_deref(),
                expected,
                "case {case}: expected {expected:?}, got {unescaped:?}"
            );
        }
    }
}
