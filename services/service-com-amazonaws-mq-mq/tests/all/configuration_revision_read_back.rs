// SPDX-License-Identifier: CC0-1.0

//! What `DescribeConfigurationRevision` reports for the revision its path names.
//!
//! RULE: the read answers the document and the description stored ON THE REVISION the
//! `ConfigurationRevision` label names — for the revision a create makes, the default
//! document of the configuration's OWN engine, since no member of `CreateConfiguration`
//! carries one ("Amazon MQ uses the default configuration (the engine type and
//! version)"); for a revision an accepted `UpdateConfiguration` made, exactly the `Data`
//! and `Description` that write supplied — so no read ever answers another revision's
//! document, another engine's document, or no document at all.
//! (`smithy_query(query="operation", operation="DescribeConfigurationRevision")` models
//! GET /v1/configurations/{ConfigurationId}/revisions/{ConfigurationRevision} -> 200 with
//! `Data` ("Amazon MQ for ActiveMQ: the base64-encoded XML configuration. Amazon MQ for
//! RabbitMQ: base64-encoded Cuttlefish.") and `Description`.)
//!
//! The table spans what the rule generalises over: both engines the model enumerates
//! (`ENGINE_TYPE_VALUES`), the kind of document each engine takes (one statement, several
//! statements with comments, a document repeated under another label, an element with
//! text, a prolog with nesting), the forms a `Description` is sent in (a sentence, the
//! member absent, the empty string), and the order the chain is read back in (oldest
//! first and newest first, each pass twice), with a write the engine refuses between
//! every pair of accepted ones so the numbers a read resolves count the writes the
//! service ACCEPTED.
//!
//! Which document belongs to which engine is not asserted from a constant this file
//! spells: each engine's default is handed to BOTH engines' parsers through
//! `UpdateConfiguration`, so a default served for the wrong engine shows up as the
//! parser's own refusal rather than as a literal this test and the service share.

use serde_json::Value;

use service_com_amazonaws_mq_mq::ops::types::ENGINE_TYPE_VALUES;
use service_com_amazonaws_mq_mq::state::{
    ENGINE_TYPE_ACTIVEMQ, ENGINE_TYPE_RABBITMQ, FIRST_CONFIGURATION_REVISION,
};

use crate::broker_lifecycle::{string_at, Mq};
use crate::configuration_document_syntax::configuration_of;
use crate::configuration_wire_form::base64_data;

/// One write of the table: the document an `UpdateConfiguration` sends and the
/// `Description` it sends with it — `None` leaving the member out altogether.
struct WriteCase {
    what: &'static str,
    document: &'static str,
    description: Option<&'static str>,
}

/// One engine of the table: the writes its parser takes, and a document in the OTHER
/// engine's format, which its parser does not.
struct EngineCase {
    engine_type: &'static str,
    writes: &'static [WriteCase],
    refused: &'static str,
}

/// Every modelled engine, each with documents in the format `Data` documents for it: a
/// Cuttlefish configuration for RabbitMQ, an XML `broker.xml` for ActiveMQ. Each document
/// is already in the form the service stores it in, so the document a read reports is the
/// one the write sent.
const ENGINE_CASES: &[EngineCase] = &[
    EngineCase {
        engine_type: ENGINE_TYPE_RABBITMQ,
        writes: &[
            WriteCase {
                what: "one Cuttlefish assignment",
                document: "consumer_timeout = 3600000\n",
                description: Some("raise the consumer timeout"),
            },
            WriteCase {
                what: "several assignments with comments and a dotted key",
                document: "# management headers\nsecure.management.http.headers.enabled = true\n\n# delivery acknowledgement\nconsumer_timeout = 1800000\n",
                description: None,
            },
            WriteCase {
                what: "the earlier document again under another label",
                document: "consumer_timeout = 3600000\n",
                description: Some("pin the earlier document"),
            },
            WriteCase {
                what: "a dashed key carrying a value with spaces",
                document: "cluster-name = payments cluster one\n",
                description: Some(""),
            },
        ],
        refused: "<broker/>\n",
    },
    EngineCase {
        engine_type: ENGINE_TYPE_ACTIVEMQ,
        writes: &[
            WriteCase {
                what: "the smallest ActiveMQ document",
                document: "<broker/>",
                description: Some("the smallest document"),
            },
            WriteCase {
                what: "a prolog with an attribute and nesting",
                document: "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<broker xmlns=\"http://activemq.apache.org/schema/core\">\n  <destinationPolicy>\n    <policyMap/>\n  </destinationPolicy>\n</broker>",
                description: None,
            },
            WriteCase {
                what: "the earlier document again under another label",
                document: "<broker/>",
                description: Some("pin the earlier document"),
            },
            WriteCase {
                what: "an element carrying text",
                document: "<broker>\n  <brokerName>payments-live</brokerName>\n</broker>",
                description: Some(""),
            },
        ],
        refused: "consumer_timeout = 1800000\n",
    },
];

