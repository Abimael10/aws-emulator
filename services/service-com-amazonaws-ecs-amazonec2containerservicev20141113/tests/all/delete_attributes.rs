// SPDX-License-Identifier: CC0-1.0

//! `DeleteAttributes`, driven end to end over the shared wire harness in
//! [`crate::ecs_wire`].
//!
//! The rule under test: `DeleteAttributes` removes from the resource each entry names —
//! by `targetId`, inside the cluster the request names (or the default cluster when it
//! names none) — the attribute that entry names by `name`, reports the attributes it
//! actually removed against the full ARN of the resource they were on, and refuses, with
//! nothing removed, a request whose cluster is not there (`ClusterNotFoundException`) or
//! whose entry names a resource that cluster does not hold
//! (`TargetNotFoundException`).
//!
//! The docs that rule comes from:
//!
//! * `DeleteAttributes`: "Deletes one or more custom attributes from an Amazon ECS
//!   resource."
//! * `DeleteAttributes$attributes`: "The attributes to delete from your resource. You can
//!   specify up to 10 attributes for each request. For custom attributes, specify the
//!   attribute name and target ID, but don't specify the value."
//! * `DeleteAttributes$cluster`: "If you do not specify a cluster, the default cluster is
//!   assumed."
//! * `DeleteAttributesResponse$attributes`: "A list of attribute objects that were
//!   successfully deleted from your resource."
//! * `Attribute$targetId`: "The ID of the target. You can specify the short form ID for a
//!   resource or the full Amazon Resource Name (ARN)."
//! * `Attribute$targetType`: "This parameter is required if you use the short form ID for
//!   a resource instead of the full ARN."
//! * `TargetNotFoundException`: "The specified target wasn't found."
//! * `ContainerInstance$version`: "Every time a container instance experiences a change
//!   ... the version counter is incremented."
//! * `DeregisterContainerInstance`: "Deregistering a container instance removes the
//!   instance from a cluster."
//!
//! # The attribute WRITE, and the ceiling it is bounded by
//!
//! `PutAttributes` is the other half of the same mutation pair, and its own rule is only
//! observable through this one: a resource carries at most ten of the CALLER's custom
//! attributes ("you can apply up to 10 custom attributes for each resource",
//! `AttributeLimitExceededException`), a write that would take it past that count is refused
//! with HTTP 400 `AttributeLimitExceededException` and applies nothing, and a slot freed
//! with `DeleteAttributes` takes the refused write. The suites at the end of this file pin
//! that rule and every request `PutAttributes` refuses; they share this file rather than
//! taking a `put_attributes.rs` of their own because this crate's `tests/all.rs` is a
//! hand-maintained declaration list that a new suite file would have to be added to.
//!
//! * `PutAttributes`: "Create or update an attribute on an Amazon ECS resource. If the
//!   attribute doesn't exist, it's created. If the attribute exists, its value is replaced
//!   with the specified value."
//! * `PutAttributes$attributes`: "You can specify up to 10 custom attributes for each
//!   resource. You can specify up to 10 attributes in a single call."
//! * `PutAttributesResponse$attributes`: "The attributes applied to your resource."
//! * `Attribute`: "Use attributes to extend the Amazon ECS data model by adding custom
//!   metadata to your resources."
//! * `Attribute$name`: "The `name` must contain between 1 and 128 characters. The name may
//!   contain letters (uppercase and lowercase), numbers, hyphens (-), underscores (_),
//!   forward slashes (/), back slashes (\), or periods (.)."
//! * `Attribute$value`: "The `value` must contain between 1 and 128 characters. It can
//!   contain letters (uppercase and lowercase), numbers, hyphens (-), underscores (_),
//!   periods (.), at signs (@), forward slashes (/), back slashes (\), colons (:), or
//!   spaces. The value can't start or end with a space."

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{
    create_cluster, count, list, ok, refusal, refused, service, ACCOUNT, SIGNED_REGION,
};
use crate::list_attributes::{list_attributes, register_with_attributes, Listed};

/// The cluster every case works in.
const CLUSTER: &str = "demo";

/// The EC2 instance the fixture registers its container instance for.
const INSTANCE_ID: &str = "i-0aaa0000000000001";

/// The sentence a `TargetNotFoundException` carries, whichever resource the entry named:
/// the condition is a property of the request, so the text is the same for every entry
/// that meets it.
const TARGET_NOT_FOUND: &str = "The specified target wasn't found.";

