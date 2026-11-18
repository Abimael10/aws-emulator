// SPDX-License-Identifier: CC0-1.0

//! Whose version an `If-Match` is checked against, end to end over HTTP.
//!
//! Every assertion goes through the router, so each case exercises routing, restXml
//! (de)serialization, the handler, the persisted state and serialization — the same path a
//! client takes. The rule under test is the one the modelled `PreconditionFailed` states ("the
//! precondition in one or more of the request fields evaluated to false"): a conditional
//! mutation compares the `If-Match` header against the version token of the ENTITY IT
//! ADDRESSES, so
//!
//! * the entity's own current token — the value the service last returned in that entity's
//!   `ETag` response header — is served (HTTP 204 for these deletes);
//! * a version token this service issued to ANOTHER resource is not this entity's current
//!   version, so it is `PreconditionFailed` (HTTP 412) and the entity is left exactly as it
//!   was, still readable at the same token;
//! * a well-formed token this service never issued is the same refusal, and an absent header
//!   is `InvalidIfMatchVersion` (HTTP 400) — a property of the request alone.
//!
//! The rows span what the rule generalises over: the three resource kinds of one field-level
//! encryption chain (a public key, the profile that encrypts with it and the configuration
//! that maps to that profile), each addressed with every other kind's current token as well as
//! with its own. The chain matters because the tokens are minted from one counter, so a token
//! another resource of the same account holds is exactly the value a caller that mixed up two
//! handles would send — and it names no version of the entity addressed.
//!
//! The wire harness is [`crate::distribution_lifecycle`]'s, and the three creates and their
//! readers are the public key, profile and configuration suites' own helpers — reused rather
//! than copied.

use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{call, emulator, API_VERSION};
use crate::field_level_encryption_config_lifecycle::{create_config, get_config, simple};
use crate::field_level_encryption_profile_lifecycle::{
    create_profile, get_profile, upload_key, Entity, Profile,
};
use crate::public_key_delete::delete_key;
use crate::public_key_lifecycle::get_key;

/// The sentences and codes these refusals carry, spelled out here rather than taken from the
/// crate, so a reworded refusal fails this suite.
const PRECONDITION_FAILED_CODE: &str = "PreconditionFailed";
const INVALID_IF_MATCH_VERSION_CODE: &str = "InvalidIfMatchVersion";

/// A well-formed version token this service never minted: the shape CloudFront writes one in
/// (`E` followed by 13 characters of the identifier alphabet), so what refuses it is the
/// COMPARISON rather than the form.
const UNISSUED_TOKEN: &str = "EZZZZZZZZZZZZ9";

/// One resource of the chain: how it is addressed, read and deleted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    PublicKey,
    Profile,
    Config,
}

impl Kind {
    /// The resource path this kind's delete addresses, under `id`.
    fn path(self, id: &str) -> String {
        match self {
            Self::PublicKey => format!("/{API_VERSION}/public-key/{id}"),
            Self::Profile => format!("/{API_VERSION}/field-level-encryption-profile/{id}"),
            Self::Config => format!("/{API_VERSION}/field-level-encryption/{id}"),
        }
    }
}

/// Every kind, in the order the chain is built.
const KINDS: [Kind; 3] = [Kind::PublicKey, Kind::Profile, Kind::Config];

/// One resource of the chain as it stands: its id and the token the service last returned for
/// it.
#[derive(Debug, Clone)]
struct Held {
    kind: Kind,
    id: String,
    etag: String,
}

/// `DELETE <path>` under the version token `if_match`.
async fn delete(router: &MultiServiceRouter, held: &Held, if_match: Option<&str>) -> crate::distribution_lifecycle::Answer {
    call(router, "DELETE", &held.kind.path(&held.id), None, if_match).await
}

/// The token the service reports for `held` right now, read through that kind's own reader.
async fn current_token(router: &MultiServiceRouter, held: &Held) -> String {
    let answer = match held.kind {
        Kind::PublicKey => get_key(router, &held.id).await,
        Kind::Profile => get_profile(router, &held.id).await,
        Kind::Config => get_config(router, &held.id).await,
    };
    assert_eq!(
        answer.status, 200,
        "case reading {:?} {}: expected HTTP 200, got {} with {}",
        held.kind, held.id, answer.status, answer.raw
    );
    answer.header("etag").to_string()
}

