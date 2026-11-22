// SPDX-License-Identifier: CC0-1.0

//! The tag plane over a resource's whole LIFE, for every kind of resource this service
//! both creates and holds a tag set on.
//!
//! The rule under test: **a tag request addresses a resource by
//! `(ResourceType, ResourceId)`, and that pair resolves against the records the Region
//! holds for that type AT THE TIME OF THE CALL** — so, for each such kind, the tags its
//! create carried are what a tag read reports, `AddTagsToResource` merges into that same
//! set, `RemoveTagsFromResource` drops from it, and once the resource is deleted its id
//! names no resource at all, which every operation of the tag plane refuses with the
//! modelled `InvalidResourceId` at HTTP 400.
//!
//! The table's rows differ along what the rule generalises over: the KIND of resource
//! addressed (and so which store the pair resolves against, and whether the
//! `ResourceId` is the name the create supplied or an id the create minted), the tags the
//! create carried, how many tags one request adds, whether an added key is one the
//! resource already carries, and which keys a remove names. `all/resource_tags.rs` pins
//! the merge and the removal rules on parameters and documents; this suite pins the same
//! rules on the two kinds whose ids their creates MINT — a maintenance window and a patch
//! baseline — and pins what a DELETE of any of the four leaves behind, which is the state
//! a tag request against a resource that has been removed resolves against. The last
//! suite pins the other side of "the records the Region holds for the account": a patch
//! baseline Systems Manager predefines is not the account's to tag, as a document Systems
//! Manager publishes is not.
//!
//! Every call goes through the front door a client uses — the shared harness in
//! `all/parameter_store.rs` dispatches `POST /` with
//! `X-Amz-Target: AmazonSSM.<Operation>`, and the three tag operations are the one copy
//! that harness holds — and every resource is created and deleted through this service's
//! own operations, so nothing reaches into a state struct.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::parameter_store::{
    add, call, error_code, list, listed, remove, router, tag_list, DOCUMENT_CONTENT,
};

/// A patch a baseline may approve by name, so a create satisfies "Between Approved Patches
/// and Approval Rule, at least one must be non empty" without picking filter criteria an
/// operating system has to accept.
const APPROVED_PATCH: &str = "KB2032276";

/// The error code the tag plane answers for a well-formed id naming no resource the Region
/// holds (`smithy_query(query="operation", operation="AddTagsToResource")`:
/// `InvalidResourceId`, client, HTTP 400).
const INVALID_RESOURCE_ID: &str = "InvalidResourceId";

/// One kind of resource the tag plane addresses: the `ResourceTypeForTagging` value a
/// request declares for it (`smithy_query(query="shape", shape="ResourceTypeForTagging")`),
/// beside the operations that create and delete one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// A Parameter Store parameter, addressed by its name.
    Parameter,
    /// An SSM document, addressed by its name.
    Document,
    /// A maintenance window, addressed by the `mw-` id its create minted.
    MaintenanceWindow,
    /// A patch baseline, addressed by the `pb-` id its create minted.
    PatchBaseline,
}

