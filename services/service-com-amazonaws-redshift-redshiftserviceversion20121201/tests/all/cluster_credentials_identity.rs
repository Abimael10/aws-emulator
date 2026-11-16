// SPDX-License-Identifier: CC0-1.0

//! WHICH database user `GetClusterCredentialsWithIAM` maps a caller to, end to
//! end over the awsQuery wire.
//!
//! RULE: the operation takes no `DbUser` — "the database user is mapped 1:1 to the
//! source IAM identity" (`ClusterExtendedCredentials$DbUser`) — so the reported
//! `DbUser` is the IAM IDENTITY's own name, the name field of the ARN
//! `sts:GetCallerIdentity` answers for the very credential the request was signed
//! with, under the prefix that states which kind of identity it is: `IAMR:` for a
//! role, the plain `IAM:` for every other IAM identity. It is therefore a name a
//! client can grant to in the database and a substring of the caller's own ARN,
//! never the access key id the request presented — an access key id identifies a
//! CREDENTIAL, so deriving a user name from it would give one identity as many
//! database users as it has keys.
//!
//! The identities are real: every row's caller is made with IAM's and AWS STS's
//! OWN operations over the same engine — an IAM user and a long-term key of it, a
//! user under a path, a session assumed on a role, a federated user — and what
//! this service reports is cross-checked against what `sts:GetCallerIdentity`
//! answers for that same credential.
//!
//! The rows differ along what the rule generalises over: the KIND of identity,
//! whether the identity's name sits under a path, and how many credentials one
//! identity holds (the 1:1 direction of the mapping) — and two different
//! identities are asserted to map to two different database users, which is the
//! rule read from the other side.
//!
//! The harness is shared with [`crate::cluster_lifecycle`], which owns it.

use service_com_amazonaws_redshift_redshiftserviceversion20121201::state;

use crate::cluster_lifecycle::{Answer, Emulator, DEFAULT_ACCESS_KEY_ID};

/// The cluster whose database the credentials below are issued for; one is
/// enough, since the rule under test is about the CALLER and not the cluster.
const CLUSTER: &str = "iam-identity-cluster";

/// A trust policy granting the account permission to assume a role, so the
/// assumed-role row can be minted at all.
const TRUST_POLICY: &str = concat!(
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","#,
    r#""Principal":{"AWS":"arn:aws:iam::000000000000:root"},"Action":"sts:AssumeRole"}]}"#
);

/// One caller the rule is asserted for.
struct Row {
    case: &'static str,
    /// The access key id the `GetClusterCredentialsWithIAM` call presents.
    access_key_id: String,
    /// The database user that caller must map to, spelled out here rather than
    /// read off the crate under test.
    db_user: String,
}

/// An emulator holding one serving cluster, with IAM and AWS STS mounted beside
/// Redshift on the same engine so the callers can be minted at all.
async fn seeded() -> Emulator {
    let emulator = Emulator::with_identity_services();
    let created = emulator.create_minimal(CLUSTER).await;
    assert_eq!(
        created.status, 200,
        "case seed the cluster: expected 200, got {} with body {}",
        created.status, created.body
    );
    emulator
        .poll_until(
            "seed the cluster",
            "DescribeClusters",
            &[("ClusterIdentifier", CLUSTER)],
            |answer| answer.text("ClusterStatus").as_deref() == Some(state::STATUS_AVAILABLE),
        )
        .await;
    emulator
}

/// One IAM or AWS STS call of the fixture, which must succeed.
async fn setup(
    emulator: &Emulator,
    case: &str,
    signing_name: &str,
    version: &str,
    action: &str,
    params: &[(&str, &str)],
) -> Answer {
    let answer = emulator
        .call_query_as(DEFAULT_ACCESS_KEY_ID, signing_name, version, action, params)
        .await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: the fixture {action} must succeed, got {} with body {}",
        answer.status, answer.body
    );
    answer
}

