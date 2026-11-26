// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `DeleteManagedPrefixList` called on a list the clock still owes a move,
//! driven over the wire the service actually speaks: `ec2Query` — an
//! `application/x-www-form-urlencoded` POST whose body carries `Action=<Operation>`, answered
//! with XML (README.md / OPERATIONS.md "Wire protocol"). ec2Query is selected by the `ec2`
//! signing name in the request's SigV4 credential scope, and the Region in that scope is the
//! Region the service answers for, so every call here carries one.
//!
//! The rule under test: a managed prefix list delete turns ONLY on the references other
//! resources of the account still hold to the list — "you must first remove all references to
//! the prefix list in your resources" (`DeleteManagedPrefixList`) — so a list the account owns
//! is deleted whichever state of the modelled `PrefixListState` catalog it stands in, the
//! transient states a create, a revision or a restore leaves it in included: the call answers
//! the stored list with `delete-in-progress` in place of that state and the clock removes it.
//! The documented precondition is unaffected by that state: a list a resource of the account
//! references is still refused, in a transient state exactly as in a settled one, and the same
//! delete succeeds once the reference is gone.
//!
//! Each row of the table below is a distinct input along what the rule generalises over: WHICH
//! clock-owed move is in flight when the delete arrives — a creation, a revision of the entry
//! set, a restore of an earlier version — with the settled counterpart of each as the control
//! row, and the version the list stands at differing with it.

use std::time::Duration;

use ec2::state::{
    ComAmazonawsEc2Amazonec2State as Ec2State, FIRST_PREFIX_LIST_VERSION,
    PREFIX_LIST_ADDRESS_FAMILY_IPV4, PREFIX_LIST_STATE_CREATE_COMPLETE,
    PREFIX_LIST_STATE_CREATE_IN_PROGRESS, PREFIX_LIST_STATE_DELETE_IN_PROGRESS,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{element, error_code, error_message};
use super::wire::{Param, param};

/// The Region every call here is signed for.
const REGION: &str = "us-west-2";

/// EC2's code for a request naming an id no prefix list the caller can see carries.
const NOT_FOUND: &str = "InvalidPrefixListID.NotFound";

/// EC2's code for a delete refused because another resource still references the resource the
/// request named.
const DEPENDENCY_VIOLATION: &str = "DependencyViolation";

/// The states of the modelled catalog (`smithy_query(query="shape",shape="PrefixListState")`) a
/// list is left in while the move that will settle it is still owed to the clock, for the two
/// the crate does not already name beside the ones imported above.
const MODIFY_IN_PROGRESS: &str = "modify-in-progress";
const RESTORE_IN_PROGRESS: &str = "restore-in-progress";

/// The settled states of that same catalog a revision and a restore reach.
const MODIFY_COMPLETE: &str = "modify-complete";
const RESTORE_COMPLETE: &str = "restore-complete";

/// Long enough past the span a clock-owed move takes that it is certainly overdue: the deadline
/// the polls below give up at, generous because what is under test is the state a list reaches
/// rather than how fast it reaches it.
fn move_deadline() -> Duration {
    Duration::from_secs(60)
}

/// How long a poll below waits between two reads of a list.
fn poll_interval() -> Duration {
    Duration::from_millis(25)
}

/// One `action` call carrying `params`, as its HTTP status and XML body.
async fn call(router: &MultiServiceRouter, action: &str, params: &[Param]) -> (u16, String) {
    super::ec2_query::call(router, REGION, action, params).await
}

/// One `action` call that has to succeed, and the body it answered.
async fn ok(case: &str, router: &MultiServiceRouter, action: &str, params: &[Param]) -> String {
    let (status, body) = call(router, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// One element of a response body, or a panic naming the case.
fn member(case: &str, body: &str, name: &str) -> String {
    element(body, name).unwrap_or_else(|| panic!("case {case:?}: no <{name}> in {body}"))
}

/// One `DescribeManagedPrefixLists` narrowed to `prefix_list_id`, as its status and XML body.
async fn describe_one(router: &MultiServiceRouter, prefix_list_id: &str) -> (u16, String) {
    call(
        router,
        "DescribeManagedPrefixLists",
        &[param("PrefixListId.1", prefix_list_id)],
    )
    .await
}

/// One `DeleteManagedPrefixList` naming `prefix_list_id`, as its status and XML body.
async fn delete(router: &MultiServiceRouter, prefix_list_id: &str) -> (u16, String) {
    call(
        router,
        "DeleteManagedPrefixList",
        &[param("PrefixListId", prefix_list_id)],
    )
    .await
}

/// Wait until the describe reports `prefix_list_id` in `state`.
///
/// Polls for that STATE rather than sleeping for the span the clock owes the move, so a loaded
/// machine cannot read the list before the move has been applied.
async fn poll_until_state(
    case: &str,
    router: &MultiServiceRouter,
    prefix_list_id: &str,
    state: &str,
) {
    let deadline = tokio::time::Instant::now() + move_deadline();
    loop {
        let (status, body) = describe_one(router, prefix_list_id).await;
        let reported = element(&body, "state");
        if status == 200 && reported.as_deref() == Some(state) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case:?}: {prefix_list_id} must reach {state:?}, last read {status} with \
             {reported:?}"
        );
        tokio::time::sleep(poll_interval()).await;
    }
}

/// Wait until nothing reports `prefix_list_id` any more — the REMOVAL a delete starts, which the
/// clock owes the list.
async fn poll_until_gone(case: &str, router: &MultiServiceRouter, prefix_list_id: &str) {
    let deadline = tokio::time::Instant::now() + move_deadline();
    loop {
        let (status, body) = describe_one(router, prefix_list_id).await;
        if (status, error_code(&body).as_str()) == (400, NOT_FOUND) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case:?}: {prefix_list_id} must stop being reported, last read {status} with \
             body {body}"
        );
        tokio::time::sleep(poll_interval()).await;
    }
}

