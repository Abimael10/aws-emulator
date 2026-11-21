// SPDX-License-Identifier: CC0-1.0

//! Every API VERSION the broker ADVERTISES, driven over a published bootstrap address.
//!
//! RULE under test: a broker answers every `(api key, version)` pair it advertises in
//! `ApiVersions` with a well-formed response frame in THAT version. A client negotiates
//! the version it sends from the advertisement, and Apache Kafka answers a frame it
//! cannot serve by closing the connection — so a pair that is advertised and then
//! dropped leaves the client reconnecting against a socket that disconnects it, with no
//! error code to branch on. A response may carry only the fields its version defines, so
//! what the broker reports has to be narrowed to the negotiated version rather than
//! framed in the newest one.
//!
//! The rows are the WHOLE advertised table, read from the broker's own `ApiVersions`
//! answer: every api key it names, at every version from its `min_version` to its
//! `max_version` — so an API that gains a response field, or a version range that is
//! widened, is covered the moment it is advertised, and the suite spans the oldest
//! version a client may pick as well as the newest.
//!
//! Every call goes over a real socket the core opened for the cluster, dialled at the
//! address `GetBootstrapBrokers` published.

use ::kafka_protocol::messages::{
    ApiKey, ApiVersionsRequest, ApiVersionsResponse, CreateAclsRequest, CreateAclsResponse,
    CreatePartitionsRequest, CreatePartitionsResponse, CreateTopicsRequest, CreateTopicsResponse,
    DeleteAclsRequest, DeleteAclsResponse, DeleteGroupsRequest, DeleteGroupsResponse,
    DeleteTopicsRequest, DeleteTopicsResponse, DescribeAclsRequest, DescribeAclsResponse,
    DescribeConfigsRequest, DescribeConfigsResponse, DescribeGroupsRequest, DescribeGroupsResponse,
    FetchRequest, FetchResponse, FindCoordinatorRequest, FindCoordinatorResponse, HeartbeatRequest,
    HeartbeatResponse, InitProducerIdRequest, InitProducerIdResponse, JoinGroupRequest,
    JoinGroupResponse, LeaveGroupRequest, LeaveGroupResponse, ListGroupsRequest, ListGroupsResponse,
    ListOffsetsRequest, ListOffsetsResponse, MetadataRequest, MetadataResponse,
    OffsetCommitRequest, OffsetCommitResponse, OffsetFetchRequest, OffsetFetchResponse,
    OffsetForLeaderEpochRequest, OffsetForLeaderEpochResponse, ProduceRequest, ProduceResponse,
    SaslAuthenticateRequest, SaslAuthenticateResponse, SaslHandshakeRequest, SaslHandshakeResponse,
    SyncGroupRequest, SyncGroupResponse,
};

use crate::broker_data_plane::Client;
use crate::kafka_harness::{ClusterRequest, Emulator};