/// The text of `member` in a fixture answer.
fn member(case: &str, answer: &Answer, name: &str) -> String {
    answer
        .text(name)
        .unwrap_or_else(|| panic!("case {case:?}: the answer carries no {name}: {}", answer.body))
}

/// An IAM user at `path` carrying a long-term access key, as a client makes one:
/// answers the key id.
async fn iam_user_key(emulator: &Emulator, case: &str, user_name: &str, path: &str) -> String {
    setup(
        emulator,
        case,
        "iam",
        "2010-05-08",
        "CreateUser",
        &[("UserName", user_name), ("Path", path)],
    )
    .await;
    let minted = setup(
        emulator,
        case,
        "iam",
        "2010-05-08",
        "CreateAccessKey",
        &[("UserName", user_name)],
    )
    .await;
    member(case, &minted, "AccessKeyId")
}

/// A session assumed on a fresh role, as a client makes one: answers the
/// session's access key id.
async fn assumed_role_key(
    emulator: &Emulator,
    case: &str,
    role_name: &str,
    session_name: &str,
) -> String {
    let created = setup(
        emulator,
        case,
        "iam",
        "2010-05-08",
        "CreateRole",
        &[
            ("RoleName", role_name),
            ("AssumeRolePolicyDocument", TRUST_POLICY),
        ],
    )
    .await;
    let role_arn = member(case, &created, "Arn");
    let assumed = setup(
        emulator,
        case,
        "sts",
        "2011-06-15",
        "AssumeRole",
        &[
            ("RoleArn", role_arn.as_str()),
            ("RoleSessionName", session_name),
        ],
    )
    .await;
    member(case, &assumed, "AccessKeyId")
}

/// A federated-user session, as a client makes one: answers its access key id.
async fn federated_key(emulator: &Emulator, case: &str, name: &str) -> String {
    let minted = setup(
        emulator,
        case,
        "sts",
        "2011-06-15",
        "GetFederationToken",
        &[("Name", name)],
    )
    .await;
    member(case, &minted, "AccessKeyId")
}

/// The ARN `sts:GetCallerIdentity` answers for `access_key_id` — the identity the
/// reported database user is asserted against.
async fn caller_arn(emulator: &Emulator, case: &str, access_key_id: &str) -> String {
    let answered = emulator
        .call_query_as(access_key_id, "sts", "2011-06-15", "GetCallerIdentity", &[])
        .await;
    assert_eq!(
        answered.status, 200,
        "case {case:?}: GetCallerIdentity must succeed, got {} with body {}",
        answered.status, answered.body
    );
    member(case, &answered, "Arn")
}

/// Issue credentials for [`CLUSTER`] as the caller `access_key_id` names.
async fn credentials(emulator: &Emulator, case: &str, access_key_id: &str) -> Answer {
    let answered = emulator
        .call_as(
            access_key_id,
            "GetClusterCredentialsWithIAM",
            &[("ClusterIdentifier", CLUSTER)],
        )
        .await;
    assert_eq!(
        answered.status, 200,
        "case {case:?}: GetClusterCredentialsWithIAM expected 200, got {} with body {}",
        answered.status, answered.body
    );
    answered
}

