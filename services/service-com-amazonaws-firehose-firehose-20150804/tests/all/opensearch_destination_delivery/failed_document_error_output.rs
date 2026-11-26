// SPDX-License-Identifier: CC0-1.0

//! What becomes of a document an Amazon OpenSearch Service destination did not take, driven
//! end to end over the wire: which documents of ONE delivered batch the destination index's
//! mappings accept, and the vocabulary the failed ones are reported in.
//!
//! Two rules, one per test:
//!
//! 1. A delivered record is indexed only where the destination index's mappings accept it —
//!    a field they do not carry where they are `strict`, and a value a mapped field's type
//!    cannot parse, are refused — and each refused record is written to the destination's
//!    Amazon S3 backup under the failed-document folder instead, carrying the record's own
//!    bytes. So the index holds exactly the conforming records of a mixed batch and the
//!    backup exactly the rest.
//! 2. The error output of a failed document is written in the DESTINATION KIND's own
//!    vocabulary: an `errorCode` in that kind's error namespace naming Firehose's own
//!    exception class for the failure — never the engine's error type — beside the index and
//!    document id the delivery used, named as that kind names them.
//!
//! A child of [`super`] rather than a suite beside it, so the domain, index, stream and
//! backup harness that suite already has is the one driven here too.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};
use service_com_amazonaws_opensearch_amazonopensearchservice::state as search_state;
use service_com_amazonaws_opensearch_amazonopensearchservice::state::ingest;

use super::{
    create_domain, create_index, destination, indexed, indexed_documents, parked_documents,
    strict_schema, FAILED_FOLDER,
};
use crate::delivery_stream_lifecycle::REGION;
use crate::destination_delivery::{base64, Emulator};

/// The engine error types a write of one document is refused with
/// (`::…::state::ingest`), read from the module that states them rather than spelled here:
/// none of them may reach an `errorCode`, which is Firehose's own namespace.
const ENGINE_ERROR_TYPES: [&str; 5] = [
    ingest::SECURITY_EXCEPTION,
    ingest::MAPPER_PARSING_EXCEPTION,
    ingest::STRICT_DYNAMIC_MAPPING_EXCEPTION,
    ingest::INDEX_NOT_FOUND_EXCEPTION,
    ingest::ILLEGAL_ARGUMENT_EXCEPTION,
];

/// A principal that is NOT the delivery role, so a domain policy naming only it leaves the
/// delivery unauthorized.
const ANOTHER_ROLE: &str = "arn:aws:iam::000000000000:role/someone-else";

/// A strict mapping over one field of each class the mappings can judge a value by.
///
/// The field types are the engine's own documented catalog (OpenSearch "Supported field
/// types", the catalog
/// `service_com_amazonaws_opensearch_amazonopensearchservice::state::index_schema::MAPPING_FIELD_TYPES`
/// is taken from): a string field, a whole-number field with coercion turned off (the
/// `coerce` mapping parameter: "coerce: try to convert strings to numbers"), a floating-point
/// field left coercing, an `ip` field, and an object field whose own mappings are strict in
/// turn.
fn catalog_schema() -> Value {
    json!({
        "mappings": {
            "dynamic": "strict",
            "properties": {
                "label": { "type": "keyword" },
                "qty": { "type": "integer", "coerce": false },
                "score": { "type": "double" },
                "tags": { "type": "keyword" },
                "host": { "type": "ip" },
                "meta": {
                    "dynamic": "strict",
                    "properties": { "region": { "type": "keyword" } },
                },
            },
        }
    })
}

/// The `errorCode` an Amazon OpenSearch Service destination reports a mapping conflict
/// under.
const OS_PARSE_EXCEPTION: &str = "OS.AmazonOpenSearchServiceParseException";

/// One record of the mixed batch, and whether the mappings above accept it.
struct Row {
    case: &'static str,
    /// The record's bytes, as a producer puts them.
    record: &'static str,
    /// Whether the index is expected to hold it.
    accepted: bool,
}

/// The record's bytes as the document they are.
fn document_of(case: &str, record: &str) -> Value {
    serde_json::from_str(record)
        .unwrap_or_else(|error| panic!("case {case:?}: the row's record is a document: {error}"))
}

