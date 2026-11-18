// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for this service's REFUSAL identities: which error
//! class a refused request names, and the message that class carries.
//!
//! Every test drives the service the way the router does, through the harness
//! `terminology_lifecycle` already builds (`router`, `call`, `error_code`,
//! `message`, `import`), so the wire protocol, the boundary constraint
//! validation, the service's refusal wording and the handlers are all
//! exercised.
//!
//! Each test asserts a RULE over a set of inputs rather than one recorded
//! request: the sizes on both sides of the text limit, several language pairs,
//! each terminology operation's own not-found wording, several rejected enum
//! values, and tag counts around the per-resource tag limit.

use serde_json::{json, Value};

use service_com_amazonaws_translate_awsshinefrontendservice_20170701::ops::types::DISPLAY_LANGUAGE_CODE_VALUES;

use crate::terminology_lifecycle::{
    call, error_code, expected_arn, import, message, router, string_at, ACCOUNT, EN_FR_CSV,
};

/// The envelope a request refused for one broken constraint is stated in.
fn one_violation(clause: &str) -> String {
    format!("1 validation error detected: {clause}")
}

/// The clause stating that `value`, submitted at `path`, is outside the
/// enumerated type's accepted-value set — the set listed as the service lists
/// it.
fn enum_clause(value: &str, path: &str, value_set: &str) -> String {
    format!(
        "Value '{value}' at '{path}' failed to satisfy constraint: Member must satisfy enum \
         value set: {value_set}"
    )
}

/// The clause stating that `value`, submitted at `path`, does not match the
/// expression the member's values have to match.
fn pattern_clause(value: &str, path: &str, expression: &str) -> String {
    format!(
        "Value '{value}' at '{path}' failed to satisfy constraint: Member must satisfy regular \
         expression pattern: {expression}"
    )
}

/// The base64 encoding `awsJson1_1` carries a blob member in.
fn blob(file: &[u8]) -> String {
    aws_smithy_types::base64::encode(file)
}

/// A `CreateParallelData` request whose members are all inside their
/// constraints, for a test to break exactly one of them.
fn parallel_data_body(name: &str) -> Value {
    json!({
        "Name": name,
        "ClientToken": "client-token-1",
        "ParallelDataConfig": {"S3Uri": "s3://bucket-one/corpus.tsv", "Format": "TSV"},
    })
}

/// An `ImportTerminology` request whose members are all inside their
/// constraints, for a test to break exactly one of them.
fn import_terminology_body(name: &str) -> Value {
    json!({
        "Name": name,
        "MergeStrategy": "OVERWRITE",
        "TerminologyData": {"File": blob(EN_FR_CSV), "Format": "CSV"},
    })
}

/// A `StartTextTranslationJob` request whose members are all inside their
/// constraints, for a test to break exactly one of them.
fn translation_job_body(name: &str) -> Value {
    json!({
        "JobName": name,
        "InputDataConfig": {"S3Uri": "s3://bucket-one/in/", "ContentType": "text/plain"},
        "OutputDataConfig": {"S3Uri": "s3://bucket-one/out/"},
        "DataAccessRoleArn": "arn:aws:iam::123456789012:role/translate-batch",
        "SourceLanguageCode": "en",
        "TargetLanguageCodes": ["fr"],
        "ClientToken": "client-token-2",
    })
}

/// The documented size limit on a translation request's text, in BYTES: "The
/// text string can be a maximum of 10,000 bytes long"
/// (`smithy_query shape=TranslateTextRequest member=Text`).
const MAX_TEXT_BYTES: usize = 10_000;

/// The documented tag limit for one resource: "There can be a maximum of 50
/// tags (both existing and pending) associated with a specific resource"
/// (`smithy_query shape=TagResourceRequest member=Tags`).
const MAX_TAGS_PER_RESOURCE: usize = 50;

/// A paragraph of English, so a request that asks the service to determine its
/// own source language resolves to a concrete code.
const ENGLISH_TEXT: &str = "The quick brown fox jumps over the lazy dog while the gardener \
                            waters the roses in the early morning light.";