impl Kind {
    /// The `ResourceType` a tag request declares for this kind.
    fn resource_type(self) -> &'static str {
        match self {
            Kind::Parameter => "Parameter",
            Kind::Document => "Document",
            Kind::MaintenanceWindow => "MaintenanceWindow",
            Kind::PatchBaseline => "PatchBaseline",
        }
    }

    /// Whether the `ResourceId` of this kind is the NAME its create supplied — "For the
    /// Document and Parameter values, use the name of the resource"
    /// (`AddTagsToResourceRequest$ResourceId`) — rather than an id the create minted.
    fn addressed_by_name(self) -> bool {
        matches!(self, Kind::Parameter | Kind::Document)
    }

    /// Create one resource of this kind under `name`, carrying `tags`, and answer the
    /// `ResourceId` a tag request addresses it by.
    async fn create(
        self,
        router: &MultiServiceRouter,
        case: &str,
        name: &str,
        tags: &[(&str, &str)],
    ) -> String {
        let tags = tag_list(tags);
        let (operation, body, minted) = match self {
            Kind::Parameter => (
                "PutParameter",
                json!({ "Name": name, "Type": "String", "Value": "a value", "Tags": tags }),
                None,
            ),
            Kind::Document => (
                "CreateDocument",
                json!({ "Name": name, "Content": DOCUMENT_CONTENT, "Tags": tags }),
                None,
            ),

            Kind::MaintenanceWindow => (
                "CreateMaintenanceWindow",
                json!({
                    "Name": name,
                    "Schedule": "cron(0 16 ? * TUE *)",
                    "Duration": 3,
                    "Cutoff": 1,
                    "AllowUnassociatedTargets": true,
                    "Tags": tags,
                }),
                Some("WindowId"),
            ),
            Kind::PatchBaseline => (
                "CreatePatchBaseline",
                json!({
                    "Name": name,
                    "ApprovedPatches": [APPROVED_PATCH],
                    "Tags": tags,
                }),
                Some("BaselineId"),
            ),
        };
        let (status, response) = call(router, operation, &body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: {operation} {name} must succeed, got {status} {response}"
        );
        match minted {
            None => name.to_string(),
            Some(member) => response[member]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: {operation} must report {member}, got {response}")
                })
                .to_string(),
        }
    }

    /// Delete the resource `resource_id` names, through this service's own delete.
    async fn delete(self, router: &MultiServiceRouter, case: &str, resource_id: &str) {
        let (operation, body) = match self {
            Kind::Parameter => ("DeleteParameter", json!({ "Name": resource_id })),
            Kind::Document => ("DeleteDocument", json!({ "Name": resource_id })),
            Kind::MaintenanceWindow => (
                "DeleteMaintenanceWindow",
                json!({ "WindowId": resource_id }),
            ),
            Kind::PatchBaseline => (
                "DeletePatchBaseline",
                json!({ "BaselineId": resource_id }),
            ),
        };
        let (status, response) = call(router, operation, &body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: {operation} {resource_id} must succeed, got {status} {response}"
        );
    }
}

/// One row: a kind of resource, the name its create supplies, the tags that create
/// carries, the tags one `AddTagsToResource` call then submits, the set the resource must
/// carry afterwards, the keys a remove then names, and the set left after that remove.
struct Row<'a> {
    case: &'a str,
    kind: Kind,
    name: &'a str,
    created_with: &'a [(&'a str, &'a str)],
    added: &'a [(&'a str, &'a str)],
    merged: &'a [(&'a str, &'a str)],
    removed_keys: &'a [&'a str],
    after_removal: &'a [(&'a str, &'a str)],
}

/// The rows, one per kind, each carrying a different tag shape so the merge and the
/// removal are exercised over more than one set.
const ROWS: &[Row] = &[
    Row {
        case: "a parameter in a hierarchy, created untagged",
        kind: Kind::Parameter,
        name: "/TagLife/db/password",
        created_with: &[],
        added: &[("Owner", "DbAdmin"), ("Stack", "Production")],
        merged: &[("Owner", "DbAdmin"), ("Stack", "Production")],
        removed_keys: &["Stack"],
        after_removal: &[("Owner", "DbAdmin")],
    },
    Row {
        case: "a document whose create carried one tag, overwritten by the add",
        kind: Kind::Document,
        name: "TagLife.Document",
        created_with: &[("Stack", "Test")],
        added: &[("Stack", "Pre-Production"), ("Owner", "Dev")],
        merged: &[("Owner", "Dev"), ("Stack", "Pre-Production")],
        removed_keys: &["Owner", "NobodyWroteThis"],
        after_removal: &[("Stack", "Pre-Production")],
    },
    Row {
        case: "a maintenance window, addressed by the id its create minted",
        kind: Kind::MaintenanceWindow,
        name: "TagLife-window",
        created_with: &[("Owner", "SysAdmin")],
        added: &[("cost:center", "4711")],
        merged: &[("Owner", "SysAdmin"), ("cost:center", "4711")],
        removed_keys: &["Owner"],
        after_removal: &[("cost:center", "4711")],
    },
    Row {
        case: "a patch baseline, addressed by the id its create minted",
        kind: Kind::PatchBaseline,
        name: "TagLife-baseline",
        created_with: &[("PatchSeverity", "Critical"), ("OS", "Windows")],
        // An empty value is a modelled one (`TagValue` length 0..256).
        added: &[("OS", "Linux"), ("team", "")],
        merged: &[
            ("OS", "Linux"),
            ("PatchSeverity", "Critical"),
            ("team", ""),
        ],
        removed_keys: &["PatchSeverity", "team"],
        after_removal: &[("OS", "Linux")],
    },
];

/// The pairs sorted the way a report walks them: a tag set is a map from key to value, so
/// a read reports it in key order.
fn in_key_order(pairs: &[(&str, &str)]) -> Value {
    let mut sorted = pairs.to_vec();
    sorted.sort_by(|left, right| left.0.cmp(right.0));
    tag_list(&sorted)
}

