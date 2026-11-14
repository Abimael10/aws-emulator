// SPDX-License-Identifier: CC0-1.0

//! MemoryDB subnet groups, end to end over the wire.
//!
//! Every case drives the SAME path a client does: an `awsJson1_1` `POST /` with
//! `X-Amz-Target: AmazonMemoryDB.<Operation>` through the multi-service router (the shared
//! harness in `cluster_lifecycle`), so what is asserted is the emulator's HTTP behaviour
//! and not a handler called directly.
//!
//! The rules under test, each over a SET of inputs rather than one recorded request:
//!
//! * `CreateSubnetGroup` persists the group and answers the stored record: `Name` folded
//!   to lower case (a MemoryDB resource name is a case-insensitive identifier),
//!   `Description` as given or the service's default when none was given, `Subnets`
//!   carrying the identifiers the request listed in the order it listed them, and an `ARN`
//!   of the form that service's own ARNs take, scoped to the region the request was signed
//!   for;
//! * `DescribeSubnetGroups` answers from that same state — the one group a request names,
//!   or every stored group in name order — so a read reports exactly what the create
//!   persisted, and the tags the create carried are the ones `ListTags` answers for the
//!   ARN it minted;
//! * re-creating a stored group is the modelled already-exists fault; naming a group that
//!   is not stored is the modelled not-found fault, on the read and on the delete; a
//!   subnet identifier that is not an Amazon EC2 subnet id is the modelled `InvalidSubnet`;
//! * `DeleteSubnetGroup` removes the group from that state — it answers the record it
//!   removed and both readers then answer the not-found fault for the name — while a group
//!   a cluster still runs in is refused and left in place.

use serde_json::{json, Value};
use service_com_amazonaws_memorydb_amazonmemorydb::state::MAX_SUBNETS_PER_GROUP;

use crate::cluster_lifecycle::{call, error_code, ok, router};

/// The region every request of this suite is signed for, so each ARN it reads back must
/// carry it rather than a constant baked into the service.
const REGION: &str = "us-west-2";

/// The account id every generated emulator presents (`engine::EMULATOR_ACCOUNT_ID`).
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// A node type MemoryDB publishes, for the cluster that holds a group in use.
const NODE_TYPE: &str = "db.t4g.small";

/// The ACL MemoryDB provides in every account, so a cluster can be created without one
/// being made first (`CreateCluster$ACLName` is required).
const BUILTIN_ACL: &str = "open-access";

/// One `CreateSubnetGroup` request, and what the service must report for it.
///
/// The rows differ along everything the create's rule generalises over: how the name is
/// spelled (lower case, mixed case, digits and hyphens, a long name), whether a
/// description was supplied (one, none, the empty one), how many subnets the group lists
/// and in which order, the identifier LENGTH (the 17-character form AWS mints now and the
/// original 8-character one), and whether tags were supplied.
struct Case {
    case: &'static str,
    /// The name as the REQUEST spells it.
    requested_name: &'static str,
    /// The name the store keys the group under, which every read reports.
    stored_name: &'static str,
    description: Option<&'static str>,
    subnet_ids: &'static [&'static str],
    tags: &'static [(&'static str, &'static str)],
}

const CASES: &[Case] = &[
    Case {
        case: "one subnet, no description and no tags",
        requested_name: "alpha-subnets",
        stored_name: "alpha-subnets",
        description: None,
        subnet_ids: &["subnet-0123456789abcdef0"],
        tags: &[],
    },
    Case {
        case: "a mixed-case name, folded, with a description",
        requested_name: "Beta-Subnets",
        stored_name: "beta-subnets",
        description: Some("the private tier"),
        subnet_ids: &["subnet-00000000000000001", "subnet-00000000000000002"],
        tags: &[],
    },
    Case {
        case: "three subnets whose order is not sorted, with tags",
        requested_name: "gamma-subnets-003",
        stored_name: "gamma-subnets-003",
        description: Some("three zones"),
        subnet_ids: &[
            "subnet-0ccccccccccccccc0",
            "subnet-0aaaaaaaaaaaaaaa0",
            "subnet-0bbbbbbbbbbbbbbb0",
        ],
        tags: &[("team", "platform"), ("env", "test")],
    },
    Case {
        case: "the original 8-character identifier form, and an empty description",
        requested_name: "delta-subnets",
        stored_name: "delta-subnets",
        description: Some(""),
        subnet_ids: &["subnet-1a2b3c4d"],
        tags: &[("only", "one")],
    },
    Case {
        case: "a long name sorting last of these rows",
        requested_name: "zeta-subnets-0042-longer-name",
        stored_name: "zeta-subnets-0042-longer-name",
        description: Some("long name"),
        subnet_ids: &["subnet-0f0f0f0f0f0f0f0f0", "subnet-1a2b3c4d"],
        tags: &[],
    },
];

/// The `CreateSubnetGroup` request body for one case.
fn create_body(case: &Case) -> Value {
    let mut body = json!({
        "SubnetGroupName": case.requested_name,
        "SubnetIds": case.subnet_ids,
    });
    if let Some(description) = case.description {
        body["Description"] = json!(description);
    }
    if !case.tags.is_empty() {
        body["Tags"] = Value::Array(
            case.tags
                .iter()
                .map(|(key, value)| json!({ "Key": key, "Value": value }))
                .collect(),
        );
    }
    body
}

