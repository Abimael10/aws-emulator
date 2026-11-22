// SPDX-License-Identifier: CC0-1.0

//! What SURVIVES a `DeleteAssetModel` another asset model's hierarchy holds back.
//!
//! The rule this suite pins: a delete of an asset model that at least one OTHER
//! asset model's hierarchy still names as its `childAssetModelId` is not refused
//! synchronously — it is accepted on the asynchronous plane (HTTP 202, with
//! `assetModelStatus.state` = `DELETING`) and the transition it owes settles the
//! model at `FAILED` carrying `assetModelStatus.error` =
//! `ErrorDetails{VALIDATION_ERROR, <the fixed sentence naming the rule>}`, while the
//! model stays exactly where it was: every member `DescribeAssetModel` marks
//! @required is still served, the definition it carries is unchanged, and the
//! listing still reports it. Only once the LAST such reference is gone does the
//! identical delete converge, after which the model reads back absent.
//!
//! The held-back delete IS a write the model accepted, though, so it takes the
//! model to its next `assetModelVersion` — the version counts the writes the model
//! accepted, whichever state each of them settles at — while the version that was
//! last `ACTIVE` stays the one a read scoped to the active version answers from.
//!
//! The rows differ along what the rule generalises over: how many other models
//! name the subject (none, one, two), whether the naming model holds hierarchies
//! besides the one that names it, whether the subject is itself the parent of a
//! further model, what the subject's own definition carries (a composite model
//! among it, so the composite projection is part of what survives), and which
//! spelling the reference and the delete address the subject by (its id, or its
//! `externalId:` alias). The rows where NOTHING names the subject sit beside them,
//! so the suite cannot pass by holding every delete back.
//!
//! What the read serves alongside the body is pinned too: the model keeps answering
//! its `ETag` response header, which moves with the version the accepted delete
//! wrote, and the delete that finally converges reports the subject's own
//! `assetModelId` beside the `DELETING` status.
//!
//! `assetModelStatus` and the @required projection come from
//! `smithy_query(query="operation", operation="DescribeAssetModel")` and
//! `smithy_query(query="shape", shape="AssetModelStatus,AssetModelSummary")`. The
//! diagnosis this suite reads, the request wrappers it drives and the settle wait it
//! polls with are the ones the sibling suites already own
//! (`super::asset_model_settle_failures`, `crate::support`), so the fixed sentence
//! has one spelling in this crate's tests.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::asset_lifecycle::{call, error_code, router};
use crate::asset_model_settle_failures::{
    create, delete, describe, measurement, update, REFERENCED_BY_PARENT, VALIDATION_ERROR,
};
use crate::hierarchies_etags_and_refusals::{describe_e_tag, is_modelled_e_tag};
use crate::support::{named_by_stored_id, settled_asset_model_status};

/// Every member `DescribeAssetModelResponse` marks @required, from
/// `smithy_query(query="operation", operation="DescribeAssetModel")`. A model the
/// held-back delete left in state has to serve all of them.
const REQUIRED_PROJECTION: [&str; 9] = [
    "assetModelId",
    "assetModelArn",
    "assetModelName",
    "assetModelDescription",
    "assetModelProperties",
    "assetModelHierarchies",
    "assetModelCreationDate",
    "assetModelLastUpdateDate",
    "assetModelStatus",
];

/// The members of that projection a held-back delete may NOT change: it states no
/// new definition, so the model's identity, its definition and its creation date
/// read back as they were. `assetModelStatus` is what the delete settles, and
/// `assetModelVersion` is what the accepted write advances — both are asserted on
/// their own below.
const UNCHANGED_BY_A_HELD_BACK_DELETE: [&str; 9] = [
    "assetModelId",
    "assetModelArn",
    "assetModelName",
    "assetModelDescription",
    "assetModelProperties",
    "assetModelHierarchies",
    "assetModelCompositeModels",
    "assetModelCompositeModelSummaries",
    "assetModelCreationDate",
];

