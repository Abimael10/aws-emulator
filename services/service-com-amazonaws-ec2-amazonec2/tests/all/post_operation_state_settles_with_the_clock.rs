// SPDX-License-Identifier: CC0-1.0

//! What a MUTATING EC2 call reports about the state it leaves behind, driven over the wire
//! the service actually speaks: `ec2Query` — an `application/x-www-form-urlencoded` POST
//! whose body carries `Action=<Operation>`, answered with XML (README.md / OPERATIONS.md
//! "Wire protocol"). ec2Query is selected by the `ec2` signing name in the request's SigV4
//! credential scope, and the Region in that scope is the Region the service answers for, so
//! every call here carries one.
//!
//! The rules under test:
//!
//! 1. A CALL THAT STARTS A TEARDOWN IS ANSWERED IN THE TRANSITIONAL STATE, and the CLOCK —
//!    not a later call — carries the resource to the terminal one: `DisassociateRouteServer`
//!    answers its association `disassociating`, `GetRouteServerAssociations` reports that
//!    word until the move falls due and reports the pair no longer once the disassociation
//!    has been carried out, and the pair it took apart joins the two no longer from the call
//!    on, so the VPC can be associated again.
//! 2. A PER-ITEM CALL REPORTS EACH ITEM'S OWN OUTCOME: `DisableFastSnapshotRestores` turns
//!    off only a (snapshot, Availability Zone) pair the account has a fast snapshot restore
//!    IN FORCE for, and reports every other pair under its snapshot id in `Unsuccessful`
//!    with that pair's own cause — a pair with no restore as `InvalidParameterValue`, a
//!    snapshot the account does not hold as that id's own refusal.
//! 3. A NEW VERSION OF A MANAGED PREFIX LIST IS CREATED WHEN THE CHANGE IS CARRIED OUT:
//!    "adding or removing entries in a prefix list creates a new version of the prefix list"
//!    and "changing the name of the prefix list does not affect the version", so the call
//!    that accepts a modification answers the version the list STANDS at, and the revised
//!    entry set and the version it belongs to come into force together with the state the
//!    clock settles the list into.
//! 4. A SNAPSHOT MEASURES THE BLOCKS ITS SOURCE HELD: "the full size of the snapshot […]
//!    represents the size of all the blocks that were written to the source volume at the
//!    time the snapshot was created" (`Snapshot$FullSnapshotSizeInBytes`), so a snapshot of a
//!    volume created empty measures none of it whatever that volume's size, and a copy of
//!    such a snapshot measures exactly what the snapshot it copies measures.
//!
//! Each table row is a distinct input along what the rule generalises over — which state the
//! association had reached, which cause a pair fails for, which kind of modification a prefix
//! list takes, which size the source volume was created at — so no row is the specification.

use std::time::Duration;

use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use ec2::state::{
    FIRST_PREFIX_LIST_VERSION, INVALID_PARAMETER_VALUE, INVALID_SNAPSHOT_ID_NOT_FOUND,
    PREFIX_LIST_ADDRESS_FAMILY_IPV4, PREFIX_LIST_STATE_CREATE_COMPLETE,
    availability_zones_in_region,
};

use super::ec2_query::{element, error_code};
use super::wire::{Param, child, child_text, document, param, set_items};

/// The Region every call here is signed for.
const REGION: &str = "us-west-2";

/// The `RouteServerAssociationState` words a disassociation moves an association through
/// (`smithy_query(query="shape",shape="RouteServerAssociationState")`, whose members are
/// `associating`, `associated`, `disassociating` and `disassociated`).
const ASSOCIATED: &str = "associated";
const DISASSOCIATING: &str = "disassociating";

/// The two words of the modelled `PrefixListState` catalog a revision moves a list through
/// (`smithy_query(query="shape",shape="PrefixListState")`).
const MODIFY_IN_PROGRESS: &str = "modify-in-progress";
const MODIFY_COMPLETE: &str = "modify-complete";

