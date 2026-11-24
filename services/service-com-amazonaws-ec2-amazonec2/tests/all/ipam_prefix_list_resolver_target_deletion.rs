// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `DeleteIpamPrefixListResolverTarget`, driven over the wire the service
//! actually speaks: `ec2Query` — an `application/x-www-form-urlencoded` POST whose body carries
//! `Action=…`, answered with XML (README.md / OPERATIONS.md "Wire protocol").
//!
//! The rules under test:
//!
//! * deleting a resolver target WRITES that deletion against exactly the target the request
//!   names: it is answered, and read back by `DescribeIpamPrefixListResolverTargets`, in the
//!   transient `delete-in-progress` state with every other member of the stored target
//!   unchanged, and the CLOCK — not a later call — moves it on to `delete-complete`;
//! * "this removes the association between the resolver and the managed prefix list"
//!   (`DeleteIpamPrefixListResolverTarget`): the prefix list the deleted target named is free
//!   again, so it can be made the target of a resolver a second time, where a list that still
//!   has one is refused;
//! * "before deleting a resolver, you must first delete all resolver targets associated with
//!   it" (`DeleteIpamPrefixListResolver`): the resolver whose targets have been deleted IS then
//!   deleted, where the same call while a target is still held is refused — the whole teardown
//!   a client performs;
//! * an id the account holds no target for is refused with this resource's own `NotFound` code,
//!   and a target whose deletion has already been asked for with `IncorrectState` naming the
//!   state it is in — neither writes anything;
//! * `DryRun` answers the permission question with `DryRunOperation` and writes nothing.
//!
//! Each table row is a distinct input along what those rules generalise over: which prefix list
//! the target synchronizes, whether it tracks the latest version or is pinned to one, how many
//! tags it kept, and which id the request names and in what way it does not resolve.

use std::collections::BTreeSet;
use std::time::Duration;

use ec2::state::{
    ADDRESS_FAMILY_IPV4, DEPENDENCY_VIOLATION, DRY_RUN_OPERATION, INCORRECT_STATE,
    INVALID_IPAM_PREFIX_LIST_RESOLVER_TARGET_ID_NOT_FOUND,
    IPAM_PREFIX_LIST_RESOLVER_STATE_DELETE_IN_PROGRESS,
    IPAM_PREFIX_LIST_RESOLVER_TARGET_STATE_DELETE_COMPLETE,
    IPAM_PREFIX_LIST_RESOLVER_TARGET_STATE_DELETE_IN_PROGRESS, PREFIX_LIST_ADDRESS_FAMILY_IPV4,
    RESOURCE_ALREADY_ASSOCIATED, RESOURCE_TYPE_IPAM_PREFIX_LIST_RESOLVER_TARGET, TRANSITION_DELAY,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, elements, error_code, error_message, indexed};
use super::wire::{Param, param};

/// The Region every call here is signed for, which is therefore each IPAM's home Region.
const REGION: &str = "us-east-1";

/// The action under test.
const DELETE_TARGET: &str = "DeleteIpamPrefixListResolverTarget";

/// The action that creates the targets it deletes.
const CREATE_TARGET: &str = "CreateIpamPrefixListResolverTarget";

/// The action that reads them back.
const DESCRIBE_TARGETS: &str = "DescribeIpamPrefixListResolverTargets";

/// The resolver a target belongs to, and the delete that can only follow its targets'.
const CREATE_RESOLVER: &str = "CreateIpamPrefixListResolver";
const DELETE_RESOLVER: &str = "DeleteIpamPrefixListResolver";

/// The HTTP status EC2 answers `DryRunOperation` with.
const DRY_RUN_STATUS: u16 = 412;

/// An `IpamPrefixListResolverTargetId` the account holds no target for, spelled from the top of
/// the hexadecimal range: this store mints ids from a monotonic counter, so an id this high
/// cannot collide with one a created target was given.
const UNKNOWN_TARGET_ID: &str = "ipam-plrt-0ffffffffffffffff";

/// Longer than the span the clock owes a `delete-in-progress` target, so a read after this much
/// has certainly passed the due instant.
fn past_the_transition() -> Duration {
    TRANSITION_DELAY + Duration::from_millis(250)
}

fn emulator() -> (MultiServiceRouter, std::sync::Arc<engine::InMemoryEngine>) {
    super::ec2_query::emulator()
}

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

/// Create one IPAM through the operation that owns it and answer the `IpamId` it minted.
async fn create_ipam(case: &str, router: &MultiServiceRouter) -> String {
    let body = ok(case, router, "CreateIpam", &[]).await;
    element(&body, "ipamId")
        .unwrap_or_else(|| panic!("case {case:?}: no <ipamId> on the created IPAM in {body}"))
}