/// `assetModelVersion` as the counter it spells.
///
/// The member targets `Version` with the pattern `^(0|([1-9]{1}\d*))$`, from
/// `smithy_query(query="operation", operation="DescribeAssetModel")`, so it is a
/// decimal counter carried as a string — which the spelling is checked against
/// here before it is read as a number.
fn version_of(case: &str, described: &Value) -> u64 {
    let raw = described["assetModelVersion"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no assetModelVersion in {described}"));
    assert!(
        raw == "0"
            || (!raw.starts_with('0') && raw.chars().all(|digit| digit.is_ascii_digit())),
        "case {case:?}: expected a decimal assetModelVersion, got {raw:?}"
    );
    raw.parse()
        .unwrap_or_else(|_| panic!("case {case:?}: unreadable assetModelVersion {raw:?}"))
}

/// `GET /asset-models/{label}?assetModelVersion=ACTIVE`: the model as the version
/// a read scoped to the ACTIVE one answers from.
async fn described_active(router: &MultiServiceRouter, case: &str, label: &str) -> Value {
    let uri = format!("/asset-models/{label}?assetModelVersion=ACTIVE");
    let (status, described) = call(router, "GET", &uri, None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeAssetModel({uri}): got {status} {described}"
    );
    described
}

/// How many single-model pages [`listed`] walks before it gives up: no row here
/// builds more than a handful of asset models, so a walk longer than this means
/// the listing never reported a last page.
const MAX_PAGES: usize = 32;

/// The name the subject is reached under in every row's graph.
const HIERARCHY: &str = "Turbines";

/// `POST /asset-models`, answering the id the accepted create minted.
async fn created(router: &MultiServiceRouter, case: &str, body: Value) -> String {
    let (status, answered) = create(router, body.clone()).await;
    assert_eq!(
        status, 202,
        "case {case:?}: the create {body} was not accepted: got {status} {answered}"
    );
    answered["assetModelId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no assetModelId in {answered}"))
        .to_string()
}

/// Wait for the model `label` to settle and assert it settled `ACTIVE`: the state
/// every model in a row's graph is in before the row's delete lands.
async fn settled_active(router: &MultiServiceRouter, case: &str, label: &str) {
    let settled = settled_asset_model_status(router, label).await;
    assert_eq!(
        settled["state"].as_str(),
        Some("ACTIVE"),
        "case {case:?}: {label} must settle ACTIVE before the delete, got {settled}"
    );
}

/// The `ListAssetModels` summary of `model_id`, walked one page at a time with
/// `maxResults=1`, so a model that survives a held-back delete has to be reported
/// by the paginated listing a client actually uses rather than by one unpaged read.
async fn listed(router: &MultiServiceRouter, case: &str, model_id: &str) -> Option<Value> {
    let mut token: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let uri = match token.as_deref() {
            Some(token) => format!("/asset-models?maxResults=1&nextToken={token}"),
            None => "/asset-models?maxResults=1".to_string(),
        };
        let (status, page) = call(router, "GET", &uri, None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: ListAssetModels({uri}): got {status} {page}"
        );
        let found = page["assetModelSummaries"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|summary| summary["id"].as_str() == Some(model_id))
            .cloned();
        if found.is_some() {
            return found;
        }
        match page["nextToken"].as_str() {
            Some(next) => {
                // The token goes back in a query string as it arrived, so a spelling
                // that needed percent-encoding would resume somewhere else and the
                // walk would end in the page cap below rather than saying why.
                assert!(
                    next.chars()
                        .all(|character| character.is_ascii_alphanumeric()
                            || "-._~".contains(character)),
                    "case {case:?}: expected a nextToken this walk can carry in a query \
                     string, got {next:?}"
                );
                token = Some(next.to_string());
            }
            None => return None,
        }
    }
    panic!("case {case:?}: ListAssetModels never reported a last page")
}