/// RULE: a delivered record is indexed only where the destination index's mappings accept it
/// — a field they do not carry where they are `strict`, and a value a mapped field's type
/// cannot parse — and each one they refuse is written to the destination's Amazon S3 backup
/// under the failed-document folder instead, so one mixed batch leaves the index holding
/// exactly its conforming records.
///
/// The rows span what a value can be against a mapping: a value of the mapped type, a scalar
/// a string field converts, an array of mapped values, a null, a numeric string a COERCING
/// field reads, a nested object the mappings carry — and, on the refusing side, an object for
/// a string field, a string and a fraction and a boolean for a whole-number field with
/// coercion off, a string no IP address can be read from, a field the strict mappings do not
/// carry and a sub-field the strict nested mappings do not carry.
#[tokio::test]
async fn the_index_holds_exactly_the_records_its_mappings_accept() {
    let rows: &[Row] = &[
        Row {
            case: "a value of each mapped type",
            record: r#"{"label":"a","qty":1,"score":1.5,"host":"10.0.0.1"}"#,
            accepted: true,
        },
        Row {
            case: "a number for a string field, which takes any scalar",
            record: r#"{"label":7,"qty":2}"#,
            accepted: true,
        },
        Row {
            case: "an array of values of the mapped type",
            record: r#"{"label":"c","tags":["x","y"],"qty":3}"#,
            accepted: true,
        },
        Row {
            case: "a null, which every type takes",
            record: r#"{"label":"d","qty":null}"#,
            accepted: true,
        },
        Row {
            case: "a numeric string for a field that still coerces",
            record: r#"{"label":"e","score":"2.5","qty":5}"#,
            accepted: true,
        },
        Row {
            case: "a nested object whose own fields the mappings carry",
            record: r#"{"label":"f","qty":6,"meta":{"region":"us-west-2"}}"#,
            accepted: true,
        },
        Row {
            case: "an object for a string field",
            record: r#"{"label":{"not":"a-keyword"},"qty":7}"#,
            accepted: false,
        },
        Row {
            case: "a string for a whole-number field with coercion off",
            record: r#"{"label":"h","qty":"12"}"#,
            accepted: false,
        },
        Row {
            case: "a fraction for a whole-number field with coercion off",
            record: r#"{"label":"i","qty":1.5}"#,
            accepted: false,
        },
        Row {
            case: "a boolean for a numeric field",
            record: r#"{"label":"j","qty":true}"#,
            accepted: false,
        },
        Row {
            case: "a string no IP address can be read from",
            record: r#"{"label":"k","qty":11,"host":"not-an-ip"}"#,
            accepted: false,
        },
        Row {
            case: "a field the strict mappings do not carry",
            record: r#"{"label":"l","qty":12,"surprise":"x"}"#,
            accepted: false,
        },
        Row {
            case: "a sub-field the strict nested mappings do not carry",
            record: r#"{"label":"m","qty":13,"meta":{"zone":"a"}}"#,
            accepted: false,
        },
    ];

    let domain = "search-catalog";
    let index = "catalog";
    let prefix = "catalog/";
    let emulator = Emulator::new();
    emulator.create_bucket(super::BACKUP_BUCKET).await;
    let domain_arn = create_domain(&emulator, domain, None).await;
    create_index(&emulator, domain, index, catalog_schema()).await;
    emulator
        .create_active_stream(
            "catalog-stream",
            "AmazonopensearchserviceDestinationConfiguration",
            destination(&domain_arn, index, prefix),
        )
        .await;

    let batch: Vec<Vec<u8>> = rows
        .iter()
        .map(|row| row.record.as_bytes().to_vec())
        .collect();
    let (status, answered) = emulator.put_batch("catalog-stream", &batch).await;
    assert_eq!(
        status, 200,
        "the mixed batch must be accepted, got {answered:?}"
    );

    let refused: Vec<&Row> = rows.iter().filter(|row| !row.accepted).collect();
    let accepted: Vec<&Row> = rows.iter().filter(|row| row.accepted).collect();

    // The backup object of a flush is written once every record of it has been offered to
    // the index, so by the time the refused documents are readable the indexing is over and
    // what the index holds can be asserted exactly.
    let parked = parked_documents(
        &emulator,
        "the mixed batch",
        &format!("{prefix}{FAILED_FOLDER}"),
        refused.len(),
    )
    .await;
    let parked_by_record: BTreeMap<String, Value> = parked
        .iter()
        .map(|document| {
            (
                document["rawData"].as_str().unwrap_or_default().to_string(),
                document.clone(),
            )
        })
        .collect();
    assert_eq!(
        parked_by_record.len(),
        refused.len(),
        "expected exactly the refused records parked, got {parked:?}"
    );
    for row in &refused {
        let document = parked_by_record
            .get(&base64(row.record.as_bytes()))
            .unwrap_or_else(|| {
                panic!(
                    "case {:?}: expected the record's own bytes parked, got {parked_by_record:?}",
                    row.case
                )
            });
        assert_eq!(
            document["errorCode"].as_str(),
            Some(OS_PARSE_EXCEPTION),
            "case {:?}: expected Firehose's own code for a mapping conflict, got {document:?}",
            row.case
        );
        assert_eq!(
            document["osIndexName"].as_str(),
            Some(index),
            "case {:?}: expected the index the delivery used, got {document:?}",
            row.case
        );
        assert!(
            document["errorMessage"]
                .as_str()
                .is_some_and(|message| !message.is_empty()),
            "case {:?}: expected the engine's reason beside the code, got {document:?}",
            row.case
        );
    }

    let documents = indexed_documents(
        &emulator,
        "the mixed batch",
        domain,
        index,
        accepted.len(),
    )
    .await;
    let mut held: Vec<String> = documents.values().map(Value::to_string).collect();
    held.sort();
    let mut expected: Vec<String> = accepted
        .iter()
        .map(|row| document_of(row.case, row.record).to_string())
        .collect();
    expected.sort();
    assert_eq!(
        held, expected,
        "expected the index to hold exactly the records its mappings accept"
    );
}

