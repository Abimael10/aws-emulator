// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `DescribeSnapshotAttribute`, driven over the wire the service
//! actually speaks: `ec2Query` — an `application/x-www-form-urlencoded` POST whose body
//! carries `Action=…`, answered with XML (README.md / OPERATIONS.md "Wire protocol").
//!
//! The rules under test are:
//!
//! 1. A `SnapshotAttributeName` ATTRIBUTE IS A PROPERTY OF THE SNAPSHOT THE INVENTORY HOLDS:
//!    the read names the snapshot it describes and reports the member the request asked for —
//!    and ONLY that member, "you can specify only one attribute at a time" — for a snapshot of
//!    every side of the account-visible inventory: one the account created from a volume, one
//!    the EBS direct APIs minted for it, and one of the public snapshots the AMI catalogue is
//!    published with;
//! 2. `createVolumePermission` IS WHO MAY CREATE A VOLUME FROM THE SNAPSHOT: a PUBLIC snapshot
//!    — one the calling account does not own — carries the `all` group permission ("all Amazon
//!    Web Services accounts have create volume permissions for these snapshots"), and a
//!    snapshot of the account carries none, its owner's permission being implicit rather than
//!    one of the permissions this attribute lists;
//! 3. `productCodes` ARE THE MARKETPLACE CODES THE SNAPSHOT IS BILLED THROUGH, which travel
//!    with the data: the publishing AMI's own codes for a public snapshot, and the source
//!    volume's for a snapshot the account took of one — so each row's expectation is READ OFF
//!    that image or that volume rather than written here;
//! 4. EACH REFUSAL KEEPS ITS OWN CAUSE: a value that is no snapshot id EC2 could have issued is
//!    refused as a bad parameter, a well-formed id the inventory holds no snapshot for as not
//!    found, an attribute outside the modelled catalog by the request boundary, and a `DryRun`
//!    of a request that would have succeeded with `DryRunOperation`;
//! 5. THE READ IS A READ: two reads of one snapshot report the same thing, the inventory is
//!    unchanged by them, and the id of a snapshot that has been deleted stops resolving.
//!
//! Each table row is a distinct input along what the rule generalises over — which side of the
//! inventory the snapshot comes from, which source volume it was taken of, which account owns
//! it, which attribute is asked for, and how a refused value leaves the form of an id — so no
//! row is the specification.

use std::collections::BTreeMap;
use std::sync::Arc;

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_ec2_amazonec2 as ec2;
use service_com_amazonaws_ec2_amazonec2::state::{
    DRY_RUN_OPERATION, DRY_RUN_OPERATION_MESSAGE, DRY_RUN_OPERATION_STATUS,
    IMAGE_OWNER_ALIAS_AMAZON, INVALID_SNAPSHOT_ID_MALFORMED, INVALID_SNAPSHOT_ID_NOT_FOUND,
    SNAPSHOT_ATTRIBUTE_CREATE_VOLUME_PERMISSION, SNAPSHOT_ATTRIBUTE_PRODUCT_CODES,
};

use super::ec2_query::{element, error_code, error_message, indexed};
use super::volume_lifecycle::{REGION, p, zone_name};
use super::wire::{child, child_text, document, set_items};

/// Every attribute of a snapshot, in the order the model publishes them. Pinned against the
/// model's own `SnapshotAttributeName` catalog by [`the_attribute_catalog_is_the_models_own`],
/// so a member the model gains cannot be left untested here.
const ATTRIBUTES: [&str; 2] = [
    SNAPSHOT_ATTRIBUTE_CREATE_VOLUME_PERMISSION,
    SNAPSHOT_ATTRIBUTE_PRODUCT_CODES,
];

/// The error code the centralized request walk answers for an input that breaks a MODELLED
/// constraint — here an `Attribute` outside the `SnapshotAttributeName` enum, which is refused
/// before any handler runs, and so carries the boundary's own code rather than one a handler
/// chose.
const BOUNDARY_CONSTRAINT_CODE: &str = "ValidationError";

/// A snapshot id of the long form this service mints that no snapshot of the inventory carries
/// — a well-formed id naming nothing is a different cause from a value that is not a snapshot
/// id at all.
const UNKNOWN_SNAPSHOT_ID: &str = "snap-0123456789abcdef0";

