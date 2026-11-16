// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for how this front door reads a BOOLEAN member of an awsQuery request.
//!
//! The rule under test: a boolean member's value on this wire is one of the spellings in the
//! lexical space of an XML Schema boolean — `true`, `false`, `1` and `0` (W3C XML Schema
//! Part 2, §3.2.2), the numerals standing for the same two values the words do — and text in
//! none of those spellings is refused by the PROTOCOL layer, with HTTP 400,
//! `<Type>Sender</Type>`, the Query protocol's code for a body it could not read and ONE
//! fixed sentence that names neither the member nor the value the request carried.
//!
//! Both halves are asserted over a table, because the rule is the protocol's and not one
//! handler's: the rows differ in which operation carries the member, whether the member is a
//! `Boolean` or a `BooleanOptional`, whether it sits at the top level of the input or inside
//! an element of a list of structures, and what kind of text the value is (a word, a case
//! variant of a spelling, a number outside the space, a padded numeral, a decimal, a
//! negative number, nothing at all). The recorded request is ONE row.
//!
//! The members and their targets come from the model
//! (`smithy_query(query="operation",operation="CreateDBCluster,DeleteDBCluster,\
//! DescribeDBEngineVersions,ModifyDBClusterParameterGroup,RestoreDBClusterToPointInTime")`
//! reports each boolean member and its target shape; `smithy_query(query="shape",\
//! shape="Parameter")` reports `IsModifiable` targeting `Boolean`).
//!
//! Every call goes over the awsQuery wire through the harness this test target shares
//! (`db_cluster_lifecycle::Neptune`), so each assertion reads the XML the emulator itself
//! answered with.

use crate::db_cluster_lifecycle::Neptune;

/// The error code the Query protocol answers a request whose body it could not read with,
/// which is the class a boolean member's text that spells no boolean falls in.
const MALFORMED_INPUT: &str = "MalformedInput";

/// The whole sentence that refusal carries: fixed text, with no member name, no value and no
/// accepted-literal set in it.
const NON_BOOLEAN_VALUE_MESSAGE: &str = service_common::query_boolean_values::NON_BOOLEAN_VALUE_MESSAGE;

/// The XML namespace this service's envelope carries, as the model declares it
/// (`smithy_query(query="service_info")` reports `xmlNamespace.uri`).
///
/// Asserted below because the protocol this service serves is CONSTRUCTED with that namespace
/// (`src/service/xsd_boolean.rs`) rather than taken from the generated entry: a namespace that
/// drifted from the model would change every response envelope silently.
const XML_NAMESPACE: &str = "http://rds.amazonaws.com/doc/2014-10-31/";

/// The request members a row sends.
type Params = &'static [(&'static str, &'static str)];

/// One row of the refusal table: the case, the action, and the request that carries a
/// boolean member whose value is no boolean.
type RefusalRow = (&'static str, &'static str, Params);

