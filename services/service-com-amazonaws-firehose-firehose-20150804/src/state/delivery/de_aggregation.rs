// SPDX-License-Identifier: CC0-1.0

//! The `RecordDeAggregation` processor a destination's data processing can declare: what one
//! accepted record is split into before anything else reads it.
//!
//! A producer is free to pack several events into ONE `PutRecord` / `PutRecordBatch` record,
//! and dynamic partitioning — which "creates smaller data sets from the streaming data by
//! partitioning it based on partition keys" (`DynamicPartitioningConfiguration`) — then has
//! to see those events SEPARATELY, because the partition keys of one of them say nothing
//! about the next. That is what a `RecordDeAggregation` processor is for: it parses each
//! record and separates it into the sub-records its `SubRecordType` parameter names, and the
//! de-aggregation happens BEFORE the partition keys are derived, so each sub-record is
//! partitioned, prefixed and delivered on its own.
//!
//! The two forms of sub-record, which are the two values `SubRecordType` carries:
//!
//! * [`JSON_SUB_RECORD_TYPE`] — the record's bytes are a sequence of JSON values written one
//!   after another, and each value is one sub-record ([`json_sub_records`]).
//! * [`DELIMITED_SUB_RECORD_TYPE`] — the record's bytes are the sub-records separated by the
//!   `Delimiter` parameter, which is carried base64-encoded ([`delimited_sub_records`]).
//!
//! A record this cannot read as the declared form — bytes that are not a sequence of JSON
//! values, a delimited form whose delimiter is not given, a record carrying more sub-records
//! than [`MAX_SUB_RECORDS_PER_RECORD`] — is left WHOLE rather than dropped or split
//! half-way: it then goes wherever the destination's own processing takes it, which for an
//! enabled metadata extraction that cannot read it is the error output prefix. Nothing a put
//! accepted is lost by a de-aggregation that did not happen.

use std::borrow::Cow;

use crate::ops::types::ProcessingConfiguration;
use crate::state::{StoredRecord, RECORD_DE_AGGREGATION_PROCESSOR_TYPE, SUB_RECORD_TYPE_PARAMETER};

/// The `SubRecordType` value whose sub-records are the JSON values the record's bytes carry
/// one after another.
const JSON_SUB_RECORD_TYPE: &str = "JSON";

/// The `SubRecordType` value whose sub-records are the pieces a delimiter separates.
const DELIMITED_SUB_RECORD_TYPE: &str = "DELIMITED";

/// The `ProcessorParameterName` carrying the separator of a [`DELIMITED_SUB_RECORD_TYPE`]
/// record, base64-encoded.
const DELIMITER_PARAMETER: &str = "Delimiter";

/// The most sub-records one record may be split into.
///
/// An EMULATOR ceiling, not an AWS quota: every sub-record is held BESIDE the record it came
/// out of while the objects of the flush are built, and a record is bounded in BYTES (the
/// modelled `Data` length) rather than in how many values those bytes carry — a record of
/// minimal JSON values would otherwise cost one buffered record each. It sits far above what
/// a producer aggregating events into one record sends. A record carrying more is left whole,
/// like any other this cannot read as the declared form.
const MAX_SUB_RECORDS_PER_RECORD: usize = 10_000;

/// The most sub-records the records of ONE flush may together be split into.
///
/// The aggregate half of the bound above, and an EMULATOR ceiling for the same reason: a
/// per-record cap alone leaves the PRODUCT of the record count and the sub-record count
/// unbounded, and a buffer is counted in the bytes the producer put rather than in the values
/// those bytes carry. It is the records one stream may keep buffered
/// ([`crate::state::MAX_BUFFERED_RECORDS_PER_STREAM`]), so de-aggregating a flush cannot cost
/// more records than the buffer it came from may itself hold. Once it is spent the remaining
/// records of the flush are delivered whole.
const MAX_SUB_RECORDS_PER_DELIVERY: usize = crate::state::MAX_BUFFERED_RECORDS_PER_STREAM;

/// The form a destination's `RecordDeAggregation` processor splits each record into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeAggregation {
    /// Each JSON value the record's bytes carry is a sub-record.
    Json,
    /// The record's bytes are its sub-records separated by these bytes.
    Delimited(Vec<u8>),
}