/// Both services on ONE engine — the EBS direct APIs mint snapshots of the same account, and
/// one of the inventory's three sides is theirs — through the suite that owns that harness.
fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    super::ebs_direct_api_snapshots::emulator()
}

/// One `action` call signed for the Region this suite works in.
async fn op(router: &MultiServiceRouter, action: &str, params: &[(String, String)]) -> (u16, String) {
    super::ec2_query::call(router, REGION, action, params).await
}

/// One `DescribeSnapshotAttribute` call, as its HTTP status and XML body.
async fn describe_attribute(
    router: &MultiServiceRouter,
    params: &[(String, String)],
) -> (u16, String) {
    op(router, "DescribeSnapshotAttribute", params).await
}

/// The form fields of a `DescribeSnapshotAttribute` for `attribute` of `snapshot_id`.
fn read_params(snapshot_id: &str, attribute: &str) -> Vec<(String, String)> {
    vec![
        p("SnapshotId", snapshot_id),
        p("Attribute", attribute),
    ]
}

/// The body of a `DescribeSnapshotAttribute` for `attribute` of `snapshot_id`, required to
/// succeed, to name the snapshot it describes and to carry NO attribute but the one asked for
/// ("you can specify only one attribute at a time").
async fn read_attribute(
    router: &MultiServiceRouter,
    case: &str,
    snapshot_id: &str,
    attribute: &str,
) -> String {
    let (status, body) = describe_attribute(router, &read_params(snapshot_id, attribute)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeSnapshotAttribute for {attribute}, \
         got {status} with body {body}"
    );
    let parsed = document(case, &body);
    let root = parsed.root_element();
    assert_eq!(
        child_text(root, "snapshotId").as_deref(),
        Some(snapshot_id),
        "case {case:?}: DescribeSnapshotAttribute must name the snapshot it describes, \
         body {body}"
    );
    for other in ATTRIBUTES.iter().filter(|other| **other != attribute) {
        assert!(
            child(root, other).is_none(),
            "case {case:?}: a DescribeSnapshotAttribute for {attribute} must not report \
             {other}, body {body}"
        );
    }
    assert!(
        child(root, attribute).is_some(),
        "case {case:?}: no <{attribute}> in the DescribeSnapshotAttribute body {body}"
    );
    body
}

/// The create volume permissions of `snapshot_id`, as the attribute read reports them: one
/// `(userId, group)` pair per `<item>`, with an absent member read as empty.
async fn read_create_volume_permissions(
    router: &MultiServiceRouter,
    case: &str,
    snapshot_id: &str,
) -> Vec<(String, String)> {
    let body = read_attribute(
        router,
        case,
        snapshot_id,
        SNAPSHOT_ATTRIBUTE_CREATE_VOLUME_PERMISSION,
    )
    .await;
    pairs_of(
        case,
        &body,
        SNAPSHOT_ATTRIBUTE_CREATE_VOLUME_PERMISSION,
        "userId",
        "group",
    )
}

/// The product codes of `snapshot_id`, as the attribute read reports them: one
/// `(productCode, type)` pair per `<item>`.
async fn read_product_codes(
    router: &MultiServiceRouter,
    case: &str,
    snapshot_id: &str,
) -> Vec<(String, String)> {
    let body = read_attribute(router, case, snapshot_id, SNAPSHOT_ATTRIBUTE_PRODUCT_CODES).await;
    pairs_of(
        case,
        &body,
        SNAPSHOT_ATTRIBUTE_PRODUCT_CODES,
        "productCode",
        "type",
    )
}

/// The `(first, second)` members of every `<item>` of the response's `list` member, read
/// through the XML tree so a member of an item is never confused with one of the envelope.
fn pairs_of(
    case: &str,
    body: &str,
    list: &str,
    first: &str,
    second: &str,
) -> Vec<(String, String)> {
    let parsed = document(case, body);
    set_items(parsed.root_element(), list)
        .into_iter()
        .map(|item| {
            (
                child_text(item, first).unwrap_or_default(),
                child_text(item, second).unwrap_or_default(),
            )
        })
        .collect()
}

/// One snapshot of the account-visible inventory under test: how it got there, its id, and the
/// owner and source volume the inventory's own report gives it — so every expectation about it
/// is read off the service rather than written here.
#[derive(Debug, Clone)]
struct Row {
    case: String,
    snapshot_id: String,
    owner_id: String,
    volume_id: String,
}