/// One Amazon OpenSearch Service destination KIND, and the vocabulary its error output is
/// written in.
struct Kind {
    case: &'static str,
    /// The `CreateDeliveryStream` member the destination is configured under.
    member: &'static str,
    /// A short, index-name-safe spelling of this kind, for the resources a case creates.
    slug: &'static str,
    /// What every `errorCode` of this kind begins with.
    namespace: &'static str,
    /// The member naming the index the delivery used.
    index_member: &'static str,
    /// The member naming the document id the delivery used.
    document_id_member: &'static str,
    /// The exception name a mapping conflict is reported under.
    parse_exception: &'static str,
    /// The members the OTHER kind writes, which this one must not.
    foreign_members: [&'static str; 2],
}

/// The two destination kinds that index into a domain of Amazon OpenSearch Service: the
/// current offering and the same offering under its legacy Elasticsearch name
/// (`smithy_query(query="shape", shape="ElasticsearchDestinationConfiguration")` carries the
/// same members as `AmazonopensearchserviceDestinationConfiguration`, so one destination
/// configuration serves both).
const KINDS: [Kind; 2] = [
    Kind {
        case: "an Amazon OpenSearch Service destination",
        member: "AmazonopensearchserviceDestinationConfiguration",
        slug: "os",
        namespace: "OS.",
        index_member: "osIndexName",
        document_id_member: "osDocumentId",
        parse_exception: OS_PARSE_EXCEPTION,
        foreign_members: ["esIndexName", "esDocumentId"],
    },
    Kind {
        case: "an Elasticsearch destination",
        member: "ElasticsearchDestinationConfiguration",
        slug: "es",
        namespace: "ES.",
        index_member: "esIndexName",
        document_id_member: "esDocumentId",
        parse_exception: "ES.AmazonElasticsearchParseException",
        foreign_members: ["osIndexName", "osDocumentId"],
    },
];

/// Assert the members `kind` writes on one error-output document, and the members it does
/// not.
fn assert_vocabulary(kind: &Kind, case: &str, document: &Value, index: &str, code: &str) {
    assert_eq!(
        document["errorCode"].as_str(),
        Some(code),
        "case {case:?}: expected {code}, got {document:?}"
    );
    for engine_type in ENGINE_ERROR_TYPES {
        assert!(
            !document["errorCode"]
                .as_str()
                .unwrap_or_default()
                .contains(engine_type),
            "case {case:?}: expected the engine's own error type kept out of the code, got \
             {document:?}"
        );
    }
    assert!(
        document["errorCode"]
            .as_str()
            .is_some_and(|code| code.starts_with(kind.namespace)),
        "case {case:?}: expected a code in the {:?} namespace, got {document:?}",
        kind.namespace
    );
    assert_eq!(
        document[kind.index_member].as_str(),
        Some(index),
        "case {case:?}: expected {} to name the index the delivery used, got {document:?}",
        kind.index_member
    );
    for foreign in kind.foreign_members {
        assert!(
            document.get(foreign).is_none(),
            "case {case:?}: expected no {foreign} on this destination kind's output, got \
             {document:?}"
        );
    }
    assert!(
        document["errorMessage"]
            .as_str()
            .is_some_and(|message| !message.is_empty()),
        "case {case:?}: expected a reason beside the code, got {document:?}"
    );
}

/// Turn implicit index creation OFF on `domain`'s cluster, through the domain's own REST data
/// plane.
///
/// The setting is the one that decides whether a write to an index the cluster does not hold
/// creates it (`search_state::AUTO_CREATE_INDEX_SETTING`, written in the scope
/// `search_state::SETTINGS_PERSISTENT` names), so a delivery into an index nobody created is
/// a refusal of the cluster's rather than a create.
async fn given_no_implicit_index_creation(emulator: &Emulator, domain: &str) {
    let mut scope = ::serde_json::Map::new();
    scope.insert(
        search_state::AUTO_CREATE_INDEX_SETTING.to_string(),
        json!("false"),
    );
    let mut body = ::serde_json::Map::new();
    body.insert(
        search_state::SETTINGS_PERSISTENT.to_string(),
        Value::Object(scope),
    );
    let answer = emulator
        .signed(
            "es",
            "PUT",
            &format!("/_aws/es/{domain}/_cluster/settings"),
            Value::Object(body).to_string().as_bytes(),
        )
        .await;
    assert_eq!(
        answer.status,
        200,
        "the cluster setting must be written on {domain}, got {} with {}",
        answer.status,
        String::from_utf8_lossy(&answer.body)
    );
}

/// The two records a row whose delivery fails for a reason that is not about the DOCUMENT
/// delivers: documents a strict mapping would accept, so what parks them is the class under
/// test and nothing of their own.
fn conforming_records() -> [Vec<u8>; 2] {
    [
        br#"{"level":"info","count":1}"#.to_vec(),
        br#"{"level":"warn","count":2}"#.to_vec(),
    ]
}

/// What makes ONE row's delivery fail, and so the class its error output reports.
#[derive(Debug, Clone, Copy)]
enum Class {
    /// The destination index's mappings refuse the documents: a strict index, delivered a
    /// record carrying a field it does not map and one carrying a value its mapped type
    /// cannot parse — two engine error types of one Firehose class.
    MappingConflict,
    /// The cluster holds no such index and does not let the write create one, so the write
    /// met an index that is not there: a refusal of no class of its own.
    NoSuchIndex,
    /// No domain of this emulator carries the destination's address, so nothing judged the
    /// documents at all — parked once the destination's retry window is spent.
    NoDestination,
}

impl Class {
    /// How this class is named in a row's failure message.
    fn case(self) -> &'static str {
        match self {
            Self::MappingConflict => "a batch the index's mappings refuse",
            Self::NoSuchIndex => "an index the cluster does not hold",
            Self::NoDestination => "a destination nothing answers",
        }
    }