fn translate_body(text: &str, source: &str, target: &str) -> Value {
    json!({
        "Text": text,
        "SourceLanguageCode": source,
        "TargetLanguageCode": target,
    })
}

/// The size limit is a BYTE limit, so what decides a refusal is the UTF-8 length
/// of the request's text, not its character count — and the refusal is the
/// operation's own `TextSizeLimitExceededException`, naming the limit and the
/// size that was sent.
///
/// The rows differ in character width as well as in size: a multi-byte text can
/// be well inside the member's character bound and still past the byte limit,
/// which is the case no character-length constraint can decide.
#[tokio::test]
async fn text_past_the_byte_limit_is_refused_as_a_text_size_limit() {
    struct Row {
        case: &'static str,
        text: String,
    }

    let rows = [
        Row {
            case: "a short ASCII text",
            text: "a".repeat(100),
        },
        Row {
            case: "one-byte characters exactly at the limit",
            text: "a".repeat(MAX_TEXT_BYTES),
        },
        Row {
            case: "one-byte characters one past the limit",
            text: "a".repeat(MAX_TEXT_BYTES + 1),
        },
        Row {
            case: "one-byte characters well past the limit",
            text: "a".repeat(MAX_TEXT_BYTES + 137),
        },
        Row {
            case: "two-byte characters exactly at the byte limit",
            text: "\u{e9}".repeat(MAX_TEXT_BYTES / 2),
        },
        Row {
            case: "two-byte characters one character past the byte limit",
            text: "\u{e9}".repeat(MAX_TEXT_BYTES / 2 + 1),
        },
        Row {
            case: "three-byte characters inside the character bound but past the byte limit",
            text: "\u{20ac}".repeat(4_000),
        },
    ];

    let router = router();
    for row in &rows {
        let bytes = row.text.len();
        let refused = bytes > MAX_TEXT_BYTES;
        let (status, body) = call(
            &router,
            "TranslateText",
            &translate_body(&row.text, "en", "es"),
        )
        .await;

        if !refused {
            assert_eq!(
                status, 200,
                "case {:?}: a {bytes}-byte text is inside the limit, got {status} {body}",
                row.case
            );
            assert!(
                string_at(&body, "/TranslatedText").ends_with(&row.text),
                "case {:?}: the translation must carry the text that was sent, got {} characters",
                row.case,
                string_at(&body, "/TranslatedText").chars().count()
            );
            continue;
        }

        assert_eq!(
            status, 400,
            "case {:?}: a {bytes}-byte text is past the limit, got {status} {body}",
            row.case
        );
        assert_eq!(
            error_code(&body),
            "TextSizeLimitExceededException",
            "case {:?}: got {body}",
            row.case
        );
        assert_eq!(
            message(&body),
            format!(
                "Input text size exceeds limit. Max length of request text allowed is \
                 {MAX_TEXT_BYTES} bytes while in this request the text size is {bytes} bytes"
            ),
            "case {:?}: the refusal must name the limit and the size sent, got {body}",
            row.case
        );
    }
}

