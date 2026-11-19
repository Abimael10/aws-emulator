// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `ModifySnapshotAttribute` and `ResetSnapshotAttribute`, driven over the
//! wire the service actually speaks: `ec2Query` — an `application/x-www-form-urlencoded` POST
//! whose body carries `Action=…`, answered with XML (README.md / OPERATIONS.md "Wire protocol").
//!
//! The rules under test are:
//!
//! 1. A SNAPSHOT'S CREATE VOLUME PERMISSIONS ARE STATE A MODIFY WRITES: a modify records exactly
//!    the grantees it names — an account id, the `all` group, or both — and a later read reports
//!    them, whichever of the two forms EC2 publishes the modification arrived in (the
//!    `CreateVolumePermission` `Add`/`Remove` structure, or the flat `OperationType` +
//!    `UserIds`/`GroupNames` members);
//! 2. A REMOVAL DROPS EXACTLY THE GRANTEES IT NAMES, leaving the rest standing, and a grant or a
//!    removal repeated is the same state again — a permission is a membership, not a counter;
//! 3. A RESET CLEARS EVERY CREATE VOLUME PERMISSION: "resets permission settings for the
//!    specified snapshot", whatever had been granted on it, and a reset of a snapshot nobody has
//!    shared is the same empty list;
//! 4. THE PERMISSIONS ARE THE ONE FACT EVERY READER SEES: the inventory's `RestorableByUserIds`
//!    narrowing selects the snapshot for an account it has been shared with and for `all` once
//!    it is public, and a snapshot that has left the inventory keeps no permission at all;
//! 5. EACH REFUSAL KEEPS ITS OWN CAUSE AND WRITES NOTHING: an attribute this action cannot
//!    modify, a request that both adds and removes, grantee lists with no direction, a grantee
//!    that is no account and no group, a snapshot another account owns, public sharing of an
//!    encrypted snapshot or while block public access is on, and a `DryRun` of a request that
//!    would have succeeded.
//!
//! Each table row is a distinct input along what the rule generalises over — which form the
//! modification arrives in, which kind of grantee it names, how many it names, which direction
//! it moves them in, and which attribute and snapshot it names — so no row is the
//! specification.

use std::collections::BTreeSet;
use std::sync::Arc;

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_ec2_amazonec2 as ec2;
use service_com_amazonaws_ec2_amazonec2::state::{
    CREATE_VOLUME_PERMISSION_GROUP_ALL, DRY_RUN_OPERATION, DRY_RUN_OPERATION_STATUS,
    IMAGE_OWNER_ALIAS_AMAZON, INVALID_SNAPSHOT_ID_MALFORMED, INVALID_SNAPSHOT_ID_NOT_FOUND,
    MAX_SNAPSHOT_ATTRIBUTE_MODIFICATIONS, SNAPSHOT_ATTRIBUTE_CREATE_VOLUME_PERMISSION,
    SNAPSHOT_ATTRIBUTE_PRODUCT_CODES, SNAPSHOT_BLOCK_PUBLIC_ACCESS_MODES,
};

use super::ec2_query::{element, error_code, error_message, indexed};
use super::volume_lifecycle::{REGION, p, zone_name};
use super::wire::{child, child_text, document, set_items};

/// Account ids the snapshots under test are shared with: well-formed Amazon Web Services
/// account ids (twelve decimal digits) other than the one this emulator serves, so a grant to
/// one is a grant to somebody else.
const OTHER_ACCOUNTS: [&str; 3] = ["111122223333", "444455556666", "222233334444"];

/// The code EC2 answers for a parameter value it rejects.
const INVALID_PARAMETER_VALUE: &str = "InvalidParameterValue";

/// The code EC2 answers for parameters that are each valid alone but cannot be given together.
const INVALID_PARAMETER_COMBINATION: &str = "InvalidParameterCombination";

/// The code EC2 answers for a parameter the operation needs and the request left out.
const MISSING_PARAMETER: &str = "MissingParameter";

/// The code EC2 answers when the request is well formed and the resource exists, but something
/// about it forbids the action.
const OPERATION_NOT_PERMITTED: &str = "OperationNotPermitted";

/// The code EC2 answers for a user id that is no account id at all.
const INVALID_USER_ID_MALFORMED: &str = "InvalidUserID.Malformed";

/// A snapshot id of the long form this service mints that no snapshot of the inventory carries.
const UNKNOWN_SNAPSHOT_ID: &str = "snap-0123456789abcdef0";

/// A value in the printed long id form but OUTSIDE the one EC2 issues — the shape documentation
/// examples and hand-written probes reach for, which no snapshot could ever have held.
const UNISSUABLE_SNAPSHOT_ID: &str = "snap-1234567890abcdef0";

/// The sentence EC2 answers an operation that WRITES an attribute with when the attribute it
/// names is not one that action can write: fixed text end to end, naming no value — as AWS
/// answered the recorded `ModifySnapshotAttribute` call behind this suite's issue.
const UNRECOGNIZED_ATTRIBUTE_MESSAGE: &str = "Unrecognized attribute.";

/// The EC2 service on its own engine, which is handed back so a test can read the store the
/// handlers write.
fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    super::ec2_query::emulator()
}

/// One `action` call signed for the Region this suite works in.
async fn op(
    router: &MultiServiceRouter,
    action: &str,
    params: &[(String, String)],
) -> (u16, String) {
    super::ec2_query::call(router, REGION, action, params).await
}

