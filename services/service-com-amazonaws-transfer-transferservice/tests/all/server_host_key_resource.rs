// SPDX-License-Identifier: CC0-1.0

//! A server's ACTIVE host key is one of that server's host-key resources.
//!
//! RULE: the key whose fingerprint `DescribeServer` reports as the server's own is a
//! host key of that server, so `ListHostKeys` enumerates it and `DescribeHostKey`
//! reports it — whether the service GENERATED it for a server created without one (an
//! RSA key, as the service's own documented generation command writes), or the create
//! supplied it, or an update rotated it. A server therefore never reports a fingerprint
//! none of its host keys has, and every key it holds reports the algorithm of its OWN
//! material: a reader of a generated key sees `ssh-rsa`, a reader of a supplied one sees
//! what that material is.
//!
//! The rows differ along what sets that key: a create that supplies none (the service
//! generates it), a create that supplies one, and an update that rotates it — and, for
//! the supplied cases, along the key material itself. A key a caller IMPORTS beside it is
//! [`crate::host_key_lifecycle`]'s subject; what this suite asserts is that the server's
//! own key is in the same collection, under its own algorithm.
//!
//! SECOND RULE, on the VALUE every one of those readers reports: a fingerprint is
//! `SHA256:` followed by the Base64 encoding of the SHA-256 digest of the key it
//! identifies — see
//! [`a_reported_fingerprint_is_the_keys_sha256_digest_in_base64`].

use serde_json::{json, Value};

use crate::harness::{
    generated_host_key, host_key_algorithm, is_modelled_host_key_id, member_of, Emulator, ACCOUNT,
    REGION,
};
use crate::host_key_listing::walk_host_keys;

/// The algorithm names `DescribedHostKey.Type` documents (source:
/// `smithy_query(query="shape",shape="DescribedHostKey")`, member `Type`): "ssh-rsa,
/// ssh-ed25519, ecdsa-sha2-nistp256, ecdsa-sha2-nistp384, ecdsa-sha2-nistp521".
const DOCUMENTED_HOST_KEY_TYPES: [&str; 5] = [
    "ssh-rsa",
    "ssh-ed25519",
    "ecdsa-sha2-nistp256",
    "ecdsa-sha2-nistp384",
    "ecdsa-sha2-nistp521",
];

/// The algorithm of the key the SERVICE generates for a server created without one:
/// `CreateServer`'s `HostKey` documents `ssh-keygen -t rsa -b 2048 -N "" -m PEM` as the
/// way to make a host key for a server, and the key the service makes for itself is one
/// of those.
const GENERATED_HOST_KEY_TYPE: &str = "ssh-rsa";

/// The fingerprint a server reports as its own.
async fn active_fingerprint(emulator: &Emulator, case: &str, server_id: &str) -> String {
    let answer = emulator
        .call("DescribeServer", json!({"ServerId": server_id}))
        .await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected DescribeServer 200, got {} with body {}",
        answer.status, answer.body
    );
    let server = answer.get("Server").clone();
    member_of(&server, "HostKeyFingerprint")
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected the server to report a host key fingerprint, got {server}")
        })
        .to_string()
}

/// The listed host key of `server_id` whose fingerprint is `fingerprint`, which the rule
/// says must exist exactly once.
async fn listed_with_fingerprint(
    emulator: &Emulator,
    case: &str,
    server_id: &str,
    fingerprint: &str,
) -> Value {
    let listed = walk_host_keys(emulator, server_id, None).await;
    let matching: Vec<&Value> = listed
        .iter()
        .filter(|key| member_of(key, "Fingerprint").as_str() == Some(fingerprint))
        .collect();
    assert_eq!(
        matching.len(),
        1,
        "case {case:?}: expected exactly one listed host key with the server's active \
         fingerprint {fingerprint:?}, got {listed:?}"
    );
    matching[0].clone()
}

