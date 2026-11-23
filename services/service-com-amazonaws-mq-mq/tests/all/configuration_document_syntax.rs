// SPDX-License-Identifier: CC0-1.0

//! Which configuration document a write may be made from, and what a refused write
//! costs the configuration's revision chain.
//!
//! RULES these tables pin, each one the whole class rather than any one request:
//!
//! 1. Amazon MQ parses the document an `UpdateConfiguration` carries against the engine
//!    the configuration runs — `Data` is "the base64-encoded XML configuration" for
//!    ActiveMQ and "the base64-encoded Cuttlefish configuration" for RabbitMQ
//!    (`smithy_query(query="doc", shape="UpdateConfigurationRequest", member="Data")`) —
//!    and refuses a document that does not parse in that format with HTTP 400,
//!    `x-amzn-errortype: BadRequestException`, `errorAttribute` naming `data`, and the
//!    service's sentence for an unparsable document, whose only variable part is the
//!    reason the parse stopped for.
//! 2. A configuration's revision numbers are consecutive integers from the service's
//!    first revision number and are taken ONLY by a write the service accepted: a
//!    refused write appends no revision and consumes no number, so the next accepted
//!    write is handed the number the refused one did not take, and that number names
//!    nothing until it does.
//! 3. The revision an accepted write makes carries THAT write's document and
//!    description, which is what every later read of that number reports; the revision a
//!    CREATE makes carries the engine's own default document, since no request member
//!    supplies one ("Amazon MQ uses the default configuration (the engine type and
//!    version)").
//!
//! The tables span what the rules generalise over: both engines the model enumerates
//! (`ENGINE_TYPE_VALUES`), the form of a document that does not parse (the other
//! engine's format, text that is no statement at all, an assignment with no value, a key
//! outside the characters a key is spelled with, an element that never closes, a second
//! root element, a document with no element, bytes that are not text), where in the
//! document the parse stops (its first line and a later one), and the documents each
//! engine does take (one and several statements, comments, attributes, nested elements,
//! escaped text).

use serde_json::{json, Value};

use service_com_amazonaws_mq_mq::ops::types::ENGINE_TYPE_VALUES;
use service_com_amazonaws_mq_mq::state::{
    default_configuration_data, default_configuration_document, ENGINE_TYPE_ACTIVEMQ,
    ENGINE_TYPE_RABBITMQ, FIRST_CONFIGURATION_REVISION,
};

use crate::broker_lifecycle::{string_at, Mq};
use crate::configuration_name::configuration_body;

/// Amazon MQ's whole sentence for a `Data` whose document it could not parse: fixed
/// text whose only variable part is the parser's reason.
fn unparsable_message(reason: &str) -> String {
    format!("The specified configuration data could not be parsed for reason [{reason}]. Check your syntax.")
}

/// The reason a parse that stopped on `line` reports.
fn invalid_token(line: usize) -> String {
    format!("invalid token at line {line}")
}

/// One row of the refusal table: a document that is not a configuration document of
/// `engine_type`, and the line the parse stops on.
struct RefusedCase {
    what: &'static str,
    engine_type: &'static str,
    /// The document as BYTES, so a row can send a byte sequence that is not text at all
    /// while the others stay readable.
    document: &'static [u8],
    line: usize,
}