/// Send one request of `key` at `version` and read the response back, decoding it in the
/// same version: the broker must answer, and the answer must be readable as that
/// version's response.
///
/// One arm per api key the table may advertise, so a key the broker starts advertising
/// without a probe here fails loudly rather than being skipped. Every request is the
/// minimal one a client can send — what is under test is the ANSWER, not the work the
/// request asks for.
async fn probe(client: &mut Client, case: &str, key: ApiKey, version: i16) {
    match key {
        ApiKey::Produce => {
            let _: ProduceResponse = client
                .call(
                    case,
                    key,
                    version,
                    &ProduceRequest::default().with_acks(1).with_timeout_ms(1_000),
                )
                .await;
        }
        ApiKey::Fetch => {
            let _: FetchResponse = client.call(case, key, version, &FetchRequest::default()).await;
        }
        ApiKey::ListOffsets => {
            let _: ListOffsetsResponse = client
                .call(case, key, version, &ListOffsetsRequest::default())
                .await;
        }
        ApiKey::Metadata => {
            let _: MetadataResponse = client
                .call(case, key, version, &MetadataRequest::default())
                .await;
        }
        ApiKey::OffsetCommit => {
            let _: OffsetCommitResponse = client
                .call(case, key, version, &OffsetCommitRequest::default())
                .await;
        }
        ApiKey::OffsetFetch => {
            let _: OffsetFetchResponse = client
                .call(case, key, version, &OffsetFetchRequest::default())
                .await;
        }
        ApiKey::FindCoordinator => {
            let _: FindCoordinatorResponse = client
                .call(case, key, version, &FindCoordinatorRequest::default())
                .await;
        }
        ApiKey::JoinGroup => {
            let _: JoinGroupResponse = client
                .call(case, key, version, &JoinGroupRequest::default())
                .await;
        }
        ApiKey::Heartbeat => {
            let _: HeartbeatResponse = client
                .call(case, key, version, &HeartbeatRequest::default())
                .await;
        }
        ApiKey::LeaveGroup => {
            let _: LeaveGroupResponse = client
                .call(case, key, version, &LeaveGroupRequest::default())
                .await;
        }
        ApiKey::SyncGroup => {
            let _: SyncGroupResponse = client
                .call(case, key, version, &SyncGroupRequest::default())
                .await;
        }
        ApiKey::DescribeGroups => {
            let _: DescribeGroupsResponse = client
                .call(case, key, version, &DescribeGroupsRequest::default())
                .await;
        }
        ApiKey::ListGroups => {
            let _: ListGroupsResponse = client
                .call(case, key, version, &ListGroupsRequest::default())
                .await;
        }
        ApiKey::SaslHandshake => {
            let _: SaslHandshakeResponse = client
                .call(case, key, version, &SaslHandshakeRequest::default())
                .await;
        }
        ApiKey::ApiVersions => {
            let _: ApiVersionsResponse = client
                .call(case, key, version, &ApiVersionsRequest::default())
                .await;
        }
        ApiKey::CreateTopics => {
            let _: CreateTopicsResponse = client
                .call(
                    case,
                    key,
                    version,
                    &CreateTopicsRequest::default()
                        .with_timeout_ms(1_000)
                        .with_topics(vec![
                            ::kafka_protocol::messages::create_topics_request::CreatableTopic::default()
                                .with_name(::kafka_protocol::messages::TopicName(
                                    ::kafka_protocol::protocol::StrBytes::from_string(format!(
                                        "versions-{}-{version}",
                                        key as i16
                                    )),
                                ))
                                // The count and the factor the BROKER decides, which is
                                // what the fields a newer response carries report back.
                                .with_num_partitions(-1)
                                .with_replication_factor(-1),
                        ]),
                )
                .await;
        }
        ApiKey::DeleteTopics => {
            let _: DeleteTopicsResponse = client
                .call(
                    case,
                    key,
                    version,
                    &DeleteTopicsRequest::default().with_timeout_ms(1_000),
                )
                .await;
        }
        ApiKey::InitProducerId => {
            let _: InitProducerIdResponse = client
                .call(case, key, version, &InitProducerIdRequest::default())
                .await;
        }
        ApiKey::OffsetForLeaderEpoch => {
            let _: OffsetForLeaderEpochResponse = client
                .call(case, key, version, &OffsetForLeaderEpochRequest::default())
                .await;
        }
        ApiKey::DescribeAcls => {
            let _: DescribeAclsResponse = client
                .call(case, key, version, &DescribeAclsRequest::default())
                .await;
        }
        ApiKey::CreateAcls => {
            let _: CreateAclsResponse = client
                .call(case, key, version, &CreateAclsRequest::default())
                .await;
        }
        ApiKey::DeleteAcls => {
            let _: DeleteAclsResponse = client
                .call(case, key, version, &DeleteAclsRequest::default())
                .await;
        }
        ApiKey::DescribeConfigs => {
            let _: DescribeConfigsResponse = client
                .call(case, key, version, &DescribeConfigsRequest::default())
                .await;
        }
        ApiKey::SaslAuthenticate => {
            let _: SaslAuthenticateResponse = client
                .call(case, key, version, &SaslAuthenticateRequest::default())
                .await;
        }
        ApiKey::CreatePartitions => {
            let _: CreatePartitionsResponse = client
                .call(
                    case,
                    key,
                    version,
                    &CreatePartitionsRequest::default().with_timeout_ms(1_000),
                )
                .await;
        }
        ApiKey::DeleteGroups => {
            let _: DeleteGroupsResponse = client
                .call(case, key, version, &DeleteGroupsRequest::default())
                .await;
        }
        other => panic!(
            "case {case:?}: the broker advertises {other:?} and this suite has no request for \
             it, so the version it answers is unchecked"
        ),
    }
}

#[tokio::test]
async fn every_advertised_api_version_is_answered() {
    let case = "a cluster reached in the clear";
    let emulator = Emulator::new();
    let cluster = emulator
        .create_ok(&ClusterRequest {
            client_broker: Some("PLAINTEXT"),
            brokers: 2,
            subnets: vec!["subnet-a", "subnet-b"],
            ..ClusterRequest::named("advertised-versions")
        })
        .await;
    // LOOKS AGAIN until the cluster publishes its endpoints rather than waiting out a
    // span: the move out of `CREATING` is a transition the clock owes it.
    let authority = emulator
        .published_bootstrap(&cluster, "bootstrapBrokerString", case)
        .await;

    // The table as the BROKER states it: what a client negotiates against.
    let mut client = Client::plaintext(case, &authority).await;
    let advertised = client.api_versions(case).await;
    assert_eq!(
        advertised.error_code, 0,
        "case {case:?}: ApiVersions must be answered, got error {}",
        advertised.error_code
    );
    let table: Vec<(i16, i16, i16)> = advertised
        .api_keys
        .iter()
        .map(|api| (api.api_key, api.min_version, api.max_version))
        .collect();
    assert!(
        !table.is_empty(),
        "case {case:?}: the broker must advertise the APIs it serves"
    );

    for (api_key, min_version, max_version) in table {
        let key = ApiKey::try_from(api_key).unwrap_or_else(|_| {
            panic!("case {case:?}: {api_key} is advertised and names no known API")
        });
        assert!(
            min_version >= 0 && min_version <= max_version,
            "case {case:?}: {key:?} is advertised over {min_version}..={max_version}"
        );
        for version in min_version..=max_version {
            let probed = format!("{case} / {key:?} v{version}");
            // A fresh connection per pair: a version that goes wrong must not be
            // diagnosed as the next pair's failure, and `SaslHandshake` leaves a
            // connection's negotiated mechanism behind it.
            let mut client = Client::plaintext(&probed, &authority).await;
            probe(&mut client, &probed, key, version).await;
        }
    }
}