/// RULE: the reported `DbUser` is the calling IAM identity's own name under the
/// prefix its kind carries — the identity AWS STS reports for the same
/// credential — and never the access key id the request presented.
#[tokio::test]
async fn the_database_user_is_the_calling_iam_identitys_own_name() {
    let emulator = seeded().await;

    let analyst_key = iam_user_key(&emulator, "an IAM user's own key", "rs-analyst", "/").await;
    let engineer_key = iam_user_key(
        &emulator,
        "an IAM user under a path",
        "rs-engineer",
        "/team/warehouse/",
    )
    .await;
    let admin_session = assumed_role_key(
        &emulator,
        "a session assumed on a role",
        "RsWarehouseAdmin",
        "jane.doe",
    )
    .await;
    let federated = federated_key(&emulator, "a federated user", "Bob").await;

    let rows = [
        Row {
            // Neither AWS STS nor IAM holds this key, so it speaks for the
            // identity the emulator publishes at start: an IAM user of the
            // account, whose ARN ends in `user/emulator`.
            case: "a key no session and no IAM record names",
            access_key_id: DEFAULT_ACCESS_KEY_ID.to_string(),
            db_user: "IAM:emulator".to_string(),
        },
        Row {
            case: "an IAM user's own long-term access key",
            access_key_id: analyst_key.clone(),
            db_user: "IAM:rs-analyst".to_string(),
        },
        Row {
            // A user's PATH says where the identity sits, not what it is called,
            // so the database user carries the name alone.
            case: "an IAM user whose name sits under a path",
            access_key_id: engineer_key,
            db_user: "IAM:rs-engineer".to_string(),
        },
        Row {
            // A role identity carries the role prefix, and the name is the
            // ROLE's: the session name is chosen per session, so a database user
            // built from it would not be the identity's.
            case: "a session assumed on a role",
            access_key_id: admin_session,
            db_user: "IAMR:RsWarehouseAdmin".to_string(),
        },
        Row {
            case: "a federated user",
            access_key_id: federated,
            db_user: "IAM:Bob".to_string(),
        },
    ];

    for row in &rows {
        let Row {
            case,
            access_key_id,
            db_user,
        } = row;

        let answered = credentials(&emulator, case, access_key_id).await;
        let reported = member(case, &answered, "DbUser");
        assert_eq!(
            &reported, db_user,
            "case {case:?}: expected the database user {db_user:?}, got {reported:?}"
        );

        // The identity is the one AWS STS reports for the very same credential,
        // so the name part of the database user appears in that ARN.
        let arn = caller_arn(&emulator, case, access_key_id).await;
        let name = reported
            .split_once(':')
            .map(|(_prefix, name)| name.to_string())
            .unwrap_or_default();
        assert!(
            !name.is_empty() && arn.contains(&name),
            "case {case:?}: expected a name drawn from the caller's ARN {arn:?}, got \
             {reported:?}"
        );
        assert!(
            !reported.contains(access_key_id.as_str()),
            "case {case:?}: an access key id identifies a credential, never a database \
             user, got {reported:?} for key {access_key_id:?}"
        );

        // Mapped 1:1 to the identity: a second call by the same caller maps to
        // the same database user.
        let again = credentials(&emulator, case, access_key_id).await;
        assert_eq!(
            member(case, &again, "DbUser"),
            reported,
            "case {case:?}: expected the same database user on a second call, got {:?} then \
             {reported:?}",
            member(case, &again, "DbUser")
        );
    }

    // Two CREDENTIALS of one identity map to that one database user: the mapping
    // is made from the identity, so a second access key of the same IAM user adds
    // no second database user.
    let second_analyst_key = {
        let minted = setup(
            &emulator,
            "a second key of one identity",
            "iam",
            "2010-05-08",
            "CreateAccessKey",
            &[("UserName", "rs-analyst")],
        )
        .await;
        member("a second key of one identity", &minted, "AccessKeyId")
    };
    assert_ne!(
        second_analyst_key, analyst_key,
        "case a second key of one identity: the fixture must mint a second, different key"
    );
    let under_second_key = credentials(
        &emulator,
        "a second key of one identity",
        &second_analyst_key,
    )
    .await;
    assert_eq!(
        member("a second key of one identity", &under_second_key, "DbUser"),
        "IAM:rs-analyst",
        "case a second key of one identity: expected one database user for one identity, got \
         {:?}",
        member("a second key of one identity", &under_second_key, "DbUser")
    );

    // And the rule read from the other side: two different identities calling one
    // cluster map to two different database users.
    let reported: Vec<String> = {
        let mut seen = Vec::new();
        for row in &rows {
            let answered = credentials(&emulator, row.case, &row.access_key_id).await;
            seen.push(member(row.case, &answered, "DbUser"));
        }
        seen
    };
    let mut unique = reported.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(
        unique.len(),
        reported.len(),
        "expected one database user per identity, got {reported:?}"
    );
}
