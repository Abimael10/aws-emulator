// SPDX-License-Identifier: CC0-1.0

//! Which namespace a `RenameTable` refusal is ABOUT, and how the refusal says so.
//!
//! THE RULE — a rename addresses two namespaces: the one it would key the record UNDER
//! (`newNamespaceName`, or the `namespace` path label when the request names no new one,
//! because a move of the name alone leaves the table where it is) and the one the table is
//! addressed IN (that same path label). Either being absent from the addressed table
//! bucket is the same `NotFoundException` (HTTP 404) — the model declares no code that
//! tells the two apart
//! (`smithy_query(query="operation", operation="RenameTable")`: PUT
//! `/tables/{tableBucketARN}/{namespace}/{name}/rename`, errors `BadRequestException`
//! 400, `ConflictException` 409, `NotFoundException` 404, and `NotFoundException` carries
//! only `message`) — so the SENTENCE is the only discriminator a client has: the namespace
//! the move would key the record under states the destination sentence, and the namespace
//! the table is addressed in states the plain one. Both are whole fixed sentences: neither
//! names the namespace, the table or the bucket, so nothing of the request or of state
//! appears in either.
//!
//! The ORDER the coordinates are resolved in is part of the rule, because it decides which
//! sentence a request that gets two of them wrong is answered with: inside the addressed
//! table bucket the namespace the move would key the record under is resolved FIRST — this
//! operation creates no namespace, so there is nowhere to key the record — so its absence
//! is stated as such whatever the source coordinates resolve to; the namespace the table
//! is addressed in and the record itself follow, so the plain sentence is what a move
//! ACROSS namespaces whose source namespace has gone is refused with.
//!
//! The rows below span what the rule generalises over: which coordinates the body names
//! (the destination alone, the destination and a new name, a new name alone), the form and
//! length of the namespace name that is absent (the modelled one-character minimum,
//! digits and interior underscores, a long name — all inside `NamespaceName`'s
//! `^[0-9a-z_]*$` and `@length(1, 255)`), whether the caller also presents its
//! `versionToken`, which of the two namespaces is the absent one (the destination, the
//! source, or both), and the moves that are NOT refused at all, so the discrimination is
//! pinned against a served rename and not only against other refusals.

use serde_json::json;
use service_com_amazonaws_s3tables_s3tablebuckets::state::{
    OpenTableFormat, DESTINATION_NAMESPACE_NOT_FOUND_MESSAGE, NAMESPACE_NOT_FOUND_MESSAGE,
    TABLE_NOT_FOUND_MESSAGE,
};

use crate::harness::{Emulator, Reply};

/// The region every case is scoped to; the assertions follow the ARN the create reports,
/// so the region is one choice and not a rule.
const REGION: &str = "us-west-2";

/// The namespace the subject table is created in, and a second namespace the bucket also
/// holds so a served move has somewhere to go. Both satisfy `NamespaceName`'s
/// `^[0-9a-z_]*$`.
const SOURCE_NAMESPACE: &str = "dest_source_ns";
const RESIDENT_NAMESPACE: &str = "dest_resident_ns";

/// The table every case addresses, inside [`SOURCE_NAMESPACE`].
const SUBJECT_TABLE: &str = "subject_tbl";

/// One request, and the answer the rule says it gets.
#[derive(Debug, Clone, Copy)]
struct Case {
    label: &'static str,
    /// The `namespace` path label: `None` for the namespace the table lives in, or a
    /// name the bucket does not hold.
    source_namespace: Option<&'static str>,
    /// The `name` path label: `None` for the table the fixture created.
    name: Option<&'static str>,
    /// `newNamespaceName`, when the body names one.
    new_namespace: Option<&'static str>,
    /// `newName`, when the body names one.
    new_name: Option<&'static str>,
    /// Whether the body also presents the table's current `versionToken`.
    presents_version_token: bool,
    status: u16,
    /// The error code and the fixed sentence, or `None` for a move that is served.
    code: Option<&'static str>,
    message: Option<&'static str>,
}

/// A namespace name no case creates, so every spelling below is absent from the bucket.
/// The spellings span `NamespaceName`'s form: the one-character minimum, digits with
/// interior underscores, and a long name well inside the modelled 255.
const ABSENT_SHORTEST: &str = "z";
const ABSENT_DIGITS: &str = "ns_2026_q1_07";
const ABSENT_LONG: &str =
    "absent_destination_namespace_that_nothing_in_this_bucket_ever_created_0123456789";