    /// A short, name-safe spelling of this class, for the resources a row creates.
    fn slug(self) -> &'static str {
        match self {
            Self::MappingConflict => "mapping",
            Self::NoSuchIndex => "no-index",
            Self::NoDestination => "absent",
        }
    }

    /// The `errorCode` `kind` reports this class under: that kind's own error namespace,
    /// followed by the Firehose exception name for the class.
    fn code(self, kind: &Kind) -> String {
        match self {
            Self::MappingConflict => kind.parse_exception.to_string(),
            Self::NoSuchIndex => format!("{}ServiceException", kind.namespace),
            Self::NoDestination => format!("{}ServiceUnavailable", kind.namespace),
        }
    }
}

/// RULE: every record Firehose parks under the failed-document folder carries an `errorCode`
/// in the destination kind's own error namespace naming the Firehose exception class of the
/// failure — never the engine's own error type, whose wording stays in `errorMessage` — so
/// one batch that failed for one reason carries exactly ONE distinct code, whatever the
/// engine called each refusal.
///
/// The rows are the two destination kinds crossed with the classes a delivery of theirs can
/// fail in: documents the index's mappings refuse (delivered as two DIFFERENT engine error
/// types, which one Firehose class covers), an index the cluster does not hold and will not
/// create, and a destination no domain answers whose retry window is spent.
#[tokio::test]
async fn every_parked_record_carries_one_namespaced_code_for_its_failure_class() {
    let emulator = Emulator::new();
    emulator.create_bucket(super::BACKUP_BUCKET).await;
    let index = "events";

    for kind in &KINDS {
        for class in [
            Class::MappingConflict,
            Class::NoSuchIndex,
            Class::NoDestination,
        ] {
            let case = format!("{} / {}", kind.case, class.case());
            let slug = format!("{}-{}", kind.slug, class.slug());
            let domain = format!("search-class-{slug}");
            let stream = format!("class-{slug}-stream");
            let prefix = format!("class-{slug}/");
            // Each record of the row, beside what the engine's own reason for not taking it
            // must state — the variable part of that reason, taken from the request and the
            // emulator's state rather than spelled out here.
            let mut rows: Vec<(Vec<u8>, String)> = Vec::new();
            let config = match class {
                Class::MappingConflict => {
                    let domain_arn = create_domain(&emulator, &domain, None).await;
                    create_index(&emulator, &domain, index, strict_schema()).await;
                    rows.push((
                        br#"{"level":"info","surprise":"x"}"#.to_vec(),
                        "surprise".to_string(),
                    ));
                    rows.push((
                        br#"{"level":"info","count":"many"}"#.to_vec(),
                        "count".to_string(),
                    ));
                    destination(&domain_arn, index, &prefix)
                }
                Class::NoSuchIndex => {
                    let domain_arn = create_domain(&emulator, &domain, None).await;
                    given_no_implicit_index_creation(&emulator, &domain).await;
                    rows.extend(
                        conforming_records()
                            .into_iter()
                            .map(|record| (record, index.to_string())),
                    );
                    destination(&domain_arn, index, &prefix)
                }
                Class::NoDestination => {
                    // An address of the documented form that no domain of this emulator
                    // carries, so the delivery reaches no cluster at all.
                    let absent = format!("arn:aws:es:{REGION}:000000000000:domain/{domain}");
                    rows.extend(
                        conforming_records()
                            .into_iter()
                            .map(|record| (record, absent.clone())),
                    );
                    let mut config = destination(&absent, index, &prefix);
                    // "a value of 0 (zero) results in no retries"
                    // (`ElasticsearchRetryOptions$DurationInSeconds`, whose range admits it),
                    // so the one attempt the buffer gets spends the window and the records
                    // are parked instead of being owed again.
                    config["RetryOptions"] = json!({ "DurationInSeconds": 0 });
                    config
                }
            };
            emulator
                .create_active_stream(&stream, kind.member, config)
                .await;

            let records: Vec<Vec<u8>> = rows.iter().map(|(record, _)| record.clone()).collect();
            let (status, answered) = emulator.put_batch(&stream, &records).await;
            assert_eq!(
                status, 200,
                "case {case:?}: the put must be accepted, got {answered:?}"
            );

            let parked =
                parked_documents(&emulator, &case, &format!("{prefix}{FAILED_FOLDER}"), records.len())
                    .await;
            assert_eq!(
                parked.len(),
                records.len(),
                "case {case:?}: expected every record of the batch parked, got {parked:?}"
            );
            let code = class.code(kind);
            let codes: BTreeSet<&str> = parked
                .iter()
                .filter_map(|document| document["errorCode"].as_str())
                .collect();
            assert_eq!(
                codes,
                BTreeSet::from([code.as_str()]),
                "case {case:?}: expected one distinct code for the batch, got {codes:?}"
            );
            let parked_by_record: BTreeMap<String, Value> = parked
                .iter()
                .map(|document| {
                    (
                        document["rawData"].as_str().unwrap_or_default().to_string(),
                        document.clone(),
                    )
                })
                .collect();
            for (record, mentioned) in &rows {
                let document = parked_by_record.get(&base64(record)).unwrap_or_else(|| {
                    panic!(
                        "case {case:?}: expected the record's own bytes parked, got \
                         {parked_by_record:?}"
                    )
                });
                assert_vocabulary(kind, &case, document, index, &code);
                let message = document["errorMessage"].as_str().unwrap_or_default();
                assert!(
                    message.contains(mentioned.as_str()),
                    "case {case:?}: expected the reason to state {mentioned:?}, got {document:?}"
                );
            }
            if !matches!(class, Class::NoDestination) {
                assert!(
                    indexed(&emulator, &domain, index).is_empty(),
                    "case {case:?}: expected nothing indexed of a batch the cluster refused"
                );
            }
        }
    }
}