/// The table spans what the rule generalises over: the operation, the kind of boolean member
/// (`BooleanOptional`, `Boolean`, and one nested inside an element of a list of structures),
/// and the kind of text the value is.
const REFUSED: &[RefusalRow] = &[
    (
        "a word of assent for a BooleanOptional member",
        "CreateDBCluster",
        &[
            ("DBClusterIdentifier", "emu-bool-refused-a"),
            ("Engine", "neptune"),
            ("StorageEncrypted", "yes"),
        ],
    ),
    (
        "a word of refusal for another BooleanOptional member of the same input",
        "CreateDBCluster",
        &[
            ("DBClusterIdentifier", "emu-bool-refused-b"),
            ("Engine", "neptune"),
            ("DeletionProtection", "no"),
        ],
    ),
    (
        "a spelling of the lexical space in upper case",
        "CreateDBCluster",
        &[
            ("DBClusterIdentifier", "emu-bool-refused-c"),
            ("Engine", "neptune"),
            ("CopyTagsToSnapshot", "TRUE"),
        ],
    ),
    (
        "a spelling of the lexical space in title case",
        "CreateDBCluster",
        &[
            ("DBClusterIdentifier", "emu-bool-refused-d"),
            ("Engine", "neptune"),
            ("EnableIAMDatabaseAuthentication", "False"),
        ],
    ),
    (
        "a number outside the lexical space",
        "CreateDBCluster",
        &[
            ("DBClusterIdentifier", "emu-bool-refused-e"),
            ("Engine", "neptune"),
            ("StorageEncrypted", "2"),
        ],
    ),
    (
        "a negative number",
        "CreateDBCluster",
        &[
            ("DBClusterIdentifier", "emu-bool-refused-f"),
            ("Engine", "neptune"),
            ("StorageEncrypted", "-1"),
        ],
    ),
    (
        "a padded numeral",
        "CreateDBCluster",
        &[
            ("DBClusterIdentifier", "emu-bool-refused-g"),
            ("Engine", "neptune"),
            ("StorageEncrypted", "01"),
        ],
    ),
    (
        "a decimal",
        "CreateDBCluster",
        &[
            ("DBClusterIdentifier", "emu-bool-refused-h"),
            ("Engine", "neptune"),
            ("StorageEncrypted", "1.0"),
        ],
    ),
    (
        "no value at all",
        "CreateDBCluster",
        &[
            ("DBClusterIdentifier", "emu-bool-refused-i"),
            ("Engine", "neptune"),
            ("StorageEncrypted", ""),
        ],
    ),
    (
        "a plain Boolean member of a report",
        "DescribeDBEngineVersions",
        &[("Engine", "neptune"), ("DefaultOnly", "yes")],
    ),
    (
        "a BooleanOptional member of the same report",
        "DescribeDBEngineVersions",
        &[("Engine", "neptune"), ("ListSupportedCharacterSets", "on")],
    ),
    (
        "a plain Boolean member of a delete, whose cluster lookup is never reached",
        "DeleteDBCluster",
        &[
            ("DBClusterIdentifier", "emu-bool-absent-cluster"),
            ("SkipFinalSnapshot", "sure"),
        ],
    ),
    (
        "a plain Boolean member of a restore",
        "RestoreDBClusterToPointInTime",
        &[
            ("DBClusterIdentifier", "emu-bool-restored"),
            ("SourceDBClusterIdentifier", "emu-bool-absent-cluster"),
            ("UseLatestRestorableTime", "latest"),
        ],
    ),
    (
        "a boolean nested inside an element of a list of structures",
        "ModifyDBClusterParameterGroup",
        &[
            ("DBClusterParameterGroupName", "emu-bool-absent-group"),
            (
                "Parameters.Parameter.1.ParameterName",
                "neptune_query_timeout",
            ),
            ("Parameters.Parameter.1.ParameterValue", "10000"),
            ("Parameters.Parameter.1.IsModifiable", "yes"),
        ],
    ),
];

#[tokio::test]
async fn text_that_spells_no_boolean_is_refused_with_the_fixed_sentence() {
    let neptune = Neptune::new();
    for (case, action, params) in REFUSED {
        let reply = neptune.call(action, params).await;
        assert_eq!(
            reply.status, 400,
            "case {case:?}: expected HTTP 400, got {} with body {}",
            reply.status, reply.body
        );
        assert_eq!(
            reply.error_code(),
            MALFORMED_INPUT,
            "case {case:?}: expected the code {MALFORMED_INPUT:?}, got body {}",
            reply.body
        );
        let message = reply.error_message();
        assert_eq!(
            message, NON_BOOLEAN_VALUE_MESSAGE,
            "case {case:?}: expected {NON_BOOLEAN_VALUE_MESSAGE:?}, got {message:?}"
        );
    }
}