/// Build the chain: a public key, a profile that encrypts with it, and a configuration that
/// maps a query argument to that profile.
async fn chain(router: &MultiServiceRouter) -> Vec<Held> {
    let key_id = upload_key(router, 0).await;
    let key_etag = get_key(router, &key_id).await.header("etag").to_string();

    let row = Profile::new(
        "the profile of the chain",
        "if-match-chain-profile",
        None,
        vec![Entity::new(&key_id, "provider-one", &["CreditCard"])],
    );
    let created = create_profile(router, &row.xml()).await;
    assert_eq!(
        created.status, 201,
        "case creating the profile: expected HTTP 201, got {} with {}",
        created.status, created.raw
    );
    let profile_id = created
        .root()
        .text_at("Id")
        .unwrap_or_default()
        .to_string();
    let profile_etag = created.header("etag").to_string();

    let config = simple("the configuration of the chain", "if-match-chain-config", &profile_id);
    let created = create_config(router, &config.xml()).await;
    assert_eq!(
        created.status, 201,
        "case creating the configuration: expected HTTP 201, got {} with {}",
        created.status, created.raw
    );
    let config_id = created
        .root()
        .text_at("Id")
        .unwrap_or_default()
        .to_string();
    let config_etag = created.header("etag").to_string();

    vec![
        Held {
            kind: Kind::PublicKey,
            id: key_id,
            etag: key_etag,
        },
        Held {
            kind: Kind::Profile,
            id: profile_id,
            etag: profile_etag,
        },
        Held {
            kind: Kind::Config,
            id: config_id,
            etag: config_etag,
        },
    ]
}

/// RULE: the token a conditional delete is checked against is the ADDRESSED entity's own, so a
/// token this service issued to another resource of the same account — or one it never issued
/// at all — is a failed precondition, and the entity it was aimed at is left exactly as it was.
#[tokio::test]
async fn a_version_token_of_another_resource_is_a_failed_precondition() {
    let router = emulator();
    let held = chain(&router).await;

    for addressed in &held {
        // Every OTHER resource's current token, plus a token nothing was ever issued.
        let mut foreign: Vec<(String, String)> = held
            .iter()
            .filter(|other| other.kind != addressed.kind)
            .map(|other| (format!("{:?}'s own token", other.kind), other.etag.clone()))
            .collect();
        foreign.push((
            "a token this service never issued".to_string(),
            UNISSUED_TOKEN.to_string(),
        ));

        for (why, token) in foreign {
            let case = format!("{:?} addressed with {why}", addressed.kind);
            let answer = delete(&router, addressed, Some(&token)).await;
            assert_eq!(
                answer.status, 412,
                "case {case}: expected HTTP 412, got {} with {}",
                answer.status, answer.raw
            );
            assert_eq!(
                answer.error_code(),
                PRECONDITION_FAILED_CODE,
                "case {case}: expected Code {PRECONDITION_FAILED_CODE}, got {:?}",
                answer.error_code()
            );
            // The refused delete left the entity where it was, still at its own token.
            let reported = current_token(&router, addressed).await;
            assert_eq!(
                reported, addressed.etag,
                "case {case}: expected the entity to keep its version {:?}, got {reported:?}",
                addressed.etag
            );
        }

        // An absent header is the OTHER refusal: a property of the request alone.
        let case = format!("{:?} addressed with no If-Match at all", addressed.kind);
        let answer = delete(&router, addressed, None).await;
        assert_eq!(
            answer.status, 400,
            "case {case}: expected HTTP 400, got {} with {}",
            answer.status, answer.raw
        );
        assert_eq!(
            answer.error_code(),
            INVALID_IF_MATCH_VERSION_CODE,
            "case {case}: expected Code {INVALID_IF_MATCH_VERSION_CODE}, got {:?}",
            answer.error_code()
        );
    }
}

/// RULE: the entity's OWN current token — the one the service last returned for it — is served.
///
/// Walked over the whole chain in the order the references allow, so each kind is deleted under
/// the token its own create reported: the configuration that maps to the profile, then the
/// profile that encrypts with the key, then the key.
#[tokio::test]
async fn each_entitys_own_version_deletes_it() {
    let router = emulator();
    let held = chain(&router).await;

    // The token each kind reports is the one its create handed back, whichever kind was read
    // last: a read of one entity never changes another's version.
    for one in &held {
        let reported = current_token(&router, one).await;
        assert_eq!(
            reported, one.etag,
            "case {:?}: expected the create's version {:?} to be current, got {reported:?}",
            one.kind, one.etag
        );
    }

    for kind in KINDS.iter().rev() {
        let one = held
            .iter()
            .find(|held| held.kind == *kind)
            .expect("the chain holds every kind");
        let answer = if one.kind == Kind::PublicKey {
            // Over the public key suite's own delete path, so the two suites drive one wire
            // path rather than each carrying a copy of it.
            delete_key(&router, &one.id, Some(&one.etag)).await
        } else {
            delete(&router, one, Some(&one.etag)).await
        };
        assert_eq!(
            answer.status, 204,
            "case {:?} deleted under its own version: expected HTTP 204, got {} with {}",
            one.kind, answer.status, answer.raw
        );
        assert!(
            answer.raw.trim().is_empty(),
            "case {:?} deleted under its own version: expected an empty body, got {}",
            one.kind,
            answer.raw
        );
    }
}
