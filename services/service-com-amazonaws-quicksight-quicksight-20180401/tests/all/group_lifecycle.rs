// SPDX-License-Identifier: CC0-1.0

//! Groups: the create and the read that reports it, end to end over HTTP.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` with a signed
//! restJson1 request — so what is asserted is what a QuickSight SDK client would see:
//! the route, the status, and a body the handler computed from the shared store.
//!
//! The rule under test: a QuickSight group is identified by
//! `(AwsAccountId, Namespace, GroupName)` and belongs to a namespace the account holds;
//! `CreateGroup` persists it, answers HTTP 201 for the group it created and reports that
//! group — its ARN, its name, the description it was given and the `PrincipalId` that
//! names it inside its namespace — and `DescribeGroup` answers from that same record. A
//! name the namespace already holds is refused as an existing resource, and a namespace
//! the account does not hold is not found. So the rows below vary what the rule
//! generalises over (the group name's form and length, whether a description is
//! submitted and how long it is, the namespace, and the account) rather than re-running
//! one recorded call.
//!
//! The update cases further down are the same rule read through a write that is not the
//! create: an accepted `UpdateGroup` writes the `Description` it submitted to that
//! stored record, so the next `DescribeGroup` reports it.
//!
//! The membership cases at the end of this suite are about a group's MEMBERS — the
//! users it holds — which are reached through the group they belong to, so they are
//! driven here beside the group they hang on. The role-membership cases after them are
//! the other way round: a ROLE's members are groups, so they too are driven beside the
//! group they name.

use percent_encoding::{utf8_percent_encode, AsciiSet, CONTROLS};
use serde_json::{json, Value};

use service_com_amazonaws_quicksight_quicksight_20180401::state as qs_state;

use super::data_source_data_set_lifecycle::Harness;
use super::namespace_lifecycle::{create_namespace_in, settle_creation};
use super::user_lifecycle::{register_user, sign_up_for_quicksight_identities};
use super::user_not_found_message::{
    deleted_message as deleted_user_message, delete_namespace, delete_user,
    locator as user_locator,
};

/// The account most rows use.
const ACCOUNT: &str = "000000000000";
/// A second, unrelated 12-digit account id, for the rows about account keying.
const OTHER_ACCOUNT: &str = "210987654321";
/// The region the harness signs for; it is the region in every ARN below.
const REGION: &str = "us-west-2";
/// The namespace every account already has, and the one a caller with no custom
/// namespace addresses.
/// Source: smithy_query(query="operation", operation="CreateGroup") — `Namespace` is a
/// required label.
const DEFAULT_NAMESPACE: &str = "default";
/// A namespace this suite creates, so a row can show the namespace is part of a group's
/// identity rather than a filter applied afterwards.
const CUSTOM_NAMESPACE: &str = "sales-ns";

/// The bytes a group name is percent-encoded over when it goes into the URI path label
/// `DescribeGroup` addresses. `GroupName` admits every character in `\u{20}..\u{FF}`
/// (pattern `^[\u0020-\u00FF]+$`), so a name can hold a space or a non-ASCII letter.
/// Source: smithy_query(query="shape", shape="GroupName").
const GROUP_NAME_LABEL: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'%')
    .add(b'/')
    .add(b'?')
    .add(b'#')
    .add(b'+');

/// The group name as the URI path label carries it.
///
/// `pub(crate)` because every suite that addresses a group by name needs it: the label
/// is percent-encoded over the bytes the pattern admits but a URI does not.
pub(crate) fn group_label(group_name: &str) -> String {
    utf8_percent_encode(group_name, GROUP_NAME_LABEL).to_string()
}

/// The ARN a group created under `(account, namespace, group_name)` in [`REGION`]
/// carries.
fn group_arn(account: &str, namespace: &str, group_name: &str) -> String {
    format!("arn:aws:quicksight:{REGION}:{account}:group/{namespace}/{group_name}")
}

/// `CreateGroup`: `POST /accounts/{AwsAccountId}/namespaces/{Namespace}/groups`.
/// Source: smithy_query(query="operation", operation="CreateGroup").
async fn create_group(
    harness: &Harness,
    account: &str,
    namespace: &str,
    body: Value,
) -> (u16, Value) {
    harness
        .call(
            "POST",
            &format!("/accounts/{account}/namespaces/{namespace}/groups"),
            Some(body),
        )
        .await
}

/// `DescribeGroup`:
/// `GET /accounts/{AwsAccountId}/namespaces/{Namespace}/groups/{GroupName}`.
/// Source: smithy_query(query="operation", operation="DescribeGroup").
async fn describe_group(
    harness: &Harness,
    account: &str,
    namespace: &str,
    group_name: &str,
) -> (u16, Value) {
    harness
        .call(
            "GET",
            &format!(
                "/accounts/{account}/namespaces/{namespace}/groups/{}",
                group_label(group_name)
            ),
            None,
        )
        .await
}

/// `UpdateGroup`:
/// `PUT /accounts/{AwsAccountId}/namespaces/{Namespace}/groups/{GroupName}`.
/// Source: smithy_query(query="operation", operation="UpdateGroup").
async fn update_group(
    harness: &Harness,
    account: &str,
    namespace: &str,
    group_name: &str,
    body: Value,
) -> (u16, Value) {
    harness
        .call(
            "PUT",
            &format!(
                "/accounts/{account}/namespaces/{namespace}/groups/{}",
                group_label(group_name)
            ),
            Some(body),
        )
        .await
}

/// Create the namespace the rows that do not use the default one put their groups in.
async fn create_custom_namespace(harness: &Harness, account: &str) {
    let (status, body) = create_namespace_in(
        harness,
        REGION,
        account,
        json!({ "Namespace": CUSTOM_NAMESPACE, "IdentityStore": "QUICKSIGHT" }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the namespace the group rows need was not created: {status} with {body}"
    );
}

/// One row of the create-then-read table.
struct Row {
    /// The case label every assertion names.
    case: &'static str,
    account: &'static str,
    namespace: &'static str,
    /// The `GroupName` the create names; the model admits len 1.. over the pattern
    /// `^[\u0020-\u00FF]+$`.
    /// Source: smithy_query(query="operation", operation="CreateGroup").
    group_name: String,
    /// The `Description` the create submits, if any; the model admits len 1..512.
    description: Option<String>,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "plain-name-with-description",
            account: ACCOUNT,
            namespace: DEFAULT_NAMESPACE,
            group_name: "Sales".to_string(),
            description: Some("Sales team".to_string()),
        },
        Row {
            // The pattern admits `\u0020`, so a name can hold spaces.
            case: "name-with-spaces-and-no-description",
            account: ACCOUNT,
            namespace: DEFAULT_NAMESPACE,
            group_name: "Sales Management East".to_string(),
            description: None,
        },
        Row {
            case: "punctuated-name",
            account: ACCOUNT,
            namespace: DEFAULT_NAMESPACE,
            group_name: "sales-eu_west.2:readers".to_string(),
            description: Some("Readers in eu-west-2".to_string()),
        },
        Row {
            // The pattern reaches `\u00FF`, so a name can hold a Latin-1 letter.
            case: "non-ascii-name",
            account: ACCOUNT,
            namespace: DEFAULT_NAMESPACE,
            group_name: "Ventes-Café".to_string(),
            description: Some("Équipe des ventes".to_string()),
        },
        Row {
            // The shortest name and the shortest description the model admits.
            case: "single-character-name-and-description",
            account: ACCOUNT,
            namespace: DEFAULT_NAMESPACE,
            group_name: "g".to_string(),
            description: Some("d".to_string()),
        },
        Row {
            // The longest description the model admits: `GroupDescription` is len 1..512.
            case: "long-name-and-longest-description",
            account: ACCOUNT,
            namespace: DEFAULT_NAMESPACE,
            group_name: "g".repeat(128),
            description: Some("d".repeat(512)),
        },
        Row {
            // Same name as the first row, in another namespace of the same account.
            case: "custom-namespace",
            account: ACCOUNT,
            namespace: CUSTOM_NAMESPACE,
            group_name: "Sales".to_string(),
            description: Some("Sales team of the sales namespace".to_string()),
        },
        Row {
            // Same name again, in another account's default namespace.
            case: "other-account",
            account: OTHER_ACCOUNT,
            namespace: DEFAULT_NAMESPACE,
            group_name: "Sales".to_string(),
            description: None,
        },
    ]
}

impl Row {
    fn body(&self) -> Value {
        let mut body = json!({ "GroupName": self.group_name });
        if let Some(description) = &self.description {
            body["Description"] = json!(description);
        }
        body
    }

    /// The `Description` a read of this group reports: the one submitted, or none at
    /// all when the create named none.
    fn reported_description(&self) -> Value {
        match &self.description {
            Some(description) => json!(description),
            None => Value::Null,
        }
    }
}