/// The graph a row builds around the asset model it then deletes.
#[derive(Debug, Clone, Copy)]
enum Shape {
    /// Nothing names the subject at all.
    Unreferenced,
    /// Nothing names the subject, which is itself the parent of another model: a
    /// hierarchy the subject DECLARES is not a reference that holds its delete
    /// back, because it goes away with the model declaring it.
    UnreferencedParent,
    /// One other model declares a hierarchy naming the subject.
    OneParent,
    /// One other model declares two hierarchies, only one of which names the
    /// subject — so the rule keys on the reference and not on the holder holding
    /// hierarchies at all.
    ParentWithAnotherHierarchy,
    /// Two other models each declare a hierarchy naming the subject: the delete
    /// stays held back until the LAST of them is dropped.
    TwoParents,
    /// One other model names the subject, which is itself the parent of a third.
    ParentAndGrandchild,
    /// The holder names the subject by its `externalId:` alias, and the delete
    /// addresses the subject the same way.
    ReferencedByExternalId,
    /// One other model names the subject, which carries a composite model of its
    /// own: the composite definitions and the summaries derived from them are part
    /// of the projection the held-back delete leaves untouched.
    WithCompositeModel,
}

/// A model whose hierarchy names the subject, and the hierarchies it keeps once
/// that reference is dropped.
struct Holder {
    id: String,
    name: String,
    keeps: Value,
}

/// The model a row deletes, and the references that hold that delete back in the
/// order the row drops them.
struct Subject {
    id: String,
    /// How the delete and the reads ADDRESS the subject: its id, or its
    /// `externalId:` alias.
    label: String,
    holders: Vec<Holder>,
}

/// Build the row's graph: the subject, whatever sits under it, and the models
/// whose hierarchy names it.
async fn build(router: &MultiServiceRouter, case: &str, shape: Shape) -> Subject {
    // A model the subject declares a hierarchy over, created first so the
    // subject's own definition can name it.
    let under = if matches!(shape, Shape::UnreferencedParent | Shape::ParentAndGrandchild) {
        let under = created(
            router,
            case,
            json!({
                "assetModelName": "Blade",
                "assetModelDescription": "a model under the subject",
                "assetModelProperties": [measurement("Pitch")],
            }),
        )
        .await;
        settled_active(router, case, &under).await;
        Some(under)
    } else {
        None
    };
    let own_hierarchies = match under.as_deref() {
        Some(under) => json!([{"name": "Blades", "childAssetModelId": under}]),
        None => json!([]),
    };

    // The subject carries a description and a property, so its projection holds
    // every member the read marks @required.
    let external_id = matches!(shape, Shape::ReferencedByExternalId).then_some("turbine.under.test");
    let mut subject_body = json!({
        "assetModelName": "Turbine",
        "assetModelDescription": "the model the row deletes",
        "assetModelProperties": [measurement("Power")],
        "assetModelHierarchies": own_hierarchies,
    });
    if let Some(external_id) = external_id {
        subject_body["assetModelExternalId"] = json!(external_id);
    }
    // A composite model on the subject, so `assetModelCompositeModels` and the
    // `assetModelCompositeModelSummaries` derived from it are both non-empty in the
    // projection the held-back delete has to leave as it was.
    if matches!(shape, Shape::WithCompositeModel) {
        subject_body["assetModelCompositeModels"] = json!([{
            "name": "Vibration",
            "type": "CUSTOM",
            "properties": [measurement("Amplitude")],
        }]);
    }
    let id = created(router, case, subject_body).await;
    settled_active(router, case, &id).await;
    // Both the holder's reference and the delete spell the subject this way.
    let label = match external_id {
        Some(external_id) => format!("externalId:{external_id}"),
        None => id.clone(),
    };

    // A model no reference of the row names, so a holder can keep a hierarchy
    // while dropping the one that names the subject.
    let beside = if matches!(shape, Shape::ParentWithAnotherHierarchy) {
        let beside = created(
            router,
            case,
            json!({
                "assetModelName": "Nacelle",
                "assetModelDescription": "a model the holder keeps a hierarchy over",
                "assetModelProperties": [measurement("Yaw")],
            }),
        )
        .await;
        settled_active(router, case, &beside).await;
        Some(beside)
    } else {
        None
    };

    let holder_names: &[&str] = match shape {
        Shape::Unreferenced | Shape::UnreferencedParent => &[],
        Shape::TwoParents => &["Farm", "Fleet"],
        _ => &["Farm"],
    };
    let mut holders = Vec::new();
    for holder_name in holder_names {
        let mut hierarchies = vec![json!({"name": HIERARCHY, "childAssetModelId": label})];
        let keeps = match beside.as_deref() {
            Some(beside) => {
                hierarchies.push(json!({"name": "Nacelles", "childAssetModelId": beside}));
                json!([{"name": "Nacelles", "childAssetModelId": beside}])
            }
            None => json!([]),
        };
        let holder = created(
            router,
            case,
            json!({
                "assetModelName": holder_name,
                "assetModelDescription": "a model whose hierarchy names the subject",
                "assetModelHierarchies": hierarchies,
            }),
        )
        .await;
        settled_active(router, case, &holder).await;
        holders.push(Holder {
            id: holder,
            name: (*holder_name).to_string(),
            keeps,
        });
    }

    Subject { id, label, holders }
}

