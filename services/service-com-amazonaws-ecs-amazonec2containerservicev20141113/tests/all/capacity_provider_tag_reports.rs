// SPDX-License-Identifier: CC0-1.0

//! The TAGS a capacity provider holds, as the operations that ACT on one report them —
//! `DeleteCapacityProvider` above all, driven end to end over the shared wire harness in
//! [`crate::ecs_wire`] against `DescribeCapacityProviders` as the reader.
//!
//! The rule under test: an operation that reports a `CapacityProvider` and models no
//! `include` member reports the stored record WHOLE, the tags it holds among them, and a
//! delete leaves those tags exactly as they stood whatever the delete did — so the retired
//! record still carries them. `DescribeCapacityProviders` is the one operation of the three
//! that models `include` (`smithy_query(query="operation",
//! operation="CreateCapacityProvider,UpdateCapacityProvider,DeleteCapacityProvider,DescribeCapacityProviders")`:
//! only the describe has it), and only there does asking for the field decide whether the
//! tags come back.
//!
//! The rows differ along the TAG SET the provider carries — how many tags (none, one,
//! several, and the 50 the `Tag` doc gives as the "Maximum number of tags per resource"),
//! and what one tag is made of (an empty value, which `Tag$value`'s `@length` min of 0
//! allows; the longest key and value those `@length` maxima allow, 128 and 256; and a key
//! and value spelt out of the separator characters `Tag$key`'s own `@pattern` admits) —
//! and along WHAT THE DELETE DID: a provider nothing holds is retired, while one a live
//! cluster still lists is reported with its delete failed. The tag set is read off the same
//! stored record in both.
//!
//! Nothing here reaches into the state struct: every value asserted is read back through a
//! describe.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::capacity_providers::group_arn;
use crate::ecs_wire::{
    associate_capacity_providers, create_auto_scaling_group, create_cluster, list, ok, poll_until,
    service,
};

/// The most tags a resource may carry, from the `Tag` shape's own documentation: "Maximum
/// number of tags per resource - 50" (`Tags` carries the matching `@length` max of 50).
const MAX_TAGS: usize = 50;

/// The longest a tag key may be, from `Tag$key`'s `@length` max: "Maximum key length - 128
/// Unicode characters in UTF-8".
const MAX_KEY_LENGTH: usize = 128;

/// The longest a tag value may be, from `Tag$value`'s `@length` max: "Maximum value length -
/// 256 Unicode characters in UTF-8".
const MAX_VALUE_LENGTH: usize = 256;

/// One capacity provider as `DescribeCapacityProviders` reports it, with its tags asked for
/// when `include_tags` says so (`DescribeCapacityProviders$include`).
async fn describe_provider(
    router: &MultiServiceRouter,
    name: &str,
    include_tags: bool,
    case: &str,
) -> Value {
    let mut request = json!({ "capacityProviders": [name] });
    if include_tags {
        request["include"] = json!(["TAGS"]);
    }
    let body = ok(router, "DescribeCapacityProviders", request, case).await;
    let providers = list(&body, "capacityProviders", case);
    assert_eq!(
        providers.len(),
        1,
        "case {case:?}: expected exactly one described provider, got {body}"
    );
    providers[0].clone()
}

/// Create one capacity provider over its own Auto Scaling group, carrying `tags` — sent as
/// no member at all when the row carries no tag, which is the form a client with nothing to
/// tag sends.
async fn create_provider(
    router: &MultiServiceRouter,
    name: &str,
    group: &str,
    tags: &[Value],
    case: &str,
) -> Value {
    create_auto_scaling_group(router, group, case).await;
    let mut request = json!({
        "name": name,
        "autoScalingGroupProvider": { "autoScalingGroupArn": group_arn(group) },
    });
    if !tags.is_empty() {
        request["tags"] = json!(tags);
    }
    let created = ok(router, "CreateCapacityProvider", request, case).await;
    created["capacityProvider"].clone()
}

/// One row of the tag-set table: the provider it is carried by, and the tags themselves.
struct TagCase {
    case: &'static str,
    /// The provider name stem; the suite derives a provider and a group name from it.
    stem: &'static str,
    /// The tag set the provider is created with.
    tags: fn() -> Vec<Value>,
}

