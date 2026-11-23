// SPDX-License-Identifier: CC0-1.0

//! The IAM-federated login a client performs end to end: the endpoint a serving
//! cluster PUBLISHES, and the credential `GetClusterCredentialsWithIAM` mints for
//! the calling IAM identity logging on there over the PostgreSQL wire.
//!
//! RULE: an address an operation publishes for a caller to connect to is a host
//! this emulator itself serves, and the temporary credential
//! `GetClusterCredentialsWithIAM` answers with is admitted at that address as the
//! calling IAM identity's own database user — "the database user is mapped 1:1 to
//! the source Identity and Access Management (IAM) identity"
//! (`smithy_query(query="doc", shape="ClusterExtendedCredentials", member="DbUser")`)
//! — whatever kind of identity signed the request. A request that scopes those
//! credentials to a custom domain name no cluster of the account serves names no
//! active association, so it is refused as the parameter value it is and the
//! cluster keeps serving the endpoint it published.
//!
//! The rows span what the rule generalises over: the KIND of the calling identity
//! (a key no record names, an IAM user's own long-term key, a session assumed on a
//! role), the shape of the cluster the endpoint belongs to (single-node on the
//! default port, multi-node whose create names a port), and how the session is
//! opened (in the clear, and with the in-band TLS upgrade a `sslmode=require`
//! client performs).
//!
//! The wire client, the cluster fixture and the awsQuery harness are the ones
//! [`crate::cluster_engine_endpoint`] and [`crate::cluster_lifecycle`] own.

use service_com_amazonaws_redshift_redshiftserviceversion20121201::state;

use crate::cluster_engine_endpoint::{available_cluster, endpoint_of, postgres_login, Transport};
use crate::cluster_lifecycle::{Answer, Emulator, DEFAULT_ACCESS_KEY_ID};

/// The public DNS suffix the REAL service's cluster endpoints sit under. No
/// address this emulator publishes may end in it: the emulator answers on none of
/// those hosts, so announcing one sends a client to another deployment or to
/// nothing at all.
const AWS_OWNED_SUFFIX: &str = "amazonaws.com";

/// The sentence a get-credentials call scoped to a custom domain name that is no
/// active association of the account is refused with. The whole sentence is fixed
/// text and names nothing of the request, so it is spelled out here.
const INACTIVE_DOMAIN_SENTENCE: &str =
    "To use get credentials, you must provide an active custom domain name.";

/// A trust policy granting the account permission to assume a role, so the
/// assumed-role row's caller can be minted at all.
const TRUST_POLICY: &str = concat!(
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","#,
    r#""Principal":{"AWS":"arn:aws:iam::000000000000:root"},"Action":"sts:AssumeRole"}]}"#
);

/// One cluster an endpoint is read from, and how a session on it is opened.
struct ClusterRow {
    case: &'static str,
    identifier: &'static str,
    /// The create members beyond the required ones.
    params: Vec<(&'static str, &'static str)>,
    transport: Transport,
}

/// One caller the credentials are minted for.
struct IdentityRow {
    case: &'static str,
    /// The access key id the `GetClusterCredentialsWithIAM` call presents.
    access_key_id: String,
    /// The database user that identity maps to, spelled out here rather than read
    /// off the crate under test.
    db_user: String,
}

/// The clusters the endpoint rule is asserted on.
fn cluster_rows() -> Vec<ClusterRow> {
    vec![
        ClusterRow {
            case: "a single-node cluster on the default port, a session in the clear",
            identifier: "iam-login-single",
            params: vec![("ClusterType", "single-node")],
            transport: Transport::Plaintext,
        },
        ClusterRow {
            case: "a multi-node cluster whose create names a port, TLS negotiated in band",
            identifier: "iam-login-multi",
            params: vec![
                ("ClusterType", "multi-node"),
                ("NumberOfNodes", "2"),
                ("Port", "5462"),
            ],
            transport: Transport::Tls,
        },
    ]
}

/// One IAM or AWS STS call of the fixture, which must succeed.
async fn fixture(
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
        "case {case}: the fixture {action} must succeed, got {} with body {}",
        answer.status, answer.body
    );
    answer
}

