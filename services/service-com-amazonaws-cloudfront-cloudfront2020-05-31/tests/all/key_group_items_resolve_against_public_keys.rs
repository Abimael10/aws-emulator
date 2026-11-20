// SPDX-License-Identifier: CC0-1.0

//! Every public key id a key group CONFIGURATION names is resolved against this account's
//! public keys, on both calls that write one.
//!
//! `KeyGroupConfig$Items` is "a list of the identifiers of the public keys in the key group",
//! and a key group is an authorization boundary: a cache behaviour that trusts the group
//! serves only requests signed with a private key whose public key the group lists. An entry
//! that resolves to no stored public key therefore names no signer at all, so CloudFront
//! refuses the configuration — `InvalidArgument`, HTTP 400, with the fixed sentence
//! [`ABSENT_PUBLIC_KEY_IN_KEY_GROUP_MESSAGE`] — rather than storing a group that holds a
//! reference to a key that is not there. The rule is about the CONFIGURATION, so it holds on
//! every payload that states one: `CreateKeyGroup` and `UpdateKeyGroup` alike.
//!
//! The rows span what the rule generalises over:
//!
//! * the FORM the unresolvable entry takes — a well-formed public key id CloudFront never
//!   minted, the id of a key that was deleted, another resource kind's identifier, a minted
//!   id whose case was changed, a public key's NAME rather than its id, and a token of no
//!   resource kind's shape;
//! * its POSITION in the list — the only entry, the first, the middle, and the last of a list
//!   at the published quota;
//! * the direction the write comes from — a create and an update.
//!
//! Each refusal is also held to changing nothing: a refused create leaves the stored key
//! groups and the name it named exactly as they were, and a refused update leaves the
//! addressed group's configuration and version token exactly as they were.
//!
//! The harness (the router, the request helpers and the element scanner) is the distribution
//! lifecycle suite's, the key group calls and the `Group` document are the key group
//! lifecycle suite's, and the public key calls are the public key suites' — reused rather
//! than copied.

use std::collections::BTreeSet;