/// The rows: the dimensions the rule generalises over are the NUMBER of tags and the SHAPE
/// of one tag, each within the bounds the model publishes.
fn rows() -> [TagCase; 6] {
    [
        TagCase {
            case: "a provider carrying no tag at all",
            stem: "notags",
            tags: Vec::new,
        },
        TagCase {
            case: "a provider carrying one tag",
            stem: "onetag",
            tags: || vec![json!({ "key": "team", "value": "platform" })],
        },
        TagCase {
            case: "a provider carrying several tags",
            stem: "severaltags",
            tags: || {
                vec![
                    json!({ "key": "env", "value": "test" }),
                    json!({ "key": "owner", "value": "ecs" }),
                    json!({ "key": "tier", "value": "capacity" }),
                ]
            },
        },
        TagCase {
            case: "a provider carrying a tag whose value is empty",
            stem: "emptyvalue",
            tags: || vec![json!({ "key": "placeholder", "value": "" })],
        },
        TagCase {
            case: "a provider carrying the longest key and value the model allows",
            stem: "longesttag",
            tags: || {
                vec![json!({
                    "key": "k".repeat(MAX_KEY_LENGTH),
                    "value": "v".repeat(MAX_VALUE_LENGTH),
                })]
            },
        },
        TagCase {
            case: "a provider carrying the most tags a resource may hold",
            stem: "maxtags",
            tags: || {
                (0..MAX_TAGS)
                    .map(|index| json!({ "key": format!("key{index}"), "value": format!("{index}") }))
                    .collect()
            },
        },
    ]
}

/// The tag set a report carries, as the response renders it.
fn reported_tags(provider: &Value, case: &str) -> Vec<Value> {
    list(provider, "tags", case)
}

#[tokio::test]
async fn a_delete_reports_the_tags_the_retired_provider_holds() {
    let router = service();

    for row in rows() {
        let case = row.case;
        let tags = (row.tags)();
        let provider = format!("{}free", row.stem);
        let created = create_provider(
            &router,
            &provider,
            &format!("asg-{}free", row.stem),
            &tags,
            case,
        )
        .await;
        // The create is the first report with no `include` member of its own, so it already
        // carries the tag set the delete below has to report.
        assert_eq!(
            reported_tags(&created, case),
            tags,
            "case {case:?}: the create reports the tags it stored, got {created}"
        );

        let answer = ok(
            &router,
            "DeleteCapacityProvider",
            json!({ "capacityProvider": provider }),
            case,
        )
        .await;
        let deleted = answer["capacityProvider"].clone();
        assert_eq!(
            deleted["status"], json!("INACTIVE"),
            "case {case:?}: a provider nothing holds is retired by the delete, got {deleted}"
        );
        assert_eq!(
            reported_tags(&deleted, case),
            tags,
            "case {case:?}: the delete reports the provider's tags, got {deleted}"
        );

        // The retired record still holds them: the delete reported state it had rather than
        // echoing the request, and retiring a provider is no reason to forget its metadata.
        let read = describe_provider(&router, &provider, true, case).await;
        assert_eq!(
            reported_tags(&read, case),
            tags,
            "case {case:?}: the tombstone still carries the tags, got {read}"
        );
        // And the one operation that DOES model `include` answers an empty list when the
        // field was not asked for — the difference the member makes, which the delete's
        // report has no member to make.
        let without = describe_provider(&router, &provider, false, case).await;
        assert_eq!(
            reported_tags(&without, case),
            Vec::<Value>::new(),
            "case {case:?}: a describe without the field reports no tag, got {without}"
        );
    }
}

