// SPDX-License-Identifier: CC0-1.0

//! WHO may be told where a consumer group's COORDINATOR is, on a connection Amazon MSK's
//! IAM access control governs.
//!
//! RULE under test: finding a group's coordinator is a DESCRIBE of that group, so
//! `FindCoordinator` is decided against the group's own scope — a connection whose
//! principal's policies do not allow `kafka-cluster:DescribeGroup` on the group it names
//! is answered `GROUP_AUTHORIZATION_FAILED` and no broker (Apache Kafka answers a refused
//! lookup with its no-node: id -1, no host, port -1) instead of being sent to a broker
//! that would then refuse every call it makes about the group. A key that names a
//! TRANSACTION is not a group, and a connection no policy bounds is not refused.
//!
//! The rows span what the rule generalises over: whether the policy covers the group the
//! request names, which VERSION of the API the client negotiated (the versions that ask
//! about one key and carry the answer in the response's own members, and the versions
//! that ask about a LIST of keys and carry a per-key answer), and how many keys one
//! request names.
//!
//! Every assertion goes over a real socket the core opened for the cluster, dialled at
//! the SASL/IAM address `GetBootstrapBrokers` published and authenticated with the
//! credential the row names.

use ::bytes::Bytes;
use ::kafka_protocol::messages::{
    ApiKey, FindCoordinatorRequest, FindCoordinatorResponse, SaslAuthenticateRequest,
    SaslAuthenticateResponse, SaslHandshakeRequest, SaslHandshakeResponse,
};
use ::kafka_protocol::protocol::StrBytes;
use ::kafka_protocol::ResponseError;
use serde_json::{json, Value};

use crate::broker_data_plane::{Client, AWS_MSK_IAM, CLIENT_ID, LOOPBACK};
use crate::kafka_harness::{
    given_assumed_role_session, given_role_inline_policy, ClusterRequest, Emulator, ACCOUNT,
    REGION,
};

/// The group the policy below covers, and one it does not.
const GRANTED_GROUP: &str = "readers";
const UNGRANTED_GROUP: &str = "auditors";

/// The `CoordinatorType` a request names a consumer group by, and the one it names a
/// transaction by (Apache Kafka protocol, `FindCoordinator`).
const GROUP_KEY: i8 = 0;
const TRANSACTION_KEY: i8 = 1;

/// What Apache Kafka answers a refused coordinator lookup with: its no-node.
const NO_NODE: i32 = -1;
const NO_PORT: i32 = -1;

/// RULE: a lookup of a group the connection may not describe is refused, and one of a
/// group it may describe names a broker of this cluster — at every version of the API.
#[tokio::test]
async fn finding_a_groups_coordinator_is_decided_against_the_groups_describe_scope() {
    let case = "a policy scoped to one consumer group";
    let emulator = Emulator::with_iam();
    let cluster = given_iam_cluster(&emulator, "coordinator-scope").await;
    let access_key_id = "ASIAKAFKACOORDSCOPE";
    given_principal(
        &emulator,
        "kafka-coordinator-client",
        access_key_id,
        Some(&json!({
            "Version": "2012-10-17",
            "Statement": [
                {
                    "Effect": "Allow",
                    "Action": ["kafka-cluster:Connect"],
                    "Resource": [format!("arn:aws:kafka:{REGION}:{ACCOUNT}:cluster/*")],
                },
                {
                    "Effect": "Allow",
                    "Action": ["kafka-cluster:DescribeGroup"],
                    "Resource": [format!("arn:aws:kafka:{REGION}:{ACCOUNT}:group/*/{GRANTED_GROUP}")],
                },
            ],
        })),
    );
    let authority = emulator
        .published_bootstrap(&cluster, "bootstrapBrokerStringSaslIam", case)
        .await;
    let mut client = iam_client(case, &authority, access_key_id).await;

    // (row, the version negotiated, the group the request names, the error expected)
    let rows: &[(&str, i16, &str, i16)] = &[
        ("the oldest version, a granted group", 0, GRANTED_GROUP, 0),
        (
            "the oldest version, a group no policy describes",
            0,
            UNGRANTED_GROUP,
            ResponseError::GroupAuthorizationFailed.code(),
        ),
        ("the last single-key version, a granted group", 3, GRANTED_GROUP, 0),
        (
            "the last single-key version, a group no policy describes",
            3,
            UNGRANTED_GROUP,
            ResponseError::GroupAuthorizationFailed.code(),
        ),
    ];
    for (row, version, group, expected) in rows {
        let answered = find_one(&mut client, case, *version, group, GROUP_KEY).await;
        assert_eq!(
            answered.error_code, *expected,
            "case {case:?}: row {row:?}: expected error {expected}, got {}",
            answered.error_code
        );
        match *expected {
            0 => {
                assert!(
                    answered.node_id.0 >= 0
                        && answered.host.as_str() == LOOPBACK
                        && answered.port > 0,
                    "case {case:?}: row {row:?}: a served lookup must name a broker of this \
                     cluster, got node {} at {}:{}",
                    answered.node_id.0,
                    answered.host.as_str(),
                    answered.port
                );
            }
            _ => {
                assert_eq!(
                    (answered.node_id.0, answered.host.as_str(), answered.port),
                    (NO_NODE, "", NO_PORT),
                    "case {case:?}: row {row:?}: a refused lookup must name no broker"
                );
            }
        }
    }

    // From version 4 on the answer is a per-key list, so ONE request carrying both keys
    // is answered the refusal for one and the coordinator for the other.
    let listed: FindCoordinatorResponse = client
        .call(
            case,
            ApiKey::FindCoordinator,
            4,
            &FindCoordinatorRequest::default()
                .with_key_type(GROUP_KEY)
                .with_coordinator_keys(vec![
                    StrBytes::from_static_str(GRANTED_GROUP),
                    StrBytes::from_static_str(UNGRANTED_GROUP),
                ]),
        )
        .await;
    let answered: Vec<(String, i16)> = listed
        .coordinators
        .iter()
        .map(|coordinator| (coordinator.key.to_string(), coordinator.error_code))
        .collect();
    assert_eq!(
        answered,
        vec![
            (GRANTED_GROUP.to_string(), 0),
            (
                UNGRANTED_GROUP.to_string(),
                ResponseError::GroupAuthorizationFailed.code()
            ),
        ],
        "case {case:?}: each key of one request must be answered its own decision"
    );
    let refused = listed
        .coordinators
        .iter()
        .find(|coordinator| coordinator.key.as_str() == UNGRANTED_GROUP)
        .unwrap_or_else(|| panic!("case {case:?}: the answer must cover every key"));
    assert_eq!(
        (refused.node_id.0, refused.host.as_str(), refused.port),
        (NO_NODE, "", NO_PORT),
        "case {case:?}: a refused key must name no broker"
    );

    // A TRANSACTION key is no group, so the group scope does not decide it.
    let transactional = find_one(&mut client, case, 3, UNGRANTED_GROUP, TRANSACTION_KEY).await;
    assert_eq!(
        transactional.error_code, 0,
        "case {case:?}: a transaction key must not be decided against a group's scope, got \
         error {}",
        transactional.error_code
    );
}

