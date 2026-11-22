// SPDX-License-Identifier: CC0-1.0

//! The `Items` list of a stored field-level encryption configuration's `QueryArgProfiles`
//! wrapper, end to end over HTTP.
//!
//! Every assertion goes through the router, so each case exercises routing, restXml
//! (de)serialization, the handler, the persisted state and serialization — the same path a
//! client takes. The rule under test is the one CloudFront's own answers state for this
//! wrapper: the query argument-profile mapping a configuration is STORED with carries its
//! `Items` list, so every read reports the list beside the `Quantity` that counts it — empty
//! where the mapping maps nothing — rather than turning a list stated as empty into a member
//! that is absent. It holds whichever call stored the configuration, so the create path and
//! the update path are both driven here, and whichever read reports it: the mutation's own
//! answer, both readers of the one record (`GetFieldLevelEncryption` and
//! `GetFieldLevelEncryptionConfig`) and the summary a listing carries.
//!
//! The rows differ along what the rule generalises over — how a request states the wrapper: a
//! wrapper stating a quantity and no list at all, one stating the empty list, one mapping a
//! single query argument and one mapping several, plus (on the create, the only call that may
//! leave the section out) a configuration stating no mapping section whatsoever, for which the
//! service supplies the mapping that maps nothing.
//!
//! A section the request states WITHOUT a wrapper is stored without one, which is
//! [`crate::server_supplied_config_members`]'s row rather than a case of this rule. The wire
//! harness, the readers, the profile every mapping points at and the listing are the crate's
//! own — reused rather than copied.

use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{emulator, is_version_token, Answer, Node, API_VERSION};
use crate::field_level_encryption_config_lifecycle::{
    create_config, get_config, get_config_document, is_config_id,
};
use crate::field_level_encryption_config_list::list_configs;
use crate::field_level_encryption_config_update::update_config;
use crate::field_level_encryption_profile_lifecycle::store_profile;

/// One row: how a request states the query argument-profile mapping, and how many entries the
/// stored list carries.
struct Row {
    /// What this row is a case of, named in every assertion it drives.
    case: &'static str,
    /// The `QueryArgProfiles` wrapper this row's `QueryArgProfileConfig` states, with
    /// [`PROFILE_PLACEHOLDER`] standing in for the profile a mapping names — or `None` to
    /// state no `QueryArgProfileConfig` section at all.
    wrapper: Option<&'static str>,
    /// The entries the stored list carries, which is also the `Quantity` that counts them.
    entries: usize,
}

/// What a row's wrapper writes where the id of the stored profile belongs.
const PROFILE_PLACEHOLDER: &str = "{profile}";

/// The forms a request states the wrapper in.
const ROWS: &[Row] = &[
    Row {
        case: "a wrapper stating a quantity and no Items list",
        wrapper: Some("<QueryArgProfiles><Quantity>0</Quantity></QueryArgProfiles>"),
        entries: 0,
    },
    Row {
        case: "a wrapper stating the empty Items list",
        wrapper: Some(
            "<QueryArgProfiles><Quantity>0</Quantity><Items></Items></QueryArgProfiles>",
        ),
        entries: 0,
    },
    Row {
        case: "a wrapper mapping one query argument",
        wrapper: Some(
            "<QueryArgProfiles><Quantity>1</Quantity><Items>\
             <QueryArgProfile><QueryArg>fle-profile</QueryArg>\
             <ProfileId>{profile}</ProfileId></QueryArgProfile>\
             </Items></QueryArgProfiles>",
        ),
        entries: 1,
    },
    Row {
        case: "a wrapper mapping three query arguments",
        wrapper: Some(
            "<QueryArgProfiles><Quantity>3</Quantity><Items>\
             <QueryArgProfile><QueryArg>fle-profile</QueryArg>\
             <ProfileId>{profile}</ProfileId></QueryArgProfile>\
             <QueryArgProfile><QueryArg>fle-profile-two</QueryArg>\
             <ProfileId>{profile}</ProfileId></QueryArgProfile>\
             <QueryArgProfile><QueryArg>fle-profile-three</QueryArg>\
             <ProfileId>{profile}</ProfileId></QueryArgProfile>\
             </Items></QueryArgProfiles>",
        ),
        entries: 3,
    },
    Row {
        case: "no query argument mapping section at all",
        wrapper: None,
        entries: 0,
    },
];

