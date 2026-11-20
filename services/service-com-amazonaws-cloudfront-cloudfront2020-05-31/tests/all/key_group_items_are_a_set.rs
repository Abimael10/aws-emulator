// SPDX-License-Identifier: CC0-1.0

//! What a key group holds when its configuration names one public key id more than once, end
//! to end over HTTP.
//!
//! RULE: `KeyGroupConfig$Items` is "a list of the identifiers of the public keys in the key
//! group" — the group's MEMBERSHIP — so an id names the same public key however many times a
//! payload repeats it, and the group holds it ONCE: every write that stores a whole
//! configuration (`CreateKeyGroup`, `UpdateKeyGroup`) collapses repeated ids, keeping the
//! place of each id's FIRST mention since the list is reported in the order it was submitted,
//! and every readback of the group — the write's own 2xx body, `GetKeyGroup`,
//! `GetKeyGroupConfig` and `ListKeyGroups` — reports each of the group's keys once. A readback
//! repeating an id gives a client more members than the group has signers, so a count over
//! `Items` and any quota arithmetic over it are off, and a read-modify-write round trip
//! re-submits the repeat so the divergence compounds.
//!
//! The rows span what the rule generalises over: how many ids are repeated, how often each is
//! repeated, whether the repeats are adjacent or separated by other ids, where in the list the
//! first mention sits (first, middle, last), a list every one of whose entries is the same id,
//! a quota-sized list submitted twice over, and — as the control — a list with no repeat at
//! all, which has to come back exactly as submitted.
//!
//! The harness (the router, the request helpers, the key group document and the minted public
//! keys) is the key group lifecycle suite's, reused rather than copied.

use service_com_amazonaws_cloudfront_cloudfront2020_05_31::state::MAX_PUBLIC_KEYS_IN_KEY_GROUP;
use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{call, emulator, is_version_token, Answer, Node, API_VERSION};
use crate::key_group_lifecycle::{create_group, get_group, mint_public_keys, Group};

// ---------------------------------------------------------------------------
// Calling the surface under test
// ---------------------------------------------------------------------------

/// `GET /2020-05-31/key-group/{Id}/config`.
async fn get_group_config(router: &MultiServiceRouter, id: &str) -> Answer {
    call(
        router,
        "GET",
        &format!("/{API_VERSION}/key-group/{id}/config"),
        None,
        None,
    )
    .await
}

/// `GET /2020-05-31/key-group`, the whole unpaged listing.
async fn list_groups(router: &MultiServiceRouter) -> Answer {
    call(router, "GET", &format!("/{API_VERSION}/key-group"), None, None).await
}

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

// ---------------------------------------------------------------------------
// The configurations under test
// ---------------------------------------------------------------------------

/// One row: the `Items` a payload submits, written as indices into the account's minted public
/// keys, so a row states the SHAPE of the repetition and the ids themselves are the ones
/// `CreatePublicKey` minted.
struct Row {
    case: &'static str,
    /// The positions, in the minted key list, the submitted `Items` names in order.
    submitted: Vec<usize>,
}

/// The rows every assertion below runs over.
///
/// `MAX_PUBLIC_KEYS_IN_KEY_GROUP` is the published quota on one group's key list, so no row
/// names more DISTINCT keys than that; how many times each is repeated is what the rows differ
/// along.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "no repeat at all",
            submitted: vec![0, 1, 2],
        },
        Row {
            case: "one id twice, adjacent",
            submitted: vec![0, 0],
        },
        Row {
            case: "one id twice, separated by another id",
            submitted: vec![0, 1, 0],
        },
        Row {
            case: "one id three times",
            submitted: vec![1, 1, 1],
        },
        Row {
            case: "the repeat of an id first mentioned last",
            submitted: vec![2, 3, 3, 2],
        },
        Row {
            case: "two ids each repeated, interleaved",
            submitted: vec![0, 1, 0, 2, 1, 0],
        },
        Row {
            case: "every entry the same id",
            submitted: vec![4, 4, 4, 4, 4, 4, 4],
        },
        Row {
            case: "the quota's worth of keys submitted twice over",
            submitted: (0..MAX_PUBLIC_KEYS_IN_KEY_GROUP)
                .chain(0..MAX_PUBLIC_KEYS_IN_KEY_GROUP)
                .collect(),
        },
        Row {
            case: "the quota's worth of keys reversed, each repeated",
            submitted: (0..MAX_PUBLIC_KEYS_IN_KEY_GROUP)
                .rev()
                .flat_map(|index| [index, index])
                .collect(),
        },
    ]
}

