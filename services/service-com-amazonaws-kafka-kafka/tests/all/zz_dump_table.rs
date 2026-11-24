// SPDX-License-Identifier: CC0-1.0

//! The FLOOR of each API version range the broker advertises.
//!
//! RULE under test: the versions a broker advertises for an API are the versions a broker
//! of the Apache Kafka release the cluster reports answers — so the range of the APIs a
//! client's produce/fetch round trip and its topic administration are made of opens at
//! version 0, the oldest version every 3.x release answers, and the table names no api key
//! that is not an Apache Kafka API. A client that frames the protocol itself picks the
//! OLDEST form it can write, and a broker that advertises only the newer versions answers
//! such a client by closing the connection — which is what Apache Kafka does with a frame
//! it will not parse — leaving it with no error code to branch on.
//!
//! That every advertised pair is ANSWERED is `broker_advertised_api_versions`'s subject;
//! this suite is about what the table OPENS at.

use ::kafka_protocol::messages::ApiKey;

use crate::broker_data_plane::Client;
use crate::kafka_harness::{ClusterRequest, Emulator};

/// RULE: the advertised range of the produce, fetch and topic-administration APIs opens at
/// version 0, and every api key the table names is an Apache Kafka API.
#[tokio::test]
async fn the_advertised_range_of_the_data_plane_apis_opens_at_the_oldest_version() {
    let case = "a cluster reached in the clear";
    let emulator = Emulator::new();
    let cluster = emulator
        .create_ok(&ClusterRequest {
            client_broker: Some("PLAINTEXT"),
            ..ClusterRequest::named("advertised-floors")
        })
        .await;
    let authority = emulator
        .published_bootstrap(&cluster, "bootstrapBrokerString", case)
        .await;
    let mut client = Client::plaintext(case, &authority).await;
    let advertised = client.api_versions(case).await;
    assert!(
        !advertised.api_keys.is_empty(),
        "case {case:?}: the broker must advertise the APIs it serves"
    );

    // The APIs a client's own round trip is made of: it produces, it fetches, and an admin
    // client creates and deletes the topic it does both over. Each is an API every Apache
    // Kafka 3.x release answers from version 0.
    let rows: &[(&str, ApiKey)] = &[
        ("Produce", ApiKey::Produce),
        ("Fetch", ApiKey::Fetch),
        ("CreateTopics", ApiKey::CreateTopics),
        ("DeleteTopics", ApiKey::DeleteTopics),
    ];
    for (row, key) in rows {
        let advertised = advertised
            .api_keys
            .iter()
            .find(|api| api.api_key == *key as i16)
            .unwrap_or_else(|| panic!("case {row:?}: the broker advertises no {key:?}"));
        assert_eq!(
            advertised.min_version, 0,
            "case {row:?}: expected the range to open at version 0, which every Apache Kafka \
             3.x broker answers, got {}..={}",
            advertised.min_version, advertised.max_version
        );
        assert!(
            advertised.max_version >= advertised.min_version,
            "case {row:?}: expected a range, got {}..={}",
            advertised.min_version,
            advertised.max_version
        );
    }

    // And nothing in the table is an api key that names no Apache Kafka API: a client reads
    // the whole table and negotiates against it.
    for api in &advertised.api_keys {
        assert!(
            ApiKey::try_from(api.api_key).is_ok(),
            "case {case:?}: the broker advertises api key {}, which names no Kafka API",
            api.api_key
        );
    }
}
