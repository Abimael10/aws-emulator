// SPDX-License-Identifier: CC0-1.0

//! The TRANSACTIONS a producer runs over the published bootstrap address: the APIs the
//! broker advertises for them, and what committing or aborting one does to the log.
//!
//! RULE under test: a broker advertises the transaction APIs of the Kafka version it runs
//! and implements them. A producer that names a transactional id is assigned a producer id
//! and an epoch; the records it writes inside a transaction are appended at once but are
//! UNDECIDED — the partition's last stable offset stops at the first of them, so a
//! `read_committed` reader is answered none of them while a `read_uncommitted` one is
//! answered all — and ending the transaction appends one `COMMIT` or `ABORT` control batch
//! per partition it touched, each taking an offset of its own. After a commit the records
//! are the ones a committed reader is given; after an abort they stay in the log, are
//! reported as an aborted transaction and are the ones such a reader drops. A second
//! `InitProducerId` on one transactional id raises the epoch, which aborts the transaction
//! the previous producer left open and refuses everything it sends afterwards.
//!
//! The rows span what the rule generalises over: which api key and version the client
//! negotiates, whether the transaction commits or aborts, whether it is still open,
//! whether one partition or two are enlisted, which isolation level reads it, and whether
//! the producer holding the transactional id is the one in force.
//!
//! Every assertion goes over a real socket the core opened for the cluster, dialled at the
//! address `GetBootstrapBrokers` published. What a batch carries is read back with the
//! protocol implementation's own decoder
//! ([`::kafka_protocol::records::RecordBatchDecoder`]) — an independent reading of the
//! bytes — and the visible records are then filtered exactly as a client filters them:
//! control batches are dropped, and so are the records of the producers the fetch reported
//! as aborted.

use ::bytes::{Buf, Bytes};
use ::kafka_protocol::messages::{
    add_partitions_to_txn_request::AddPartitionsToTxnTopic,
    fetch_request::{FetchPartition, FetchTopic},
    list_offsets_request::{ListOffsetsPartition, ListOffsetsTopic},
    offset_fetch_request::OffsetFetchRequestTopic,
    produce_request::{PartitionProduceData, TopicProduceData},
    txn_offset_commit_request::{TxnOffsetCommitRequestPartition, TxnOffsetCommitRequestTopic},
    AddOffsetsToTxnRequest, AddOffsetsToTxnResponse, AddPartitionsToTxnRequest,
    AddPartitionsToTxnResponse, ApiKey, EndTxnRequest, EndTxnResponse, FetchRequest, FetchResponse,
    GroupId, InitProducerIdRequest, InitProducerIdResponse, ListOffsetsRequest, ListOffsetsResponse,
    OffsetFetchRequest, OffsetFetchResponse, ProduceRequest, ProduceResponse, ProducerId, TopicName,
    TransactionalId, TxnOffsetCommitRequest, TxnOffsetCommitResponse,
};
use ::kafka_protocol::protocol::StrBytes;
use ::kafka_protocol::records::{Record, RecordBatchDecoder};
use ::kafka_protocol::ResponseError;
use serde_json::json;
use service_com_amazonaws_kafka_kafka::state::broker_plane::{
    records, RecordCodec, RecordProducer, StoredRecord,
};

use crate::broker_data_plane::Client;
use crate::kafka_harness::{ClusterRequest, Emulator};

/// The two values the protocol's `IsolationLevel` takes: `0` reads every record in the
/// log and `1` only the decided ones (Apache Kafka protocol, `Fetch`/`ListOffsets`).
const READ_UNCOMMITTED: i8 = 0;
const READ_COMMITTED: i8 = 1;

/// The timestamp a transaction's producer asks for, in milliseconds: well above the time
/// a row of this suite takes, so no row depends on how fast the host runs.
const TRANSACTION_TIMEOUT_MS: i32 = 60_000;

/// The timestamps the records of a row carry: stated by the producer, so the offsets and
/// the order a row asserts do not depend on when it ran.
const BASE_TIMESTAMP_MS: i64 = 1_700_000_000_000;

/// The offset a `ListOffsets` request asks for the END of the log with (Apache Kafka
/// protocol: `-1` is the latest offset).
const OFFSET_LATEST: i64 = -1;

/// One transaction of the cycle below: what it is called, whether it COMMITS, the values
/// it writes, the offsets its produce must be acknowledged at, and the offset its marker
/// must take.
type CycleRow<'a> = (&'a str, bool, &'a [&'a str], &'a [i64], i64);