/// The table must stay the model's: an engine the model declares and no row covers would
/// leave that engine's revisions unread.
#[test]
fn the_table_covers_every_modelled_engine_type() {
    for engine_type in ENGINE_TYPE_VALUES {
        let covered = ENGINE_CASES
            .iter()
            .any(|case| case.engine_type == *engine_type);
        assert!(
            covered,
            "case {engine_type:?}: the table must carry a row for every modelled engine \
             type, got none"
        );
    }
}

/// The `data` member of one revision, read through `DescribeConfigurationRevision` and
/// checked to be the base64 the member documents.
async fn revision_data(mq: &Mq, case: &str, id: &str, revision: i64) -> (String, String) {
    let (status, code, read) = mq
        .describe_configuration_revision(id, &revision.to_string())
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected revision {revision} to read back with 200, got {status} \
         {code:?} {read:?}"
    );
    assert_eq!(
        read.pointer("/revision").and_then(Value::as_i64),
        Some(revision),
        "case {case:?}: the answer must name the revision it describes, got {read:?}"
    );
    assert_eq!(
        string_at(&read, "/configurationId"),
        id,
        "case {case:?}: the answer must name the configuration it describes, got {read:?}"
    );
    let data = read
        .pointer("/data")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!("case {case:?}: the revision must carry a data member, got {read:?}")
        })
        .to_string();
    assert!(
        !data.is_empty(),
        "case {case:?}: the document a revision carries is not empty, got {read:?}"
    );
    let bytes = aws_smithy_types::base64::decode(&data).unwrap_or_else(|error| {
        panic!("case {case:?}: data must be base64 as the member documents, got {data:?} ({error})")
    });
    let document = String::from_utf8(bytes).unwrap_or_else(|error| {
        panic!("case {case:?}: an engine configuration document is text, got {error}")
    });
    assert!(
        !document.trim().is_empty(),
        "case {case:?}: the decoded document carries settings, got {document:?}"
    );
    (data, document)
}

/// The description a revision reports, read through `DescribeConfigurationRevision`.
async fn revision_description(mq: &Mq, case: &str, id: &str, revision: i64) -> String {
    let (status, code, read) = mq
        .describe_configuration_revision(id, &revision.to_string())
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected revision {revision} to read back with 200, got {status} \
         {code:?} {read:?}"
    );
    read.pointer("/description")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!("case {case:?}: the revision must carry a description member, got {read:?}")
        })
        .to_string()
}