/// A pair the service does not translate is refused with a message naming that
/// pair, built from the codes the service RESOLVED — so a request that asked the
/// service to determine the source language is refused by the code it resolved,
/// not by the word it was asked with.
#[tokio::test]
async fn the_unsupported_pair_refusal_names_the_resolved_pair() {
    struct Row {
        case: &'static str,
        source: &'static str,
        target: &'static str,
        /// The pair the refusal must name, or `None` when the pair is served.
        refused_as: Option<(&'static str, &'static str)>,
    }

    let rows = [
        Row {
            case: "a target language the service does not translate",
            source: "en",
            target: "zz",
            refused_as: Some(("en", "zz")),
        },
        Row {
            case: "a source language the service does not translate",
            source: "zz",
            target: "fr",
            refused_as: Some(("zz", "fr")),
        },
        Row {
            case: "the same language on both sides",
            source: "it",
            target: "it",
            refused_as: Some(("it", "it")),
        },
        Row {
            case: "a regional variant into itself",
            source: "zh-TW",
            target: "zh-TW",
            refused_as: Some(("zh-TW", "zh-TW")),
        },
        Row {
            case: "a detected source language into a target the service does not translate",
            source: "auto",
            target: "zz",
            refused_as: Some(("en", "zz")),
        },
        Row {
            case: "a pair the service does translate",
            source: "de",
            target: "ja",
            refused_as: None,
        },
    ];

    let router = router();
    for row in &rows {
        let (status, body) = call(
            &router,
            "TranslateText",
            &translate_body(ENGLISH_TEXT, row.source, row.target),
        )
        .await;

        let Some((source, target)) = row.refused_as else {
            assert_eq!(
                status, 200,
                "case {:?}: expected the pair to be served, got {status} {body}",
                row.case
            );
            continue;
        };

        assert_eq!(
            status, 400,
            "case {:?}: expected the pair to be refused, got {status} {body}",
            row.case
        );
        assert_eq!(
            error_code(&body),
            "UnsupportedLanguagePairException",
            "case {:?}: got {body}",
            row.case
        );
        assert_eq!(
            message(&body),
            format!("Unsupported language pair: {source} to {target}."),
            "case {:?}: the refusal must name the pair it rejected, got {body}",
            row.case
        );
        // The same resolved pair travels in the error shape's own members.
        assert_eq!(
            string_at(&body, "/SourceLanguageCode"),
            source,
            "case {:?}: got {body}",
            row.case
        );
        assert_eq!(
            string_at(&body, "/TargetLanguageCode"),
            target,
            "case {:?}: got {body}",
            row.case
        );
    }
}

/// A terminology no import stored is reported by the resource the OPERATION
/// addresses — the terminology itself for a read or a translation, its metadata
/// record for a delete — together with the account and the name the request
/// asked for.
#[tokio::test]
async fn a_missing_terminology_refusal_names_the_resource_the_operation_addresses() {
    struct Row {
        case: &'static str,
        operation: &'static str,
        name: &'static str,
        resource: &'static str,
    }

    let rows = [
        Row {
            case: "a read of a name no import stored",
            operation: "GetTerminology",
            name: "ghost-terms",
            resource: "Terminology",
        },
        Row {
            case: "a delete of a name no import stored",
            operation: "DeleteTerminology",
            name: "other-ghost-terms",
            resource: "TerminologyMetadata",
        },
        Row {
            case: "a translation naming a terminology no import stored",
            operation: "TranslateText",
            name: "third-ghost-terms",
            resource: "Terminology",
        },
    ];

    let router = router();
    for row in &rows {
        let body = if row.operation == "TranslateText" {
            let mut body = translate_body("hello there", "en", "fr");
            body["TerminologyNames"] = json!([row.name]);
            body
        } else {
            json!({"Name": row.name})
        };
        let (status, answered) = call(&router, row.operation, &body).await;
        assert_eq!(
            status, 400,
            "case {:?}: got {status} {answered}",
            row.case
        );
        assert_eq!(
            error_code(&answered),
            "ResourceNotFoundException",
            "case {:?}: got {answered}",
            row.case
        );
        assert_eq!(
            message(&answered),
            format!("The {} for {ACCOUNT} {} was not found", row.resource, row.name),
            "case {:?}: got {answered}",
            row.case
        );
    }

    // And for a name that WAS stored and is now gone, the same two wordings:
    // the reader reports the terminology, the delete its metadata record.
    let (status, imported) = import(&router, "short-lived-terms", EN_FR_CSV, "CSV", Some("UNI")).await;
    assert_eq!(status, 200, "ImportTerminology answered {imported}");
    let (status, deleted) =
        call(&router, "DeleteTerminology", &json!({"Name": "short-lived-terms"})).await;
    assert_eq!(status, 200, "DeleteTerminology answered {deleted}");

    let (status, gone) =
        call(&router, "GetTerminology", &json!({"Name": "short-lived-terms"})).await;
    assert_eq!(status, 400, "GetTerminology answered {gone}");
    assert_eq!(
        message(&gone),
        format!("The Terminology for {ACCOUNT} short-lived-terms was not found"),
        "a read after the delete reports the terminology, got {gone}"
    );

    let (status, again) =
        call(&router, "DeleteTerminology", &json!({"Name": "short-lived-terms"})).await;
    assert_eq!(status, 400, "the second delete answered {again}");
    assert_eq!(
        message(&again),
        format!("The TerminologyMetadata for {ACCOUNT} short-lived-terms was not found"),
        "a second delete reports the metadata record, got {again}"
    );
}