#[tokio::test]
async fn create_group_persists_it_and_describe_reports_it() {
    let harness = Harness::new();
    create_custom_namespace(&harness, ACCOUNT).await;

    for row in rows() {
        let case = row.case;
        let group_name = row.group_name.as_str();
        let namespace = row.namespace;
        let expected_arn = group_arn(row.account, namespace, group_name);

        let (status, body) = create_group(&harness, row.account, namespace, row.body()).await;
        assert_eq!(
            status, 201,
            "case {case:?}: create expected 201, got {status} with body {body}"
        );
        let created = &body["Group"];
        assert_eq!(
            created["Arn"],
            json!(expected_arn),
            "case {case:?}: create reported the wrong Arn: {body}"
        );
        assert_eq!(
            created["GroupName"],
            json!(group_name),
            "case {case:?}: create reported the wrong GroupName: {body}"
        );
        assert_eq!(
            created["Description"],
            row.reported_description(),
            "case {case:?}: create reported the wrong Description: {body}"
        );
        let principal_id = created["PrincipalId"].as_str().unwrap_or_default().to_string();
        assert!(
            !principal_id.is_empty(),
            "case {case:?}: create reported no PrincipalId: {body}"
        );
        assert!(
            body["RequestId"]
                .as_str()
                .is_some_and(|request_id| !request_id.is_empty()),
            "case {case:?}: create reported no RequestId: {body}"
        );

        // The create wrote the group to the shared store, keyed by the account, the
        // namespace and the name, so the reads have a record to answer from.
        {
            let state = qs_state::service_state(&*harness.engine).expect("the QuickSight store");
            let store = state.lock();
            let record = store
                .group(row.account, namespace, group_name)
                .unwrap_or_else(|| panic!("case {case:?}: the created group is not stored"));
            assert_eq!(
                record.arn, expected_arn,
                "case {case:?}: the stored group carries the wrong Arn"
            );
            assert_eq!(
                record.description, row.description,
                "case {case:?}: the submitted Description is not stored as given"
            );
            assert_eq!(
                record.principal_id, principal_id,
                "case {case:?}: the stored PrincipalId is not the one the create reported"
            );
        }

        // Two reads of the same group report the same thing: a read answers from the
        // stored record and changes nothing.
        for attempt in ["first-read", "second-read"] {
            let (status, body) = describe_group(&harness, row.account, namespace, group_name).await;
            assert_eq!(
                status, 200,
                "case {case:?}/{attempt}: describe expected 200, got {status} with {body}"
            );
            let reported = &body["Group"];
            assert_eq!(
                reported["Arn"],
                json!(expected_arn),
                "case {case:?}/{attempt}: describe reported the wrong Arn: {body}"
            );
            assert_eq!(
                reported["GroupName"],
                json!(group_name),
                "case {case:?}/{attempt}: describe reported the wrong GroupName: {body}"
            );
            assert_eq!(
                reported["Description"],
                row.reported_description(),
                "case {case:?}/{attempt}: describe reported the wrong Description: {body}"
            );
            assert_eq!(
                reported["PrincipalId"],
                json!(principal_id),
                "case {case:?}/{attempt}: the PrincipalId must not change between \
                 reads: {body}"
            );
        }

        // A second create of the same name in the same namespace is refused: the name
        // belongs to the group holding it.
        let (status, body) = create_group(&harness, row.account, namespace, row.body()).await;
        assert_eq!(
            status, 409,
            "case {case:?}: re-creating the same group expected 409, got {status} with {body}"
        );
        assert_eq!(
            body["__type"],
            json!("ResourceExistsException"),
            "case {case:?}: a name already in use names an existing resource: {body}"
        );
        assert_eq!(
            body["Message"],
            json!(format!(
                "Group {group_name} already exists. AWS account id: {}, groupName: \
                 {group_name}. ",
                row.account
            )),
            "case {case:?}: the refusal must name the group and its account: {body}"
        );
        assert_eq!(
            body["ResourceType"],
            json!("GROUP"),
            "case {case:?}: the refusal is about a group: {body}"
        );

        // And the group the first create stored is untouched by the refusal.
        let (status, body) = describe_group(&harness, row.account, namespace, group_name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the refused create must not disturb the group: {status} with {body}"
        );
        assert_eq!(
            body["Group"]["PrincipalId"],
            json!(principal_id),
            "case {case:?}: the stored group is still the one the first create minted: {body}"
        );
    }
}