use service_com_amazonaws_cloudfront_cloudfront2020_05_31::state::{
    ABSENT_PUBLIC_KEY_IN_KEY_GROUP_MESSAGE, MAX_PUBLIC_KEYS_IN_KEY_GROUP,
    NO_SUCH_PUBLIC_KEY_MESSAGE, PUBLIC_KEY_IN_USE_MESSAGE,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{call, emulator, Answer, API_VERSION};
use crate::key_group_lifecycle::{create_group, get_group, mint_public_keys, Group};
use crate::public_key_delete::delete_key;
use crate::public_key_lifecycle::{create_key, pem};
use crate::response_headers_policy_lifecycle::escape;

// ---------------------------------------------------------------------------
// Calling the surfaces this suite drives
// ---------------------------------------------------------------------------

/// `PUT /2020-05-31/key-group/{Id}` with the `KeyGroupConfig` document `body` under the
/// version token `if_match`.
async fn update_group(
    router: &MultiServiceRouter,
    id: &str,
    body: &str,
    if_match: &str,
) -> Answer {
    call(
        router,
        "PUT",
        &format!("/{API_VERSION}/key-group/{id}"),
        Some(body),
        Some(if_match),
    )
    .await
}

/// `DELETE /2020-05-31/key-group/{Id}` under the version token `if_match`.
async fn delete_group(router: &MultiServiceRouter, id: &str, if_match: &str) -> Answer {
    call(
        router,
        "DELETE",
        &format!("/{API_VERSION}/key-group/{id}"),
        None,
        Some(if_match),
    )
    .await
}

/// `GET /2020-05-31/key-group` — the listing, read only to check what the store holds.
async fn list_groups(router: &MultiServiceRouter) -> Answer {
    call(
        router,
        "GET",
        &format!("/{API_VERSION}/key-group"),
        None,
        None,
    )
    .await
}

/// `GET /2020-05-31/public-key/{Id}` — the read the refusal's state is derived from.
async fn get_key(router: &MultiServiceRouter, id: &str) -> Answer {
    call(
        router,
        "GET",
        &format!("/{API_VERSION}/public-key/{id}"),
        None,
        None,
    )
    .await
}

/// The ids the listing reports, as a set.
async fn stored_group_ids(router: &MultiServiceRouter) -> BTreeSet<String> {
    let page = list_groups(router).await;
    page.root()
        .at("Items")
        .map(|items| {
            items
                .children_named("KeyGroupSummary")
                .into_iter()
                .filter_map(|summary| summary.at("KeyGroup"))
                .map(|group| group.text_at("Id").unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Upload one public key and report `(id, etag)` — the version token a delete of it is
/// addressed under, which [`mint_public_keys`] does not hand back.
async fn mint_key(router: &MultiServiceRouter, tag: &str) -> (String, String) {
    let body = format!(
        "<PublicKeyConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CallerReference>{tag}</CallerReference>\
         <Name>{tag}</Name>\
         <EncodedKey>{}</EncodedKey></PublicKeyConfig>",
        escape(&pem(2, 'D'))
    );
    let answer = create_key(router, &body).await;
    assert_eq!(
        answer.status, 201,
        "case minting {tag}: expected HTTP 201 from CreatePublicKey, got {} with {}",
        answer.status, answer.raw
    );
    (
        answer.root().text_at("Id").unwrap_or_default().to_string(),
        answer.header("etag").to_string(),
    )
}

// ---------------------------------------------------------------------------
// The cases
// ---------------------------------------------------------------------------

/// One configuration under test: the `Items` it submits and what the row is a case of.
#[derive(Debug, Clone)]
struct Row {
    case: String,
    items: Vec<String>,
}

/// The forms an unresolvable entry takes, each paired with a live key group id and a live
/// public key the account really holds, so "another resource kind's identifier" and "a name
/// rather than an id" are values the store does hold — just not as a public key id.
fn absent_forms(key_group_id: &str, held_key: &str, held_key_name: &str) -> Vec<(String, String)> {
    vec![
        (
            "a public key id CloudFront never minted".to_string(),
            "K2NEVERMINTED1".to_string(),
        ),
        (
            "another resource kind's identifier".to_string(),
            key_group_id.to_string(),
        ),
        (
            "a minted public key id in lower case".to_string(),
            held_key.to_lowercase(),
        ),
        (
            "a public key's name rather than its id".to_string(),
            held_key_name.to_string(),
        ),
        (
            "a token of no resource kind's shape".to_string(),
            "not-a-public-key-id".to_string(),
        ),
    ]
}

/// Every (form, position) row: the unresolvable entry `absent` placed in a list otherwise
/// built from `held`, the ids of keys the account holds.
fn rows(held: &[String], absent: &[(String, String)]) -> Vec<Row> {
    assert_eq!(
        held.len() + 1,
        MAX_PUBLIC_KEYS_IN_KEY_GROUP,
        "case the table itself: the longest row must sit exactly at the published quota, got \
         {} resolvable ids",
        held.len()
    );
    let mut rows: Vec<Row> = Vec::new();
    for (form, id) in absent {
        let at_the_quota = {
            let mut items: Vec<String> = held.to_vec();
            items.push(id.clone());
            items
        };
        for (position, items) in [
            ("as the only entry", vec![id.clone()]),
            (
                "first of three entries",
                vec![id.clone(), held[0].clone(), held[1].clone()],
            ),
            (
                "in the middle of three entries",
                vec![held[0].clone(), id.clone(), held[1].clone()],
            ),
            ("last of a list at the published quota", at_the_quota),
        ] {
            rows.push(Row {
                case: format!("{form}, {position}"),
                items,
            });
        }
    }
    rows
}

/// Assert `answer` is the modelled refusal of a configuration naming a key the account does
/// not hold: HTTP 400, `InvalidArgument`, and CloudFront's fixed sentence for the condition.
fn assert_refused(answer: &Answer, case: &str) {
    assert_eq!(
        answer.status, 400,
        "case {case:?}: expected HTTP 400, got {} with {}",
        answer.status, answer.raw
    );
    assert_eq!(
        answer.error_code(),
        "InvalidArgument",
        "case {case:?}: expected InvalidArgument, got {}",
        answer.raw
    );
    let message = answer.root().find_text("Message").unwrap_or_default();
    assert_eq!(
        message, ABSENT_PUBLIC_KEY_IN_KEY_GROUP_MESSAGE,
        "case {case:?}: expected the absent-key-in-group sentence, got {}",
        answer.raw
    );
}

#[tokio::test]
async fn a_create_naming_a_public_key_the_account_does_not_hold_is_refused() {
    let router = emulator();
    let keys = mint_public_keys(
        &router,
        "kg-items-create",
        MAX_PUBLIC_KEYS_IN_KEY_GROUP - 1,
    )
    .await;

    // One group the account really holds: its id is a value the store knows, but not as a
    // public key id, and the listing baseline below is read with it stored.
    let seed = Group {
        case: "the group the account already holds".to_string(),
        name: "kg-items-create-seed".to_string(),
        items: keys[..1].to_vec(),
        comment: None,
    };
    let created = create_group(&router, &seed.xml()).await;
    assert_eq!(
        created.status, 201,
        "case the seeded group: expected HTTP 201 from CreateKeyGroup, got {} with {}",
        created.status, created.raw
    );
    let seed_id = created.root().text_at("Id").unwrap_or_default().to_string();

    let baseline = stored_group_ids(&router).await;
    let table = rows(
        &keys,
        &absent_forms(&seed_id, &keys[0], "kg-items-create-0"),
    );
    for (index, row) in table.iter().enumerate() {
        let name = format!("kg-items-create-refused-{index}");
        let submitted = Group {
            case: row.case.clone(),
            name: name.clone(),
            items: row.items.clone(),
            comment: Some("names a key the account does not hold".to_string()),
        };
        let answer = create_group(&router, &submitted.xml()).await;
        assert_refused(&answer, &row.case);

        // The refusal minted nothing: no group was added, and the name the refused create
        // named is still free, so the same name takes a configuration every entry of which
        // resolves.
        let stored = stored_group_ids(&router).await;
        assert_eq!(
            stored, baseline,
            "case {:?}: expected the stored key groups to be unchanged, got {stored:?}",
            row.case
        );
        let repaired = Group {
            case: format!("{} repaired", row.case),
            name,
            items: keys[..1].to_vec(),
            comment: None,
        };
        let accepted = create_group(&router, &repaired.xml()).await;
        assert_eq!(
            accepted.status, 201,
            "case {:?}: expected the repaired configuration to be accepted, got {} with {}",
            repaired.case, accepted.status, accepted.raw
        );
        let id = accepted.root().text_at("Id").unwrap_or_default().to_string();
        let read = get_group(&router, &id).await;
        let config = read
            .root()
            .at("KeyGroupConfig")
            .unwrap_or_else(|| panic!("case {:?}: no configuration in {}", repaired.case, read.raw));
        repaired.assert_matches(config, "GetKeyGroup", &read.raw);

        // The repaired create is removed again, so the next row's baseline is this one's.
        let cleanup = delete_group(&router, &id, accepted.header("etag")).await;
        assert_eq!(
            cleanup.status, 204,
            "case {:?}: expected HTTP 204 removing the repaired group, got {} with {}",
            repaired.case, cleanup.status, cleanup.raw
        );
    }
}

#[tokio::test]
async fn an_update_naming_a_public_key_the_account_does_not_hold_is_refused() {
    let router = emulator();
    let keys = mint_public_keys(
        &router,
        "kg-items-update",
        MAX_PUBLIC_KEYS_IN_KEY_GROUP - 1,
    )
    .await;

    let held = Group {
        case: "the configuration the create stored".to_string(),
        name: "kg-items-update-subject".to_string(),
        items: keys[..2].to_vec(),
        comment: Some("the configuration an update must not replace".to_string()),
    };
    let created = create_group(&router, &held.xml()).await;
    assert_eq!(
        created.status, 201,
        "case the subject group: expected HTTP 201 from CreateKeyGroup, got {} with {}",
        created.status, created.raw
    );
    let id = created.root().text_at("Id").unwrap_or_default().to_string();
    let etag = created.header("etag").to_string();

    let table = rows(&keys, &absent_forms(&id, &keys[0], "kg-items-update-0"));
    for row in &table {
        let submitted = Group {
            case: row.case.clone(),
            name: held.name.clone(),
            items: row.items.clone(),
            comment: Some("names a key the account does not hold".to_string()),
        };
        let answer = update_group(&router, &id, &submitted.xml(), &etag).await;
        assert_refused(&answer, &row.case);

        // The refusal wrote nothing: the group still answers the configuration its create
        // stored, at the version token that create minted.
        let read = get_group(&router, &id).await;
        assert_eq!(
            read.status, 200,
            "case {:?}: expected the group to still be readable, got {} with {}",
            row.case, read.status, read.raw
        );
        assert_eq!(
            read.header("etag"),
            etag,
            "case {:?}: expected the stored version token to be unchanged, got {:?}",
            row.case,
            read.header("etag")
        );
        let config = read
            .root()
            .at("KeyGroupConfig")
            .unwrap_or_else(|| panic!("case {:?}: no configuration in {}", row.case, read.raw));
        held.assert_matches(config, "GetKeyGroup after the refusal", &read.raw);
    }

    // The same update lands once every entry resolves, which is what makes the refusals
    // above about the unresolvable entry rather than about the update.
    let accepted_config = Group {
        case: "every entry resolves".to_string(),
        name: held.name.clone(),
        items: keys.to_vec(),
        comment: Some("the replacement configuration".to_string()),
    };
    let accepted = update_group(&router, &id, &accepted_config.xml(), &etag).await;
    assert_eq!(
        accepted.status, 200,
        "case every entry resolves: expected HTTP 200 from UpdateKeyGroup, got {} with {}",
        accepted.status, accepted.raw
    );
    let read = get_group(&router, &id).await;
    let config = read
        .root()
        .at("KeyGroupConfig")
        .unwrap_or_else(|| panic!("case every entry resolves: no configuration in {}", read.raw));
    accepted_config.assert_matches(config, "GetKeyGroup after the update", &read.raw);
}

/// A key group holds a public key id only while the key is there: once the group that listed
/// the key has let go and the key itself is deleted, no later write may name that id again —
/// which is the state the refusal is derived from, as `GetPublicKey` on the same id reports.
#[tokio::test]
async fn an_id_whose_public_key_was_deleted_cannot_be_named_again() {
    let router = emulator();
    let (key, key_etag) = mint_key(&router, "kg-items-deleted-key").await;
    let (spare, _) = mint_key(&router, "kg-items-deleted-spare").await;

    let first = Group {
        case: "the group that listed the key".to_string(),
        name: "kg-items-deleted-first".to_string(),
        items: vec![key.clone()],
        comment: None,
    };
    let created = create_group(&router, &first.xml()).await;
    assert_eq!(
        created.status, 201,
        "case the group that listed the key: expected HTTP 201, got {} with {}",
        created.status, created.raw
    );
    let group_id = created.root().text_at("Id").unwrap_or_default().to_string();
    let group_etag = created.header("etag").to_string();

    // While the group lists the key, the key cannot be released.
    let refused = delete_key(&router, &key, Some(&key_etag)).await;
    assert_eq!(
        refused.status, 409,
        "case the key while a group lists it: expected HTTP 409, got {} with {}",
        refused.status, refused.raw
    );
    assert_eq!(
        refused.error_code(),
        "PublicKeyInUse",
        "case the key while a group lists it: expected PublicKeyInUse, got {}",
        refused.raw
    );
    assert_eq!(
        refused.root().find_text("Message").unwrap_or_default(),
        PUBLIC_KEY_IN_USE_MESSAGE,
        "case the key while a group lists it: expected the in-use sentence, got {}",
        refused.raw
    );

    // The group goes, then the key.
    let dropped = delete_group(&router, &group_id, &group_etag).await;
    assert_eq!(
        dropped.status, 204,
        "case deleting the group: expected HTTP 204, got {} with {}",
        dropped.status, dropped.raw
    );
    let released = delete_key(&router, &key, Some(&key_etag)).await;
    assert_eq!(
        released.status, 204,
        "case deleting the released key: expected HTTP 204, got {} with {}",
        released.status, released.raw
    );
    let gone = get_key(&router, &key).await;
    assert_eq!(
        gone.status, 404,
        "case reading the deleted key: expected HTTP 404, got {} with {}",
        gone.status, gone.raw
    );
    assert_eq!(
        gone.root().find_text("Message").unwrap_or_default(),
        NO_SUCH_PUBLIC_KEY_MESSAGE,
        "case reading the deleted key: expected the absent-key sentence, got {}",
        gone.raw
    );

    // A create naming the deleted id is refused, and stores nothing.
    let second = Group {
        case: "a second group naming the deleted key".to_string(),
        name: "kg-items-deleted-second".to_string(),
        items: vec![key.clone()],
        comment: Some("should fail: references a deleted public key".to_string()),
    };
    let answer = create_group(&router, &second.xml()).await;
    assert_refused(&answer, &second.case);
    let stored = stored_group_ids(&router).await;
    assert!(
        stored.is_empty(),
        "case a second group naming the deleted key: expected no stored key group, got \
         {stored:?}"
    );

    // So is an update of a live group onto the deleted id, and that group keeps its keys.
    let live = Group {
        case: "the live group".to_string(),
        name: "kg-items-deleted-live".to_string(),
        items: vec![spare.clone()],
        comment: None,
    };
    let live_created = create_group(&router, &live.xml()).await;
    assert_eq!(
        live_created.status, 201,
        "case the live group: expected HTTP 201, got {} with {}",
        live_created.status, live_created.raw
    );
    let live_id = live_created
        .root()
        .text_at("Id")
        .unwrap_or_default()
        .to_string();
    let live_etag = live_created.header("etag").to_string();
    let onto_deleted = Group {
        case: "an update onto the deleted key".to_string(),
        name: live.name.clone(),
        items: vec![key.clone()],
        comment: None,
    };
    let answer = update_group(&router, &live_id, &onto_deleted.xml(), &live_etag).await;
    assert_refused(&answer, &onto_deleted.case);
    let read = get_group(&router, &live_id).await;
    let config = read.root().at("KeyGroupConfig").unwrap_or_else(|| {
        panic!(
            "case an update onto the deleted key: no configuration in {}",
            read.raw
        )
    });
    live.assert_matches(config, "GetKeyGroup after the refused update", &read.raw);

    // A key uploaded in the deleted one's place is a key the account holds, so a group may
    // name it: what the rule selects on is the store, not the id's history.
    let (replacement, _) = mint_key(&router, "kg-items-deleted-replacement").await;
    let third = Group {
        case: "a group naming the replacement key".to_string(),
        name: "kg-items-deleted-third".to_string(),
        items: vec![replacement.clone()],
        comment: None,
    };
    let accepted = create_group(&router, &third.xml()).await;
    assert_eq!(
        accepted.status, 201,
        "case a group naming the replacement key: expected HTTP 201, got {} with {}",
        accepted.status, accepted.raw
    );
    let id = accepted.root().text_at("Id").unwrap_or_default().to_string();
    let read = get_group(&router, &id).await;
    let config = read.root().at("KeyGroupConfig").unwrap_or_else(|| {
        panic!(
            "case a group naming the replacement key: no configuration in {}",
            read.raw
        )
    });
    third.assert_matches(config, "GetKeyGroup", &read.raw);
}

/// Every configuration whose entries ALL resolve is stored, whatever the count and the order:
/// the rule refuses an unresolvable entry and nothing else. What the group then HOLDS is its
/// membership, so a list naming one held key twice holds it once
/// (`key_group_items_are_a_set.rs` owns that rule).
#[tokio::test]
async fn a_configuration_naming_only_keys_the_account_holds_is_accepted() {
    let router = emulator();
    let keys = mint_public_keys(&router, "kg-items-held", MAX_PUBLIC_KEYS_IN_KEY_GROUP).await;

    let mut rows: Vec<Row> = Vec::new();
    for count in 1..=MAX_PUBLIC_KEYS_IN_KEY_GROUP {
        rows.push(Row {
            case: format!("{count} held key(s), in the order they were created"),
            items: keys[..count].to_vec(),
        });
        rows.push(Row {
            case: format!("{count} held key(s), reversed"),
            items: keys[..count].iter().rev().cloned().collect(),
        });
    }
    // One list naming the same held key twice: the rule here is about each entry resolving,
    // and a held key named twice resolves as surely as one named once.
    rows.push(Row {
        case: "one held key named twice".to_string(),
        items: vec![keys[0].clone(), keys[0].clone()],
    });

    for (index, row) in rows.iter().enumerate() {
        let submitted = Group {
            case: row.case.clone(),
            name: format!("kg-items-held-{index}"),
            items: row.items.clone(),
            comment: None,
        };
        let answer = create_group(&router, &submitted.xml()).await;
        assert_eq!(
            answer.status, 201,
            "case {:?}: expected HTTP 201 from CreateKeyGroup, got {} with {}",
            row.case, answer.status, answer.raw
        );
        // The group holds each public key its configuration names ONCE, at the place of that
        // id's first mention, so the readback of a row naming a key twice reports it once.
        let mut held: Vec<String> = Vec::new();
        for item in &row.items {
            if !held.contains(item) {
                held.push(item.clone());
            }
        }
        let stored = Group {
            items: held,
            ..submitted.clone()
        };
        let id = answer.root().text_at("Id").unwrap_or_default().to_string();
        let read = get_group(&router, &id).await;
        let config = read
            .root()
            .at("KeyGroupConfig")
            .unwrap_or_else(|| panic!("case {:?}: no configuration in {}", row.case, read.raw));
        stored.assert_matches(config, "GetKeyGroup", &read.raw);
    }
}