/// An out-of-set enum value is refused with the accepted-value set listed in
/// the order this service's front door lists it — the SET is the model's, the
/// ORDER follows from the values themselves, so every member targeting one
/// enumerated type reads alike and only the rejected value varies with the
/// request.
///
/// The rows span the operations and the depths such a member is carried at: a
/// top-level member, a member of a nested structure, and two types declaring
/// the same three values in different orders — which are listed identically,
/// because the listing belongs to the value set and not to the declaration.
#[tokio::test]
async fn the_enum_refusal_lists_the_value_set_in_the_order_the_service_states_it() {
    struct Row {
        case: &'static str,
        operation: &'static str,
        body: Value,
        path: &'static str,
        value: &'static str,
        /// The accepted-value set as a clause lists it. Source for the values:
        /// `smithy_query(query="shape", shape="com.amazonaws.translate#<Enum>")`.
        value_set: &'static str,
    }

    let mut bad_format = parallel_data_body("enum-parallel-data");
    bad_format["ParallelDataConfig"]["Format"] = json!("XLIFF");
    let mut bad_directionality = import_terminology_body("enum-terms");
    bad_directionality["TerminologyData"]["Directionality"] = json!("BI");
    let mut bad_strategy = import_terminology_body("enum-strategy-terms");
    bad_strategy["MergeStrategy"] = json!("APPEND");

    let rows = [
        Row {
            case: "DisplayLanguageCode, a top-level member",
            operation: "ListLanguages",
            body: json!({"DisplayLanguageCode": "xx"}),
            path: "displayLanguageCode",
            value: "xx",
            value_set: "[de, zh-TW, pt, ko, en, it, fr, zh, es, ja]",
        },
        Row {
            case: "TerminologyDataFormat, a top-level member",
            operation: "GetTerminology",
            body: json!({"Name": "ghost-terms", "TerminologyDataFormat": "XLIFF"}),
            path: "terminologyDataFormat",
            value: "XLIFF",
            value_set: "[CSV, TSV, TMX]",
        },
        Row {
            case: "ParallelDataFormat, nested in a structure member",
            operation: "CreateParallelData",
            body: bad_format,
            path: "parallelDataConfig.format",
            value: "XLIFF",
            value_set: "[CSV, TSV, TMX]",
        },
        Row {
            case: "Directionality, nested in a structure member",
            operation: "ImportTerminology",
            body: bad_directionality,
            path: "terminologyData.directionality",
            value: "BI",
            value_set: "[UNI, MULTI]",
        },
        Row {
            case: "MergeStrategy, a one-value type",
            operation: "ImportTerminology",
            body: bad_strategy,
            path: "mergeStrategy",
            value: "APPEND",
            value_set: "[OVERWRITE]",
        },
        Row {
            case: "Formality, nested in a structure member",
            operation: "TranslateText",
            body: json!({
                "Text": "hello there",
                "SourceLanguageCode": "en",
                "TargetLanguageCode": "fr",
                "Settings": {"Formality": "CASUAL"},
            }),
            path: "settings.formality",
            value: "CASUAL",
            value_set: "[FORMAL, INFORMAL]",
        },
    ];

    // The order is not the model's declaration order, so the rows above cannot
    // be reading one list against itself.
    assert_eq!(
        DISPLAY_LANGUAGE_CODE_VALUES.join(", "),
        "de, en, es, fr, it, ja, ko, pt, zh, zh-TW",
        "the model declares this type's values in a different order from the one a clause \
         lists them in"
    );

    let router = router();
    for row in &rows {
        let (status, answered) = call(&router, row.operation, &row.body).await;
        assert_eq!(
            status, 400,
            "case {:?}: expected a constraint refusal, got {status} {answered}",
            row.case
        );
        assert_eq!(
            error_code(&answered),
            "ValidationException",
            "case {:?}: got {answered}",
            row.case
        );
        let expected = one_violation(&enum_clause(row.value, row.path, row.value_set));
        let reported = message(&answered);
        assert_eq!(
            reported, expected,
            "case {:?}: expected {expected:?}, got {reported:?}",
            row.case
        );
    }

    // A value the model declares is admitted past that validation, whatever the
    // order the set is listed in.
    for value in DISPLAY_LANGUAGE_CODE_VALUES {
        let (_, body) = call(
            &router,
            "ListLanguages",
            &json!({"DisplayLanguageCode": value}),
        )
        .await;
        assert!(
            !message(&body).contains("enum value set"),
            "case {value:?}: a declared value must not be refused by the value set, got {body}"
        );
    }
}

