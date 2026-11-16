// SPDX-License-Identifier: CC0-1.0

//! How this front door reads the value of a BOOLEAN MEMBER of a request — the member itself,
//! not a `ParameterValue` a parameter group carries (which is a string member, and whose own
//! refusal the sibling suites `boolean_parameter_value` and `boolean_refusal_request_shape`
//! span).
//!
//! The rule under test: a boolean member's value on this Query wire is one of the spellings in
//! the lexical space of an XML Schema boolean — `true`, `false`, `1` and `0` (W3C XML Schema
//! Part 2, §3.2.2), the numerals standing for the same two values the words do, read after the
//! `whiteSpace = collapse` facet that type carries has stripped the whitespace around the
//! value — and text in none of those spellings is refused by the PROTOCOL layer, with HTTP
//! 400, `<Type>Sender</Type>`, the Query protocol's code for a body it could not read, and ONE
//! fixed sentence that names neither the member nor the value the request carried.
//!
//! It is the protocol's rule rather than any handler's, so the table spans the operations and
//! the members rather than one of each: a `BooleanOptional` member and a plain `Boolean` one,
//! on a create, on a delete, on a copy and on a report, a boolean nested inside an element of
//! a list of structures, and — for the refusal — an operation whose own resource does not even
//! exist, because the refusal happens while the request is READ, ahead of every handler. The
//! members and their targets come from the model
//! (`smithy_query(query="operation",operation="CreateDBInstance,CreateDBCluster,\
//! DeleteDBInstance,CopyDBClusterSnapshot,DescribeDBEngineVersions,\
//! ModifyDBClusterParameterGroup")`; `smithy_query(query="shape",shape="Parameter")` reports
//! `IsModifiable` targeting `Boolean`).
//!
//! Every call goes over the wire through the harness in `db_instances.rs`, so each assertion
//! reads the XML the emulator itself answered with.

use crate::db_instances::{call, emulator, expect_ok, subtrees, text};

/// The error code the Query protocol answers a request whose body it could not read with,
/// which is the class a boolean member's text that spells no boolean falls in.
const MALFORMED_INPUT: &str = "MalformedInput";

/// The whole sentence that refusal carries, read from the module that owns the rule rather
/// than restated here.
const NON_BOOLEAN_VALUE_MESSAGE: &str =
    service_common::query_boolean_values::NON_BOOLEAN_VALUE_MESSAGE;

/// The XML namespace this service's envelope carries, as the model declares it
/// (`smithy_query(query="service_info")` reports `xmlNamespace.uri`).
///
/// Asserted below because the protocol this service serves is CONSTRUCTED with that namespace
/// (`src/service.rs`) rather than taken from the generated entry: a namespace that drifted
/// from the model would change every response envelope silently.
const XML_NAMESPACE: &str = "http://rds.amazonaws.com/doc/2014-10-31/";

/// One row of the refusal table: the case, the action, and the request carrying a boolean
/// member whose value is no boolean.
type RefusalRow = (
    &'static str,
    &'static str,
    &'static [(&'static str, &'static str)],
);

const REFUSED: &[RefusalRow] = &[
    (
        "a word of assent for a BooleanOptional member of a create",
        "CreateDBInstance",
        &[
            ("DBInstanceIdentifier", "emu-boolean-a"),
            ("Engine", "postgres"),
            ("DBInstanceClass", "db.t3.micro"),
            ("StorageEncrypted", "yes"),
        ],
    ),
    (
        "a spelling of the space in upper case",
        "CreateDBInstance",
        &[
            ("DBInstanceIdentifier", "emu-boolean-b"),
            ("Engine", "postgres"),
            ("DBInstanceClass", "db.t3.micro"),
            ("PubliclyAccessible", "TRUE"),
        ],
    ),
    (
        "a number outside the space on a cluster create",
        "CreateDBCluster",
        &[
            ("DBClusterIdentifier", "emu-boolean-c"),
            ("Engine", "aurora-postgresql"),
            ("DeletionProtection", "2"),
        ],
    ),
    (
        "a negative number on a copy",
        "CopyDBClusterSnapshot",
        &[
            ("SourceDBClusterSnapshotIdentifier", "emu-boolean-source"),
            ("TargetDBClusterSnapshotIdentifier", "emu-boolean-target"),
            ("CopyTags", "-1"),
        ],
    ),
    (
        "no value at all for a plain Boolean member of a delete, whose instance does not exist",
        "DeleteDBInstance",
        &[
            ("DBInstanceIdentifier", "emu-boolean-absent"),
            ("SkipFinalSnapshot", ""),
        ],
    ),
    (
        "a word of assent for a plain Boolean member of a report",
        "DescribeDBEngineVersions",
        &[("Engine", "postgres"), ("DefaultOnly", "yes")],
    ),
    (
        "whitespace alone, which the collapse facet leaves empty",
        "DescribeDBEngineVersions",
        &[("Engine", "postgres"), ("DefaultOnly", "   ")],
    ),
    (
        "a word with whitespace inside it",
        "DescribeDBEngineVersions",
        &[("Engine", "postgres"), ("DefaultOnly", "tr ue")],
    ),
    (
        "a padded numeral for a BooleanOptional member of the same report",
        "DescribeDBEngineVersions",
        &[("Engine", "postgres"), ("ListSupportedCharacterSets", "01")],
    ),
    (
        "a boolean nested inside an element of a list of structures",
        "ModifyDBClusterParameterGroup",
        &[
            ("DBClusterParameterGroupName", "emu-boolean-absent-group"),
            ("Parameters.Parameter.1.ParameterName", "not-a-parameter"),
            ("Parameters.Parameter.1.ParameterValue", "1"),
            ("Parameters.Parameter.1.IsModifiable", "yes"),
        ],
    ),
];