const CASES: &[Case] = &[
    Case {
        label: "a destination the bucket does not hold, the table keeping its name",
        source_namespace: None,
        name: None,
        new_namespace: Some(ABSENT_DIGITS),
        new_name: None,
        presents_version_token: false,
        status: 404,
        code: Some("NotFoundException"),
        message: Some(DESTINATION_NAMESPACE_NOT_FOUND_MESSAGE),
    },
    Case {
        label: "a destination the bucket does not hold, and a new name with it",
        source_namespace: None,
        name: None,
        new_namespace: Some(ABSENT_DIGITS),
        new_name: Some("moved_tbl"),
        presents_version_token: false,
        status: 404,
        code: Some("NotFoundException"),
        message: Some(DESTINATION_NAMESPACE_NOT_FOUND_MESSAGE),
    },
    Case {
        label: "an absent destination spelled as the shortest namespace name there is",
        source_namespace: None,
        name: None,
        new_namespace: Some(ABSENT_SHORTEST),
        new_name: None,
        presents_version_token: false,
        status: 404,
        code: Some("NotFoundException"),
        message: Some(DESTINATION_NAMESPACE_NOT_FOUND_MESSAGE),
    },
    Case {
        label: "an absent destination spelled as a long name",
        source_namespace: None,
        name: None,
        new_namespace: Some(ABSENT_LONG),
        new_name: Some("t"),
        presents_version_token: false,
        status: 404,
        code: Some("NotFoundException"),
        message: Some(DESTINATION_NAMESPACE_NOT_FOUND_MESSAGE),
    },
    Case {
        label: "an absent destination, with the caller's current version token presented",
        source_namespace: None,
        name: None,
        new_namespace: Some(ABSENT_DIGITS),
        new_name: None,
        presents_version_token: true,
        status: 404,
        code: Some("NotFoundException"),
        message: Some(DESTINATION_NAMESPACE_NOT_FOUND_MESSAGE),
    },
    Case {
        label: "an absent destination whose name the table's own namespace is not",
        source_namespace: None,
        name: None,
        new_namespace: Some(ABSENT_SHORTEST),
        new_name: Some("subject_tbl"),
        presents_version_token: true,
        status: 404,
        code: Some("NotFoundException"),
        message: Some(DESTINATION_NAMESPACE_NOT_FOUND_MESSAGE),
    },
    Case {
        label: "the namespace the table lives in is gone, the destination is there",
        source_namespace: Some(ABSENT_DIGITS),
        name: None,
        new_namespace: Some(RESIDENT_NAMESPACE),
        new_name: None,
        presents_version_token: false,
        status: 404,
        code: Some("NotFoundException"),
        message: Some(NAMESPACE_NOT_FOUND_MESSAGE),
    },
    Case {
        label: "the namespace the table is addressed in is gone and only a new name is named",
        source_namespace: Some(ABSENT_LONG),
        name: None,
        new_namespace: None,
        new_name: Some("moved_tbl"),
        presents_version_token: false,
        status: 404,
        code: Some("NotFoundException"),
        message: Some(DESTINATION_NAMESPACE_NOT_FOUND_MESSAGE),
    },
    Case {
        label: "neither namespace exists: the destination is resolved first, so it is named",
        source_namespace: Some(ABSENT_SHORTEST),
        name: None,
        new_namespace: Some(ABSENT_DIGITS),
        new_name: None,
        presents_version_token: false,
        status: 404,
        code: Some("NotFoundException"),
        message: Some(DESTINATION_NAMESPACE_NOT_FOUND_MESSAGE),
    },
    Case {
        label: "a table the namespace does not hold, moved to a destination that is absent too",
        source_namespace: None,
        name: Some("absent_tbl"),
        new_namespace: Some(ABSENT_DIGITS),
        new_name: None,
        presents_version_token: false,
        status: 404,
        code: Some("NotFoundException"),
        message: Some(DESTINATION_NAMESPACE_NOT_FOUND_MESSAGE),
    },
    Case {
        label: "a table the namespace does not hold, moved to a destination that is there",
        source_namespace: None,
        name: Some("absent_tbl"),
        new_namespace: Some(RESIDENT_NAMESPACE),
        new_name: None,
        presents_version_token: false,
        status: 404,
        code: Some("NotFoundException"),
        message: Some(TABLE_NOT_FOUND_MESSAGE),
    },
    Case {
        label: "a name-only move whose namespace is gone: the namespace it would stay in",
        source_namespace: Some(ABSENT_SHORTEST),
        name: None,
        new_namespace: None,
        new_name: Some("t0"),
        presents_version_token: false,
        status: 404,
        code: Some("NotFoundException"),
        message: Some(DESTINATION_NAMESPACE_NOT_FOUND_MESSAGE),
    },
    Case {
        label: "a destination the bucket does hold: the move is served",
        source_namespace: None,
        name: None,
        new_namespace: Some(RESIDENT_NAMESPACE),
        new_name: None,
        presents_version_token: false,
        status: 204,
        code: None,
        message: None,
    },
    Case {
        label: "a destination the bucket does hold, a new name and the token: served",
        source_namespace: None,
        name: None,
        new_namespace: Some(RESIDENT_NAMESPACE),
        new_name: Some("moved_tbl"),
        presents_version_token: true,
        status: 204,
        code: None,
        message: None,
    },
];

