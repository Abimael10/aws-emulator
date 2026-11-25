// SPDX-License-Identifier: CC0-1.0

//! The SENTENCES the three attribute APIs refuse a request with, driven end to end over the
//! shared wire harness in [`crate::ecs_wire`].
//!
//! The rule under test: a refusal `PutAttributes`, `DeleteAttributes` or `ListAttributes`
//! answers carries the sentence the SERVICE publishes for that error class — its fixed text,
//! with every variable part taken from the request and from stored state — rather than the
//! documentation of the error shape it is answered under or a clause assembled from the wire
//! member path and the member's pattern. Concretely:
//!
//! * an `attributes` entry whose `targetId` names a container instance the addressed cluster
//!   holds no registered record of is `TargetNotFoundException` with
//!   `Container instance <id> not found.`, the id being the resource's own — the last segment
//!   of an ARN, or the short form id itself — whichever of the two forms
//!   `Attribute$targetId` documents the entry named it in;
//! * an entry naming an instance the cluster DID hold and has since deregistered is the same
//!   error code with `Container instance <id> is inactive.`, so a target that is out of
//!   service is distinguished from one that never existed;
//! * a `targetType` outside the one value `TargetType` models is
//!   `InvalidParameterException` with `Unexpected targetType '<value>'.`, echoing the value
//!   the request carried;
//! * an attribute `value` whose length is outside the bound `Attribute$value` documents is
//!   `InvalidParameterException` with the member's own length sentence, which is entirely
//!   fixed text.
//!
//! Each sentence is asserted over a SET of inputs that differ along what the rule
//! generalises over — the form and the case of the reference, the scope it is resolved in,
//! which of the two mutations carried it, whether the entry carried a value at all, the
//! rejected `targetType` value, and which end of the documented length the text missed —
//! because the fixed text is the requirement and the variable part is read off the request.
//!
//! The docs and the model those sentences come from:
//!
//! * `Attribute$targetId`: "The ID of the target. You can specify the short form ID for a
//!   resource or the full Amazon Resource Name (ARN)."
//! * `Attribute$targetType`: "This parameter is required if you use the short form ID for a
//!   resource instead of the full ARN."
//! * `Attribute$value`: "The `value` must contain between 1 and 128 characters."
//! * `DeregisterContainerInstance`: "Deregistering a container instance removes the instance
//!   from a cluster."
//! * `ListAttributes$targetType` is `@required` and targets `TargetType`, whose one modelled
//!   value is `container-instance`
//!   (`smithy_query(query="shape",shape="TargetType")`).
//! * `PutAttributes`/`DeleteAttributes`/`ListAttributes` each model
//!   `TargetNotFoundException` (the two mutations) and `InvalidParameterException` at HTTP
//!   400.
//!
//! The LENGTH bound spanning both of `Attribute`'s text members is pinned in
//! [`crate::attribute_text_length`], and the reference forms a reachable target is addressed
//! by in [`crate::delete_attributes`]; what this suite adds is the identity the refusal
//! carries across reference forms, letter case and cluster scope, and the two value-worded
//! sentences beside it.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::attribute_text_length::VALUE_LENGTH_MESSAGE;
use crate::delete_attributes::{target_inactive, target_not_found};
use crate::ecs_wire::{create_cluster, ok, refusal, service, ACCOUNT, SIGNED_REGION};
use crate::list_attributes::{list_attributes, register_with_attributes};

/// The cluster every request below addresses.
const CLUSTER: &str = "attribute-sentences";

/// A SECOND cluster of the same account, holding a container instance of its own: an
/// instance registered into it is not one a request addressing [`CLUSTER`] names.
const OTHER_CLUSTER: &str = "attribute-sentences-elsewhere";

/// The error code both mutations answer an entry naming a resource they cannot reach under.
const TARGET_NOT_FOUND: &str = "TargetNotFoundException";

/// The error code every value this API will not take is refused under.
const INVALID_PARAMETER: &str = "InvalidParameterException";

/// The one value `TargetType` models, as the listing's positive control names it.
const MODELLED_TARGET_TYPE: &str = "container-instance";

/// The length bound `Attribute$value` documents, read from the crate's own reading of the
/// member rather than from a second copy of the number.
const MAX_TEXT: usize =
    service_com_amazonaws_ecs_amazonec2containerservicev20141113::state::MAX_ATTRIBUTE_TEXT_LENGTH;

/// One row of the unreachable-target table: the case name, the reference the entry names its
/// target by, and the sentence that reference must be refused with.
type TargetRow = (String, String, String);