/// Create one IPAM prefix list resolver in the IPAM `ipam_id` names and answer its id.
async fn create_resolver(case: &str, router: &MultiServiceRouter, ipam_id: &str) -> String {
    let body = ok(
        case,
        router,
        CREATE_RESOLVER,
        &[
            param("IpamId", ipam_id),
            param("AddressFamily", ADDRESS_FAMILY_IPV4),
            param("Description", case),
        ],
    )
    .await;
    element(&body, "ipamPrefixListResolverId")
        .unwrap_or_else(|| panic!("case {case:?}: no <ipamPrefixListResolverId> in {body}"))
}

/// Create one customer-managed prefix list and answer the `PrefixListId` it minted.
async fn create_prefix_list(case: &str, router: &MultiServiceRouter, name: &str) -> String {
    let body = ok(
        case,
        router,
        "CreateManagedPrefixList",
        &[
            param("PrefixListName", name),
            param("AddressFamily", PREFIX_LIST_ADDRESS_FAMILY_IPV4),
            param("MaxEntries", "5"),
        ],
    )
    .await;
    element(&body, "prefixListId")
        .unwrap_or_else(|| panic!("case {case:?}: no <prefixListId> in {body}"))
}

/// What one row of the table creates: a target of its own configuration, over a prefix list of
/// its own.
struct TargetCase {
    case: &'static str,
    /// "Indicates whether the resolver target should automatically track the latest version of
    /// the prefix list" (`CreateIpamPrefixListResolverTargetRequest$TrackLatestVersion`).
    track_latest_version: bool,
    /// "The specific version of the prefix list to target"
    /// (`$DesiredVersion`), or `None` where the request pins none.
    desired_version: Option<&'static str>,
    /// The tags the create kept on the target.
    tags: &'static [(&'static str, &'static str)],
}

/// The rows the suite drives: the configurations a target can be created in.
fn target_cases() -> Vec<TargetCase> {
    vec![
        TargetCase {
            case: "a target that tracks the latest version",
            track_latest_version: true,
            desired_version: None,
            tags: &[],
        },
        TargetCase {
            case: "a target pinned to the version its list stands at, with one tag",
            track_latest_version: false,
            desired_version: Some("1"),
            tags: &[("Name", "pinned")],
        },
        TargetCase {
            case: "a target that tracks the latest version and keeps two tags",
            track_latest_version: true,
            desired_version: None,
            tags: &[("Name", "tracking"), ("Owner", "networking")],
        },
    ]
}

/// The form fields for one `ipam-prefix-list-resolver-target` tag specification over `tags`.
fn tag_params(tags: &[(&str, &str)]) -> Vec<Param> {
    if tags.is_empty() {
        return Vec::new();
    }
    let mut params = vec![param(
        "TagSpecification.1.ResourceType",
        RESOURCE_TYPE_IPAM_PREFIX_LIST_RESOLVER_TARGET,
    )];
    for (index, (key, value)) in tags.iter().enumerate() {
        params.push(param(
            &format!("TagSpecification.1.Tag.{}.Key", index + 1),
            key,
        ));
        params.push(param(
            &format!("TagSpecification.1.Tag.{}.Value", index + 1),
            value,
        ));
    }
    params
}

/// Create the target one row describes and answer the body the create reported, together with
/// the target's id and the prefix list it synchronizes.
async fn create_target(
    router: &MultiServiceRouter,
    resolver_id: &str,
    prefix_list_id: &str,
    row: &TargetCase,
) -> (String, String) {
    let mut params = vec![
        param("IpamPrefixListResolverId", resolver_id),
        param("PrefixListId", prefix_list_id),
        param("PrefixListRegion", REGION),
        param(
            "TrackLatestVersion",
            if row.track_latest_version {
                "true"
            } else {
                "false"
            },
        ),
    ];
    if let Some(desired_version) = row.desired_version {
        params.push(param("DesiredVersion", desired_version));
    }
    params.extend(tag_params(row.tags));
    let body = ok(row.case, router, CREATE_TARGET, &params).await;
    let target_id = element(&body, "ipamPrefixListResolverTargetId")
        .unwrap_or_else(|| panic!("case {case:?}: no target id in {body}", case = row.case));
    (target_id, body)
}

/// The members a delete must answer unchanged from the stored target, read off a response body.
fn carried_members(body: &str) -> Vec<(&'static str, Option<String>)> {
    [
        "ipamPrefixListResolverTargetId",
        "ipamPrefixListResolverTargetArn",
        "ipamPrefixListResolverId",
        "ownerId",
        "prefixListId",
        "prefixListRegion",
        "desiredVersion",
        "trackLatestVersion",
    ]
    .into_iter()
    .map(|member| (member, element(body, member)))
    .collect()
}