impl DeAggregation {
    /// The de-aggregation `processing` asks for, and `None` where each accepted record is
    /// delivered as the producer sent it.
    ///
    /// A configuration whose `Enabled` is not `true` processes nothing — "Enables or disables
    /// data processing" (`ProcessingConfiguration$Enabled`) — so the processors it lists split
    /// no record. De-aggregation belongs to the dynamic partitioning mechanism that reads the
    /// sub-records separately, so a destination with that mechanism switched off keeps every
    /// record whole, exactly as a destination with no such processor does.
    #[must_use]
    pub fn of(
        dynamic_partitioning_enabled: bool,
        processing: Option<&ProcessingConfiguration>,
    ) -> Option<Self> {
        if !dynamic_partitioning_enabled {
            return None;
        }
        let processing = processing.filter(|processing| processing.enabled == Some(true))?;
        let processor = processing
            .processors
            .iter()
            .flatten()
            .find(|processor| processor.r#type == RECORD_DE_AGGREGATION_PROCESSOR_TYPE)?;
        let parameter = |name: &str| {
            processor
                .parameters
                .iter()
                .flatten()
                .find(|parameter| parameter.parameter_name == name)
                .map(|parameter| parameter.parameter_value.as_str())
        };
        match parameter(SUB_RECORD_TYPE_PARAMETER)? {
            JSON_SUB_RECORD_TYPE => Some(Self::Json),
            // The delimiter is carried base64-encoded, so what separates the sub-records is
            // the bytes it decodes to. A delimited form that names no delimiter, or one whose
            // value is not base64 at all, separates nothing: the records stay whole.
            DELIMITED_SUB_RECORD_TYPE => {
                let encoded = parameter(DELIMITER_PARAMETER)?;
                let delimiter = ::aws_smithy_types::base64::decode(encoded).ok()?;
                (!delimiter.is_empty()).then_some(Self::Delimited(delimiter))
            }
            // A form this emulator cannot split a record into is one it leaves whole. Read at
            // DEBUG: this is re-read on every flush of the stream, so it would otherwise
            // repeat for as long as the destination carries the parameter.
            other => {
                ::tracing::debug!(
                    %other,
                    "the destination asks for a sub-record form this emulator does not split"
                );
                None
            }
        }
    }

    /// The sub-records `data` carries in this form, or `None` when the bytes are not that
    /// form — or carry just one sub-record, which is the record itself.
    ///
    /// `ceiling` is the most sub-records this record may be split into, which is what is left
    /// of the flush's own budget where that is the narrower of the two: a record carrying more
    /// is left whole.
    fn split(&self, data: &[u8], ceiling: usize) -> Option<Vec<Vec<u8>>> {
        match self {
            Self::Json => json_sub_records(data, ceiling),
            Self::Delimited(delimiter) => delimited_sub_records(data, delimiter, ceiling),
        }
    }
}

/// The records a location delivers: the sub-records `de_aggregation` splits each buffered
/// record into, and the buffer itself where the destination declares no de-aggregation.
///
/// Borrowed where the destination declares no de-aggregation at all, which is every location
/// of every destination that lists no such processor: such a delivery copies no record data.
/// A record the declared form cannot be read as — and one that carries a single sub-record,
/// which is already the whole record — is carried through as it stands.
///
/// A sub-record keeps the ingestion instant and the attempt count of the record it came out
/// of — those are facts about what the PRODUCER sent — and carries an identity of its own,
/// the `RecordId` the put answered for that record plus its ordinal within it
/// ([`sub_record_id`]): the delivery tells two records apart by that id (the `recordId` of a
/// data transformation's entry, and the record a reported fault is about), so two sub-records
/// of one put must not share one.
///
/// ASYNC because splitting is CPU-bound work over the whole buffer: the loop hands the
/// runtime back between records, so a timeout, a cancellation or a shutdown can land while a
/// long buffer is being split instead of the whole run holding the worker — the same reason
/// the per-record loops after it ([`super::processed_records`]) are async.
#[must_use]
pub async fn sub_records<'r>(
    records: &'r [StoredRecord],
    de_aggregation: Option<&DeAggregation>,
) -> Cow<'r, [StoredRecord]> {
    let Some(de_aggregation) = de_aggregation else {
        return Cow::Borrowed(records);
    };
    // The sub-records the rest of this flush may still hold, charged down as the records are
    // split, so what one flush expands to is bounded and not only what each record does.
    let mut budget = MAX_SUB_RECORDS_PER_DELIVERY;
    let mut sub_records = Vec::with_capacity(records.len());
    for record in records {
        ::tokio::task::yield_now().await;
        match de_aggregation.split(&record.data, MAX_SUB_RECORDS_PER_RECORD.min(budget)) {
            None => {
                budget = budget.saturating_sub(1);
                sub_records.push(record.clone());
            }
            Some(pieces) => {
                budget = budget.saturating_sub(pieces.len());
                sub_records.extend(pieces.into_iter().enumerate().map(|(ordinal, data)| {
                    StoredRecord {
                        record_id: sub_record_id(&record.record_id, ordinal),
                        data,
                        ..record.clone()
                    }
                }));
            }
        }
    }
    Cow::Owned(sub_records)
}

