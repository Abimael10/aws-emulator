// SPDX-License-Identifier: CC0-1.0

//! @op DeriveSharedSecret
//! IMPLEMENTED: runs the Elliptic Curve Cryptography Cofactor Diffie-Hellman Primitive
//! (ECDH) between the private half of the named KMS key's elliptic curve key pair and the
//! peer public key the request submits, and answers the raw shared secret it derives.
//! This is the ONLY file to touch for this operation.
//!
//! The key must be one that derives a shared secret: its `KeyUsage` has to be
//! `KEY_AGREEMENT`, which only a NIST-standard elliptic curve or SM2 key spec carries,
//! and any other key is "a KMS key [whose] KeyUsage ... is incompatible with the API
//! operation" (`smithy_query(query="doc",shape="InvalidKeyUsageException")`).
//!
//! What the primitive does with the two halves is the primitive's own rule, and
//! `crate::state::derive_shared_secret` is where it is run rather than decided here: the
//! secret is a function of the two key pairs alone, so deriving twice over the same pair
//! of keys answers the same bytes, and the peer running the agreement from the opposite
//! two halves — its private key and this key's published public key — reaches exactly
//! those bytes. The answer is the RAW secret: deriving a symmetric key from it is the
//! caller's own step, which "KMS recommends ... follow[s] NIST recommendations for key
//! derivation" (`smithy_query(query="doc",shape="DeriveSharedSecret")`).
//!
//! "Both key pairs must be on the same elliptic curve", so a `PublicKey` that is not a
//! DER-encoded X.509 `SubjectPublicKeyInfo` on this key pair's curve is refused as an
//! invalid request rather than agreed with.
//!
//! ## Inputs
//!
//! * `KeyId` (KeyIdType, REQUIRED) — the key whose private half agrees.
//! * `KeyAgreementAlgorithm` (KeyAgreementAlgorithmSpec, REQUIRED) — the primitive to run.
//! * `PublicKey` (PublicKeyType, REQUIRED) — the peer's DER-encoded X.509 public key.
//! * `GrantTokens` (GrantTokenList) — grant tokens the caller draws permission from.
//! * `Recipient` (RecipientInfo) — an attestation document to encrypt the secret to.
//! * `DryRun` (NullableBooleanType) — check the request and refuse.
//!
//! ## Outputs
//!
//! * `SharedSecret` (PlaintextType) — the raw secret the agreement derived.
//! * `CiphertextForRecipient` (CiphertextType) — the secret under an enclave's key.
//! * `KeyId` (KeyIdType) — the key ARN of the key that agreed.
//! * `KeyOrigin` (OriginType) — the source of that key's key material.
//! * `KeyAgreementAlgorithm` (KeyAgreementAlgorithmSpec) — the primitive that ran.
//!
//! ## Errors
//!
//! * `DependencyTimeoutException` (HTTP 503) — the system timed out.
//! * `DisabledException` (HTTP 409) — the KMS key is not enabled.
//! * `DryRunOperationException` (HTTP 412) — the `DryRun` option was set.
//! * `InvalidGrantTokenException` (HTTP 400) — a grant token is not valid.
//! * `InvalidKeyUsageException` (HTTP 400) — the key's `KeyUsage` does not agree.
//! * `KeyUnavailableException` (HTTP 500) — the key was not available.
//! * `KMSInternalException` (HTTP 500) — an internal exception occurred.
//! * `KMSInvalidStateException` (HTTP 409) — the key state is not valid for the request.
//! * `NotFoundException` (HTTP 404) — the identifier names no key of this account.
//!
//! Shared types live in `super::types`; the error channel in `crate::scaffolded`.

#[allow(unused_imports)]
use super::types::*;

// `#[operation_error]` below expands to an invocation of the LOCAL declarative
// `smithy!` macro, which this line is what emits into the module. Without it the
// expansion fails with `cannot find macro 'smithy' in this scope` and the
// `DeriveSharedSecretError` enum is never produced.
smithy_macros::smithy_model!("smithy-model/com-amazonaws-kms-trentservice.json");