/// The `FieldLevelEncryptionConfig` document a row submits: the row's query argument section
/// beside a content type mapping that names `profile`, so a configuration whose query argument
/// mapping maps nothing still encrypts something (`QueryArgProfileEmpty` otherwise).
fn config_document(row: &Row, reference: &str, comment: &str, profile: &str) -> String {
    let mut out = format!(
        "<FieldLevelEncryptionConfig \
         xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CallerReference>{reference}</CallerReference>\
         <Comment>{comment}</Comment>"
    );
    if let Some(wrapper) = row.wrapper {
        out.push_str(
            "<QueryArgProfileConfig><ForwardWhenQueryArgProfileIsUnknown>true\
             </ForwardWhenQueryArgProfileIsUnknown>",
        );
        out.push_str(&wrapper.replace(PROFILE_PLACEHOLDER, profile));
        out.push_str("</QueryArgProfileConfig>");
    }
    out.push_str(&format!(
        "<ContentTypeProfileConfig><ForwardWhenContentTypeIsUnknown>true\
         </ForwardWhenContentTypeIsUnknown><ContentTypeProfiles><Quantity>1</Quantity><Items>\
         <ContentTypeProfile><Format>URLEncoded</Format><ProfileId>{profile}</ProfileId>\
         <ContentType>application/x-www-form-urlencoded</ContentType></ContentTypeProfile>\
         </Items></ContentTypeProfiles></ContentTypeProfileConfig>\
         </FieldLevelEncryptionConfig>"
    ));
    out
}

/// Assert that `config` — a serialised `FieldLevelEncryptionConfig` or the
/// `FieldLevelEncryptionSummary` that repeats the same section — reports the stored mapping:
/// the wrapper, the `Quantity` counting its entries and the `Items` list ITSELF, carrying
/// `entries` entries that each name `profile`.
fn assert_stored_mapping(
    config: &Node,
    entries: usize,
    profile: &str,
    case: &str,
    context: &str,
    raw: &str,
) {
    let wrapper = config
        .at("QueryArgProfileConfig/QueryArgProfiles")
        .unwrap_or_else(|| {
            panic!("case {case:?} ({context}): expected a QueryArgProfiles wrapper, got {raw}")
        });
    let quantity = wrapper.text_at("Quantity");
    assert_eq!(
        quantity,
        Some(entries.to_string().as_str()),
        "case {case:?} ({context}): expected QueryArgProfiles Quantity {entries}, got \
         {quantity:?} in {raw}"
    );
    let items = wrapper.child("Items").unwrap_or_else(|| {
        panic!(
            "case {case:?} ({context}): expected the QueryArgProfiles Items list, got no Items \
             member in {raw}"
        )
    });
    let listed = items.children_named("QueryArgProfile");
    assert_eq!(
        listed.len(),
        entries,
        "case {case:?} ({context}): expected {entries} QueryArgProfile entries, got {} in {raw}",
        listed.len()
    );
    for entry in listed {
        let named = entry.text_at("ProfileId");
        assert_eq!(
            named,
            Some(profile),
            "case {case:?} ({context}): expected a mapping naming profile {profile:?}, got \
             {named:?} in {raw}"
        );
    }
}

/// Assert both readers of the configuration `id` report the stored mapping, and so does the
/// summary a listing carries for it.
async fn assert_reads(
    router: &MultiServiceRouter,
    id: &str,
    row: &Row,
    profile: &str,
    context: &str,
) {
    for (reader, answer) in [
        ("GetFieldLevelEncryption", get_config(router, id).await),
        (
            "GetFieldLevelEncryptionConfig",
            get_config_document(router, id).await,
        ),
    ] {
        assert_eq!(
            answer.status, 200,
            "case {:?} ({context}): expected HTTP 200 from {reader}, got {} with {}",
            row.case, answer.status, answer.raw
        );
        let config = answer
            .root()
            .descendant("FieldLevelEncryptionConfig")
            .unwrap_or_else(|| {
                panic!(
                    "case {:?} ({context}): expected a FieldLevelEncryptionConfig from {reader}, \
                     got {}",
                    row.case, answer.raw
                )
            });
        assert_stored_mapping(
            config,
            row.entries,
            profile,
            row.case,
            &format!("{context}, {reader}"),
            &answer.raw,
        );
    }

    let listed = list_configs(router, "").await;
    assert_eq!(
        listed.status, 200,
        "case {:?} ({context}): expected HTTP 200 from ListFieldLevelEncryptionConfigs, got {} \
         with {}",
        row.case, listed.status, listed.raw
    );
    let summary = summary_for(&listed, id).unwrap_or_else(|| {
        panic!(
            "case {:?} ({context}): expected a listed summary for {id}, got {}",
            row.case, listed.raw
        )
    });
    assert_stored_mapping(
        summary,
        row.entries,
        profile,
        row.case,
        &format!("{context}, the listed summary"),
        &listed.raw,
    );
}