const REFUSED_CASES: &[RefusedCase] = &[
    // The other engine's format: the mistake a client makes by pointing an update at
    // the wrong configuration.
    RefusedCase {
        what: "an ActiveMQ XML document sent as a RabbitMQ configuration",
        engine_type: ENGINE_TYPE_RABBITMQ,
        document: b"<?xml version=\"1.0\"?>\n<broker xmlns=\"http://activemq.apache.org/schema/core\"/>\n",
        line: 1,
    },
    RefusedCase {
        what: "an XML element sent as a RabbitMQ configuration",
        engine_type: ENGINE_TYPE_RABBITMQ,
        document: b"<broker/>\n",
        line: 1,
    },
    RefusedCase {
        what: "a Cuttlefish assignment sent as an ActiveMQ configuration",
        engine_type: ENGINE_TYPE_ACTIVEMQ,
        document: b"consumer_timeout = 1800000\n",
        line: 1,
    },
    RefusedCase {
        what: "a commented Cuttlefish document sent as an ActiveMQ configuration",
        engine_type: ENGINE_TYPE_ACTIVEMQ,
        document: b"# raise the timeout\nconsumer_timeout = 3600000\n",
        line: 1,
    },
    // Text that is no statement of either engine.
    RefusedCase {
        what: "an Erlang term, which is no Cuttlefish statement",
        engine_type: ENGINE_TYPE_RABBITMQ,
        document: b"[{rabbit,[{vm_memory_high_watermark,0.4}]}].",
        line: 1,
    },
    RefusedCase {
        what: "prose where a Cuttlefish statement belongs, after two statements that parse",
        engine_type: ENGINE_TYPE_RABBITMQ,
        document: b"# a comment\nconsumer_timeout = 1800000\nthis is not a setting\n",
        line: 3,
    },
    RefusedCase {
        what: "a Cuttlefish assignment with no value",
        engine_type: ENGINE_TYPE_RABBITMQ,
        document: b"# a comment\nconsumer_timeout =\n",
        line: 2,
    },
    RefusedCase {
        what: "a Cuttlefish key carrying a space",
        engine_type: ENGINE_TYPE_RABBITMQ,
        document: b"consumer timeout = 1800000\n",
        line: 1,
    },
    RefusedCase {
        what: "a line that is no statement, after a comment carrying multi-byte characters",
        engine_type: ENGINE_TYPE_RABBITMQ,
        document: "# le délai de livraison — en millisecondes\nnot a setting\n".as_bytes(),
        line: 2,
    },
    RefusedCase {
        what: "bytes that are not text at all, sent as a RabbitMQ configuration",
        engine_type: ENGINE_TYPE_RABBITMQ,
        document: &[0xff, 0xfe, 0x00, 0x01],
        line: 1,
    },
    // XML that is not a document.
    RefusedCase {
        what: "an XML element that never closes",
        engine_type: ENGINE_TYPE_ACTIVEMQ,
        document: b"<broker><plugins></broker>",
        line: 1,
    },
    RefusedCase {
        what: "a second root element, on the document's second line",
        engine_type: ENGINE_TYPE_ACTIVEMQ,
        document: b"<broker/>\n<broker/>\n",
        line: 2,
    },
    RefusedCase {
        what: "an XML prolog with no element under it",
        engine_type: ENGINE_TYPE_ACTIVEMQ,
        document: b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>",
        line: 1,
    },
    RefusedCase {
        what: "whitespace where an ActiveMQ document belongs",
        engine_type: ENGINE_TYPE_ACTIVEMQ,
        document: b"   \n",
        line: 1,
    },
];

/// One row of the acceptance table: a document of `engine_type` the engine parses, in
/// the form the service stores it (these rows are already canonical, so the document a
/// read reports is the one sent).
struct AcceptedCase {
    what: &'static str,
    engine_type: &'static str,
    document: &'static str,
    /// The `Description` the write sends — `None` sending the member not at all.
    description: Option<&'static str>,
}

const ACCEPTED_CASES: &[AcceptedCase] = &[
    AcceptedCase {
        what: "one Cuttlefish assignment",
        engine_type: ENGINE_TYPE_RABBITMQ,
        document: "consumer_timeout = 3600000\n",
        description: Some("raise the consumer timeout"),
    },
    AcceptedCase {
        what: "a Cuttlefish document with comments, a blank line and a dotted key",
        engine_type: ENGINE_TYPE_RABBITMQ,
        document: "# management headers\nsecure.management.http.headers.enabled = true\n\n# delivery acknowledgement\nconsumer_timeout = 1800000\n",
        description: None,
    },
    AcceptedCase {
        what: "a Cuttlefish value carrying spaces and a dashed key",
        engine_type: ENGINE_TYPE_RABBITMQ,
        document: "cluster-name = payments cluster one\n",
        description: Some(""),
    },
    AcceptedCase {
        what: "one empty ActiveMQ element",
        engine_type: ENGINE_TYPE_ACTIVEMQ,
        document: "<broker/>",
        description: Some("the smallest document"),
    },
    AcceptedCase {
        what: "an ActiveMQ document with a prolog, attributes and nesting",
        engine_type: ENGINE_TYPE_ACTIVEMQ,
        document: "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<broker xmlns=\"http://activemq.apache.org/schema/core\">\n  <destinationPolicy>\n    <policyMap/>\n  </destinationPolicy>\n</broker>",
        description: None,
    },
    AcceptedCase {
        what: "an ActiveMQ document carrying a comment and escaped text",
        engine_type: ENGINE_TYPE_ACTIVEMQ,
        document: "<broker>\n  <!-- the live broker -->\n  <brokerName>a &amp; b</brokerName>\n</broker>",
        description: Some("label the broker"),
    },
];