/// One `CreateVolume` of `size` GiB in the Region's first zone, with whatever extra parameters
/// the case needs, as the volume id it minted.
async fn create_volume(
    router: &MultiServiceRouter,
    case: &str,
    size: i64,
    extra: Vec<(String, String)>,
) -> String {
    let mut params = vec![p("AvailabilityZone", zone_name(0)), p("Size", size.to_string())];
    params.extend(extra);
    let (status, body) = op(router, "CreateVolume", &params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateVolume, got {status} with body {body}"
    );
    element(&body, "volumeId")
        .unwrap_or_else(|| panic!("case {case:?}: CreateVolume reports the volume it minted"))
}

/// One snapshot of the account, taken of a volume this suite creates for it, as the snapshot id
/// it minted — the resource whose permissions every row below modifies.
async fn create_snapshot(router: &MultiServiceRouter, case: &str) -> String {
    snapshot_of_volume(router, case, Vec::new()).await
}

/// One snapshot of the account taken of a volume created with `extra` — the seam a row that
/// needs an ENCRYPTED snapshot goes through, "snapshots that are taken from encrypted volumes
/// are automatically encrypted".
async fn snapshot_of_volume(
    router: &MultiServiceRouter,
    case: &str,
    extra: Vec<(String, String)>,
) -> String {
    let volume_id = create_volume(router, case, 1, extra).await;
    let (status, body) = op(router, "CreateSnapshot", &[p("VolumeId", volume_id.as_str())]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateSnapshot, got {status} with body {body}"
    );
    element(&body, "snapshotId")
        .unwrap_or_else(|| panic!("case {case:?}: CreateSnapshot reports the snapshot it minted"))
}

#[tokio::test]
async fn each_refusal_carries_the_sentence_aws_words_it_in() {
    let (router, _engine) = emulator();
    let case = "the snapshot the sentences below are asked about";
    let snapshot_id = create_snapshot(&router, case).await;

    // The operations that WRITE a snapshot's attributes answer one fixed sentence for an
    // attribute they cannot act on — it names no value at all, unlike the describe beside them.
    for (action, params) in [
        (
            "ModifySnapshotAttribute",
            {
                let mut params = flat(&snapshot_id, "add", &[OTHER_ACCOUNTS[0]], &[]);
                params.retain(|(name, _)| name != "Attribute");
                params.push(p("Attribute", SNAPSHOT_ATTRIBUTE_PRODUCT_CODES));
                params
            },
        ),
        (
            "ResetSnapshotAttribute",
            vec![
                p("SnapshotId", snapshot_id.as_str()),
                p("Attribute", SNAPSHOT_ATTRIBUTE_PRODUCT_CODES),
            ],
        ),
    ] {
        let (status, body) = op(&router, action, &params).await;
        assert_eq!(
            status, 400,
            "case {case:?}: {action} of an attribute it cannot write must be refused, got \
             {status} with body {body}"
        );
        assert_eq!(
            error_message(&body),
            UNRECOGNIZED_ATTRIBUTE_MESSAGE,
            "case {case:?}: expected {action} to answer {UNRECOGNIZED_ATTRIBUTE_MESSAGE:?}, \
             got body {body}"
        );
    }

    // And one sentence for a value it could not read as a snapshot id, built from the value the
    // request sent: both writers answer the one the reset's own caller recorded, which names the
    // resource the id was given for.
    for (action, expected) in [
        (
            "ModifySnapshotAttribute",
            format!("The snapshot ID '{UNISSUABLE_SNAPSHOT_ID}' is malformed"),
        ),
        (
            "ResetSnapshotAttribute",
            format!("The snapshot ID '{UNISSUABLE_SNAPSHOT_ID}' is malformed"),
        ),
    ] {
        let params = match action {
            "ModifySnapshotAttribute" => {
                flat(UNISSUABLE_SNAPSHOT_ID, "add", &[OTHER_ACCOUNTS[0]], &[])
            }
            _ => vec![
                p("SnapshotId", UNISSUABLE_SNAPSHOT_ID),
                p("Attribute", SNAPSHOT_ATTRIBUTE_CREATE_VOLUME_PERMISSION),
            ],
        };
        let (status, body) = op(&router, action, &params).await;
        assert_eq!(
            status, 400,
            "case {case:?}: {action} must refuse an id outside the issued form, got {status} \
             with body {body}"
        );
        assert_eq!(
            error_code(&body),
            INVALID_SNAPSHOT_ID_MALFORMED,
            "case {case:?}: expected the malformed-id code from {action}, got body {body}"
        );
        assert_eq!(
            error_message(&body),
            expected,
            "case {case:?}: expected {action} to answer {expected:?}, got body {body}"
        );
    }
}

/// One `ModifySnapshotAttribute` call, as its HTTP status and XML body.
async fn modify(router: &MultiServiceRouter, params: &[(String, String)]) -> (u16, String) {
    op(router, "ModifySnapshotAttribute", params).await
}

/// One `ResetSnapshotAttribute` of `attribute` on `snapshot_id`.
async fn reset(
    router: &MultiServiceRouter,
    snapshot_id: &str,
    attribute: &str,
) -> (u16, String) {
    op(
        router,
        "ResetSnapshotAttribute",
        &[p("SnapshotId", snapshot_id), p("Attribute", attribute)],
    )
    .await
}

/// A modification that must SUCCEED, answering an empty 200 — the operation models no output
/// members at all, so the response says only that the call was made.
async fn modify_successfully(router: &MultiServiceRouter, case: &str, params: &[(String, String)]) {
    let (status, body) = modify(router, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from ModifySnapshotAttribute, got {status} with \
         body {body}"
    );
}