/// RULE: the error output of a failed document is written in the DESTINATION KIND's own
/// vocabulary — an `errorCode` in that kind's error namespace naming Firehose's own exception
/// class for the failure, never the engine's error type, beside the index and document id the
/// delivery used under the names that kind gives them.
///
/// The rows are the two kinds that deliver into a domain of this service, and each is driven
/// through two failure classes: a document the index's mappings refuse, and a delivery the
/// domain does not authorize at all.
#[tokio::test]
async fn the_error_output_speaks_the_destination_kinds_own_vocabulary() {
    let emulator = Emulator::new();
    emulator.create_bucket(super::BACKUP_BUCKET).await;

    for kind in &KINDS {
        // The mapping-conflict class: a domain that authorizes the delivery role, holding a
        // strict index, delivered one conforming and one conflicting record.
        let domain = format!("search-vocab-{}", kind.slug);
        let stream = format!("vocab-{}-stream", kind.slug);
        let prefix = format!("vocab-{}/", kind.slug);
        let index = "events";
        let domain_arn = create_domain(&emulator, &domain, None).await;
        create_index(&emulator, &domain, index, strict_schema()).await;
        emulator
            .create_active_stream(&stream, kind.member, destination(&domain_arn, index, &prefix))
            .await;
        let conforming = br#"{"level":"info","count":1}"#.to_vec();
        let conflicting = br#"{"level":"info","count":"many"}"#.to_vec();
        let (status, answered) = emulator
            .put_batch(&stream, &[conforming.clone(), conflicting.clone()])
            .await;
        assert_eq!(
            status, 200,
            "case {:?}: the put must be accepted, got {answered:?}",
            kind.case
        );
        let conflicting_id = answered["RequestResponses"][1]["RecordId"]
            .as_str()
            .unwrap_or_else(|| {
                panic!(
                    "case {:?}: the put must answer a RecordId per record, got {answered:?}",
                    kind.case
                )
            })
            .to_string();

        let parked = parked_documents(
            &emulator,
            kind.case,
            &format!("{prefix}{FAILED_FOLDER}"),
            1,
        )
        .await;
        assert_eq!(
            parked.len(),
            1,
            "case {:?}: expected only the conflicting record parked, got {parked:?}",
            kind.case
        );
        let document = &parked[0];
        assert_vocabulary(kind, kind.case, document, index, kind.parse_exception);
        assert_eq!(
            document[kind.document_id_member].as_str(),
            Some(conflicting_id.as_str()),
            "case {:?}: expected {} to name the document id the delivery used, got {document:?}",
            kind.case,
            kind.document_id_member
        );
        assert_eq!(
            document["rawData"].as_str(),
            Some(base64(&conflicting).as_str()),
            "case {:?}: expected the conflicting record's own bytes, got {document:?}",
            kind.case
        );
        let held = indexed_documents(&emulator, kind.case, &domain, index, 1).await;
        assert_eq!(
            held.len(),
            1,
            "case {:?}: expected only the conforming record indexed, got {held:?}",
            kind.case
        );

        // The authorization class: a domain whose access policy names only another
        // principal, so no record of the delivery is written at all.
        let denied_case = format!("{} the domain does not authorize", kind.case);
        let denied_domain = format!("search-denied-{}", kind.slug);
        let denied_stream = format!("denied-{}-stream", kind.slug);
        let denied_prefix = format!("denied-{}/", kind.slug);
        let denied_arn = create_domain(
            &emulator,
            &denied_domain,
            Some(json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Allow",
                    "Principal": { "AWS": ANOTHER_ROLE },
                    "Action": "es:*",
                    "Resource": "*",
                }],
            })),
        )
        .await;
        emulator
            .create_active_stream(
                &denied_stream,
                kind.member,
                destination(&denied_arn, index, &denied_prefix),
            )
            .await;
        let (status, answered) = emulator
            .put_batch(&denied_stream, &[conforming.clone(), conflicting.clone()])
            .await;
        assert_eq!(
            status, 200,
            "case {denied_case:?}: the put must be accepted, got {answered:?}"
        );
        let denied = parked_documents(
            &emulator,
            &denied_case,
            &format!("{denied_prefix}{FAILED_FOLDER}"),
            2,
        )
        .await;
        assert_eq!(
            denied.len(),
            2,
            "case {denied_case:?}: expected every record parked, got {denied:?}"
        );
        let denied_code = format!("{}BulkWritePermissionDenied", kind.namespace);
        for document in &denied {
            assert_vocabulary(kind, &denied_case, document, index, &denied_code);
        }
        assert!(
            indexed(&emulator, &denied_domain, index).is_empty(),
            "case {denied_case:?}: expected nothing indexed on a domain that refused the role"
        );
    }
}