/// The state the describe reports for `target_id`.
async fn described_state(case: &str, router: &MultiServiceRouter, target_id: &str) -> String {
    let body = ok(
        case,
        router,
        DESCRIBE_TARGETS,
        &indexed("IpamPrefixListResolverTargetId", &[target_id]),
    )
    .await;
    element(&body, "state").unwrap_or_else(|| {
        panic!("case {case:?}: the describe reported no state for {target_id} in {body}")
    })
}

/// Every target id the store holds — what proves a refusal wrote nothing.
fn stored_target_ids(engine: &engine::InMemoryEngine) -> BTreeSet<String> {
    let state = ec2::state::service_state(engine).expect("the EC2 store");
    let store = state.lock();
    store
        .ipam_prefix_list_resolver_targets
        .keys()
        .cloned()
        .collect()
}

/// The states the store holds for its targets, by id — what proves a refusal moved nothing.
fn stored_states(engine: &engine::InMemoryEngine) -> Vec<(String, Option<String>)> {
    let state = ec2::state::service_state(engine).expect("the EC2 store");
    let store = state.lock();
    store
        .ipam_prefix_list_resolver_targets
        .iter()
        .map(|(id, target)| (id.clone(), target.state.clone()))
        .collect()
}

// ---------------------------------------------------------------------------
// The deletion itself: answered as stored, read back, and settled by the clock
// ---------------------------------------------------------------------------

