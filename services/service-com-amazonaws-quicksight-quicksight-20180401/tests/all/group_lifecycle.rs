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
//! group — its ARN, its name, the description it was given and the `PrincipalId` the
//! service minted for it — and `DescribeGroup` answers from that same record. A name the
//! namespace already holds is refused as an existing resource, and a namespace the
//! account does not hold is not found. So the rows below vary what the rule generalises
//! over (the group name's form and length, whether a description is submitted and how
//! long it is, the namespace, and the account) rather than re-running one recorded call.
//!
//! The membership cases at the end of this suite are about a group's MEMBERS — the
//! users it holds — which are reached through the group they belong to, so they are
//! driven here beside the group they hang on.

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

    // Each of the three is a group of its own, so no two share a handle.
    for (left, right) in [(0usize, 1usize), (0, 2), (1, 2)] {
        assert_ne!(
            principal_ids[left], principal_ids[right],
            "case {:?}/{:?}: two distinct groups must not share a PrincipalId",
            places[left].0, places[right].0
        );
    }

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