/// Assert that `(resource_type, resource_id)` names no resource the Region holds: every
/// operation of the tag plane answers the modelled existence refusal, at its status and
/// with its wire code.
async fn assert_names_no_resource(
    router: &MultiServiceRouter,
    case: &str,
    resource_type: &str,
    resource_id: &str,
) {
    let answers = [
        (
            "AddTagsToResource",
            add(
                router,
                resource_type,
                resource_id,
                &tag_list(&[("Owner", "DbAdmin")]),
            )
            .await,
        ),
        (
            "ListTagsForResource",
            list(router, resource_type, resource_id).await,
        ),
        (
            "RemoveTagsFromResource",
            remove(router, resource_type, resource_id, &["Owner"]).await,
        ),
    ];
    for (operation, (status, response)) in answers {
        assert_eq!(
            status, 400,
            "case {case:?}: {operation} {resource_id} must be refused, got {status} {response}"
        );
        assert_eq!(
            error_code(&response),
            INVALID_RESOURCE_ID,
            "case {case:?}: {operation} {resource_id} must answer {INVALID_RESOURCE_ID}, \
             got {response}"
        );
    }
}

/// RULE: for every kind of resource this service holds a tag set on, the pair a tag
/// request names resolves to that resource's own set — so the create's tags are what a
/// read reports, an add merges into them and a remove drops exactly the keys it names.
#[tokio::test]
async fn every_kind_of_resource_carries_its_own_tag_set() {
    let router = router();

    for row in ROWS {
        let case = row.case;
        let resource_type = row.kind.resource_type();
        let resource_id = row.kind.create(&router, case, row.name, row.created_with).await;

        // Before any tag write the resource carries exactly what its create carried.
        let before = listed(&router, case, resource_type, &resource_id).await;
        assert_eq!(
            before,
            in_key_order(row.created_with),
            "case {case:?}: a read must report the tags the create carried, got {before}"
        );

        let (status, response) = add(&router, resource_type, &resource_id, &tag_list(row.added)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: AddTagsToResource {resource_type} {resource_id} must succeed, \
             got {status} {response}"
        );
        let merged = listed(&router, case, resource_type, &resource_id).await;
        assert_eq!(
            merged,
            in_key_order(row.merged),
            "case {case:?}: the merged set must be what a read reports, got {merged}"
        );

        // A read changes nothing a later caller sees.
        let again = listed(&router, case, resource_type, &resource_id).await;
        assert_eq!(
            again, merged,
            "case {case:?}: a second read must report the same set, got {again}"
        );

        let (status, response) =
            remove(&router, resource_type, &resource_id, row.removed_keys).await;
        assert_eq!(
            status, 200,
            "case {case:?}: RemoveTagsFromResource {resource_type} {resource_id} must succeed, \
             got {status} {response}"
        );
        let left = listed(&router, case, resource_type, &resource_id).await;
        assert_eq!(
            left,
            in_key_order(row.after_removal),
            "case {case:?}: a remove must leave exactly the other keys, got {left}"
        );
    }
}

/// RULE, at the end of a resource's life: a delete takes the resource and its tag set
/// away, so the id it was addressed by names no resource the Region holds and every
/// operation of the tag plane refuses it — while the resources the delete did not address
/// keep the tags they carried.
#[tokio::test]
async fn a_deleted_resource_is_no_longer_addressable_by_the_tag_plane() {
    let router = router();

    // A resource of each kind that is never deleted, so each refusal below is paired with
    // a resource of the SAME kind the Region still holds and still reports tags for.
    let mut bystanders: Vec<(&str, &str, String)> = Vec::new();
    for row in ROWS {
        let case = row.case;
        // A name of its own for a kind addressed by its name; for a kind whose id its
        // create MINTS, the name need not differ at all — the id does — but a distinct one
        // keeps the two creates of a row apart in a failure message.
        let name = format!("{}-kept", row.name);
        let kept = row
            .kind
            .create(&router, case, &name, &[("Owner", "kept")])
            .await;
        bystanders.push((case, row.kind.resource_type(), kept));
    }

    for row in ROWS {
        let case = row.case;
        let resource_type = row.kind.resource_type();
        let resource_id = row.kind.create(&router, case, row.name, row.created_with).await;
        let (status, response) = add(&router, resource_type, &resource_id, &tag_list(row.added)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the resource must be taggable while it is held, got {status} \
             {response}"
        );

        row.kind.delete(&router, case, &resource_id).await;

        assert_names_no_resource(&router, case, resource_type, &resource_id).await;

        // A resource re-created under the same NAME is a NEW resource: it carries the tags
        // its own create carried and none of the deleted resource's, so the tag set went
        // with the resource rather than outliving it in a store of its own.
        if row.kind.addressed_by_name() {
            let recreated = row.kind.create(&router, case, row.name, &[]).await;
            let reported = listed(&router, case, resource_type, &recreated).await;
            assert_eq!(
                reported,
                tag_list(&[]),
                "case {case:?}: a re-created resource must carry only its own create's \
                 tags, got {reported}"
            );
        }
    }

    // The bystanders are untouched: a delete removes the resource it addressed and no other.
    for (case, resource_type, resource_id) in &bystanders {
        let reported = listed(&router, case, resource_type, resource_id).await;
        assert_eq!(
            reported,
            tag_list(&[("Owner", "kept")]),
            "case {case:?}: the resource no delete addressed must keep its tags, \
             got {reported}"
        );
    }
}