/// Drop `holder`'s reference to the subject, keeping whatever else it declared,
/// and wait for that update to settle.
async fn drop_reference(router: &MultiServiceRouter, case: &str, holder: &Holder) {
    let (status, answered) = update(
        router,
        &holder.id,
        json!({
            "assetModelName": holder.name,
            "assetModelDescription": "a model whose hierarchy names the subject",
            "assetModelHierarchies": holder.keeps,
        }),
    )
    .await;
    assert_eq!(
        status, 202,
        "case {case:?}: dropping {}'s reference: got {status} {answered}",
        holder.name
    );
    settled_active(router, case, &holder.id).await;
}

/// The whole rule, over one row's graph: every reference still naming the subject
/// holds its delete back at `FAILED` while the model stays served unchanged, and
/// the delete converges only once the last of them is gone.
async fn delete_is_held_back_until_the_last_reference_goes(
    router: &MultiServiceRouter,
    case: &str,
    subject: Subject,
) {
    let (status, before) = describe(router, &subject.label).await;
    assert_eq!(
        status, 200,
        "case {case:?}: describing the subject before the delete: got {status} {before}"
    );

    // The version the subject carries going into each delete: every accepted
    // write advances it by one, and the held-back delete is one of them.
    let mut version = version_of(case, &before);
    // The entity tag the read serves beside the body. It is derived from the stored
    // definition and the version, so an accepted write moves it — and a model the
    // delete left in state still has one to serve.
    let mut tag = describe_e_tag(router, &subject.label).await;
    assert!(
        is_modelled_e_tag(&tag),
        "case {case:?}: expected a modelled ETag before the delete, got {tag:?}"
    );

    for (dropped, holder) in subject.holders.iter().enumerate() {
        let left = subject.holders.len() - dropped;
        let step = format!("case {case:?}: with {left} reference(s) still naming the subject");

        // The call is ACCEPTED: the reference is not a synchronous refusal.
        let (status, answered) = delete(router, &subject.label).await;
        assert_eq!(
            status, 202,
            "{step}: the delete must be accepted, got {status} {answered}"
        );
        assert_eq!(
            answered["assetModelStatus"]["state"].as_str(),
            Some("DELETING"),
            "{step}: the accepted delete reports DELETING, got {answered}"
        );
        assert_eq!(
            answered["assetModelId"].as_str(),
            Some(subject.id.as_str()),
            "{step}: the accepted delete names the subject, got {answered}"
        );

        // It settles FAILED with the fixed diagnosis, on the asynchronous plane.
        let settled = settled_asset_model_status(router, &subject.label).await;
        assert_eq!(
            settled["state"].as_str(),
            Some("FAILED"),
            "{step}: the held-back delete settles FAILED, got {settled}"
        );
        assert_eq!(
            settled["error"]["code"].as_str(),
            Some(VALIDATION_ERROR),
            "{step}: expected a {VALIDATION_ERROR} diagnosis, got {settled}"
        );
        assert_eq!(
            settled["error"]["message"].as_str(),
            Some(REFERENCED_BY_PARENT),
            "{step}: expected {REFERENCED_BY_PARENT:?}, got {settled}"
        );

        // And the model is still THERE, with the projection it already had.
        let (status, after) = describe(router, &subject.label).await;
        assert_eq!(
            status, 200,
            "{step}: the model must stay describable, got {status} {after}"
        );
        for member in REQUIRED_PROJECTION {
            assert!(
                !after[member].is_null(),
                "{step}: the required member {member} is absent from {after}"
            );
        }
        for member in UNCHANGED_BY_A_HELD_BACK_DELETE {
            assert_eq!(
                after[member], before[member],
                "{step}: {member} changed: expected {:?}, got {:?}",
                before[member], after[member]
            );
        }
        // The accepted delete took the model to its next version, and the version
        // it wrote never becomes the ACTIVE one: that stays the last version the
        // model settled ACTIVE at.
        version += 1;
        let read = version_of(case, &after);
        assert_eq!(
            read, version,
            "{step}: expected assetModelVersion {version}, got {read} in {after}"
        );
        let active = described_active(router, case, &subject.label).await;
        assert_eq!(
            active["assetModelStatus"]["state"].as_str(),
            Some("ACTIVE"),
            "{step}: the active version reads ACTIVE, got {active}"
        );
        let active_version = version_of(case, &active);
        assert!(
            active_version < version,
            "{step}: the held-back delete's version {version} must not be the active one, \
             got {active_version} in {active}"
        );
        let summary = listed(router, case, &subject.id).await;
        // The surviving model still answers its ETag header, moved with the version
        // the accepted delete wrote.
        let now_tag = describe_e_tag(router, &subject.label).await;
        assert!(
            is_modelled_e_tag(&now_tag),
            "{step}: the surviving model must still serve a modelled ETag, got {now_tag:?}"
        );
        assert_ne!(
            now_tag, tag,
            "{step}: the accepted delete must advance the ETag, both {tag:?}"
        );
        tag = now_tag;

        let summary = summary
            .unwrap_or_else(|| panic!("{step}: the surviving model is absent from the listing"));
        assert_eq!(
            summary["name"], before["assetModelName"],
            "{step}: the listing must keep the model's name, got {summary}"
        );
        assert_eq!(
            summary["status"], after["assetModelStatus"],
            "{step}: the listing and the read must report one status, got {summary}"
        );

        drop_reference(router, case, holder).await;
    }

    // With the last reference gone the identical delete converges to removal.
    let (status, answered) = delete(router, &subject.label).await;
    assert_eq!(
        status, 202,
        "case {case:?}: with no reference left the delete must be accepted, got {status} {answered}"
    );
    assert_eq!(
        answered["assetModelStatus"]["state"].as_str(),
        Some("DELETING"),
        "case {case:?}: the accepted delete reports DELETING, got {answered}"
    );
    assert_eq!(
        answered["assetModelId"].as_str(),
        Some(subject.id.as_str()),
        "case {case:?}: the converged delete names the subject, got {answered}"
    );
    let (status, gone) = describe(router, &subject.label).await;
    assert_eq!(
        status, 404,
        "case {case:?}: the removed model must read back absent, got {status} {gone}"
    );
    assert_eq!(
        error_code(&gone),
        "ResourceNotFoundException",
        "case {case:?}: unexpected error code in {gone}"
    );
    let still_listed = listed(router, case, &subject.id).await;
    assert!(
        still_listed.is_none(),
        "case {case:?}: the removed model is still listed as {still_listed:?}"
    );
}

