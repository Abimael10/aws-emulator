// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `RestoreManagedPrefixListVersion`, driven over the wire the service
//! actually speaks: `ec2Query` — an `application/x-www-form-urlencoded` POST whose body
//! carries `Action=<Operation>`, answered with XML (README.md / OPERATIONS.md "Wire
//! protocol"). ec2Query is selected by the `ec2` signing name in the request's SigV4
//! credential scope, and the Region in that scope is the Region the service answers for, so
//! every call here carries one.
//!
//! The rules under test:
//!
//! 1. A restore writes the entry set an EARLIER version of the list had onto a NEW version of
//!    it: "restores the entries from a previous version of a managed prefix list to a new
//!    version of the prefix list". The version the list stood at is left behind and reports
//!    ITS own entries, so a restore is itself restorable. Every row of the table is a distinct
//!    input along what that generalises over: how many versions back the restored one is, how
//!    many entries it carried (none included), whether the restored set is larger or smaller
//!    than the one it replaces, the descriptions those entries carry, and the address family.
//! 2. The list is answered in the non-terminal `restore-in-progress` state the model
//!    publishes, and the clock — not a later call — settles it into `restore-complete`; a
//!    further revision of the list starts from there.
//! 3. A request this operation cannot serve is REFUSED with EC2's code for that class and
//!    changes nothing: an id that is no prefix list id, an id nothing the caller can see
//!    carries, a list Amazon Web Services publishes rather than the account owning, a
//!    `CurrentVersion` that is not the version the list stands at, a `PreviousVersion` the
//!    list has never had, the version the list already stands at, a restore that would take
//!    the list past its maximum size, and a list whose creation is still in progress. An id no
//!    prefix list the caller can see carries is refused with the sentence EC2 words that code
//!    with for the operations that RESOLVE one managed prefix list.
//! 4. A `DryRun` answers 412 `DryRunOperation` and changes nothing at all: not the version,
//!    not the entries, not the state.

use std::time::Duration;

