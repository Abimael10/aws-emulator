// SPDX-License-Identifier: CC0-1.0

//! The `Location` URI the RESOURCE creates of this surface announce, end to end over HTTP.
//!
//! RULE: a create's `Location` is an address the CALLER then follows, so it is rooted at the
//! endpoint this emulator answered that create on — scheme, host and port — with the new
//! resource's own path beneath it. A request that reached the emulator's own endpoint is
//! answered that same address, port included, since that is the only address a client on this
//! machine can dial; a caller that resolves AWS's own hostnames (a request addressed to one of
//! this service's published hosts, on an emulator served at no address of its own) is answered
//! this service's published endpoint, which is the address such a caller already resolves. A
//! URI rooted anywhere else sends the client that follows it away from this emulator, where
//! the resource it names does not exist.
//!
//! One rule, every create on this surface that binds an output member to the `Location` header
//! (each one's `Location` is an `httpHeader` and its route answers `201`:
//! `smithy_query(query="operation",operation="CreatePublicKey,…")`). The sibling suite
//! [`crate::announced_location_uri`] drives the distribution, origin access identity, response
//! headers policy and invalidation creates; this one drives the NINE resource creates beside
//! them, which differ in exactly what the rule generalises over:
//!
//! * which identifier form the announced path ends at — a `K`-prefixed public key token, an
//!   `E`-prefixed identity, access control or field-level encryption token, a minted UUID for
//!   the policies, and the whole ARN for a key value store, which is how that surface names a
//!   store;
//! * whether the create's own document is the resource configuration (`PublicKeyConfig`,
//!   `KeyGroupConfig`, …) or the request shape itself (`CreateKeyValueStoreRequest`);
//! * whether the new resource is addressed by a path a read answers on at all (a key value
//!   store's read is by `Name`, the model's own asymmetry);
//! * and the host the request ADDRESSED: the emulator's own endpoint as a loopback literal, as
//!   a loopback name on a port of its own and as an address carrying no port, against this
//!   service's global published host and its regional spelling.
//!
//! Each announced URI is then FOLLOWED on the host it was announced to, which is the whole
//! point of the header: the resource the create just made answers there.
//!
//! The harness (the router, the request helper that takes the `Host` a request carries, and the
//! element scanner) is [`crate::distribution_lifecycle`]'s, the hosts and the identifier reader
//! are the sibling suite's own table and reader, and each document is written by the writer its
//! own suite already states, so this suite states no second spelling of anything it shares.

use smithy_server_runtime::MultiServiceRouter;

use crate::announced_location_uri::{
    addressed_hosts, created_id, origin_access_identity_xml, Addressed, AddressedHosts,
};
use crate::distribution_lifecycle::{call_from_host, emulator, Answer, API_VERSION};
use crate::field_level_encryption_config_lifecycle::simple;
use crate::field_level_encryption_profile_lifecycle::{Entity, Profile};
use crate::key_group_lifecycle::Group;
use crate::key_value_store_lifecycle::Create as StoreCreate;
use crate::public_key_lifecycle::{pem, Key};

/// The hosts every create below is issued from: the table the sibling suite already states
/// (the emulator's own endpoint as a loopback literal and as a loopback name, each on a port of
/// its own, against this service's global published host and its regional spelling), and beside
/// them an own endpoint carrying NO port, which is the one form that table does not hold.
fn rows() -> AddressedHosts {
    let mut rows = addressed_hosts();
    rows.rows.push(Addressed {
        case: "the emulator's own endpoint carrying no port",
        host: "127.0.0.1".to_string(),
        authority: "127.0.0.1".to_string(),
        own_endpoint: true,
    });
    rows
}

/// One create under test, and what it answered.
struct Case {
    /// Names the row in every assertion below.
    case: String,
    /// The collection path the new resource lives under, between the API version and the
    /// identifier — which is also the path the create itself was posted to.
    collection: &'static str,
    /// The member of the created resource's payload that carries the identifier the announced
    /// path ends at: `ARN` for a key value store, `Id` for the rest.
    identifier_member: &'static str,
    /// Whether the announced path is also the route the resource is read back on. It is for
    /// every create here except `CreateKeyValueStore`, whose read is addressed by `Name`.
    readable: bool,
    /// A value the REQUEST supplied — a name or a caller reference — which the announced path
    /// must NOT address the resource by.
    caller_input: String,
    answer: Answer,
}

/// `POST /2020-05-31/{collection}` with `body`, addressed to `host`.
async fn create(router: &MultiServiceRouter, host: &str, collection: &str, body: &str) -> Answer {
    call_from_host(
        router,
        host,
        "POST",
        &format!("/{API_VERSION}/{collection}"),
        Some(body),
        &[],
    )
    .await
}