/// Assert the whole rule on one server: its active fingerprint is listed among its host
/// keys, under a modelled `HostKeyId`, ARNed under the server, with a documented
/// algorithm `Type`, and `DescribeHostKey` reports the same record.
async fn assert_active_key_is_a_resource(
    emulator: &Emulator,
    case: &str,
    server_id: &str,
    expected_type: &str,
) -> String {
    let fingerprint = active_fingerprint(emulator, case, server_id).await;
    let entry = listed_with_fingerprint(emulator, case, server_id, &fingerprint).await;
    let host_key_id = member_of(&entry, "HostKeyId")
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a HostKeyId, got {entry}"))
        .to_string();
    assert!(
        is_modelled_host_key_id(&host_key_id),
        "case {case:?}: expected a HostKeyId matching ^hostkey-[0-9a-f]{{17}}$, got \
         {host_key_id:?}"
    );
    assert_eq!(
        member_of(&entry, "Arn").as_str(),
        Some(
            format!("arn:aws:transfer:{REGION}:{ACCOUNT}:host-key/{server_id}/{host_key_id}")
                .as_str()
        ),
        "case {case:?}: expected the host key ARNed under its server, got {entry}"
    );
    let reported_type = member_of(&entry, "Type")
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a Type, got {entry}"))
        .to_string();
    assert!(
        DOCUMENTED_HOST_KEY_TYPES.contains(&reported_type.as_str()),
        "case {case:?}: expected one of the documented host key types \
         {DOCUMENTED_HOST_KEY_TYPES:?}, got {reported_type:?}"
    );
    assert_eq!(
        reported_type, expected_type,
        "case {case:?}: expected the key's own algorithm {expected_type:?}, got \
         {reported_type:?}"
    );

    // The same record, read through the other reader.
    let described = emulator
        .call(
            "DescribeHostKey",
            json!({"ServerId": server_id, "HostKeyId": &host_key_id}),
        )
        .await;
    assert_eq!(
        described.status, 200,
        "case {case:?}: expected DescribeHostKey 200 for the server's own key, got {} with \
         body {}",
        described.status, described.body
    );
    let record = described.get("HostKey").clone();
    assert_eq!(
        member_of(&record, "HostKeyFingerprint").as_str(),
        Some(fingerprint.as_str()),
        "case {case:?}: expected the described key to report the active fingerprint, got {record}"
    );
    assert_eq!(
        member_of(&record, "Type").as_str(),
        Some(expected_type),
        "case {case:?}: expected both readers to report the same algorithm, got {record}"
    );
    assert!(
        member_of(&record, "DateImported").is_number(),
        "case {case:?}: expected a DateImported on the server's own key, got {record}"
    );
    host_key_id
}

/// RULE (see the module docs): every server's active host key is one of its host keys,
/// whichever call set it, and it reports the algorithm of its own material.
///
/// The rows differ along what set the key: a create carrying no `HostKey` at all (the
/// service generates an RSA one), and creates carrying two different generated keys.
#[tokio::test]
async fn a_servers_active_host_key_is_one_of_its_host_keys() {
    let emulator = Emulator::new();

    // (case, the CreateServer request, the algorithm the row expects back, the
    // fingerprint it expects when the request supplied the material)
    let (first_material, first_fingerprint) = generated_host_key();
    let (second_material, second_fingerprint) = generated_host_key();
    let rows: Vec<(&str, Value, String, Option<String>)> = vec![
        (
            "a server created with no host key at all",
            json!({}),
            GENERATED_HOST_KEY_TYPE.to_string(),
            None,
        ),
        (
            "a server created with no host key and an explicit SFTP configuration",
            json!({"EndpointType": "PUBLIC", "IdentityProviderType": "SERVICE_MANAGED",
                   "Protocols": ["SFTP"]}),
            GENERATED_HOST_KEY_TYPE.to_string(),
            None,
        ),
        (
            "a server created with supplied host key material",
            json!({"HostKey": &first_material}),
            host_key_algorithm(&first_material),
            Some(first_fingerprint.clone()),
        ),
        (
            "a server created with other supplied host key material",
            json!({"HostKey": &second_material}),
            host_key_algorithm(&second_material),
            Some(second_fingerprint.clone()),
        ),
    ];

    for (case, request, expected_type, expected_fingerprint) in rows {
        let server_id = emulator.create_server(request).await;
        if let Some(expected) = expected_fingerprint {
            let reported = active_fingerprint(&emulator, case, &server_id).await;
            assert_eq!(
                reported, expected,
                "case {case:?}: expected the server to report the supplied key's own \
                 fingerprint {expected:?}, got {reported:?}"
            );
        }
        assert_active_key_is_a_resource(&emulator, case, &server_id, &expected_type).await;

        // The server's own key is the ONLY key it holds before any import: materialising
        // it adds one record, not a second copy of it.
        let listed = walk_host_keys(&emulator, &server_id, None).await;
        assert_eq!(
            listed.len(),
            1,
            "case {case:?}: expected the server's own host key alone before any import, \
             got {listed:?}"
        );
    }
}