#[tokio::test]
async fn group_identity_is_the_account_the_namespace_and_the_name() {
    let harness = Harness::new();
    create_custom_namespace(&harness, ACCOUNT).await;
    let shared = "Shared Group";

    // The same name in three places: two namespaces of one account, and another
    // account's default namespace.
    let places = [
        ("same-account-default-namespace", ACCOUNT, DEFAULT_NAMESPACE),
        ("same-account-custom-namespace", ACCOUNT, CUSTOM_NAMESPACE),
        ("other-account", OTHER_ACCOUNT, DEFAULT_NAMESPACE),
    ];

    let mut principal_ids = Vec::new();
    for (case, account, namespace) in places {
        let (status, body) = create_group(
            &harness,
            account,
            namespace,
            json!({ "GroupName": shared, "Description": case }),
        )
        .await;
        assert_eq!(
            status, 201,
            "case {case:?}: one group of that name per namespace, so this create \
             expected 201, got {status} with {body}"
        );
        assert_eq!(
            body["Group"]["Arn"],
            json!(group_arn(account, namespace, shared)),
            "case {case:?}: the ARN must carry the owning account and namespace: {body}"
        );
        principal_ids.push(
            body["Group"]["PrincipalId"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        );
    }

    for (index, (case, account, namespace)) in places.iter().enumerate() {
        let (status, body) = describe_group(&harness, account, namespace, shared).await;
        assert_eq!(
            status, 200,
            "case {case:?}: describe expected 200, got {status} with {body}"
        );
        assert_eq!(
            body["Group"]["Description"],
            json!(case),
            "case {case:?}: describe must report that namespace's own group: {body}"
        );
        assert_eq!(
            body["Group"]["PrincipalId"],
            json!(principal_ids[index]),
            "case {case:?}: each group has its own PrincipalId: {body}"
        );
    }

    // Each of the three is a group of its own, and a principal id names the group
    // inside its NAMESPACE: the two in different namespaces are named differently, and
    // the two addressing the same namespace of different accounts are named the same —
    // what carries the account is the ARN, asserted above.
    assert_ne!(
        principal_ids[0], principal_ids[1],
        "case {:?}/{:?}: two namespaces name their groups differently",
        places[0].0, places[1].0
    );
    assert_ne!(
        principal_ids[1], principal_ids[2],
        "case {:?}/{:?}: two namespaces name their groups differently",
        places[1].0, places[2].0
    );
    assert_eq!(
        principal_ids[0], principal_ids[2],
        "case {:?}/{:?}: one name in one namespace is one principal id, whichever \
         account holds it",
        places[0].0, places[2].0
    );

    // A name no group in that namespace is held under is not found.
    for (case, account, namespace, group_name) in [
        (
            "unused-name",
            ACCOUNT,
            DEFAULT_NAMESPACE,
            "No Such Group".to_string(),
        ),
        (
            "name-used-in-another-namespace-only",
            ACCOUNT,
            CUSTOM_NAMESPACE,
            "Sales".to_string(),
        ),
        (
            "account-that-created-nothing",
            "123456789012",
            DEFAULT_NAMESPACE,
            shared.to_string(),
        ),
    ] {
        let (status, body) = describe_group(&harness, account, namespace, &group_name).await;
        assert_eq!(
            status, 404,
            "case {case:?}: a group that was never created cannot be described, got \
             {status} with {body}"
        );
        assert_eq!(
            body["__type"],
            json!("ResourceNotFoundException"),
            "case {case:?}: a name no group is held under is not found: {body}"
        );
        assert_eq!(
            body["Message"],
            json!(group_miss_message(account, namespace, &group_name)),
            "case {case:?}: the refusal must name what was not found: {body}"
        );
        assert_eq!(
            body["ResourceType"],
            json!("GROUP"),
            "case {case:?}: the refusal is about a group: {body}"
        );
    }
}

#[tokio::test]
async fn a_group_needs_a_namespace_the_account_holds() {
    let harness = Harness::new();

    for (case, account, namespace) in [
        ("never-created", ACCOUNT, "no-such-namespace"),
        // A namespace created under one account is no namespace of another.
        ("created-under-another-account", OTHER_ACCOUNT, CUSTOM_NAMESPACE),
    ] {
        if case == "created-under-another-account" {
            create_custom_namespace(&harness, ACCOUNT).await;
        }
        let (status, body) =
            create_group(&harness, account, namespace, json!({ "GroupName": "Sales" })).await;
        assert_eq!(
            status, 404,
            "case {case:?}: a group needs a namespace to live in, got {status} with {body}"
        );
        assert_eq!(
            body["__type"],
            json!("ResourceNotFoundException"),
            "case {case:?}: a namespace the account does not hold is not found: {body}"
        );
        assert_eq!(
            body["Message"],
            json!(format!("Namespace {namespace} not found in account {account}.")),
            "case {case:?}: the refusal must name the namespace: {body}"
        );
        assert_eq!(
            body["ResourceType"],
            json!("NAMESPACE"),
            "case {case:?}: the refusal is about a namespace: {body}"
        );

        // Nothing was stored for the refused create.
        let state = qs_state::service_state(&*harness.engine).expect("the QuickSight store");
        let store = state.lock();
        assert!(
            store.group(account, namespace, "Sales").is_none(),
            "case {case:?}: a refused create must persist nothing"
        );
    }
}

// --- A group's members ------------------------------------------------------------
//
// The rule under test: a group membership joins a group to a user of the SAME namespace
// and is identified by `(AwsAccountId, Namespace, GroupName, MemberName)`;
// `CreateGroupMembership` persists it and reports the member — the member's name and the
// ARN of the user it names — `DescribeGroupMembership` answers from that same record, and
// neither a group nor a user the namespace does not hold can be joined. The rows vary
// what the rule generalises over (the member name's form and length, the group, the
// namespace and the account) rather than re-running one recorded call.

/// The address of one membership, as the model binds both the create and the read:
/// `.../namespaces/{Namespace}/groups/{GroupName}/members/{MemberName}`. `GroupName` and
/// `GroupMemberName` carry the same pattern, so both labels are percent-encoded over the
/// same bytes ([`group_label`]).
/// Source: smithy_query(query="operation",
/// operation="CreateGroupMembership,DescribeGroupMembership").
fn membership_path(account: &str, namespace: &str, group_name: &str, member_name: &str) -> String {
    format!(
        "/accounts/{account}/namespaces/{namespace}/groups/{}/members/{}",
        group_label(group_name),
        group_label(member_name)
    )
}

/// `CreateGroupMembership`: `PUT` on the membership's own address.
async fn add_group_member(
    harness: &Harness,
    account: &str,
    namespace: &str,
    group_name: &str,
    member_name: &str,
) -> (u16, Value) {
    harness
        .call(
            "PUT",
            &membership_path(account, namespace, group_name, member_name),
            None,
        )
        .await
}

/// `DescribeGroupMembership`: `GET` on that same address.
async fn describe_group_member(
    harness: &Harness,
    account: &str,
    namespace: &str,
    group_name: &str,
    member_name: &str,
) -> (u16, Value) {
    harness
        .call(
            "GET",
            &membership_path(account, namespace, group_name, member_name),
            None,
        )
        .await
}

/// The ARN a member reports: the one the user it names carries, since a group's members
/// are users.
/// Source: smithy_query(query="shape", shape="GroupMember") — `Arn` is "the Amazon
/// Resource Name (ARN) for the group member (user)".
fn member_arn(account: &str, namespace: &str, member_name: &str) -> String {
    format!("arn:aws:quicksight:{REGION}:{account}:user/{namespace}/{member_name}")
}

/// What a membership read reports for a member the group does not hold: the identity
/// path's locator over the four path labels the call carried, ending in a period and a
/// space.
fn membership_locator(
    account: &str,
    namespace: &str,
    group_name: &str,
    member_name: &str,
) -> String {
    format!(
        "AWS account id: {account}, Namespace: {namespace}, groupName: {group_name}, \
         memberName: {member_name}. "
    )
}

/// What a group operation reports for a `GroupName` the namespace holds no group under.
fn group_miss_message(account: &str, namespace: &str, group_name: &str) -> String {
    format!("Group {group_name} not found in namespace {namespace} in account {account}.")
}

/// Create [`CUSTOM_NAMESPACE`] under `account` and bring its creation transition forward,
/// so it is a place a user can be registered in as well as a place a group can live.
async fn create_settled_custom_namespace(harness: &Harness, account: &str) {
    create_custom_namespace(harness, account).await;
    settle_creation(harness, account, CUSTOM_NAMESPACE);
}

/// Create the group a membership case joins its member to.
async fn given_group(harness: &Harness, case: &str, account: &str, namespace: &str, name: &str) {
    let (status, body) = create_group(harness, account, namespace, json!({ "GroupName": name })).await;
    assert_eq!(
        status, 201,
        "case {case:?}: the fixture group {name:?} expected 201, got {status} with {body}"
    );
}

/// Register the user a membership case names as its member.
async fn given_user(harness: &Harness, case: &str, account: &str, namespace: &str, name: &str) {
    let (status, body) = register_user(
        harness,
        account,
        namespace,
        json!({
            "IdentityType": "QUICKSIGHT",
            "Email": "member@example.com",
            "UserRole": "READER",
            "UserName": name,
        }),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: the fixture user {name:?} expected 201, got {status} with {body}"
    );
}

/// How many memberships the store holds.
fn held_memberships(harness: &Harness) -> usize {
    let state = qs_state::service_state(&*harness.engine).expect("the QuickSight store");
    let store = state.lock();
    store.group_members.len()
}

/// Fill the store up to the total allowance this emulator holds memberships under, with
/// memberships of groups no case addresses, so the next NEW membership meets that
/// ceiling.
///
/// The ceiling bounds what the PROCESS holds, so it is reached by filling the store
/// rather than by driving a hundred thousand calls that would say nothing more; the
/// refusal itself is then asserted over the wire.
fn fill_memberships_to_the_ceiling(harness: &Harness) {
    let state = qs_state::service_state(&*harness.engine).expect("the QuickSight store");
    let mut store = state.lock();
    let mut filler = 0usize;
    while store.group_members.len() < qs_state::MAX_GROUP_MEMBERS {
        store.group_members.insert(
            (
                ACCOUNT.to_string(),
                DEFAULT_NAMESPACE.to_string(),
                format!("filler-group-{filler}"),
                "filler-member".to_string(),
            ),
            qs_state::GroupMemberRecord {
                member_name: "filler-member".to_string(),
                arn: member_arn(ACCOUNT, DEFAULT_NAMESPACE, "filler-member"),
            },
        );
        filler += 1;
    }
}

/// One row of the add-then-read table.
struct MemberRow {
    case: &'static str,
    account: &'static str,
    namespace: &'static str,
    /// The group the member is added to; `GroupName` is len 1.. over the pattern
    /// `^[\u0020-\u00FF]+$`.
    group_name: String,
    /// The user added to it; `GroupMemberName` carries the same pattern, bounded at 256.
    /// Source: smithy_query(query="shape", shape="GroupMemberName").
    member_name: String,
}

fn member_rows() -> Vec<MemberRow> {
    vec![
        MemberRow {
            case: "plain-names",
            account: ACCOUNT,
            namespace: DEFAULT_NAMESPACE,
            group_name: "Sales".to_string(),
            member_name: "analyst-1".to_string(),
        },
        MemberRow {
            // The pattern admits `\u0020`, so both names can hold spaces.
            case: "names-with-spaces",
            account: ACCOUNT,
            namespace: DEFAULT_NAMESPACE,
            group_name: "Sales Management East".to_string(),
            member_name: "Jane Doe".to_string(),
        },
        MemberRow {
            // The pattern reaches `\u00FF`, so both can hold a Latin-1 letter.
            case: "non-ascii-names",
            account: ACCOUNT,
            namespace: DEFAULT_NAMESPACE,
            group_name: "Ventes-Café".to_string(),
            member_name: "Émile Zola".to_string(),
        },
        MemberRow {
            // The shortest names the model admits.
            case: "single-character-names",
            account: ACCOUNT,
            namespace: DEFAULT_NAMESPACE,
            group_name: "g".to_string(),
            member_name: "u".to_string(),
        },
        MemberRow {
            // The longest member name the model admits: `GroupMemberName` is len 1..256,
            // and a `UserName` of that length satisfies its own len 1.. bound.
            case: "longest-member-name",
            account: ACCOUNT,
            namespace: DEFAULT_NAMESPACE,
            group_name: "Bounded".to_string(),
            member_name: "m".repeat(256),
        },
        MemberRow {
            // The same two names again, in another namespace of the same account.
            case: "custom-namespace",
            account: ACCOUNT,
            namespace: CUSTOM_NAMESPACE,
            group_name: "Sales".to_string(),
            member_name: "analyst-1".to_string(),
        },
        MemberRow {
            // And once more in another account's default namespace.
            case: "other-account",
            account: OTHER_ACCOUNT,
            namespace: DEFAULT_NAMESPACE,
            group_name: "Sales".to_string(),
            member_name: "analyst-1".to_string(),
        },
    ]
}

#[tokio::test]
async fn adding_a_member_persists_the_membership_and_describe_reports_it() {
    let harness = Harness::new();
    // A user QuickSight manages itself needs an account with an identity store of its
    // own, so both accounts are signed up for one.
    sign_up_for_quicksight_identities(&harness, ACCOUNT).await;
    sign_up_for_quicksight_identities(&harness, OTHER_ACCOUNT).await;
    create_settled_custom_namespace(&harness, ACCOUNT).await;

    for row in member_rows() {
        let case = row.case;
        let account = row.account;
        let namespace = row.namespace;
        let group_name = row.group_name.as_str();
        let member_name = row.member_name.as_str();
        let expected_arn = member_arn(account, namespace, member_name);

        given_group(&harness, case, account, namespace, group_name).await;
        given_user(&harness, case, account, namespace, member_name).await;

        let (status, body) =
            add_group_member(&harness, account, namespace, group_name, member_name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: adding the member expected 200, got {status} with {body}"
        );
        assert_eq!(
            body["GroupMember"]["MemberName"],
            json!(member_name),
            "case {case:?}: the add reported the wrong MemberName: {body}"
        );
        assert_eq!(
            body["GroupMember"]["Arn"],
            json!(expected_arn),
            "case {case:?}: a member's Arn is the ARN of the user it names: {body}"
        );
        assert!(
            body["RequestId"]
                .as_str()
                .is_some_and(|request_id| !request_id.is_empty()),
            "case {case:?}: the add reported no RequestId: {body}"
        );

        // The add wrote the membership to the shared store, keyed by the account, the
        // namespace, the group and the member, so the read has a record to answer from.
        let after_first_add = {
            let state = qs_state::service_state(&*harness.engine).expect("the QuickSight store");
            let store = state.lock();
            let record = store
                .group_member(account, namespace, group_name, member_name)
                .unwrap_or_else(|| panic!("case {case:?}: the membership is not stored"));
            assert_eq!(
                record.arn, expected_arn,
                "case {case:?}: the stored membership carries the wrong Arn"
            );
            assert_eq!(
                record.member_name, member_name,
                "case {case:?}: the stored membership carries the wrong MemberName"
            );
            store.group_members.len()
        };

        // Two reads of the same membership report the same thing: a read answers from
        // the stored record and changes nothing.
        for attempt in ["first-read", "second-read"] {
            let (status, body) =
                describe_group_member(&harness, account, namespace, group_name, member_name).await;
            assert_eq!(
                status, 200,
                "case {case:?}/{attempt}: describe expected 200, got {status} with {body}"
            );
            assert_eq!(
                body["GroupMember"]["MemberName"],
                json!(member_name),
                "case {case:?}/{attempt}: describe reported the wrong MemberName: {body}"
            );
            assert_eq!(
                body["GroupMember"]["Arn"],
                json!(expected_arn),
                "case {case:?}/{attempt}: describe reported the wrong Arn: {body}"
            );
        }

        // Adding a member the group already holds reports that same membership: one user
        // is a member of one group once.
        let (status, body) =
            add_group_member(&harness, account, namespace, group_name, member_name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: re-adding a member the group holds expected 200, got {status} \
             with {body}"
        );
        assert_eq!(
            body["GroupMember"]["Arn"],
            json!(expected_arn),
            "case {case:?}: the repeated add reported the wrong Arn: {body}"
        );
        assert_eq!(
            held_memberships(&harness),
            after_first_add,
            "case {case:?}: re-adding a member must not store a second membership"
        );
    }
}

#[tokio::test]
async fn a_membership_needs_the_group_and_the_user_it_joins() {
    let harness = Harness::new();
    sign_up_for_quicksight_identities(&harness, ACCOUNT).await;
    create_settled_custom_namespace(&harness, ACCOUNT).await;

    // A group that holds no member yet, a user that is in no group yet, and a user this
    // service deleted — so the rows are about what the call names rather than about an
    // empty store.
    given_group(&harness, "fixture", ACCOUNT, DEFAULT_NAMESPACE, "Members").await;
    given_user(&harness, "fixture", ACCOUNT, DEFAULT_NAMESPACE, "member-1").await;
    given_user(&harness, "fixture", ACCOUNT, DEFAULT_NAMESPACE, "gone").await;
    let (status, body) = delete_user(&harness, ACCOUNT, DEFAULT_NAMESPACE, "gone").await;
    assert_eq!(
        status, 200,
        "the fixture deletion expected 200, got {status} with {body}"
    );

    // Rows: which half of the join is missing, and in what way — the group name, the
    // namespace and the account the group is looked for in, and the member name's form,
    // including one whose user this service deleted.
    let rows: Vec<(&str, &str, &str, String, String, &str, String)> = vec![
        (
            "a group name nothing created",
            ACCOUNT,
            DEFAULT_NAMESPACE,
            "No Such Group".to_string(),
            "member-1".to_string(),
            "GROUP",
            group_miss_message(ACCOUNT, DEFAULT_NAMESPACE, "No Such Group"),
        ),
        (
            "a group held in another namespace only",
            ACCOUNT,
            CUSTOM_NAMESPACE,
            "Members".to_string(),
            "member-1".to_string(),
            "GROUP",
            group_miss_message(ACCOUNT, CUSTOM_NAMESPACE, "Members"),
        ),
        (
            "a group held in another account only",
            OTHER_ACCOUNT,
            DEFAULT_NAMESPACE,
            "Members".to_string(),
            "member-1".to_string(),
            "GROUP",
            group_miss_message(OTHER_ACCOUNT, DEFAULT_NAMESPACE, "Members"),
        ),
        (
            "a member name nothing registered",
            ACCOUNT,
            DEFAULT_NAMESPACE,
            "Members".to_string(),
            "no-such-user".to_string(),
            "USER",
            user_locator(ACCOUNT, DEFAULT_NAMESPACE, "no-such-user"),
        ),
        (
            "a member name holding a space",
            ACCOUNT,
            DEFAULT_NAMESPACE,
            "Members".to_string(),
            "No Such Member".to_string(),
            "USER",
            user_locator(ACCOUNT, DEFAULT_NAMESPACE, "No Such Member"),
        ),
        (
            "a Latin-1 member name the pattern admits",
            ACCOUNT,
            DEFAULT_NAMESPACE,
            "Members".to_string(),
            "Café Lecteur".to_string(),
            "USER",
            user_locator(ACCOUNT, DEFAULT_NAMESPACE, "Café Lecteur"),
        ),
        (
            "a member whose user this service deleted",
            ACCOUNT,
            DEFAULT_NAMESPACE,
            "Members".to_string(),
            "gone".to_string(),
            "USER",
            deleted_user_message(ACCOUNT, DEFAULT_NAMESPACE, "gone"),
        ),
        (
            "a namespace the account does not hold",
            ACCOUNT,
            "no-such-ns",
            "Members".to_string(),
            "member-1".to_string(),
            "NAMESPACE",
            format!("Namespace no-such-ns not found in account {ACCOUNT}."),
        ),
    ];

    for (case, account, namespace, group_name, member_name, resource_type, message) in rows {
        let (status, body) =
            add_group_member(&harness, account, namespace, &group_name, &member_name).await;
        assert_eq!(
            status, 404,
            "case {case:?}: a membership joins two resources that exist, so this add \
             expected 404, got {status} with {body}"
        );
        assert_eq!(
            body["__type"],
            json!("ResourceNotFoundException"),
            "case {case:?}: a resource that is not there is not found: {body}"
        );
        assert_eq!(
            body["ResourceType"],
            json!(resource_type),
            "case {case:?}: the refusal must name the kind of resource it is about: {body}"
        );
        assert_eq!(
            body["Message"],
            json!(message),
            "case {case:?}: expected Message {message:?}, got {body}"
        );
    }

    // Not one of the refused adds stored anything.
    assert_eq!(
        held_memberships(&harness),
        0,
        "a refused add must persist nothing"
    );
}

#[tokio::test]
async fn a_membership_is_reported_only_where_it_was_created() {
    let harness = Harness::new();
    sign_up_for_quicksight_identities(&harness, ACCOUNT).await;
    sign_up_for_quicksight_identities(&harness, OTHER_ACCOUNT).await;
    create_settled_custom_namespace(&harness, ACCOUNT).await;

    let group = "Shared Group";
    let member = "Shared Member";
    // The same group name and the same member name in three places: two namespaces of
    // one account, and another account's default namespace.
    let places = [
        ("same-account-default-namespace", ACCOUNT, DEFAULT_NAMESPACE),
        ("same-account-custom-namespace", ACCOUNT, CUSTOM_NAMESPACE),
        ("other-account", OTHER_ACCOUNT, DEFAULT_NAMESPACE),
    ];
    for (case, account, namespace) in places {
        given_group(&harness, case, account, namespace, group).await;
        given_user(&harness, case, account, namespace, member).await;
    }
    // A second group of the first place, whose member nothing added.
    given_group(&harness, "other-group", ACCOUNT, DEFAULT_NAMESPACE, "Other Group").await;

    // The member is added in the FIRST place only.
    let (status, body) = add_group_member(&harness, ACCOUNT, DEFAULT_NAMESPACE, group, member).await;
    assert_eq!(
        status, 200,
        "the one add this case makes expected 200, got {status} with {body}"
    );

    // Every other address — another namespace, another account, another group of the
    // same namespace — reports a member that group does not hold, and says so with the
    // locator for the address the call named.
    let misses = [
        ("another-namespace", ACCOUNT, CUSTOM_NAMESPACE, group),
        ("another-account", OTHER_ACCOUNT, DEFAULT_NAMESPACE, group),
        (
            "another-group-of-the-same-namespace",
            ACCOUNT,
            DEFAULT_NAMESPACE,
            "Other Group",
        ),
    ];
    for (case, account, namespace, group_name) in misses {
        let (status, body) =
            describe_group_member(&harness, account, namespace, group_name, member).await;
        assert_eq!(
            status, 404,
            "case {case:?}: a membership is reported only where it was created, got \
             {status} with {body}"
        );
        assert_eq!(
            body["__type"],
            json!("ResourceNotFoundException"),
            "case {case:?}: a member the group does not hold is not found: {body}"
        );
        assert_eq!(
            body["ResourceType"],
            json!("USER"),
            "case {case:?}: a group's members are users: {body}"
        );
        assert_eq!(
            body["Message"],
            json!(membership_locator(account, namespace, group_name, member)),
            "case {case:?}: the refusal must report the address the call named: {body}"
        );
    }

    // And the one membership that was created is still readable.
    let (status, body) =
        describe_group_member(&harness, ACCOUNT, DEFAULT_NAMESPACE, group, member).await;
    assert_eq!(
        status, 200,
        "the created membership must stay readable, got {status} with {body}"
    );
    assert_eq!(
        body["GroupMember"]["Arn"],
        json!(member_arn(ACCOUNT, DEFAULT_NAMESPACE, member)),
        "the membership must report the user of its own namespace: {body}"
    );
    assert_eq!(
        held_memberships(&harness),
        1,
        "exactly one membership was created"
    );
}

#[tokio::test]
async fn a_membership_goes_with_the_user_it_names_and_the_namespace_it_is_in() {
    let harness = Harness::new();
    sign_up_for_quicksight_identities(&harness, ACCOUNT).await;
    create_settled_custom_namespace(&harness, ACCOUNT).await;

    let group = "Doomed Group";
    // Two members, so each removal is shown to take the memberships it owes and no
    // others; the names' forms differ.
    let members = ["doomed", "Doomed Café"];
    given_group(&harness, "doomed", ACCOUNT, CUSTOM_NAMESPACE, group).await;
    for member in members {
        given_user(&harness, member, ACCOUNT, CUSTOM_NAMESPACE, member).await;
        let (status, body) =
            add_group_member(&harness, ACCOUNT, CUSTOM_NAMESPACE, group, member).await;
        assert_eq!(
            status, 200,
            "case {member:?}: the fixture add expected 200, got {status} with {body}"
        );
    }

    // Deleting the user it names takes the membership with it: a group's members are
    // users, so a user that is gone is a member of nothing.
    let (status, body) = delete_user(&harness, ACCOUNT, CUSTOM_NAMESPACE, members[0]).await;
    assert_eq!(
        status, 200,
        "deleting the member's user expected 200, got {status} with {body}"
    );
    let (status, body) =
        describe_group_member(&harness, ACCOUNT, CUSTOM_NAMESPACE, group, members[0]).await;
    assert_eq!(
        status, 404,
        "the membership of a deleted user must be gone, got {status} with {body}"
    );
    let (status, body) =
        describe_group_member(&harness, ACCOUNT, CUSTOM_NAMESPACE, group, members[1]).await;
    assert_eq!(
        status, 200,
        "the other member's membership must be untouched, got {status} with {body}"
    );

    // And deleting the namespace takes the groups it holds, so their memberships go too.
    let (status, body) = delete_namespace(&harness, ACCOUNT, CUSTOM_NAMESPACE).await;
    assert_eq!(
        status, 200,
        "deleting the namespace expected 200, got {status} with {body}"
    );
    for member in members {
        let (status, body) =
            describe_group_member(&harness, ACCOUNT, CUSTOM_NAMESPACE, group, member).await;
        assert_eq!(
            status, 404,
            "case {member:?}: the memberships of a deleted namespace go with it, got \
             {status} with {body}"
        );
    }
    assert_eq!(
        held_memberships(&harness),
        0,
        "the deleted namespace still holds memberships"
    );
}

#[tokio::test]
async fn a_new_membership_past_the_allowance_is_refused_and_a_held_one_still_answers() {
    let harness = Harness::new();
    sign_up_for_quicksight_identities(&harness, ACCOUNT).await;
    given_group(&harness, "allowance", ACCOUNT, DEFAULT_NAMESPACE, "Members").await;
    given_user(&harness, "allowance", ACCOUNT, DEFAULT_NAMESPACE, "member-1").await;
    given_user(&harness, "allowance", ACCOUNT, DEFAULT_NAMESPACE, "member-2").await;

    let (status, body) =
        add_group_member(&harness, ACCOUNT, DEFAULT_NAMESPACE, "Members", "member-1").await;
    assert_eq!(
        status, 200,
        "the membership held before the allowance runs out expected 200, got {status} \
         with {body}"
    );
    fill_memberships_to_the_ceiling(&harness);

    // Adding a member the group already holds asks for no further room, so it still
    // answers from the store.
    let (status, body) =
        add_group_member(&harness, ACCOUNT, DEFAULT_NAMESPACE, "Members", "member-1").await;
    assert_eq!(
        status, 200,
        "a membership the group already holds must not be refused by the allowance, got \
         {status} with {body}"
    );

    // A membership that is new is refused, and the refusal states the allowance that ran
    // out.
    let (status, body) =
        add_group_member(&harness, ACCOUNT, DEFAULT_NAMESPACE, "Members", "member-2").await;
    assert_eq!(
        status, 503,
        "one past the allowance expected 503, got {status} with {body}"
    );
    assert_eq!(
        body["__type"],
        json!("ResourceUnavailableException"),
        "an allowance this emulator holds memberships under is reported as a resource \
         that is unavailable: {body}"
    );
    assert_eq!(
        body["ResourceType"],
        json!("GROUP"),
        "the exhausted resource is the group the members hang on: {body}"
    );
    assert_eq!(
        body["Message"],
        json!(format!(
            "This QuickSight emulator holds the maximum of {} group memberships.",
            qs_state::MAX_GROUP_MEMBERS
        )),
        "the refusal must state the allowance that ran out: {body}"
    );

    // The refused add stored nothing, and the membership that was held is untouched.
    let (status, body) =
        describe_group_member(&harness, ACCOUNT, DEFAULT_NAMESPACE, "Members", "member-2").await;
    assert_eq!(
        status, 404,
        "the refused membership must not exist, got {status} with {body}"
    );
    let (status, body) =
        describe_group_member(&harness, ACCOUNT, DEFAULT_NAMESPACE, "Members", "member-1").await;
    assert_eq!(
        status, 200,
        "the held membership must survive the refusal, got {status} with {body}"
    );
    assert_eq!(
        held_memberships(&harness),
        qs_state::MAX_GROUP_MEMBERS,
        "the store must hold exactly the allowance's worth of memberships"
    );
}

// --- A role's members -------------------------------------------------------------
//
// The rule under test: a role membership joins one of the service's roles to a GROUP of
// the same namespace and is identified by `(AwsAccountId, Namespace, Role, MemberName)`;
// `CreateRoleMembership` persists it, `ListRoleMemberships` reports the groups one role
// holds by name — in name order, a page at a time — and `DeleteRoleMembership` takes one
// out again. A role is held by a group the account manages itself, so an account whose
// authentication method gives it no QuickSight identity store of its own is denied its
// roles altogether. The rows vary what the rule generalises over (the role, the number
// and form of the group names, the page size, the namespace and the account) rather than
// re-running one recorded call.

/// A third 12-digit account id, for the row whose account takes its identities from a
/// directory.
const DIRECTORY_ACCOUNT: &str = "123456789012";

/// Every member of the model's `Role` enum: the roles a group can be added to, and the
/// only values the `Role` label admits.
/// Source: smithy_query(query="shape", shape="Role").
const ROLES: [&str; 6] = [
    "ADMIN",
    "ADMIN_PRO",
    "AUTHOR",
    "AUTHOR_PRO",
    "READER",
    "READER_PRO",
];

/// The address of one role's membership list:
/// `.../namespaces/{Namespace}/roles/{Role}/members`.
/// Source: smithy_query(query="operation", operation="ListRoleMemberships").
fn role_members_path(account: &str, namespace: &str, role: &str) -> String {
    format!("/accounts/{account}/namespaces/{namespace}/roles/{role}/members")
}

/// The address of one role membership: that list with the group's name under it.
/// `MemberName` targets `GroupName`, so the label is percent-encoded over the same bytes
/// a group's own name is ([`group_label`]).
/// Source: smithy_query(query="operation",
/// operation="CreateRoleMembership,DeleteRoleMembership").
fn role_member_path(account: &str, namespace: &str, role: &str, member_name: &str) -> String {
    format!(
        "{}/{}",
        role_members_path(account, namespace, role),
        group_label(member_name)
    )
}

/// `ListRoleMemberships`: `GET` on a role's membership list, with `query` carrying the
/// pagination parameters the model binds to the query string (`max-results`,
/// `next-token`).
async fn list_role_members(
    harness: &Harness,
    account: &str,
    namespace: &str,
    role: &str,
    query: &str,
) -> (u16, Value) {
    harness
        .call(
            "GET",
            &format!("{}{query}", role_members_path(account, namespace, role)),
            None,
        )
        .await
}

/// `CreateRoleMembership`: `POST` on the membership's own address.
async fn add_role_member(
    harness: &Harness,
    account: &str,
    namespace: &str,
    role: &str,
    member_name: &str,
) -> (u16, Value) {
    harness
        .call(
            "POST",
            &role_member_path(account, namespace, role, member_name),
            None,
        )
        .await
}

/// `DeleteRoleMembership`: `DELETE` on that same address.
async fn remove_role_member(
    harness: &Harness,
    account: &str,
    namespace: &str,
    role: &str,
    member_name: &str,
) -> (u16, Value) {
    harness
        .call(
            "DELETE",
            &role_member_path(account, namespace, role, member_name),
            None,
        )
        .await
}

/// Sign `account` up for QuickSight with `subscription`, the sign-up body that chooses
/// where the account's identities come from.
/// Source: smithy_query(query="operation", operation="CreateAccountSubscription").
async fn sign_up(harness: &Harness, case: &str, account: &str, subscription: Value) {
    let (status, body) = harness
        .call("POST", &format!("/account/{account}"), Some(subscription))
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the sign-up this case needs failed: {status} with {body}"
    );
}