#[tokio::test]
async fn a_refused_boolean_leaves_the_request_unserved() {
    let neptune = Neptune::new();
    let identifier = "emu-bool-unserved";
    let refused = neptune
        .call(
            "CreateDBCluster",
            &[
                ("DBClusterIdentifier", identifier),
                ("Engine", "neptune"),
                ("StorageEncrypted", "yes"),
            ],
        )
        .await;
    assert_eq!(
        refused.error_message(),
        NON_BOOLEAN_VALUE_MESSAGE,
        "expected the create to be refused for the boolean, got body {}",
        refused.body
    );

    // The refusal is the protocol's, ahead of the handler, so nothing was created.
    let described = neptune
        .call("DescribeDBClusters", &[("DBClusterIdentifier", identifier)])
        .await;
    assert_eq!(
        described.status, 404,
        "expected the refused cluster to exist nowhere, got {} with body {}",
        described.status, described.body
    );
    assert_eq!(
        described.error_code(),
        "DBClusterNotFoundFault",
        "expected the cluster reported absent, got body {}",
        described.body
    );
}

/// One row of the accepted table: the case, the member the request sends, the spelling it
/// sends, the element the cluster report names that member by, and the value it must report.
struct AcceptedRow {
    case: &'static str,
    member: &'static str,
    spelling: &'static str,
    reported_as: &'static str,
    reported: &'static str,
}

/// Every spelling of the lexical space, on every boolean member of a create this service
/// reports back: the two words and the two numerals, each on a member of its own, so the
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
        case: "the word false",
        member: "StorageEncrypted",
        spelling: "false",
        reported_as: "StorageEncrypted",
        reported: "false",
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
        case: "the numeral one on the deletion-protection member",
        member: "DeletionProtection",
        spelling: "1",
        reported_as: "DeletionProtection",
        reported: "true",
    },
    AcceptedRow {
        case: "the numeral zero on the deletion-protection member",
        member: "DeletionProtection",
        spelling: "0",
        reported_as: "DeletionProtection",
        reported: "false",
    },
    AcceptedRow {
        case: "the numeral one on the copy-tags member",
        member: "CopyTagsToSnapshot",
        spelling: "1",
        reported_as: "CopyTagsToSnapshot",
        reported: "true",
    },
    AcceptedRow {
        case: "the numeral one on the database-authentication member",
        member: "EnableIAMDatabaseAuthentication",
        spelling: "1",
        reported_as: "IAMDatabaseAuthenticationEnabled",
        reported: "true",
    },
    AcceptedRow {
        case: "the word true on the database-authentication member",
        member: "EnableIAMDatabaseAuthentication",
        spelling: "true",
        reported_as: "IAMDatabaseAuthenticationEnabled",
        reported: "true",
    },
];

#[tokio::test]
async fn every_spelling_of_the_lexical_space_is_read_as_the_boolean_it_spells() {
    let neptune = Neptune::new();
    for (index, row) in ACCEPTED.iter().enumerate() {
        let identifier = format!("emu-bool-read-{index}");
        let created = neptune
            .call(
                "CreateDBCluster",
                &[
                    ("DBClusterIdentifier", identifier.as_str()),
                    ("Engine", "neptune"),
                    (row.member, row.spelling),
                ],
            )
            .await;
        assert_eq!(
            created.status, 200,
            "case {:?}: expected the create to be served, got {} with body {}",
            row.case, created.status, created.body
        );
        // The envelope the protocol this service serves writes is the model's own, namespace
        // and all — only the reading of a boolean's text differs from the framework's.
        assert!(
            created.body.contains(&format!("xmlns=\"{XML_NAMESPACE}\"")),
            "case {:?}: expected the response envelope to carry the model's namespace \
             {XML_NAMESPACE:?}, got {}",
            row.case,
            created.body
        );
        let answered = created.field(&["CreateDBClusterResult", "DBCluster", row.reported_as]);
        assert_eq!(
            answered, row.reported,
            "case {:?}: expected {} to be reported {:?}, got {answered:?} in {}",
            row.case, row.reported_as, row.reported, created.body
        );

        // And the value the create read is the value the record holds: read it back.
        let described = neptune
            .call(
                "DescribeDBClusters",
                &[("DBClusterIdentifier", identifier.as_str())],
            )
            .await;
        assert_eq!(
            described.status, 200,
            "case {:?}: expected the describe to be served, got {} with body {}",
            row.case, described.status, described.body
        );
        let stored = described.field(&[
            "DescribeDBClustersResult",
            "DBClusters",
            "DBCluster",
            row.reported_as,
        ]);
        assert_eq!(
            stored, row.reported,
            "case {:?}: expected the stored {} to read back {:?}, got {stored:?} in {}",
            row.case, row.reported_as, row.reported, described.body
        );
    }
}