impl Row {
    /// Is this a snapshot every account may create a volume from — one the calling account does
    /// not own?
    fn is_public(&self, account: &str) -> bool {
        self.owner_id != account
    }
}

/// One `CreateVolume` of `size` GiB in the Region's first zone, as the id it minted.
async fn create_volume(router: &MultiServiceRouter, case: &str, size: i64) -> String {
    let (status, body) = op(
        router,
        "CreateVolume",
        &[p("AvailabilityZone", zone_name(0)), p("Size", size.to_string())],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateVolume, got {status} with body {body}"
    );
    element(&body, "volumeId")
        .unwrap_or_else(|| panic!("case {case:?}: CreateVolume reports the volume it minted"))
}

/// One `CreateSnapshot` of `volume_id`, as the id it minted.
async fn create_snapshot(router: &MultiServiceRouter, case: &str, volume_id: &str) -> String {
    let (status, body) = op(router, "CreateSnapshot", &[p("VolumeId", volume_id)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateSnapshot, got {status} with body {body}"
    );
    element(&body, "snapshotId")
        .unwrap_or_else(|| panic!("case {case:?}: CreateSnapshot reports the snapshot it minted"))
}

/// The snapshot `snapshot_id` names as the INVENTORY reports it, as a row of the tables below.
async fn row(router: &MultiServiceRouter, case: String, snapshot_id: String) -> Row {
    let (status, body) = op(
        router,
        "DescribeSnapshots",
        &indexed("SnapshotId", &[snapshot_id.as_str()]),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the inventory must report {snapshot_id}, got {status} with body {body}"
    );
    let parsed = document(&case, &body);
    let reported = parsed
        .descendants()
        .find(|node| {
            node.is_element()
                && child_text(*node, "snapshotId").as_deref() == Some(snapshot_id.as_str())
        })
        .unwrap_or_else(|| panic!("case {case:?}: no <snapshotId> {snapshot_id} in {body}"));
    Row {
        case,
        snapshot_id,
        owner_id: child_text(reported, "ownerId").unwrap_or_default(),
        volume_id: child_text(reported, "volumeId").unwrap_or_default(),
    }
}

/// The public snapshots the published AMI catalogue is backed by, and the Marketplace product
/// codes of the image each one was published with: `snapshotId -> [(productCode, type)]`.
///
/// `IncludeDeprecated`, because a deprecated build is left out of a describe by default
/// ("if `true`, all deprecated AMIs are included in the response",
/// `DescribeImagesRequest$IncludeDeprecated`) while the snapshot it restores from is a snapshot
/// of the inventory like any other. Both sides are read OFF the image, so the expectation for a
/// published snapshot is the catalogue's own and not this suite's.
async fn catalogue_snapshot_product_codes(
    router: &MultiServiceRouter,
) -> BTreeMap<String, Vec<(String, String)>> {
    let case = "the published AMI catalogue";
    let mut params = indexed("Owner", &[IMAGE_OWNER_ALIAS_AMAZON]);
    params.push(p("IncludeDeprecated", "true"));
    let (status, body) = op(router, "DescribeImages", &params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeImages, got {status} with body {body}"
    );
    let parsed = document(case, &body);
    let mut codes = BTreeMap::new();
    for image in parsed
        .descendants()
        .filter(|node| node.is_element() && child(*node, "imageId").is_some())
    {
        let product_codes: Vec<(String, String)> = set_items(image, "productCodes")
            .into_iter()
            .map(|item| {
                (
                    child_text(item, "productCode").unwrap_or_default(),
                    child_text(item, "type").unwrap_or_default(),
                )
            })
            .collect();
        for mapping in set_items(image, "blockDeviceMapping") {
            let Some(snapshot_id) = child(mapping, "ebs")
                .and_then(|ebs| child_text(ebs, "snapshotId"))
                .filter(|snapshot_id| !snapshot_id.is_empty())
            else {
                continue;
            };
            codes.insert(snapshot_id, product_codes.clone());
        }
    }
    assert!(
        !codes.is_empty(),
        "case {case:?}: the catalogue restores its root devices from snapshots, got {body}"
    );
    codes
}

/// Every row of the inventory under test: the account's own snapshots, one the EBS direct APIs
/// hold, and every public snapshot the AMI catalogue is published with.
async fn inventory(router: &MultiServiceRouter) -> Vec<Row> {
    let mut rows: Vec<Row> = Vec::new();
    // The account's own: one row per source volume size, so the rows differ in the volume the
    // snapshot was taken of rather than only in its id.
    for size in [1_i64, 8] {
        let case = format!("a snapshot of a {size} GiB volume of the account");
        let volume_id = create_volume(router, &case, size).await;
        let snapshot_id = create_snapshot(router, &case, &volume_id).await;
        rows.push(row(router, case, snapshot_id).await);
    }
    // A snapshot of the account the EBS DIRECT APIs minted, which is written block by block
    // rather than taken of a volume — so it names none.
    let case = "a snapshot the EBS direct APIs hold".to_string();
    let snapshot_id = super::ebs_direct_api_snapshots::start_snapshot(
        router,
        &case,
        serde_json::json!({ "VolumeSize": 1 }),
    )
    .await;
    rows.push(row(router, case, snapshot_id).await);
    // Every PUBLIC snapshot of the inventory: the ones the AMI catalogue is published with.
    for snapshot_id in catalogue_snapshot_product_codes(router).await.into_keys() {
        let case = format!("the published snapshot {snapshot_id}");
        rows.push(row(router, case, snapshot_id).await);
    }
    rows
}

/// The attributes this suite tables are the model's own `SnapshotAttributeName` catalog, so a
/// member the model gains fails here rather than going untested.
#[tokio::test]
async fn the_attribute_catalog_is_the_models_own() {
    let mut published = ec2::ops::types::SNAPSHOT_ATTRIBUTE_NAME_VALUES.to_vec();
    published.sort_unstable();
    let mut tabled = ATTRIBUTES.to_vec();
    tabled.sort_unstable();
    assert_eq!(
        tabled, published,
        "the suite must table every SnapshotAttributeName the model publishes, got {tabled:?} \
         against {published:?}"
    );
}

/// RULE 1: the read names the snapshot it describes and reports exactly the attribute it was
/// asked for, for a snapshot of every side of the inventory.
#[tokio::test]
async fn an_attribute_read_reports_only_the_attribute_it_was_asked_for() {
    let (router, _engine) = emulator();
    for row in inventory(&router).await {
        for attribute in ATTRIBUTES {
            // `read_attribute` is where the rule is asserted: the status, the snapshot the
            // answer names, the member asked for and the absence of every other.
            read_attribute(&router, &row.case, &row.snapshot_id, attribute).await;
        }
    }
}

/// RULE 2: the create volume permissions of a snapshot are the `all` group for a public one and
/// none for one the account owns.
#[tokio::test]
async fn create_volume_permissions_follow_whether_the_snapshot_is_public() {
    let (router, _engine) = emulator();
    let account = engine::emulator_account_id();
    let rows = inventory(&router).await;
    // The group a public snapshot's permission names is read off the MODEL's own
    // `PermissionGroup` catalog ("the possible value is `all`"), so the expectation is the
    // model's and not a word copied out of the handler.
    let groups = ec2::ops::types::PERMISSION_GROUP_VALUES;
    assert_eq!(
        groups.len(),
        1,
        "the model publishes one PermissionGroup member, got {groups:?}"
    );
    let group = groups[0];
    let mut public_rows = 0_usize;
    let mut owned_rows = 0_usize;
    for row in &rows {
        let expected: Vec<(String, String)> = if row.is_public(account) {
            public_rows += 1;
            vec![(String::new(), group.to_string())]
        } else {
            owned_rows += 1;
            Vec::new()
        };
        let reported = read_create_volume_permissions(&router, &row.case, &row.snapshot_id).await;
        assert_eq!(
            reported, expected,
            "case {:?}: a snapshot owned by {:?} (the caller is {account}) has the create \
             volume permissions {expected:?}, got {reported:?}",
            row.case, row.owner_id
        );
    }
    // Both halves of the rule were really exercised: a table that happened to hold only public
    // or only owned snapshots would assert half of it.
    assert!(
        public_rows > 0 && owned_rows > 0,
        "the inventory must hold both public and owned snapshots to compare, got \
         {public_rows} public and {owned_rows} owned"
    );
}

/// RULE 3: the product codes of a snapshot are the ones of whatever it was made from — the
/// publishing AMI's for a public snapshot, its source volume's for one of the account's.
#[tokio::test]
async fn product_codes_are_the_codes_the_snapshot_is_billed_through() {
    let (router, _engine) = emulator();
    let account = engine::emulator_account_id();
    let published = catalogue_snapshot_product_codes(&router).await;
    for row in inventory(&router).await {
        let expected = if row.is_public(account) {
            published
                .get(&row.snapshot_id)
                .cloned()
                .unwrap_or_else(|| panic!("case {:?}: no catalogue image backs it", row.case))
        } else if row.volume_id.is_empty() {
            // A snapshot written block by block was taken of no volume, so there is nothing
            // for a code to have travelled from.
            Vec::new()
        } else {
            volume_product_codes(&router, &row.case, &row.volume_id).await
        };
        let reported = read_product_codes(&router, &row.case, &row.snapshot_id).await;
        assert_eq!(
            reported, expected,
            "case {:?}: expected the product codes {expected:?} of what the snapshot was made \
             from, got {reported:?}",
            row.case
        );
    }
}

/// The product codes of `volume_id`, as `DescribeVolumeAttribute` reports them — the other read
/// of the same Marketplace codes, so a volume and a snapshot of it cannot be billed two ways.
async fn volume_product_codes(
    router: &MultiServiceRouter,
    case: &str,
    volume_id: &str,
) -> Vec<(String, String)> {
    let (status, body) = op(
        router,
        "DescribeVolumeAttribute",
        &[p("VolumeId", volume_id), p("Attribute", "productCodes")],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeVolumeAttribute for {volume_id}, got \
         {status} with body {body}"
    );
    pairs_of(case, &body, "productCodes", "productCode", "type")
}

/// RULE 4: every refused value keeps its own cause — an unreadable id, a well-formed id nothing
/// holds, an attribute outside the catalog, and a dry run of a request that would have
/// succeeded.
#[tokio::test]
async fn each_refusal_keeps_its_own_cause() {
    let (router, _engine) = emulator();

    /// One row: `(the SnapshotId, the expected code, the expected message)`.
    type RefusalRow = (&'static str, &'static str, String);

    // The unreadable rows differ in HOW the value leaves the form EC2 issues an id in: the
    // all-zero placeholder no mint can ever produce, no prefix at all, the prefix with
    // non-hexadecimal text, a width between the two EC2 prints, the prefix alone, and an id of
    // another kind of resource.
    let unreadable = |value: &str| -> String {
        format!("Value ( {value} ) for parameter SNAPSHOT is invalid. ")
    };
    let rows: Vec<RefusalRow> = vec![
        (
            "snap-00000000000000000",
            INVALID_SNAPSHOT_ID_MALFORMED,
            unreadable("snap-00000000000000000"),
        ),
        (
            "not-a-snapshot",
            INVALID_SNAPSHOT_ID_MALFORMED,
            unreadable("not-a-snapshot"),
        ),
        (
            "snap-bad-id",
            INVALID_SNAPSHOT_ID_MALFORMED,
            unreadable("snap-bad-id"),
        ),
        (
            "snap-0123456789abcdef",
            INVALID_SNAPSHOT_ID_MALFORMED,
            unreadable("snap-0123456789abcdef"),
        ),
        ("snap-", INVALID_SNAPSHOT_ID_MALFORMED, unreadable("snap-")),
        (
            "vol-0123456789abcdef0",
            INVALID_SNAPSHOT_ID_MALFORMED,
            unreadable("vol-0123456789abcdef0"),
        ),
        (
            UNKNOWN_SNAPSHOT_ID,
            INVALID_SNAPSHOT_ID_NOT_FOUND,
            format!("The snapshot '{UNKNOWN_SNAPSHOT_ID}' does not exist."),
        ),
    ];

    // Which attribute was asked for cannot change which id is refused, so every row is sent
    // for both of them.
    for (value, code, message) in rows {
        for attribute in ATTRIBUTES {
            let case = format!("{value:?} for {attribute}");
            let (status, body) = describe_attribute(&router, &read_params(value, attribute)).await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected HTTP 400, got {status} with body {body}"
            );
            assert_eq!(
                error_code(&body),
                code,
                "case {case:?}: expected the code {code:?}, got {:?}",
                error_code(&body)
            );
            assert_eq!(
                error_message(&body),
                message,
                "case {case:?}: expected the message {message:?}, got {:?}",
                error_message(&body)
            );
        }
    }

    // An attribute outside the modelled catalog never reaches the handler: the request
    // boundary refuses it with its own code.
    let volume_id = create_volume(&router, "an attribute outside the catalog", 1).await;
    let snapshot_id = create_snapshot(&router, "an attribute outside the catalog", &volume_id).await;
    for attribute in ["createvolumepermission", "launchPermission", "autoEnableIO"] {
        let case = format!("the attribute {attribute:?}");
        let (status, body) =
            describe_attribute(&router, &read_params(&snapshot_id, attribute)).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            BOUNDARY_CONSTRAINT_CODE,
            "case {case:?}: expected the boundary's {BOUNDARY_CONSTRAINT_CODE:?}, got {:?}",
            error_code(&body)
        );
    }

    // "If you have the required permissions, the error response is DryRunOperation" — on a
    // request that would have succeeded. A dry run of a request that would have been REFUSED
    // is refused with that reason instead, so the flag never hides an unresolvable id.
    for attribute in ATTRIBUTES {
        let case = format!("a dry run for {attribute}");
        let mut params = read_params(&snapshot_id, attribute);
        params.push(p("DryRun", "true"));
        let (status, body) = describe_attribute(&router, &params).await;
        assert_eq!(
            status, DRY_RUN_OPERATION_STATUS,
            "case {case:?}: expected HTTP {DRY_RUN_OPERATION_STATUS}, got {status} with body \
             {body}"
        );
        assert_eq!(
            (error_code(&body), error_message(&body)),
            (
                DRY_RUN_OPERATION.to_string(),
                DRY_RUN_OPERATION_MESSAGE.to_string()
            ),
            "case {case:?}: expected the dry-run verdict, got body {body}"
        );

        let case = format!("a dry run naming an unresolvable snapshot for {attribute}");
        let mut params = read_params(UNKNOWN_SNAPSHOT_ID, attribute);
        params.push(p("DryRun", "true"));
        let (status, body) = describe_attribute(&router, &params).await;
        assert_eq!(
            (status, error_code(&body)),
            (400, INVALID_SNAPSHOT_ID_NOT_FOUND.to_string()),
            "case {case:?}: expected the not-found refusal, got {status} with body {body}"
        );
    }
}