/// The create volume permissions of `snapshot_id` as `DescribeSnapshotAttribute` reports them:
/// one `(userId, group)` pair per `<item>`, as a SET — a grant is a membership, so nothing here
/// depends on the order the grants were made in.
async fn permissions_of(
    router: &MultiServiceRouter,
    case: &str,
    snapshot_id: &str,
) -> BTreeSet<(String, String)> {
    let (status, body) = op(
        router,
        "DescribeSnapshotAttribute",
        &[
            p("SnapshotId", snapshot_id),
            p("Attribute", SNAPSHOT_ATTRIBUTE_CREATE_VOLUME_PERMISSION),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeSnapshotAttribute, got {status} with \
         body {body}"
    );
    let parsed = document(case, &body);
    assert_eq!(
        child_text(parsed.root_element(), "snapshotId").as_deref(),
        Some(snapshot_id),
        "case {case:?}: the read must name the snapshot it describes, body {body}"
    );
    set_items(
        parsed.root_element(),
        SNAPSHOT_ATTRIBUTE_CREATE_VOLUME_PERMISSION,
    )
    .into_iter()
    .map(|item| {
        (
            child_text(item, "userId").unwrap_or_default(),
            child_text(item, "group").unwrap_or_default(),
        )
    })
    .collect()
}

/// The permission a grant to the account `user_id` reads back as.
fn shared_with(user_id: &str) -> (String, String) {
    (user_id.to_string(), String::new())
}

/// The permission a grant to the `all` group reads back as.
fn shared_publicly() -> (String, String) {
    (
        String::new(),
        CREATE_VOLUME_PERMISSION_GROUP_ALL.to_string(),
    )
}

/// `(case, the form fields that modify the snapshot, the permissions the read must then report)`
/// — the shape of the tables below, with the snapshot id filled in per row.
type Row = (
    &'static str,
    fn(&str) -> Vec<(String, String)>,
    Vec<(String, String)>,
);

/// `(case, the form fields under test, the error code EC2 answers)` — the shape of the refusal
/// table below.
type RefusalRow = (&'static str, Vec<(String, String)>, &'static str);

/// The flat form of a modification: `OperationType` plus the `UserId.N` and `UserGroup.N` lists
/// EC2 flattens `UserIds` and `GroupNames` into (their modelled `xmlName`s).
fn flat(
    snapshot_id: &str,
    operation_type: &str,
    user_ids: &[&str],
    groups: &[&str],
) -> Vec<(String, String)> {
    let mut params = vec![
        p("SnapshotId", snapshot_id),
        p("Attribute", SNAPSHOT_ATTRIBUTE_CREATE_VOLUME_PERMISSION),
        p("OperationType", operation_type),
    ];
    params.extend(indexed("UserId", user_ids));
    params.extend(indexed("UserGroup", groups));
    params
}

/// The structured form: `CreateVolumePermission.<Add|Remove>.N.<UserId|Group>`, "a JSON
/// representation of the snapshot attribute modification".
fn structured(
    snapshot_id: &str,
    direction: &str,
    grantees: &[(&str, &str)],
) -> Vec<(String, String)> {
    let mut params = vec![p("SnapshotId", snapshot_id)];
    for (position, (user_id, group)) in grantees.iter().enumerate() {
        let entry = format!("CreateVolumePermission.{direction}.{}", position + 1);
        if !user_id.is_empty() {
            params.push(p(&format!("{entry}.UserId"), *user_id));
        }
        if !group.is_empty() {
            params.push(p(&format!("{entry}.Group"), *group));
        }
    }
    params
}

#[tokio::test]
async fn a_grant_records_exactly_the_grantees_the_request_names() {
    let (router, _engine) = emulator();
    // One row per way a grant can differ: the form it arrives in, the kind of grantee it names,
    // how many it names, and whether one entry names both kinds at once.
    let rows: Vec<Row> = vec![
        (
            "one account, the flat form",
            |snapshot_id| flat(snapshot_id, "add", &[OTHER_ACCOUNTS[0]], &[]),
            vec![shared_with(OTHER_ACCOUNTS[0])],
        ),
        (
            "three accounts, the flat form",
            |snapshot_id| flat(snapshot_id, "add", &OTHER_ACCOUNTS, &[]),
            OTHER_ACCOUNTS.iter().map(|id| shared_with(id)).collect(),
        ),
        (
            "the all group, the flat form",
            |snapshot_id| {
                flat(
                    snapshot_id,
                    "add",
                    &[],
                    &[CREATE_VOLUME_PERMISSION_GROUP_ALL],
                )
            },
            vec![shared_publicly()],
        ),
        (
            "one account, the structured form",
            |snapshot_id| structured(snapshot_id, "Add", &[(OTHER_ACCOUNTS[1], "")]),
            vec![shared_with(OTHER_ACCOUNTS[1])],
        ),
        (
            "two accounts in one structured modification",
            |snapshot_id| {
                structured(
                    snapshot_id,
                    "Add",
                    &[(OTHER_ACCOUNTS[0], ""), (OTHER_ACCOUNTS[2], "")],
                )
            },
            vec![
                shared_with(OTHER_ACCOUNTS[0]),
                shared_with(OTHER_ACCOUNTS[2]),
            ],
        ),
        (
            "the all group, the structured form",
            |snapshot_id| {
                structured(
                    snapshot_id,
                    "Add",
                    &[("", CREATE_VOLUME_PERMISSION_GROUP_ALL)],
                )
            },
            vec![shared_publicly()],
        ),
        (
            "an account and the group in one structured entry",
            |snapshot_id| {
                structured(
                    snapshot_id,
                    "Add",
                    &[(OTHER_ACCOUNTS[1], CREATE_VOLUME_PERMISSION_GROUP_ALL)],
                )
            },
            vec![shared_publicly(), shared_with(OTHER_ACCOUNTS[1])],
        ),
        (
            "an account named twice in one modification",
            |snapshot_id| {
                structured(
                    snapshot_id,
                    "Add",
                    &[(OTHER_ACCOUNTS[0], ""), (OTHER_ACCOUNTS[0], "")],
                )
            },
            vec![shared_with(OTHER_ACCOUNTS[0])],
        ),
    ];

    for (case, params, expected) in rows {
        let snapshot_id = create_snapshot(&router, case).await;
        // A snapshot of the account starts out shared with nobody: its owner's permission is
        // implicit rather than one of the permissions this attribute lists.
        let before = permissions_of(&router, case, &snapshot_id).await;
        assert!(
            before.is_empty(),
            "case {case:?}: a new snapshot of the account must carry no create volume \
             permission, got {before:?}"
        );
        modify_successfully(&router, case, &params(&snapshot_id)).await;
        let reported = permissions_of(&router, case, &snapshot_id).await;
        let expected: BTreeSet<(String, String)> = expected.into_iter().collect();
        assert_eq!(
            reported, expected,
            "case {case:?}: expected the create volume permissions {expected:?}, got {reported:?}"
        );
        // Granting again is the same state: a permission is a membership of a list, so a
        // repeated grant adds nothing.
        modify_successfully(&router, case, &params(&snapshot_id)).await;
        let repeated = permissions_of(&router, case, &snapshot_id).await;
        assert_eq!(
            repeated, expected,
            "case {case:?}: a repeated grant must leave the create volume permissions \
             {expected:?}, got {repeated:?}"
        );
    }
}

#[tokio::test]
async fn a_removal_drops_exactly_the_grantees_it_names() {
    let (router, _engine) = emulator();
    // Every row starts from the same granted set — all three accounts and the public group — and
    // differs in which grantees it takes away and in the form it takes them away in.
    let rows: Vec<Row> = vec![
        (
            "one account, the flat form",
            |snapshot_id| flat(snapshot_id, "remove", &[OTHER_ACCOUNTS[0]], &[]),
            vec![
                shared_publicly(),
                shared_with(OTHER_ACCOUNTS[1]),
                shared_with(OTHER_ACCOUNTS[2]),
            ],
        ),
        (
            "two accounts, the structured form",
            |snapshot_id| {
                structured(
                    snapshot_id,
                    "Remove",
                    &[(OTHER_ACCOUNTS[1], ""), (OTHER_ACCOUNTS[2], "")],
                )
            },
            vec![shared_publicly(), shared_with(OTHER_ACCOUNTS[0])],
        ),
        (
            "the all group alone, the structured form",
            |snapshot_id| {
                structured(
                    snapshot_id,
                    "Remove",
                    &[("", CREATE_VOLUME_PERMISSION_GROUP_ALL)],
                )
            },
            OTHER_ACCOUNTS.iter().map(|id| shared_with(id)).collect(),
        ),
        (
            "the all group alone, the flat form",
            |snapshot_id| {
                flat(
                    snapshot_id,
                    "remove",
                    &[],
                    &[CREATE_VOLUME_PERMISSION_GROUP_ALL],
                )
            },
            OTHER_ACCOUNTS.iter().map(|id| shared_with(id)).collect(),
        ),
        (
            "every grantee at once",
            |snapshot_id| {
                flat(
                    snapshot_id,
                    "remove",
                    &OTHER_ACCOUNTS,
                    &[CREATE_VOLUME_PERMISSION_GROUP_ALL],
                )
            },
            Vec::new(),
        ),
        (
            "a grantee that was never granted",
            |snapshot_id| structured(snapshot_id, "Remove", &[("999988887777", "")]),
            {
                let mut held: Vec<(String, String)> =
                    OTHER_ACCOUNTS.iter().map(|id| shared_with(id)).collect();
                held.push(shared_publicly());
                held
            },
        ),
    ];

    for (case, params, expected) in rows {
        let snapshot_id = create_snapshot(&router, case).await;
        modify_successfully(
            &router,
            case,
            &flat(
                &snapshot_id,
                "add",
                &OTHER_ACCOUNTS,
                &[CREATE_VOLUME_PERMISSION_GROUP_ALL],
            ),
        )
        .await;
        modify_successfully(&router, case, &params(&snapshot_id)).await;
        let reported = permissions_of(&router, case, &snapshot_id).await;
        let expected: BTreeSet<(String, String)> = expected.into_iter().collect();
        assert_eq!(
            reported, expected,
            "case {case:?}: expected the create volume permissions {expected:?} to be left, \
             got {reported:?}"
        );
        // A removal repeated is the same state again: there is nothing left to take away.
        modify_successfully(&router, case, &params(&snapshot_id)).await;
        let repeated = permissions_of(&router, case, &snapshot_id).await;
        assert_eq!(
            repeated, expected,
            "case {case:?}: a repeated removal must leave the create volume permissions \
             {expected:?}, got {repeated:?}"
        );
    }
}

#[tokio::test]
async fn a_reset_clears_every_create_volume_permission() {
    let (router, _engine) = emulator();
    // One row per state a reset can be asked of: the kinds of grant differ, and one row has
    // nothing to clear at all.
    let rows: Vec<(&str, Vec<(String, String)>)> = vec![
        ("a snapshot nobody has been given", Vec::new()),
        (
            "a snapshot shared with one account",
            flat("", "add", &[OTHER_ACCOUNTS[0]], &[]),
        ),
        (
            "a snapshot shared with three accounts",
            flat("", "add", &OTHER_ACCOUNTS, &[]),
        ),
        (
            "a public snapshot",
            flat("", "add", &[], &[CREATE_VOLUME_PERMISSION_GROUP_ALL]),
        ),
        (
            "a snapshot both shared and public",
            flat(
                "",
                "add",
                &OTHER_ACCOUNTS,
                &[CREATE_VOLUME_PERMISSION_GROUP_ALL],
            ),
        ),
    ];

    for (case, granted) in rows {
        let snapshot_id = create_snapshot(&router, case).await;
        if !granted.is_empty() {
            // The row's grant, with the snapshot id this row minted written into it.
            let params: Vec<(String, String)> = granted
                .iter()
                .map(|(name, value)| {
                    if name == "SnapshotId" {
                        p(name, snapshot_id.clone())
                    } else {
                        (name.clone(), value.clone())
                    }
                })
                .collect();
            modify_successfully(&router, case, &params).await;
            let before = permissions_of(&router, case, &snapshot_id).await;
            assert!(
                !before.is_empty(),
                "case {case:?}: the row's own grant must stand before the reset, got {before:?}"
            );
        }
        for attempt in ["the first reset", "the reset repeated"] {
            let (status, body) = reset(
                &router,
                &snapshot_id,
                SNAPSHOT_ATTRIBUTE_CREATE_VOLUME_PERMISSION,
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?} ({attempt}): expected HTTP 200 from ResetSnapshotAttribute, \
                 got {status} with body {body}"
            );
            let reported = permissions_of(&router, case, &snapshot_id).await;
            assert!(
                reported.is_empty(),
                "case {case:?} ({attempt}): a reset must leave no create volume permission, \
                 got {reported:?}"
            );
        }
    }
}