#[tokio::test]
async fn text_that_spells_no_boolean_is_refused_with_the_fixed_sentence() {
    let router = emulator();
    for (case, action, params) in REFUSED {
        let (status, body) = call(&router, action, params).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with body {body}"
        );
        let code = text(&body, "Code");
        assert_eq!(
            code, MALFORMED_INPUT,
            "case {case:?}: expected the code {MALFORMED_INPUT:?}, got {code:?} in {body}"
        );
        let message = text(&body, "Message");
        assert_eq!(
            message, NON_BOOLEAN_VALUE_MESSAGE,
            "case {case:?}: expected {NON_BOOLEAN_VALUE_MESSAGE:?}, got {message:?}"
        );
        let kind = text(&body, "Type");
        assert_eq!(
            kind, "Sender",
            "case {case:?}: expected a sender fault, got {kind:?} in {body}"
        );
    }
}

/// One row of the accepted table: the case, the member the request sends, the spelling it
/// sends, the element the instance report names that member by, and the value it must report.
struct AcceptedRow {
    case: &'static str,
    member: &'static str,
    spelling: &'static str,
    reported_as: &'static str,
    reported: &'static str,
}

/// Every spelling of the lexical space, on boolean members a create reports back — so the
/// numerals are read as the values they stand for rather than refused.
const ACCEPTED: &[AcceptedRow] = &[
    AcceptedRow {
        case: "the word true",
        member: "StorageEncrypted",
        spelling: "true",
        reported_as: "StorageEncrypted",
        reported: "true",
    },
    AcceptedRow {
        case: "the numeral one",
        member: "StorageEncrypted",
        spelling: "1",
        reported_as: "StorageEncrypted",
        reported: "true",
    },
    AcceptedRow {
        case: "the numeral zero",
        member: "StorageEncrypted",
        spelling: "0",
        reported_as: "StorageEncrypted",
        reported: "false",
    },
    AcceptedRow {
        case: "the word false",
        member: "StorageEncrypted",
        spelling: "false",
        reported_as: "StorageEncrypted",
        reported: "false",
    },
    AcceptedRow {
        case: "the numeral one on the multi-AZ member",
        member: "MultiAZ",
        spelling: "1",
        reported_as: "MultiAZ",
        reported: "true",
    },
    AcceptedRow {
        case: "the numeral zero on the public-accessibility member",
        member: "PubliclyAccessible",
        spelling: "0",
        reported_as: "PubliclyAccessible",
        reported: "false",
    },
    AcceptedRow {
        case: "the numeral one on the deletion-protection member",
        member: "DeletionProtection",
        spelling: "1",
        reported_as: "DeletionProtection",
        reported: "true",
    },
    // The `whiteSpace = collapse` facet the type carries: the whitespace around a value is
    // not part of it, so a padded spelling is the value it pads (W3C XML Schema Part 2,
    // §3.2.2 and §4.3.6). The harness percent-encodes a value, so this reaches the wire as
    // the caller sent it.
    AcceptedRow {
        case: "a numeral with whitespace around it",
        member: "StorageEncrypted",
        spelling: " 1 ",
        reported_as: "StorageEncrypted",
        reported: "true",
    },
    AcceptedRow {
        case: "a word with whitespace around it",
        member: "MultiAZ",
        spelling: " false ",
        reported_as: "MultiAZ",
        reported: "false",
    },
];

#[tokio::test]
async fn every_spelling_of_the_lexical_space_is_read_as_the_boolean_it_spells() {
    let router = emulator();
    for (index, row) in ACCEPTED.iter().enumerate() {
        let identifier = format!("emu-boolean-read-{index}");
        let case = row.case;
        expect_ok(
            case,
            call(
                &router,
                "CreateDBInstance",
                &[
                    ("DBInstanceIdentifier", identifier.as_str()),
                    ("Engine", "postgres"),
                    ("DBInstanceClass", "db.t3.micro"),
                    (row.member, row.spelling),
                ],
            )
            .await,
        );

        // The value the create read is the value the record holds: read it back.
        let reported = expect_ok(
            case,
            call(
                &router,
                "DescribeDBInstances",
                &[("DBInstanceIdentifier", identifier.as_str())],
            )
            .await,
        );
        // The envelope the protocol this service serves writes is the model's own, namespace
        // and all — only the reading of a boolean's text differs from the framework's.
        assert!(
            reported.contains(&format!("xmlns=\"{XML_NAMESPACE}\"")),
            "case {case:?}: expected the response envelope to carry the model's namespace \
             {XML_NAMESPACE:?}, got {reported}"
        );
        let instance = subtrees(&reported, "DBInstance")
            .into_iter()
            .next()
            .unwrap_or_else(|| panic!("case {case:?}: expected the instance reported, got {reported}"));
        let stored = text(&instance, row.reported_as);
        assert_eq!(
            stored, row.reported,
            "case {case:?}: expected the stored {} to read back {:?}, got {stored:?} in {reported}",
            row.reported_as, row.reported
        );
    }
}