/// RULE 5: the read changes nothing a later caller sees, and the id of a deleted snapshot stops
/// resolving.
#[tokio::test]
async fn the_read_changes_nothing_and_follows_the_store() {
    let (router, _engine) = emulator();
    let case = "a snapshot of the account, read twice";
    let volume_id = create_volume(&router, case, 1).await;
    let snapshot_id = create_snapshot(&router, case, &volume_id).await;

    let before = inventory_ids(&router, case).await;
    let first = read_create_volume_permissions(&router, case, &snapshot_id).await;
    let codes = read_product_codes(&router, case, &snapshot_id).await;
    let again = read_create_volume_permissions(&router, case, &snapshot_id).await;
    let codes_again = read_product_codes(&router, case, &snapshot_id).await;
    assert_eq!(
        (first, codes),
        (again, codes_again),
        "case {case:?}: a second read of one snapshot's attributes reports the same thing"
    );
    assert_eq!(
        inventory_ids(&router, case).await,
        before,
        "case {case:?}: reading an attribute leaves the inventory as it was"
    );

    // The resolution follows the STORE: once the snapshot is deleted its id names nothing, so
    // the attribute read answers the not-found refusal for it.
    let (status, body) = op(
        &router,
        "DeleteSnapshot",
        &[p("SnapshotId", snapshot_id.as_str())],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DeleteSnapshot, got {status} with body {body}"
    );
    for attribute in ATTRIBUTES {
        let case = format!("{attribute} of the deleted snapshot");
        let (status, body) =
            describe_attribute(&router, &read_params(&snapshot_id, attribute)).await;
        assert_eq!(
            (status, error_code(&body), error_message(&body)),
            (
                400,
                INVALID_SNAPSHOT_ID_NOT_FOUND.to_string(),
                format!("The snapshot '{snapshot_id}' does not exist.")
            ),
            "case {case:?}: expected the not-found refusal, got {status} with body {body}"
        );
    }
}

/// Every snapshot id the inventory reports, in report order — what "nothing moved" compares.
async fn inventory_ids(router: &MultiServiceRouter, case: &str) -> Vec<String> {
    let (status, body) = op(router, "DescribeSnapshots", &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeSnapshots, got {status} with body {body}"
    );
    super::ec2_query::elements(&body, "snapshotId")
}