/// How many role memberships the store holds.
fn held_role_memberships(harness: &Harness) -> usize {
    let state = qs_state::service_state(&*harness.engine).expect("the QuickSight store");
    let store = state.lock();
    store.role_memberships.len()
}

/// The group names one `ListRoleMemberships` response reported, in the order it reported
/// them.
fn reported_members(case: &str, body: &Value) -> Vec<String> {
    body["MembersList"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: no MembersList in {body}"))
        .iter()
        .map(|name| name.as_str().unwrap_or_default().to_string())
        .collect()
}

/// What a role-membership call on an account that holds no QuickSight identity store of
/// its own reports: the store that account HAS — which is what forbids the call — and
/// then the identity path's locator for the address the call named, which follows the
/// sentence directly.
fn role_denial_message(identity_store: &str, account: &str, namespace: &str, role: &str) -> String {
    format!(
        "Operation is not supported for {identity_store} identity store.AWS account id: \
         {account}, Namespace: {namespace}, Role: {role}. "
    )
}

/// What a role-membership delete reports for a group the role does not hold: the locator
/// alone, because nothing is known to have existed at that address.
fn role_membership_locator(
    account: &str,
    namespace: &str,
    role: &str,
    member_name: &str,
) -> String {
    format!(
        "AWS account id: {account}, Namespace: {namespace}, Role: {role}, \
         memberName: {member_name}. "
    )
}

#[tokio::test]
async fn a_role_reports_exactly_the_groups_added_to_it() {
    let harness = Harness::new();
    sign_up_for_quicksight_identities(&harness, ACCOUNT).await;
    sign_up_for_quicksight_identities(&harness, OTHER_ACCOUNT).await;
    create_settled_custom_namespace(&harness, ACCOUNT).await;

    // Rows: one per member of the model's `Role` catalog, differing in how many groups
    // the role holds (none, one, several), the form and length of those group names, the
    // namespace the role belongs to and the account it is in.
    let rows: [(&str, &str, &str, &str, &[&str]); 6] = [
        ("one-group", ACCOUNT, DEFAULT_NAMESPACE, "ADMIN", &["Admins"]),
        (
            "three-groups",
            ACCOUNT,
            DEFAULT_NAMESPACE,
            "AUTHOR",
            &["Analysts", "Report Authors", "Zürich Authoren"],
        ),
        ("no-group", ACCOUNT, DEFAULT_NAMESPACE, "READER", &[]),
        (
            "custom-namespace",
            ACCOUNT,
            CUSTOM_NAMESPACE,
            "ADMIN_PRO",
            &["Admins"],
        ),
        (
            "other-account",
            OTHER_ACCOUNT,
            DEFAULT_NAMESPACE,
            "AUTHOR_PRO",
            &["Admins"],
        ),
        (
            "digits-in-the-names",
            ACCOUNT,
            DEFAULT_NAMESPACE,
            "READER_PRO",
            &["team-42", "team-7"],
        ),
    ];

    // Every group a row names has to exist where the row puts it: a role takes an
    // EXISTING group of its own namespace.
    let mut created: Vec<(&str, &str, &str)> = Vec::new();
    for (case, account, namespace, _, members) in rows {
        for member in members {
            if created.contains(&(account, namespace, member)) {
                continue;
            }
            given_group(&harness, case, account, namespace, member).await;
            created.push((account, namespace, member));
        }
    }

    let mut added = 0usize;
    for (case, account, namespace, role, members) in rows {
        for member in members {
            let (status, body) = add_role_member(&harness, account, namespace, role, member).await;
            assert_eq!(
                status, 200,
                "case {case:?}: adding {member:?} to {role} expected 200, got {status} \
                 with {body}"
            );
            assert!(
                body["RequestId"].is_string(),
                "case {case:?}: the add must report a RequestId: {body}"
            );
            added += 1;
        }
    }

    for (case, account, namespace, role, members) in rows {
        let (status, body) = list_role_members(&harness, account, namespace, role, "").await;
        assert_eq!(
            status, 200,
            "case {case:?}: listing {role} expected 200, got {status} with {body}"
        );
        let mut expected: Vec<String> = members.iter().map(|name| name.to_string()).collect();
        expected.sort();
        assert_eq!(
            reported_members(case, &body),
            expected,
            "case {case:?}: {role} must report exactly the groups added to it, in name \
             order: {body}"
        );
        assert_eq!(
            body["NextToken"],
            Value::Null,
            "case {case:?}: a complete page needs no NextToken: {body}"
        );
        assert!(
            body["RequestId"].is_string(),
            "case {case:?}: the listing must report a RequestId: {body}"
        );
    }

    // A membership is held under ONE role of ONE namespace: every other role of the
    // custom namespace reports nothing, though a group of the same name holds a role in
    // the account's default namespace.
    for role in ROLES {
        let (status, body) = list_role_members(&harness, ACCOUNT, CUSTOM_NAMESPACE, role, "").await;
        assert_eq!(
            status, 200,
            "case {role:?}: listing the custom namespace's role expected 200, got \
             {status} with {body}"
        );
        let expected: Vec<String> = if role == "ADMIN_PRO" {
            vec!["Admins".to_string()]
        } else {
            Vec::new()
        };
        assert_eq!(
            reported_members(role, &body),
            expected,
            "case {role:?}: a membership must be reported by its own role only: {body}"
        );
    }

    assert_eq!(
        held_role_memberships(&harness),
        added,
        "the store must hold exactly the memberships the adds created"
    );
}

#[tokio::test]
async fn role_membership_pages_walk_every_member_exactly_once() {
    let harness = Harness::new();
    sign_up_for_quicksight_identities(&harness, ACCOUNT).await;

    // Seven groups in one role, so a page size smaller than the list leaves a remainder.
    let members: Vec<String> = (0..7).map(|n| format!("page-group-{n:02}")).collect();
    for member in &members {
        given_group(&harness, "paging", ACCOUNT, DEFAULT_NAMESPACE, member).await;
        let (status, body) =
            add_role_member(&harness, ACCOUNT, DEFAULT_NAMESPACE, "READER", member).await;
        assert_eq!(
            status, 200,
            "fixture {member:?}: the add expected 200, got {status} with {body}"
        );
    }

    // Rows: the unpaginated request, both published bounds of `MaxResults` (1..100) and
    // sizes in between. Each must walk the seven members exactly once.
    // Source: smithy_query(query="operation", operation="ListRoleMemberships").
    for max_results in [None, Some(1), Some(2), Some(3), Some(7), Some(100)] {
        let case = format!("max-results={max_results:?}");
        let mut seen: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        let mut pages = 0usize;
        loop {
            let query = match (&token, max_results) {
                (Some(token), Some(max)) => format!("?max-results={max}&next-token={token}"),
                (Some(token), None) => format!("?next-token={token}"),
                (None, Some(max)) => format!("?max-results={max}"),
                (None, None) => String::new(),
            };
            let (status, body) =
                list_role_members(&harness, ACCOUNT, DEFAULT_NAMESPACE, "READER", &query).await;
            assert_eq!(
                status, 200,
                "case {case}: listing expected 200, got {status} with {body}"
            );
            let page = reported_members(&case, &body);
            if let Some(max) = max_results {
                assert!(
                    page.len() <= usize::try_from(max).unwrap_or_default(),
                    "case {case}: a page held {} members, more than MaxResults: {body}",
                    page.len()
                );
            }
            seen.extend(page);
            pages += 1;
            assert!(
                pages <= members.len() + 1,
                "case {case}: pagination did not terminate after {pages} pages"
            );
            match body["NextToken"].as_str() {
                Some(next) => token = Some(next.to_string()),
                None => break,
            }
        }
        assert_eq!(
            seen, members,
            "case {case}: paging must yield every member exactly once, in name order"
        );
    }

    // A token this service did not issue names no position, so it is refused rather than
    // read as "from the beginning": an opaque-looking token, one with this service's
    // prefix but a body that is not hex, and an empty one.
    for token in ["not-a-token", "qs1-zzzz", "qs1-abc", ""] {
        let (status, body) = list_role_members(
            &harness,
            ACCOUNT,
            DEFAULT_NAMESPACE,
            "READER",
            &format!("?next-token={token}"),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {token:?}: an unreadable NextToken expected 400, got {status} with \
             {body}"
        );
        assert_eq!(
            body["__type"],
            json!("InvalidNextTokenException"),
            "case {token:?}: expected InvalidNextTokenException, got {body}"
        );
    }
}

#[tokio::test]
async fn a_role_is_reached_only_in_an_account_that_holds_a_quicksight_identity_store() {
    let harness = Harness::new();
    // Rows: the ways an account ends up with no QuickSight identity store of its own —
    // it never signed up, so it authenticates Amazon Web Services identities alone (the
    // service default); it signed up choosing exactly that; or it signed up against a
    // directory, which is where its identities then come from. The store the refusal
    // names is the one the row's account has rather than a constant.
    // Source: smithy_query(query="shape", shape="AuthenticationMethodOption").
    let rows = [
        ("never-signed-up", ACCOUNT, "IAM_ONLY", Value::Null),
        (
            "signed-up-without-one",
            OTHER_ACCOUNT,
            "IAM_ONLY",
            json!({
                "AccountName": format!("roles-{OTHER_ACCOUNT}"),
                "AuthenticationMethod": "IAM_ONLY",
                "NotificationEmail": "ops@example.com",
                "Edition": "ENTERPRISE",
            }),
        ),
        (
            "signed-up-against-a-directory",
            DIRECTORY_ACCOUNT,
            "ACTIVE_DIRECTORY",
            json!({
                "AccountName": format!("roles-{DIRECTORY_ACCOUNT}"),
                "AuthenticationMethod": "ACTIVE_DIRECTORY",
                "NotificationEmail": "ops@example.com",
                "Edition": "ENTERPRISE",
                "ActiveDirectoryName": "corp.example.com",
                "Realm": "CORP.EXAMPLE.COM",
                "AdminGroup": ["Domain Admins"],
            }),
        ),
    ];
    for (case, account, identity_store, subscription) in rows {
        if !subscription.is_null() {
            sign_up(&harness, case, account, subscription).await;
        }
        // The group itself CAN be created there, so what the refusals below are about is
        // the role rather than a group the namespace is missing.
        given_group(&harness, case, account, DEFAULT_NAMESPACE, "Admins").await;
        for role in ROLES {
            let calls = [
                (
                    "list",
                    list_role_members(&harness, account, DEFAULT_NAMESPACE, role, "").await,
                ),
                (
                    "create",
                    add_role_member(&harness, account, DEFAULT_NAMESPACE, role, "Admins").await,
                ),
                (
                    "delete",
                    remove_role_member(&harness, account, DEFAULT_NAMESPACE, role, "Admins").await,
                ),
            ];
            for (call, (status, body)) in calls {
                assert_eq!(
                    status, 401,
                    "case {case:?} {call} {role}: an account with no QuickSight identity \
                     store must be denied its roles, got {status} with {body}"
                );
                assert_eq!(
                    body["__type"],
                    json!("AccessDeniedException"),
                    "case {case:?} {call} {role}: expected AccessDeniedException, got \
                     {body}"
                );
                assert_eq!(
                    body["Message"],
                    json!(role_denial_message(
                        identity_store,
                        account,
                        DEFAULT_NAMESPACE,
                        role
                    )),
                    "case {case:?} {call} {role}: the refusal must name the identity \
                     store the account has: {body}"
                );
                assert!(
                    body["RequestId"].is_string(),
                    "case {case:?} {call} {role}: the refusal must report a RequestId: \
                     {body}"
                );
            }
        }
    }
    assert_eq!(
        held_role_memberships(&harness),
        0,
        "a denied call must persist nothing"
    );
}

#[tokio::test]
async fn removing_a_group_from_a_role_takes_it_out_of_the_listing() {
    let harness = Harness::new();
    sign_up_for_quicksight_identities(&harness, ACCOUNT).await;
    create_settled_custom_namespace(&harness, ACCOUNT).await;

    let doomed = "Doomed Admins";
    let kept = "Kept Admins";
    for member in [doomed, kept] {
        given_group(&harness, "removal", ACCOUNT, DEFAULT_NAMESPACE, member).await;
        let (status, body) =
            add_role_member(&harness, ACCOUNT, DEFAULT_NAMESPACE, "ADMIN", member).await;
        assert_eq!(
            status, 200,
            "fixture {member:?}: the add expected 200, got {status} with {body}"
        );
    }

    let (status, body) = remove_role_member(&harness, ACCOUNT, DEFAULT_NAMESPACE, "ADMIN", doomed)
        .await;
    assert_eq!(
        status, 200,
        "removing a group the role holds expected 200, got {status} with {body}"
    );
    assert!(
        body["RequestId"].is_string(),
        "the removal must report a RequestId: {body}"
    );

    let (status, body) = list_role_members(&harness, ACCOUNT, DEFAULT_NAMESPACE, "ADMIN", "").await;
    assert_eq!(
        status, 200,
        "the listing after the removal expected 200, got {status} with {body}"
    );
    assert_eq!(
        reported_members("after-removal", &body),
        vec![kept.to_string()],
        "the removed group must be out of the listing and the other one still in it: \
         {body}"
    );
    assert_eq!(
        held_role_memberships(&harness),
        1,
        "the removal must take exactly the membership it named"
    );

    // Removing it again finds no membership at that address, and says so with the
    // locator for the address the call named.
    let (status, body) = remove_role_member(&harness, ACCOUNT, DEFAULT_NAMESPACE, "ADMIN", doomed)
        .await;
    assert_eq!(
        status, 404,
        "a membership the role does not hold expected 404, got {status} with {body}"
    );
    assert_eq!(
        body["__type"],
        json!("ResourceNotFoundException"),
        "a membership the role does not hold is not found: {body}"
    );
    assert_eq!(
        body["ResourceType"],
        json!("GROUP"),
        "a role's members are groups: {body}"
    );
    assert_eq!(
        body["Message"],
        json!(role_membership_locator(
            ACCOUNT,
            DEFAULT_NAMESPACE,
            "ADMIN",
            doomed
        )),
        "the refusal must report the address the call named: {body}"
    );

    // A role takes an EXISTING group of its namespace: a name no group there is known by
    // names no member. The group the row names exists in ANOTHER namespace of the same
    // account, so what is missing is the group in this one.
    given_group(&harness, "elsewhere", ACCOUNT, CUSTOM_NAMESPACE, "Sales").await;
    let (status, body) =
        add_role_member(&harness, ACCOUNT, DEFAULT_NAMESPACE, "AUTHOR", "Sales").await;
    assert_eq!(
        status, 404,
        "a role takes an existing group, got {status} with {body}"
    );
    assert_eq!(
        body["ResourceType"],
        json!("GROUP"),
        "the refusal is about the group the call named: {body}"
    );
    assert_eq!(
        body["Message"],
        json!(group_miss_message(ACCOUNT, DEFAULT_NAMESPACE, "Sales")),
        "the refusal must name the group the namespace does not hold: {body}"
    );

    // A role is a role OF a namespace, so every call on one the account does not hold
    // reports that namespace.
    let missing = "no-such-namespace";
    let calls = [
        (
            "list",
            list_role_members(&harness, ACCOUNT, missing, "ADMIN", "").await,
        ),
        (
            "create",
            add_role_member(&harness, ACCOUNT, missing, "ADMIN", kept).await,
        ),
        (
            "delete",
            remove_role_member(&harness, ACCOUNT, missing, "ADMIN", kept).await,
        ),
    ];
    for (call, (status, body)) in calls {
        assert_eq!(
            status, 404,
            "case {call}: a namespace the account does not hold expected 404, got \
             {status} with {body}"
        );
        assert_eq!(
            body["ResourceType"],
            json!("NAMESPACE"),
            "case {call}: the refusal is about a namespace: {body}"
        );
        assert_eq!(
            body["Message"],
            json!(format!(
                "Namespace {missing} not found in account {ACCOUNT}."
            )),
            "case {call}: the refusal must name the namespace: {body}"
        );
    }

    // And a membership goes with the group it names: the groups of a namespace go when
    // that namespace is deleted, so the roles they held go with them.
    let (status, body) =
        add_role_member(&harness, ACCOUNT, CUSTOM_NAMESPACE, "AUTHOR", "Sales").await;
    assert_eq!(
        status, 200,
        "the membership of the doomed namespace expected 200, got {status} with {body}"
    );
    let (status, body) = delete_namespace(&harness, ACCOUNT, CUSTOM_NAMESPACE).await;
    assert_eq!(
        status, 200,
        "deleting the namespace expected 200, got {status} with {body}"
    );
    assert_eq!(
        held_role_memberships(&harness),
        1,
        "the memberships of a deleted namespace go with it, and only those"
    );
    let (status, body) = list_role_members(&harness, ACCOUNT, DEFAULT_NAMESPACE, "ADMIN", "").await;
    assert_eq!(
        status, 200,
        "the untouched namespace's role must still answer, got {status} with {body}"
    );
    assert_eq!(
        reported_members("survivor", &body),
        vec![kept.to_string()],
        "the membership of another namespace must survive the deletion: {body}"
    );
}

/// The descriptions the update rows submit, one per row of [`rows`], so the table varies
/// what the read-your-write rule generalises over: the shortest and the longest
/// `GroupDescription` the model admits (len 1..512), a non-ASCII one, one holding
/// punctuation and separators, and a plain re-description.
/// Source: smithy_query(query="operation", operation="UpdateGroup") — `Description`
/// targets `GroupDescription`, len 1..512.
fn updated_descriptions() -> Vec<(&'static str, String)> {
    vec![
        ("plain", "Sales team, reorganised".to_string()),
        ("shortest", "d".to_string()),
        ("longest", "u".repeat(512)),
        ("non-ascii", "Équipe des ventes — Café".to_string()),
        ("punctuated", "sales/eu-west-2: readers (2026)".to_string()),
    ]
}

#[tokio::test]
async fn update_group_persists_the_new_description_and_describe_reports_it() {
    let harness = Harness::new();
    create_custom_namespace(&harness, ACCOUNT).await;
    let descriptions = updated_descriptions();

    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let group_name = row.group_name.as_str();
        let namespace = row.namespace;

        let (status, body) = create_group(&harness, row.account, namespace, row.body()).await;
        assert_eq!(
            status, 201,
            "case {case:?}: create expected 201, got {status} with body {body}"
        );
        let arn = body["Group"]["Arn"].clone();
        let principal_id = body["Group"]["PrincipalId"].clone();

        // Two updates in a row, each submitting another description: whatever the
        // latest accepted update submitted is what the group carries.
        for step in ["first-update", "second-update"] {
            let (label, description) = &descriptions[if step == "first-update" {
                index % descriptions.len()
            } else {
                (index + 1) % descriptions.len()
            }];
            let (status, body) = update_group(
                &harness,
                row.account,
                namespace,
                group_name,
                json!({ "Description": description }),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}/{step}/{label}: update expected 200, got {status} with {body}"
            );
            let updated = &body["Group"];
            assert_eq!(
                updated["Description"],
                json!(description),
                "case {case:?}/{step}/{label}: the update must report the description it \
                 was given: {body}"
            );
            assert_eq!(
                updated["GroupName"],
                json!(group_name),
                "case {case:?}/{step}/{label}: the update must report the group it \
                 addressed: {body}"
            );
            assert_eq!(
                updated["Arn"], arn,
                "case {case:?}/{step}/{label}: an update must not change the group's \
                 Arn: {body}"
            );
            assert_eq!(
                updated["PrincipalId"], principal_id,
                "case {case:?}/{step}/{label}: an update must not change the group's \
                 PrincipalId: {body}"
            );
            assert!(
                body["RequestId"]
                    .as_str()
                    .is_some_and(|request_id| !request_id.is_empty()),
                "case {case:?}/{step}/{label}: the update reported no RequestId: {body}"
            );

            // The read that follows answers from the record the update wrote, and twice
            // over: a read changes nothing.
            for attempt in ["first-read", "second-read"] {
                let (status, body) =
                    describe_group(&harness, row.account, namespace, group_name).await;
                assert_eq!(
                    status, 200,
                    "case {case:?}/{step}/{label}/{attempt}: describe expected 200, got \
                     {status} with {body}"
                );
                assert_eq!(
                    body["Group"]["Description"],
                    json!(description),
                    "case {case:?}/{step}/{label}/{attempt}: describe must report the \
                     Description the update submitted, not the one the create did: {body}"
                );
                assert_eq!(
                    body["Group"]["PrincipalId"], principal_id,
                    "case {case:?}/{step}/{label}/{attempt}: the PrincipalId must not \
                     change: {body}"
                );
            }

            // And the stored record is what those reads answered from.
            {
                let state = qs_state::service_state(&*harness.engine).expect("the QuickSight store");
                let store = state.lock();
                let record = store
                    .group(row.account, namespace, group_name)
                    .unwrap_or_else(|| {
                        panic!("case {case:?}/{step}/{label}: the group is not stored")
                    });
                assert_eq!(
                    record.description.as_deref(),
                    Some(description.as_str()),
                    "case {case:?}/{step}/{label}: the submitted Description did not \
                     reach the stored record"
                );
            }
        }

        // `Description` is optional, so an update that submits none says nothing about
        // the description: the one the last update wrote stays.
        let (_, kept) = &descriptions[(index + 1) % descriptions.len()];
        let (status, body) =
            update_group(&harness, row.account, namespace, group_name, json!({})).await;
        assert_eq!(
            status, 200,
            "case {case:?}: an update submitting no Description expected 200, got \
             {status} with {body}"
        );
        assert_eq!(
            body["Group"]["Description"],
            json!(kept),
            "case {case:?}: an update that names no Description must leave the one the \
             group carries: {body}"
        );
        let (status, body) = describe_group(&harness, row.account, namespace, group_name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: describe expected 200, got {status} with {body}"
        );
        assert_eq!(
            body["Group"]["Description"],
            json!(kept),
            "case {case:?}: describe must still report the description the last update \
             wrote: {body}"
        );
    }
}