/// RULE, on the other side of "the records the Region holds for the ACCOUNT": a patch
/// baseline Systems Manager predefines is owned by Systems Manager rather than by the
/// account, so — exactly as for a document Systems Manager publishes — the account holds
/// no tag set on it and its id addresses no resource of its own to tag, even though every
/// READ of that baseline serves it.
///
/// The rows are the operating systems whose predefined baseline this endpoint reports, so
/// the rule is seen on more than the one default an endpoint answers with no operating
/// system named at all.
#[tokio::test]
async fn a_baseline_systems_manager_predefines_is_not_the_accounts_to_tag() {
    let router = router();

    // A baseline the ACCOUNT created, so the refusals below are about who owns the
    // baseline rather than about the type being untaggable.
    let own = Kind::PatchBaseline
        .create(
            &router,
            "a baseline the account created",
            "TagLife-owned-baseline",
            &[("Owner", "DbAdmin")],
        )
        .await;
    let reported = listed(
        &router,
        "a baseline the account created",
        "PatchBaseline",
        &own,
    )
    .await;
    assert_eq!(
        reported,
        tag_list(&[("Owner", "DbAdmin")]),
        "case \"a baseline the account created\": its own tags must be readable, got {reported}"
    );

    // `OperatingSystem` is optional, and "If you don't specify an operating system value,
    // the default patch baseline for Windows is returned" (`GetDefaultPatchBaseline`'s own
    // doc), so the first row names none at all.
    for (case, operating_system) in [
        ("the default this endpoint answers with no operating system", None),
        ("the predefined WINDOWS baseline", Some("WINDOWS")),
        ("the predefined AMAZON_LINUX_2 baseline", Some("AMAZON_LINUX_2")),
        ("the predefined UBUNTU baseline", Some("UBUNTU")),
    ] {
        let body = match operating_system {
            None => json!({}),
            Some(operating_system) => json!({ "OperatingSystem": operating_system }),
        };
        let (status, response) = call(&router, "GetDefaultPatchBaseline", &body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetDefaultPatchBaseline must succeed, got {status} {response}"
        );
        let reported_id = response["BaselineId"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {case:?}: the read must report a BaselineId, got {response}")
            })
            .to_string();

        // A baseline Systems Manager predefines is reported by its full ARN — "to specify
        // an Amazon Web Services-predefined patch baseline as the default, specify the
        // full patch baseline Amazon Resource Name (ARN)". A tag request names a baseline
        // by the id form `AddTagsToResourceRequest$ResourceId` publishes ("PatchBaseline:
        // pb-012345abcde"), which is the tail of that ARN.
        let baseline_id = reported_id
            .rsplit_once('/')
            .map(|(_, id)| id.to_string())
            .unwrap_or_else(|| reported_id.clone());

        // The id DOES name a baseline every read serves — so what the tag plane refuses is
        // the account tagging a baseline it does not own, not an id of no baseline at all.
        let (status, served) = call(
            &router,
            "GetPatchBaseline",
            &json!({ "BaselineId": reported_id }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetPatchBaseline {reported_id} must serve the predefined \
             baseline, got {status} {served}"
        );

        assert_names_no_resource(&router, case, "PatchBaseline", &baseline_id).await;
    }

    // The account's own baseline still carries exactly the tags it carried: the refusals
    // wrote nothing.
    let after = listed(
        &router,
        "a baseline the account created",
        "PatchBaseline",
        &own,
    )
    .await;
    assert_eq!(
        after,
        tag_list(&[("Owner", "DbAdmin")]),
        "case \"a baseline the account created\": the refusals must write nothing, got {after}"
    );
}