/// The text of the member `name` the answer reports, which is what the announced path has to
/// end at.
fn member(answer: &Answer, name: &str, case: &str) -> String {
    answer
        .root()
        .find_text(name)
        .unwrap_or_else(|| panic!("case {case}: the answer carries no {name}: {}", answer.raw))
        .to_string()
}

/// A minimal `CachePolicyConfig`: `Name` and `MinTTL` are its required members
/// (`smithy_query(query="shape",shape="CachePolicyConfig")`).
fn cache_policy_body(row: usize) -> String {
    format!(
        "<CachePolicyConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <Name>location-cache-policy-{row}</Name>\
         <MinTTL>{row}</MinTTL>\
         </CachePolicyConfig>"
    )
}

/// A minimal `OriginRequestPolicyConfig`: `Name`, `HeadersConfig`, `CookiesConfig` and
/// `QueryStringsConfig` are required, and each of the three sections carries only its own
/// required behaviour member, whose `none` is a value of its enum
/// (`smithy_query(query="shape",shape="OriginRequestPolicyConfig")`).
fn origin_request_policy_body(row: usize) -> String {
    format!(
        "<OriginRequestPolicyConfig \
         xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <Name>location-origin-request-policy-{row}</Name>\
         <HeadersConfig><HeaderBehavior>none</HeaderBehavior></HeadersConfig>\
         <CookiesConfig><CookieBehavior>none</CookieBehavior></CookiesConfig>\
         <QueryStringsConfig><QueryStringBehavior>none</QueryStringBehavior></QueryStringsConfig>\
         </OriginRequestPolicyConfig>"
    )
}

/// A minimal `OriginAccessControlConfig`: every member but `Description` is required, and the
/// three enum values below are ones their enums admit
/// (`smithy_query(query="shape",shape="OriginAccessControlConfig,OriginAccessControlOriginTypes")`).
fn origin_access_control_body(row: usize) -> String {
    format!(
        "<OriginAccessControlConfig \
         xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <Name>location-origin-access-control-{row}</Name>\
         <SigningProtocol>sigv4</SigningProtocol>\
         <SigningBehavior>always</SigningBehavior>\
         <OriginAccessControlOriginType>s3</OriginAccessControlOriginType>\
         </OriginAccessControlConfig>"
    )
}

/// Every resource create of this surface, issued once from `host` under identifiers of its own
/// so no two rows collide on a name or a caller reference.
///
/// The creates are issued in dependency order: a key group names a public key that must already
/// be stored, a field-level encryption profile encrypts with one, and a field-level encryption
/// configuration maps a query argument onto a profile — so each prerequisite is itself one of
/// the creates under test rather than an identifier invented here.
async fn creates(router: &MultiServiceRouter, row: usize, addressed: &Addressed) -> Vec<Case> {
    let host = addressed.host.as_str();
    let named = |operation: &str| format!("{} / {operation}", addressed.case);
    let mut cases: Vec<Case> = Vec::new();

    let public_key = Key {
        case: named("CreatePublicKey"),
        caller_reference: format!("location-public-key-{row}"),
        name: format!("location-public-key-{row}"),
        encoded_key: pem(1 + row % 3, char::from(b'A' + u8::try_from(row % 26).unwrap_or(0))),
        comment: None,
    };
    let answer = create(router, host, "public-key", &public_key.xml()).await;
    let public_key_id = created_id(&answer, &public_key.case);
    cases.push(Case {
        case: public_key.case.clone(),
        collection: "public-key",
        identifier_member: "Id",
        readable: true,
        caller_input: public_key.name.clone(),
        answer,
    });

    let group = Group {
        case: named("CreateKeyGroup"),
        name: format!("location-key-group-{row}"),
        items: vec![public_key_id.clone()],
        comment: None,
    };
    cases.push(Case {
        case: group.case.clone(),
        collection: "key-group",
        identifier_member: "Id",
        readable: true,
        caller_input: group.name.clone(),
        answer: create(router, host, "key-group", &group.xml()).await,
    });

    let profile = Profile::new(
        &named("CreateFieldLevelEncryptionProfile"),
        &format!("location-fle-profile-{row}"),
        None,
        vec![Entity::new(&public_key_id, "provider-one", &["CreditCard"])],
    );
    let answer = create(
        router,
        host,
        "field-level-encryption-profile",
        &profile.xml(),
    )
    .await;
    let profile_id = created_id(&answer, &profile.case);
    cases.push(Case {
        case: profile.case.clone(),
        collection: "field-level-encryption-profile",
        identifier_member: "Id",
        readable: true,
        caller_input: profile.caller_reference.clone(),
        answer,
    });

    let config = simple(
        &named("CreateFieldLevelEncryptionConfig"),
        &format!("location-fle-config-{row}"),
        &profile_id,
    );
    cases.push(Case {
        case: config.case.clone(),
        collection: "field-level-encryption",
        identifier_member: "Id",
        readable: true,
        caller_input: config.caller_reference.clone(),
        answer: create(router, host, "field-level-encryption", &config.xml()).await,
    });

    cases.push(Case {
        case: named("CreateCachePolicy"),
        collection: "cache-policy",
        identifier_member: "Id",
        readable: true,
        caller_input: format!("location-cache-policy-{row}"),
        answer: create(router, host, "cache-policy", &cache_policy_body(row)).await,
    });

    cases.push(Case {
        case: named("CreateOriginRequestPolicy"),
        collection: "origin-request-policy",
        identifier_member: "Id",
        readable: true,
        caller_input: format!("location-origin-request-policy-{row}"),
        answer: create(
            router,
            host,
            "origin-request-policy",
            &origin_request_policy_body(row),
        )
        .await,
    });

    cases.push(Case {
        case: named("CreateOriginAccessControl"),
        collection: "origin-access-control",
        identifier_member: "Id",
        readable: true,
        caller_input: format!("location-origin-access-control-{row}"),
        answer: create(
            router,
            host,
            "origin-access-control",
            &origin_access_control_body(row),
        )
        .await,
    });

    let identity_reference = format!("location-identity-{row}");
    cases.push(Case {
        case: named("CreateCloudFrontOriginAccessIdentity"),
        collection: "origin-access-identity/cloudfront",
        identifier_member: "Id",
        readable: true,
        caller_input: identity_reference.clone(),
        answer: create(
            router,
            host,
            "origin-access-identity/cloudfront",
            &origin_access_identity_xml(&identity_reference),
        )
        .await,
    });

    // The key value store is announced by its whole ARN, which is how that surface names a
    // store, and its read is addressed by `Name` instead — so this row's announced path is
    // asserted but not followed.
    let store = StoreCreate {
        case: "the announced location",
        name: format!("location-store-{row}"),
        comment: None,
        tags: Vec::new(),
    };
    cases.push(Case {
        case: named("CreateKeyValueStore"),
        collection: "key-value-store",
        identifier_member: "ARN",
        readable: false,
        caller_input: store.name.clone(),
        answer: create(router, host, "key-value-store", &store.xml()).await,
    });

    cases
}