/// One table bucket holding two namespaces, with [`SUBJECT_TABLE`] in the first: the
/// state every case starts from.
struct Fixture {
    emulator: Emulator,
    bucket_arn: String,
}

impl Fixture {
    async fn new(bucket: &str, case: &str) -> Self {
        let emulator = Emulator::new();
        let created = emulator
            .create_table_bucket(REGION, &json!({ "name": bucket }))
            .await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: CreateTableBucket: expected 200, got {created:?}"
        );
        let bucket_arn = created.text("arn");
        for namespace in [SOURCE_NAMESPACE, RESIDENT_NAMESPACE] {
            let reply = emulator
                .create_namespace(REGION, &bucket_arn, &json!({ "namespace": [namespace] }))
                .await;
            assert_eq!(
                reply.status, 200,
                "case {case:?}: CreateNamespace {namespace}: expected 200, got {reply:?}"
            );
        }
        let reply = emulator
            .create_table(
                REGION,
                &bucket_arn,
                SOURCE_NAMESPACE,
                // `format` is required and `ICEBERG` is the only value the modelled
                // `OpenTableFormat` enum declares.
                &json!({ "name": SUBJECT_TABLE, "format": OpenTableFormat::Iceberg.as_wire() }),
            )
            .await;
        assert_eq!(
            reply.status, 200,
            "case {case:?}: CreateTable {SUBJECT_TABLE}: expected 200, got {reply:?}"
        );
        Self {
            emulator,
            bucket_arn,
        }
    }

    /// The table as `GetTable` reports it at `namespace`/`name`.
    async fn read(&self, namespace: &str, name: &str) -> Reply {
        self.emulator
            .get_table(REGION, &self.bucket_arn, namespace, name)
            .await
    }
}

