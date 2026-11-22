// SPDX-License-Identifier: CC0-1.0

//! Which prefix list VERSION an IPAM prefix list resolver target synchronizes, and what
//! `ModifyIpamPrefixListResolverTarget` changes about it — driven over the wire the service
//! actually speaks: `ec2Query` — an `application/x-www-form-urlencoded` POST whose body carries
//! `Action=…`, answered with XML (README.md / OPERATIONS.md "Wire protocol").
//!
//! The rules under test:
//!
//! * a target that has been CREATED has synchronized a version of the prefix list it names, and
//!   reports it as "the version of the prefix list that was last successfully synchronized by
//!   this target" (`IpamPrefixListResolverTarget$LastSyncedVersion`) — the version the list
//!   stands at for a target that tracks the list's latest version, and the version the target is
//!   pinned to otherwise; a version the list does not have is one nothing could synchronize, so
//!   such a target reports none;
//! * `ModifyIpamPrefixListResolverTarget` applies the members the request names to the stored
//!   target, leaves every other one as it stands, answers the target in the non-terminal
//!   `modify-in-progress` state, and SYNCHRONIZES at the instant the clock settles it — which
//!   leaves it in `sync-complete`, the state that says the synchronization ran; so pinning the
//!   version a target last synced is what makes it stop following the list, and tracking the
//!   latest again is what makes it follow it once more;
//! * a target id the account holds no target for, and a desired version the prefix list does not
//!   have, are refused and change nothing; a dry run is answered without changing anything.
//!
//! The rows differ along what those rules generalise over: whether the target tracks the latest
//! version, whether it pins one and where that version sits relative to the one the list stands
//! at, and which version the list is at (its first, or one a revision minted). Every resource a
//! row names is created through the operation that owns it — `CreateIpam`,
//! `CreateManagedPrefixList`, `ModifyManagedPrefixList`, `CreateIpamPrefixListResolver` and
//! `CreateIpamPrefixListResolverTarget` — so nothing here is seeded behind the API's back.

use std::time::Duration;

use ec2::state::{
    ADDRESS_FAMILY_IPV4, DRY_RUN_OPERATION, FIRST_PREFIX_LIST_VERSION,
    INVALID_IPAM_PREFIX_LIST_RESOLVER_TARGET_ID_NOT_FOUND, INVALID_PARAMETER_VALUE,
    IPAM_PREFIX_LIST_RESOLVER_TARGET_STATE_CREATE_COMPLETE,
    IPAM_PREFIX_LIST_RESOLVER_TARGET_STATE_CREATE_IN_PROGRESS,
    IPAM_PREFIX_LIST_RESOLVER_TARGET_STATE_MODIFY_IN_PROGRESS,
    IPAM_PREFIX_LIST_RESOLVER_TARGET_STATE_SYNC_COMPLETE, PREFIX_LIST_ADDRESS_FAMILY_IPV4,
    RESOURCE_TYPE_IPAM_PREFIX_LIST_RESOLVER_TARGET,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, error_code, error_message, indexed};
use super::wire::{child, child_text, document, param, Param};

/// The Region every call here is signed for, which is therefore the IPAM's home Region and the
/// Region the prefix lists live in.
const REGION: &str = "us-east-1";

/// The action that creates a target.
const CREATE: &str = "CreateIpamPrefixListResolverTarget";

/// The action under test.
const MODIFY: &str = "ModifyIpamPrefixListResolverTarget";

/// The action that reports a target.
const DESCRIBE: &str = "DescribeIpamPrefixListResolverTargets";

/// How long a wait below may go on for before the test gives up: far above the span the store
/// owes a transition on any host, so a slow host does not fail a claim about the service.
const GENEROUS_DEADLINE: Duration = Duration::from_secs(60);

/// How often those waits look again.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// One call of `action` signed for [`REGION`].
async fn invoke(router: &MultiServiceRouter, action: &str, params: &[Param]) -> (u16, String) {
    call(router, REGION, action, params).await
}