/// What a row of the table does to the list between the create and the delete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Settling {
    /// Nothing: the delete arrives while the creation is still owed to the clock.
    Creating,
    /// The creation has settled, and nothing else was asked of the list.
    Created,
    /// A revision of the entry set is still owed to the clock.
    Revising,
    /// That revision has settled.
    Revised,
    /// A restore of the list's first version is still owed to the clock.
    Restoring,
    /// That restore has settled.
    Restored,
}

/// One row of the table: the move in flight when the delete arrives, the state the list is then
/// in, and the version it then stands at.
struct Case {
    /// The case name every assertion of the row names.
    name: &'static str,
    /// What is done to the list before the delete.
    settling: Settling,
    /// The state of the modelled catalog the list is in when the delete arrives.
    state: &'static str,
    /// The version the list stands at when the delete arrives: a revision's new version comes
    /// into force with the state it settles into, so a revision still in flight leaves the list
    /// at the version it was read at.
    version: i64,
}

/// The rows the table is written against: each transient state a list of the account can be
/// left in by its own lifecycle, beside the settled state that move reaches.
fn cases() -> Vec<Case> {
    vec![
        Case {
            name: "a creation still owed to the clock",
            settling: Settling::Creating,
            state: PREFIX_LIST_STATE_CREATE_IN_PROGRESS,
            version: FIRST_PREFIX_LIST_VERSION,
        },
        Case {
            name: "a settled creation",
            settling: Settling::Created,
            state: PREFIX_LIST_STATE_CREATE_COMPLETE,
            version: FIRST_PREFIX_LIST_VERSION,
        },
        Case {
            name: "a revision still owed to the clock",
            settling: Settling::Revising,
            state: MODIFY_IN_PROGRESS,
            version: FIRST_PREFIX_LIST_VERSION,
        },
        Case {
            name: "a settled revision",
            settling: Settling::Revised,
            state: MODIFY_COMPLETE,
            version: FIRST_PREFIX_LIST_VERSION + 1,
        },
        Case {
            name: "a restore still owed to the clock",
            settling: Settling::Restoring,
            state: RESTORE_IN_PROGRESS,
            version: FIRST_PREFIX_LIST_VERSION + 1,
        },
        Case {
            name: "a settled restore",
            settling: Settling::Restored,
            state: RESTORE_COMPLETE,
            version: FIRST_PREFIX_LIST_VERSION + 2,
        },
    ]
}