/// One row of the deletion table: the case name, the entries it sends (built from the
/// target's full ARN and its short form id) and the names of the attributes the call must
/// report as deleted.
type DeletionRow = (&'static str, fn(&str, &str) -> Value, &'static [&'static str]);

/// One row of the refusal table: the case name, the ONE `attributes` entry it sends, and
/// the error code and sentence it must be refused with.
type RefusalRow = (&'static str, Value, &'static str, &'static str);

/// The attributes a fresh fixture stores against its container instance. The rows differ
/// along what the rule generalises over: a plain name, a dotted name, a name carrying a
/// slash, an attribute stored with NO value, and a value carrying punctuation.
fn seeded() -> Value {
    json!([
        { "name": "stack", "value": "production" },
        { "name": "ecs.os-type", "value": "linux" },
        { "name": "fleet.id/west", "value": "a-1" },
        { "name": "ecs.capability.external" },
        { "name": "tier", "value": "gpu@1" },
    ])
}

/// The name and value of every attribute [`seeded`] stores, an attribute stored without a
/// value reading as the empty string.
fn seeded_pairs() -> Vec<(String, String)> {
    seeded()
        .as_array()
        .expect("the seed is a list")
        .iter()
        .map(|attribute| {
            (
                attribute["name"]
                    .as_str()
                    .expect("each seeded attribute is named")
                    .to_string(),
                attribute["value"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

/// The value [`seeded`] stores against `name`.
fn seeded_value(name: &str) -> String {
    seeded_pairs()
        .into_iter()
        .find(|(seeded_name, _)| seeded_name == name)
        .map(|(_, value)| value)
        .unwrap_or_else(|| panic!("the seed holds an attribute named {name:?}"))
}

/// The listing [`seeded`] implies for the resource at `arn`, minus the names in
/// `without`, in the order `ListAttributes` reports it.
fn seeded_listing(arn: &str, without: &[&str]) -> Vec<Listed> {
    let mut expected: Vec<Listed> = seeded_pairs()
        .into_iter()
        .filter(|(name, _)| !without.contains(&name.as_str()))
        .map(|(name, value)| (arn.to_string(), name, value))
        .collect();
    expected.sort();
    expected
}

/// The short form id of a container instance: the last segment of its ARN
/// (`arn:aws:ecs:<region>:<account>:container-instance/<cluster>/<id>`) — the other form
/// `Attribute$targetId` documents beside the full ARN.
fn short_id(arn: &str) -> &str {
    arn.rsplit('/')
        .next()
        .expect("a container-instance ARN ends in its id")
}

/// A router holding one cluster whose single container instance carries [`seeded`], and
/// that instance's ARN.
async fn fixture(case: &str) -> (MultiServiceRouter, String) {
    let router = service();
    create_cluster(&router, CLUSTER, case).await;
    let arn = register_with_attributes(&router, CLUSTER, INSTANCE_ID, &seeded(), case).await;
    (router, arn)
}

/// The attributes one `DeleteAttributes` call reports as deleted, each reduced to the
/// target ARN, name and value a client keys on — the same shape
/// [`crate::list_attributes`] reduces a listing to, so the two can be compared.
///
/// Every entry is checked to carry no `targetType`: the response names its target by full
/// ARN, which is the form `Attribute$targetType` is NOT required beside.
async fn delete_attributes(
    router: &MultiServiceRouter,
    request: Value,
    case: &str,
) -> Vec<Listed> {
    let body = ok(router, "DeleteAttributes", request, case).await;
    list(&body, "attributes", case)
        .iter()
        .map(|attribute| {
            assert!(
                attribute.get("targetType").and_then(Value::as_str).is_none(),
                "case {case:?}: expected no targetType beside a full-ARN targetId, \
                 got {attribute}"
            );
            let target_id = attribute["targetId"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: expected a targetId, got {attribute}"))
                .to_string();
            let name = attribute["name"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: expected a name, got {attribute}"))
                .to_string();
            let value = attribute["value"].as_str().unwrap_or_default().to_string();
            (target_id, name, value)
        })
        .collect()
}

/// The attribute names and the `version` `DescribeContainerInstances` reports for the
/// instance at `arn` — the resource read back through its own reader.
async fn described(router: &MultiServiceRouter, arn: &str, case: &str) -> (Vec<String>, i64) {
    let body = ok(
        router,
        "DescribeContainerInstances",
        json!({ "cluster": CLUSTER, "containerInstances": [arn] }),
        case,
    )
    .await;
    let instances = list(&body, "containerInstances", case);
    let instance = instances
        .first()
        .unwrap_or_else(|| panic!("case {case:?}: expected the described instance, got {body}"));
    let mut names: Vec<String> = list(instance, "attributes", case)
        .iter()
        .map(|attribute| {
            attribute["name"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: expected a name, got {attribute}"))
                .to_string()
        })
        .collect();
    names.sort();
    (names, count(instance, "version", case))
}

#[tokio::test]
async fn the_named_attributes_are_deleted_from_their_target_and_reported() {
    // Each row names its target one of the two documented ways, and differs along what
    // the rule generalises over: how many attributes one request deletes, whether the
    // attribute has a value, whether the entry carries a value of its own, whether the
    // name is one the resource carries, and a repeated entry.
    let rows: [DeletionRow; 8] = [
        (
            "one attribute, target by full ARN",
            |arn, _| json!([{ "name": "stack", "targetId": arn }]),
            &["stack"],
        ),
        (
            "one attribute, target by short form id and targetType",
            |_, short| {
                json!([{
                    "name": "ecs.os-type",
                    "targetId": short,
                    "targetType": "container-instance",
                }])
            },
            &["ecs.os-type"],
        ),
        (
            "an attribute stored without a value",
            |arn, _| json!([{ "name": "ecs.capability.external", "targetId": arn }]),
            &["ecs.capability.external"],
        ),
        (
            "a name carrying a slash and a period",
            |arn, _| json!([{ "name": "fleet.id/west", "targetId": arn }]),
            &["fleet.id/west"],
        ),
        (
            "three attributes in one request, named both ways",
            |arn, short| {
                json!([
                    { "name": "stack", "targetId": arn },
                    { "name": "tier", "targetId": short, "targetType": "container-instance" },
                    { "name": "ecs.capability.external", "targetId": arn },
                ])
            },
            &["stack", "tier", "ecs.capability.external"],
        ),
        (
            "a name the resource does not carry deletes nothing",
            |arn, _| json!([{ "name": "ecs.vpc-id", "targetId": arn }]),
            &[],
        ),
        (
            "an entry carrying a value selects by name alone",
            |arn, _| json!([{ "name": "stack", "value": "staging", "targetId": arn }]),
            &["stack"],
        ),
        (
            "the same entry twice deletes it once",
            |arn, _| {
                json!([
                    { "name": "tier", "targetId": arn },
                    { "name": "tier", "targetId": arn },
                ])
            },
            &["tier"],
        ),
    ];

    for (case, build, deleted_names) in rows {
        let (router, arn) = fixture(case).await;
        let entries = build(&arn, short_id(&arn));

        let mut reported = delete_attributes(
            &router,
            json!({ "cluster": CLUSTER, "attributes": entries }),
            case,
        )
        .await;
        reported.sort();
        let mut expected: Vec<Listed> = deleted_names
            .iter()
            .map(|name| (arn.clone(), name.to_string(), seeded_value(name)))
            .collect();
        expected.sort();
        assert_eq!(
            reported, expected,
            "case {case:?}: expected the attributes this call removed, got {reported:?}"
        );

        // What the resource carries afterwards is the seeded set minus exactly those.
        let (listed, next) = list_attributes(
            &router,
            json!({ "cluster": CLUSTER, "targetType": "container-instance" }),
            case,
        )
        .await;
        let remaining = seeded_listing(&arn, deleted_names);
        assert_eq!(
            listed, remaining,
            "case {case:?}: expected the stored attributes minus {deleted_names:?}, \
             got {listed:?}"
        );
        assert_eq!(
            next, None,
            "case {case:?}: expected no nextToken for a complete listing, got {next:?}"
        );
    }
}

#[tokio::test]
async fn a_deleted_attribute_is_gone_from_the_resource_and_a_second_delete_removes_nothing() {
    let case = "round trip";
    let (router, arn) = fixture(case).await;
    let (names_before, version_before) = described(&router, &arn, case).await;
    assert!(
        names_before.contains(&"stack".to_string()),
        "case {case:?}: expected the seeded attribute on the resource, got {names_before:?}"
    );

    let request = json!({
        "cluster": CLUSTER,
        "attributes": [{ "name": "stack", "targetId": arn }],
    });
    let deleted = delete_attributes(&router, request.clone(), case).await;
    assert_eq!(
        deleted,
        vec![(arn.clone(), "stack".to_string(), "production".to_string())],
        "case {case:?}: expected the deleted attribute reported against its target, \
         got {deleted:?}"
    );

    // The resource read back through its own reader no longer carries it, and the change
    // moved its version counter.
    let (names_after, version_after) = described(&router, &arn, case).await;
    assert!(
        !names_after.contains(&"stack".to_string()),
        "case {case:?}: expected the attribute to be gone from the resource, \
         got {names_after:?}"
    );
    assert_eq!(
        version_after,
        version_before + 1,
        "case {case:?}: expected the version counter to move once, got {version_after}"
    );

    // The same call again: there is nothing left to remove, so it removes nothing, reports
    // nothing and leaves the resource — version included — as it stands.
    let again = delete_attributes(&router, request, case).await;
    assert!(
        again.is_empty(),
        "case {case:?}: expected a second delete to report nothing deleted, got {again:?}"
    );
    let (names_again, version_again) = described(&router, &arn, case).await;
    assert_eq!(
        (names_again, version_again),
        (names_after, version_after),
        "case {case:?}: expected a call that removed nothing to change nothing"
    );
}

#[tokio::test]
async fn up_to_ten_attributes_go_in_one_request_and_one_more_is_refused() {
    let case = "ten per request";
    let router = service();
    create_cluster(&router, CLUSTER, case).await;
    // "You can specify up to 10 attributes for each request": the published bound itself
    // is accepted, and one past it is refused.
    let stored: Vec<Value> = (1..=10)
        .map(|index| json!({ "name": format!("probe-{index}"), "value": format!("v{index}") }))
        .collect();
    let arn = register_with_attributes(
        &router,
        CLUSTER,
        "i-0bbb0000000000002",
        &json!(stored),
        case,
    )
    .await;

    let entries: Vec<Value> = (1..=10)
        .map(|index| json!({ "name": format!("probe-{index}"), "targetId": arn }))
        .collect();
    let deleted = delete_attributes(
        &router,
        json!({ "cluster": CLUSTER, "attributes": entries }),
        case,
    )
    .await;
    let mut expected: Vec<Listed> = (1..=10)
        .map(|index| (arn.clone(), format!("probe-{index}"), format!("v{index}")))
        .collect();
    expected.sort();
    let mut reported = deleted;
    reported.sort();
    assert_eq!(
        reported, expected,
        "case {case:?}: expected all ten attributes deleted in one request, got {reported:?}"
    );
    let (listed, _) = list_attributes(
        &router,
        json!({ "cluster": CLUSTER, "targetType": "container-instance" }),
        case,
    )
    .await;
    assert!(
        listed.is_empty(),
        "case {case:?}: expected the resource to carry no attribute afterwards, got {listed:?}"
    );

    let too_many: Vec<Value> = (1..=11)
        .map(|index| json!({ "name": format!("probe-{index}"), "targetId": arn }))
        .collect();
    let (code, message) = refusal(
        &router,
        "DeleteAttributes",
        json!({ "cluster": CLUSTER, "attributes": too_many }),
        case,
    )
    .await;
    assert_eq!(
        (code.as_str(), message.as_str()),
        (
            "InvalidParameterException",
            "attributes can have at most 10 items."
        ),
        "case {case:?}: expected eleven attributes to be refused on the documented bound"
    );
}

#[tokio::test]
async fn an_unusable_request_is_refused_and_deletes_nothing() {
    let case = "refusals";
    let (router, arn) = fixture(case).await;

    // A resource of ANOTHER cluster, and one this cluster no longer holds: neither is a
    // target of a request against `CLUSTER`.
    create_cluster(&router, "other", case).await;
    let other_arn = register_with_attributes(
        &router,
        "other",
        "i-0ccc0000000000003",
        &json!([{ "name": "stack", "value": "staging" }]),
        case,
    )
    .await;
    let deregistered = register_with_attributes(
        &router,
        CLUSTER,
        "i-0ddd0000000000004",
        &json!([{ "name": "stack", "value": "draining" }]),
        case,
    )
    .await;
    ok(
        &router,
        "DeregisterContainerInstance",
        json!({ "cluster": CLUSTER, "containerInstance": deregistered }),
        case,
    )
    .await;

    // A cluster that is not there — including the default cluster, which this router has
    // none of.
    let missing: [(&str, Value); 3] = [
        ("a cluster that was never created", json!("no-such-cluster")),
        (
            "a cluster ARN that names no cluster",
            json!(format!(
                "arn:aws:ecs:{SIGNED_REGION}:{ACCOUNT}:cluster/no-such-cluster"
            )),
        ),
        ("the assumed default cluster", Value::Null),
    ];
    for (row, cluster) in missing {
        let mut request = json!({ "attributes": [{ "name": "stack", "targetId": arn }] });
        if let Value::String(cluster) = &cluster {
            request["cluster"] = json!(cluster);
        }
        refused(
            &router,
            "DeleteAttributes",
            request,
            "ClusterNotFoundException",
            row,
        )
        .await;
    }

    let unusable: [RefusalRow; 7] = [
        (
            "a short form id nothing minted",
            json!({
                "name": "stack",
                "targetId": "0".repeat(32),
                "targetType": "container-instance",
            }),
            "TargetNotFoundException",
            TARGET_NOT_FOUND,
        ),
        (
            "a container-instance ARN of another cluster",
            json!({ "name": "stack", "targetId": other_arn }),
            "TargetNotFoundException",
            TARGET_NOT_FOUND,
        ),
        (
            "a container-instance ARN this cluster no longer holds",
            json!({ "name": "stack", "targetId": deregistered }),
            "TargetNotFoundException",
            TARGET_NOT_FOUND,
        ),
        (
            "a container-instance ARN nothing minted",
            json!({
                "name": "stack",
                "targetId": format!(
                    "arn:aws:ecs:{SIGNED_REGION}:{ACCOUNT}:container-instance/{CLUSTER}/{}",
                    "f".repeat(32)
                ),
            }),
            "TargetNotFoundException",
            TARGET_NOT_FOUND,
        ),
        (
            "an entry that names no target",
            json!({ "name": "stack" }),
            "InvalidParameterException",
            "attributes.targetId is required to name the resource the attribute is on",
        ),
        (
            "an entry whose targetId is blank",
            json!({ "name": "stack", "targetId": "   ", "targetType": "container-instance" }),
            "InvalidParameterException",
            "attributes.targetId is required to name the resource the attribute is on",
        ),
        (
            "a short form id with no targetType",
            json!({ "name": "stack", "targetId": short_id(&arn) }),
            "InvalidParameterException",
            "attributes.targetType is required when targetId is the short form id of a resource",
        ),
    ];
    for (row, entry, expected_code, expected_message) in unusable {
        let (code, message) = refusal(
            &router,
            "DeleteAttributes",
            json!({ "cluster": CLUSTER, "attributes": [entry] }),
            row,
        )
        .await;
        assert_eq!(
            (code.as_str(), message.as_str()),
            (expected_code, expected_message),
            "case {row:?}: expected the documented refusal for this entry"
        );
    }

    // A request whose entries are not ALL usable deletes none of them.
    refused(
        &router,
        "DeleteAttributes",
        json!({
            "cluster": CLUSTER,
            "attributes": [
                { "name": "stack", "targetId": arn },
                { "name": "tier", "targetId": other_arn },
            ],
        }),
        "TargetNotFoundException",
        "one usable entry beside an unusable one",
    )
    .await;

    // Every refusal above left the store as it was.
    let (listed, _) = list_attributes(
        &router,
        json!({ "cluster": CLUSTER, "targetType": "container-instance" }),
        case,
    )
    .await;
    assert_eq!(
        listed,
        seeded_listing(&arn, &[]),
        "case {case:?}: expected a refused request to delete nothing, got {listed:?}"
    );
}

/// How many CUSTOM attributes one resource may carry: "You can apply up to 10 custom
/// attributes for each resource" (`AttributeLimitExceededException`).
const CEILING: usize = 10;

/// The sentence `AttributeLimitExceededException` carries, whichever resource the write
/// addressed: fixed text, with no part read off the request or the store.
const LIMIT_EXCEEDED: &str = "Attribute per target limit exceeded.";

/// The names of the attributes a fresh registration carries, as the agent publishes them,
/// read through this crate's own reading of which names are the CALLER's
/// (`state::is_custom_attribute_name`) rather than through a second copy of that rule.
fn is_custom_attribute(name: &str) -> bool {
    service_com_amazonaws_ecs_amazonec2containerservicev20141113::state::is_custom_attribute_name(
        name,
    )
}

/// The sentence a `name` outside the rule `Attribute$name` documents is refused with.
const NAME_PATTERN_MESSAGE: &str = r"attributes.name must match ^[a-zA-Z0-9\-_/\\.]{1,128}$";

/// The same for a `value` outside the rule `Attribute$value` documents.
const VALUE_PATTERN_MESSAGE: &str = concat!(
    r"attributes.value must match ",
    r"^[a-zA-Z0-9\-_.@/\\:]([a-zA-Z0-9\-_.@/\\: ]{0,126}[a-zA-Z0-9\-_.@/\\:])?$",
);

/// The attributes the agent of a fresh registration publishes about the machine it runs on:
/// every one of them in Amazon ECS's own namespaces, so none is a custom attribute.
fn published() -> Value {
    json!([
        { "name": "ecs.instance-type", "value": "t3.micro" },
        { "name": "ecs.availability-zone", "value": "us-west-2a" },
        { "name": "ecs.os-type", "value": "linux" },
        { "name": "com.amazonaws.ecs.capability.privileged-container" },
    ])
}

/// The CUSTOM attributes `ListAttributes` reports for the resource at `arn`, as name and
/// value, in name order — walked page by page, so a listing longer than one page cannot
/// hide one.
async fn custom_attributes(
    router: &MultiServiceRouter,
    arn: &str,
    case: &str,
) -> Vec<(String, String)> {
    let mut custom: Vec<(String, String)> = Vec::new();
    let mut token: Option<String> = None;
    for _ in 0..50 {
        let mut request = json!({ "cluster": CLUSTER, "targetType": "container-instance" });
        if let Some(token) = &token {
            request["nextToken"] = json!(token);
        }
        let (listed, next) = list_attributes(router, request, case).await;
        custom.extend(
            listed
                .into_iter()
                .filter(|(target_id, name, _)| target_id == arn && is_custom_attribute(name))
                .map(|(_, name, value)| (name, value)),
        );
        match next {
            Some(next) => token = Some(next),
            None => {
                custom.sort();
                return custom;
            }
        }
    }
    panic!("case {case:?}: the listing of {CLUSTER:?} did not terminate");
}

/// The attributes one `PutAttributes` reports as applied, each reduced to the target ARN,
/// the name and the value a client keys on — the same shape [`crate::list_attributes`]
/// reduces a listing to.
///
/// Every entry is checked to carry no `targetType`: the response names its target by full
/// ARN, which is the form `Attribute$targetType` is NOT required beside.
async fn put_attributes(router: &MultiServiceRouter, request: Value, case: &str) -> Vec<Listed> {
    let body = ok(router, "PutAttributes", request, case).await;
    let mut applied: Vec<Listed> = list(&body, "attributes", case)
        .iter()
        .map(|attribute| {
            assert!(
                attribute.get("targetType").and_then(Value::as_str).is_none(),
                "case {case:?}: expected no targetType beside a full-ARN targetId, \
                 got {attribute}"
            );
            let target_id = attribute["targetId"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: expected a targetId, got {attribute}"))
                .to_string();
            let name = attribute["name"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: expected a name, got {attribute}"))
                .to_string();
            let value = attribute["value"].as_str().unwrap_or_default().to_string();
            (target_id, name, value)
        })
        .collect();
    applied.sort();
    applied
}

/// The name and the value of the `index`th attribute a write row applies.
fn probe(index: usize) -> (String, String) {
    (format!("probe-{index}"), format!("v{index}"))
}

/// One row of the attribute-WRITE table: how its ten custom attributes are spread over
/// calls, and which of the two documented forms its entries name the target in.
struct WriteRow {
    case: &'static str,
    /// The EC2 instance the row's own container instance is registered for.
    instance_id: &'static str,
    /// How many of the ten each successive `PutAttributes` carries; the batches sum to ten.
    batches: &'static [usize],
    /// Whether the entries name the target by its short form id (with a `targetType` beside
    /// it) rather than by its full ARN.
    by_short_id: bool,
}

/// The rows differ along what the rule generalises over: how many calls the ten attributes
/// arrive in (one call carrying the documented per-call bound, one at a time, and two
/// uneven batches either way round) and the form the entry names its target in.
const WRITE_ROWS: [WriteRow; 4] = [
    WriteRow {
        case: "ten in one call, the target named by full ARN",
        instance_id: "i-0a110000000000001",
        batches: &[10],
        by_short_id: false,
    },
    WriteRow {
        case: "ten written one at a time, the target named by short form id",
        instance_id: "i-0a110000000000002",
        batches: &[1, 1, 1, 1, 1, 1, 1, 1, 1, 1],
        by_short_id: true,
    },
    WriteRow {
        case: "ten in two uneven batches, the target named by full ARN",
        instance_id: "i-0a110000000000003",
        batches: &[4, 6],
        by_short_id: false,
    },
    WriteRow {
        case: "ten in two batches the other way round, by short form id",
        instance_id: "i-0a110000000000004",
        batches: &[7, 3],
        by_short_id: true,
    },
];

impl WriteRow {
    /// One `attributes` entry of this row: the target named the way the row names it.
    fn entry(&self, target: &str, name: &str, value: &str) -> Value {
        if self.by_short_id {
            json!({
                "name": name,
                "value": value,
                "targetId": short_id(target),
                "targetType": "container-instance",
            })
        } else {
            json!({ "name": name, "value": value, "targetId": target })
        }
    }
}

#[tokio::test]
async fn ten_custom_attributes_apply_to_one_target_and_an_eleventh_is_refused() {
    for row in WRITE_ROWS {
        let case = row.case;
        let router = service();
        create_cluster(&router, CLUSTER, case).await;
        let arn =
            register_with_attributes(&router, CLUSTER, row.instance_id, &published(), case).await;

        // The attributes the agent published are in Amazon ECS's own namespaces, so the
        // resource starts with no CUSTOM attribute and the full ten are available.
        let custom = custom_attributes(&router, &arn, case).await;
        assert!(
            custom.is_empty(),
            "case {case:?}: expected a fresh registration to carry no custom attribute, \
             got {custom:?}"
        );

        let mut written = 0_usize;
        let mut expected: Vec<(String, String)> = Vec::new();
        for size in row.batches.iter().copied() {
            let entries: Vec<Value> = (1..=size)
                .map(|offset| {
                    let (name, value) = probe(written + offset);
                    expected.push((name.clone(), value.clone()));
                    row.entry(&arn, &name, &value)
                })
                .collect();
            let applied = put_attributes(
                &router,
                json!({ "cluster": CLUSTER, "attributes": entries }),
                case,
            )
            .await;
            let mut wanted: Vec<Listed> = (1..=size)
                .map(|offset| {
                    let (name, value) = probe(written + offset);
                    (arn.clone(), name, value)
                })
                .collect();
            wanted.sort();
            assert_eq!(
                applied, wanted,
                "case {case:?}: expected the call to report the attributes it applied, \
                 got {applied:?}"
            );
            written += size;
        }
        assert_eq!(
            written, CEILING,
            "case {case:?}: the row's batches write the ceiling, got {written}"
        );
        expected.sort();
        let custom = custom_attributes(&router, &arn, case).await;
        assert_eq!(
            custom, expected,
            "case {case:?}: expected the ten written attributes on the resource, got {custom:?}"
        );
        // The attributes the agent published are still there: a caller's writes do not
        // replace what Amazon ECS reports about the resource.
        let (_, version_before) = described(&router, &arn, case).await;

        // One more CUSTOM attribute on the same resource is past the ceiling: refused, with
        // nothing of the call applied — not the refused attribute, and not the version
        // counter of the resource it addressed.
        let (over_name, over_value) = probe(CEILING + 1);
        let eleventh = json!({
            "cluster": CLUSTER,
            "attributes": [row.entry(&arn, &over_name, &over_value)],
        });
        let (code, message) = refusal(&router, "PutAttributes", eleventh.clone(), case).await;
        assert_eq!(
            (code.as_str(), message.as_str()),
            ("AttributeLimitExceededException", LIMIT_EXCEEDED),
            "case {case:?}: expected the documented refusal past the ceiling"
        );
        let after = custom_attributes(&router, &arn, case).await;
        assert_eq!(
            after, expected,
            "case {case:?}: expected a refused write to apply nothing, got {after:?}"
        );
        let (_, version_after) = described(&router, &arn, case).await;
        assert_eq!(
            version_after, version_before,
            "case {case:?}: expected a refused write to leave the resource unchanged, \
             got version {version_after}"
        );

        // A write that REPLACES the value of one of the ten is not past the ceiling: "if the
        // attribute exists, its value is replaced with the specified value", so the resource
        // still carries ten.
        let (replaced_name, _) = probe(3);
        put_attributes(
            &router,
            json!({
                "cluster": CLUSTER,
                "attributes": [row.entry(&arn, &replaced_name, "replaced")],
            }),
            case,
        )
        .await;
        for (name, value) in expected.iter_mut() {
            if *name == replaced_name {
                *value = "replaced".to_string();
            }
        }
        let replaced = custom_attributes(&router, &arn, case).await;
        assert_eq!(
            replaced, expected,
            "case {case:?}: expected the replacement to keep the resource at the ceiling, \
             got {replaced:?}"
        );

        // And once `DeleteAttributes` frees exactly one slot, the identical write succeeds:
        // "you can remove existing attributes on a resource with DeleteAttributes".
        let (freed_name, _) = probe(5);
        ok(
            &router,
            "DeleteAttributes",
            json!({
                "cluster": CLUSTER,
                "attributes": [{ "name": freed_name, "targetId": arn }],
            }),
            case,
        )
        .await;
        expected.retain(|(name, _)| *name != freed_name);
        let applied = put_attributes(&router, eleventh, case).await;
        assert_eq!(
            applied,
            vec![(arn.clone(), over_name.clone(), over_value.clone())],
            "case {case:?}: expected the freed slot to take the write, got {applied:?}"
        );
        expected.push((over_name, over_value));
        expected.sort();
        let converged = custom_attributes(&router, &arn, case).await;
        assert_eq!(
            converged.len(),
            CEILING,
            "case {case:?}: expected the resource back at the ceiling, got {converged:?}"
        );
        assert_eq!(
            converged, expected,
            "case {case:?}: expected the deleted attribute absent and the new one present, \
             got {converged:?}"
        );

        // The attributes the agent published are reported throughout: the ceiling bounds the
        // caller's attributes, not what Amazon ECS says about the resource.
        let (names, _) = described(&router, &arn, case).await;
        for attribute in published().as_array().expect("the published set is a list") {
            let name = attribute["name"]
                .as_str()
                .expect("each published attribute is named")
                .to_string();
            assert!(
                names.contains(&name),
                "case {case:?}: expected {name:?} to survive the caller's writes, got {names:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_write_in_the_services_own_namespace_cannot_grow_one_record_without_limit() {
    let case = "the record's own bound";
    let router = service();
    create_cluster(&router, CLUSTER, case).await;
    let arn =
        register_with_attributes(&router, CLUSTER, "i-0a330000000000001", &published(), case).await;
    let start = published().as_array().expect("the published set is a list").len();

    // A name in Amazon ECS's own namespaces is not one of the caller's custom attributes, so
    // the modelled ceiling does not count it — the record keeps its own bound instead, which
    // a write loop reaches in calls of the documented ten.
    let bound = service_com_amazonaws_ecs_amazonec2containerservicev20141113::state::MAX_ATTRIBUTES_PER_TARGET;
    let mut written = start;
    while written + CEILING <= bound {
        let entries: Vec<Value> = (1..=CEILING)
            .map(|offset| {
                json!({
                    "name": format!("ecs.probe-{}", written + offset),
                    "value": "published",
                    "targetId": arn,
                })
            })
            .collect();
        put_attributes(
            &router,
            json!({ "cluster": CLUSTER, "attributes": entries }),
            case,
        )
        .await;
        written += CEILING;
    }
    let (names, _) = described(&router, &arn, case).await;
    assert_eq!(
        names.len(),
        written,
        "case {case:?}: expected the record to hold every write up to its bound, got {}",
        names.len()
    );
    // None of them consumed the modelled ceiling: the ten custom attributes still go on.
    let custom = custom_attributes(&router, &arn, case).await;
    assert!(
        custom.is_empty(),
        "case {case:?}: expected a write in the service's namespaces to leave the custom \
         ceiling untouched, got {custom:?}"
    );

    // The write that would take the record past its bound is refused, and applies nothing.
    let over: Vec<Value> = (1..=CEILING)
        .map(|offset| {
            json!({
                "name": format!("ecs.probe-{}", written + offset),
                "value": "published",
                "targetId": arn,
            })
        })
        .collect();
    let (code, message) = refusal(
        &router,
        "PutAttributes",
        json!({ "cluster": CLUSTER, "attributes": over }),
        case,
    )
    .await;
    assert_eq!(
        (code.as_str(), message.as_str()),
        (
            "ClientException",
            format!("attributes of a resource can have at most {bound} items.").as_str()
        ),
        "case {case:?}: expected the refusal of a write past the record's own bound"
    );
    let (after, _) = described(&router, &arn, case).await;
    assert_eq!(
        after.len(),
        written,
        "case {case:?}: expected a refused write to apply nothing, got {} attributes",
        after.len()
    );
}

#[tokio::test]
async fn an_unusable_attribute_write_is_refused_and_applies_nothing() {
    let case = "write refusals";
    let router = service();
    create_cluster(&router, CLUSTER, case).await;
    let arn =
        register_with_attributes(&router, CLUSTER, "i-0a220000000000001", &published(), case).await;

    // A resource of ANOTHER cluster, and one this cluster no longer holds: neither is a
    // target of a write addressed to `CLUSTER`.
    create_cluster(&router, "other", case).await;
    let other_arn = register_with_attributes(
        &router,
        "other",
        "i-0a220000000000002",
        &published(),
        case,
    )
    .await;
    let deregistered = register_with_attributes(
        &router,
        CLUSTER,
        "i-0a220000000000003",
        &published(),
        case,
    )
    .await;
    ok(
        &router,
        "DeregisterContainerInstance",
        json!({ "cluster": CLUSTER, "containerInstance": deregistered }),
        case,
    )
    .await;

    // A cluster that is not there — including the default cluster, which this router has
    // none of.
    let missing: [(&str, Value); 3] = [
        ("a cluster that was never created", json!("no-such-cluster")),
        (
            "a cluster ARN that names no cluster",
            json!(format!(
                "arn:aws:ecs:{SIGNED_REGION}:{ACCOUNT}:cluster/no-such-cluster"
            )),
        ),
        ("the assumed default cluster", Value::Null),
    ];
    for (row, cluster) in missing {
        let mut request = json!({
            "attributes": [{ "name": "stack", "value": "production", "targetId": arn }],
        });
        if let Value::String(cluster) = &cluster {
            request["cluster"] = json!(cluster);
        }
        refused(
            &router,
            "PutAttributes",
            request,
            "ClusterNotFoundException",
            row,
        )
        .await;
    }

    // The rows differ along what makes the request unusable: the SIZE of the list, the
    // target an entry names (absent, blank, a short form id with no type, a resource of
    // another cluster, one this cluster no longer holds, one nothing minted) and the NAME or
    // VALUE it carries (empty, one character past the documented length, a character the
    // member does not admit, a space at an end).
    let too_many: Vec<Value> = (1..=CEILING + 1)
        .map(|index| {
            let (name, value) = probe(index);
            json!({ "name": name, "value": value, "targetId": arn })
        })
        .collect();
    let unusable: Vec<(String, Value, &str, String)> = vec![
        (
            "eleven attributes in a single call".to_string(),
            json!(too_many),
            "InvalidParameterException",
            "attributes can have at most 10 items.".to_string(),
        ),
        (
            "an entry that names no target".to_string(),
            json!([{ "name": "stack", "value": "production" }]),
            "InvalidParameterException",
            "attributes.targetId is required to name the resource the attribute is on"
                .to_string(),
        ),
        (
            "an entry whose targetId is blank".to_string(),
            json!([{
                "name": "stack",
                "value": "production",
                "targetId": "   ",
                "targetType": "container-instance",
            }]),
            "InvalidParameterException",
            "attributes.targetId is required to name the resource the attribute is on"
                .to_string(),
        ),
        (
            "a short form id with no targetType".to_string(),
            json!([{ "name": "stack", "value": "production", "targetId": short_id(&arn) }]),
            "InvalidParameterException",
            "attributes.targetType is required when targetId is the short form id of a resource"
                .to_string(),
        ),
        (
            "a container-instance ARN of another cluster".to_string(),
            json!([{ "name": "stack", "value": "production", "targetId": other_arn }]),
            "TargetNotFoundException",
            TARGET_NOT_FOUND.to_string(),
        ),
        (
            "a container-instance ARN this cluster no longer holds".to_string(),
            json!([{ "name": "stack", "value": "production", "targetId": deregistered }]),
            "TargetNotFoundException",
            TARGET_NOT_FOUND.to_string(),
        ),
        (
            "a container-instance ARN nothing minted".to_string(),
            json!([{
                "name": "stack",
                "value": "production",
                "targetId": format!(
                    "arn:aws:ecs:{SIGNED_REGION}:{ACCOUNT}:container-instance/{CLUSTER}/{}",
                    "f".repeat(32)
                ),
            }]),
            "TargetNotFoundException",
            TARGET_NOT_FOUND.to_string(),
        ),
        (
            "a short form id nothing minted".to_string(),
            json!([{
                "name": "stack",
                "value": "production",
                "targetId": "0".repeat(32),
                "targetType": "container-instance",
            }]),
            "TargetNotFoundException",
            TARGET_NOT_FOUND.to_string(),
        ),
        (
            "a name with no character at all".to_string(),
            json!([{ "name": "", "value": "production", "targetId": arn }]),
            "InvalidParameterException",
            NAME_PATTERN_MESSAGE.to_string(),
        ),
        (
            "a name one character past its documented length".to_string(),
            json!([{ "name": "n".repeat(129), "value": "production", "targetId": arn }]),
            "InvalidParameterException",
            NAME_PATTERN_MESSAGE.to_string(),
        ),
        (
            "a name carrying a character the member does not admit".to_string(),
            json!([{ "name": "stack tier", "value": "production", "targetId": arn }]),
            "InvalidParameterException",
            NAME_PATTERN_MESSAGE.to_string(),
        ),
        (
            "a value with no character at all".to_string(),
            json!([{ "name": "stack", "value": "", "targetId": arn }]),
            "InvalidParameterException",
            VALUE_PATTERN_MESSAGE.to_string(),
        ),
        (
            "a value one character past its documented length".to_string(),
            json!([{ "name": "stack", "value": "v".repeat(129), "targetId": arn }]),
            "InvalidParameterException",
            VALUE_PATTERN_MESSAGE.to_string(),
        ),
        (
            "a value that starts with a space".to_string(),
            json!([{ "name": "stack", "value": " production", "targetId": arn }]),
            "InvalidParameterException",
            VALUE_PATTERN_MESSAGE.to_string(),
        ),
        (
            "a value carrying a character the member does not admit".to_string(),
            json!([{ "name": "stack", "value": "production,staging", "targetId": arn }]),
            "InvalidParameterException",
            VALUE_PATTERN_MESSAGE.to_string(),
        ),
        (
            "one usable entry beside an unusable one".to_string(),
            json!([
                { "name": "stack", "value": "production", "targetId": arn },
                { "name": "tier", "value": "gpu", "targetId": other_arn },
            ]),
            "TargetNotFoundException",
            TARGET_NOT_FOUND.to_string(),
        ),
    ];

    for (row, attributes, expected_code, expected_message) in unusable {
        let (code, message) = refusal(
            &router,
            "PutAttributes",
            json!({ "cluster": CLUSTER, "attributes": attributes }),
            &row,
        )
        .await;
        assert_eq!(
            (code.as_str(), message.as_str()),
            (expected_code, expected_message.as_str()),
            "case {row:?}: expected the documented refusal for this request"
        );
    }

    // Every refusal above applied nothing, and the documented per-call bound is accepted
    // afterwards.
    let custom = custom_attributes(&router, &arn, case).await;
    assert!(
        custom.is_empty(),
        "case {case:?}: expected a refused write to apply nothing, got {custom:?}"
    );
    let entries: Vec<Value> = (1..=CEILING)
        .map(|index| {
            let (name, value) = probe(index);
            json!({ "name": name, "value": value, "targetId": arn })
        })
        .collect();
    put_attributes(
        &router,
        json!({ "cluster": CLUSTER, "attributes": entries }),
        case,
    )
    .await;
    let applied = custom_attributes(&router, &arn, case).await;
    assert_eq!(
        applied.len(),
        CEILING,
        "case {case:?}: expected the documented bound to be accepted, got {applied:?}"
    );
}