/// The tables must stay the model's: an engine the model declares and no row covers
/// would leave that engine's parse untested.
#[test]
fn the_tables_cover_every_modelled_engine_type() {
    for engine_type in ENGINE_TYPE_VALUES {
        for (what, covered) in [
            (
                "refused",
                REFUSED_CASES
                    .iter()
                    .any(|case| case.engine_type == *engine_type),
            ),
            (
                "accepted",
                ACCEPTED_CASES
                    .iter()
                    .any(|case| case.engine_type == *engine_type),
            ),
        ] {
            assert!(
                covered,
                "case {engine_type:?}: the {what} table must carry a row for every modelled \
                 engine type, got none"
            );
        }
    }
}

/// Create a configuration of `engine_type` and report its id.
pub(crate) async fn configuration_of(mq: &Mq, case: &str, name: &str, engine_type: &str) -> String {
    let mut body = configuration_body(name);
    body["engineType"] = json!(engine_type);
    let (status, created) = mq.call("POST", "/v1/configurations", Some(body)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: setup create expected 200, got {status} {created:?}"
    );
    assert_eq!(
        created
            .pointer("/latestRevision/revision")
            .and_then(Value::as_i64),
        Some(i64::from(FIRST_CONFIGURATION_REVISION)),
        "case {case:?}: a created configuration starts at the published first revision, \
         got {created:?}"
    );
    string_at(&created, "/id")
}

/// The configuration's latest revision number, read back through its own describe.
async fn latest_revision(mq: &Mq, case: &str, id: &str) -> i64 {
    let (status, code, described) = mq.describe_configuration(id).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the configuration to read back, got {status} {code:?} \
         {described:?}"
    );
    described
        .pointer("/latestRevision/revision")
        .and_then(Value::as_i64)
        .unwrap_or_else(|| panic!("case {case:?}: no latestRevision.revision in {described:?}"))
}

/// How many revisions the configuration's chain holds, through the listing.
async fn revisions_held(mq: &Mq, case: &str, id: &str) -> usize {
    let (status, code, listed) = mq.list_configuration_revisions(id, "maxResults=100").await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200, got {status} {code:?} {listed:?}"
    );
    listed
        .pointer("/revisions")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or_default()
}