/// One prefix list of the account, taken to the point the row asks for, as the id the create
/// minted.
///
/// `index` keeps the names of the lists the rows create apart.
async fn list_for(case: &Case, router: &MultiServiceRouter, index: usize) -> String {
    let name = case.name;
    let created = ok(
        name,
        router,
        "CreateManagedPrefixList",
        &[
            param("PrefixListName", &format!("settling-{index}")),
            param("AddressFamily", PREFIX_LIST_ADDRESS_FAMILY_IPV4),
            param("MaxEntries", "5"),
            param("Entry.1.Cidr", "10.0.0.0/8"),
        ],
    )
    .await;
    let prefix_list_id = member(name, &created, "prefixListId");
    if case.settling == Settling::Creating {
        return prefix_list_id;
    }
    poll_until_state(name, router, &prefix_list_id, PREFIX_LIST_STATE_CREATE_COMPLETE).await;
    if case.settling == Settling::Created {
        return prefix_list_id;
    }

    // A revision of the entry set, which takes the list to a later version.
    ok(
        name,
        router,
        "ModifyManagedPrefixList",
        &[
            param("PrefixListId", &prefix_list_id),
            param("AddEntry.1.Cidr", "172.16.0.0/12"),
        ],
    )
    .await;
    if case.settling == Settling::Revising {
        return prefix_list_id;
    }
    poll_until_state(name, router, &prefix_list_id, MODIFY_COMPLETE).await;
    if case.settling == Settling::Revised {
        return prefix_list_id;
    }

    // A restore of the list's FIRST version, which is a revision of the entry set like any
    // other: "PreviousVersion" and "CurrentVersion" are both required
    // (`smithy_query(query="operation",operation="RestoreManagedPrefixListVersion")`).
    ok(
        name,
        router,
        "RestoreManagedPrefixListVersion",
        &[
            param("PrefixListId", &prefix_list_id),
            param("PreviousVersion", &FIRST_PREFIX_LIST_VERSION.to_string()),
            param(
                "CurrentVersion",
                &(FIRST_PREFIX_LIST_VERSION + 1).to_string(),
            ),
        ],
    )
    .await;
    if case.settling == Settling::Restoring {
        return prefix_list_id;
    }
    poll_until_state(name, router, &prefix_list_id, RESTORE_COMPLETE).await;
    prefix_list_id
}

/// The rule: whichever state of its own lifecycle a list of the account stands in, the delete
/// answers it in `delete-in-progress` and the clock removes it.
#[tokio::test]
async fn a_delete_does_not_wait_for_a_move_the_clock_owes() {
    let (router, engine) = super::ec2_query::emulator();

    for (index, case) in cases().iter().enumerate() {
        let name = case.name;
        let prefix_list_id = list_for(case, &router, index).await;

        // The state and version the row says the list stands in when the delete arrives: the
        // premise of the row, asserted so a row that stopped exercising its own transient state
        // fails instead of passing for the wrong reason.
        let (status, before) = describe_one(&router, &prefix_list_id).await;
        assert_eq!(
            (status, element(&before, "state").as_deref()),
            (200, Some(case.state)),
            "case {name:?}: expected the list to stand in {state:?}, got {status} with body \
             {before}",
            state = case.state
        );
        assert_eq!(
            element(&before, "version").as_deref(),
            Some(case.version.to_string().as_str()),
            "case {name:?}: expected version {version}, got {before}",
            version = case.version
        );

        let (status, body) = delete(&router, &prefix_list_id).await;
        assert_eq!(
            status, 200,
            "case {name:?}: a list of the account in {state:?} must be deleted, got {status} \
             with body {body}",
            state = case.state
        );
        assert_eq!(
            element(&body, "prefixListId").as_deref(),
            Some(prefix_list_id.as_str()),
            "case {name:?}: the delete must answer the list it was asked for, got {body}"
        );
        assert_eq!(
            element(&body, "state").as_deref(),
            Some(PREFIX_LIST_STATE_DELETE_IN_PROGRESS),
            "case {name:?}: expected the transient delete state, got {body}"
        );
        // A delete is no revision of the entry set, so the version the answer carries is the one
        // the list stood at — the revision a delete supersedes never comes into force.
        assert_eq!(
            element(&body, "version").as_deref(),
            Some(case.version.to_string().as_str()),
            "case {name:?}: expected version {version} in the answer, got {body}",
            version = case.version
        );

        // The removal is owed to the CLOCK: the list is still reported in the transient state
        // straight after the delete, and nothing reports it once that removal has fallen due.
        let (status, straight_after) = describe_one(&router, &prefix_list_id).await;
        assert_eq!(
            (status, element(&straight_after, "state").as_deref()),
            (200, Some(PREFIX_LIST_STATE_DELETE_IN_PROGRESS)),
            "case {name:?}: a list whose removal is still owed must be reported in \
             {PREFIX_LIST_STATE_DELETE_IN_PROGRESS:?}, got {status} with body {straight_after}"
        );
        poll_until_gone(name, &router, &prefix_list_id).await;
        assert!(
            !engine
                .state::<Ec2State>()
                .lock()
                .managed_prefix_lists
                .contains_key(&prefix_list_id),
            "case {name:?}: the store must hold no removed list"
        );
    }
}

