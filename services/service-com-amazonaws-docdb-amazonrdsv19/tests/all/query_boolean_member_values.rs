// SPDX-License-Identifier: CC0-1.0

//! How this front door reads the value of a BOOLEAN MEMBER of a request.
//!
//! The rule under test: a boolean member's value on this Query wire is one of the spellings in
//! the lexical space of an XML Schema boolean — `true`, `false`, `1` and `0` (W3C XML Schema
//! Part 2, §3.2.2), the numerals standing for the same two values the words do — and text in
//! none of those spellings is refused by the PROTOCOL layer, with HTTP 400,
//! `<Type>Sender</Type>`, the Query protocol's code for a body it could not read, and ONE
//! fixed sentence that names neither the member nor the value the request carried.
//!
//! It is the protocol's rule and not any handler's, so the rows span the operations and the
//! members: a `BooleanOptional` member of a create, a plain `Boolean` member of a delete whose
//! cluster does not even exist (the refusal happens while the request is READ, ahead of every
//! handler), and a boolean nested inside an element of a list of structures. The members and
//! their targets come from the model
//! (`smithy_query(query="operation",operation="CreateDBCluster,DeleteDBCluster,\
//! ModifyDBClusterParameterGroup")`; `smithy_query(query="shape",shape="Parameter")` reports
//! `IsModifiable` targeting `Boolean`).
//!
//! Every call goes over the wire through the harness in `cluster_lifecycle.rs`, so each
//! assertion reads the XML the emulator itself answered with.

use crate::cluster_lifecycle::{error_code, text, Docdb};

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

#[tokio::test]
async fn text_that_spells_no_boolean_is_refused_with_the_fixed_sentence() {
    let docdb = Docdb::new();
    let rows: [RefusalRow; 5] = [
        (
            "a word of assent for a BooleanOptional member of a create",
            "CreateDBCluster",
            &[
                ("DBClusterIdentifier", "emu-boolean-a"),
                ("Engine", "docdb"),
                ("StorageEncrypted", "yes"),
            ],
        ),
        (
            "a spelling of the space in upper case",
            "CreateDBCluster",
            &[
                ("DBClusterIdentifier", "emu-boolean-b"),
                ("Engine", "docdb"),
                ("DeletionProtection", "TRUE"),
            ],
        ),
        (
            "a number outside the space",
            "CreateDBCluster",
            &[
                ("DBClusterIdentifier", "emu-boolean-c"),
                ("Engine", "docdb"),
                ("CopyTagsToSnapshot", "2"),
            ],
        ),
        (
            "no value at all for a plain Boolean member of a delete, whose cluster is absent",
            "DeleteDBCluster",
            &[
                ("DBClusterIdentifier", "emu-boolean-absent"),
                ("SkipFinalSnapshot", ""),
            ],
        ),
        (
            "a boolean nested inside an element of a list of structures",
            "ModifyDBClusterParameterGroup",
            &[
                ("DBClusterParameterGroupName", "emu-boolean-absent-group"),
                ("Parameters.Parameter.1.ParameterName", "not-a-parameter"),
                ("Parameters.Parameter.1.ParameterValue", "enabled"),
                ("Parameters.Parameter.1.IsModifiable", "yes"),
            ],
        ),
    ];

    for (case, action, params) in rows {
        let reply = docdb.call(action, params).await;
        assert_eq!(
            reply.status, 400,
            "case {case:?}: expected HTTP 400, got {} with {}",
            reply.status, reply.xml
        );
        assert_eq!(
            error_code(&reply.xml),
            MALFORMED_INPUT,
            "case {case:?}: expected the code {MALFORMED_INPUT:?}, got {}",
            reply.xml
        );
        let message = text(&reply.xml, "Message");
        assert_eq!(
            message, NON_BOOLEAN_VALUE_MESSAGE,
            "case {case:?}: expected {NON_BOOLEAN_VALUE_MESSAGE:?}, got {message:?}"
        );
    }
}

#[tokio::test]
async fn every_spelling_of_the_lexical_space_is_read_as_the_boolean_it_spells() {
    let docdb = Docdb::new();
    // (case, member, the spelling the request sends, the value the cluster must report)
    for (index, (case, member, spelling, reported)) in [
        ("the word true", "StorageEncrypted", "true", "true"),
        ("the numeral one", "StorageEncrypted", "1", "true"),
        ("the numeral zero", "StorageEncrypted", "0", "false"),
        ("the word false", "StorageEncrypted", "false", "false"),
        (
            "the numeral one on the deletion-protection member",
            "DeletionProtection",
            "1",
            "true",
        ),
        (
            "the numeral zero on the copy-tags member",
            "CopyTagsToSnapshot",
            "0",
            "false",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let identifier = format!("emu-boolean-read-{index}");
        let created = docdb
            .ok(
                case,
                "CreateDBCluster",
                &[
                    ("DBClusterIdentifier", identifier.as_str()),
                    ("Engine", "docdb"),
                    (member, spelling),
                ],
            )
            .await;
        assert_eq!(
            text(&created, member),
            reported,
            "case {case:?}: expected {member} to be reported {reported:?}, got {created}"
        );
        // The envelope the protocol this service serves writes is the model's own, namespace
        // and all — only the reading of a boolean's text differs from the framework's.
        assert!(
            created.contains(&format!("xmlns=\"{XML_NAMESPACE}\"")),
            "case {case:?}: expected the response envelope to carry the model's namespace \
             {XML_NAMESPACE:?}, got {created}"
        );

        // The value the create read is the value the record holds: read it back.
        let described = docdb
            .ok(
                case,
                "DescribeDBClusters",
                &[("DBClusterIdentifier", identifier.as_str())],
            )
            .await;
        assert_eq!(
            text(&described, member),
            reported,
            "case {case:?}: expected the stored {member} to read back {reported:?}, got {described}"
        );
    }
}