/// How many characters the standard Base64 encoding of a 32-byte SHA-256 digest is:
/// four characters per three bytes, so eleven whole groups and a final group of two
/// bytes completed by one `=` pad.
const BASE64_SHA256_DIGEST_LENGTH: usize = 44;

/// Assert that `value` is a fingerprint in the form the service reports one: the literal
/// `SHA256:` followed by the Base64 encoding of a SHA-256 digest — 44 characters, the
/// last of them the pad that completes the final group, decoding to exactly 32 bytes.
///
/// Used for the key the SERVICE generates, whose material the caller never sees and
/// whose digest can therefore only be checked as a digest; every supplied key's
/// fingerprint is asserted against the text computed from that key's own bytes. Shared
/// with [`crate::server_config_and_state`], so one check decides the form.
pub(crate) fn assert_fingerprint_form(case: &str, value: &str) {
    use base64::Engine as _;

    let digest = value.strip_prefix("SHA256:").unwrap_or_else(|| {
        panic!("case {case:?}: expected a SHA256: fingerprint, got {value:?}")
    });
    assert_eq!(
        digest.len(),
        BASE64_SHA256_DIGEST_LENGTH,
        "case {case:?}: expected {BASE64_SHA256_DIGEST_LENGTH} Base64 characters of \
         SHA-256 digest, got {value:?}"
    );
    assert!(
        digest.ends_with('='),
        "case {case:?}: expected the padded Base64 encoding of the digest, got {value:?}"
    );
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(digest)
        .unwrap_or_else(|error| {
            panic!("case {case:?}: expected standard Base64, got {value:?} ({error})")
        });
    assert_eq!(
        decoded.len(),
        32,
        "case {case:?}: expected a 32-byte SHA-256 digest, got {value:?} ({} bytes)",
        decoded.len()
    );
}