/// The ARN Amazon MemoryDB reports for a subnet group of this name:
/// `arn:<partition>:memorydb:<region>:<account>:subnetgroup/<name>`.
fn subnet_group_arn(name: &str) -> String {
    format!("arn:aws:memorydb:{REGION}:{ACCOUNT}:subnetgroup/{name}")
}

/// What the service must report for one case.
fn expected_group(case: &Case) -> Value {
    json!({
        "Name": case.stored_name,
        // A group created without a `Description` carries the service's own default
        // (a single space), not an absent member.
        "Description": case.description.unwrap_or(" "),
        "Subnets": case
            .subnet_ids
            .iter()
            .map(|identifier| json!({ "Identifier": identifier }))
            .collect::<Vec<Value>>(),
        "ARN": subnet_group_arn(case.stored_name),
    })
}

/// `SubnetGroups[*].Name` of a `DescribeSubnetGroups` body, in answer order.
fn group_names(body: &Value, case: &str) -> Vec<String> {
    body.get("SubnetGroups")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: expected SubnetGroups in {body}"))
        .iter()
        .map(|group| {
            group
                .get("Name")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("case {case:?}: expected Name in {group}"))
                .to_string()
        })
        .collect()
}

/// A create persists the group, and a later describe answers from that state: the same
/// name, description, subnets and ARN come back, whichever members the request carried.
#[tokio::test]
async fn a_created_subnet_group_is_reported_by_a_later_describe() {
    let router = router();

    for case in CASES {
        let created = ok(&router, "CreateSubnetGroup", &create_body(case), case.case).await;
        assert_eq!(
            created["SubnetGroup"],
            expected_group(case),
            "case {:?}: the create must answer the record it persisted, got {created}",
            case.case
        );

        // Read back by name — the request's own spelling, which the store folds.
        let described = ok(
            &router,
            "DescribeSubnetGroups",
            &json!({ "SubnetGroupName": case.requested_name }),
            case.case,
        )
        .await;
        assert_eq!(
            described["SubnetGroups"],
            json!([expected_group(case)]),
            "case {:?}: a describe must report the stored group, got {described}",
            case.case
        );

        // And the tags the create carried are the ones the group's ARN answers.
        let listed = ok(
            &router,
            "ListTags",
            &json!({ "ResourceArn": subnet_group_arn(case.stored_name) }),
            case.case,
        )
        .await;
        let mut expected: Vec<Value> = case
            .tags
            .iter()
            .map(|(key, value)| json!({ "Key": key, "Value": value }))
            .collect();
        expected.sort_by_key(|tag| tag["Key"].as_str().unwrap_or_default().to_string());
        assert_eq!(
            listed["TagList"],
            Value::Array(expected),
            "case {:?}: ListTags must answer the tags the create carried, got {listed}",
            case.case
        );
    }

    // The full listing reports every stored group, in name order, and this store starts
    // with none of its own (a default subnet group's subnets would be the default VPC's,
    // which the Amazon EC2 state this emulator serves does not start with either) — so the
    // set is exactly what these rows created.
    let described = ok(&router, "DescribeSubnetGroups", &json!({}), "the full listing").await;
    let mut expected: Vec<String> = CASES
        .iter()
        .map(|case| case.stored_name.to_string())
        .collect();
    expected.sort();
    assert_eq!(
        group_names(&described, "the full listing"),
        expected,
        "the listing must report every stored group in name order, got {described}"
    );
}

/// A refused create writes nothing, and the refusal is the modelled fault for the rule
/// the request broke: a name already in state, a subnet identifier of another form, and a
/// list longer than the service stores.
#[tokio::test]
async fn a_refused_create_is_the_modelled_fault_and_stores_nothing() {
    let router = router();
    let name = "refusals-subnets";
    ok(
        &router,
        "CreateSubnetGroup",
        &json!({ "SubnetGroupName": name, "SubnetIds": ["subnet-0123456789abcdef0"] }),
        "the seed",
    )
    .await;

    // (case, the request, the fault it must answer)
    let rows: Vec<(&str, Value, &str)> = vec![
        (
            "a name already in state",
            json!({ "SubnetGroupName": name, "SubnetIds": ["subnet-0123456789abcdef0"] }),
            "SubnetGroupAlreadyExistsFault",
        ),
        (
            "the same name in another letter case",
            json!({ "SubnetGroupName": "Refusals-Subnets", "SubnetIds": ["subnet-1a2b3c4d"] }),
            "SubnetGroupAlreadyExistsFault",
        ),
        (
            "a value that is no subnet identifier at all",
            json!({ "SubnetGroupName": "bad-subnets", "SubnetIds": ["not-a-subnet"] }),
            "InvalidSubnet",
        ),
        (
            "a subnet identifier of neither published length",
            json!({ "SubnetGroupName": "short-subnets", "SubnetIds": ["subnet-0123"] }),
            "InvalidSubnet",
        ),
        (
            "one well-formed identifier beside one that is not",
            json!({
                "SubnetGroupName": "mixed-subnets",
                "SubnetIds": ["subnet-0123456789abcdef0", "subnet-NOTHEX0123456789"],
            }),
            "InvalidSubnet",
        ),
        (
            "more subnets than the service stores in one group",
            json!({
                "SubnetGroupName": "crowded-subnets",
                "SubnetIds": (0..=MAX_SUBNETS_PER_GROUP)
                    .map(|index| format!("subnet-{index:017x}"))
                    .collect::<Vec<String>>(),
            }),
            "SubnetQuotaExceededFault",
        ),
    ];

    for (case, body, fault) in rows {
        let (status, answered) = call(&router, "CreateSubnetGroup", &body).await;
        assert_eq!(
            (status, error_code(&answered).as_str()),
            (400, fault),
            "case {case:?}: expected the modelled {fault}, got {status} with {answered}"
        );
    }

    // Nothing the refusals named is in state: the listing still holds the one seed.
    let described = ok(&router, "DescribeSubnetGroups", &json!({}), "after refusals").await;
    assert_eq!(
        group_names(&described, "after refusals"),
        vec![name.to_string()],
        "a refused create must store nothing, got {described}"
    );
}