/// A value outside a member's `@pattern` is refused with the expression quoted
/// as this API publishes it: WITHOUT the `^`/`$` pair the model writes around it
/// to turn a find into a whole-value match. The rule holds for every
/// pattern-constrained member of the service, so the rows span four operations
/// and both depths such a member is carried at.
#[tokio::test]
async fn the_pattern_refusal_quotes_the_expression_without_whole_value_anchors() {
    struct Row {
        case: &'static str,
        operation: &'static str,
        body: Value,
        path: &'static str,
        value: &'static str,
        /// The expression a clause quotes: the member's `@pattern` without the
        /// whole-value pair. Source for each pattern:
        /// `smithy_query(query="shape", shape="com.amazonaws.translate#<Shape>")`.
        expression: &'static str,
    }

    let mut bad_token = parallel_data_body("pattern-parallel-data");
    bad_token["ClientToken"] = json!("client token 3");
    let mut bad_uri = parallel_data_body("pattern-uri-parallel-data");
    bad_uri["ParallelDataConfig"]["S3Uri"] = json!("https://example.com/corpus.tsv");
    let mut bad_key = import_terminology_body("pattern-terms");
    bad_key["EncryptionKey"] = json!({"Type": "KMS", "Id": "not a key id!"});
    let mut bad_role = translation_job_body("pattern-job");
    bad_role["DataAccessRoleArn"] = json!("arn:aws:iam::123456789012:user/not-a-role");
    let mut bad_content_type = translation_job_body("pattern-content-type-job");
    bad_content_type["InputDataConfig"]["ContentType"] = json!("plain text");

    let rows = [
        Row {
            case: "a terminology name, a top-level member",
            operation: "GetTerminology",
            body: json!({"Name": "ghost terms!"}),
            path: "name",
            value: "ghost terms!",
            expression: r"([A-Za-z0-9-]_?)+",
        },
        Row {
            case: "a pagination token, a top-level member",
            operation: "ListLanguages",
            body: json!({"NextToken": "caf\u{e9}"}),
            path: "nextToken",
            value: "caf\u{e9}",
            expression: r"\p{ASCII}{0,8192}",
        },
        Row {
            case: "an idempotency token, a top-level member",
            operation: "CreateParallelData",
            body: bad_token,
            path: "clientToken",
            value: "client token 3",
            expression: r"[a-zA-Z0-9-]+",
        },
        Row {
            case: "an object-storage URI, nested in a structure member",
            operation: "CreateParallelData",
            body: bad_uri,
            path: "parallelDataConfig.s3Uri",
            value: "https://example.com/corpus.tsv",
            expression: r"s3://[a-z0-9][\.\-a-z0-9]{1,61}[a-z0-9](/.*)?",
        },
        Row {
            case: "an encryption key id, nested in a structure member",
            operation: "ImportTerminology",
            body: bad_key,
            path: "encryptionKey.id",
            value: "not a key id!",
            expression: r"(arn:aws((-us-gov)|(-iso)|(-iso-b)|(-cn))?:kms:)?([a-z]{2}-[a-z]+(-[a-z]+)?-\d:)?(\d{12}:)?(((key/)?[a-zA-Z0-9-_]+)|(alias/[a-zA-Z0-9:/_-]+))",
        },
        Row {
            case: "a role ARN, a top-level member",
            operation: "StartTextTranslationJob",
            body: bad_role,
            path: "dataAccessRoleArn",
            value: "arn:aws:iam::123456789012:user/not-a-role",
            expression: r"arn:aws(-[^:]+)?:iam::[0-9]{12}:role/.+",
        },
        Row {
            case: "a content type, nested in a structure member",
            operation: "StartTextTranslationJob",
            body: bad_content_type,
            path: "inputDataConfig.contentType",
            value: "plain text",
            expression: r"[-\w.]+\/[-\w.+]+",
        },
    ];

    let router = router();
    for row in &rows {
        let (status, answered) = call(&router, row.operation, &row.body).await;
        assert_eq!(
            status, 400,
            "case {:?}: expected a constraint refusal, got {status} {answered}",
            row.case
        );
        assert_eq!(
            error_code(&answered),
            "ValidationException",
            "case {:?}: got {answered}",
            row.case
        );
        let expected = one_violation(&pattern_clause(row.value, row.path, row.expression));
        let reported = message(&answered);
        assert_eq!(
            reported, expected,
            "case {:?}: expected {expected:?}, got {reported:?}",
            row.case
        );
        // Stated plainly, because it is the whole point of the rendering: the
        // quoted expression carries neither end of the model's anchor pair.
        assert!(
            !reported.contains(&format!("pattern: ^{}$", row.expression)),
            "case {:?}: the quoted expression must carry no whole-value anchors, got \
             {reported:?}",
            row.case
        );
    }

    // Every one of those members ACCEPTS a value its expression admits: the
    // rendering above changed what a refusal says, not what is refused.
    for (case, operation, body) in [
        (
            "an admitted idempotency token",
            "CreateParallelData",
            parallel_data_body("pattern-ok-parallel-data"),
        ),
        (
            "an admitted encryption key id",
            "ImportTerminology",
            import_terminology_body("pattern-ok-terms"),
        ),
        (
            "an admitted role ARN",
            "StartTextTranslationJob",
            translation_job_body("pattern-ok-job"),
        ),
    ] {
        let (_, answered) = call(&router, operation, &body).await;
        assert!(
            !message(&answered).contains("regular expression pattern"),
            "case {case:?}: a value the expression admits must not be refused by it, got \
             {answered}"
        );
    }
}