/// THE RULE over every row: the status, the error code and the fixed sentence each class
/// of absence is stated with, and — for the refusals — the table still exactly where it
/// was, since a refused move writes nothing.
#[tokio::test]
async fn the_namespace_a_rename_refusal_is_about_is_named_by_its_own_sentence() {
    for (index, case) in CASES.iter().enumerate() {
        let label = case.label;
        let fixture = Fixture::new(&format!("rename-dest-ns-{index}"), label).await;

        let before = fixture.read(SOURCE_NAMESPACE, SUBJECT_TABLE).await;
        assert_eq!(
            before.status, 200,
            "case {label:?}: GetTable before the rename: expected 200, got {before:?}"
        );
        let version_token = before.text("versionToken");
        let table_arn = before.text("tableARN");

        let mut request = json!({});
        if let Some(namespace) = case.new_namespace {
            request["newNamespaceName"] = json!(namespace);
        }
        if let Some(name) = case.new_name {
            request["newName"] = json!(name);
        }
        if case.presents_version_token {
            request["versionToken"] = json!(version_token);
        }

        let answer = fixture
            .emulator
            .rename_table(
                REGION,
                &fixture.bucket_arn,
                case.source_namespace.unwrap_or(SOURCE_NAMESPACE),
                case.name.unwrap_or(SUBJECT_TABLE),
                &request,
            )
            .await;
        assert_eq!(
            answer.status, case.status,
            "case {label:?}: expected status {expected}, got {actual:?}",
            expected = case.status,
            actual = answer
        );
        assert_eq!(
            answer.error_code.as_deref(),
            case.code,
            "case {label:?}: expected error code {expected:?}, got {actual:?}",
            expected = case.code,
            actual = answer.error_code
        );
        if let Some(expected) = case.message {
            assert_eq!(
                answer.message(),
                expected,
                "case {label:?}: expected message {expected:?}, got {actual:?}",
                actual = answer.message()
            );
        }

        let after = fixture.read(SOURCE_NAMESPACE, SUBJECT_TABLE).await;
        if case.code.is_some() {
            // A refused move leaves the record where it was, under its own identity.
            assert_eq!(
                after.status, 200,
                "case {label:?}: a refused rename moved the table, got {after:?}"
            );
            assert_eq!(
                after.text("tableARN"),
                table_arn,
                "case {label:?}: a refused rename changed the table's ARN, got {actual:?}",
                actual = after.text("tableARN")
            );
            assert_eq!(
                after.text("versionToken"),
                version_token,
                "case {label:?}: a refused rename settled a new versionToken, got {actual:?}",
                actual = after.text("versionToken")
            );
        } else {
            // A served move re-keys it: nothing answers at the old coordinates, and the
            // record stands, under the same identity, at the ones the request named.
            assert_eq!(
                after.status, 404,
                "case {label:?}: the old coordinates must resolve to nothing after a served \
                 rename, got {after:?}"
            );
            let moved = fixture
                .read(
                    case.new_namespace.unwrap_or(SOURCE_NAMESPACE),
                    case.new_name.unwrap_or(SUBJECT_TABLE),
                )
                .await;
            assert_eq!(
                moved.status, 200,
                "case {label:?}: GetTable at the new coordinates: expected 200, got {moved:?}"
            );
            assert_eq!(
                moved.text("tableARN"),
                table_arn,
                "case {label:?}: the moved table kept no identity, got {actual:?}",
                actual = moved.text("tableARN")
            );
        }
    }
}

/// The two sentences are DIFFERENT, and each is whole fixed text: it names neither the
/// namespace, nor the table, nor the bucket of the request it refuses, so one refusal's
/// message is byte for byte another's whatever coordinates the two requests carried.
#[tokio::test]
async fn the_destination_sentence_is_fixed_text_and_differs_from_the_source_one() {
    let case = "two absent destinations of different names";
    let fixture = Fixture::new("rename-dest-ns-fixed", case).await;

    let mut destination_refusals = Vec::new();
    for absent in [ABSENT_SHORTEST, ABSENT_DIGITS, ABSENT_LONG] {
        let refused = fixture
            .emulator
            .rename_table(
                REGION,
                &fixture.bucket_arn,
                SOURCE_NAMESPACE,
                SUBJECT_TABLE,
                &json!({ "newNamespaceName": absent }),
            )
            .await;
        assert_eq!(
            refused.status, 404,
            "case {case:?}: destination {absent:?}: expected 404, got {refused:?}"
        );
        let message = refused.message();
        assert!(
            !message.contains(absent)
                && !message.contains(SUBJECT_TABLE)
                && !message.contains(SOURCE_NAMESPACE)
                && !message.contains(&fixture.bucket_arn),
            "case {case:?}: destination {absent:?}: the sentence must take nothing from the \
             request, got {message:?}"
        );
        destination_refusals.push(message);
    }
    for message in &destination_refusals {
        assert_eq!(
            message, &destination_refusals[0],
            "case {case:?}: every absent destination states the same fixed sentence, got \
             {message:?} beside {first:?}",
            first = destination_refusals[0]
        );
        assert_ne!(
            message.as_str(),
            NAMESPACE_NOT_FOUND_MESSAGE,
            "case {case:?}: the destination sentence must differ from the source one, got \
             {message:?}"
        );
    }

    // The same bucket, the same table, the SOURCE namespace absent instead: the other
    // sentence, so the two cases a client must tell apart are told apart.
    let source_refused = fixture
        .emulator
        .rename_table(
            REGION,
            &fixture.bucket_arn,
            ABSENT_DIGITS,
            SUBJECT_TABLE,
            &json!({ "newNamespaceName": RESIDENT_NAMESPACE }),
        )
        .await;
    assert_eq!(
        source_refused.status, 404,
        "case {case:?}: an absent source namespace: expected 404, got {source_refused:?}"
    );
    assert_eq!(
        source_refused.message(),
        NAMESPACE_NOT_FOUND_MESSAGE,
        "case {case:?}: an absent source namespace states its own sentence, got {actual:?}",
        actual = source_refused.message()
    );
}