/// A name no stored group matches is the modelled not-found fault, whichever operation
/// names it — the read and the delete alike.
///
/// The status is the 400 this service answers every fault about a resource that is not in
/// state with (`fault_http_status.rs` pins that rule for the service as a whole), not the
/// 404 the shape declares.
#[tokio::test]
async fn an_absent_subnet_group_is_the_modelled_not_found_fault() {
    let router = router();
    for operation in ["DescribeSubnetGroups", "DeleteSubnetGroup"] {
        for name in ["no-such-subnets", "Absent-Group"] {
            let case = format!("{operation} names {name:?}");
            let (status, answered) =
                call(&router, operation, &json!({ "SubnetGroupName": name })).await;
            assert_eq!(
                (status, error_code(&answered).as_str()),
                (400, "SubnetGroupNotFoundFault"),
                "case {case:?}: expected the modelled not-found fault, got {status} \
                 with {answered}"
            );
            let message = answered
                .get("message")
                .or_else(|| answered.get("Message"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            assert_eq!(
                message,
                format!("Subnet group {} not found.", name.to_ascii_lowercase()),
                "case {case:?}: expected the service's sentence for an absent group, got \
                 {answered}"
            );
        }
    }
}

/// A delete removes the group from state — and a group a cluster still runs in is refused
/// and left exactly where it was.
#[tokio::test]
async fn a_deleted_subnet_group_is_gone_and_one_in_use_is_refused() {
    let router = router();
    for (name, subnets) in [
        ("spare-subnets", vec!["subnet-0123456789abcdef0"]),
        ("used-subnets", vec!["subnet-0f0f0f0f0f0f0f0f0", "subnet-1a2b3c4d"]),
    ] {
        ok(
            &router,
            "CreateSubnetGroup",
            &json!({ "SubnetGroupName": name, "SubnetIds": subnets }),
            name,
        )
        .await;
    }
    // A cluster placed in the second group, which is what puts it in use.
    ok(
        &router,
        "CreateCluster",
        &json!({
            "ClusterName": "subnet-group-cluster",
            "NodeType": NODE_TYPE,
            "ACLName": BUILTIN_ACL,
            "SubnetGroupName": "Used-Subnets",
        }),
        "a cluster in the group",
    )
    .await;

    // The group nothing runs in goes away: the delete answers the record it removed, and
    // both readers then answer not-found for the name.
    let deleted = ok(
        &router,
        "DeleteSubnetGroup",
        &json!({ "SubnetGroupName": "spare-subnets" }),
        "the spare group",
    )
    .await;
    assert_eq!(
        deleted["SubnetGroup"]["ARN"],
        json!(subnet_group_arn("spare-subnets")),
        "the delete must answer the record it removed, got {deleted}"
    );
    let (status, answered) = call(
        &router,
        "DescribeSubnetGroups",
        &json!({ "SubnetGroupName": "spare-subnets" }),
    )
    .await;
    assert_eq!(
        (status, error_code(&answered).as_str()),
        (400, "SubnetGroupNotFoundFault"),
        "a deleted group must be gone, got {status} with {answered}"
    );

    // The group the cluster runs in is refused, and stays in state.
    let (status, answered) = call(
        &router,
        "DeleteSubnetGroup",
        &json!({ "SubnetGroupName": "used-subnets" }),
    )
    .await;
    assert_eq!(
        (status, error_code(&answered).as_str()),
        (400, "SubnetGroupInUseFault"),
        "a group a cluster runs in must be refused, got {status} with {answered}"
    );
    let described = ok(
        &router,
        "DescribeSubnetGroups",
        &json!({ "SubnetGroupName": "used-subnets" }),
        "the group in use",
    )
    .await;
    assert_eq!(
        described["SubnetGroups"][0]["Name"],
        json!("used-subnets"),
        "a refused delete must leave the group in place, got {described}"
    );
}