#[tokio::test]
async fn the_inventory_selects_a_snapshot_by_who_may_restore_it() {
    let (router, _engine) = emulator();
    let case = "a snapshot shared with one account and then made public";
    let snapshot_id = create_snapshot(&router, case).await;

    // `self` selects it from the start: "an Amazon Web Services account has implicit create
    // volume permissions for all snapshots it owns".
    assert!(
        restorable_by(&router, case, "self").await.contains(&snapshot_id),
        "case {case:?}: the owner must be able to restore its own snapshot"
    );
    for term in [OTHER_ACCOUNTS[0], CREATE_VOLUME_PERMISSION_GROUP_ALL] {
        let selected = restorable_by(&router, case, term).await;
        assert!(
            !selected.contains(&snapshot_id),
            "case {case:?}: {term} must not select a snapshot nobody has shared, got {selected:?}"
        );
    }

    modify_successfully(
        &router,
        case,
        &flat(&snapshot_id, "add", &[OTHER_ACCOUNTS[0]], &[]),
    )
    .await;
    let shared = restorable_by(&router, case, OTHER_ACCOUNTS[0]).await;
    assert!(
        shared.contains(&snapshot_id),
        "case {case:?}: an account the snapshot was shared with must be able to restore it, \
         got {shared:?}"
    );
    let public = restorable_by(&router, case, CREATE_VOLUME_PERMISSION_GROUP_ALL).await;
    assert!(
        !public.contains(&snapshot_id),
        "case {case:?}: sharing with one account must not make the snapshot public, got \
         {public:?}"
    );
    // A grant to another account is not a grant to this one.
    let unshared = restorable_by(&router, case, OTHER_ACCOUNTS[1]).await;
    assert!(
        !unshared.contains(&snapshot_id),
        "case {case:?}: an account the snapshot was not shared with must not select it, got \
         {unshared:?}"
    );

    modify_successfully(
        &router,
        case,
        &flat(
            &snapshot_id,
            "add",
            &[],
            &[CREATE_VOLUME_PERMISSION_GROUP_ALL],
        ),
    )
    .await;
    let public = restorable_by(&router, case, CREATE_VOLUME_PERMISSION_GROUP_ALL).await;
    assert!(
        public.contains(&snapshot_id),
        "case {case:?}: a snapshot granted to the all group must be selected by all, got \
         {public:?}"
    );

    // And the reset takes both away again, so the narrowing and the attribute read agree about
    // one snapshot throughout.
    let (status, body) = reset(
        &router,
        &snapshot_id,
        SNAPSHOT_ATTRIBUTE_CREATE_VOLUME_PERMISSION,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from ResetSnapshotAttribute, got {status} with \
         body {body}"
    );
    for term in [OTHER_ACCOUNTS[0], CREATE_VOLUME_PERMISSION_GROUP_ALL] {
        let selected = restorable_by(&router, case, term).await;
        assert!(
            !selected.contains(&snapshot_id),
            "case {case:?}: after the reset {term} must select nothing, got {selected:?}"
        );
    }
}