/// The `RecordId` of the sub-record at `ordinal` of a record the put answered `record_id`
/// for: that id with the ordinal after it, so every sub-record of one flush has an id of its
/// own and the record each came out of is still readable off it.
///
/// A record id is what the delivery identifies ONE record by — the `recordId` a data
/// transformation's entry carries and answers for
/// ([`crate::state::delivery::lambda_processor`]), and the key a fault reported about one
/// record is held under — so sub-records sharing their parent's id would be one record to
/// everything downstream. The ids a put mints carry hexadecimal digits only, so the `-` this
/// adds appears in no minted id and two sub-record ids cannot collide. It reaches no client:
/// the id a put answers is the one for the record the producer sent, and this one names a
/// part of it.
fn sub_record_id(record_id: &str, ordinal: usize) -> String {
    format!("{record_id}-{ordinal}")
}

/// The JSON values `data` carries one after another, each as the bytes it was written as.
///
/// Firehose "parses the records and looks for multiple valid JSON objects within each API
/// call", so what separates two sub-records is where one value ends and the next begins —
/// nothing else has to sit between them. The bytes of each value are taken as the producer
/// wrote them rather than re-serialized, so what is delivered for a sub-record is the
/// producer's own text.
///
/// `None` when the bytes are not wholly a sequence of JSON values — a half-written value, or
/// anything that is not one at all — and when they carry a SINGLE value, which is already the
/// whole record.
fn json_sub_records(data: &[u8], ceiling: usize) -> Option<Vec<Vec<u8>>> {
    let mut values = ::serde_json::Deserializer::from_slice(data).into_iter::<::serde_json::Value>();
    let mut pieces: Vec<Vec<u8>> = Vec::new();
    let mut read = 0usize;
    while let Some(value) = values.next() {
        value.ok()?;
        let ends_at = values.byte_offset();
        // What the reader consumed for this value, less the whitespace that separated it from
        // the previous one: the separator belongs to neither sub-record.
        let piece = data.get(read..ends_at)?.trim_ascii();
        if piece.is_empty() {
            return None;
        }
        pieces.push(piece.to_vec());
        read = ends_at;
        if pieces.len() > ceiling {
            return None;
        }
    }
    // Whatever follows the last value must be whitespace: a record ending in bytes that are
    // not a value is not a sequence of values, however many whole ones precede them.
    if data.get(read..)?.trim_ascii().is_empty() && pieces.len() > 1 {
        return Some(pieces);
    }
    None
}