/// The per-resource tag limit is a rule of the tagging operation, not of the
/// request's shape: a tag list the model admits is refused when the resource
/// would be left holding more tags than the limit, and the refusal applies none
/// of them.
#[tokio::test]
async fn the_tag_limit_is_enforced_over_the_resulting_tag_set() {
    fn tags(count: usize) -> Value {
        Value::Array(
            (0..count)
                .map(|index| json!({"Key": format!("k{index}"), "Value": "v"}))
                .collect(),
        )
    }

    async fn tags_of(router: &smithy_server_runtime::MultiServiceRouter, arn: &str) -> Vec<Value> {
        let (status, body) = call(router, "ListTagsForResource", &json!({"ResourceArn": arn})).await;
        assert_eq!(status, 200, "ListTagsForResource answered {body}");
        body.pointer("/Tags")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    }

    let router = router();

    // The rows differ in how many tags one call carries: none, one, exactly the
    // limit, one past it, and well past it — every one inside the `TagList`
    // bound the model declares, so the shape walk admits them all.
    for count in [0usize, 1, MAX_TAGS_PER_RESOURCE, MAX_TAGS_PER_RESOURCE + 1, 60] {
        let name = format!("tagged-{count}-terms");
        let (status, imported) = import(&router, &name, EN_FR_CSV, "CSV", Some("UNI")).await;
        assert_eq!(status, 200, "case {count}: ImportTerminology answered {imported}");
        let arn = expected_arn(&name, EN_FR_CSV);

        let (status, answered) = call(
            &router,
            "TagResource",
            &json!({"ResourceArn": arn, "Tags": tags(count)}),
        )
        .await;

        if count <= MAX_TAGS_PER_RESOURCE {
            assert_eq!(
                status, 200,
                "case {count}: a tag set inside the limit is accepted, got {status} {answered}"
            );
            assert_eq!(
                tags_of(&router, &arn).await.len(),
                count,
                "case {count}: the accepted tags are the ones the resource reports"
            );
            continue;
        }

        assert_eq!(
            status, 400,
            "case {count}: a tag set past the limit is refused, got {status} {answered}"
        );
        assert_eq!(
            error_code(&answered),
            "TooManyTagsException",
            "case {count}: got {answered}"
        );
        assert_eq!(
            message(&answered),
            "Allowable tag limit for resource exceeded",
            "case {count}: got {answered}"
        );
        assert_eq!(
            string_at(&answered, "/ResourceArn"),
            arn,
            "case {count}: the refusal names the resource it refused, got {answered}"
        );
        assert!(
            tags_of(&router, &arn).await.is_empty(),
            "case {count}: a refused call applies none of its tags"
        );
    }

    // The limit counts the tags the resource already holds together with the
    // ones the call adds, and a key the call restates replaces a value rather
    // than adding a tag.
    let name = "merged-terms";
    let (status, imported) = import(&router, name, EN_FR_CSV, "CSV", Some("UNI")).await;
    assert_eq!(status, 200, "ImportTerminology answered {imported}");
    let arn = expected_arn(name, EN_FR_CSV);

    let (status, answered) = call(
        &router,
        "TagResource",
        &json!({"ResourceArn": arn, "Tags": tags(MAX_TAGS_PER_RESOURCE)}),
    )
    .await;
    assert_eq!(status, 200, "TagResource answered {answered}");

    let (status, answered) = call(
        &router,
        "TagResource",
        &json!({"ResourceArn": arn, "Tags": [{"Key": "one-too-many", "Value": "v"}]}),
    )
    .await;
    assert_eq!(
        status, 400,
        "a resource at the limit takes no further key, got {status} {answered}"
    );
    assert_eq!(error_code(&answered), "TooManyTagsException", "got {answered}");
    assert_eq!(
        tags_of(&router, &arn).await.len(),
        MAX_TAGS_PER_RESOURCE,
        "the refused call leaves the stored tags as they were"
    );

    let (status, answered) = call(
        &router,
        "TagResource",
        &json!({"ResourceArn": arn, "Tags": [{"Key": "k0", "Value": "replaced"}]}),
    )
    .await;
    assert_eq!(
        status, 200,
        "re-stating a key the resource holds adds no tag, got {status} {answered}"
    );
    let held = tags_of(&router, &arn).await;
    assert_eq!(
        held.len(),
        MAX_TAGS_PER_RESOURCE,
        "a restated key replaces a value rather than adding a tag, got {held:?}"
    );
    assert!(
        held.contains(&json!({"Key": "k0", "Value": "replaced"})),
        "the restated key carries the new value, got {held:?}"
    );

    // An ARN this account holds no resource for is a not-found refusal from both
    // tagging paths, never an empty tag list.
    let unknown = expected_arn("no-such-terms", b"no-such-file");
    for operation in ["TagResource", "ListTagsForResource"] {
        let (status, answered) = call(
            &router,
            operation,
            &json!({"ResourceArn": unknown, "Tags": tags(1)}),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {operation:?}: got {status} {answered}"
        );
        assert_eq!(
            error_code(&answered),
            "ResourceNotFoundException",
            "case {operation:?}: got {answered}"
        );
    }
}