/// The snapshot ids `DescribeSnapshots` reports for one `RestorableByUserIds` term.
async fn restorable_by(
    router: &MultiServiceRouter,
    case: &str,
    term: &str,
) -> BTreeSet<String> {
    let (status, body) = op(
        router,
        "DescribeSnapshots",
        &indexed("RestorableBy", &[term]),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeSnapshots restorable by {term}, got \
         {status} with body {body}"
    );
    let parsed = document(case, &body);
    parsed
        .descendants()
        .filter(|node| node.is_element() && child(*node, "snapshotId").is_some())
        .filter_map(|node| child_text(node, "snapshotId"))
        .collect()
}

#[tokio::test]
async fn a_refused_modification_records_nothing() {
    let (router, _engine) = emulator();
    let case = "the snapshot every refusal below names";
    let snapshot_id = create_snapshot(&router, case).await;
    // Every refusal is asked of a snapshot that is already shared with one account, so a
    // refusal that quietly wrote something — or quietly dropped the standing grant — is visible.
    modify_successfully(
        &router,
        case,
        &flat(&snapshot_id, "add", &[OTHER_ACCOUNTS[0]], &[]),
    )
    .await;
    let held = permissions_of(&router, case, &snapshot_id).await;

    // One row per cause, each with the parameters that reach it and the code EC2 answers.
    let mut rows: Vec<RefusalRow> = vec![
        (
            "an attribute this action cannot modify",
            {
                let mut params = flat(&snapshot_id, "add", &[OTHER_ACCOUNTS[1]], &[]);
                params.retain(|(name, _)| name != "Attribute");
                params.push(p("Attribute", SNAPSHOT_ATTRIBUTE_PRODUCT_CODES));
                params
            },
            INVALID_PARAMETER_VALUE,
        ),
        (
            "adding and removing in one operation",
            {
                let mut params =
                    structured(&snapshot_id, "Add", &[(OTHER_ACCOUNTS[1], "")]);
                params.extend(
                    structured(&snapshot_id, "Remove", &[(OTHER_ACCOUNTS[2], "")])
                        .into_iter()
                        .filter(|(name, _)| name != "SnapshotId"),
                );
                params
            },
            INVALID_PARAMETER_COMBINATION,
        ),
        (
            "grantee lists with no operation type",
            {
                let mut params = flat(&snapshot_id, "add", &[OTHER_ACCOUNTS[1]], &[]);
                params.retain(|(name, _)| name != "OperationType");
                params
            },
            MISSING_PARAMETER,
        ),
        (
            "a modification that names no grantee at all",
            vec![
                p("SnapshotId", snapshot_id.as_str()),
                p("Attribute", SNAPSHOT_ATTRIBUTE_CREATE_VOLUME_PERMISSION),
                p("OperationType", "add"),
            ],
            MISSING_PARAMETER,
        ),
        (
            "a user id that is no account id",
            flat(&snapshot_id, "add", &["not-an-account"], &[]),
            INVALID_USER_ID_MALFORMED,
        ),
        (
            "an account id of the wrong length",
            flat(&snapshot_id, "add", &["12345"], &[]),
            INVALID_USER_ID_MALFORMED,
        ),
        (
            "a group outside the modelled catalog",
            flat(&snapshot_id, "add", &[], &["everyone"]),
            INVALID_PARAMETER_VALUE,
        ),
        (
            "a structured entry naming a grantee that is no account id",
            vec![
                p("SnapshotId", snapshot_id.as_str()),
                p("CreateVolumePermission.Add.1.UserId", "not-an-account"),
            ],
            INVALID_USER_ID_MALFORMED,
        ),
        (
            "a value that is no snapshot id",
            flat("not-a-snapshot-id", "add", &[OTHER_ACCOUNTS[1]], &[]),
            INVALID_SNAPSHOT_ID_MALFORMED,
        ),
        (
            "an id in the printed form but outside the one EC2 issues",
            flat(UNISSUABLE_SNAPSHOT_ID, "add", &[OTHER_ACCOUNTS[1]], &[]),
            INVALID_SNAPSHOT_ID_MALFORMED,
        ),
        (
            "the all-zero placeholder id",
            flat(
                "snap-00000000000000000",
                "add",
                &[OTHER_ACCOUNTS[1]],
                &[],
            ),
            INVALID_SNAPSHOT_ID_MALFORMED,
        ),
        (
            "a well-formed snapshot id naming nothing",
            flat(UNKNOWN_SNAPSHOT_ID, "add", &[OTHER_ACCOUNTS[1]], &[]),
            INVALID_SNAPSHOT_ID_NOT_FOUND,
        ),
        (
            "a dry run of a modification that would have succeeded",
            {
                let mut params = flat(&snapshot_id, "add", &[OTHER_ACCOUNTS[1]], &[]);
                params.push(p("DryRun", "true"));
                params
            },
            DRY_RUN_OPERATION,
        ),
    ];
    // "You can make up to 500 modifications to a snapshot in a single operation": one past the
    // published bound, built from account ids this suite generates rather than listed.
    let too_many: Vec<String> = (0..=MAX_SNAPSHOT_ATTRIBUTE_MODIFICATIONS)
        .map(|nth| format!("{:012}", nth + 1))
        .collect();
    rows.push((
        "one modification past the published maximum",
        flat(
            &snapshot_id,
            "add",
            &too_many.iter().map(String::as_str).collect::<Vec<&str>>(),
            &[],
        ),
        INVALID_PARAMETER_VALUE,
    ));

    for (case, params, expected) in rows {
        let (status, body) = modify(&router, &params).await;
        let expected_status = if expected == DRY_RUN_OPERATION {
            DRY_RUN_OPERATION_STATUS
        } else {
            400
        };
        assert_eq!(
            status, expected_status,
            "case {case:?}: expected HTTP {expected_status}, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            expected,
            "case {case:?}: expected the error code {expected}, got body {body}"
        );
        let after = permissions_of(&router, case, &snapshot_id).await;
        assert_eq!(
            after, held,
            "case {case:?}: a refused modification must leave the create volume permissions \
             {held:?}, got {after:?}"
        );
    }

    // The same bound on the reset's own attribute: only the permission to create volumes can be
    // reset, and a refused reset clears nothing.
    let (status, body) = reset(&router, &snapshot_id, SNAPSHOT_ATTRIBUTE_PRODUCT_CODES).await;
    assert_eq!(
        status, 400,
        "a reset of the product codes attribute must be refused, got {status} with body {body}"
    );
    assert_eq!(
        error_code(&body),
        INVALID_PARAMETER_VALUE,
        "a reset of an attribute that cannot be reset must be refused as a bad parameter, got \
         body {body}"
    );
    let after = permissions_of(&router, case, &snapshot_id).await;
    assert_eq!(
        after, held,
        "a refused reset must leave the create volume permissions {held:?}, got {after:?}"
    );

    // And a dry run of the reset leaves them standing too.
    let (status, body) = op(
        &router,
        "ResetSnapshotAttribute",
        &[
            p("SnapshotId", snapshot_id.as_str()),
            p("Attribute", SNAPSHOT_ATTRIBUTE_CREATE_VOLUME_PERMISSION),
            p("DryRun", "true"),
        ],
    )
    .await;
    assert_eq!(
        status, DRY_RUN_OPERATION_STATUS,
        "a dry run of the reset must answer {DRY_RUN_OPERATION_STATUS}, got {status} with \
         body {body}"
    );
    assert_eq!(
        error_code(&body),
        DRY_RUN_OPERATION,
        "a dry run of the reset must answer {DRY_RUN_OPERATION}, got body {body}"
    );
    let after = permissions_of(&router, case, &snapshot_id).await;
    assert_eq!(
        after, held,
        "a dry run of the reset must leave the create volume permissions {held:?}, got {after:?}"
    );
}