/// RULE: a connection no readable policy bounds is told where every group it asks about
/// is coordinated — nothing that can be read says it may not.
#[tokio::test]
async fn a_connection_no_policy_bounds_is_told_where_a_group_is_coordinated() {
    let case = "a credential this account holds no policies for";
    let emulator = Emulator::with_iam();
    let cluster = given_iam_cluster(&emulator, "coordinator-unbounded").await;
    let authority = emulator
        .published_bootstrap(&cluster, "bootstrapBrokerStringSaslIam", case)
        .await;
    let mut client = iam_client(
        case,
        &authority,
        crate::kafka_harness::DEFAULT_ACCESS_KEY_ID,
    )
    .await;
    for group in [GRANTED_GROUP, UNGRANTED_GROUP] {
        let answered = find_one(&mut client, case, 3, group, GROUP_KEY).await;
        assert_eq!(
            answered.error_code, 0,
            "case {case:?}: the lookup of {group} must be served, got error {}",
            answered.error_code
        );
    }
}

/// `FindCoordinator` for one key, at the version a client negotiated.
async fn find_one(
    client: &mut Client,
    case: &str,
    version: i16,
    key: &str,
    key_type: i8,
) -> FindCoordinatorResponse {
    client
        .call(
            case,
            ApiKey::FindCoordinator,
            version,
            &FindCoordinatorRequest::default()
                .with_key(StrBytes::from_string(key.to_string()))
                .with_key_type(key_type),
        )
        .await
}

/// A cluster whose only access path is SASL/IAM over TLS, which is the path MSK's IAM
/// access control governs.
async fn given_iam_cluster(emulator: &Emulator, name: &str) -> String {
    emulator
        .create_ok(&ClusterRequest {
            client_broker: Some("TLS"),
            sasl_iam: Some(true),
            ..ClusterRequest::named(name)
        })
        .await
}

/// Record the principal `access_key_id` speaks for and the policies in force on it, as
/// `sts:AssumeRole` and `iam:PutRolePolicy` record them.
fn given_principal(emulator: &Emulator, role: &str, access_key_id: &str, policy: Option<&Value>) {
    given_assumed_role_session(&emulator.engine, role, "session-1", access_key_id);
    given_role_inline_policy(&emulator.engine, role, policy);
}

/// A client on the SASL/IAM path, authenticated with the credential `access_key_id`
/// names: the exchange Amazon MSK's IAM access control defines.
async fn iam_client(case: &str, authority: &str, access_key_id: &str) -> Client {
    let mut client = Client::tls(case, authority).await;
    client.api_versions(case).await;
    let handshake: SaslHandshakeResponse = client
        .call(
            case,
            ApiKey::SaslHandshake,
            1,
            &SaslHandshakeRequest::default().with_mechanism(StrBytes::from_static_str(AWS_MSK_IAM)),
        )
        .await;
    assert_eq!(
        handshake.error_code, 0,
        "case {case:?}: the cluster must offer the IAM mechanism, got error {}",
        handshake.error_code
    );
    let payload = json!({
        "version": "2020_10_22",
        "host": LOOPBACK,
        "user-agent": CLIENT_ID,
        "action": "kafka-cluster:Connect",
        "x-amz-algorithm": "AWS4-HMAC-SHA256",
        "x-amz-credential": format!("{access_key_id}/20261101/{REGION}/kafka-cluster/aws4_request"),
        "x-amz-date": "20261101T000000Z",
        "x-amz-signedheaders": "host",
        "x-amz-signature": "a-signature",
    });
    let authenticated: SaslAuthenticateResponse = client
        .call(
            case,
            ApiKey::SaslAuthenticate,
            2,
            &SaslAuthenticateRequest::default()
                .with_auth_bytes(Bytes::from(payload.to_string().into_bytes())),
        )
        .await;
    assert_eq!(
        authenticated.error_code, 0,
        "case {case:?}: the IAM exchange must authenticate the connection, got error {}",
        authenticated.error_code
    );
    client
}