/// Every resource create announces its new resource at the endpoint the request reached, and
/// nowhere else: the authority is the one the request addressed when that host is the
/// emulator's own — port included — and this service's published endpoint for a caller that
/// resolves AWS's own hostnames. The path beneath it is the resource's own, ending at the
/// identifier the SERVICE generated and reported in the same response, and following it on the
/// host it was announced to reaches the resource the create just made.
#[tokio::test]
async fn every_resource_create_announces_the_endpoint_the_request_reached() {
    let router = emulator();
    // Held for the whole test, as the sibling suite's table is: the ports the own-endpoint rows
    // address stay this process's own, so no listener this emulator opens takes one.
    let addressed_hosts = rows();

    for (row, addressed) in addressed_hosts.rows.iter().enumerate() {
        for Case {
            case,
            collection,
            identifier_member,
            readable,
            caller_input,
            answer,
        } in creates(&router, row, addressed).await
        {
            assert_eq!(
                answer.status, 201,
                "case {case}: expected the create to be accepted with HTTP 201, got {} with {}",
                answer.status, answer.raw
            );

            let identifier = member(&answer, identifier_member, &case);
            let id = created_id(&answer, &case);
            let path = format!("/{API_VERSION}/{collection}/{identifier}");
            let location = answer.header("location").to_string();
            assert_eq!(
                location,
                format!("https://{}{path}", addressed.authority),
                "case {case}: expected the new resource's path rooted at the endpoint the \
                 request reached ({}), got {location:?}",
                addressed.authority
            );
            // Whichever identifier form the resource is addressed by, the announced path ends
            // at the identifier the service GENERATED — a store's ARN ends with its own id —
            // and never at a value the request supplied.
            assert!(
                location.ends_with(&format!("/{id}")),
                "case {case}: expected the announced URI to end at the generated Id {id:?}, got \
                 {location:?}"
            );
            let segment = location.rsplit('/').next().unwrap_or_default().to_string();
            assert_ne!(
                segment, caller_input,
                "case {case}: the announced URI must not address the resource by the request's \
                 own value {caller_input:?}"
            );

            // Where the resource's read route is that path, FOLLOWING the header reaches the
            // resource the create just made — on the very host it was announced to, which is
            // what makes the announced address usable rather than merely well formed.
            if readable {
                let followed =
                    call_from_host(&router, &addressed.host, "GET", &path, None, &[]).await;
                assert_eq!(
                    followed.status, 200,
                    "case {case}: following {location:?} must reach the resource, got {} with {}",
                    followed.status, followed.raw
                );
                let read_id = member(&followed, "Id", &case);
                assert_eq!(
                    read_id, id,
                    "case {case}: following the announced URI must reach the created resource \
                     {id:?}, got {read_id:?}"
                );
            }
        }
    }
}