// The operation's MODELED errors. `#[operation_error]` accepts only variants the
// model declares for this operation.
#[smithy_macros::operation_error("com.amazonaws.kms#DeriveSharedSecret")]
#[derive(Debug)]
pub enum DeriveSharedSecretErrorModeled {
    DependencyTimeoutException(DependencyTimeoutException),
    DisabledException(DisabledException),
    DryRunOperationException(DryRunOperationException),
    InvalidGrantTokenException(InvalidGrantTokenException),
    InvalidKeyUsageException(InvalidKeyUsageException),
    KeyUnavailableException(KeyUnavailableException),
    KMSInternalException(KMSInternalException),
    KMSInvalidStateException(KMSInvalidStateException),
    NotFoundException(NotFoundException),
}

/// This operation's error channel, as the generated `impl Operation` names it
/// (`crate::ops::derive_shared_secret::DeriveSharedSecretError`). The wrapper is also what
/// carries the refusal classes this operation models no error for — a `ValidationException`
/// for a public key it cannot agree with, an `AccessDeniedException` for a caller without
/// permission.
pub type DeriveSharedSecretError = crate::scaffolded::Scaffolded<DeriveSharedSecretErrorModeled>;

/// Lets `?` and `.into()` lift a modeled failure into the error channel, so the handler
/// writes `Err(SomeModeledError.into())` rather than naming the wrapper variant.
impl From<DeriveSharedSecretErrorModeled> for DeriveSharedSecretError {
    fn from(modeled: DeriveSharedSecretErrorModeled) -> Self {
        crate::scaffolded::Scaffolded::Modeled(modeled)
    }
}

/// The `DeriveSharedSecret` operation.
///
/// Owns the shared engine (the constructor seam) and implements
/// [`smithy_server_runtime::Operation`]. Generic over the engine type `Eng`, so this
/// crate names no concrete engine type.
pub struct DeriveSharedSecret<Eng> {
    engine: ::std::sync::Arc<Eng>,
}

impl<Eng> DeriveSharedSecret<Eng> {
    /// Construct the operation from the shared engine. INFALLIBLE — the engine is a
    /// construction-time dependency, so engine access is guaranteed by the time any
    /// request runs. Called by the generated `EmulatedService::new(engine)` constructor.
    pub fn new(engine: ::std::sync::Arc<Eng>) -> Self {
        Self { engine }
    }
}