#[tokio::test]
async fn a_delete_an_association_holds_off_reports_the_same_tags() {
    let router = service();
    create_cluster(&router, "tagholder", "setup").await;

    for row in rows() {
        let case = row.case;
        let tags = (row.tags)();
        let provider = format!("{}held", row.stem);
        create_provider(
            &router,
            &provider,
            &format!("asg-{}held", row.stem),
            &tags,
            case,
        )
        .await;
        associate_capacity_providers(&router, "tagholder", &[provider.as_str()], case).await;

        // The delete an association holds off is ANSWERED on the provider, and what it
        // answers is the same stored record — including every tag, which a refused delete
        // has no reason to touch either.
        let answer = ok(
            &router,
            "DeleteCapacityProvider",
            json!({ "capacityProvider": provider, "cluster": "tagholder" }),
            case,
        )
        .await;
        let held = answer["capacityProvider"].clone();
        assert_eq!(
            (held["status"].clone(), held["updateStatus"].clone()),
            (json!("ACTIVE"), json!("DELETE_FAILED")),
            "case {case:?}: the provider keeps its status with its delete failed, got {held}"
        );
        assert_eq!(
            reported_tags(&held, case),
            tags,
            "case {case:?}: a delete that did not happen reports the stored tags, got {held}"
        );
        let read = describe_provider(&router, &provider, true, case).await;
        assert_eq!(
            reported_tags(&read, case),
            tags,
            "case {case:?}: the stored provider still carries the tags, got {read}"
        );

        // Freeing the provider lets the identical delete through, and the retired record
        // reports the very same tag set: the tags are a property of the record and not of
        // which branch the delete took.
        associate_capacity_providers(&router, "tagholder", &[], case).await;
        let answer = ok(
            &router,
            "DeleteCapacityProvider",
            json!({ "capacityProvider": provider, "cluster": "tagholder" }),
            case,
        )
        .await;
        let retired = answer["capacityProvider"].clone();
        assert_eq!(
            retired["status"], json!("INACTIVE"),
            "case {case:?}: the delete of a provider nothing holds goes through, got {retired}"
        );
        assert_eq!(
            reported_tags(&retired, case),
            tags,
            "case {case:?}: the retired provider reports the same tags, got {retired}"
        );
    }
}

#[tokio::test]
async fn an_update_and_a_delete_of_one_provider_report_its_tags_alike() {
    // The rule is about the operations that report a `CapacityProvider` with no `include`
    // member, so it spans the update as well as the delete: one provider, carrying tags, is
    // reported by its create, by an update of its configuration, and by the delete that
    // retires it — the same tag set every time, with the describe in between asked for the
    // field and then not asked for it.
    let router = service();
    let tags = vec![
        json!({ "key": "cost.center/team", "value": "platform-1" }),
        json!({ "key": "owner@example", "value": "ecs=capacity" }),
    ];
    let case = "the create of the tagged provider";
    let created = create_provider(&router, "taggedupdate", "asg-taggedupdate", &tags, case).await;
    assert_eq!(
        reported_tags(&created, case),
        tags,
        "case {case:?}: the create reports the tags it stored, got {created}"
    );

    let case = "an update of the provider's managed scaling";
    let answer = ok(
        &router,
        "UpdateCapacityProvider",
        json!({
            "name": "taggedupdate",
            "autoScalingGroupProvider": {
                "managedScaling": { "status": "ENABLED", "targetCapacity": 75 },
            },
        }),
        case,
    )
    .await;
    let updated = answer["capacityProvider"].clone();
    assert_eq!(
        reported_tags(&updated, case),
        tags,
        "case {case:?}: an update reports the provider's tags and changes none of them, got \
         {updated}"
    );

    // The update's own completion is owed by the clock; the tags do not wait on it, and are
    // still there once it has landed.
    let case = "the provider once the update has landed";
    let settled = poll_until(
        case,
        "the update to land",
        || describe_provider(&router, "taggedupdate", true, case),
        |provider| provider["updateStatus"] == json!("UPDATE_COMPLETE"),
    )
    .await;
    assert_eq!(
        reported_tags(&settled, case),
        tags,
        "case {case:?}: the settled provider carries the same tags, got {settled}"
    );

    let case = "the delete of the updated provider";
    let answer = ok(
        &router,
        "DeleteCapacityProvider",
        json!({ "capacityProvider": "taggedupdate" }),
        case,
    )
    .await;
    let deleted = answer["capacityProvider"].clone();
    assert_eq!(
        reported_tags(&deleted, case),
        tags,
        "case {case:?}: the delete reports the same tags the update did, got {deleted}"
    );
}