#[tokio::test]
async fn a_delete_a_hierarchy_reference_holds_back_leaves_the_model_served_unchanged() {
    // Rows: how many other models name the subject, what else the holder declares,
    // whether the subject is itself a parent, and which spelling names it — beside
    // the graphs that hold no reference to it at all.
    let rows: Vec<(&str, Shape)> = vec![
        ("nothing names the subject", Shape::Unreferenced),
        (
            "the subject is a parent that nothing names",
            Shape::UnreferencedParent,
        ),
        ("one model's hierarchy names the subject", Shape::OneParent),
        (
            "the naming model declares another hierarchy beside it",
            Shape::ParentWithAnotherHierarchy,
        ),
        (
            "two models' hierarchies name the subject",
            Shape::TwoParents,
        ),
        (
            "the named subject is itself a parent",
            Shape::ParentAndGrandchild,
        ),
        (
            "the reference and the delete spell the subject by its external id",
            Shape::ReferencedByExternalId,
        ),
        (
            "the named subject carries a composite model",
            Shape::WithCompositeModel,
        ),
    ];

    for (case, shape) in rows {
        // Each row stands on its own store, so only its own graph is in the one the
        // rule is asked about.
        let router = router();
        let subject = build(&router, case, shape).await;
        delete_is_held_back_until_the_last_reference_goes(&router, case, subject).await;
    }
}