/// The `FieldLevelEncryptionSummary` a listing carries for the configuration `id`, or `None`.
fn summary_for<'a>(listed: &'a Answer, id: &str) -> Option<&'a Node> {
    listed
        .root()
        .at("Items")
        .map(|items| items.children_named("FieldLevelEncryptionSummary"))
        .unwrap_or_default()
        .into_iter()
        .find(|summary| summary.text_at("Id") == Some(id))
}

/// The id and the version token of the configuration a create stored for `row`.
async fn create(
    router: &MultiServiceRouter,
    row: &Row,
    reference: &str,
    profile: &str,
) -> (String, String) {
    let created = create_config(
        router,
        &config_document(row, reference, "the mapping a create stored", profile),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "case {:?}: expected HTTP 201 from CreateFieldLevelEncryptionConfig, got {} with {}",
        row.case, created.status, created.raw
    );
    let id = created.root().text_at("Id").unwrap_or_default().to_string();
    assert!(
        is_config_id(&id),
        "case {:?}: expected a minted configuration id, got {id:?}",
        row.case
    );
    let etag = created.header("etag").to_string();
    assert!(
        is_version_token(&etag),
        "case {:?}: expected a minted version token, got {etag:?}",
        row.case
    );
    let config = created
        .root()
        .descendant("FieldLevelEncryptionConfig")
        .unwrap_or_else(|| {
            panic!(
                "case {:?}: expected a FieldLevelEncryptionConfig from the create, got {}",
                row.case, created.raw
            )
        });
    assert_stored_mapping(
        config,
        row.entries,
        profile,
        row.case,
        "the create's own answer",
        &created.raw,
    );
    (id, etag)
}

/// RULE: a configuration the CREATE stored carries its `Items` list, whichever form the
/// request stated the wrapper in — reported by the create itself, by both readers of the
/// record and by the summary a listing carries.
#[tokio::test]
async fn a_created_configuration_carries_its_query_argument_items_list() {
    let router = emulator();
    let profile = store_profile(&router, "query-arg-items-created-profile").await;

    for (index, row) in ROWS.iter().enumerate() {
        let (id, _) = create(
            &router,
            row,
            &format!("query-arg-items-created-{index}"),
            &profile,
        )
        .await;
        assert_reads(&router, &id, row, &profile, "after the create").await;
    }
}

/// RULE: the same holds for a configuration an UPDATE stored, which replaces the mapping
/// whole — including the update that empties a mapping that mapped three query arguments, the
/// case a client walks when it stops encrypting under a profile it is about to delete.
///
/// `UpdateFieldLevelEncryptionConfig` requires the section on every payload
/// (`IllegalUpdate`), so the row that states none at all is a create's case only.
#[tokio::test]
async fn an_updated_configuration_carries_its_query_argument_items_list() {
    let router = emulator();
    let profile = store_profile(&router, "query-arg-items-updated-profile").await;
    let seed = ROWS
        .iter()
        .find(|row| row.entries == 3)
        .expect("a row mapping several query arguments");

    for (index, row) in ROWS.iter().enumerate() {
        if row.wrapper.is_none() {
            continue;
        }
        let reference = format!("query-arg-items-updated-{index}");
        let (id, etag) = create(&router, seed, &reference, &profile).await;
        let updated = update_config(
            &router,
            &id,
            &config_document(row, &reference, "the mapping an update stored", &profile),
            Some(&etag),
        )
        .await;
        assert_eq!(
            updated.status, 200,
            "case {:?}: expected HTTP 200 from UpdateFieldLevelEncryptionConfig, got {} with {}",
            row.case, updated.status, updated.raw
        );
        let fresh = updated.header("etag").to_string();
        assert!(
            is_version_token(&fresh) && fresh != etag,
            "case {:?}: expected a fresh version token from the update, got {fresh:?} after \
             {etag:?}",
            row.case
        );
        let config = updated
            .root()
            .descendant("FieldLevelEncryptionConfig")
            .unwrap_or_else(|| {
                panic!(
                    "case {:?}: expected a FieldLevelEncryptionConfig from the update, got {}",
                    row.case, updated.raw
                )
            });
        assert_stored_mapping(
            config,
            row.entries,
            profile.as_str(),
            row.case,
            "the update's own answer",
            &updated.raw,
        );
        assert_reads(&router, &id, row, &profile, "after the update").await;
    }
}