/// RULE: a fingerprint this service reports for a key is `SHA256:` followed by the
/// Base64 encoding of the SHA-256 digest of that key — so it is the key's own digest, in
/// that one rendering, whichever member reports it and whichever call put the key on the
/// server.
///
/// The rows differ along the key whose digest is reported (the one the service generates,
/// two different supplied keys, an imported one — so the digest differs in every row) and
/// along the call that put it there (a create, an update's rotation, an import). Each
/// supplied key's expectation is the text computed from that key's own bytes by a hash
/// crate and a Base64 crate (`harness::reported_fingerprint`), independently of the
/// handlers under test; the generated key's material is the service's own, so its
/// fingerprint is held to the form a digest in this rendering has.
#[tokio::test]
async fn a_reported_fingerprint_is_the_keys_sha256_digest_in_base64() {
    let emulator = Emulator::new();

    // Row 1: the key the SERVICE generates for a server created without one. Its
    // material is never handed to the caller, so what is asserted is that the value is a
    // SHA-256 digest in this rendering, and that all three readers report that one value.
    let generated_server = emulator.create_server(json!({})).await;
    let reported = active_fingerprint(&emulator, "a generated host key", &generated_server).await;
    assert_fingerprint_form("a generated host key", &reported);
    assert_active_key_is_a_resource(
        &emulator,
        "a generated host key",
        &generated_server,
        GENERATED_HOST_KEY_TYPE,
    )
    .await;

    // Rows 2 and 3: supplied material, whose digest the test computes itself — once
    // through a create, once through the update that rotates a server onto it.
    let (created_material, created_fingerprint) = generated_host_key();
    let (rotated_material, rotated_fingerprint) = generated_host_key();
    let supplied_server = emulator
        .create_server(json!({"HostKey": &created_material}))
        .await;
    let rotated_server = emulator.create_server(json!({})).await;
    let rotation = emulator
        .call(
            "UpdateServer",
            json!({"ServerId": &rotated_server, "HostKey": &rotated_material}),
        )
        .await;
    assert_eq!(
        rotation.status, 200,
        "case \"a key supplied by a rotation\": expected UpdateServer 200, got {} with \
         body {}",
        rotation.status, rotation.body
    );

    for (case, server_id, expected) in [
        (
            "a key supplied at create",
            &supplied_server,
            &created_fingerprint,
        ),
        (
            "a key supplied by a rotation",
            &rotated_server,
            &rotated_fingerprint,
        ),
    ] {
        assert_fingerprint_form(case, expected);
        let reported = active_fingerprint(&emulator, case, server_id).await;
        assert_eq!(
            &reported, expected,
            "case {case:?}: expected the supplied key's own digest {expected:?}, got \
             {reported:?}"
        );
        // ...and the same text from the host-key readers of that server.
        let entry = listed_with_fingerprint(&emulator, case, server_id, &reported).await;
        let host_key_id = member_of(&entry, "HostKeyId")
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected a HostKeyId, got {entry}"))
            .to_string();
        let described = emulator
            .call(
                "DescribeHostKey",
                json!({"ServerId": server_id, "HostKeyId": &host_key_id}),
            )
            .await;
        assert_eq!(
            member_of(described.get("HostKey"), "HostKeyFingerprint").as_str(),
            Some(expected.as_str()),
            "case {case:?}: expected the described key to report the same digest, got {}",
            described.body
        );
    }

    // Row 4: a key an IMPORT adds beside the server's own reports its own digest in the
    // same rendering, so a caller comparing the two reads one text per key.
    let (imported_material, imported_fingerprint) = generated_host_key();
    let imported = emulator
        .call(
            "ImportHostKey",
            json!({"ServerId": &generated_server, "HostKeyBody": &imported_material}),
        )
        .await;
    assert_eq!(
        imported.status, 200,
        "case \"an imported key\": expected ImportHostKey 200, got {} with body {}",
        imported.status, imported.body
    );
    let imported_id = imported
        .get("HostKeyId")
        .as_str()
        .expect("an accepted import carries its HostKeyId")
        .to_string();
    let described = emulator
        .call(
            "DescribeHostKey",
            json!({"ServerId": &generated_server, "HostKeyId": &imported_id}),
        )
        .await;
    assert_eq!(
        member_of(described.get("HostKey"), "HostKeyFingerprint").as_str(),
        Some(imported_fingerprint.as_str()),
        "case \"an imported key\": expected the imported key's own digest \
         {imported_fingerprint:?}, got {}",
        described.body
    );
    let entry = listed_with_fingerprint(
        &emulator,
        "an imported key",
        &generated_server,
        &imported_fingerprint,
    )
    .await;
    assert_eq!(
        member_of(&entry, "HostKeyId").as_str(),
        Some(imported_id.as_str()),
        "case \"an imported key\": expected the listed entry to be the imported key, got \
         {entry}"
    );
    assert_ne!(
        imported_fingerprint, reported,
        "case \"an imported key\": expected a digest of its own, not the generated key's"
    );
}