#[tokio::test]
async fn public_sharing_is_refused_for_a_snapshot_that_cannot_be_made_public() {
    let (router, _engine) = emulator();
    // "Encrypted snapshots … cannot be made public": a snapshot taken of an encrypted volume is
    // itself encrypted, so the rule is reached through the snapshot's source.
    let case = "an encrypted snapshot of the account";
    let encrypted = snapshot_of_volume(&router, case, vec![p("Encrypted", "true")]).await;
    let (status, body) = modify(
        &router,
        &flat(
            &encrypted,
            "add",
            &[],
            &[CREATE_VOLUME_PERMISSION_GROUP_ALL],
        ),
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: making an encrypted snapshot public must be refused, got {status} \
         with body {body}"
    );
    assert_eq!(
        error_code(&body),
        INVALID_PARAMETER_VALUE,
        "case {case:?}: expected the bad-parameter code, got body {body}"
    );
    let reported = permissions_of(&router, case, &encrypted).await;
    assert!(
        reported.is_empty(),
        "case {case:?}: the refused grant must have recorded nothing, got {reported:?}"
    );
    // Sharing the same encrypted snapshot with ONE account is a different request, and it
    // stands: only PUBLIC sharing is what the rule withdraws.
    modify_successfully(
        &router,
        case,
        &flat(&encrypted, "add", &[OTHER_ACCOUNTS[0]], &[]),
    )
    .await;
    let reported = permissions_of(&router, case, &encrypted).await;
    assert_eq!(
        reported,
        [shared_with(OTHER_ACCOUNTS[0])].into_iter().collect(),
        "case {case:?}: an encrypted snapshot may still be shared with an account, got \
         {reported:?}"
    );

    // "After you enable block public access for snapshots in a Region, users can no longer
    // request public sharing for snapshots in that Region" — in either blocking mode.
    for mode in SNAPSHOT_BLOCK_PUBLIC_ACCESS_MODES {
        let case = &format!("block public access in {mode} mode");
        let snapshot_id = create_snapshot(&router, case).await;
        let (status, body) = op(
            &router,
            "EnableSnapshotBlockPublicAccess",
            &[p("State", mode)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from EnableSnapshotBlockPublicAccess, got \
             {status} with body {body}"
        );
        let (status, body) = modify(
            &router,
            &flat(
                &snapshot_id,
                "add",
                &[],
                &[CREATE_VOLUME_PERMISSION_GROUP_ALL],
            ),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: public sharing must be refused while it is blocked, got {status} \
             with body {body}"
        );
        assert_eq!(
            error_code(&body),
            OPERATION_NOT_PERMITTED,
            "case {case:?}: expected the not-permitted code, got body {body}"
        );
        let reported = permissions_of(&router, case, &snapshot_id).await;
        assert!(
            reported.is_empty(),
            "case {case:?}: the refused grant must have recorded nothing, got {reported:?}"
        );
        // A grant to one account is not public sharing, so the block does not reach it.
        modify_successfully(
            &router,
            case,
            &flat(&snapshot_id, "add", &[OTHER_ACCOUNTS[1]], &[]),
        )
        .await;
        let reported = permissions_of(&router, case, &snapshot_id).await;
        assert_eq!(
            reported,
            [shared_with(OTHER_ACCOUNTS[1])].into_iter().collect(),
            "case {case:?}: sharing with one account must stand while public sharing is \
             blocked, got {reported:?}"
        );
        // Unblocked again, the same public grant is served: the setting is the only thing that
        // refused it.
        let (status, body) = op(&router, "DisableSnapshotBlockPublicAccess", &[]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DisableSnapshotBlockPublicAccess, got \
             {status} with body {body}"
        );
        modify_successfully(
            &router,
            case,
            &flat(
                &snapshot_id,
                "add",
                &[],
                &[CREATE_VOLUME_PERMISSION_GROUP_ALL],
            ),
        )
        .await;
        let reported = permissions_of(&router, case, &snapshot_id).await;
        assert!(
            reported.contains(&shared_publicly()),
            "case {case:?}: the public grant must stand once the block is lifted, got \
             {reported:?}"
        );
    }
}

#[tokio::test]
async fn a_snapshot_of_another_account_is_not_this_accounts_to_share() {
    let (router, _engine) = emulator();
    let case = "a public snapshot of the published AMI catalogue";
    let snapshot_id = published_snapshot(&router, case).await;
    // It is visible, and reported public, because it is published — so the refusal below is
    // about who may CHANGE its permissions, not about whether the id resolves.
    let reported = permissions_of(&router, case, &snapshot_id).await;
    assert!(
        reported.contains(&shared_publicly()),
        "case {case:?}: a published snapshot must be reported public, got {reported:?}"
    );

    for (action, params) in [
        (
            "ModifySnapshotAttribute",
            flat(&snapshot_id, "add", &[OTHER_ACCOUNTS[0]], &[]),
        ),
        (
            "ResetSnapshotAttribute",
            vec![
                p("SnapshotId", snapshot_id.as_str()),
                p("Attribute", SNAPSHOT_ATTRIBUTE_CREATE_VOLUME_PERMISSION),
            ],
        ),
    ] {
        let (status, body) = op(&router, action, &params).await;
        assert_eq!(
            status, 400,
            "case {case:?}: {action} on a snapshot of another account must be refused, got \
             {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            OPERATION_NOT_PERMITTED,
            "case {case:?}: expected the not-permitted code from {action}, got body {body}"
        );
    }
    let after = permissions_of(&router, case, &snapshot_id).await;
    assert_eq!(
        after, reported,
        "case {case:?}: the refused calls must leave the published snapshot's permissions \
         {reported:?}, got {after:?}"
    );
}

/// One snapshot the published AMI catalogue restores a device from — a snapshot of the
/// inventory the calling account does NOT own, read off the catalogue rather than written here.
async fn published_snapshot(router: &MultiServiceRouter, case: &str) -> String {
    let mut params = indexed("Owner", &[IMAGE_OWNER_ALIAS_AMAZON]);
    params.push(p("IncludeDeprecated", "true"));
    let (status, body) = op(router, "DescribeImages", &params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeImages, got {status} with body {body}"
    );
    let parsed = document(case, &body);
    parsed
        .descendants()
        .filter(|node| node.is_element() && node.tag_name().name() == "ebs")
        .filter_map(|ebs| child_text(ebs, "snapshotId"))
        .find(|snapshot_id| !snapshot_id.is_empty())
        .unwrap_or_else(|| {
            panic!("case {case:?}: the catalogue restores its root devices from snapshots: {body}")
        })
}

#[tokio::test]
async fn a_deleted_snapshot_keeps_no_create_volume_permission() {
    let (router, engine) = emulator();
    let case = "a shared snapshot that is then deleted";
    let snapshot_id = create_snapshot(&router, case).await;
    let charged_before = stored_bytes(&engine);
    modify_successfully(
        &router,
        case,
        &flat(
            &snapshot_id,
            "add",
            &OTHER_ACCOUNTS,
            &[CREATE_VOLUME_PERMISSION_GROUP_ALL],
        ),
    )
    .await;
    assert!(
        stored_bytes(&engine) > charged_before,
        "case {case:?}: the grants retain caller-chosen account ids, so they must be charged \
         against the store's budget"
    );
    assert!(
        recorded_grantees(&engine, &snapshot_id) > 0,
        "case {case:?}: the grants must be recorded against the snapshot"
    );

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
    assert_eq!(
        recorded_grantees(&engine, &snapshot_id),
        0,
        "case {case:?}: a snapshot that has left the inventory can share nothing, so its \
         grants must go with it"
    );
    assert_eq!(
        stored_bytes(&engine),
        charged_before,
        "case {case:?}: the bytes the grants were charged must be given back"
    );
    // And the id stops resolving for the attribute read, so no later caller sees the grants.
    let (status, body) = op(
        &router,
        "DescribeSnapshotAttribute",
        &[
            p("SnapshotId", snapshot_id.as_str()),
            p("Attribute", SNAPSHOT_ATTRIBUTE_CREATE_VOLUME_PERMISSION),
        ],
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: the deleted snapshot's id must stop resolving, got {status} with \
         body {body}"
    );
    assert_eq!(
        error_code(&body),
        INVALID_SNAPSHOT_ID_NOT_FOUND,
        "case {case:?}: expected the not-found code, got body {body}"
    );
}

/// The caller-chosen bytes the EC2 store has charged right now.
fn stored_bytes(engine: &Arc<engine::InMemoryEngine>) -> usize {
    ec2::state::service_state(&**engine)
        .expect("the EC2 store")
        .lock()
        .stored_bytes()
}

/// How many create volume permissions the store holds against `snapshot_id`.
fn recorded_grantees(engine: &Arc<engine::InMemoryEngine>, snapshot_id: &str) -> usize {
    let state = ec2::state::service_state(&**engine).expect("the EC2 store");
    let store = state.lock();
    store
        .snapshot_create_volume_permissions
        .get(snapshot_id)
        .map_or(0, |permissions| {
            permissions.user_ids.len() + permissions.groups.len()
        })
}
