// SPDX-License-Identifier: CC0-1.0

//! The SASL/SCRAM credential a broker authenticates a connection against, when the
//! Secrets Manager secret that carries it is SEALED under a customer managed KMS key.
//!
//! RULE under test: the credential a SASL/SCRAM connection is authenticated against is
//! the username/password pair in one of the cluster's ASSOCIATED secrets, whatever the
//! value of that secret is stored under — so a secret Secrets Manager sealed under a
//! customer managed key authenticates its user exactly as one it holds in the clear
//! does, and a user no associated secret names is refused either way.
//!
//! The rows differ along what the rule generalises over: whether the service that owns
//! the key is mounted beside this one (so whether the stored value really is sealed),
//! and whether the user offering a credential is one an associated secret names.
//!
//! Every assertion goes over a real socket the core opened for the cluster, dialled at
//! the address `GetBootstrapBrokers` published for the SASL/SCRAM path.

use crate::broker_data_plane::{given_secret_associated_under_key, Client, SCRAM_SHA_512};
use crate::kafka_harness::{ClusterRequest, Emulator, REGION};

/// The user and password the secret below carries.
const SCRAM_USER: &str = "sealed-user";
const SCRAM_PASSWORD: &str = "sealed-Secret-99";

/// A customer managed key the account does NOT hold, for the row that mounts no KMS: a
/// secret may only name a customer managed key, and with no KMS to ask the value is
/// stored as it was sent.
const UNHELD_KEY_ARN: &str =
    "arn:aws:kms:us-west-2:000000000000:key/9b1dca1e-7c4f-4a6e-8f0a-5b2d7c3e1f40";

/// RULE: an associated secret's credential authenticates its user over SASL/SCRAM
/// whether or not the secret's value is sealed under a customer managed key, and a user
/// no associated secret names is refused.
#[tokio::test]
async fn an_associated_credential_authenticates_however_its_value_is_stored() {
    // (case, whether KMS is mounted to seal the value with)
    let rows: &[(&str, bool)] = &[
        ("a secret sealed under a customer managed key the account holds", true),
        ("a secret whose named key no KMS is mounted to seal with", false),
    ];
    for (case, kms_mounted) in rows {
        let emulator = match kms_mounted {
            true => Emulator::with_secrets_manager_and_kms(),
            false => Emulator::with_secrets_manager(),
        };
        let key = match kms_mounted {
            true => created_key(&emulator, case).await,
            false => UNHELD_KEY_ARN.to_string(),
        };
        let cluster = emulator
            .create_ok(&ClusterRequest {
                client_broker: Some("TLS"),
                sasl_scram: Some(true),
                ..ClusterRequest::named("scram-sealed")
            })
            .await;
        let authority = emulator
            .published_bootstrap(&cluster, "bootstrapBrokerStringSaslScram", case)
            .await;
        given_secret_associated_under_key(
            &emulator,
            &cluster,
            "AmazonMSK_scram_sealed",
            SCRAM_USER,
            SCRAM_PASSWORD,
            &key,
            case,
        )
        .await;

        // The associated user authenticates: the broker read the credential out of the
        // secret, however that secret's value is stored.
        let mut client = Client::tls(case, &authority).await;
        client.api_versions(case).await;
        let code = client
            .authenticate_as(case, SCRAM_SHA_512, SCRAM_USER, SCRAM_PASSWORD)
            .await;
        assert_eq!(
            code, 0,
            "case {case:?}: the associated SCRAM user must authenticate, got error code {code}"
        );

        // And the authenticated connection is served, which is what a client does next.
        let metadata = client.metadata(case).await;
        assert!(
            metadata
                .brokers
                .iter()
                .any(|broker| format!("{}:{}", broker.host.as_str(), broker.port) == authority),
            "case {case:?}: Metadata must report the address the client dialled ({authority})"
        );

        // A user no associated secret names holds no credential here, whatever the
        // password offered.
        let mut other = Client::tls(case, &authority).await;
        other.api_versions(case).await;
        let refused = other
            .authenticate_as(case, SCRAM_SHA_512, "no-such-user", SCRAM_PASSWORD)
            .await;
        assert_eq!(
            refused,
            ::kafka_protocol::ResponseError::SaslAuthenticationFailed.code(),
            "case {case:?}: a user no associated secret names must be refused, got {refused}"
        );
    }
}

/// A customer managed key, minted through KMS's own `CreateKey` — the key a secret's
/// value is then sealed under.
async fn created_key(emulator: &Emulator, case: &str) -> String {
    let created = emulator
        .kms(
            REGION,
            "CreateKey",
            ::serde_json::json!({ "Description": "the key the SCRAM secret is sealed under" }),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: KMS must create the key, got {} {}",
        created.status, created.body
    );
    created
        .at("/KeyMetadata/Arn")
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: CreateKey reported no key ARN"))
        .to_string()
}