/// RULE: a server's key set is BOUNDED, and a rotation is one more key in it — so a
/// rotation a server has no room for is refused, and refused before anything is written:
/// the server keeps the key it was serving with.
#[tokio::test]
async fn a_rotation_a_server_has_no_room_for_is_refused() {
    const BOUND: usize =
        service_com_amazonaws_transfer_transferservice::state::MAX_HOST_KEYS_PER_SERVER;

    let emulator = Emulator::new();
    let server_id = emulator.create_server(json!({})).await;

    // Fill the server to its bound: it already holds the key it serves with, so the
    // room left is read from the store rather than assumed.
    let held = walk_host_keys(&emulator, &server_id, None).await.len();
    for index in held..BOUND {
        let (material, _) = generated_host_key();
        let case = format!("filling key {index}");
        let answer = emulator
            .call(
                "ImportHostKey",
                json!({"ServerId": &server_id, "HostKeyBody": material}),
            )
            .await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected the import to be accepted, got {} with body {}",
            answer.status, answer.body
        );
    }

    let serving = active_fingerprint(&emulator, "at the bound", &server_id).await;
    let (material, _) = generated_host_key();
    let refused = emulator
        .call(
            "UpdateServer",
            json!({"ServerId": &server_id, "HostKey": &material}),
        )
        .await;
    assert_eq!(
        (refused.status, refused.error_code().as_str()),
        (400, "InvalidRequestException"),
        "case \"a rotation past the bound\": expected 400 InvalidRequestException, got {} \
         with body {}",
        refused.status,
        refused.body
    );
    assert_eq!(
        active_fingerprint(&emulator, "after the refusal", &server_id).await,
        serving,
        "case \"a rotation past the bound\": expected the refused rotation to leave the \
         server serving with the key it had"
    );
    let listed = walk_host_keys(&emulator, &server_id, None).await;
    assert_eq!(
        listed.len(),
        BOUND,
        "case \"a rotation past the bound\": expected the refused rotation to store no key, \
         got {listed:?}"
    );

    // Freeing one slot makes the same rotation acceptable.
    let freed = member_of(&listed[0], "HostKeyId")
        .as_str()
        .expect("a listed key carries its id")
        .to_string();
    let deleted = emulator
        .call(
            "DeleteHostKey",
            json!({"ServerId": &server_id, "HostKeyId": &freed}),
        )
        .await;
    assert_eq!(
        deleted.status, 200,
        "expected DeleteHostKey 200, got {} with body {}",
        deleted.status, deleted.body
    );
    let accepted = emulator
        .call(
            "UpdateServer",
            json!({"ServerId": &server_id, "HostKey": &material}),
        )
        .await;
    assert_eq!(
        accepted.status, 200,
        "case \"the refused rotation after a delete\": expected UpdateServer 200, got {} \
         with body {}",
        accepted.status,
        accepted.body
    );
    assert_active_key_is_a_resource(
        &emulator,
        "the refused rotation after a delete",
        &server_id,
        &host_key_algorithm(&material),
    )
    .await;
}

/// RULE: an `UpdateServer` that rotates the host key leaves the invariant standing — the
/// server reports the new key's fingerprint and that key is among its host keys, beside
/// the one it was serving with before.
#[tokio::test]
async fn rotating_a_servers_host_key_keeps_it_among_its_host_keys() {
    let emulator = Emulator::new();
    let server_id = emulator.create_server(json!({})).await;
    let generated = assert_active_key_is_a_resource(
        &emulator,
        "the generated key before any rotation",
        &server_id,
        GENERATED_HOST_KEY_TYPE,
    )
    .await;

    for case in ["the first rotation", "a second rotation"] {
        let (material, fingerprint) = generated_host_key();
        let updated = emulator
            .call(
                "UpdateServer",
                json!({"ServerId": &server_id, "HostKey": &material}),
            )
            .await;
        assert_eq!(
            updated.status, 200,
            "case {case:?}: expected UpdateServer 200, got {} with body {}",
            updated.status, updated.body
        );

        let reported = active_fingerprint(&emulator, case, &server_id).await;
        assert_eq!(
            reported, fingerprint,
            "case {case:?}: expected the server to report the rotated key's fingerprint \
             {fingerprint:?}, got {reported:?}"
        );
        let rotated = assert_active_key_is_a_resource(
            &emulator,
            case,
            &server_id,
            &host_key_algorithm(&material),
        )
        .await;
        assert_ne!(
            rotated, generated,
            "case {case:?}: expected the rotated key to be its own record, got the one the \
             create generated"
        );

        // The key the server was serving with before is still a host key of it: a
        // rotation adds the new key, it does not delete the records a client may still
        // be auditing.
        let listed = walk_host_keys(&emulator, &server_id, None).await;
        assert!(
            listed
                .iter()
                .any(|key| member_of(key, "HostKeyId").as_str() == Some(generated.as_str())),
            "case {case:?}: expected the earlier key still listed, got {listed:?}"
        );
    }
}