/// The documented precondition is what the delete turns on, in a transient state exactly as in
/// a settled one: a referenced list is refused `DependencyViolation` and stays, and the same
/// delete succeeds once the reference is gone — the list still being `create-in-progress`.
#[tokio::test]
async fn a_referenced_list_is_refused_while_a_move_is_owed() {
    let (router, _engine) = super::ec2_query::emulator();
    let case = "a referenced list whose creation is still owed";

    let created = ok(
        case,
        &router,
        "CreateManagedPrefixList",
        &[
            param("PrefixListName", "referenced-while-creating"),
            param("AddressFamily", PREFIX_LIST_ADDRESS_FAMILY_IPV4),
            param("MaxEntries", "5"),
            param("Entry.1.Cidr", "10.0.0.0/8"),
        ],
    )
    .await;
    let prefix_list_id = member(case, &created, "prefixListId");

    // One reference of the account to that list: a security group rule whose grant names it,
    // which is one of the resources `GetManagedPrefixListAssociations` reports.
    let group = ok(
        case,
        &router,
        "CreateSecurityGroup",
        &[
            param("GroupName", "references-a-creating-list"),
            param("GroupDescription", "a group that references a prefix list"),
        ],
    )
    .await;
    let group_id = member(case, &group, "groupId");
    let grant = vec![
        param("GroupId", &group_id),
        param("IpPermissions.1.IpProtocol", "tcp"),
        param("IpPermissions.1.FromPort", "443"),
        param("IpPermissions.1.ToPort", "443"),
        param("IpPermissions.1.PrefixListIds.1.PrefixListId", &prefix_list_id),
    ];
    ok(case, &router, "AuthorizeSecurityGroupIngress", &grant).await;

    let (status, body) = delete(&router, &prefix_list_id).await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (400, DEPENDENCY_VIOLATION),
        "case {case:?}: expected 400 {DEPENDENCY_VIOLATION}, got {status} with body {body}"
    );
    assert!(
        !error_message(&body).is_empty(),
        "case {case:?}: a refusal must say what was wrong, got {body}"
    );
    // The refused delete wrote nothing: the list is still the list it was, state included.
    let (status, described) = describe_one(&router, &prefix_list_id).await;
    assert_eq!(
        status, 200,
        "case {case:?}: a refused delete must leave the list it named, got {status} with body \
         {described}"
    );
    assert_ne!(
        element(&described, "state").as_deref(),
        Some(PREFIX_LIST_STATE_DELETE_IN_PROGRESS),
        "case {case:?}: a refused delete must not start one, got {described}"
    );

    // The reference taken back out, and the same delete — the list having had no chance to
    // settle in between is exactly what the rule is about.
    ok(case, &router, "RevokeSecurityGroupIngress", &grant).await;
    let (status, body) = delete(&router, &prefix_list_id).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the delete must succeed once the reference is gone, got {status} with \
         body {body}"
    );
    assert_eq!(
        element(&body, "state").as_deref(),
        Some(PREFIX_LIST_STATE_DELETE_IN_PROGRESS),
        "case {case:?}: expected the transient delete state, got {body}"
    );
    poll_until_gone(case, &router, &prefix_list_id).await;
}