/// The message EC2 words for a (snapshot, zone) pair a disable finds no fast snapshot restore
/// in force for — fixed text, naming neither the snapshot nor the zone, both of which the
/// item carrying it already reports.
const NO_RESTORE_IN_FORCE: &str = "Fast Restore is not enabled for this snapshot";

/// How long the polls below give a clock-owed move before they give up: generous, because
/// what is under test is the state a resource REACHES rather than how fast it reaches it, so
/// a loaded host cannot turn "not yet" into a failure.
fn move_deadline() -> Duration {
    Duration::from_secs(60)
}

/// How long a poll waits between two reads.
fn poll_interval() -> Duration {
    Duration::from_millis(25)
}

/// One `action` call carrying `params`, as its HTTP status and XML body.
async fn op(router: &MultiServiceRouter, action: &str, params: &[Param]) -> (u16, String) {
    super::ec2_query::call(router, REGION, action, params).await
}

/// One `action` call that must answer HTTP 200, as its XML body.
async fn ok(router: &MultiServiceRouter, case: &str, action: &str, params: &[Param]) -> String {
    let (status, body) = op(router, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// The `zone_name` of zone `index` of the signed Region.
fn zone_name(index: usize) -> String {
    availability_zones_in_region(REGION)[index].zone_name.clone()
}

// ---------------------------------------------------------------------------
// Rule 1: a teardown is answered in the transitional state
// ---------------------------------------------------------------------------

/// Which state the association had reached when the row disassociated it.
#[derive(Debug, Clone, Copy)]
enum When {
    /// Straight after the associate, while the `associating` -> `associated` move is still
    /// owed.
    StillAssociating,
    /// Once the clock has carried the association to `associated`.
    OnceAssociated,
}

/// The state `GetRouteServerAssociations` reports for the pair, or `None` when it reports no
/// association of that route server at all.
async fn association_state(
    router: &MultiServiceRouter,
    case: &str,
    route_server_id: &str,
) -> Option<String> {
    let body = ok(
        router,
        case,
        "GetRouteServerAssociations",
        &[param("RouteServerId", route_server_id)],
    )
    .await;
    let doc = document(case, &body);
    set_items(doc.root_element(), "routeServerAssociationSet")
        .into_iter()
        .find_map(|item| child_text(item, "state"))
}

/// The state the pair reports once it is `wanted`: read AGAIN in a loop until it reads that
/// way or a deadline far past the transition span passes, so the wait is for the CONDITION
/// rather than for a span of the clock.
async fn poll_association_state(
    router: &MultiServiceRouter,
    case: &str,
    route_server_id: &str,
    wanted: &str,
) -> Option<String> {
    let deadline = std::time::Instant::now() + move_deadline();
    loop {
        let state = association_state(router, case, route_server_id).await;
        if state.as_deref() == Some(wanted) || std::time::Instant::now() >= deadline {
            break state;
        }
        tokio::time::sleep(poll_interval()).await;
    }
}

/// Whether the route server reports any association at all, read in a loop until it reports
/// NONE or the same generous deadline passes: the pair a disassociation has taken apart is
/// reported no longer, which is the condition this waits for.
async fn poll_until_no_association(
    router: &MultiServiceRouter,
    case: &str,
    route_server_id: &str,
) -> Option<String> {
    let deadline = std::time::Instant::now() + move_deadline();
    loop {
        let state = association_state(router, case, route_server_id).await;
        if state.is_none() || std::time::Instant::now() >= deadline {
            break state;
        }
        tokio::time::sleep(poll_interval()).await;
    }
}

/// Rule 1: the disassociation is answered `disassociating`, the clock carries the pair to its
/// terminal state — after which the account holds no such association — and the pair holds
/// the VPC no longer from the call on.
#[tokio::test]
async fn a_disassociation_is_answered_in_progress_and_the_clock_settles_it() {
    let (router, _engine) = super::ec2_query::emulator();

    for (case, when) in [
        ("a pair whose association is still owed", When::StillAssociating),
        ("a pair the clock has associated", When::OnceAssociated),
    ] {
        let vpc = ok(
            &router,
            case,
            "CreateVpc",
            &[param("CidrBlock", "10.60.0.0/16")],
        )
        .await;
        let vpc_id = element(&vpc, "vpcId")
            .unwrap_or_else(|| panic!("case {case:?}: CreateVpc reports the VPC it minted"));
        let created = ok(
            &router,
            case,
            "CreateRouteServer",
            &[param("AmazonSideAsn", "64512")],
        )
        .await;
        let route_server_id = element(&created, "routeServerId").unwrap_or_else(|| {
            panic!("case {case:?}: CreateRouteServer reports the route server it minted")
        });

        let pairing = [
            param("RouteServerId", &route_server_id),
            param("VpcId", &vpc_id),
        ];
        ok(&router, case, "AssociateRouteServer", &pairing).await;
        if matches!(when, When::OnceAssociated) {
            let settled = poll_association_state(&router, case, &route_server_id, ASSOCIATED).await;
            assert_eq!(
                settled.as_deref(),
                Some(ASSOCIATED),
                "case {case:?}: the association must settle before the row disassociates it, \
                 got {settled:?}"
            );
        }

        let answered = ok(&router, case, "DisassociateRouteServer", &pairing).await;
        assert_eq!(
            element(&answered, "state").as_deref(),
            Some(DISASSOCIATING),
            "case {case:?}: a disassociation is answered in the state it STARTS, got {answered}"
        );
        let straight_after = association_state(&router, case, &route_server_id).await;
        assert_eq!(
            straight_after.as_deref(),
            Some(DISASSOCIATING),
            "case {case:?}: the association reports the word the call left it in, got \
             {straight_after:?}"
        );

        // The pair holds the two apart from the call on, whatever the clock has reached: a
        // second disassociation of it has nothing to take apart, and the VPC is free to be
        // served again.
        let (status, body) = op(&router, "DisassociateRouteServer", &pairing).await;
        assert_eq!(
            error_code(&body),
            "InvalidRouteServerId.NotAssociated",
            "case {case:?}: a pair already taken apart has nothing to disassociate, got \
             {status} with body {body}"
        );

        // The clock carries the pair to its terminal state, at which instant the account
        // holds no such association any more — so what the describe reports is NO
        // association of this route server.
        let terminal = poll_until_no_association(&router, case, &route_server_id).await;
        assert_eq!(
            terminal, None,
            "case {case:?}: the CLOCK must carry the pair to its terminal state, after which \
             the account holds no such association, got {terminal:?}"
        );

        // The VPC can be associated again, which is what "the pair joins the two no longer"
        // means for the resource.
        let again = ok(&router, case, "AssociateRouteServer", &pairing).await;
        assert_eq!(
            element(&again, "vpcId").as_deref(),
            Some(vpc_id.as_str()),
            "case {case:?}: the VPC must be associable again, got {again}"
        );
    }
}

// ---------------------------------------------------------------------------
// Rule 2: a disable reports each pair's own outcome
// ---------------------------------------------------------------------------

/// Create a volume of `size` GiB in the signed Region's first zone and a snapshot of it, and
/// answer the pair of ids.
async fn volume_and_snapshot(
    router: &MultiServiceRouter,
    case: &str,
    size: i64,
) -> (String, String) {
    let created = ok(
        router,
        case,
        "CreateVolume",
        &[
            param("AvailabilityZone", &zone_name(0)),
            param("Size", &size.to_string()),
        ],
    )
    .await;
    let volume_id = element(&created, "volumeId")
        .unwrap_or_else(|| panic!("case {case:?}: CreateVolume reports the volume it minted"));
    let taken = ok(
        router,
        case,
        "CreateSnapshot",
        &[param("VolumeId", &volume_id)],
    )
    .await;
    let snapshot_id = element(&taken, "snapshotId")
        .unwrap_or_else(|| panic!("case {case:?}: CreateSnapshot reports the snapshot it minted"));
    (volume_id, snapshot_id)
}

/// The `(snapshotId, code, message)` triples one answer reports in `unsuccessful`, one per
/// (snapshot, zone) pair that failed.
fn per_pair_failures(case: &str, body: &str) -> Vec<(String, String, String)> {
    let doc = document(case, body);
    set_items(doc.root_element(), "unsuccessful")
        .into_iter()
        .flat_map(|item| {
            let snapshot_id = child_text(item, "snapshotId").unwrap_or_default();
            set_items(item, "fastSnapshotRestoreStateErrorSet")
                .into_iter()
                .map(move |pair| {
                    let error = child(pair, "error");
                    (
                        snapshot_id.clone(),
                        error
                            .and_then(|error| child_text(error, "code"))
                            .unwrap_or_default(),
                        error
                            .and_then(|error| child_text(error, "message"))
                            .unwrap_or_default(),
                    )
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

/// The `snapshotId`s one answer reports in `successful`.
fn turned_off(case: &str, body: &str) -> Vec<String> {
    let doc = document(case, body);
    set_items(doc.root_element(), "successful")
        .into_iter()
        .filter_map(|item| child_text(item, "snapshotId"))
        .collect()
}

/// Rule 2: one call naming three snapshots of the same zone — one with a restore in force, one
/// with none, and one id the account holds no snapshot for — reports each on its own.
#[tokio::test]
async fn a_disable_turns_off_only_the_pairs_with_a_restore_in_force() {
    let case = "three snapshots in one call";
    let (router, _engine) = super::ec2_query::emulator();

    let (_, enabled) = volume_and_snapshot(&router, case, 1).await;
    let (_, never_enabled) = volume_and_snapshot(&router, case, 8).await;
    // A well-formed id of the published form the account holds no snapshot for: the third
    // cause, whose refusal is the SNAPSHOT's rather than the pair's.
    let unknown = "snap-0123456789abcdef0";

    ok(
        &router,
        case,
        "EnableFastSnapshotRestores",
        &[
            param("SourceSnapshotId.1", &enabled),
            param("AvailabilityZone.1", &zone_name(0)),
        ],
    )
    .await;

    let body = ok(
        &router,
        case,
        "DisableFastSnapshotRestores",
        &[
            param("SourceSnapshotId.1", &enabled),
            param("SourceSnapshotId.2", &never_enabled),
            param("SourceSnapshotId.3", unknown),
            param("AvailabilityZone.1", &zone_name(0)),
        ],
    )
    .await;
    assert_eq!(
        turned_off(case, &body),
        vec![enabled.clone()],
        "case {case:?}: only the pair with a restore in force is turned off, got {body}"
    );
    let failures = per_pair_failures(case, &body);
    assert_eq!(
        failures,
        vec![
            (
                never_enabled.clone(),
                INVALID_PARAMETER_VALUE.to_string(),
                NO_RESTORE_IN_FORCE.to_string(),
            ),
            (
                unknown.to_string(),
                INVALID_SNAPSHOT_ID_NOT_FOUND.to_string(),
                format!("The snapshot '{unknown}' does not exist."),
            ),
        ],
        "case {case:?}: each failing pair carries its own cause, got {failures:?}"
    );
}

// ---------------------------------------------------------------------------
// Rule 3: a prefix list's new version comes into force when the change is carried out
// ---------------------------------------------------------------------------

/// One modification of a prefix list: the form fields it sends, and whether it revises the
/// ENTRY SET — which is what mints a new version.
struct ModificationCase {
    /// What the row is called in an assertion.
    name: &'static str,
    /// The modification's own form fields, beside the `PrefixListId`.
    fields: Vec<Param>,
    /// Whether the modification revises the entry set.
    revises_entries: bool,
    /// The CIDR blocks the list carries once the modification has been carried out.
    settled_blocks: Vec<&'static str>,
}

/// The rows: every kind of modification `ModifyManagedPrefixList` takes — the three that
/// revise the entry set and the two that do not.
fn modification_cases() -> Vec<ModificationCase> {
    vec![
        ModificationCase {
            name: "an entry added",
            fields: vec![param("AddEntry.1.Cidr", "10.72.0.0/16")],
            revises_entries: true,
            settled_blocks: vec!["10.70.0.0/16", "10.71.0.0/16", "10.72.0.0/16"],
        },
        ModificationCase {
            name: "an entry removed",
            fields: vec![param("RemoveEntry.1.Cidr", "10.70.0.0/16")],
            revises_entries: true,
            settled_blocks: vec!["10.71.0.0/16"],
        },
        ModificationCase {
            name: "an entry added and another removed",
            fields: vec![
                param("AddEntry.1.Cidr", "10.73.0.0/16"),
                param("RemoveEntry.1.Cidr", "10.71.0.0/16"),
            ],
            revises_entries: true,
            settled_blocks: vec!["10.70.0.0/16", "10.73.0.0/16"],
        },
        ModificationCase {
            name: "a rename",
            fields: vec![param("PrefixListName", "the-renamed-list")],
            revises_entries: false,
            settled_blocks: vec!["10.70.0.0/16", "10.71.0.0/16"],
        },
        ModificationCase {
            name: "a larger maximum size",
            fields: vec![param("MaxEntries", "9")],
            revises_entries: false,
            settled_blocks: vec!["10.70.0.0/16", "10.71.0.0/16"],
        },
    ]
}

/// The CIDR blocks `GetManagedPrefixListEntries` reports for the version the list stands at.
async fn reported_blocks(router: &MultiServiceRouter, case: &str, prefix_list_id: &str) -> Vec<String> {
    let body = ok(
        router,
        case,
        "GetManagedPrefixListEntries",
        &[param("PrefixListId", prefix_list_id)],
    )
    .await;
    let doc = document(case, &body);
    set_items(doc.root_element(), "entrySet")
        .into_iter()
        .filter_map(|item| child_text(item, "cidr"))
        .collect()
}

/// The state `DescribeManagedPrefixLists` reports for the list, polled until it is `wanted`.
async fn poll_prefix_list_state(
    router: &MultiServiceRouter,
    case: &str,
    prefix_list_id: &str,
    wanted: &str,
) -> String {
    let deadline = std::time::Instant::now() + move_deadline();
    loop {
        let body = ok(
            router,
            case,
            "DescribeManagedPrefixLists",
            &[param("PrefixListId.1", prefix_list_id)],
        )
        .await;
        let state = element(&body, "state").unwrap_or_default();
        if state == wanted || std::time::Instant::now() >= deadline {
            break state;
        }
        tokio::time::sleep(poll_interval()).await;
    }
}

/// The version `DescribeManagedPrefixLists` reports for the list.
async fn reported_version(router: &MultiServiceRouter, case: &str, prefix_list_id: &str) -> String {
    let body = ok(
        router,
        case,
        "DescribeManagedPrefixLists",
        &[param("PrefixListId.1", prefix_list_id)],
    )
    .await;
    element(&body, "version").unwrap_or_default()
}

/// Rule 3: the call that accepts a modification answers the version the list stands at, and
/// only a modification that revises the entry set mints a new one — once it is carried out.
#[tokio::test]
async fn a_prefix_lists_new_version_comes_into_force_when_the_revision_is_carried_out() {
    let (router, _engine) = super::ec2_query::emulator();

    for (index, case) in modification_cases().into_iter().enumerate() {
        let name = case.name;
        let created = ok(
            &router,
            name,
            "CreateManagedPrefixList",
            &[
                param("PrefixListName", &format!("modification-case-{index}")),
                param("AddressFamily", PREFIX_LIST_ADDRESS_FAMILY_IPV4),
                param("MaxEntries", "4"),
                param("Entry.1.Cidr", "10.70.0.0/16"),
                param("Entry.2.Cidr", "10.71.0.0/16"),
            ],
        )
        .await;
        let prefix_list_id = element(&created, "prefixListId").unwrap_or_else(|| {
            panic!("case {name:?}: CreateManagedPrefixList reports the list it minted")
        });
        let settled = poll_prefix_list_state(
            &router,
            name,
            &prefix_list_id,
            PREFIX_LIST_STATE_CREATE_COMPLETE,
        )
        .await;
        assert_eq!(
            settled, PREFIX_LIST_STATE_CREATE_COMPLETE,
            "case {name:?}: the create must settle before the modification, got {settled:?}"
        );

        let mut fields = vec![param("PrefixListId", &prefix_list_id)];
        fields.extend(case.fields.clone());
        let answered = ok(&router, name, "ModifyManagedPrefixList", &fields).await;
        assert_eq!(
            element(&answered, "state").as_deref(),
            Some(MODIFY_IN_PROGRESS),
            "case {name:?}: a modification is answered in the state it STARTS, got {answered}"
        );
        assert_eq!(
            element(&answered, "version"),
            Some(FIRST_PREFIX_LIST_VERSION.to_string()),
            "case {name:?}: the call answers the version the list STANDS at, got {answered}"
        );
        let held = reported_blocks(&router, name, &prefix_list_id).await;
        assert_eq!(
            held,
            vec!["10.70.0.0/16".to_string(), "10.71.0.0/16".to_string()],
            "case {name:?}: the entry set in force is still the one the create stored, got \
             {held:?}"
        );

        let settled = poll_prefix_list_state(&router, name, &prefix_list_id, MODIFY_COMPLETE).await;
        assert_eq!(
            settled, MODIFY_COMPLETE,
            "case {name:?}: the CLOCK must settle the modification, got {settled:?}"
        );
        let expected_version = if case.revises_entries {
            FIRST_PREFIX_LIST_VERSION + 1
        } else {
            FIRST_PREFIX_LIST_VERSION
        };
        let version = reported_version(&router, name, &prefix_list_id).await;
        assert_eq!(
            version,
            expected_version.to_string(),
            "case {name:?}: expected version {expected_version} once the modification was \
             carried out, got {version:?}"
        );
        let blocks = reported_blocks(&router, name, &prefix_list_id).await;
        assert_eq!(
            blocks,
            case.settled_blocks
                .iter()
                .map(|block| (*block).to_string())
                .collect::<Vec<_>>(),
            "case {name:?}: expected the entry set the modification left, got {blocks:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Rule 4: a snapshot measures the blocks its source held
// ---------------------------------------------------------------------------

/// The `fullSnapshotSizeInBytes` `DescribeSnapshots` reports for one snapshot.
async fn reported_full_size(
    router: &MultiServiceRouter,
    case: &str,
    snapshot_id: &str,
) -> Option<String> {
    let body = ok(
        router,
        case,
        "DescribeSnapshots",
        &[param("SnapshotId.1", snapshot_id)],
    )
    .await;
    let doc = document(case, &body);
    set_items(doc.root_element(), "snapshotSet")
        .into_iter()
        .find_map(|item| child_text(item, "fullSnapshotSizeInBytes"))
}

/// Rule 4: a snapshot of a volume created empty measures none of it, whatever its size, and a
/// copy of that snapshot measures exactly what the snapshot it copies measures.
#[tokio::test]
async fn a_snapshot_measures_the_blocks_its_source_held() {
    let (router, _engine) = super::ec2_query::emulator();

    // The rows span the sizes a volume can be created at: nothing was ever written to any of
    // them, so what they assert is that the volume's SIZE is not what the member measures.
    for size in [1_i64, 8, 500] {
        let case = format!("a {size} GiB source volume");
        let (_, snapshot_id) = volume_and_snapshot(&router, &case, size).await;
        let full_size = reported_full_size(&router, &case, &snapshot_id).await;
        assert_eq!(
            full_size.as_deref(),
            Some("0"),
            "case {case:?}: no block was ever written to the source volume, so the snapshot \
             measures none, got {full_size:?}"
        );

        let copied = ok(
            &router,
            &case,
            "CopySnapshot",
            &[
                param("SourceSnapshotId", &snapshot_id),
                param("SourceRegion", REGION),
            ],
        )
        .await;
        let copy_id = element(&copied, "snapshotId")
            .unwrap_or_else(|| panic!("case {case:?}: CopySnapshot reports the copy it minted"));
        let copy_size = reported_full_size(&router, &case, &copy_id).await;
        assert_eq!(
            copy_size, full_size,
            "case {case:?}: a copy holds every block the source snapshot holds, so it \
             measures the same, got {copy_size:?}"
        );
    }
}