#[tokio::test]
async fn a_held_back_delete_takes_the_model_to_its_next_version() {
    // Rows: how many writes the subject already accepted before the delete lands,
    // so the version the delete settles at is not one constant — and a row where
    // the delete is itself the second held-back one, so the version advances per
    // accepted delete rather than once per model.
    let rows: [(&str, u64, usize); 5] = [
        ("the create is the model's only write", 0, 1),
        ("one update before the delete", 1, 1),
        ("two updates before the delete", 2, 1),
        ("five updates before the delete", 5, 1),
        ("two held-back deletes in a row", 1, 2),
    ];

    for (case, updates, deletes) in rows {
        let router = router();
        let subject = build(&router, case, Shape::OneParent).await;

        for _ in 0..updates {
            // A write that restates the definition the model already carries is
            // still a write the model accepted, so it advances the version too.
            // The property is named by the id the model already holds for it, the
            // way a client resubmitting a stored property keeps it.
            let properties =
                named_by_stored_id(&router, &subject.id, json!([measurement("Power")])).await;
            let (status, answered) = update(
                &router,
                &subject.label,
                json!({
                    "assetModelName": "Turbine",
                    "assetModelDescription": "the model the row deletes",
                    "assetModelProperties": properties,
                }),
            )
            .await;
            assert_eq!(
                status, 202,
                "case {case:?}: the update must be accepted, got {status} {answered}"
            );
            settled_active(&router, case, &subject.id).await;
        }

        let (status, before) = describe(&router, &subject.label).await;
        assert_eq!(
            status, 200,
            "case {case:?}: describing the subject before the delete: got {status} {before}"
        );
        // The create is the model's first version, and each accepted update since
        // took it to the next one.
        let mut expected = version_of(case, &before);
        assert_eq!(
            expected,
            1 + updates,
            "case {case:?}: expected version {} after {updates} update(s), got {expected}",
            1 + updates
        );

        for attempt in 1..=deletes {
            let step = format!("case {case:?}: held-back delete {attempt}");
            let (status, answered) = delete(&router, &subject.label).await;
            assert_eq!(
                status, 202,
                "{step}: the delete must be accepted, got {status} {answered}"
            );
            let settled = settled_asset_model_status(&router, &subject.label).await;
            assert_eq!(
                settled["state"].as_str(),
                Some("FAILED"),
                "{step}: the held-back delete settles FAILED, got {settled}"
            );
            expected += 1;
            let (status, after) = describe(&router, &subject.label).await;
            assert_eq!(
                status, 200,
                "{step}: the model must stay describable, got {status} {after}"
            );
            let read = version_of(case, &after);
            assert_eq!(
                read, expected,
                "{step}: expected assetModelVersion {expected}, got {read} in {after}"
            );
        }

        // What the delete advanced is the LATEST version: the active one is still
        // the last version the model settled ACTIVE at, which is the one the read
        // before the delete reported.
        let active = described_active(&router, case, &subject.label).await;
        assert_eq!(
            version_of(case, &active),
            1 + updates,
            "case {case:?}: the active version must stay {}, got {active}",
            1 + updates
        );
        assert_eq!(
            active["assetModelStatus"]["state"].as_str(),
            Some("ACTIVE"),
            "case {case:?}: the active version reads ACTIVE, got {active}"
        );
    }
}