/// Each configuration a target can hold is deleted the same way: the call answers the stored
/// target in `delete-in-progress` with every other member unchanged, the describe reports that
/// same state, a second delete is refused for the state it is in, and the clock — nothing else —
/// moves it to `delete-complete`.
#[tokio::test]
async fn deleting_a_target_answers_the_stored_target_in_delete_in_progress() {
    let (router, engine) = emulator();
    let ipam_id = create_ipam("the IPAM the resolver belongs to", &router).await;
    let resolver_id = create_resolver("the resolver the targets belong to", &router, &ipam_id).await;

    let rows = target_cases();
    let mut deleted: Vec<String> = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let prefix_list_id =
            create_prefix_list(row.case, &router, &format!("deleted-target-{index}")).await;
        let (target_id, created) =
            create_target(&router, &resolver_id, &prefix_list_id, row).await;

        let body = ok(row.case, &router, DELETE_TARGET, &[
            param("IpamPrefixListResolverTargetId", &target_id),
        ])
        .await;

        let state = element(&body, "state");
        assert_eq!(
            state.as_deref(),
            Some(IPAM_PREFIX_LIST_RESOLVER_TARGET_STATE_DELETE_IN_PROGRESS),
            "case {case:?}: expected the delete to answer \
             {IPAM_PREFIX_LIST_RESOLVER_TARGET_STATE_DELETE_IN_PROGRESS}, got {state:?} from \
             {body}",
            case = row.case,
        );
        for (member, created_value) in carried_members(&created) {
            let answered = element(&body, member);
            assert_eq!(
                answered, created_value,
                "case {case:?}: expected the delete to answer {member} {created_value:?}, got \
                 {answered:?}",
                case = row.case,
            );
        }

        // The round trip: the describe reports the deletion this call wrote.
        let reported = described_state(row.case, &router, &target_id).await;
        assert_eq!(
            reported, IPAM_PREFIX_LIST_RESOLVER_TARGET_STATE_DELETE_IN_PROGRESS,
            "case {case:?}: expected the describe to report the delete under way, got \
             {reported:?}",
            case = row.case,
        );

        // A second delete has nothing left to delete: it is refused for the state the target
        // is in, and moves nothing.
        let before = stored_states(&engine);
        let (status, second) = invoke(&router, DELETE_TARGET, &[
            param("IpamPrefixListResolverTargetId", &target_id),
        ])
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 from the second delete, got {status} with body \
             {second}",
            case = row.case,
        );
        assert_eq!(
            error_code(&second),
            INCORRECT_STATE,
            "case {case:?}: expected {INCORRECT_STATE}, got {second}",
            case = row.case,
        );
        assert!(
            error_message(&second)
                .contains(IPAM_PREFIX_LIST_RESOLVER_TARGET_STATE_DELETE_IN_PROGRESS),
            "case {case:?}: the refusal must name the state the target is in, got {:?}",
            error_message(&second),
            case = row.case,
        );
        assert_eq!(
            stored_states(&engine),
            before,
            "case {case:?}: a refused delete must move nothing",
            case = row.case,
        );
        deleted.push(target_id);
    }

    // The clock owes each deleted target its `delete-complete` move, with nothing but the
    // clock making it happen: the wait is on the condition, not on a fixed duration.
    let deadline = std::time::Instant::now() + past_the_transition() * 20;
    for target_id in &deleted {
        loop {
            let state = described_state("after the clock's move", &router, target_id).await;
            if state == IPAM_PREFIX_LIST_RESOLVER_TARGET_STATE_DELETE_COMPLETE {
                break;
            }
            assert_eq!(
                state, IPAM_PREFIX_LIST_RESOLVER_TARGET_STATE_DELETE_IN_PROGRESS,
                "case {target_id:?}: a deleted target is one of the two delete states, got \
                 {state:?}"
            );
            assert!(
                std::time::Instant::now() < deadline,
                "case {target_id:?}: expected the clock to move the target to \
                 {IPAM_PREFIX_LIST_RESOLVER_TARGET_STATE_DELETE_COMPLETE}, it stayed {state:?}"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

/// The association IS the target: a prefix list that already has one cannot be made the target
/// of a resolver a second time, and once that target is deleted the same prefix list is free
/// again.
#[tokio::test]
async fn deleting_a_target_frees_the_prefix_list_it_was_the_association_of() {
    let (router, _engine) = emulator();
    let ipam_id = create_ipam("the IPAM", &router).await;
    let resolver_id = create_resolver("the resolver", &router, &ipam_id).await;

    for row in target_cases() {
        let prefix_list_id = create_prefix_list(row.case, &router, "freed-list").await;
        let (target_id, _) = create_target(&router, &resolver_id, &prefix_list_id, &row).await;

        // While the target holds it, the list is already associated.
        let params = vec![
            param("IpamPrefixListResolverId", &resolver_id),
            param("PrefixListId", &prefix_list_id),
            param("PrefixListRegion", REGION),
            param("TrackLatestVersion", "true"),
        ];
        let (status, body) = invoke(&router, CREATE_TARGET, &params).await;
        assert_eq!(
            status, 400,
            "case {case:?}: a list that already has a target must be refused, got {status} \
             with body {body}",
            case = row.case,
        );
        assert_eq!(
            error_code(&body),
            RESOURCE_ALREADY_ASSOCIATED,
            "case {case:?}: expected {RESOURCE_ALREADY_ASSOCIATED}, got {body}",
            case = row.case,
        );

        // Deleting the target removes that association, so the list takes a target again.
        let _ = ok(row.case, &router, DELETE_TARGET, &[
            param("IpamPrefixListResolverTargetId", &target_id),
        ])
        .await;
        let body = ok(row.case, &router, CREATE_TARGET, &params).await;
        let second = element(&body, "ipamPrefixListResolverTargetId")
            .unwrap_or_else(|| panic!("case {case:?}: no target id in {body}", case = row.case));
        assert_ne!(
            second, target_id,
            "case {case:?}: the second association must be a target of its own",
            case = row.case,
        );
        // Taken away again, so the next row starts from a store with no target of this list.
        let _ = ok(row.case, &router, DELETE_TARGET, &[
            param("IpamPrefixListResolverTargetId", &second),
        ])
        .await;
    }
}

// ---------------------------------------------------------------------------
// The teardown the targets stand in the way of
// ---------------------------------------------------------------------------

/// "Before deleting a resolver, you must first delete all resolver targets associated with it":
/// the resolver delete is refused while any target is held, and SERVED once every one of them
/// has been deleted — the teardown a client performs, end to end.
#[tokio::test]
async fn a_resolver_is_deleted_once_its_targets_have_been() {
    let (router, _engine) = emulator();
    let ipam_id = create_ipam("the IPAM", &router).await;
    let resolver_id = create_resolver("the resolver torn down", &router, &ipam_id).await;

    let rows = target_cases();
    let mut targets: Vec<String> = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let prefix_list_id =
            create_prefix_list(row.case, &router, &format!("teardown-{index}")).await;
        let (target_id, _) = create_target(&router, &resolver_id, &prefix_list_id, row).await;
        targets.push(target_id);
    }

    // Every target still held, then one fewer: the resolver delete is refused until the last
    // of them is gone, so the refusal is about the targets rather than about the resolver.
    for (deleted, target_id) in targets.iter().enumerate() {
        let case = format!("{deleted} of {} targets deleted", targets.len());
        let (status, body) = invoke(
            &router,
            DELETE_RESOLVER,
            &[param("IpamPrefixListResolverId", &resolver_id)],
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 while a target is held, got {status} with body \
             {body}"
        );
        assert_eq!(
            error_code(&body),
            DEPENDENCY_VIOLATION,
            "case {case:?}: expected {DEPENDENCY_VIOLATION}, got {body}"
        );
        let _ = ok(&case, &router, DELETE_TARGET, &[
            param("IpamPrefixListResolverTargetId", target_id),
        ])
        .await;
    }

    // Every target deleted: the resolver itself is now deleted, answered in its own transient
    // delete state.
    let body = ok(
        "every target deleted",
        &router,
        DELETE_RESOLVER,
        &[param("IpamPrefixListResolverId", &resolver_id)],
    )
    .await;
    assert_eq!(
        element(&body, "ipamPrefixListResolverId").as_deref(),
        Some(resolver_id.as_str()),
        "the delete must answer the resolver it deleted, got {body}"
    );
    assert_eq!(
        element(&body, "state").as_deref(),
        Some(IPAM_PREFIX_LIST_RESOLVER_STATE_DELETE_IN_PROGRESS),
        "the delete must answer the resolver in \
         {IPAM_PREFIX_LIST_RESOLVER_STATE_DELETE_IN_PROGRESS}, got {body}"
    );
    assert_eq!(
        elements(&body, "ipamArn").len(),
        1,
        "the delete must answer the stored resolver's projection, got {body}"
    );
}

// ---------------------------------------------------------------------------
// What the delete refuses, and what it writes when it refuses: nothing
// ---------------------------------------------------------------------------

/// An id the account holds no target for, and a dry run, each answer without writing: the rows
/// differ in the id named and in whether the request is a dry run of one that would have
/// succeeded or of one that would not.
#[tokio::test]
async fn a_delete_that_cannot_proceed_answers_and_writes_nothing() {
    let (router, engine) = emulator();
    let ipam_id = create_ipam("the IPAM", &router).await;
    let resolver_id = create_resolver("the resolver", &router, &ipam_id).await;
    let prefix_list_id = create_prefix_list("the held target's list", &router, "refusals").await;
    let row = &target_cases()[0];
    let (target_id, _) = create_target(&router, &resolver_id, &prefix_list_id, row).await;

    // (case, the parameters, the status, the code)
    let rows: Vec<(&str, Vec<Param>, u16, &str)> = vec![
        (
            "an id the account holds no target for",
            vec![param("IpamPrefixListResolverTargetId", UNKNOWN_TARGET_ID)],
            400,
            INVALID_IPAM_PREFIX_LIST_RESOLVER_TARGET_ID_NOT_FOUND,
        ),
        (
            "a dry run of a delete that would have succeeded",
            vec![
                param("IpamPrefixListResolverTargetId", &target_id),
                param("DryRun", "true"),
            ],
            DRY_RUN_STATUS,
            DRY_RUN_OPERATION,
        ),
        (
            "a dry run naming an id the account holds no target for",
            vec![
                param("IpamPrefixListResolverTargetId", UNKNOWN_TARGET_ID),
                param("DryRun", "true"),
            ],
            DRY_RUN_STATUS,
            DRY_RUN_OPERATION,
        ),
    ];

    let before_ids = stored_target_ids(&engine);
    let before_states = stored_states(&engine);
    for (case, params, expected_status, expected_code) in rows {
        let (status, body) = invoke(&router, DELETE_TARGET, &params).await;
        assert_eq!(
            status, expected_status,
            "case {case:?}: expected HTTP {expected_status}, got {status} with body {body}"
        );
        let code = error_code(&body);
        assert_eq!(
            code, expected_code,
            "case {case:?}: expected {expected_code}, got {code} with message {:?}",
            error_message(&body)
        );
        assert!(
            !error_message(&body).is_empty(),
            "case {case:?}: a refusal has to say what was refused, got {body}"
        );
        assert_eq!(
            stored_target_ids(&engine),
            before_ids,
            "case {case:?}: a refused delete must remove nothing"
        );
        assert_eq!(
            stored_states(&engine),
            before_states,
            "case {case:?}: a refused delete must move nothing"
        );
    }

    // And the target the refusals were asserted against is still deletable.
    let body = ok(
        "the held target, after every refusal",
        &router,
        DELETE_TARGET,
        &[param("IpamPrefixListResolverTargetId", &target_id)],
    )
    .await;
    assert_eq!(
        element(&body, "state").as_deref(),
        Some(IPAM_PREFIX_LIST_RESOLVER_TARGET_STATE_DELETE_IN_PROGRESS),
        "the delete after the refusals must still write the deletion, got {body}"
    );
}