impl<Eng: ::std::marker::Send + ::std::marker::Sync + 'static> smithy_server_runtime::Operation
    for DeriveSharedSecret<Eng>
{
    type Input = DeriveSharedSecretInput;
    type Output = DeriveSharedSecretOutput;
    type Error = DeriveSharedSecretError;

    const NAME: &'static str = "DeriveSharedSecret";
    const INPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::DERIVE_SHARED_SECRET_INPUT_SCHEMA;
    const OUTPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::DERIVE_SHARED_SECRET_OUTPUT_SCHEMA;
    const SUCCESS_STATUS: u16 = 200u16;

    async fn handle(
        &self,
        input: Self::Input,
        ctx: &smithy_server_runtime::OperationContext<'_>,
    ) -> Result<Self::Output, Self::Error> {
        let Some(state) = crate::state::service_state(&*self.engine) else {
            return Err(internal("the KMS state store is unavailable"));
        };

        // Who the request speaks for, resolved BEFORE this service's store is locked, so
        // the IAM and STS stores the resolution reads are never reached while this
        // service's guard is held.
        let caller = crate::state::authorization::caller(&*self.engine, ctx);
        let region = crate::state::request_region(ctx);
        let mut state = state.lock();

        let tokens = input.grant_tokens.clone().unwrap_or_default();

        // `DeriveSharedSecret` models no `InvalidArnException`, so a malformed ARN is
        // reported the same way as an identifier that names nothing: the key is not found.
        let key = match state.resolve(&region, &input.key_id) {
            crate::state::KeyRef::Found(key) => key,
            crate::state::KeyRef::UnauthorizedAccount => {
                return Err(crate::state::unauthorized_account(Self::NAME));
            }
            crate::state::KeyRef::NotFound | crate::state::KeyRef::MalformedArn => {
                return Err(DeriveSharedSecretErrorModeled::NotFoundException(
                    NotFoundException {
                        message: Some(crate::state::key_not_found_message(&region, &input.key_id)),
                    },
                )
                .into());
            }
        };

        // "The KMS key that you use for this operation must be in a compatible key state"
        // (`smithy_query(query="doc",shape="DeriveSharedSecret")`): a disabled key answers
        // `DisabledException`, and any other non-usable state answers
        // `KMSInvalidStateException`, which names the state the key is actually in. Judged
        // BEFORE the grant tokens, so a key that cannot be used at all reports why rather
        // than reporting an authorization failure.
        if !key.is_usable() {
            if key.key_state == crate::state::KEY_STATE_DISABLED {
                return Err(DeriveSharedSecretErrorModeled::DisabledException(
                    DisabledException {
                        message: Some(crate::state::invalid_state_message(key)),
                    },
                )
                .into());
            }
            return Err(DeriveSharedSecretErrorModeled::KMSInvalidStateException(
                KMSInvalidStateException {
                    message: Some(crate::state::invalid_state_message(key)),
                },
            )
            .into());
        }

        // A token that is not one this service minted names no grant at all, so the
        // TOKEN "is not valid". Everything else about permission is the service's one
        // authorization decision (`crate::state::authorization`). `DeriveSharedSecret`
        // takes no `EncryptionContext`, so no encryption-context grant constraint applies
        // to it.
        let named = match state.grants_for_tokens(&region, &tokens) {
            Ok(named) => named,
            Err(crate::state::GrantRefusal::MalformedToken) => {
                return Err(DeriveSharedSecretErrorModeled::InvalidGrantTokenException(
                    InvalidGrantTokenException {
                        message: Some("The specified grant token is not valid.".to_string()),
                    },
                )
                .into());
            }
            // Every token is well formed but names a grant the store no longer holds,
            // which carries no permission; what else the caller holds is judged below.
            Err(crate::state::GrantRefusal::NotAuthorized) => Vec::new(),
        };
        let presented = (!tokens.is_empty()).then_some(named.as_slice());
        if let Err(message) = crate::state::authorization::authorize(
            &caller,
            &state,
            &region,
            key,
            Self::NAME,
            None,
            presented,
        ) {
            return Err(crate::scaffolded::Scaffolded::access_denied(message));
        }

        // Which primitive this key agrees under is exactly the key's own key-agreement
        // algorithms — the list that follows from its key spec — and a key with no such
        // list cannot agree at all: "the key usage of the KMS key must be KEY_AGREEMENT"
        // (`smithy_query(query="doc",shape="DeriveSharedSecretRequest",member="KeyId")`).
        let Some(algorithms) = key.algorithms_under(crate::state::KEY_USAGE_KEY_AGREEMENT) else {
            return Err(invalid_key_usage(crate::state::wrong_key_usage_message(
                key,
                Self::NAME,
            )));
        };
        // "The only valid value is ECDH" (the `KeyAgreementAlgorithm` member), and a
        // primitive the key's own spec does not name is "incompatible with the type of key
        // material in the KMS key (KeySpec)"
        // (`smithy_query(query="doc",shape="InvalidKeyUsageException")`).
        if !algorithms.contains(&input.key_agreement_algorithm) {
            return Err(invalid_key_usage(
                crate::state::incompatible_algorithm_message(
                    &input.key_agreement_algorithm,
                    &key.key_spec,
                ),
            ));
        }

        // `Recipient` asks KMS to return the shared secret encrypted under the public key
        // in a Nitro enclave's attestation document. Nothing here can verify an
        // attestation, so that capability is reported as not implemented rather than
        // answered with a `CiphertextForRecipient` no enclave produced.
        if input.recipient.is_some() {
            return Err(crate::scaffolded::Scaffolded::not_implemented(
                "DeriveSharedSecret with a Recipient (enclave attestation)",
            ));
        }

        // "The request was rejected because the DryRun parameter was specified"
        // (`smithy_query(query="doc",shape="DryRunOperationException")`): a dry run checks
        // the request and then always fails, so it is answered AFTER every check above
        // and BEFORE the primitive runs.
        if input.dry_run == Some(true) {
            return Err(DeriveSharedSecretErrorModeled::DryRunOperationException(
                DryRunOperationException {
                    message: Some(crate::state::DRY_RUN_MESSAGE.to_string()),
                },
            )
            .into());
        }

        // The agreement runs over the private half this key holds and the public key the
        // request submits, so the secret is the one the peer reaches from the opposite two
        // halves. Nothing about the secret is stored: it is a function of the two key
        // pairs, which is why nothing has to be.
        let shared_secret = match crate::state::derive_shared_secret(key, &input.public_key) {
            Ok(shared_secret) => shared_secret,
            // The submitted bytes are not a public key this key pair agrees with, which is
            // a bad request rather than a failure of the primitive.
            Err(crate::state::KeyAgreementRefusal::PeerPublicKey) => {
                return Err(crate::scaffolded::Scaffolded::validation(
                    crate::state::invalid_peer_public_key_message(&key.key_spec),
                ));
            }
            // A key whose material is not held here at all — a key whose origin is
            // `EXTERNAL` and whose material has been deleted — is not available for a
            // cryptographic operation.
            Err(crate::state::KeyAgreementRefusal::Unsupported) => {
                return Err(DeriveSharedSecretErrorModeled::KeyUnavailableException(
                    KeyUnavailableException {
                        message: Some(crate::state::invalid_state_message(key)),
                    },
                )
                .into());
            }
            Err(crate::state::KeyAgreementRefusal::Failed) => {
                return Err(internal("the shared secret could not be derived"));
            }
        };

        // KMS answers with the key ARN of the key that agreed, not the identifier the
        // request used, and with "the source of the key material for the specified KMS
        // key" (`smithy_query(query="doc",shape="DeriveSharedSecretResponse",member="KeyOrigin")`).
        let key_arn = key.arn.clone();
        let key_id = key.key_id.clone();
        let key_origin = key.origin.clone();

        // The key was just used in a successful cryptographic operation, and
        // `DeriveSharedSecret` is one of the operations that use is recorded for
        // (`smithy_query(query="shape",shape="KeyLastUsageTrackingOperation")`).
        state.record_last_usage(&region, &key_id, Self::NAME);

        Ok(DeriveSharedSecretOutput {
            key_id: Some(key_arn),
            shared_secret: Some(shared_secret),
            // No enclave took part, so there is nothing to report under the recipient's
            // key: a `Recipient` is refused above.
            ciphertext_for_recipient: None,
            key_agreement_algorithm: Some(input.key_agreement_algorithm),
            key_origin: Some(key_origin),
        })
    }
}

/// Refuse a key that cannot derive a shared secret: "the KeyUsage value of the KMS key is
/// incompatible with the API operation"
/// (`smithy_query(query="doc",shape="InvalidKeyUsageException")`).
fn invalid_key_usage(message: impl Into<String>) -> DeriveSharedSecretError {
    DeriveSharedSecretErrorModeled::InvalidKeyUsageException(InvalidKeyUsageException {
        message: Some(message.into()),
    })
    .into()
}

/// The state store is missing, or the primitive refused an agreement every rule allows: a
/// server fault rather than a bad request.
fn internal(message: impl Into<String>) -> DeriveSharedSecretError {
    DeriveSharedSecretErrorModeled::KMSInternalException(KMSInternalException {
        message: Some(message.into()),
    })
    .into()
}