#[tokio::test]
async fn update_group_touches_only_the_group_it_addresses() {
    let harness = Harness::new();
    create_custom_namespace(&harness, ACCOUNT).await;
    let shared = "Shared Group";

    // The same name in three places, as the identity case above: two namespaces of one
    // account, and another account's default namespace.
    let places = [
        ("same-account-default-namespace", ACCOUNT, DEFAULT_NAMESPACE),
        ("same-account-custom-namespace", ACCOUNT, CUSTOM_NAMESPACE),
        ("other-account", OTHER_ACCOUNT, DEFAULT_NAMESPACE),
    ];
    for (case, account, namespace) in places {
        let (status, body) = create_group(
            &harness,
            account,
            namespace,
            json!({ "GroupName": shared, "Description": case }),
        )
        .await;
        assert_eq!(
            status, 201,
            "case {case:?}: create expected 201, got {status} with {body}"
        );
    }

    let (case, account, namespace) = places[1];
    let (status, body) = update_group(
        &harness,
        account,
        namespace,
        shared,
        json!({ "Description": "only this one" }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the update expected 200, got {status} with {body}"
    );

    for (index, (case, account, namespace)) in places.iter().enumerate() {
        let expected = if index == 1 { "only this one" } else { *case };
        let (status, body) = describe_group(&harness, account, namespace, shared).await;
        assert_eq!(
            status, 200,
            "case {case:?}: describe expected 200, got {status} with {body}"
        );
        assert_eq!(
            body["Group"]["Description"],
            json!(expected),
            "case {case:?}: an update addresses the group of one namespace of one \
             account: {body}"
        );
    }
}

#[tokio::test]
async fn updating_a_group_name_no_namespace_holds_is_not_found() {
    let harness = Harness::new();
    create_custom_namespace(&harness, ACCOUNT).await;
    let (status, body) = create_group(
        &harness,
        ACCOUNT,
        DEFAULT_NAMESPACE,
        json!({ "GroupName": "Sales", "Description": "Sales team" }),
    )
    .await;
    assert_eq!(status, 201, "the create expected 201, got {status} with {body}");

    // A name no group is held under, a name held only in another namespace, a name held
    // only in another account, and an account that holds nothing at all.
    for (case, account, namespace, group_name) in [
        ("unused-name", ACCOUNT, DEFAULT_NAMESPACE, "No Such Group"),
        (
            "name-used-in-another-namespace-only",
            ACCOUNT,
            CUSTOM_NAMESPACE,
            "Sales",
        ),
        ("other-account", OTHER_ACCOUNT, DEFAULT_NAMESPACE, "Sales"),
        ("account-with-nothing", "123456789012", DEFAULT_NAMESPACE, "Sales"),
    ] {
        let (status, body) = update_group(
            &harness,
            account,
            namespace,
            group_name,
            json!({ "Description": "new description" }),
        )
        .await;
        assert_eq!(
            status, 404,
            "case {case:?}: updating a group that does not exist expected 404, got \
             {status} with {body}"
        );
        assert_eq!(
            body["__type"],
            json!("ResourceNotFoundException"),
            "case {case:?}: a name no group is held under is not found: {body}"
        );
        assert_eq!(
            body["Message"],
            json!(format!(
                "Group {group_name} not found in namespace {namespace} in account {account}."
            )),
            "case {case:?}: the refusal must name the group, its namespace and its \
             account: {body}"
        );

        // A refused update creates nothing.
        let (status, body) = describe_group(&harness, account, namespace, group_name).await;
        assert_eq!(
            status, 404,
            "case {case:?}: a refused update must persist nothing, got {status} with \
             {body}"
        );
    }

    // And the group the namespace does hold is untouched by the refusals.
    let (status, body) = describe_group(&harness, ACCOUNT, DEFAULT_NAMESPACE, "Sales").await;
    assert_eq!(status, 200, "describe expected 200, got {status} with {body}");
    assert_eq!(
        body["Group"]["Description"],
        json!("Sales team"),
        "a refused update must not change the group it did not reach: {body}"
    );
}

#[tokio::test]
async fn a_group_principal_id_is_the_namespace_and_the_name_it_is_known_by() {
    // The rule: a group's `PrincipalId` is not a handle the service mints but the group
    // itself — the namespace it belongs to and the name it is known by there, which is
    // the resource part of the group's own ARN — so the same value comes back from every
    // read of that group, and the namespace and the name can be recovered from it.
    let harness = Harness::new();
    create_custom_namespace(&harness, ACCOUNT).await;

    for row in rows() {
        let case = row.case;
        let group_name = row.group_name.as_str();
        let namespace = row.namespace;
        let expected = format!("group/{namespace}/{group_name}");
        let expected_arn = group_arn(row.account, namespace, group_name);

        let (status, body) = create_group(&harness, row.account, namespace, row.body()).await;
        assert_eq!(
            status, 201,
            "case {case:?}: create expected 201, got {status} with {body}"
        );
        assert_eq!(
            body["Group"]["PrincipalId"],
            json!(expected),
            "case {case:?}: the create must report the group's own identity as its \
             PrincipalId: {body}"
        );
        assert_eq!(
            body["Group"]["Arn"],
            json!(expected_arn),
            "case {case:?}: and that identity is the resource part of the group's ARN: \
             {body}"
        );
        assert!(
            expected_arn.ends_with(&format!(":{expected}")),
            "case {case:?}: the ARN {expected_arn} must end in the principal id \
             {expected}"
        );

        // Every read of the group reports that same value, and so does a write.
        let (status, body) = describe_group(&harness, row.account, namespace, group_name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: describe expected 200, got {status} with {body}"
        );
        assert_eq!(
            body["Group"]["PrincipalId"],
            json!(expected),
            "case {case:?}: describe must report the same PrincipalId the create did: \
             {body}"
        );
        let (status, body) = update_group(
            &harness,
            row.account,
            namespace,
            group_name,
            json!({ "Description": "re-described" }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: update expected 200, got {status} with {body}"
        );
        assert_eq!(
            body["Group"]["PrincipalId"],
            json!(expected),
            "case {case:?}: an update must report the same PrincipalId too: {body}"
        );
    }
}