/// RULES 1 and 2: a document the configuration's engine cannot parse is refused with
/// Amazon MQ's sentence naming `data`, appends no revision and takes no revision
/// number — so the number it did not take still names nothing, and the next accepted
/// write is handed it.
#[tokio::test]
async fn a_document_the_engine_cannot_parse_is_refused_and_takes_no_revision_number() {
    let mq = Mq::new("us-east-1");

    for (index, case) in REFUSED_CASES.iter().enumerate() {
        let label = format!("{} ({})", case.what, case.engine_type);
        let id = configuration_of(
            &mq,
            &label,
            &format!("unparsable-{index}"),
            case.engine_type,
        )
        .await;
        let data = aws_smithy_types::base64::encode(case.document);

        // The refusal is the same however often it is retried: nothing about the
        // configuration changed, so neither does the answer.
        for attempt in 1..=2 {
            let label = format!("{label}, attempt {attempt}");
            let (status, code, answer) = mq
                .update_configuration(&id, &data, Some("a refused write"))
                .await;
            assert_eq!(
                status, 400,
                "case {label:?}: expected 400, got {status} {code:?} {answer:?}"
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
            assert_eq!(
                answer.pointer("/message").and_then(Value::as_str),
                Some(unparsable_message(&invalid_token(case.line)).as_str()),
                "case {label:?}: unexpected refusal sentence, got {answer:?}"
            );

            // Nothing was taken: neither a revision record nor the number the write
            // would have been given.
            assert_eq!(
                latest_revision(&mq, &label, &id).await,
                i64::from(FIRST_CONFIGURATION_REVISION),
                "case {label:?}: a refused write must make no revision"
            );
            assert_eq!(
                revisions_held(&mq, &label, &id).await,
                1,
                "case {label:?}: the chain must still hold only the create's revision"
            );
            let next = i64::from(FIRST_CONFIGURATION_REVISION) + 1;
            let (status, code, answer) = mq
                .describe_configuration_revision(&id, &next.to_string())
                .await;
            assert_eq!(
                status, 404,
                "case {label:?}: revision {next} must name nothing yet, got {status} {code:?} \
                 {answer:?}"
            );
            assert_eq!(
                code, "NotFoundException",
                "case {label:?}: expected NotFoundException, got {code:?} {answer:?}"
            );
        }

        // The number the refused writes did not take is the one the next accepted write
        // is handed — the off-by-one a consumed number would show up as.
        let accepted = ACCEPTED_CASES
            .iter()
            .find(|accepted| accepted.engine_type == case.engine_type)
            .unwrap_or_else(|| panic!("case {label:?}: no accepted document for this engine"));
        let (status, code, updated) = mq
            .update_configuration(
                &id,
                &aws_smithy_types::base64::encode(accepted.document),
                Some("the write that is accepted"),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {label:?}: expected the parsable document to be accepted, got {status} \
             {code:?} {updated:?}"
        );
        assert_eq!(
            updated
                .pointer("/latestRevision/revision")
                .and_then(Value::as_i64),
            Some(i64::from(FIRST_CONFIGURATION_REVISION) + 1),
            "case {label:?}: the accepted write takes the number no refused write took, \
             got {updated:?}"
        );
    }
}

/// RULES 1 and 3 on the revision a CREATE makes: it carries the engine's own default
/// document, in the format that engine's documents are written in — so reading it and
/// writing it straight back is a write the engine parses, which is the read-modify-write
/// loop every configuration client runs.
#[tokio::test]
async fn the_create_time_revision_carries_a_document_its_own_engine_parses() {
    let mq = Mq::new("us-east-1");

    for engine_type in ENGINE_TYPE_VALUES {
        let id = configuration_of(
            &mq,
            engine_type,
            &format!("engine-default-{}", engine_type.to_lowercase()),
            engine_type,
        )
        .await;

        let first = FIRST_CONFIGURATION_REVISION.to_string();
        let (status, code, read) = mq.describe_configuration_revision(&id, &first).await;
        assert_eq!(
            status, 200,
            "case {engine_type:?}: expected the first revision to read back, got {status} \
             {code:?} {read:?}"
        );
        let data = read
            .pointer("/data")
            .and_then(Value::as_str)
            .unwrap_or_else(|| {
                panic!("case {engine_type:?}: the first revision must carry a document, got {read:?}")
            });
        assert_eq!(
            data,
            default_configuration_data(engine_type),
            "case {engine_type:?}: expected this engine's default document, got {read:?}"
        );
        let decoded = aws_smithy_types::base64::decode(data)
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .unwrap_or_else(|error| {
                panic!("case {engine_type:?}: data must be base64 as the member documents ({error})")
            });
        assert_eq!(
            decoded,
            default_configuration_document(engine_type),
            "case {engine_type:?}: the document must decode to this engine's default"
        );

        // Written straight back: a document the service itself published for this
        // engine is one the same engine parses, and it is stored as it was read.
        let (status, code, updated) = mq
            .update_configuration(&id, data, Some("the default, written back"))
            .await;
        assert_eq!(
            status, 200,
            "case {engine_type:?}: the engine's own default must be accepted, got {status} \
             {code:?} {updated:?}"
        );
        let next = i64::from(FIRST_CONFIGURATION_REVISION) + 1;
        let (status, code, read) = mq
            .describe_configuration_revision(&id, &next.to_string())
            .await;
        assert_eq!(
            status, 200,
            "case {engine_type:?}: expected revision {next} to read back, got {status} \
             {code:?} {read:?}"
        );
        assert_eq!(
            read.pointer("/data").and_then(Value::as_str),
            Some(data),
            "case {engine_type:?}: the document written back must be stored as read, \
             got {read:?}"
        );
    }
}

/// RULES 1, 2 and 3: every document the configuration's engine parses is accepted, the
/// numbers run consecutively over the writes that were accepted, and each number reads
/// back carrying the document and the description of the write that made it.
#[tokio::test]
async fn an_accepted_write_numbers_consecutively_and_reads_back_its_own_document() {
    let mq = Mq::new("eu-west-2");

    for engine_type in ENGINE_TYPE_VALUES {
        let documents: Vec<&AcceptedCase> = ACCEPTED_CASES
            .iter()
            .filter(|case| case.engine_type == *engine_type)
            .collect();
        let id = configuration_of(
            &mq,
            engine_type,
            &format!("parsable-{}", engine_type.to_lowercase()),
            engine_type,
        )
        .await;

        // A refused write between every pair of accepted ones, so the numbering below is
        // a count of the ACCEPTED writes and not of the calls made.
        let refused = REFUSED_CASES
            .iter()
            .find(|case| case.engine_type == *engine_type)
            .unwrap_or_else(|| panic!("case {engine_type:?}: no refused document for this engine"));
        let refused_data = aws_smithy_types::base64::encode(refused.document);

        for (index, case) in documents.iter().enumerate() {
            let label = format!("{} ({engine_type})", case.what);
            let expected_revision = i64::from(FIRST_CONFIGURATION_REVISION) + index as i64 + 1;

            let (status, code, answer) = mq
                .update_configuration(&id, &refused_data, Some("refused in between"))
                .await;
            assert_eq!(
                status, 400,
                "case {label:?}: the write in between must be refused, got {status} {code:?} \
                 {answer:?}"
            );

            let data = aws_smithy_types::base64::encode(case.document);
            let (status, code, updated) = mq
                .update_configuration(&id, &data, case.description)
                .await;
            assert_eq!(
                status, 200,
                "case {label:?}: expected 200, got {status} {code:?} {updated:?}"
            );
            assert_eq!(
                updated
                    .pointer("/latestRevision/revision")
                    .and_then(Value::as_i64),
                Some(expected_revision),
                "case {label:?}: expected revision {expected_revision}, got {updated:?}"
            );
            assert_eq!(
                updated
                    .pointer("/latestRevision/description")
                    .and_then(Value::as_str),
                Some(case.description.unwrap_or("")),
                "case {label:?}: the revision carries the description the write sent, \
                 got {updated:?}"
            );

            // RULE 3: that number reads back carrying THIS write's document and
            // description, whatever the writes before and after it sent.
            let (status, code, read) = mq
                .describe_configuration_revision(&id, &expected_revision.to_string())
                .await;
            assert_eq!(
                status, 200,
                "case {label:?}: expected the revision to read back, got {status} {code:?} \
                 {read:?}"
            );
            assert_eq!(
                read.pointer("/data").and_then(Value::as_str),
                Some(data.as_str()),
                "case {label:?}: expected the document this write sent, got {read:?}"
            );
            assert_eq!(
                read.pointer("/description").and_then(Value::as_str),
                Some(case.description.unwrap_or("")),
                "case {label:?}: expected the description this write sent, got {read:?}"
            );
            assert_eq!(
                read.pointer("/revision").and_then(Value::as_i64),
                Some(expected_revision),
                "case {label:?}: the answer names the revision it describes, got {read:?}"
            );
        }

        // The whole chain, once the rows are done: the create's revision plus exactly one
        // per accepted write, and nothing for the refused ones.
        let expected_held = documents.len() + 1;
        assert_eq!(
            revisions_held(&mq, engine_type, &id).await,
            expected_held,
            "case {engine_type:?}: expected {expected_held} revisions, one per accepted write"
        );

        // Every accepted revision still reports its own document after all the writes:
        // a later write never rewrites an earlier revision.
        for (index, case) in documents.iter().enumerate() {
            let revision = i64::from(FIRST_CONFIGURATION_REVISION) + index as i64 + 1;
            let label = format!("{} ({engine_type}) revision {revision}", case.what);
            let (status, code, read) = mq
                .describe_configuration_revision(&id, &revision.to_string())
                .await;
            assert_eq!(
                status, 200,
                "case {label:?}: expected 200, got {status} {code:?} {read:?}"
            );
            assert_eq!(
                read.pointer("/data").and_then(Value::as_str),
                Some(aws_smithy_types::base64::encode(case.document).as_str()),
                "case {label:?}: expected the document this write sent, got {read:?}"
            );
        }
    }
}