/// RULE, the create-time half: the revision a create makes reports a document, and that
/// document is the default of the configuration's OWN engine — the engine that
/// configuration runs accepts it as a write, and the other engine refuses it, so no
/// configuration is ever served a document its own engine cannot read.
#[tokio::test]
async fn the_created_revision_reports_the_default_document_of_its_own_engine() {
    let mq = Mq::new("us-east-1");

    // Both engines' configurations live in ONE store, so a document served from a place
    // that is not this configuration's own record shows up as a foreign document.
    let mut served: Vec<(&str, String, String)> = Vec::new();
    for case in ENGINE_CASES {
        let label = format!("a {} configuration", case.engine_type);
        let id = configuration_of(
            &mq,
            &label,
            &format!("engine-default-read-{}", case.engine_type.to_lowercase()),
            case.engine_type,
        )
        .await;

        let first = i64::from(FIRST_CONFIGURATION_REVISION);
        let (data, document) = revision_data(&mq, &label, &id, first).await;

        // The description the read reports is the one the configuration's own surface
        // reports for that revision, not the empty string and not another revision's.
        let (status, code, described) = mq.describe_configuration(&id).await;
        assert_eq!(
            status, 200,
            "case {label:?}: expected the configuration to read back, got {status} {code:?} \
             {described:?}"
        );
        let expected_description = string_at(&described, "/latestRevision/description");
        assert!(
            !expected_description.is_empty(),
            "case {label:?}: the create-time revision carries a generated description, \
             got {described:?}"
        );
        assert_eq!(
            revision_description(&mq, &label, &id, first).await,
            expected_description,
            "case {label:?}: the read reports this revision's own description"
        );

        served.push((case.engine_type, id, data));
        // Reported for the assertion below, where a document decoded here is handed to
        // the other engine's parser.
        assert!(
            !document.is_empty(),
            "case {label:?}: the decoded document carries settings, got {document:?}"
        );
    }

    // No two engines' defaults are the same document, and each is a document only its own
    // engine parses: handed to the other engine's configuration it is refused naming the
    // member at fault.
    for (engine, _, data) in &served {
        for (other_engine, other_id, other_data) in &served {
            if engine == other_engine {
                continue;
            }
            let label = format!("the {engine} default written to a {other_engine} configuration");
            assert_ne!(
                data, other_data,
                "case {label:?}: two engines cannot share one default document"
            );
            let (status, code, answer) = mq
                .update_configuration(other_id, data, Some("the other engine's default"))
                .await;
            assert_eq!(
                status, 400,
                "case {label:?}: the other engine's document must be refused, got {status} \
                 {code:?} {answer:?}"
            );
            assert_eq!(
                code, "BadRequestException",
                "case {label:?}: expected BadRequestException, got {code:?} {answer:?}"
            );
            assert_eq!(
                answer.pointer("/errorAttribute").and_then(Value::as_str),
                Some("data"),
                "case {label:?}: the refusal must name the member at fault, got {answer:?}"
            );
        }
    }

    // And each engine takes its own default back: the read-modify-write loop a client
    // runs on a configuration it did not write starts from a document the engine parses,
    // and the revision the create made still reports it afterwards.
    for (engine, id, data) in &served {
        let label = format!("the {engine} default written back");
        let (status, code, updated) = mq
            .update_configuration(id, data, Some("the default, written back"))
            .await;
        assert_eq!(
            status, 200,
            "case {label:?}: an engine's own default must be accepted, got {status} {code:?} \
             {updated:?}"
        );
        let next = updated
            .pointer("/latestRevision/revision")
            .and_then(Value::as_i64)
            .unwrap_or_else(|| panic!("case {label:?}: the write must name its revision, got {updated:?}"));
        let (written, _) = revision_data(&mq, &label, id, next).await;
        assert_eq!(
            written, *data,
            "case {label:?}: the revision the write made reports the document it sent"
        );
        let (first, _) = revision_data(&mq, &label, id, i64::from(FIRST_CONFIGURATION_REVISION)).await;
        assert_eq!(
            first, *data,
            "case {label:?}: a later write does not change the revision the create made"
        );
    }
}