#[tokio::test]
async fn a_numeric_spelling_is_read_on_another_operation_too() {
    let neptune = Neptune::new();
    let cluster = "emu-bool-instances";
    let seeded = neptune
        .call(
            "CreateDBCluster",
            &[("DBClusterIdentifier", cluster), ("Engine", "neptune")],
        )
        .await;
    assert_eq!(
        seeded.status, 200,
        "seeding the cluster: got {} with body {}",
        seeded.status, seeded.body
    );

    // The same rule on a SECOND operation, over its own boolean members
    // (`smithy_query(query="operation",operation="CreateDBInstance")` reports
    // PubliclyAccessible / AutoMinorVersionUpgrade / MultiAZ / DeletionProtection targeting
    // BooleanOptional), each sent in a spelling of the lexical space.
    for (index, (case, member, spelling, reported_as, reported)) in [
        (
            "the numeral one",
            "PubliclyAccessible",
            "1",
            "PubliclyAccessible",
            "true",
        ),
        (
            "the numeral zero, where the default is true",
            "AutoMinorVersionUpgrade",
            "0",
            "AutoMinorVersionUpgrade",
            "false",
        ),
        ("the word true", "MultiAZ", "true", "MultiAZ", "true"),
        (
            "the numeral one on the deletion-protection member",
            "DeletionProtection",
            "1",
            "DeletionProtection",
            "true",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let identifier = format!("emu-bool-instance-{index}");
        let created = neptune
            .call(
                "CreateDBInstance",
                &[
                    ("DBInstanceIdentifier", identifier.as_str()),
                    ("DBInstanceClass", "db.r5.large"),
                    ("Engine", "neptune"),
                    ("DBClusterIdentifier", cluster),
                    (member, spelling),
                ],
            )
            .await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: expected the create to be served, got {} with body {}",
            created.status, created.body
        );
        let described = neptune
            .call(
                "DescribeDBInstances",
                &[("DBInstanceIdentifier", identifier.as_str())],
            )
            .await;
        let stored = described.field(&[
            "DescribeDBInstancesResult",
            "DBInstances",
            "DBInstance",
            reported_as,
        ]);
        assert_eq!(
            stored, reported,
            "case {case:?}: expected the stored {reported_as} to read back {reported:?}, got \
             {stored:?} in {}",
            described.body
        );
    }

    // And a value in no spelling of the space is refused on that operation too.
    let refused = neptune
        .call(
            "CreateDBInstance",
            &[
                ("DBInstanceIdentifier", "emu-bool-instance-refused"),
                ("DBInstanceClass", "db.r5.large"),
                ("Engine", "neptune"),
                ("DBClusterIdentifier", cluster),
                ("PubliclyAccessible", "yes"),
            ],
        )
        .await;
    assert_eq!(
        (refused.status, refused.error_code().as_str()),
        (400, MALFORMED_INPUT),
        "expected the instance create to be refused for the boolean, got body {}",
        refused.body
    );
    assert_eq!(
        refused.error_message(),
        NON_BOOLEAN_VALUE_MESSAGE,
        "expected {NON_BOOLEAN_VALUE_MESSAGE:?}, got {:?}",
        refused.error_message()
    );
}