/// The text of `member` in an answer.
fn member(case: &str, answer: &Answer, name: &str) -> String {
    answer
        .text(name)
        .unwrap_or_else(|| panic!("case {case}: the answer carries no {name}: {}", answer.body))
}

/// An IAM user carrying a long-term access key, as a client makes one: answers
/// the key id.
async fn iam_user_key(emulator: &Emulator, case: &str, user_name: &str) -> String {
    fixture(
        emulator,
        case,
        "iam",
        "2010-05-08",
        "CreateUser",
        &[("UserName", user_name)],
    )
    .await;
    let minted = fixture(
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
    let created = fixture(
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
    let assumed = fixture(
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

/// The ARN `sts:GetCallerIdentity` answers for `access_key_id` — the identity the
/// reported database user is cross-checked against.
async fn caller_arn(emulator: &Emulator, case: &str, access_key_id: &str) -> String {
    let answered = emulator
        .call_query_as(access_key_id, "sts", "2011-06-15", "GetCallerIdentity", &[])
        .await;
    assert_eq!(
        answered.status, 200,
        "case {case}: GetCallerIdentity must succeed, got {} with body {}",
        answered.status, answered.body
    );
    member(case, &answered, "Arn")
}

/// Mint credentials for `identifier`'s database as the caller `access_key_id`
/// names, asserting the call is served.
async fn minted_credentials(
    emulator: &Emulator,
    case: &str,
    access_key_id: &str,
    identifier: &str,
) -> Answer {
    let answered = emulator
        .call_as(
            access_key_id,
            "GetClusterCredentialsWithIAM",
            &[
                ("ClusterIdentifier", identifier),
                ("DbName", state::DEFAULT_DB_NAME),
                (
                    "DurationSeconds",
                    &state::CREDENTIAL_DURATION_MIN_SECONDS.to_string(),
                ),
            ],
        )
        .await;
    assert_eq!(
        answered.status, 200,
        "case {case}: GetClusterCredentialsWithIAM expected 200, got {} with body {}",
        answered.status, answered.body
    );
    answered
}

/// Assert the address a cluster published is one this emulator serves rather than
/// a host of the real service's own domain.
fn assert_own_host(case: &str, address: &str) {
    assert!(
        !address.ends_with(AWS_OWNED_SUFFIX),
        "case {case}: expected an address this emulator serves, got the AWS-owned host \
         {address:?}"
    );
}

/// RULE: the address a serving cluster publishes is one this emulator answers on,
/// and the credential `GetClusterCredentialsWithIAM` mints for the calling IAM
/// identity logs on there as that identity's own database user.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_iam_identitys_minted_credential_logs_on_to_the_published_endpoint() {
    let emulator = Emulator::with_identity_services();

    let loader_key = iam_user_key(&emulator, "an IAM user's own key", "rs-loader").await;
    let session_key = assumed_role_key(
        &emulator,
        "a session assumed on a role",
        "RsLoaderRole",
        "nightly-load",
    )
    .await;
    let identities = [
        IdentityRow {
            // Neither IAM nor AWS STS holds this key, so it speaks for the
            // identity the emulator publishes at start: an IAM user of the
            // account.
            case: "a key no session and no IAM record names",
            access_key_id: DEFAULT_ACCESS_KEY_ID.to_string(),
            db_user: "IAM:emulator".to_string(),
        },
        IdentityRow {
            case: "an IAM user's own long-term access key",
            access_key_id: loader_key,
            db_user: "IAM:rs-loader".to_string(),
        },
        IdentityRow {
            // A role identity carries the role prefix and the ROLE's name: the
            // session name is chosen per session, so a user built from it would
            // not be the identity's.
            case: "a session assumed on a role",
            access_key_id: session_key,
            db_user: "IAMR:RsLoaderRole".to_string(),
        },
    ];

    for cluster in cluster_rows() {
        let described = available_cluster(&emulator, cluster.identifier, &cluster.params).await;
        let (address, port) = endpoint_of(cluster.case, &described);
        assert_own_host(cluster.case, &address);

        for identity in &identities {
            let case = format!("{} for {}", cluster.case, identity.case);
            let answered = minted_credentials(
                &emulator,
                &case,
                &identity.access_key_id,
                cluster.identifier,
            )
            .await;
            let reported = member(&case, &answered, "DbUser");
            assert_eq!(
                reported, identity.db_user,
                "case {case}: expected the database user {:?}, got {reported:?}",
                identity.db_user
            );
            // The identity is the one AWS STS reports for the very same
            // credential, so the name part of the database user appears in that
            // ARN and the access key id appears nowhere in it.
            let arn = caller_arn(&emulator, &case, &identity.access_key_id).await;
            let name = reported
                .split_once(':')
                .map(|(_prefix, name)| name.to_string())
                .unwrap_or_default();
            assert!(
                !name.is_empty() && arn.contains(&name),
                "case {case}: expected a name drawn from the caller's ARN {arn:?}, got \
                 {reported:?}"
            );
            assert!(
                !reported.contains(identity.access_key_id.as_str()),
                "case {case}: an access key id identifies a credential, never a database user, \
                 got {reported:?}"
            );
            let password = member(&case, &answered, "DbPassword");

            // The published address is where that pair is ADMITTED: the login the
            // response authorises is attempted on the very endpoint the control
            // plane announced.
            let mut session = postgres_login(
                &case,
                &address,
                port,
                cluster.transport,
                &reported,
                &password,
                Some(state::DEFAULT_DB_NAME),
            )
            .await
            .unwrap_or_else(|refusal| {
                panic!("case {case}: the minted credential must be admitted, got {refusal:?}")
            });
            let answer = session
                .query("SELECT 1")
                .await
                .unwrap_or_else(|refusal| panic!("case {case}: SELECT 1 must run, got {refusal:?}"));
            assert_eq!(
                answer.only(),
                Some("1"),
                "case {case}: expected the engine to answer the statement, got {answer:?}"
            );
            let whoami = session
                .query("SELECT current_user()")
                .await
                .unwrap_or_else(|refusal| {
                    panic!("case {case}: current_user() must run, got {refusal:?}")
                });
            assert_eq!(
                whoami.only(),
                Some(reported.as_str()),
                "case {case}: expected the session to run as the database user the credential \
                 was issued for, got {whoami:?}"
            );
            let named = session
                .query("SELECT current_database()")
                .await
                .unwrap_or_else(|refusal| {
                    panic!("case {case}: current_database() must run, got {refusal:?}")
                });
            assert_eq!(
                named.only(),
                Some(state::DEFAULT_DB_NAME),
                "case {case}: expected the database the credential was scoped to, got {named:?}"
            );

            // Mapped 1:1 to the identity: a second call by the same caller maps to
            // the same database user, so a grant written for it keeps matching.
            let again = minted_credentials(
                &emulator,
                &case,
                &identity.access_key_id,
                cluster.identifier,
            )
            .await;
            assert_eq!(
                member(&case, &again, "DbUser"),
                reported,
                "case {case}: expected the same database user on a second call, got {:?}",
                member(&case, &again, "DbUser")
            );
        }
    }
}

/// Every custom domain name a row scopes the credentials to, none of them served
/// by any cluster of the account.
fn unassociated_domains() -> Vec<(&'static str, String)> {
    // The longest label `CustomDomainName`'s own pattern admits is 63 characters:
    // 62 from `[A-Za-z0-9-]{0,62}` plus the trailing `[A-Za-z0-9]`.
    let longest_label: String = std::iter::repeat_n('a', 62)
        .chain(std::iter::once('z'))
        .collect();
    vec![
        ("a two-label name", "absent.example.com".to_string()),
        (
            "a deeper name",
            "loader.eu.west.absent.example.org".to_string(),
        ),
        (
            "hyphenated labels",
            "no-such-warehouse.absent-domain.example.net".to_string(),
        ),
        (
            "a label at the longest the pattern admits",
            format!("{longest_label}.example.com"),
        ),
    ]
}

/// RULE: a get-credentials request scoped to a custom domain name that is no
/// active association of the account is refused as the parameter value it is —
/// nothing is issued — while the cluster it also named keeps serving the endpoint
/// it published.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn credentials_scoped_to_an_unassociated_custom_domain_are_refused() {
    let emulator = Emulator::new();
    let identifier = "iam-login-domain";
    let described =
        available_cluster(&emulator, identifier, &[("ClusterType", "single-node")]).await;
    let (address, port) = endpoint_of("the unscoped control", &described);
    assert_own_host("the unscoped control", &address);

    // The positive control: the unscoped call is served and its credential logs
    // on, so each refusal below is about the domain the request named rather than
    // about this cluster.
    let before = minted_credentials(
        &emulator,
        "the unscoped control before the refusals",
        crate::cluster_lifecycle::DEFAULT_ACCESS_KEY_ID,
        identifier,
    )
    .await;
    let mut session = postgres_login(
        "the unscoped control before the refusals",
        &address,
        port,
        Transport::Tls,
        &member("the unscoped control before the refusals", &before, "DbUser"),
        &member(
            "the unscoped control before the refusals",
            &before,
            "DbPassword",
        ),
        Some(state::DEFAULT_DB_NAME),
    )
    .await
    .unwrap_or_else(|refusal| {
        panic!(
            "case the unscoped control before the refusals: the credential must be admitted, \
             got {refusal:?}"
        )
    });
    let answered = session.query("SELECT 1").await.unwrap_or_else(|refusal| {
        panic!("case the unscoped control before the refusals: SELECT 1 must run, got {refusal:?}")
    });
    assert_eq!(
        answered.only(),
        Some("1"),
        "case the unscoped control before the refusals: expected the engine's answer, got \
         {answered:?}"
    );

    for (domain_case, domain) in unassociated_domains() {
        // With and without a cluster of this account named beside the domain: the
        // domain is still no active association either way.
        for (scope_case, params) in [
            (
                "the domain alone",
                vec![("CustomDomainName", domain.as_str())],
            ),
            (
                "the domain beside a cluster of this account",
                vec![
                    ("ClusterIdentifier", identifier),
                    ("CustomDomainName", domain.as_str()),
                ],
            ),
        ] {
            let case = format!("{domain_case}, {scope_case}");
            let refused = emulator.call("GetClusterCredentialsWithIAM", &params).await;
            assert_eq!(
                (
                    refused.status,
                    refused.error_code().as_deref(),
                    refused.text("Message").as_deref()
                ),
                (
                    400,
                    Some("InvalidParameterValue"),
                    Some(INACTIVE_DOMAIN_SENTENCE)
                ),
                "case {case}: expected 400 InvalidParameterValue with \
                 {INACTIVE_DOMAIN_SENTENCE:?}, got {} / {:?} with body {}",
                refused.status,
                refused.error_code(),
                refused.body
            );
            assert_eq!(
                refused.text("DbPassword"),
                None,
                "case {case}: a refused request must issue nothing, got {:?}",
                refused.text("DbPassword")
            );
        }
    }

    // And the cluster is untouched by the refusals: it still publishes the same
    // endpoint and a freshly minted credential still logs on there.
    let after_described = emulator
        .call("DescribeClusters", &[("ClusterIdentifier", identifier)])
        .await;
    let (after_address, after_port) =
        endpoint_of("the unscoped control after the refusals", &after_described);
    assert_eq!(
        (after_address.as_str(), after_port),
        (address.as_str(), port),
        "case the unscoped control after the refusals: expected the endpoint the cluster \
         published to be unchanged, got {after_address}:{after_port}"
    );
    let after = minted_credentials(
        &emulator,
        "the unscoped control after the refusals",
        crate::cluster_lifecycle::DEFAULT_ACCESS_KEY_ID,
        identifier,
    )
    .await;
    let mut session = postgres_login(
        "the unscoped control after the refusals",
        &after_address,
        after_port,
        Transport::Plaintext,
        &member("the unscoped control after the refusals", &after, "DbUser"),
        &member(
            "the unscoped control after the refusals",
            &after,
            "DbPassword",
        ),
        Some(state::DEFAULT_DB_NAME),
    )
    .await
    .unwrap_or_else(|refusal| {
        panic!(
            "case the unscoped control after the refusals: the credential must be admitted, \
             got {refusal:?}"
        )
    });
    let answered = session.query("SELECT 1").await.unwrap_or_else(|refusal| {
        panic!("case the unscoped control after the refusals: SELECT 1 must run, got {refusal:?}")
    });
    assert_eq!(
        answered.only(),
        Some("1"),
        "case the unscoped control after the refusals: expected the engine's answer, got \
         {answered:?}"
    );
}