/// RULE, the update half: every revision reports the document and the description of the
/// write that was assigned ITS number — whatever the writes before and after it sent,
/// whichever refused writes were attempted in between, and in whatever order the chain is
/// read.
#[tokio::test]
async fn every_revision_reports_the_document_and_description_of_its_own_write() {
    let mq = Mq::new("eu-west-2");

    for case in ENGINE_CASES {
        let engine = case.engine_type;
        let id = configuration_of(
            &mq,
            engine,
            &format!("revision-read-back-{}", engine.to_lowercase()),
            engine,
        )
        .await;

        // What each revision must report: the create-time one as the service serves it
        // (the engine default, pinned by the test above), then one per accepted write.
        let first = i64::from(FIRST_CONFIGURATION_REVISION);
        let (default_data, _) = revision_data(&mq, engine, &id, first).await;
        let default_description = revision_description(&mq, engine, &id, first).await;
        let mut expected: Vec<(i64, String, String)> =
            vec![(first, default_data, default_description)];

        let refused_data = base64_data(case.refused);
        for (index, write) in case.writes.iter().enumerate() {
            let label = format!("{} ({engine})", write.what);

            // A write the engine refuses before every accepted one: it makes no revision
            // and takes no number, so the numbers resolved below count the accepted
            // writes rather than the calls made.
            let (status, code, answer) = mq
                .update_configuration(&id, &refused_data, Some("refused in between"))
                .await;
            assert_eq!(
                status, 400,
                "case {label:?}: the write in between must be refused, got {status} {code:?} \
                 {answer:?}"
            );

            let data = base64_data(write.document);
            let (status, code, updated) = mq
                .update_configuration(&id, &data, write.description)
                .await;
            assert_eq!(
                status, 200,
                "case {label:?}: expected 200, got {status} {code:?} {updated:?}"
            );
            let revision = first + index as i64 + 1;
            assert_eq!(
                updated
                    .pointer("/latestRevision/revision")
                    .and_then(Value::as_i64),
                Some(revision),
                "case {label:?}: expected revision {revision}, got {updated:?}"
            );
            expected.push((
                revision,
                data,
                write.description.unwrap_or_default().to_string(),
            ));
        }

        // Read the whole chain newest first and then oldest first, each pass twice: the
        // answer belongs to the revision asked for, and a read changes nothing a later
        // read of the same or another revision sees.
        for (order, newest_first) in [("newest first", true), ("oldest first", false)] {
            for pass in 1..=2 {
                let mut rows: Vec<&(i64, String, String)> = expected.iter().collect();
                if newest_first {
                    rows.reverse();
                }
                for (revision, data, description) in rows {
                    let label = format!("{engine} revision {revision}, {order}, pass {pass}");
                    let (status, code, read) = mq
                        .describe_configuration_revision(&id, &revision.to_string())
                        .await;
                    assert_eq!(
                        status, 200,
                        "case {label:?}: expected 200, got {status} {code:?} {read:?}"
                    );
                    assert_eq!(
                        read.pointer("/data").and_then(Value::as_str),
                        Some(data.as_str()),
                        "case {label:?}: expected the document this revision's write sent, \
                         got {read:?}"
                    );
                    assert_eq!(
                        read.pointer("/description").and_then(Value::as_str),
                        Some(description.as_str()),
                        "case {label:?}: expected the description this revision's write sent, \
                         got {read:?}"
                    );
                    assert_eq!(
                        read.pointer("/revision").and_then(Value::as_i64),
                        Some(*revision),
                        "case {label:?}: the answer names the revision it describes, got {read:?}"
                    );
                }
            }
        }

        // The chain holds exactly the create's revision plus one per accepted write, so no
        // number a read resolved came from a write the service refused.
        let (status, code, listed) = mq.list_configuration_revisions(&id, "maxResults=100").await;
        assert_eq!(
            status, 200,
            "case {engine:?}: expected 200, got {status} {code:?} {listed:?}"
        );
        let numbers: Vec<i64> = listed
            .pointer("/revisions")
            .and_then(Value::as_array)
            .map(|entries| {
                entries
                    .iter()
                    .map(|entry| {
                        entry
                            .pointer("/revision")
                            .and_then(Value::as_i64)
                            .unwrap_or_default()
                    })
                    .collect()
            })
            .unwrap_or_default();
        let expected_numbers: Vec<i64> = expected.iter().map(|(revision, _, _)| *revision).collect();
        assert_eq!(
            numbers, expected_numbers,
            "case {engine:?}: expected the chain {expected_numbers:?}, got {numbers:?} from \
             {listed:?}"
        );
    }
}
