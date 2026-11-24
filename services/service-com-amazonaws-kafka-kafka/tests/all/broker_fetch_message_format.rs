// SPDX-License-Identifier: CC0-1.0

//! The MESSAGE FORMAT a `Fetch` is answered in.
//!
//! RULE under test: a broker hands a fetch its records in the message format the
//! NEGOTIATED version of the API carries, converting the log's own batches down to it —
//! the oldest message set (magic 0) for `Fetch` version 0 and 1, the message set that
//! added a timestamp (magic 1) for version 2 and 3, and the record batch (magic 2) from
//! version 4 on, the version it arrived with. A client that negotiated an older version
//! cannot read a newer batch, so a broker that always answers the newest format leaves
//! such a client reading NO records out of an answer that is not empty — a produce it was
//! told was accepted that it can never read back.
//!
//! The rows are the WHOLE advertised class, read from the broker's own `ApiVersions`
//! answer: every version from its `min_version` for `Fetch` to its `max_version`, so each
//! of the three formats is covered, and a version range that widens is covered the moment
//! it is advertised. What the producer wrote is one record batch of the current format, so
//! every row reads the same record back through a different conversion.
//!
//! Every assertion goes over a real socket the core opened for the cluster, dialled at the
//! address `GetBootstrapBrokers` published.

use ::kafka_protocol::messages::ApiKey;
use ::kafka_protocol::protocol::Message;
use ::kafka_protocol::records::RecordBatchDecoder;

use crate::broker_data_plane::{epoch_millis, given_topic, pre_flexible, Client};
use crate::kafka_harness::{ClusterRequest, Emulator};

/// Where the MAGIC byte of an answer sits, which both framings put in the same place: a
/// record batch opens with its base offset (8), its length (4) and its partition leader
/// epoch (4), and a legacy message with its offset (8), its size (4) and its checksum (4)
/// — so the magic is the seventeenth byte either way (Apache Kafka message formats).
const MAGIC_AT: usize = 16;

/// The key and value the one produced record carries.
const KEY: &str = "order-7";
const VALUE: &str = "the record the producer sent";

/// RULE: every version of `Fetch` the broker advertises answers with the message format
/// that version carries, and the record the producer sent is readable in it.
#[tokio::test]
async fn a_fetch_is_answered_in_the_message_format_its_version_carries() {
    let case = "a cluster reached in the clear";
    let emulator = Emulator::new();
    let cluster = emulator
        .create_ok(&ClusterRequest {
            client_broker: Some("PLAINTEXT"),
            ..ClusterRequest::named("fetch-formats")
        })
        .await;
    let authority = emulator
        .published_bootstrap(&cluster, "bootstrapBrokerString", case)
        .await;
    given_topic(&emulator, &cluster, "orders", 1, None).await;

    // One record, written by a producer of the current format — which is what every
    // client since 0.11 writes, and so what the log holds.
    let timestamp = epoch_millis();
    let mut producer = Client::plaintext(case, &authority).await;
    producer.api_versions(case).await;
    producer
        .produce(case, "orders", 0, &[(timestamp, Some(KEY), VALUE)])
        .await;

    // The rows: the whole advertised range of the API, as the broker states it.
    let advertised = Client::plaintext(case, &authority)
        .await
        .api_versions(case)
        .await;
    let (min, max) = advertised
        .api_keys
        .iter()
        .find(|api| api.api_key == ApiKey::Fetch as i16)
        .map(|api| (api.min_version, api.max_version))
        .unwrap_or_else(|| panic!("case {case:?}: the broker advertises no Fetch"));
    assert!(
        min <= 1 && max >= 4,
        "case {case:?}: the advertised range {min}..={max} must span the three message \
         formats a client of this API can negotiate"
    );

    for version in min..=max {
        let row = format!("{case} / Fetch v{version}");
        // The format that version carries, from the published mapping rather than from
        // the code under test: version 0 and 1 carry the oldest message set, 2 and 3 the
        // one with a timestamp, and 4 on the record batch.
        let expected_magic: i8 = match version {
            0 | 1 => 0,
            2 | 3 => 1,
            _ => 2,
        };
        let mut client = Client::plaintext(&row, &authority).await;
        client.api_versions(&row).await;
        // The oldest versions of this API are ones the protocol crate cannot encode, so a
        // client of them frames the request itself — which is exactly the client that
        // needs the conversion.
        let (high_watermark, records) = match version < ::kafka_protocol::messages::FetchRequest::VERSIONS.min
        {
            true => {
                let answered = client
                    .exchange_pre_flexible(
                        ApiKey::Fetch,
                        version,
                        &pre_flexible::fetch_body(version, "orders", 0, 0, 100),
                    )
                    .await
                    .unwrap_or_else(|why| panic!("case {row:?}: {why}"));
                let read = pre_flexible::fetch_results(&answered, version)
                    .unwrap_or_else(|why| panic!("case {row:?}: {why}"));
                let partition = read
                    .first()
                    .cloned()
                    .unwrap_or_else(|| panic!("case {row:?}: Fetch reported no partition"));
                assert_eq!(
                    partition.2, 0,
                    "case {row:?}: Fetch must be answered, got error {}",
                    partition.2
                );
                (partition.3, partition.4)
            }
            false => client.fetch_at(&row, version, "orders", 0, 0).await,
        };
        assert_eq!(
            high_watermark, 1,
            "case {row:?}: the partition holds the one produced record, so its high \
             watermark is 1, got {high_watermark}"
        );
        assert!(
            !records.is_empty(),
            "case {row:?}: a fetch from offset 0 must answer the record the produce was \
             acknowledged for, got an empty record set"
        );
        let magic = records
            .get(MAGIC_AT)
            .map(|byte| *byte as i8)
            .unwrap_or_else(|| panic!("case {row:?}: the answer is too short to be a batch"));
        assert_eq!(
            magic, expected_magic,
            "case {row:?}: expected the answer in message format v{expected_magic}, which \
             is what a client of this version reads, got v{magic}"
        );

        // And the record itself is the one the producer sent: a client of this version
        // reads its key and its value back, out of the format that version carries.
        let read: Vec<(i64, Option<String>, Option<String>, i64)> = match expected_magic {
            2 => {
                let mut encoded = records.clone();
                RecordBatchDecoder::decode(&mut encoded)
                    .unwrap_or_else(|error| {
                        panic!("case {row:?}: the answer must decode: {error}")
                    })
                    .records
                    .iter()
                    .map(|record| {
                        (
                            record.offset,
                            record
                                .key
                                .as_ref()
                                .map(|key| String::from_utf8_lossy(key).to_string()),
                            record
                                .value
                                .as_ref()
                                .map(|value| String::from_utf8_lossy(value).to_string()),
                            record.timestamp,
                        )
                    })
                    .collect()
            }
            _ => pre_flexible::message_set_records(&records)
                .unwrap_or_else(|why| panic!("case {row:?}: the answer must decode: {why}")),
        };
        // The timestamp travels only in the formats that carry one: the oldest has no
        // field for it, so a client of version 0 or 1 is told there is none.
        let expected_timestamp = match expected_magic {
            0 => -1,
            _ => timestamp,
        };
        assert_eq!(
            read,
            vec![(
                0,
                Some(KEY.to_string()),
                Some(VALUE.to_string()),
                expected_timestamp
            )],
            "case {row:?}: expected the produced record at offset 0, got {read:?}"
        );
    }
}