/// One call of `action` requiring success, and the body it answered.
async fn ok(case: &str, router: &MultiServiceRouter, action: &str, params: &[Param]) -> String {
    let (status, body) = invoke(router, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// Create one IPAM through the operation that owns it, and answer the `IpamId` it minted.
async fn create_ipam(case: &str, router: &MultiServiceRouter) -> String {
    let body = ok(case, router, "CreateIpam", &[]).await;
    element(&body, "ipamId")
        .unwrap_or_else(|| panic!("case {case:?}: no <ipamId> on the created IPAM in {body}"))
}

/// Create one IPAM prefix list resolver in `ipam_id`, and answer its id.
async fn create_resolver(case: &str, router: &MultiServiceRouter, ipam_id: &str) -> String {
    let body = ok(
        case,
        router,
        "CreateIpamPrefixListResolver",
        &[
            param("IpamId", ipam_id),
            param("AddressFamily", ADDRESS_FAMILY_IPV4),
            param("Description", "the resolver the targets belong to"),
        ],
    )
    .await;
    element(&body, "ipamPrefixListResolverId")
        .unwrap_or_else(|| panic!("case {case:?}: no <ipamPrefixListResolverId> in {body}"))
}

/// Create one customer-managed prefix list carrying one entry, and answer the `PrefixListId` it
/// minted. A list a create just minted is at [`FIRST_PREFIX_LIST_VERSION`].
async fn create_prefix_list(case: &str, router: &MultiServiceRouter, name: &str) -> String {
    let body = ok(
        case,
        router,
        "CreateManagedPrefixList",
        &[
            param("PrefixListName", name),
            param("AddressFamily", PREFIX_LIST_ADDRESS_FAMILY_IPV4),
            param("MaxEntries", "5"),
            param("Entry.1.Cidr", "10.0.0.0/24"),
        ],
    )
    .await;
    element(&body, "prefixListId")
        .unwrap_or_else(|| panic!("case {case:?}: no <prefixListId> in {body}"))
}

/// Revise `prefix_list_id`'s entry set, which mints the list's NEXT version — the only way a
/// list comes to stand at a version above its first.
async fn revise_prefix_list(case: &str, router: &MultiServiceRouter, prefix_list_id: &str) {
    let _ = ok(
        case,
        router,
        "ModifyManagedPrefixList",
        &[
            param("PrefixListId", prefix_list_id),
            param("AddEntry.1.Cidr", "10.0.1.0/24"),
        ],
    )
    .await;
}

/// The state and the version `DescribeManagedPrefixLists` reports for `prefix_list_id` now.
async fn prefix_list_now(
    case: &str,
    router: &MultiServiceRouter,
    prefix_list_id: &str,
) -> (String, i64) {
    let body = ok(
        case,
        router,
        "DescribeManagedPrefixLists",
        &indexed("PrefixListId", &[prefix_list_id]),
    )
    .await;
    let state = element(&body, "state")
        .unwrap_or_else(|| panic!("case {case:?}: no <state> in {body}"));
    let version = element(&body, "version")
        .unwrap_or_else(|| panic!("case {case:?}: no <version> in {body}"))
        .parse()
        .unwrap_or_else(|_| panic!("case {case:?}: the version must be a number: {body}"));
    (state, version)
}

/// Wait until `prefix_list_id` is in a state a revision can start from and stands at a version
/// of at least `version`, answering the version it stands at.
///
/// The wait is for that CONDITION — looking again until it holds or [`GENEROUS_DEADLINE`]
/// passes — rather than for a span to go by, so what the test then asserts does not depend on
/// how fast this host runs.
async fn until_the_prefix_list_stands_at(
    case: &str,
    router: &MultiServiceRouter,
    prefix_list_id: &str,
    version: i64,
) -> i64 {
    let deadline = std::time::Instant::now() + GENEROUS_DEADLINE;
    loop {
        let (state, standing) = prefix_list_now(case, router, prefix_list_id).await;
        if state.ends_with(SETTLED_STATE_SUFFIX) && standing >= version {
            return standing;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: the prefix list {prefix_list_id} never settled at version \
             {version} or above; it is {state:?} at version {standing}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The suffix every state a prefix list or a resolver target SETTLES in ends with, the
/// non-terminal ones all ending in `-in-progress`
/// (`smithy_query(query="shape",shape="IpamPrefixListResolverTargetState")`,
/// `smithy_query(query="shape",shape="PrefixListState")`).
const SETTLED_STATE_SUFFIX: &str = "-complete";

/// Wait until the target `target_id` names reports `wanted`, answering it as the describe then
/// reports it. A CONDITION wait, for the reason [`until_the_prefix_list_stands_at`] is one.
async fn until_the_target_reports(
    case: &str,
    router: &MultiServiceRouter,
    target_id: &str,
    wanted: &str,
) -> ReportedTarget {
    let deadline = std::time::Instant::now() + GENEROUS_DEADLINE;
    loop {
        let reported = describe_one(case, router, target_id).await;
        if reported.state.as_deref() == Some(wanted) {
            return reported;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: the target {target_id} never reached {wanted}; got {reported:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The members of one reported target this suite asserts on.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ReportedTarget {
    state: Option<String>,
    desired_version: Option<String>,
    last_synced_version: Option<String>,
    track_latest_version: Option<String>,
}

/// The one target element of a body, as [`ReportedTarget`].
fn read_target(case: &str, body: &str) -> ReportedTarget {
    let parsed = document(case, body);
    let node = parsed
        .descendants()
        .find(|node| node.is_element() && child(*node, "ipamPrefixListResolverTargetId").is_some())
        .unwrap_or_else(|| panic!("case {case:?}: no target in {body}"));
    ReportedTarget {
        state: child_text(node, "state"),
        desired_version: child_text(node, "desiredVersion"),
        last_synced_version: child_text(node, "lastSyncedVersion"),
        track_latest_version: child_text(node, "trackLatestVersion"),
    }
}

/// The target `target_id` names as the describe reports it now.
async fn describe_one(case: &str, router: &MultiServiceRouter, target_id: &str) -> ReportedTarget {
    let body = ok(
        case,
        router,
        DESCRIBE,
        &indexed("IpamPrefixListResolverTargetId", &[target_id]),
    )
    .await;
    read_target(case, &body)
}

/// Create one target against `prefix_list_id` and answer the id it minted.
async fn create_target(
    case: &str,
    router: &MultiServiceRouter,
    resolver_id: &str,
    prefix_list_id: &str,
    tracking: bool,
    desired_version: Option<i64>,
) -> String {
    let mut params = vec![
        param("IpamPrefixListResolverId", resolver_id),
        param("PrefixListId", prefix_list_id),
        param("PrefixListRegion", REGION),
        param("TrackLatestVersion", if tracking { "true" } else { "false" }),
    ];
    if let Some(version) = desired_version {
        params.push(param("DesiredVersion", &version.to_string()));
    }
    let body = ok(case, router, CREATE, &params).await;
    let created = read_target(case, &body);
    assert_eq!(
        created.state.as_deref(),
        Some(IPAM_PREFIX_LIST_RESOLVER_TARGET_STATE_CREATE_IN_PROGRESS),
        "case {case:?}: the create answers the target while it is still being created, \
         got {created:?}"
    );
    assert_eq!(
        created.last_synced_version, None,
        "case {case:?}: nothing has synchronized a version before the creation settles, \
         got {created:?}"
    );
    element(&body, "ipamPrefixListResolverTargetId")
        .unwrap_or_else(|| panic!("case {case:?}: no target id in {body}"))
}

// ---------------------------------------------------------------------------
// Which version a created target synchronizes
// ---------------------------------------------------------------------------

/// One row of the synchronization table: the case, whether the list is revised before the target
/// is created, whether the target tracks the latest version, the version it pins, and the version
/// it must report as synchronized — as an offset from the list's own version where it follows the
/// list, or an absolute version where it pins one.
struct SyncCase {
    case: &'static str,
    revise: bool,
    tracking: bool,
    desired_version: Option<i64>,
    /// `Some(version)` is the version the target must report; `None` is "the version the list
    /// stands at", which the row computes from the list it was given.
    expected_version: Option<i64>,
}

/// The rule: a created target has synchronized a version of its prefix list by the time its
/// creation settles — the one the list stands at where it follows the list, the one it pins
/// otherwise, and none at all where it pins a version the list does not have.
#[tokio::test]
async fn a_created_target_reports_the_prefix_list_version_it_synchronized() {
    let rows: Vec<SyncCase> = vec![
        SyncCase {
            case: "tracking the latest version of a list at its first version",
            revise: false,
            tracking: true,
            desired_version: None,
            expected_version: None,
        },
        SyncCase {
            case: "tracking the latest version of a list a revision has moved on",
            revise: true,
            tracking: true,
            desired_version: None,
            expected_version: None,
        },
        SyncCase {
            case: "pinned to the first version of a list a revision has moved on",
            revise: true,
            tracking: false,
            desired_version: Some(FIRST_PREFIX_LIST_VERSION),
            expected_version: Some(FIRST_PREFIX_LIST_VERSION),
        },
        SyncCase {
            case: "neither tracking the latest version nor pinning one",
            revise: true,
            tracking: false,
            desired_version: None,
            expected_version: None,
        },
        SyncCase {
            case: "tracking the latest version while also pinning the first",
            revise: true,
            tracking: true,
            desired_version: Some(FIRST_PREFIX_LIST_VERSION),
            expected_version: None,
        },
        SyncCase {
            case: "pinned above the version the list stands at",
            revise: false,
            tracking: false,
            desired_version: Some(FIRST_PREFIX_LIST_VERSION + 41),
            expected_version: Some(0),
        },
    ];

    let (router, _engine) = super::ec2_query::emulator();
    let ipam = create_ipam("the IPAM the resolver belongs to", &router).await;
    let resolver = create_resolver("the resolver", &router, &ipam).await;

    // Every row's own prefix list, so no row's revision moves another row's list.
    let mut lists: Vec<String> = Vec::with_capacity(rows.len());
    for (nth, row) in rows.iter().enumerate() {
        lists.push(create_prefix_list(row.case, &router, &format!("synced-{nth}")).await);
    }
    // Each list has to have SETTLED before a revision can start from it, and the revision has
    // to have minted the next version before a target is created against it — both waited for
    // as conditions, never as a span.
    for (row, prefix_list) in rows.iter().zip(&lists) {
        until_the_prefix_list_stands_at(row.case, &router, prefix_list, FIRST_PREFIX_LIST_VERSION)
            .await;
        if row.revise {
            revise_prefix_list(row.case, &router, prefix_list).await;
        }
    }

    let mut versions: Vec<i64> = Vec::with_capacity(rows.len());
    let mut targets: Vec<String> = Vec::with_capacity(rows.len());
    for (row, prefix_list) in rows.iter().zip(&lists) {
        let wanted = if row.revise {
            FIRST_PREFIX_LIST_VERSION + 1
        } else {
            FIRST_PREFIX_LIST_VERSION
        };
        versions
            .push(until_the_prefix_list_stands_at(row.case, &router, prefix_list, wanted).await);
        targets.push(
            create_target(
                row.case,
                &router,
                &resolver,
                prefix_list,
                row.tracking,
                row.desired_version,
            )
            .await,
        );
    }

    for ((row, target_id), list_version) in rows.iter().zip(&targets).zip(&versions) {
        let case = row.case;
        // The target's creation settling is what performs the synchronization, so the wait is
        // for that state and the assertions below are about what it left behind.
        let reported = until_the_target_reports(
            case,
            &router,
            target_id,
            IPAM_PREFIX_LIST_RESOLVER_TARGET_STATE_CREATE_COMPLETE,
        )
        .await;
        // A row that pins a version the list does not have has nothing synchronized at all,
        // which the table spells as the impossible version 0.
        let expected = match row.expected_version {
            Some(0) => None,
            Some(version) => Some(version.to_string()),
            None => Some(list_version.to_string()),
        };
        assert_eq!(
            reported.last_synced_version, expected,
            "case {case:?}: expected the synced version {expected:?} of a list at version \
             {list_version}, got {reported:?}"
        );
        assert_eq!(
            reported.track_latest_version.as_deref(),
            Some(if row.tracking { "true" } else { "false" }),
            "case {case:?}: the target reports the tracking mode it was created with, \
             got {reported:?}"
        );
        assert_eq!(
            reported.desired_version,
            row.desired_version.map(|version| version.to_string()),
            "case {case:?}: the target reports the version it was created pinned to, \
             got {reported:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// What the modify changes
// ---------------------------------------------------------------------------

/// The rule: the modify applies what the request names, leaves the rest, answers the
/// non-terminal `modify-in-progress` state, and settles into `sync-complete` having
/// synchronized the version it was left configured for.
#[tokio::test]
async fn the_modify_pins_a_version_and_releases_it_again() {
    let (router, _engine) = super::ec2_query::emulator();
    let case = "a target tracking a list a revision has moved on";
    let ipam = create_ipam(case, &router).await;
    let resolver = create_resolver(case, &router, &ipam).await;
    let prefix_list = create_prefix_list(case, &router, "modified").await;
    // The list has to have settled before a revision can start from it, and the revision has to
    // have minted the next version before the target is created — both waited for as conditions.
    until_the_prefix_list_stands_at(case, &router, &prefix_list, FIRST_PREFIX_LIST_VERSION).await;
    revise_prefix_list(case, &router, &prefix_list).await;
    let latest = until_the_prefix_list_stands_at(
        case,
        &router,
        &prefix_list,
        FIRST_PREFIX_LIST_VERSION + 1,
    )
    .await;
    assert!(
        latest > FIRST_PREFIX_LIST_VERSION,
        "case {case:?}: a revised list stands at a version above its first, got {latest}"
    );
    let target = create_target(case, &router, &resolver, &prefix_list, true, None).await;
    let tracking = until_the_target_reports(
        case,
        &router,
        &target,
        IPAM_PREFIX_LIST_RESOLVER_TARGET_STATE_CREATE_COMPLETE,
    )
    .await;
    assert_eq!(
        tracking.last_synced_version,
        Some(latest.to_string()),
        "case {case:?}: a tracking target synchronized the version the list stands at, \
         got {tracking:?}"
    );

    // Pinning the list's FIRST version, which is the one the client wants the resolver to keep
    // publishing: the ANSWER is the target in the non-terminal state, still reporting the
    // version it had synchronized before this call.
    let case = "pinning the first version";
    let body = ok(
        case,
        &router,
        MODIFY,
        &[
            param("IpamPrefixListResolverTargetId", &target),
            param("DesiredVersion", &FIRST_PREFIX_LIST_VERSION.to_string()),
            param("TrackLatestVersion", "false"),
        ],
    )
    .await;
    let answered = read_target(case, &body);
    assert_eq!(
        answered.state.as_deref(),
        Some(IPAM_PREFIX_LIST_RESOLVER_TARGET_STATE_MODIFY_IN_PROGRESS),
        "case {case:?}: the modify answers the non-terminal state, got {answered:?}"
    );
    assert_eq!(
        answered.desired_version.as_deref(),
        Some(FIRST_PREFIX_LIST_VERSION.to_string().as_str()),
        "case {case:?}: the answered target carries the version the request pinned, \
         got {answered:?}"
    );
    assert_eq!(
        answered.track_latest_version.as_deref(),
        Some("false"),
        "case {case:?}: the answered target no longer tracks the latest version, got {answered:?}"
    );
    assert_eq!(
        answered.last_synced_version,
        Some(latest.to_string()),
        "case {case:?}: the version synchronized before this call is still the last one \
         synchronized, got {answered:?}"
    );

    let case = "the pinned target once the clock has settled it";
    let settled = until_the_target_reports(
        case,
        &router,
        &target,
        IPAM_PREFIX_LIST_RESOLVER_TARGET_STATE_SYNC_COMPLETE,
    )
    .await;
    assert_eq!(
        settled.last_synced_version,
        Some(FIRST_PREFIX_LIST_VERSION.to_string()),
        "case {case:?}: a settled modification synchronized the version it pinned, got {settled:?}"
    );

    // Tracking the latest version again, naming nothing else: the version pinned before is left
    // exactly as it stands, and the target follows the list once more.
    let case = "tracking the latest version again";
    let body = ok(
        case,
        &router,
        MODIFY,
        &[
            param("IpamPrefixListResolverTargetId", &target),
            param("TrackLatestVersion", "true"),
        ],
    )
    .await;
    let answered = read_target(case, &body);
    assert_eq!(
        answered.desired_version.as_deref(),
        Some(FIRST_PREFIX_LIST_VERSION.to_string().as_str()),
        "case {case:?}: a member the request does not name is left as it stands, got {answered:?}"
    );
    let settled = until_the_target_reports(
        case,
        &router,
        &target,
        IPAM_PREFIX_LIST_RESOLVER_TARGET_STATE_SYNC_COMPLETE,
    )
    .await;
    assert_eq!(
        settled.last_synced_version,
        Some(latest.to_string()),
        "case {case:?}: a target that tracks the latest version again synchronized the version \
         the list stands at, got {settled:?}"
    );
    assert_eq!(
        settled.track_latest_version.as_deref(),
        Some("true"),
        "case {case:?}: the target tracks the latest version, got {settled:?}"
    );
}

// ---------------------------------------------------------------------------
// What the modify refuses
// ---------------------------------------------------------------------------

/// One row of the refusal table: the case, the fields the request carries BESIDE the target id,
/// the id it names in place of the stored one, the HTTP status and the error code.
struct RefusalCase {
    case: &'static str,
    /// `None` names the target the suite created; `Some(id)` names that id instead.
    target_id: Option<&'static str>,
    extra: Vec<(&'static str, &'static str)>,
    status: u16,
    code: &'static str,
    /// The message the refusal must carry, where the rule fixes one.
    message: Option<String>,
}

/// The rule: a request the modify refuses changes nothing about the target it named, and a dry
/// run is answered without changing anything either.
#[tokio::test]
async fn the_modify_refuses_an_unknown_target_and_an_impossible_version() {
    let (router, _engine) = super::ec2_query::emulator();
    let case = "the target the refusals name";
    let ipam = create_ipam(case, &router).await;
    let resolver = create_resolver(case, &router, &ipam).await;
    let prefix_list = create_prefix_list(case, &router, "refused").await;
    let target = create_target(case, &router, &resolver, &prefix_list, true, None).await;
    let before = until_the_target_reports(
        case,
        &router,
        &target,
        IPAM_PREFIX_LIST_RESOLVER_TARGET_STATE_CREATE_COMPLETE,
    )
    .await;

    let unissued = "ipam-plrt-00000000000000000";
    let rows: Vec<RefusalCase> = vec![
        RefusalCase {
            case: "a target id the account holds no target for",
            target_id: Some(unissued),
            extra: vec![("TrackLatestVersion", "false")],
            status: 400,
            code: INVALID_IPAM_PREFIX_LIST_RESOLVER_TARGET_ID_NOT_FOUND,
            // EC2's one sentence for an id that names nothing: the resource's modelled type and
            // the id the REQUEST named.
            message: Some(format!(
                "The {RESOURCE_TYPE_IPAM_PREFIX_LIST_RESOLVER_TARGET} ID '{unissued}' does not \
                 exist"
            )),
        },
        RefusalCase {
            case: "a desired version above the one the list stands at",
            target_id: None,
            extra: vec![("DesiredVersion", "99"), ("TrackLatestVersion", "false")],
            status: 400,
            code: INVALID_PARAMETER_VALUE,
            message: None,
        },
        RefusalCase {
            case: "a desired version below the first version a list has",
            target_id: None,
            extra: vec![("DesiredVersion", "0"), ("TrackLatestVersion", "false")],
            status: 400,
            code: INVALID_PARAMETER_VALUE,
            message: None,
        },
        RefusalCase {
            case: "a dry run of a modification that would succeed",
            target_id: None,
            extra: vec![("DryRun", "true"), ("TrackLatestVersion", "false")],
            status: 412,
            code: DRY_RUN_OPERATION,
            message: None,
        },
        RefusalCase {
            case: "a dry run naming a target the account does not hold, answered on permission \
                   rather than on the resource",
            target_id: Some(unissued),
            extra: vec![("DryRun", "true")],
            status: 412,
            code: DRY_RUN_OPERATION,
            message: None,
        },
    ];

    for row in &rows {
        let case = row.case;
        let mut params = vec![param(
            "IpamPrefixListResolverTargetId",
            row.target_id.unwrap_or(target.as_str()),
        )];
        for (name, value) in &row.extra {
            params.push(param(name, value));
        }
        let (status, body) = invoke(&router, MODIFY, &params).await;
        assert_eq!(
            status, row.status,
            "case {case:?}: expected HTTP {expected}, got {status} with body {body}",
            expected = row.status
        );
        let code = error_code(&body);
        assert_eq!(
            code, row.code,
            "case {case:?}: expected the code {expected}, got {code} with body {body}",
            expected = row.code
        );
        if let Some(expected) = row.message.as_deref() {
            let actual = error_message(&body);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected the message {expected:?}, got {actual:?}"
            );
        }
        let after = describe_one(case, &router, &target).await;
        assert_eq!(
            after, before,
            "case {case:?}: a refused modification leaves the target exactly as it was"
        );
    }

    // ... and no refusal left a move owed to the clock either. What witnesses that a span has
    // gone by is another resource's OWN owed move settling — a second target created now — so
    // the claim rests on a condition rather than on a wait.
    let case = "the target after every refusal";
    let witness_list = create_prefix_list(case, &router, "witness").await;
    let witness = create_target(case, &router, &resolver, &witness_list, false, None).await;
    until_the_target_reports(
        case,
        &router,
        &witness,
        IPAM_PREFIX_LIST_RESOLVER_TARGET_STATE_CREATE_COMPLETE,
    )
    .await;
    assert_eq!(
        describe_one(case, &router, &target).await,
        before,
        "case {case:?}: nothing the refusals named was owed to the clock"
    );
}