/// The pieces `delimiter` separates `data` into.
///
/// The search is `memchr::memmem`'s, which is linear in the bytes it scans: the cost of one
/// flush is the record data the producer put and not that times the length of the delimiter a
/// caller configured.
///
/// A piece that is empty — two delimiters in a row, and the one a record ending in the
/// delimiter leaves behind — is no sub-record: the delimiter separates the sub-records rather
/// than carrying one of its own.
///
/// `None` when the bytes hold a single sub-record, which is already the whole record, and when
/// they hold none at all.
fn delimited_sub_records(data: &[u8], delimiter: &[u8], ceiling: usize) -> Option<Vec<Vec<u8>>> {
    let mut pieces: Vec<Vec<u8>> = Vec::new();
    let finder = ::memchr::memmem::Finder::new(delimiter);
    let mut rest = data;
    loop {
        let (piece, next) = match finder.find(rest) {
            Some(at) => (&rest[..at], &rest[at + delimiter.len()..]),
            None => (rest, &rest[rest.len()..]),
        };
        if !piece.is_empty() {
            pieces.push(piece.to_vec());
            if pieces.len() > ceiling {
                return None;
            }
        }
        if next.is_empty() {
            break;
        }
        rest = next;
    }
    (pieces.len() > 1).then_some(pieces)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    use crate::ops::types::{Processor, ProcessorParameter};

    /// One record carrying `data`, as a put left it in the buffer.
    fn record(data: &[u8]) -> StoredRecord {
        StoredRecord {
            record_id: "record-id".to_string(),
            data: data.to_vec(),
            received_at: SystemTime::UNIX_EPOCH,
            delivery_attempts: 0,
        }
    }

    /// The data of each record of `records`, as the text it is.
    fn bodies(records: &[StoredRecord]) -> Vec<String> {
        records
            .iter()
            .map(|record| String::from_utf8_lossy(&record.data).into_owned())
            .collect()
    }

    /// What each form splits a record into, over the shapes a record arrives in.
    ///
    /// The rows differ along what the rule generalises over: how many sub-records the record
    /// carries, which form they are in, what sits between them, and whether the bytes are that
    /// form at all.
    #[test]
    fn each_form_splits_a_record_into_the_sub_records_it_carries() {
        let comma = DeAggregation::Delimited(b",".to_vec());
        let fence = DeAggregation::Delimited(b"||".to_vec());
        for (case, form, data, expected) in [
            (
                "four JSON objects written one after another",
                DeAggregation::Json,
                "{\"tenant\":\"alpha\"}{\"tenant\":\"bravo\"}{\"tenant\":\"charlie\"}\
                 {\"tenant\":\"delta\"}",
                Some(vec![
                    "{\"tenant\":\"alpha\"}",
                    "{\"tenant\":\"bravo\"}",
                    "{\"tenant\":\"charlie\"}",
                    "{\"tenant\":\"delta\"}",
                ]),
            ),
            (
                "JSON objects separated by whitespace and newlines",
                DeAggregation::Json,
                "{\"a\":1}\n  {\"b\":[1,2]}\t{\"c\":{\"d\":\"e\"}}\n",
                Some(vec!["{\"a\":1}", "{\"b\":[1,2]}", "{\"c\":{\"d\":\"e\"}}"]),
            ),
            (
                "two JSON arrays, which are values like any other",
                DeAggregation::Json,
                "[1,2][3]",
                Some(vec!["[1,2]", "[3]"]),
            ),
            (
                "a single JSON object, which is already the whole record",
                DeAggregation::Json,
                "{\"tenant\":\"alpha\"}",
                None,
            ),
            (
                "bytes that are not JSON at all",
                DeAggregation::Json,
                "tenant=alpha",
                None,
            ),
            (
                "whole values followed by a half-written one",
                DeAggregation::Json,
                "{\"a\":1}{\"b\":",
                None,
            ),
            (
                "three pieces a one-byte delimiter separates",
                comma.clone(),
                "alpha,bravo,charlie",
                Some(vec!["alpha", "bravo", "charlie"]),
            ),
            (
                "a record that also ends in the delimiter",
                comma.clone(),
                "alpha,bravo,",
                Some(vec!["alpha", "bravo"]),
            ),
            (
                "a multi-byte delimiter, around JSON of its own",
                fence,
                "{\"a\":1}||{\"b\":2}",
                Some(vec!["{\"a\":1}", "{\"b\":2}"]),
            ),
            (
                "a record carrying the delimiter nowhere",
                comma,
                "alpha",
                None,
            ),
        ] {
            let split = form.split(data.as_bytes(), MAX_SUB_RECORDS_PER_RECORD);
            let expected = expected.map(|pieces| {
                pieces
                    .into_iter()
                    .map(|piece| piece.as_bytes().to_vec())
                    .collect::<Vec<Vec<u8>>>()
            });
            assert!(
                split == expected,
                "case {case:?}: expected {:?}, got {:?}",
                expected.as_ref().map(|pieces| pieces.len()),
                split.as_ref().map(|pieces| bodies(
                    &pieces.iter().map(|piece| record(piece)).collect::<Vec<StoredRecord>>()
                )),
            );
        }
    }

    /// A record carrying more sub-records than the ceiling admits is left whole, and one
    /// carrying exactly the ceiling is split.
    #[test]
    fn a_record_past_the_sub_record_ceiling_is_left_whole() {
        for (case, ceiling, count, splits) in [
            ("one short of the per-record ceiling", MAX_SUB_RECORDS_PER_RECORD, MAX_SUB_RECORDS_PER_RECORD - 1, true),
            ("the per-record ceiling itself", MAX_SUB_RECORDS_PER_RECORD, MAX_SUB_RECORDS_PER_RECORD, true),
            ("one past the per-record ceiling", MAX_SUB_RECORDS_PER_RECORD, MAX_SUB_RECORDS_PER_RECORD + 1, false),
            ("what is left of the flush's own budget", 3, 3, true),
            ("one past what is left of it", 3, 4, false),
            ("a flush whose budget is spent", 0, 2, false),
        ] {
            let data = "{}".repeat(count);
            let split = DeAggregation::Json.split(data.as_bytes(), ceiling);
            assert_eq!(
                split.is_some(),
                splits,
                "case {case:?}: {count} sub-records under a ceiling of {ceiling} must {} be \
                 split, got {:?} pieces",
                if splits { "" } else { "not" },
                split.map(|pieces| pieces.len())
            );
        }
    }

    /// A delimiter far longer than the pieces it separates is scanned over a record once: the
    /// search is linear in the record's own bytes, so a record whose bytes repeatedly match
    /// the delimiter's first byte without ever matching the whole of it costs that record's
    /// length rather than its length times the delimiter's.
    #[test]
    fn a_long_delimiter_is_scanned_over_the_record_once() {
        // The longest delimiter a `Delimiter` parameter can carry: the modelled
        // `ProcessorParameterValue` holds 5,120 characters, which is 3,840 base64-decoded
        // bytes. Its last byte differs from the rest, so a record of the first byte alone
        // matches its prefix at every offset and the whole of it nowhere.
        let delimiter: Vec<u8> = ::std::iter::repeat_n(b'~', 3_839).chain(*b"|").collect();
        let form = DeAggregation::Delimited(delimiter.clone());
        let repeated_prefix = vec![b'~'; 128_000];
        let mut separated = b"alpha".to_vec();
        separated.extend_from_slice(&delimiter);
        separated.extend_from_slice(b"bravo");
        for (case, data, expected) in [
            (
                "a record matching the delimiter's prefix everywhere and the whole of it \
                 nowhere",
                repeated_prefix,
                None,
            ),
            (
                "the same delimiter where it really separates two sub-records",
                separated,
                Some(vec![b"alpha".to_vec(), b"bravo".to_vec()]),
            ),
        ] {
            let split = form.split(&data, MAX_SUB_RECORDS_PER_RECORD);
            assert!(
                split == expected,
                "case {case:?}: expected {:?} piece(s), got {:?}",
                expected.as_ref().map(Vec::len),
                split.as_ref().map(Vec::len)
            );
        }
    }

    /// Every sub-record carries the ingestion facts of the record it came out of and an
    /// identity of its own, and a destination that de-aggregates nothing hands the buffer
    /// through untouched.
    #[tokio::test]
    async fn the_buffer_is_handed_through_as_the_sub_records_it_holds() {
        let records = vec![
            record(b"{\"tenant\":\"alpha\"}{\"tenant\":\"bravo\"}"),
            record(b"{\"tenant\":\"charlie\"}"),
        ];
        let whole = sub_records(&records, None).await;
        assert_eq!(
            bodies(&whole),
            bodies(&records),
            "case \"no de-aggregation\": the buffer must be handed through as it stands"
        );
        assert!(
            matches!(whole, Cow::Borrowed(_)),
            "case \"no de-aggregation\": nothing is copied when nothing is split"
        );
        let split = sub_records(&records, Some(&DeAggregation::Json)).await;
        assert_eq!(
            bodies(&split),
            vec![
                "{\"tenant\":\"alpha\"}".to_string(),
                "{\"tenant\":\"bravo\"}".to_string(),
                "{\"tenant\":\"charlie\"}".to_string(),
            ],
            "case \"one aggregated record beside a single one\": every sub-record must be \
             delivered on its own"
        );
        for sub_record in split.iter() {
            assert_eq!(
                sub_record.received_at,
                SystemTime::UNIX_EPOCH,
                "case \"ingestion facts\": a sub-record keeps the instant its record arrived, \
                 got {:?}",
                sub_record.received_at
            );
        }
        let ids: Vec<&str> = split
            .iter()
            .map(|sub_record| sub_record.record_id.as_str())
            .collect();
        // The record that was split names its own id with each sub-record's ordinal after it;
        // the one that was not keeps the id the put answered for it.
        assert_eq!(
            ids,
            vec!["record-id-0", "record-id-1", "record-id"],
            "case \"record identity\": two sub-records of one put must not share an id, got \
             {ids:?}"
        );
    }

    /// What a `ProcessingConfiguration` asks for, over the forms one is written in.
    #[test]
    fn the_configuration_decides_which_form_is_split() {
        let processor = |kind: &str, parameters: &[(&str, &str)]| Processor {
            r#type: kind.to_string(),
            parameters: Some(
                parameters
                    .iter()
                    .map(|(name, value)| ProcessorParameter {
                        parameter_name: (*name).to_string(),
                        parameter_value: (*value).to_string(),
                    })
                    .collect(),
            ),
        };
        let configuration = |enabled: Option<bool>, processors: Vec<Processor>| {
            ProcessingConfiguration {
                enabled,
                processors: Some(processors),
            }
        };
        let json = processor(
            RECORD_DE_AGGREGATION_PROCESSOR_TYPE,
            &[(SUB_RECORD_TYPE_PARAMETER, JSON_SUB_RECORD_TYPE)],
        );
        // The delimiter is base64, so `LA==` is the one comma this separates on.
        let delimited = processor(
            RECORD_DE_AGGREGATION_PROCESSOR_TYPE,
            &[
                (SUB_RECORD_TYPE_PARAMETER, DELIMITED_SUB_RECORD_TYPE),
                (DELIMITER_PARAMETER, "LA=="),
            ],
        );
        for (case, partitioning, processing, expected) in [
            ("no configuration at all", true, None, None),
            (
                "a de-aggregation whose data processing is switched off",
                true,
                Some(configuration(Some(false), vec![json.clone()])),
                None,
            ),
            (
                "a de-aggregation with dynamic partitioning switched off",
                false,
                Some(configuration(Some(true), vec![json.clone()])),
                None,
            ),
            (
                "only a processor of another kind",
                true,
                Some(configuration(
                    Some(true),
                    vec![processor("AppendDelimiterToRecord", &[])],
                )),
                None,
            ),
            (
                "the JSON form",
                true,
                Some(configuration(Some(true), vec![json.clone()])),
                Some(DeAggregation::Json),
            ),
            (
                "the JSON form beside the metadata extraction that reads its sub-records",
                true,
                Some(configuration(
                    Some(true),
                    vec![
                        json,
                        processor(
                            "MetadataExtraction",
                            &[
                                ("MetadataExtractionQuery", "{tenant:.tenant}"),
                                ("JsonParsingEngine", "JQ-1.6"),
                            ],
                        ),
                    ],
                )),
                Some(DeAggregation::Json),
            ),
            (
                "the delimited form, whose delimiter is base64",
                true,
                Some(configuration(Some(true), vec![delimited])),
                Some(DeAggregation::Delimited(b",".to_vec())),
            ),
            (
                "the delimited form naming no delimiter",
                true,
                Some(configuration(
                    Some(true),
                    vec![processor(
                        RECORD_DE_AGGREGATION_PROCESSOR_TYPE,
                        &[(SUB_RECORD_TYPE_PARAMETER, DELIMITED_SUB_RECORD_TYPE)],
                    )],
                )),
                None,
            ),
            (
                "a sub-record form this emulator does not split",
                true,
                Some(configuration(
                    Some(true),
                    vec![processor(
                        RECORD_DE_AGGREGATION_PROCESSOR_TYPE,
                        &[(SUB_RECORD_TYPE_PARAMETER, "PROTOBUF")],
                    )],
                )),
                None,
            ),
        ] {
            let read = DeAggregation::of(partitioning, processing.as_ref());
            assert_eq!(
                read, expected,
                "case {case:?}: expected {expected:?}, got {read:?}"
            );
        }
    }
}