use ec2::state::{
    ComAmazonawsEc2Amazonec2State as Ec2State, FIRST_PREFIX_LIST_VERSION,
    PREFIX_LIST_ADDRESS_FAMILY_IPV4, PREFIX_LIST_ADDRESS_FAMILY_IPV6,
    PREFIX_LIST_STATE_CREATE_COMPLETE, PREFIX_LIST_STATE_CREATE_IN_PROGRESS,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{element, elements, error_code, error_message};

/// The Region every call here is signed for.
const REGION: &str = "us-west-2";

/// The two words of the modelled `PrefixListState` catalog a restore moves a list through —
/// the non-terminal one the call answers and the settled one the clock owes it
/// (`smithy_query(query="shape",shape="PrefixListState")`).
const RESTORE_IN_PROGRESS: &str = "restore-in-progress";
const RESTORE_COMPLETE: &str = "restore-complete";

/// The settled state of that same catalog a revision of the entry set reaches, which is where
/// a list stands when a restore of one of its earlier versions may start.
const MODIFY_COMPLETE: &str = "modify-complete";

/// Long enough past the span a clock-owed move takes that it is certainly overdue: the
/// deadline the polls below give up at, generous because what is under test is the state a
/// prefix list reaches rather than how fast it reaches it.
fn move_deadline() -> Duration {
    Duration::from_secs(60)
}

/// How long a poll below waits between two reads of a prefix list's state.
fn poll_interval() -> Duration {
    Duration::from_millis(50)
}

/// One entry of a request: its CIDR block and, when it carries one, its description.
type Entry = (&'static str, Option<&'static str>);

/// One form field of an ec2Query body.
type Field = (String, String);

/// One row of the refusal table: the case, the fields it sends, and the HTTP status and EC2
/// code it must be refused with.
type RefusalRow = (String, Vec<Field>, u16, &'static str);

/// One form field.
fn param(name: &str, value: &str) -> Field {
    (name.to_string(), value.to_string())
}

/// The form fields that name `entries` under the indexed key `member`, as ec2Query flattens a
/// list of entries into the `<Member>.N.<Field>` keys the EC2 API documents (`Entry` for a
/// create's `Entries`, `AddEntry` for a modify's `AddEntries`).
fn entry_params(member: &str, entries: &[Entry]) -> Vec<Field> {
    let mut params = Vec::new();
    for (index, (cidr, description)) in entries.iter().enumerate() {
        let position = index + 1;
        params.push(param(&format!("{member}.{position}.Cidr"), cidr));
        if let Some(description) = description {
            params.push(param(
                &format!("{member}.{position}.Description"),
                description,
            ));
        }
    }
    params
}

/// One prefix list of the account, created with `entries`; answers the id it was given.
async fn create_list(
    router: &MultiServiceRouter,
    case: &str,
    prefix_list_name: &str,
    address_family: &str,
    max_entries: usize,
    entries: &[Entry],
) -> String {
    let mut params = vec![
        param("PrefixListName", prefix_list_name),
        param("AddressFamily", address_family),
        param("MaxEntries", &max_entries.to_string()),
    ];
    params.extend(entry_params("Entry", entries));
    let (status, body) =
        super::ec2_query::call(router, REGION, "CreateManagedPrefixList", &params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the create must succeed, got {status} with body {body}"
    );
    element(&body, "prefixListId")
        .unwrap_or_else(|| panic!("case {case:?}: the create reported no prefixListId: {body}"))
}

/// One `ModifyManagedPrefixList` call that replaces the list's entry set with `entries`,
/// minting the next version. Answers nothing: what it leaves behind is read back through the
/// operations that report the list.
async fn replace_entries(
    router: &MultiServiceRouter,
    case: &str,
    prefix_list_id: &str,
    held: &[Entry],
    entries: &[Entry],
) {
    // The removals are indexed from one, as ec2Query flattens a list of them.
    let mut params = vec![param("PrefixListId", prefix_list_id)];
    for (index, (cidr, _)) in held.iter().enumerate() {
        params.push(param(&format!("RemoveEntry.{}.Cidr", index + 1), cidr));
    }
    params.extend(entry_params("AddEntry", entries));
    let (status, body) =
        super::ec2_query::call(router, REGION, "ModifyManagedPrefixList", &params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the revision must succeed, got {status} with body {body}"
    );
}

/// One `RestoreManagedPrefixListVersion` call carrying `params`, as its HTTP status and XML
/// body.
async fn restore(router: &MultiServiceRouter, params: &[Field]) -> (u16, String) {
    super::ec2_query::call(router, REGION, "RestoreManagedPrefixListVersion", params).await
}

/// The form fields a restore of `previous_version` from `current_version` sends.
fn restore_params(prefix_list_id: &str, current_version: i64, previous_version: i64) -> Vec<Field> {
    vec![
        param("PrefixListId", prefix_list_id),
        param("CurrentVersion", &current_version.to_string()),
        param("PreviousVersion", &previous_version.to_string()),
    ]
}

/// One `DescribeManagedPrefixLists` call narrowed to `prefix_list_id`, as its XML body.
async fn describe_one(router: &MultiServiceRouter, case: &str, prefix_list_id: &str) -> String {
    let (status, body) = super::ec2_query::call(
        router,
        REGION,
        "DescribeManagedPrefixLists",
        &[param("PrefixListId.1", prefix_list_id)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the describe must succeed, got {status} with body {body}"
    );
    body
}

/// Look at the list AGAIN until the describe reports it in `wanted`, or a deadline far past
/// the service's own transition span passes. Answers the body that reported it.
///
/// Polls for the STATE rather than sleeping for the span the clock owes the move, so a loaded
/// machine only looks more often instead of reading before the move has been applied.
async fn poll_until_state(
    router: &MultiServiceRouter,
    case: &str,
    prefix_list_id: &str,
    wanted: &str,
) -> String {
    let deadline = tokio::time::Instant::now() + move_deadline();
    loop {
        let body = describe_one(router, case, prefix_list_id).await;
        let reported = element(&body, "state");
        if reported.as_deref() == Some(wanted) {
            return body;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case:?}: the prefix list {prefix_list_id} must reach {wanted:?}, last read \
             {reported:?}"
        );
        tokio::time::sleep(poll_interval()).await;
    }
}

/// The entries a `GetManagedPrefixListEntries` answer reports, as `(cidr, description)` in the
/// order it reported them.
///
/// Read off the document tree rather than by substring, so a description is paired with the
/// entry it belongs to rather than with the one at the same position in a flat list of
/// elements.
fn reported_entries(case: &str, body: &str) -> Vec<(String, Option<String>)> {
    let document = super::wire::document(case, body);
    super::wire::set_items(document.root_element(), "entrySet")
        .into_iter()
        .map(|item| {
            (
                super::wire::child_text(item, "cidr").unwrap_or_default(),
                super::wire::child_text(item, "description"),
            )
        })
        .collect()
}

/// The entries the list `prefix_list_id` reports for `target_version` — the version it stands
/// at when the row names none.
async fn entries_at(
    router: &MultiServiceRouter,
    case: &str,
    prefix_list_id: &str,
    target_version: Option<i64>,
) -> Vec<(String, Option<String>)> {
    let mut params = vec![param("PrefixListId", prefix_list_id)];
    if let Some(version) = target_version {
        params.push(param("TargetVersion", &version.to_string()));
    }
    let (status, body) =
        super::ec2_query::call(router, REGION, "GetManagedPrefixListEntries", &params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the entry report must succeed, got {status} with body {body}"
    );
    reported_entries(case, &body)
}

/// A row's entry set as the entry report answers it: ascending CIDR block, each block in the
/// canonical form the service stores.
fn expected_entries(entries: &[Entry]) -> Vec<(String, Option<String>)> {
    let mut expected: Vec<(String, Option<String>)> = entries
        .iter()
        .map(|(cidr, description)| {
            (
                (*cidr).to_string(),
                description.map(|description| description.to_string()),
            )
        })
        .collect();
    expected.sort_by(|left, right| left.0.cmp(&right.0));
    expected
}

/// One row of the restore table: a list created with one entry set and revised into further
/// ones, then restored back to the version `previous_version`.
struct RestoreCase {
    name: &'static str,
    address_family: &'static str,
    /// The maximum size the list is created with.
    max_entries: usize,
    /// The entry set of every version the list holds in turn, starting at
    /// [`FIRST_PREFIX_LIST_VERSION`]: the first is what the create stores and each one after
    /// it is what a revision replaces the one before with.
    versions: Vec<Vec<Entry>>,
    /// The version the restore names, one of the versions above.
    previous_version: i64,
}

/// The rows the restore table is written against, each a distinct input along what the rule
/// generalises over: the version immediately before the current one and one two revisions
/// back, an entry set carrying nothing, a restored set larger than the one it replaces and one
/// smaller, entries carrying descriptions and entries carrying none, and both address
/// families.
fn restore_cases() -> Vec<RestoreCase> {
    vec![
        RestoreCase {
            name: "the version immediately before the one the list stands at",
            address_family: PREFIX_LIST_ADDRESS_FAMILY_IPV4,
            max_entries: 3,
            versions: vec![
                vec![("10.0.0.0/8", Some("the office range"))],
                vec![("10.1.0.0/16", None)],
            ],
            previous_version: FIRST_PREFIX_LIST_VERSION,
        },
        RestoreCase {
            name: "a version two revisions back",
            address_family: PREFIX_LIST_ADDRESS_FAMILY_IPV4,
            max_entries: 3,
            versions: vec![
                vec![("10.2.0.0/16", Some("one")), ("10.3.0.0/16", None)],
                vec![("10.4.0.0/16", None)],
                vec![("10.5.0.0/16", Some("three"))],
            ],
            previous_version: FIRST_PREFIX_LIST_VERSION,
        },
        RestoreCase {
            name: "a version in the middle of three",
            address_family: PREFIX_LIST_ADDRESS_FAMILY_IPV4,
            max_entries: 3,
            versions: vec![
                vec![("10.6.0.0/16", None)],
                vec![("10.7.0.0/16", Some("the middle one"))],
                vec![("10.8.0.0/16", None)],
            ],
            previous_version: FIRST_PREFIX_LIST_VERSION + 1,
        },
        RestoreCase {
            name: "a version that carried no entry at all",
            address_family: PREFIX_LIST_ADDRESS_FAMILY_IPV4,
            max_entries: 2,
            versions: vec![Vec::new(), vec![("10.9.0.0/16", Some("added later"))]],
            previous_version: FIRST_PREFIX_LIST_VERSION,
        },
        RestoreCase {
            name: "a version carrying more entries than the one it replaces",
            address_family: PREFIX_LIST_ADDRESS_FAMILY_IPV4,
            max_entries: 3,
            versions: vec![
                vec![
                    ("10.10.0.0/16", Some("one")),
                    ("10.11.0.0/16", None),
                    ("10.12.0.0/16", Some("three")),
                ],
                vec![("10.13.0.0/16", None)],
            ],
            previous_version: FIRST_PREFIX_LIST_VERSION,
        },
        RestoreCase {
            name: "an IPv6 list restored to its first version",
            address_family: PREFIX_LIST_ADDRESS_FAMILY_IPV6,
            max_entries: 2,
            versions: vec![
                vec![("2001:db8:1::/48", Some("documentation range"))],
                vec![("2001:db8:2::/48", None)],
            ],
            previous_version: FIRST_PREFIX_LIST_VERSION,
        },
    ]
}

/// Rule 1: a restore writes the entry set of the version it names onto a NEW version, and the
/// version the list stood at is left behind reporting its own entries.
#[tokio::test]
async fn a_restore_writes_an_earlier_versions_entry_set_onto_a_new_version() {
    let (router, _engine) = super::ec2_query::emulator();

    for (index, case) in restore_cases().into_iter().enumerate() {
        let held = case
            .versions
            .first()
            .expect("every row names the entry set the create stores");
        let prefix_list_id = create_list(
            &router,
            case.name,
            &format!("restore-case-{index}"),
            case.address_family,
            case.max_entries,
            held,
        )
        .await;
        // A list whose creation is still owed to the clock is not one a revision may start
        // from, so each step waits for the state the previous one owes before taking the next.
        poll_until_state(
            &router,
            case.name,
            &prefix_list_id,
            PREFIX_LIST_STATE_CREATE_COMPLETE,
        )
        .await;
        for pair in case.versions.windows(2) {
            replace_entries(&router, case.name, &prefix_list_id, &pair[0], &pair[1]).await;
            poll_until_state(&router, case.name, &prefix_list_id, MODIFY_COMPLETE).await;
        }

        let held_versions = i64::try_from(case.versions.len())
            .expect("a row names a handful of versions");
        let current_version = FIRST_PREFIX_LIST_VERSION + held_versions - 1;
        let restored_index = usize::try_from(case.previous_version - FIRST_PREFIX_LIST_VERSION)
            .expect("the row names a version the list has held");
        let restored = &case.versions[restored_index];

        let (status, body) = restore(
            &router,
            &restore_params(&prefix_list_id, current_version, case.previous_version),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {name:?}: expected HTTP 200, got {status} with body {body}",
            name = case.name
        );
        assert_eq!(
            element(&body, "prefixListId").as_deref(),
            Some(prefix_list_id.as_str()),
            "case {name:?}: expected the list the request named, got {body}",
            name = case.name
        );
        assert_eq!(
            element(&body, "version"),
            Some((current_version + 1).to_string()),
            "case {name:?}: expected the next version, got {body}",
            name = case.name
        );
        assert_eq!(
            element(&body, "state").as_deref(),
            Some(RESTORE_IN_PROGRESS),
            "case {name:?}: expected {RESTORE_IN_PROGRESS}, got {body}",
            name = case.name
        );
        assert_eq!(
            element(&body, "maxEntries"),
            Some(case.max_entries.to_string()),
            "case {name:?}: a restore must leave the maximum size alone, got {body}",
            name = case.name
        );

        // The entry set the restore wrote, read back through the operation that reports a
        // prefix list's entries.
        let reported = entries_at(&router, case.name, &prefix_list_id, None).await;
        assert_eq!(
            reported,
            expected_entries(restored),
            "case {name:?}: expected the restored entry set, got {reported:?}",
            name = case.name
        );
        let at_new_version = entries_at(
            &router,
            case.name,
            &prefix_list_id,
            Some(current_version + 1),
        )
        .await;
        assert_eq!(
            at_new_version, reported,
            "case {name:?}: naming the version the restore minted must report the restored \
             entries, got {at_new_version:?}",
            name = case.name
        );

        // Every version the list has LEFT still reports its own entries — the one the restore
        // superseded included, which is what makes a restore itself restorable.
        for (offset, entries) in case.versions.iter().enumerate() {
            let version = FIRST_PREFIX_LIST_VERSION
                + i64::try_from(offset).expect("a row names a handful of versions");
            let at_version =
                entries_at(&router, case.name, &prefix_list_id, Some(version)).await;
            assert_eq!(
                at_version,
                expected_entries(entries),
                "case {name:?}: version {version} must still report its own entries, got \
                 {at_version:?}",
                name = case.name
            );
        }
    }
}

/// Rule 2: the clock — not a later call — settles a restored list into `restore-complete`, and
/// a further restore starts from there, so the version a restore superseded can itself be
/// restored.
#[tokio::test]
async fn the_clock_settles_a_restored_list_and_a_further_restore_starts_from_there() {
    let (router, _engine) = super::ec2_query::emulator();

    let first = [("10.30.0.0/16", Some("the first set"))];
    let second = [("10.31.0.0/16", None)];
    let prefix_list_id = create_list(
        &router,
        "a settling restore",
        "settling-restore-list",
        PREFIX_LIST_ADDRESS_FAMILY_IPV4,
        2,
        &first,
    )
    .await;
    poll_until_state(
        &router,
        "a settling restore",
        &prefix_list_id,
        PREFIX_LIST_STATE_CREATE_COMPLETE,
    )
    .await;
    replace_entries(
        &router,
        "a settling restore",
        &prefix_list_id,
        &first,
        &second,
    )
    .await;
    poll_until_state(
        &router,
        "a settling restore",
        &prefix_list_id,
        MODIFY_COMPLETE,
    )
    .await;

    // Version 1 restored onto version 3: the call answers the transient state, and the state
    // the clock owes is not applied by this reader but by the clock.
    let (status, body) = restore(
        &router,
        &restore_params(&prefix_list_id, FIRST_PREFIX_LIST_VERSION + 1, FIRST_PREFIX_LIST_VERSION),
    )
    .await;
    assert_eq!(
        status, 200,
        "the first restore: expected HTTP 200, got {status} with body {body}"
    );
    let straight_after = describe_one(&router, "straight after the restore", &prefix_list_id).await;
    assert_eq!(
        element(&straight_after, "state").as_deref(),
        Some(RESTORE_IN_PROGRESS),
        "straight after the restore: expected {RESTORE_IN_PROGRESS}, got {straight_after}"
    );

    let settled = poll_until_state(
        &router,
        "past the transition",
        &prefix_list_id,
        RESTORE_COMPLETE,
    )
    .await;
    assert_eq!(
        element(&settled, "version"),
        Some((FIRST_PREFIX_LIST_VERSION + 2).to_string()),
        "past the transition: the version the restore minted must stand, got {settled}"
    );

    // The version the restore superseded is itself restorable from the settled state.
    let (status, body) = restore(
        &router,
        &restore_params(
            &prefix_list_id,
            FIRST_PREFIX_LIST_VERSION + 2,
            FIRST_PREFIX_LIST_VERSION + 1,
        ),
    )
    .await;
    assert_eq!(
        status, 200,
        "the second restore: expected HTTP 200, got {status} with body {body}"
    );
    assert_eq!(
        element(&body, "version"),
        Some((FIRST_PREFIX_LIST_VERSION + 3).to_string()),
        "the second restore: expected the next version, got {body}"
    );
    let reported = entries_at(&router, "the second restore", &prefix_list_id, None).await;
    assert_eq!(
        reported,
        expected_entries(&second),
        "the second restore: expected the entry set of the version it named, got {reported:?}"
    );
}

/// Rule 3: a request this operation cannot serve is refused with EC2's code for that class and
/// changes nothing. Rule 4's `DryRun` row rides along, because what it asserts is the same:
/// the refusal and an untouched store.
#[tokio::test]
async fn a_request_the_operation_cannot_serve_is_refused_and_changes_nothing() {
    let (router, engine) = super::ec2_query::emulator();

    // A list with two versions behind it: the entry set the create stored, and the narrower one
    // the revision after it replaced that with. Its maximum size admits the first version back,
    // so a restore of it is refused only for what each row names.
    let created = [("10.40.0.0/16", Some("held")), ("10.41.0.0/16", None)];
    let revised = [("10.42.0.0/16", None)];
    let prefix_list_id = create_list(
        &router,
        "the refusal rows",
        "restore-refusal-list",
        PREFIX_LIST_ADDRESS_FAMILY_IPV4,
        2,
        &created,
    )
    .await;
    poll_until_state(
        &router,
        "the refusal rows",
        &prefix_list_id,
        PREFIX_LIST_STATE_CREATE_COMPLETE,
    )
    .await;
    replace_entries(
        &router,
        "the refusal rows",
        &prefix_list_id,
        &created,
        &revised,
    )
    .await;
    poll_until_state(&router, "the refusal rows", &prefix_list_id, MODIFY_COMPLETE).await;

    // A restore is held to the maximum size the list stands at, so an earlier version carrying
    // more entries than it now admits is refused. Its own list, narrowed after the revision:
    // "you cannot modify the entries of a prefix list and modify the size of a prefix list at
    // the same time", so the two changes are separate calls.
    let wide = [
        ("10.60.0.0/16", Some("one")),
        ("10.61.0.0/16", None),
        ("10.62.0.0/16", Some("three")),
    ];
    let narrow = [("10.63.0.0/16", None)];
    let narrowed_id = create_list(
        &router,
        "a version past the maximum size",
        "restore-narrowed-list",
        PREFIX_LIST_ADDRESS_FAMILY_IPV4,
        3,
        &wide,
    )
    .await;
    poll_until_state(
        &router,
        "a version past the maximum size",
        &narrowed_id,
        PREFIX_LIST_STATE_CREATE_COMPLETE,
    )
    .await;
    replace_entries(
        &router,
        "a version past the maximum size",
        &narrowed_id,
        &wide,
        &narrow,
    )
    .await;
    poll_until_state(
        &router,
        "a version past the maximum size",
        &narrowed_id,
        MODIFY_COMPLETE,
    )
    .await;
    let (status, body) = super::ec2_query::call(
        &router,
        REGION,
        "ModifyManagedPrefixList",
        &[param("PrefixListId", &narrowed_id), param("MaxEntries", "1")],
    )
    .await;
    assert_eq!(
        status, 200,
        "a version past the maximum size: the resize must succeed, got {status} with body {body}"
    );
    poll_until_state(
        &router,
        "a version past the maximum size",
        &narrowed_id,
        MODIFY_COMPLETE,
    )
    .await;
    let (status, body) = restore(
        &router,
        &restore_params(
            &narrowed_id,
            FIRST_PREFIX_LIST_VERSION + 1,
            FIRST_PREFIX_LIST_VERSION,
        ),
    )
    .await;
    assert_eq!(
        status, 400,
        "a version past the maximum size: expected HTTP 400, got {status} with body {body}"
    );
    assert_eq!(
        error_code(&body),
        "InvalidParameterValue",
        "a version past the maximum size: expected InvalidParameterValue, got {body}"
    );
    let still_narrow = entries_at(
        &router,
        "a version past the maximum size",
        &narrowed_id,
        None,
    )
    .await;
    assert_eq!(
        still_narrow,
        expected_entries(&narrow),
        "a version past the maximum size: the refused restore must leave the entry set alone, \
         got {still_narrow:?}"
    );

    // A list whose creation is still owed to the clock has no settled entry set for a restore
    // to supersede: created and restored with nothing in between, so the state it is refused
    // for is the one its create answered.
    let still_creating_id = create_list(
        &router,
        "a list still being created",
        "restore-creating-list",
        PREFIX_LIST_ADDRESS_FAMILY_IPV4,
        2,
        &[],
    )
    .await;
    let (status, body) = restore(
        &router,
        &restore_params(
            &still_creating_id,
            FIRST_PREFIX_LIST_VERSION,
            FIRST_PREFIX_LIST_VERSION,
        ),
    )
    .await;
    assert_eq!(
        status, 400,
        "a list still being created: expected HTTP 400, got {status} with body {body}"
    );
    assert_eq!(
        error_code(&body),
        "IncorrectState",
        "a list still being created: expected IncorrectState, got {body}"
    );
    assert!(
        error_message(&body).contains(PREFIX_LIST_STATE_CREATE_IN_PROGRESS),
        "a list still being created: expected the message to name the state it is in, got {body}"
    );

    // A prefix list Amazon Web Services publishes for a service of this Region: visible to the
    // caller, never revised, and so with no earlier version of its own to restore.
    let (status, published_body) =
        super::ec2_query::call(&router, REGION, "DescribePrefixLists", &[]).await;
    assert_eq!(
        status, 200,
        "the published lists must be reported, got {status} with body {published_body}"
    );
    let published_id = elements(&published_body, "prefixListId")
        .into_iter()
        .next()
        .expect("the Region must publish a prefix list for the AWS-managed row");

    // The version the list under test stands at: the create minted the first and the revision
    // of its entry set the second.
    let current_version = FIRST_PREFIX_LIST_VERSION + 1;
    let rows: Vec<RefusalRow> = vec![
        (
            "an id that is no prefix list id".to_string(),
            restore_params("vpc-0a1b2c3d", current_version, FIRST_PREFIX_LIST_VERSION),
            400,
            "InvalidPrefixListId.Malformed",
        ),
        (
            "a well-formed id nothing the caller can see carries".to_string(),
            restore_params("pl-0a1b2c3d", current_version, FIRST_PREFIX_LIST_VERSION),
            400,
            "InvalidPrefixListID.NotFound",
        ),
        (
            "a prefix list Amazon Web Services publishes".to_string(),
            restore_params(&published_id, current_version, FIRST_PREFIX_LIST_VERSION),
            400,
            "UnsupportedOperation",
        ),
        (
            "a current version that is not the one the list stands at".to_string(),
            restore_params(
                &prefix_list_id,
                current_version + 5,
                FIRST_PREFIX_LIST_VERSION,
            ),
            400,
            "IncorrectState",
        ),
        (
            "a previous version the list has never had".to_string(),
            restore_params(&prefix_list_id, current_version, current_version + 9),
            400,
            "InvalidParameterValue",
        ),
        (
            "a previous version below the first one a list can have".to_string(),
            restore_params(&prefix_list_id, current_version, 0),
            400,
            "InvalidParameterValue",
        ),
        (
            "the version the list already stands at".to_string(),
            restore_params(&prefix_list_id, current_version, current_version),
            400,
            "InvalidParameterValue",
        ),
        (
            "a dry run".to_string(),
            {
                let mut params =
                    restore_params(&prefix_list_id, current_version, FIRST_PREFIX_LIST_VERSION);
                params.push(param("DryRun", "true"));
                params
            },
            412,
            "DryRunOperation",
        ),
    ];

    for (case, params, expected_status, expected_code) in &rows {
        let (status, body) = restore(&router, params).await;
        assert_eq!(
            status, *expected_status,
            "case {case:?}: expected HTTP {expected_status}, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            *expected_code,
            "case {case:?}: expected the code {expected_code}, got {body}"
        );
        assert!(
            !error_message(&body).is_empty(),
            "case {case:?}: a refusal must say what was wrong, got {body}"
        );
        // Nothing the rows asked for was applied: the list still stands at the version, the
        // entry set and the state the revision before them left it in.
        let stored = engine.state::<Ec2State>();
        let guard = stored.lock();
        let record = guard
            .managed_prefix_lists
            .get(&prefix_list_id)
            .unwrap_or_else(|| panic!("case {case:?}: the list under test must still be stored"));
        assert_eq!(
            record.view.version,
            Some(current_version),
            "case {case:?}: a refused request must not move the version, got {version:?}",
            version = record.view.version
        );
        assert_eq!(
            record.view.state.as_deref(),
            Some(MODIFY_COMPLETE),
            "case {case:?}: a refused request must not move the state, got {state:?}",
            state = record.view.state
        );
        let stored_blocks: Vec<Option<String>> = record
            .entries
            .iter()
            .map(|entry| entry.cidr.clone())
            .collect();
        assert_eq!(
            stored_blocks,
            vec![Some("10.42.0.0/16".to_string())],
            "case {case:?}: a refused request must not touch the entries, got {stored_blocks:?}"
        );
    }
}

/// Rule 3, the WORDING of one refusal class: an id no prefix list the caller can see carries
/// is refused HTTP 400 `InvalidPrefixListID.NotFound`, and EC2 does not word that code one way
/// for every operation — this one, which RESOLVES the list the caller named, answers the
/// sentence the describe, the entry report and the delete of a list answer, `The prefix list
/// (<id>) does not exist.`, carrying only the id the request named.
#[tokio::test]
async fn an_unresolvable_prefix_list_id_is_refused_with_the_resolving_wording() {
    let (router, _engine) = super::ec2_query::emulator();

    // Both id forms EC2 mints for a prefix list — the short one and the long one — neither of
    // which this emulator holds a list under.
    for unknown in ["pl-0a1b2c3d", "pl-00000000000000000"] {
        let (status, body) = restore(
            &router,
            &restore_params(unknown, FIRST_PREFIX_LIST_VERSION + 1, FIRST_PREFIX_LIST_VERSION),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {unknown:?}: expected HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            "InvalidPrefixListID.NotFound",
            "case {unknown:?}: expected InvalidPrefixListID.NotFound, got {body}"
        );
        let expected = format!("The prefix list ({unknown}) does not exist.");
        assert_eq!(
            error_message(&body),
            expected,
            "case {unknown:?}: expected the message {expected:?}, got {actual:?}",
            actual = error_message(&body)
        );
    }
}

/// Rule 4: a dry run that would have succeeded changes nothing, and the restore that follows it
/// is the one that does.
#[tokio::test]
async fn a_dry_run_changes_nothing_and_the_restore_after_it_does() {
    let (router, _engine) = super::ec2_query::emulator();

    let first = [("10.50.0.0/16", Some("the first set"))];
    let second = [("10.51.0.0/16", None)];
    let prefix_list_id = create_list(
        &router,
        "a dry run",
        "restore-dry-run-list",
        PREFIX_LIST_ADDRESS_FAMILY_IPV4,
        2,
        &first,
    )
    .await;
    poll_until_state(
        &router,
        "a dry run",
        &prefix_list_id,
        PREFIX_LIST_STATE_CREATE_COMPLETE,
    )
    .await;
    replace_entries(&router, "a dry run", &prefix_list_id, &first, &second).await;
    poll_until_state(&router, "a dry run", &prefix_list_id, MODIFY_COMPLETE).await;

    let mut params = restore_params(
        &prefix_list_id,
        FIRST_PREFIX_LIST_VERSION + 1,
        FIRST_PREFIX_LIST_VERSION,
    );
    params.push(param("DryRun", "true"));
    let (status, body) = restore(&router, &params).await;
    assert_eq!(
        status, 412,
        "a dry run: expected HTTP 412, got {status} with body {body}"
    );
    assert_eq!(
        error_code(&body),
        "DryRunOperation",
        "a dry run: expected DryRunOperation, got {body}"
    );
    let after_dry_run = entries_at(&router, "after the dry run", &prefix_list_id, None).await;
    assert_eq!(
        after_dry_run,
        expected_entries(&second),
        "after the dry run: expected the entry set the revision left, got {after_dry_run:?}"
    );
    let described = describe_one(&router, "after the dry run", &prefix_list_id).await;
    assert_eq!(
        element(&described, "version"),
        Some((FIRST_PREFIX_LIST_VERSION + 1).to_string()),
        "after the dry run: the version must stand where it was, got {described}"
    );
    assert_eq!(
        element(&described, "state").as_deref(),
        Some(MODIFY_COMPLETE),
        "after the dry run: the state must stand where it was, got {described}"
    );

    let (status, applied) = restore(
        &router,
        &restore_params(
            &prefix_list_id,
            FIRST_PREFIX_LIST_VERSION + 1,
            FIRST_PREFIX_LIST_VERSION,
        ),
    )
    .await;
    assert_eq!(
        status, 200,
        "the restore after the dry run: expected HTTP 200, got {status} with body {applied}"
    );
    let reported = entries_at(&router, "after the restore", &prefix_list_id, None).await;
    assert_eq!(
        reported,
        expected_entries(&first),
        "after the restore: expected the restored entry set, got {reported:?}"
    );
}