/// One isolation row: what it is called, the level it reads at, the latest offset such a
/// reader must be answered, and the records it must surface, as `(offset, value)`.
type IsolationRow = (&'static str, i8, i64, Vec<(i64, String)>);

/// One refusal row: what it is called, the transactional id the call names, the producer
/// it quotes, and the protocol error it must be answered with.
type RefusalRow = (&'static str, &'static str, Producer, i16);

/// RULE: the broker advertises every API the Kafka transaction protocol defines, so a
/// client with a transactional id can begin a transaction at all.
///
/// The rows are the api keys that protocol is made of, each named by its number as the
/// Apache Kafka protocol assigns it (`InitProducerId` 22, `AddPartitionsToTxn` 24,
/// `AddOffsetsToTxn` 25, `EndTxn` 26, `TxnOffsetCommit` 28) — including keys this issue's
/// own client never sends, because a client reads the whole set before it decides the
/// cluster can run transactions.
#[tokio::test]
async fn the_broker_advertises_every_api_the_transaction_protocol_is_made_of() {
    let case = "the advertised transaction APIs";
    let emulator = Emulator::new();
    let cluster = given_cluster(&emulator, "txn-advertised").await;
    let authority = address(&emulator, &cluster, case).await;
    let mut client = Client::plaintext(case, &authority).await;
    let advertised = client.api_versions(case).await;

    let rows: &[(&str, i16)] = &[
        ("InitProducerId", 22),
        ("AddPartitionsToTxn", 24),
        ("AddOffsetsToTxn", 25),
        ("EndTxn", 26),
        ("TxnOffsetCommit", 28),
    ];
    for (name, api_key) in rows {
        let found = advertised
            .api_keys
            .iter()
            .find(|api| api.api_key == *api_key);
        let api = found.unwrap_or_else(|| {
            panic!(
                "case {case:?}: the broker must advertise {name} (api key {api_key}), it \
                 advertised {:?}",
                advertised
                    .api_keys
                    .iter()
                    .map(|api| api.api_key)
                    .collect::<Vec<i16>>()
            )
        });
        assert!(
            api.min_version >= 0 && api.min_version <= api.max_version,
            "case {case:?}: {name} must be advertised over a usable range, got \
             {}..={}",
            api.min_version,
            api.max_version
        );
    }
}

/// RULE: a transaction's records are decided by its MARKER, and the marker takes an offset
/// of its own — so a producer running several transactions over one partition is
/// acknowledged at offsets with a gap at each marker, a committed reader is given the
/// records of the committed transactions only, and an uncommitted reader is given every
/// record the log holds.
///
/// The rows are the transactions of one cycle, differing in how they end and in how many
/// records they carry: a commit of two records, an abort of one, and a commit of one.
#[tokio::test]
async fn each_transactions_marker_decides_its_records_and_occupies_one_offset() {
    let case = "a commit, an abort and a commit over one partition";
    let emulator = Emulator::new();
    let cluster = given_cluster(&emulator, "txn-cycle").await;
    let authority = address(&emulator, &cluster, case).await;
    let topic = "cycle";
    given_topic(&emulator, &cluster, topic, 1).await;
    let mut client = Client::plaintext(case, &authority).await;
    client.api_versions(case).await;

    let transactional_id = "cycle-producer";
    let producer = begin_producer(&mut client, case, transactional_id, 2).await;
    assert_eq!(
        producer.epoch, 0,
        "case {case:?}: a transactional id nobody has taken over must be at the first \
         epoch, got {}",
        producer.epoch
    );

    // (how the transaction ends, the values it writes, the offsets the produce must be
    // acknowledged at, the offset its marker must take).
    let rows: &[CycleRow<'_>] = &[
        ("a commit of two records", true, &["A1", "A2"], &[0, 1], 2),
        ("an abort of one record", false, &["B1"], &[3], 4),
        ("a commit of one record", true, &["C1"], &[5], 6),
    ];
    for (row, committed, values, offsets, marker) in rows {
        let case = &format!("{case} / {row}");
        add_partitions(&mut client, case, transactional_id, producer, topic, &[0], 3).await;
        let base = produce_ok(&mut client, case, topic, 0, values, Some(producer)).await;
        assert_eq!(
            base,
            offsets[0],
            "case {case:?}: the batch must be acknowledged at offset {}, got {base}",
            offsets[0]
        );
        let ended = end_transaction(&mut client, case, transactional_id, producer, *committed, 1)
            .await;
        assert_eq!(
            ended.error_code, 0,
            "case {case:?}: the transaction must end, got error {}",
            ended.error_code
        );
        let end_offset = latest_offset(&mut client, case, topic, 0, READ_UNCOMMITTED).await;
        assert_eq!(
            end_offset,
            marker + 1,
            "case {case:?}: the marker must take offset {marker}, so the log must end at \
             {}, got {end_offset}",
            marker + 1
        );
    }

    // Every record the log holds, read as the protocol carries them: the three batches
    // and the three markers, each marker a control batch of its own.
    let read = fetched(&mut client, case, topic, 0, 0, READ_UNCOMMITTED).await;
    assert_eq!(
        (read.high_watermark, read.last_stable_offset),
        (7, 7),
        "case {case:?}: with nothing open the log must end at 7 and be decided to 7, got \
         {} and {}",
        read.high_watermark,
        read.last_stable_offset
    );
    assert!(
        read.aborted.is_empty(),
        "case {case:?}: an uncommitted read decides nothing by a transaction, so no \
         aborted transaction is reported to it, got {:?}",
        read.aborted
    );
    assert_eq!(
        read.records
            .iter()
            .map(|record| (record.offset, record.control))
            .collect::<Vec<(i64, bool)>>(),
        vec![
            (0, false),
            (1, false),
            (2, true),
            (3, false),
            (4, true),
            (5, false),
            (6, true)
        ],
        "case {case:?}: each marker must occupy one offset as a control batch, got {:?}",
        read.records
            .iter()
            .map(|record| (record.offset, record.control))
            .collect::<Vec<(i64, bool)>>()
    );
    assert_eq!(
        visible(&read),
        vec![(0, "A1".to_string()), (1, "A2".to_string()), (3, "B1".to_string()), (5, "C1".to_string())],
        "case {case:?}: an uncommitted reader must be given every record the log holds, \
         got {:?}",
        visible(&read)
    );

    // And the same log read by a COMMITTED consumer: the aborted transaction is reported,
    // and its record is the one such a consumer drops.
    let committed = fetched(&mut client, case, topic, 0, 0, READ_COMMITTED).await;
    assert_eq!(
        committed
            .aborted
            .iter()
            .map(|(producer_id, first_offset)| (*producer_id == producer.id, *first_offset))
            .collect::<Vec<(bool, i64)>>(),
        vec![(true, 3)],
        "case {case:?}: the aborted transaction must be reported from the first offset it \
         wrote, got {:?}",
        committed.aborted
    );
    assert_eq!(
        visible(&committed),
        vec![(0, "A1".to_string()), (1, "A2".to_string()), (5, "C1".to_string())],
        "case {case:?}: a committed reader must be given the committed records only, got \
         {:?}",
        visible(&committed)
    );
}

/// RULE: while a transaction is OPEN its records are undecided: the partition's last
/// stable offset stops at the first of them, so a committed reader is answered none of
/// them and told the log ends there, while an uncommitted reader is answered all of them
/// and told where the log really ends.
///
/// The rows are the two isolation levels, read at the same instant over the same
/// partition.
#[tokio::test]
async fn an_open_transactions_records_are_read_only_by_an_uncommitted_reader() {
    let case = "a transaction still open";
    let emulator = Emulator::new();
    let cluster = given_cluster(&emulator, "txn-open").await;
    let authority = address(&emulator, &cluster, case).await;
    let topic = "open";
    given_topic(&emulator, &cluster, topic, 1).await;
    let mut client = Client::plaintext(case, &authority).await;
    client.api_versions(case).await;

    // One committed record first, so the rows below have a decided prefix: what a
    // committed reader is given is that prefix and nothing of the open transaction.
    let transactional_id = "open-producer";
    let producer = begin_producer(&mut client, case, transactional_id, 4).await;
    add_partitions(&mut client, case, transactional_id, producer, topic, &[0], 0).await;
    produce_ok(&mut client, case, topic, 0, &["settled"], Some(producer)).await;
    end_transaction(&mut client, case, transactional_id, producer, true, 3).await;
    // And then a transaction that is left open.
    add_partitions(&mut client, case, transactional_id, producer, topic, &[0], 3).await;
    produce_ok(&mut client, case, topic, 0, &["open-1", "open-2"], Some(producer)).await;

    let rows: &[IsolationRow] = &[
        (
            "an uncommitted reader",
            READ_UNCOMMITTED,
            4,
            vec![
                (0, "settled".to_string()),
                (2, "open-1".to_string()),
                (3, "open-2".to_string()),
            ],
        ),
        (
            "a committed reader",
            READ_COMMITTED,
            2,
            vec![(0, "settled".to_string())],
        ),
    ];
    for (row, isolation, end_offset, expected) in rows {
        let case = &format!("{case} / {row}");
        let read = fetched(&mut client, case, topic, 0, 0, *isolation).await;
        assert_eq!(
            visible(&read),
            *expected,
            "case {case:?}: expected {expected:?}, got {:?}",
            visible(&read)
        );
        assert_eq!(
            (read.high_watermark, read.last_stable_offset),
            (4, 2),
            "case {case:?}: the log must end at 4 and be decided to 2, got {} and {}",
            read.high_watermark,
            read.last_stable_offset
        );
        let latest = latest_offset(&mut client, case, topic, 0, *isolation).await;
        assert_eq!(
            latest, *end_offset,
            "case {case:?}: the latest offset must be {end_offset}, got {latest}"
        );
    }
}

/// RULE: a second `InitProducerId` on one transactional id hands it to the producer that
/// asked, at a higher epoch: the transaction the previous producer left open is aborted,
/// everything that producer sends afterwards is refused — its `EndTxn` as
/// `PRODUCER_FENCED` and its produce as `INVALID_PRODUCER_EPOCH` — and the producer now
/// holding the id runs its own transaction to a commit.
#[tokio::test]
async fn a_producer_that_takes_the_transactional_id_over_fences_the_one_before_it() {
    let case = "a transactional id taken over";
    let emulator = Emulator::new();
    let cluster = given_cluster(&emulator, "txn-fence").await;
    let authority = address(&emulator, &cluster, case).await;
    let topic = "fenced";
    given_topic(&emulator, &cluster, topic, 1).await;
    let mut client = Client::plaintext(case, &authority).await;
    client.api_versions(case).await;

    let transactional_id = "fenced-producer";
    let first = begin_producer(&mut client, case, transactional_id, 2).await;
    add_partitions(&mut client, case, transactional_id, first, topic, &[0], 3).await;
    produce_ok(&mut client, case, topic, 0, &["fenced"], Some(first)).await;

    let second = begin_producer(&mut client, case, transactional_id, 2).await;
    assert_eq!(
        (second.id, second.epoch),
        (first.id, first.epoch + 1),
        "case {case:?}: the id must stay and the epoch must rise, got {second:?} after \
         {first:?}"
    );

    let refused = end_transaction(&mut client, case, transactional_id, first, true, 1).await;
    assert_eq!(
        refused.error_code,
        ResponseError::ProducerFenced.code(),
        "case {case:?}: the fenced producer's commit must be refused PRODUCER_FENCED, got \
         {}",
        refused.error_code
    );
    let appended = produce(&mut client, case, topic, 0, &["after-fencing"], Some(first)).await;
    assert_eq!(
        appended.error_code,
        ResponseError::InvalidProducerEpoch.code(),
        "case {case:?}: the fenced producer's produce must be refused \
         INVALID_PRODUCER_EPOCH, got {}",
        appended.error_code
    );

    // The record the fenced producer had appended was aborted when the id changed hands:
    // it is in the log for an uncommitted reader and gone for a committed one.
    let uncommitted = fetched(&mut client, case, topic, 0, 0, READ_UNCOMMITTED).await;
    assert_eq!(
        visible(&uncommitted),
        vec![(0, "fenced".to_string())],
        "case {case:?}: an uncommitted reader must still be given the fenced record, got \
         {:?}",
        visible(&uncommitted)
    );
    let committed = fetched(&mut client, case, topic, 0, 0, READ_COMMITTED).await;
    assert!(
        visible(&committed).is_empty(),
        "case {case:?}: a committed reader must be given none of the aborted record, got \
         {:?}",
        visible(&committed)
    );

    // And the producer now holding the id commits its own transaction over the same
    // partition.
    add_partitions(&mut client, case, transactional_id, second, topic, &[0], 3).await;
    produce_ok(&mut client, case, topic, 0, &["taken-over"], Some(second)).await;
    let ended = end_transaction(&mut client, case, transactional_id, second, true, 1).await;
    assert_eq!(
        ended.error_code, 0,
        "case {case:?}: the producer holding the id must commit, got error {}",
        ended.error_code
    );
    let after = fetched(&mut client, case, topic, 0, 0, READ_COMMITTED).await;
    assert_eq!(
        visible(&after),
        vec![(2, "taken-over".to_string())],
        "case {case:?}: the committed record must be the one the holder wrote, at the \
         offset after the abort marker the takeover wrote, got {:?}",
        visible(&after)
    );
}

/// RULE: a transaction touching SEVERAL partitions is marked in each of them — the
/// coordinator writes one marker per partition the transaction enlisted, so every
/// partition's records are decided and none is left undecided behind.
#[tokio::test]
async fn a_transaction_over_several_partitions_is_marked_in_each_of_them() {
    let case = "a transaction over two partitions";
    let emulator = Emulator::new();
    let cluster = given_cluster(&emulator, "txn-spread").await;
    let authority = address(&emulator, &cluster, case).await;
    let topic = "spread";
    given_topic(&emulator, &cluster, topic, 2).await;
    let mut client = Client::plaintext(case, &authority).await;
    client.api_versions(case).await;

    let transactional_id = "spread-producer";
    let producer = begin_producer(&mut client, case, transactional_id, 2).await;
    add_partitions(&mut client, case, transactional_id, producer, topic, &[0, 1], 3).await;
    for partition in [0, 1] {
        produce_ok(
            &mut client,
            case,
            topic,
            partition,
            &[&format!("p{partition}")],
            Some(producer),
        )
        .await;
    }
    end_transaction(&mut client, case, transactional_id, producer, true, 1).await;

    for partition in [0, 1] {
        let case = &format!("{case} / partition {partition}");
        let read = fetched(&mut client, case, topic, partition, 0, READ_COMMITTED).await;
        assert_eq!(
            (read.high_watermark, read.last_stable_offset),
            (2, 2),
            "case {case:?}: the marker must have taken offset 1 here, got {} and {}",
            read.high_watermark,
            read.last_stable_offset
        );
        assert_eq!(
            visible(&read),
            vec![(0, format!("p{partition}"))],
            "case {case:?}: the committed record of this partition must be readable, got \
             {:?}",
            visible(&read)
        );
    }
}

/// RULE: the consumer offsets a transaction carries become the group's committed offsets
/// only when the transaction COMMITS, and are thrown away when it aborts — which is what
/// makes a read-process-write loop exactly-once.
///
/// The rows differ along how the transaction carrying the offsets ends, and the position
/// the group holds is read back through the group's own `OffsetFetch`.
#[tokio::test]
async fn the_offsets_a_transaction_carries_are_the_groups_only_once_it_commits() {
    let case = "offsets sent to a transaction";
    let emulator = Emulator::new();
    let cluster = given_cluster(&emulator, "txn-offsets").await;
    let authority = address(&emulator, &cluster, case).await;
    let topic = "processed";
    given_topic(&emulator, &cluster, topic, 1).await;
    let mut client = Client::plaintext(case, &authority).await;
    client.api_versions(case).await;

    let transactional_id = "offset-producer";
    let producer = begin_producer(&mut client, case, transactional_id, 2).await;
    // (how the transaction ends, the position it sends, the position the group holds
    // afterwards). A group with no committed offset at all answers -1, which is what the
    // protocol says about a partition nothing has committed.
    let rows: &[(&str, bool, i64, i64)] = &[
        ("an aborted transaction", false, 11, -1),
        ("a committed transaction", true, 42, 42),
        ("an abort after a commit", false, 99, 42),
    ];
    for (row, committed, sent, held) in rows {
        let case = &format!("{case} / {row}");
        let group = "exactly-once-readers";
        let added = add_offsets(&mut client, case, transactional_id, producer, group, 3).await;
        assert_eq!(
            added.error_code, 0,
            "case {case:?}: the group must be added to the transaction, got error {}",
            added.error_code
        );
        let staged = commit_offsets(
            &mut client,
            case,
            transactional_id,
            producer,
            group,
            topic,
            *sent,
        )
        .await;
        assert_eq!(
            staged, 0,
            "case {case:?}: the position must be taken by the transaction, got error \
             {staged}"
        );
        // Nothing is the group's until the transaction ends: the position it holds while
        // the transaction is in flight is the one the previous row left.
        let before = committed_offset(&mut client, case, group, topic, 0).await;
        let expected_before = match row.starts_with("an abort after") {
            true => 42,
            false => -1,
        };
        assert_eq!(
            before, expected_before,
            "case {case:?}: an in-flight transaction must not move the group's position, \
             expected {expected_before}, got {before}"
        );
        end_transaction(&mut client, case, transactional_id, producer, *committed, 1).await;
        let after = committed_offset(&mut client, case, group, topic, 0).await;
        assert_eq!(
            after, *held,
            "case {case:?}: expected the group to hold {held}, got {after}"
        );
    }
}

/// RULE: a transaction call the coordinator cannot place is refused with the protocol's
/// own answer rather than acted on — an `EndTxn` for a transactional id it holds no record
/// of is `INVALID_PRODUCER_ID_MAPPING`, one for an id whose producer has begun nothing is
/// `INVALID_TXN_STATE`, and one quoting a producer id the id was never assigned is
/// `INVALID_PRODUCER_ID_MAPPING`.
#[tokio::test]
async fn a_transaction_call_the_coordinator_cannot_place_is_refused() {
    let case = "transaction calls with nothing behind them";
    let emulator = Emulator::new();
    let cluster = given_cluster(&emulator, "txn-refusals").await;
    let authority = address(&emulator, &cluster, case).await;
    let mut client = Client::plaintext(case, &authority).await;
    client.api_versions(case).await;

    let known = "known-producer";
    let producer = begin_producer(&mut client, case, known, 2).await;
    let unknown_producer = Producer {
        id: producer.id + 1_000,
        epoch: producer.epoch,
    };
    let rows: &[RefusalRow] = &[
        (
            "an id the coordinator holds no record of",
            "never-initialised",
            producer,
            ResponseError::InvalidProducerIdMapping.code(),
        ),
        (
            "a producer id the transactional id was never assigned",
            known,
            unknown_producer,
            ResponseError::InvalidProducerIdMapping.code(),
        ),
        (
            "an id whose producer has begun no transaction",
            known,
            producer,
            ResponseError::InvalidTxnState.code(),
        ),
    ];
    for (row, transactional_id, quoted, expected) in rows {
        let case = &format!("{case} / {row}");
        let refused = end_transaction(&mut client, case, transactional_id, *quoted, true, 1).await;
        assert_eq!(
            refused.error_code, *expected,
            "case {case:?}: expected error {expected}, got {}",
            refused.error_code
        );
    }
}

/// The producer one transactional id is held by: the id the coordinator assigned and the
/// epoch it is at.
#[derive(Debug, Clone, Copy)]
struct Producer {
    id: i64,
    epoch: i16,
}

/// What one fetch answered: the records of every batch it carried, the two watermarks it
/// reported and the transactions it reported as aborted.
struct Read {
    records: Vec<Record>,
    high_watermark: i64,
    last_stable_offset: i64,
    aborted: Vec<(i64, i64)>,
}

/// `InitProducerId` for a transactional id, at `version`: the producer id and epoch the
/// coordinator assigned.
async fn begin_producer(
    client: &mut Client,
    case: &str,
    transactional_id: &str,
    version: i16,
) -> Producer {
    let answered: InitProducerIdResponse = client
        .call(
            case,
            ApiKey::InitProducerId,
            version,
            &InitProducerIdRequest::default()
                .with_transactional_id(Some(TransactionalId(StrBytes::from_string(
                    transactional_id.to_string(),
                ))))
                .with_transaction_timeout_ms(TRANSACTION_TIMEOUT_MS),
        )
        .await;
    assert_eq!(
        answered.error_code, 0,
        "case {case:?}: InitProducerId must assign a producer, got error {}",
        answered.error_code
    );
    assert!(
        answered.producer_id.0 >= 0,
        "case {case:?}: the assigned producer id must be a real one, got {}",
        answered.producer_id.0
    );
    Producer {
        id: answered.producer_id.0,
        epoch: answered.producer_epoch,
    }
}

/// `AddPartitionsToTxn` at `version`, asserting every partition it named was taken.
async fn add_partitions(
    client: &mut Client,
    case: &str,
    transactional_id: &str,
    producer: Producer,
    topic: &str,
    partitions: &[i32],
    version: i16,
) {
    let answered: AddPartitionsToTxnResponse = client
        .call(
            case,
            ApiKey::AddPartitionsToTxn,
            version,
            &AddPartitionsToTxnRequest::default()
                .with_v3_and_below_transactional_id(TransactionalId(StrBytes::from_string(
                    transactional_id.to_string(),
                )))
                .with_v3_and_below_producer_id(ProducerId(producer.id))
                .with_v3_and_below_producer_epoch(producer.epoch)
                .with_v3_and_below_topics(vec![AddPartitionsToTxnTopic::default()
                    .with_name(TopicName(StrBytes::from_string(topic.to_string())))
                    .with_partitions(partitions.to_vec())]),
        )
        .await;
    let answered: Vec<(i32, i16)> = answered
        .results_by_topic_v3_and_below
        .iter()
        .flat_map(|topic| topic.results_by_partition.iter())
        .map(|partition| (partition.partition_index, partition.partition_error_code))
        .collect();
    assert_eq!(
        answered,
        partitions
            .iter()
            .map(|partition| (*partition, 0))
            .collect::<Vec<(i32, i16)>>(),
        "case {case:?}: every partition named must be added to the transaction, got \
         {answered:?}"
    );
}

/// `AddOffsetsToTxn` at `version`: the group whose offsets travel with the transaction.
async fn add_offsets(
    client: &mut Client,
    case: &str,
    transactional_id: &str,
    producer: Producer,
    group: &str,
    version: i16,
) -> AddOffsetsToTxnResponse {
    client
        .call(
            case,
            ApiKey::AddOffsetsToTxn,
            version,
            &AddOffsetsToTxnRequest::default()
                .with_transactional_id(TransactionalId(StrBytes::from_string(
                    transactional_id.to_string(),
                )))
                .with_producer_id(ProducerId(producer.id))
                .with_producer_epoch(producer.epoch)
                .with_group_id(GroupId(StrBytes::from_string(group.to_string()))),
        )
        .await
}

/// `TxnOffsetCommit`: the position the transaction carries for partition 0 of `topic`,
/// answering the protocol error code that partition was answered with.
async fn commit_offsets(
    client: &mut Client,
    case: &str,
    transactional_id: &str,
    producer: Producer,
    group: &str,
    topic: &str,
    offset: i64,
) -> i16 {
    let answered: TxnOffsetCommitResponse = client
        .call(
            case,
            ApiKey::TxnOffsetCommit,
            2,
            &TxnOffsetCommitRequest::default()
                .with_transactional_id(TransactionalId(StrBytes::from_string(
                    transactional_id.to_string(),
                )))
                .with_group_id(GroupId(StrBytes::from_string(group.to_string())))
                .with_producer_id(ProducerId(producer.id))
                .with_producer_epoch(producer.epoch)
                .with_topics(vec![TxnOffsetCommitRequestTopic::default()
                    .with_name(TopicName(StrBytes::from_string(topic.to_string())))
                    .with_partitions(vec![TxnOffsetCommitRequestPartition::default()
                        .with_partition_index(0)
                        .with_committed_offset(offset)])]),
        )
        .await;
    answered
        .topics
        .first()
        .and_then(|topic| topic.partitions.first())
        .map(|partition| partition.error_code)
        .unwrap_or_else(|| panic!("case {case:?}: TxnOffsetCommit reported no partition"))
}

/// `EndTxn` at `version`: commit or abort the transaction in flight.
async fn end_transaction(
    client: &mut Client,
    case: &str,
    transactional_id: &str,
    producer: Producer,
    committed: bool,
    version: i16,
) -> EndTxnResponse {
    client
        .call(
            case,
            ApiKey::EndTxn,
            version,
            &EndTxnRequest::default()
                .with_transactional_id(TransactionalId(StrBytes::from_string(
                    transactional_id.to_string(),
                )))
                .with_producer_id(ProducerId(producer.id))
                .with_producer_epoch(producer.epoch)
                .with_committed(committed),
        )
        .await
}

/// `Produce` one batch of `values`, written by `producer` inside a transaction when one
/// is given: the partition's own result.
async fn produce(
    client: &mut Client,
    case: &str,
    topic: &str,
    partition: i32,
    values: &[&str],
    producer: Option<Producer>,
) -> ::kafka_protocol::messages::produce_response::PartitionProduceResponse {
    let batch: Vec<StoredRecord> = values
        .iter()
        .enumerate()
        .map(|(index, value)| StoredRecord {
            offset: 0,
            timestamp_ms: BASE_TIMESTAMP_MS + i64::try_from(index).unwrap_or(0),
            log_append_time: false,
            key: None,
            value: Some(value.as_bytes().to_vec()),
            headers: Vec::new(),
            codec: RecordCodec::None,
            producer: producer.map(|producer| RecordProducer {
                id: producer.id,
                epoch: producer.epoch,
                transactional: true,
                control: false,
            }),
        })
        .collect();
    let payload = records::encode(&batch, RecordCodec::None, records::MAGIC_V2)
        .unwrap_or_else(|| panic!("case {case:?}: the batch must encode"));
    let answered: ProduceResponse = client
        .call(
            case,
            ApiKey::Produce,
            9,
            &ProduceRequest::default()
                .with_acks(1)
                .with_timeout_ms(1_000)
                .with_topic_data(vec![TopicProduceData::default()
                    .with_name(TopicName(StrBytes::from_string(topic.to_string())))
                    .with_partition_data(vec![PartitionProduceData::default()
                        .with_index(partition)
                        .with_records(Some(payload))])]),
        )
        .await;
    answered
        .responses
        .first()
        .and_then(|topic| topic.partition_responses.first())
        .unwrap_or_else(|| panic!("case {case:?}: Produce reported no partition"))
        .clone()
}

/// The same produce, asserting it was appended, answering the offset the batch starts at.
async fn produce_ok(
    client: &mut Client,
    case: &str,
    topic: &str,
    partition: i32,
    values: &[&str],
    producer: Option<Producer>,
) -> i64 {
    let appended = produce(client, case, topic, partition, values, producer).await;
    assert_eq!(
        appended.error_code, 0,
        "case {case:?}: the batch must be appended, got error {}",
        appended.error_code
    );
    appended.base_offset
}

/// `Fetch` one partition from `offset` at `isolation`, with every batch the answer
/// carried decoded by the protocol implementation's own decoder.
async fn fetched(
    client: &mut Client,
    case: &str,
    topic: &str,
    partition: i32,
    offset: i64,
    isolation: i8,
) -> Read {
    let answered: FetchResponse = client
        .call(
            case,
            ApiKey::Fetch,
            11,
            &FetchRequest::default()
                .with_max_wait_ms(100)
                .with_min_bytes(0)
                .with_max_bytes(1_048_576)
                .with_isolation_level(isolation)
                .with_topics(vec![FetchTopic::default()
                    .with_topic(TopicName(StrBytes::from_string(topic.to_string())))
                    .with_partitions(vec![FetchPartition::default()
                        .with_partition(partition)
                        .with_fetch_offset(offset)
                        .with_partition_max_bytes(1_048_576)])]),
        )
        .await;
    let data = answered
        .responses
        .first()
        .and_then(|topic| topic.partitions.first())
        .unwrap_or_else(|| panic!("case {case:?}: Fetch reported no partition"))
        .clone();
    assert_eq!(
        data.error_code, 0,
        "case {case:?}: Fetch must be answered, got error {}",
        data.error_code
    );
    Read {
        records: decoded(case, data.records.clone().unwrap_or_default()),
        high_watermark: data.high_watermark,
        last_stable_offset: data.last_stable_offset,
        aborted: data
            .aborted_transactions
            .clone()
            .unwrap_or_default()
            .iter()
            .map(|aborted| (aborted.producer_id.0, aborted.first_offset))
            .collect(),
    }
}

/// Every record of every batch a fetch answered, read by the protocol implementation's
/// own decoder: one answer may carry several batches, one after the other, exactly as a
/// log segment does.
fn decoded(case: &str, payload: Bytes) -> Vec<Record> {
    let mut buf = payload;
    let mut read = Vec::new();
    while buf.has_remaining() {
        let batch = RecordBatchDecoder::decode(&mut buf)
            .unwrap_or_else(|error| panic!("case {case:?}: the batch must decode: {error}"));
        read.extend(batch.records);
    }
    read
}

/// The records of one read a CLIENT surfaces to its caller, filtered the way a client
/// filters them: a control batch is the protocol's own marker and no record of the topic,
/// and the records a producer wrote inside a transaction the fetch reported as ABORTED
/// were thrown away by it.
///
/// Read in ONE PASS over the batches, as a client reads them: an aborted transaction is
/// in force from the first offset it reported, and its producer's own ABORT marker is
/// where it stops — which is what lets one producer run an aborted transaction and a
/// committed one over the same partition.
fn visible(read: &Read) -> Vec<(i64, String)> {
    let mut pending = read.aborted.clone();
    let mut aborting: ::std::collections::BTreeSet<i64> = ::std::collections::BTreeSet::new();
    let mut surfaced = Vec::new();
    for record in &read.records {
        pending.retain(|(producer_id, first_offset)| {
            if *first_offset > record.offset {
                return true;
            }
            aborting.insert(*producer_id);
            false
        });
        if record.control {
            aborting.remove(&record.producer_id);
            continue;
        }
        if aborting.contains(&record.producer_id) {
            continue;
        }
        surfaced.push((
            record.offset,
            String::from_utf8_lossy(record.value.as_deref().unwrap_or_default()).to_string(),
        ));
    }
    surfaced
}

/// `ListOffsets` for the LATEST offset of one partition, at `isolation`.
async fn latest_offset(
    client: &mut Client,
    case: &str,
    topic: &str,
    partition: i32,
    isolation: i8,
) -> i64 {
    let answered: ListOffsetsResponse = client
        .call(
            case,
            ApiKey::ListOffsets,
            5,
            &ListOffsetsRequest::default()
                .with_replica_id(::kafka_protocol::messages::BrokerId(-1))
                .with_isolation_level(isolation)
                .with_topics(vec![ListOffsetsTopic::default()
                    .with_name(TopicName(StrBytes::from_string(topic.to_string())))
                    .with_partitions(vec![ListOffsetsPartition::default()
                        .with_partition_index(partition)
                        .with_timestamp(OFFSET_LATEST)])]),
        )
        .await;
    let reported = answered
        .topics
        .first()
        .and_then(|topic| topic.partitions.first())
        .unwrap_or_else(|| panic!("case {case:?}: ListOffsets reported no partition"))
        .clone();
    assert_eq!(
        reported.error_code, 0,
        "case {case:?}: ListOffsets must be answered, got error {}",
        reported.error_code
    );
    reported.offset
}

/// The offset a consumer group holds for one partition, as its own `OffsetFetch` reports
/// it.
async fn committed_offset(
    client: &mut Client,
    case: &str,
    group: &str,
    topic: &str,
    partition: i32,
) -> i64 {
    let answered: OffsetFetchResponse = client
        .call(
            case,
            ApiKey::OffsetFetch,
            5,
            &OffsetFetchRequest::default()
                .with_group_id(GroupId(StrBytes::from_string(group.to_string())))
                .with_topics(Some(vec![OffsetFetchRequestTopic::default()
                    .with_name(TopicName(StrBytes::from_string(topic.to_string())))
                    .with_partition_indexes(vec![partition])])),
        )
        .await;
    answered
        .topics
        .first()
        .and_then(|topic| topic.partitions.first())
        .map(|partition| partition.committed_offset)
        .unwrap_or_else(|| panic!("case {case:?}: OffsetFetch reported no partition"))
}

/// A cluster whose brokers are reached in the clear, so this suite dials one socket
/// without a credential.
async fn given_cluster(emulator: &Emulator, name: &str) -> String {
    emulator
        .create_ok(&ClusterRequest {
            client_broker: Some("PLAINTEXT"),
            ..ClusterRequest::named(name)
        })
        .await
}

/// The address a cluster publishes for its plaintext path, once it publishes one.
async fn address(emulator: &Emulator, cluster: &str, case: &str) -> String {
    emulator
        .published_bootstrap(cluster, "bootstrapBrokerString", case)
        .await
}

/// A topic of the cluster, created through the control plane. Its records carry
/// timestamps of their own, so the topic keeps every one of them for as long as the suite
/// runs: `retention.ms` -1 is Apache Kafka's "no limit".
async fn given_topic(emulator: &Emulator, cluster: &str, name: &str, partitions: i32) {
    let configs = json!({ "retention.ms": "-1" });
    let body = json!({
        "topicName": name,
        "partitionCount": partitions,
        "replicationFactor": 1,
        "configs": ::aws_smithy_types::base64::encode(configs.to_string()),
    });
    let reply = emulator
        .call(
            "POST",
            &format!("/v1/clusters/{}/topics", crate::kafka_harness::pct(cluster)),
            Some(body),
        )
        .await;
    assert_eq!(
        reply.status, 200,
        "the topic {name} must be created, got {} {}",
        reply.status,
        reply.error_message()
    );
}