/// The ids a row submits, and the ids the group then holds: each id once, at the place of its
/// first mention.
fn submitted_and_held(row: &Row, keys: &[String]) -> (Vec<String>, Vec<String>) {
    let submitted: Vec<String> = row
        .submitted
        .iter()
        .map(|index| keys[*index].clone())
        .collect();
    let mut held: Vec<String> = Vec::new();
    for id in &submitted {
        if !held.contains(id) {
            held.push(id.clone());
        }
    }
    (submitted, held)
}

/// The `Items` a serialised `KeyGroupConfig` element reports, in order.
fn items_of(config: &Node) -> Vec<String> {
    config
        .at("Items")
        .map(|items| {
            items
                .children_named("PublicKey")
                .into_iter()
                .map(|item| item.text().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Assert that the `KeyGroupConfig` under `config` reports `held`, and nothing more.
fn assert_items(config: &Node, held: &[String], case: &str, context: &str, raw: &str) {
    let items = items_of(config);
    assert_eq!(
        items,
        held.to_vec(),
        "case {case} ({context}): expected the public key ids {held:?}, got {items:?} in {raw}"
    );
}

/// The `KeyGroupConfig` element of a response that carries a whole `KeyGroup`.
fn config_of<'a>(answer: &'a Answer, case: &str, context: &str) -> &'a Node {
    answer.root().at("KeyGroupConfig").unwrap_or_else(|| {
        panic!(
            "case {case} ({context}): no KeyGroupConfig in {}",
            answer.raw
        )
    })
}

// ---------------------------------------------------------------------------
// The rules
// ---------------------------------------------------------------------------

/// A create whose `Items` repeats an id stores the group's membership: the create's own body
/// and every later read of that group report each id once, at the place of its first mention.
#[tokio::test]
async fn a_create_holds_each_public_key_id_once() {
    let router = emulator();
    let keys = mint_public_keys(&router, "kg-set-create", MAX_PUBLIC_KEYS_IN_KEY_GROUP).await;

    let mut created: Vec<(String, Vec<String>, &'static str)> = Vec::new();
    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let (submitted, held) = submitted_and_held(row, &keys);
        let group = Group {
            case: case.to_string(),
            name: format!("kg-set-create-{index}"),
            items: submitted.clone(),
            comment: Some(format!("the group for {case}")),
        };
        let answer = create_group(&router, &group.xml()).await;
        assert_eq!(
            answer.status, 201,
            "case {case}: expected HTTP 201 from CreateKeyGroup, got {} with {}",
            answer.status, answer.raw
        );
        assert_items(
            config_of(&answer, case, "the create"),
            &held,
            case,
            "the create",
            &answer.raw,
        );
        let id = answer.root().text_at("Id").unwrap_or_default().to_string();
        let etag = answer.header("etag").to_string();
        assert!(
            is_version_token(&etag),
            "case {case}: expected a minted version token in the create's ETag, got {etag:?}"
        );

        // The whole group and its configuration alone answer from the one stored record.
        let read = get_group(&router, &id).await;
        assert_eq!(
            read.status, 200,
            "case {case}: expected HTTP 200 from GetKeyGroup, got {} with {}",
            read.status, read.raw
        );
        assert_items(
            config_of(&read, case, "GetKeyGroup"),
            &held,
            case,
            "GetKeyGroup",
            &read.raw,
        );
        let config_read = get_group_config(&router, &id).await;
        assert_eq!(
            config_read.status, 200,
            "case {case}: expected HTTP 200 from GetKeyGroupConfig, got {} with {}",
            config_read.status, config_read.raw
        );
        // `KeyGroupConfig` is that output's `@httpPayload`, so it IS the document's root.
        assert_items(
            config_read.root(),
            &held,
            case,
            "GetKeyGroupConfig",
            &config_read.raw,
        );
        created.push((id, held, case));
    }

    // The listing reports every group from the same records.
    let listing = list_groups(&router).await;
    assert_eq!(
        listing.status, 200,
        "case the listing: expected HTTP 200 from ListKeyGroups, got {} with {}",
        listing.status, listing.raw
    );
    let summaries: Vec<&Node> = listing
        .root()
        .at("Items")
        .map(|items| {
            items
                .children_named("KeyGroupSummary")
                .into_iter()
                .filter_map(|summary| summary.at("KeyGroup"))
                .collect()
        })
        .unwrap_or_default();
    for (id, held, case) in &created {
        let listed = summaries
            .iter()
            .find(|group| group.text_at("Id") == Some(id.as_str()))
            .unwrap_or_else(|| {
                panic!(
                    "case {case}: the listing reports no group {id}: {}",
                    listing.raw
                )
            });
        let config = listed.at("KeyGroupConfig").unwrap_or_else(|| {
            panic!(
                "case {case}: the listed group {id} carries no configuration: {}",
                listing.raw
            )
        });
        assert_items(config, held, case, "ListKeyGroups", &listing.raw);
    }
}

/// An update whose `Items` repeats an id replaces the membership the same way: the update's own
/// body and every later read report each id once, and re-submitting the list a read just
/// reported changes nothing about the membership.
#[tokio::test]
async fn an_update_holds_each_public_key_id_once() {
    let router = emulator();
    let keys = mint_public_keys(&router, "kg-set-update", MAX_PUBLIC_KEYS_IN_KEY_GROUP).await;

    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let (submitted, held) = submitted_and_held(row, &keys);
        let name = format!("kg-set-update-{index}");
        // The group starts out naming one key, so what each row exercises is the REPLACEMENT.
        let start = Group {
            case: case.to_string(),
            name: name.clone(),
            items: keys[..1].to_vec(),
            comment: None,
        };
        let created = create_group(&router, &start.xml()).await;
        assert_eq!(
            created.status, 201,
            "case {case}: expected HTTP 201 from CreateKeyGroup, got {} with {}",
            created.status, created.raw
        );
        let id = created.root().text_at("Id").unwrap_or_default().to_string();
        let etag = created.header("etag").to_string();

        let replacement = Group {
            case: case.to_string(),
            name: name.clone(),
            items: submitted.clone(),
            comment: Some(format!("the membership for {case}")),
        };
        let updated = update_group(&router, &id, &replacement.xml(), &etag).await;
        assert_eq!(
            updated.status, 200,
            "case {case}: expected HTTP 200 from UpdateKeyGroup, got {} with {}",
            updated.status, updated.raw
        );
        assert_items(
            config_of(&updated, case, "the update"),
            &held,
            case,
            "the update",
            &updated.raw,
        );
        let updated_etag = updated.header("etag").to_string();
        assert!(
            is_version_token(&updated_etag),
            "case {case}: expected a minted version token in the update's ETag, got \
             {updated_etag:?}"
        );

        let read = get_group(&router, &id).await;
        assert_eq!(
            read.status, 200,
            "case {case}: expected HTTP 200 from GetKeyGroup, got {} with {}",
            read.status, read.raw
        );
        assert_items(
            config_of(&read, case, "GetKeyGroup after the update"),
            &held,
            case,
            "GetKeyGroup after the update",
            &read.raw,
        );

        // A read-modify-write round trip: the list a read reported, re-submitted verbatim,
        // leaves the same membership — so nothing about the repeat survives to compound.
        let round_trip = Group {
            case: case.to_string(),
            name: name.clone(),
            items: items_of(config_of(&read, case, "GetKeyGroup after the update")),
            comment: Some(format!("the membership for {case}")),
        };
        let again = update_group(&router, &id, &round_trip.xml(), &updated_etag).await;
        assert_eq!(
            again.status, 200,
            "case {case}: expected HTTP 200 re-submitting the readback, got {} with {}",
            again.status, again.raw
        );
        assert_items(
            config_of(&again, case, "the re-submitted readback"),
            &held,
            case,
            "the re-submitted readback",
            &again.raw,
        );
    }
}