/// One row of the refused-value table: the case name, the operation, the request, and the
/// sentence it must be refused with.
type ValueRow = (String, &'static str, Value, String);

/// The container instance id a reference names: the last segment of an ARN, and the value
/// itself for the short form id — the two forms `Attribute$targetId` documents.
fn target_id(reference: &str) -> &str {
    reference
        .rsplit('/')
        .next()
        .expect("a reference always has a last segment")
}

/// The ARN a container instance of `cluster` with `id` would carry, built the way the API
/// publishes one: an id nothing registered still names a well-formed resource of the
/// caller's own cluster.
fn instance_arn(cluster: &str, id: &str) -> String {
    format!("arn:aws:ecs:{SIGNED_REGION}:{ACCOUNT}:container-instance/{cluster}/{id}")
}

/// One `attributes` entry naming `reference`: by full ARN as it stands, or by the short form
/// id with the `targetType` that form requires beside it.
fn entry(name: &str, reference: &str, by_short_id: bool) -> Value {
    if by_short_id {
        json!({
            "name": name,
            "targetId": target_id(reference),
            "targetType": MODELLED_TARGET_TYPE,
        })
    } else {
        json!({ "name": name, "targetId": reference })
    }
}

/// A router holding [`CLUSTER`] with one registered container instance and one it has
/// deregistered, and [`OTHER_CLUSTER`] with a registered instance of its own.
///
/// Answers the live instance's ARN, the deregistered instance's ARN and the ARN of the
/// instance registered into the other cluster.
async fn fixture(case: &str) -> (MultiServiceRouter, String, String, String) {
    let router = service();
    create_cluster(&router, CLUSTER, case).await;
    create_cluster(&router, OTHER_CLUSTER, case).await;
    let live =
        register_with_attributes(&router, CLUSTER, "i-0ccc0000000000011", &json!([]), case).await;
    let elsewhere = register_with_attributes(
        &router,
        OTHER_CLUSTER,
        "i-0ccc0000000000012",
        &json!([]),
        case,
    )
    .await;
    let deregistered =
        register_with_attributes(&router, CLUSTER, "i-0ccc0000000000013", &json!([]), case).await;
    // "Deregistering a container instance removes the instance from a cluster": the record
    // stays, out of service, which is a different condition from never having existed.
    ok(
        &router,
        "DeregisterContainerInstance",
        json!({
            "cluster": CLUSTER,
            "containerInstance": deregistered,
            "force": true,
        }),
        case,
    )
    .await;
    (router, live, deregistered, elsewhere)
}

/// The attributes [`CLUSTER`] reports, as the ARN of the resource each is on with its name
/// and value — the store as a later caller sees it.
async fn carried(router: &MultiServiceRouter, case: &str) -> Vec<(String, String, String)> {
    let (listed, next) = list_attributes(
        router,
        json!({ "cluster": CLUSTER, "targetType": MODELLED_TARGET_TYPE }),
        case,
    )
    .await;
    assert_eq!(
        next, None,
        "case {case:?}: expected the whole listing in one page, got {next:?}"
    );
    let mut listed = listed;
    listed.sort();
    listed
}

/// The refusal of an `attributes` entry naming a resource the request cannot reach names the
/// TARGET, by the id the resource carries, and states which of the two conditions it met.
///
/// The rows differ along what the rule generalises over: the form of the id (the 32-character
/// form a registration mints and the hyphenated 36-character form), its letter case, which of
/// the two reference forms the entry used, the scope the reference resolves in (an id
/// registered in ANOTHER cluster of the same account, named by ARN and by short form id), and
/// whether the target is one nothing registered or one this cluster deregistered. Each row is
/// sent through both mutations, and through `PutAttributes` both with a value and with none,
/// since the refusal is about the target rather than about what the entry carried.
#[tokio::test]
async fn an_unreachable_attribute_target_is_refused_by_the_id_the_request_named() {
    let case = "unreachable targets";
    let (router, live, deregistered, elsewhere) = fixture(case).await;

    // Ids nothing ever registered, inside the caller's own cluster: the 32-character form in
    // mixed case, and the hyphenated form an EC2-launched instance is registered under.
    let mixed_case = instance_arn(CLUSTER, "0123456789ABCDEF0123456789abcdef");
    let hyphenated = instance_arn(CLUSTER, "7b3c1d2e-4f50-4a61-9b72-8c93d4e5f601");

    let mut rows: Vec<TargetRow> = Vec::new();
    for (shape, reference) in [
        ("an id in mixed case", &mixed_case),
        ("an id in the hyphenated form", &hyphenated),
        ("an id registered in another cluster", &elsewhere),
    ] {
        for by_short_id in [false, true] {
            rows.push((
                format!(
                    "{shape}, named by {}",
                    if by_short_id {
                        "the short form id"
                    } else {
                        "the full ARN"
                    }
                ),
                if by_short_id {
                    target_id(reference).to_string()
                } else {
                    reference.to_string()
                },
                target_not_found(reference),
            ));
        }
    }
    for by_short_id in [false, true] {
        rows.push((
            format!(
                "an instance this cluster deregistered, named by {}",
                if by_short_id {
                    "the short form id"
                } else {
                    "the full ARN"
                }
            ),
            if by_short_id {
                target_id(&deregistered).to_string()
            } else {
                deregistered.clone()
            },
            target_inactive(&deregistered),
        ));
    }

    let before = carried(&router, case).await;
    for (row, reference, expected) in &rows {
        let by_short_id = !reference.starts_with("arn:");
        // The same entry through both mutations, and through the write both with a value and
        // with none: a valueless entry is what `DeleteAttributes$attributes` has the caller
        // send, and the target is unreachable either way.
        let sent: [(&str, Value); 3] = [
            ("DeleteAttributes", entry("stack", reference, by_short_id)),
            ("PutAttributes", {
                let mut written = entry("stack", reference, by_short_id);
                written["value"] = json!("production");
                written
            }),
            ("PutAttributes", entry("stack", reference, by_short_id)),
        ];
        for (operation, attribute) in sent {
            let carrying = if attribute.get("value").is_some() {
                "carrying a value"
            } else {
                "carrying no value"
            };
            let case = format!("{operation} against {row}, {carrying}");
            let (code, message) = refusal(
                &router,
                operation,
                json!({ "cluster": CLUSTER, "attributes": [attribute] }),
                &case,
            )
            .await;
            assert_eq!(
                (code.as_str(), message.as_str()),
                (TARGET_NOT_FOUND, expected.as_str()),
                "case {case:?}: expected the refusal to name this target and its state, \
                 got {code} with {message:?}"
            );
        }
    }

    // None of those refusals wrote anything, and the instance this cluster DOES carry still
    // takes a write and gives it back through the listing — so the refusals above are about
    // the target each entry named and nothing else.
    assert_eq!(
        carried(&router, case).await,
        before,
        "case {case:?}: expected a refused request to change nothing"
    );
    ok(
        &router,
        "PutAttributes",
        json!({
            "cluster": CLUSTER,
            "attributes": [{ "name": "stack", "value": "production", "targetId": live }],
        }),
        case,
    )
    .await;
    let listed = carried(&router, case).await;
    assert!(
        listed.contains(&(
            live.clone(),
            "stack".to_string(),
            "production".to_string()
        )),
        "case {case:?}: expected the write against the registered instance in the listing, \
         got {listed:?}"
    );
}

/// A `targetType` the attribute LISTING will not take is refused with the value the request
/// carried echoed into the sentence, and an attribute `value` outside the documented length
/// with the member's own fixed sentence.
///
/// The rows differ along what the rule generalises over: for the listing, another AWS
/// resource kind, the modelled value in another letter case, the modelled value with a
/// trailing space, and a plural of it — none of which is the one value `TargetType` models,
/// and each of which is echoed exactly as sent; for the write, which end of the documented
/// length the text missed. The modelled target type and the texts of an admitted length are
/// the positive controls: they are served.
#[tokio::test]
async fn a_value_these_apis_will_not_take_is_refused_with_the_operations_own_sentence() {
    let case = "refused values";
    let (router, live, _, _) = fixture(case).await;

    let mut rows: Vec<ValueRow> = Vec::new();
    // Values outside the modelled set, none of them the one the member takes.
    for requested in ["service", "CONTAINER-INSTANCE", "container-instance ", "container-instances"]
    {
        rows.push((
            format!("a listing of targetType {requested:?}"),
            "ListAttributes",
            json!({ "cluster": CLUSTER, "targetType": requested }),
            format!("Unexpected targetType '{requested}'."),
        ));
    }
    // A value of no characters at all, and one a single character past the documented
    // maximum: both are the LENGTH rule of `Attribute$value`, which is one fixed sentence.
    for (shape, value) in [
        ("no characters at all", String::new()),
        (
            "one character past the documented maximum",
            "v".repeat(MAX_TEXT + 1),
        ),
    ] {
        rows.push((
            format!("a write whose value carries {shape}"),
            "PutAttributes",
            json!({
                "cluster": CLUSTER,
                "attributes": [{ "name": "stack", "value": value, "targetId": live }],
            }),
            VALUE_LENGTH_MESSAGE.to_string(),
        ));
    }

    let before = carried(&router, case).await;
    for (row, operation, request, expected) in rows {
        let (code, message) = refusal(&router, operation, request, &row).await;
        assert_eq!(
            (code.as_str(), message.as_str()),
            (INVALID_PARAMETER, expected.as_str()),
            "case {row:?}: expected the sentence this operation states, got {code} with \
             {message:?}"
        );
    }
    assert_eq!(
        carried(&router, case).await,
        before,
        "case {case:?}: expected a refused request to change nothing"
    );

    // The positive controls: the modelled target type is served, and a value at either end of
    // the documented length is stored and read back.
    for (shape, value) in [
        ("the documented minimum length", "v".to_string()),
        ("the documented maximum length", "v".repeat(MAX_TEXT)),
    ] {
        let row = format!("a value of {shape}");
        ok(
            &router,
            "PutAttributes",
            json!({
                "cluster": CLUSTER,
                "attributes": [{ "name": "stack", "value": value, "targetId": live }],
            }),
            &row,
        )
        .await;
        let listed = carried(&router, &row).await;
        assert!(
            listed.contains(&(live.clone(), "stack".to_string(), value.clone())),
            "case {row:?}: expected the written value in the listing, got {listed:?}"
        );
    }
}
