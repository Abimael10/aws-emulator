// SPDX-License-Identifier: CC0-1.0

//! `UpdateConnectionFunction`, end to end over HTTP.
//!
//! Every assertion goes through the router, so each case exercises routing, restXml
//! (de)serialization, the handler, the persisted state and serialization — the same path a
//! client takes. The rules under test are:
//!
//! * an update REPLACES the code and the configuration of the stored connection function's
//!   `DEVELOPMENT` stage and mints a FRESH version token for it, leaving its identity (the
//!   `Id` the create minted, the ARN built from it and the name it was created under) and
//!   its `CreatedTime` alone — so a later `DescribeConnectionFunction` and
//!   `GetConnectionFunction` answer from the record the update wrote;
//! * "The current version (ETag value) of the connection function you are updating" is
//!   checked against the STORED version, so a header that is no version token at all is
//!   `InvalidIfMatchVersion` (HTTP 400), a well-formed token that is not the current one is
//!   `PreconditionFailed` (HTTP 412), and the token the update replaces no longer satisfies
//!   an `If-Match`;
//! * the operation addresses the resource by "The connection function ID", so every other
//!   identifier its URI label can carry — the name it was created under among them — names
//!   no connection function to update: `EntityNotFound` (HTTP 404);
//! * an update the service cannot hold — code past the published function-code quota, or a
//!   configuration describing no connection function it can hold — is refused with the
//!   error the model names for it, exactly as the create path refuses the same payload;
//! * every refusal leaves the stored connection function untouched: its code, its
//!   configuration and its version token all stand;
//! * an update is not a publish: it writes only the `DEVELOPMENT` stage, so a connection
//!   function that was never published still has no `LIVE` stage afterwards, and a published
//!   one keeps the code, the configuration and the version token the publish COPIED into
//!   `LIVE`.
//!
//! The table rows differ along everything these rules generalise over: the comment's length
//! and whether it is ASCII, the runtime (each direction between the two values the modelled
//! `FunctionRuntime` enum admits), the code's size (the modelled minimum, a real script and
//! the published quota bound) and whether it is text at all, whether the configuration gains
//! or drops a key value store association, and an update submitting exactly what is already
//! stored.

use std::collections::BTreeSet;

use service_com_amazonaws_cloudfront_cloudfront2020_05_31::state::{
    MAX_CONNECTION_FUNCTION_CODE_BYTES, MAX_KEY_VALUE_STORE_ASSOCIATIONS,
};
use smithy_server_runtime::MultiServiceRouter;

// The code read, from the suite that owns the `GetConnectionFunction` surface: one copy of
// that call path rather than a second here.
use crate::connection_function_code::get_connection_function;
use crate::connection_function_lifecycle::{
    create_connection_function, create_table, describe_connection_function, key_value_store_arn,
    member, Create,
};
// The publish call path, from the suite that owns the publish surface.
use crate::connection_function_publish::publish_connection_function;
use crate::distribution_lifecycle::{
    call, emulator, is_version_token, Answer, ACCOUNT_ID, API_VERSION,
};

/// The two values the modelled `FunctionStage` enum admits (`smithy_query(query="shape",
/// shape="FunctionStage")`), spelled out here so a wrong constant in the crate cannot make
/// these assertions pass.
const STAGE_DEVELOPMENT: &str = "DEVELOPMENT";
const STAGE_LIVE: &str = "LIVE";

/// The status a stage that has never been published reports, spelled out here for the same
/// reason the stages are.
const STATUS_UNPUBLISHED: &str = "UNPUBLISHED";

/// The two values the modelled `FunctionRuntime` enum admits
/// (`smithy_query(query="shape", shape="FunctionRuntime")`).
const RUNTIME_1_0: &str = "cloudfront-js-1.0";
const RUNTIME_2_0: &str = "cloudfront-js-2.0";

/// A connection function body — the shape of code this surface carries, including the
/// characters XML predefines, so the round trip is tested on code a client really sends.
const CONNECTION_CODE: &str = "function handler(event) {\n\
     \x20   var request = event.request;\n\
     \x20   request.headers['x-updated'] = { value: '2 > 1' };\n\
     \x20   return request;\n\
     }\n";

// ---------------------------------------------------------------------------
// Calling the update
// ---------------------------------------------------------------------------

/// The `UpdateConnectionFunctionRequest` document for `update`: an update carries the
/// configuration and the code in the body, the id as the URI label and the version as the
/// `If-Match` header — it carries neither a name nor a stage.
fn update_xml(update: &Create) -> String {
    update_xml_with_config(&update.config_xml(), &update.code)
}

/// The `UpdateConnectionFunctionRequest` document carrying `config_xml` and `code`, for the
/// cases whose configuration is written out rather than built from a [`Create`].
fn update_xml_with_config(config_xml: &str, code: &[u8]) -> String {
    format!(
        "<UpdateConnectionFunctionRequest \
         xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         {config_xml}<ConnectionFunctionCode>{}</ConnectionFunctionCode>\
         </UpdateConnectionFunctionRequest>",
        // The padded standard base64 a restXml blob MEMBER travels as, through the runtime's
        // own encoder.
        ::aws_smithy_types::base64::encode(code)
    )
}

/// `PUT /2020-05-31/connection-function/{Id}` with `update`'s configuration and code, under
/// the `If-Match` header `if_match`.
async fn update_connection_function(
    router: &MultiServiceRouter,
    id: &str,
    update: &Create,
    if_match: &str,
) -> Answer {
    update_with_body(router, id, &update_xml(update), if_match).await
}

/// `PUT /2020-05-31/connection-function/{Id}` with the request document `body`.
async fn update_with_body(
    router: &MultiServiceRouter,
    id: &str,
    body: &str,
    if_match: &str,
) -> Answer {
    call(
        router,
        "PUT",
        &format!("/{API_VERSION}/connection-function/{id}"),
        Some(body),
        Some(if_match),
    )
    .await
}

// ---------------------------------------------------------------------------
// The updates under test
// ---------------------------------------------------------------------------

/// One row: the connection function to create, and the update to apply to it. An update
/// carries only a configuration and code, so the `name` and the `tags` of [`Case::update`]
/// are never sent — they stay what the create gave them.
struct Case {
    case: &'static str,
    create: Create,
    update: Create,
}

/// A create with no associations and no tags, so each row states only what it varies.
fn creation(name: &str, comment: &str, runtime: &'static str, code: Vec<u8>) -> Create {
    Create {
        case: "the connection function under update",
        name: name.to_string(),
        comment: comment.to_string(),
        runtime,
        code,
        associations: Vec::new(),
        tags: Vec::new(),
    }
}

/// The same create carrying `associations`.
fn associating(create: Create, associations: Vec<String>) -> Create {
    Create {
        associations,
        ..create
    }
}

/// The updates every rule below is asserted over.
///
/// `FunctionBlob` is `length 1..40960` and `FunctionRuntime` admits exactly
/// `cloudfront-js-1.0` and `cloudfront-js-2.0` (both from `smithy_query` on
/// `UpdateConnectionFunction` and `FunctionRuntime`), and CloudFront's published quota for
/// the function code is [`MAX_CONNECTION_FUNCTION_CODE_BYTES`]. So the rows move the runtime
/// in each direction, carry the smallest code the model allows, a real script, code that is
/// not UTF-8 at all and code exactly at the published quota, write a comment that is empty,
/// long and non-ASCII, gain and drop a key value store association, and submit exactly what
/// is already stored.
///
/// `kvs_arn` is the ARN of a key value store this service holds, so the rows that associate
/// one name a store that really exists.
fn update_table(kvs_arn: &str) -> Vec<Case> {
    vec![
        Case {
            case: "the comment grows and the code becomes a real script",
            create: creation("grow_comment", "", RUNTIME_1_0, b"f".to_vec()),
            update: creation(
                "grow_comment",
                "rewrites a header on every connection",
                RUNTIME_1_0,
                CONNECTION_CODE.as_bytes().to_vec(),
            ),
        },
        Case {
            case: "the runtime moves to 2.0 and the code shrinks to the modelled minimum",
            create: creation(
                "runtime_forward",
                "a real script",
                RUNTIME_1_0,
                CONNECTION_CODE.as_bytes().to_vec(),
            ),
            update: creation("runtime_forward", "", RUNTIME_2_0, b"f".to_vec()),
        },
        Case {
            case: "the runtime moves back to 1.0, the comment is not ASCII and the code is \
                   not UTF-8",
            create: creation("runtime_back", "ascii", RUNTIME_2_0, b"f".to_vec()),
            update: creation(
                "runtime_back",
                "プロダクション — flags & limits",
                RUNTIME_1_0,
                vec![0x00, 0xff, 0xfe, 0x80, b'\n', 0x7f, 0x01],
            ),
        },
        Case {
            case: "the code grows to exactly the published quota and the comment is long",
            create: creation("quota_code", "short", RUNTIME_2_0, b"f".to_vec()),
            update: creation(
                "quota_code",
                &"c".repeat(128),
                RUNTIME_2_0,
                vec![b'x'; MAX_CONNECTION_FUNCTION_CODE_BYTES],
            ),
        },
        Case {
            case: "the configuration gains a key value store association",
            create: creation("gain_association", "no store yet", RUNTIME_2_0, b"f".to_vec()),
            update: associating(
                creation(
                    "gain_association",
                    "reads the catalog from a key value store",
                    RUNTIME_2_0,
                    CONNECTION_CODE.as_bytes().to_vec(),
                ),
                vec![kvs_arn.to_string()],
            ),
        },
        Case {
            case: "the configuration drops the key value store association it was created \
                   with",
            create: associating(
                creation(
                    "drop_association",
                    "reads the catalog from a key value store",
                    RUNTIME_2_0,
                    CONNECTION_CODE.as_bytes().to_vec(),
                ),
                vec![kvs_arn.to_string()],
            ),
            update: creation(
                "drop_association",
                "no longer reads the catalog",
                RUNTIME_2_0,
                CONNECTION_CODE.as_bytes().to_vec(),
            ),
        },
        Case {
            case: "an update submitting exactly what is already stored",
            create: creation(
                "unchanged_payload",
                "unchanged",
                RUNTIME_1_0,
                CONNECTION_CODE.as_bytes().to_vec(),
            ),
            update: creation(
                "unchanged_payload",
                "unchanged",
                RUNTIME_1_0,
                CONNECTION_CODE.as_bytes().to_vec(),
            ),
        },
    ]
}

// ---------------------------------------------------------------------------
// Reading what the service stored
// ---------------------------------------------------------------------------

/// What one create handed back: the identity it minted and the version token of the
/// `DEVELOPMENT` stage it stored.
struct Created {
    id: String,
    arn: String,
    etag: String,
    created_time: String,
}

/// Create the connection function `create` asks for and answer what the create minted.
async fn create_one(router: &MultiServiceRouter, case: &str, create: &Create) -> Created {
    let answer = create_connection_function(router, &create.xml()).await;
    assert_eq!(
        answer.status, 201,
        "case {case}: expected HTTP 201 from the create, got {} with {}",
        answer.status, answer.raw
    );
    let root = answer.root();
    Created {
        id: member(root, "Id", case, &answer.raw),
        arn: member(root, "ConnectionFunctionArn", case, &answer.raw),
        created_time: member(root, "CreatedTime", case, &answer.raw),
        etag: answer.header("etag").to_string(),
    }
}

/// The key value store ARNs the `ConnectionFunctionSummary` in `answer` reports, in order.
fn reported_associations(answer: &Answer) -> Vec<String> {
    answer
        .root()
        .at("ConnectionFunctionConfig/KeyValueStoreAssociations/Items")
        .map(|items| {
            items
                .children_named("KeyValueStoreAssociation")
                .iter()
                .map(|association| {
                    association
                        .text_at("KeyValueStoreARN")
                        .unwrap_or_default()
                        .to_string()
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Assert the `ConnectionFunctionSummary` document `answer` carries is the `DEVELOPMENT`
/// stage of the connection function `created` minted, carrying the configuration `update`
/// submitted.
fn assert_updated_summary(
    answer: &Answer,
    case: &str,
    created: &Created,
    update: &Create,
    context: &str,
) {
    let summary = answer.root();
    let raw = answer.raw.as_str();

    let name = member(summary, "Name", case, raw);
    assert_eq!(
        name, update.name,
        "case {case} ({context}): expected the name the create was given, got {name:?}"
    );

    let id = member(summary, "Id", case, raw);
    assert_eq!(
        id, created.id,
        "case {case} ({context}): expected the Id the create minted, got {id:?}"
    );

    let arn = member(summary, "ConnectionFunctionArn", case, raw);
    assert_eq!(
        arn, created.arn,
        "case {case} ({context}): expected the ARN the create minted, got {arn:?}"
    );

    let stage = member(summary, "Stage", case, raw);
    assert_eq!(
        stage, STAGE_DEVELOPMENT,
        "case {case} ({context}): expected Stage {STAGE_DEVELOPMENT}, got {stage:?}"
    );

    let status = member(summary, "Status", case, raw);
    assert_eq!(
        status, STATUS_UNPUBLISHED,
        "case {case} ({context}): expected Status {STATUS_UNPUBLISHED}, got {status:?}"
    );

    let comment = summary
        .text_at("ConnectionFunctionConfig/Comment")
        .unwrap_or("")
        .to_string();
    assert_eq!(
        comment, update.comment,
        "case {case} ({context}): expected the submitted Comment, got {comment:?}"
    );

    let runtime = member(summary, "ConnectionFunctionConfig/Runtime", case, raw);
    assert_eq!(
        runtime, update.runtime,
        "case {case} ({context}): expected the submitted Runtime, got {runtime:?}"
    );

    let associated = reported_associations(answer);
    assert_eq!(
        associated, update.associations,
        "case {case} ({context}): expected the submitted key value store associations, got \
         {associated:?}"
    );

    let created_time = member(summary, "CreatedTime", case, raw);
    assert_eq!(
        created_time, created.created_time,
        "case {case} ({context}): an update must leave CreatedTime alone, got {created_time:?}"
    );

    let modified = member(summary, "LastModifiedTime", case, raw);
    assert!(
        !modified.is_empty(),
        "case {case} ({context}): expected a LastModifiedTime, got {modified:?}"
    );
}

/// Assert the `DEVELOPMENT` stage of the connection function `id` names still reports the
/// version token `etag`, the comment `comment` and the code `code` — what a refused update
/// must leave behind.
async fn assert_development_unchanged(
    router: &MultiServiceRouter,
    case: &str,
    id: &str,
    etag: &str,
    comment: &str,
    code: &[u8],
) {
    let described = describe_connection_function(router, id, Some(STAGE_DEVELOPMENT)).await;
    assert_eq!(
        described.status, 200,
        "case {case}: the stored connection function must still be readable, got {} with {}",
        described.status, described.raw
    );
    assert_eq!(
        described.header("etag"),
        etag,
        "case {case}: a refused update must leave the version token alone, got {}",
        described.raw
    );
    let stored_comment = described
        .root()
        .text_at("ConnectionFunctionConfig/Comment")
        .unwrap_or("")
        .to_string();
    assert_eq!(
        stored_comment, comment,
        "case {case}: a refused update must leave the configuration alone, got \
         {stored_comment:?}"
    );
    let read = get_connection_function(router, id, Some(STAGE_DEVELOPMENT)).await;
    assert_eq!(
        read.body, code,
        "case {case}: a refused update must leave the stored code alone, got {} bytes",
        read.body.len()
    );
}

// ---------------------------------------------------------------------------
// The rules
// ---------------------------------------------------------------------------

/// An update REPLACES the code and the configuration of the `DEVELOPMENT` stage and mints a
/// fresh version token for it, leaving the identity the create minted and the stage's
/// `CreatedTime` alone — and every later read of that stage answers from the record the
/// update wrote. Across the table, no two updates hand back the same version token.
#[tokio::test]
async fn an_update_rewrites_the_development_stage_and_mints_a_fresh_version() {
    let router = emulator();
    let kvs_arn = key_value_store_arn(&router, "connection_function_update_store").await;
    let mut etags = BTreeSet::new();

    for row in update_table(&kvs_arn) {
        let case = row.case;
        let created = create_one(&router, case, &row.create).await;

        let answer = update_connection_function(&router, &created.id, &row.update, &created.etag)
            .await;
        assert_eq!(
            answer.status, 200,
            "case {case}: expected HTTP 200 from the update, got {} with {}",
            answer.status, answer.raw
        );
        assert_updated_summary(&answer, case, &created, &row.update, "the update");

        let etag = answer.header("etag").to_string();
        assert!(
            is_version_token(&etag),
            "case {case}: expected a version token in the ETag header, got {etag:?}"
        );
        assert_ne!(
            etag, created.etag,
            "case {case}: expected a fresh version token, got the one the create handed back"
        );
        assert!(
            etags.insert(etag.clone()),
            "case {case}: the version token {etag} repeated"
        );

        // The describe answers from the record the update wrote, not from any request — and
        // a request that names no stage addresses the stage the update wrote.
        let described = describe_connection_function(&router, &created.id, None).await;
        assert_eq!(
            described.status, 200,
            "case {case}: expected the updated stage to be readable, got {} with {}",
            described.status, described.raw
        );
        assert_updated_summary(&described, case, &created, &row.update, "the describe");
        assert_eq!(
            described.header("etag"),
            etag,
            "case {case}: expected the describe to report the version the update minted, got {}",
            described.raw
        );

        // The code read answers the bytes the update stored, byte for byte.
        let read = get_connection_function(&router, &created.id, Some(STAGE_DEVELOPMENT)).await;
        assert_eq!(
            read.status, 200,
            "case {case}: expected HTTP 200 reading the updated code, got {}",
            read.status
        );
        assert_eq!(
            read.body, row.update.code,
            "case {case}: expected the code the update stored, got {} bytes",
            read.body.len()
        );
        assert_eq!(
            read.etag, etag,
            "case {case}: expected the code read to report the version the update minted, got \
             {:?}",
            read.etag
        );

        // The version the update replaced no longer satisfies an `If-Match`.
        let stale = update_connection_function(&router, &created.id, &row.update, &created.etag)
            .await;
        assert_eq!(
            stale.status, 412,
            "case {case}: expected HTTP 412 for the version the update replaced, got {} with {}",
            stale.status, stale.raw
        );
    }
}

/// "The current version (ETag value) of the connection function you are updating" is checked
/// against the STORED version: a token that is no version token at all is
/// `InvalidIfMatchVersion` (HTTP 400), a well-formed token that is not the current one is
/// `PreconditionFailed` (HTTP 412), and a refused update leaves the stored stage exactly as
/// it was.
#[tokio::test]
async fn an_update_needs_the_current_version_of_the_stage_it_writes() {
    let router = emulator();
    let kvs_arn = key_value_store_arn(&router, "connection_function_precondition_store").await;
    let row = create_table(&kvs_arn)
        .into_iter()
        .next()
        .expect("the create table carries at least one row");

    // A version token this service really minted that belongs to another connection
    // function, so the stale case is a well-formed token rather than a made-up string.
    let foreign = create_one(
        &router,
        "the foreign version source",
        &Create {
            name: "foreign_version_source".to_string(),
            ..row.clone()
        },
    )
    .await;

    /// How a row renders the version it submits: from the current one, or verbatim.
    enum Token {
        Current(fn(&str) -> String),
        Literal(String),
    }

    let cases = [
        (
            "the current version token, bare",
            Token::Current(|current| current.to_string()),
            None,
        ),
        (
            "the current version token, quoted as an entity tag",
            Token::Current(|current| format!("\"{current}\"")),
            None,
        ),
        (
            "the current version token, quoted as a weak entity tag",
            Token::Current(|current| format!("W/\"{current}\"")),
            None,
        ),
        (
            "a value that is no version token at all",
            Token::Literal("not-a-version".to_string()),
            Some((400, "InvalidIfMatchVersion")),
        ),
        (
            "a token of the right shape in the wrong case",
            Token::Current(|current| current.to_lowercase()),
            Some((400, "InvalidIfMatchVersion")),
        ),
        (
            "a token whose body is too short",
            Token::Literal("E123".to_string()),
            Some((400, "InvalidIfMatchVersion")),
        ),
        (
            "the version token of another connection function",
            Token::Literal(foreign.etag.clone()),
            Some((412, "PreconditionFailed")),
        ),
    ];

    for (index, (case, token, refusal)) in cases.iter().enumerate() {
        // One connection function per row, so what a row writes cannot change what the next
        // one reads.
        let create = Create {
            name: format!("precondition_probe_{index}"),
            ..row.clone()
        };
        let created = create_one(&router, case, &create).await;
        let update = Create {
            comment: "the update the precondition guards".to_string(),
            code: CONNECTION_CODE.as_bytes().to_vec(),
            associations: Vec::new(),
            ..create.clone()
        };
        let sent = match token {
            Token::Current(render) => render(&created.etag),
            Token::Literal(literal) => literal.clone(),
        };

        let answer = update_connection_function(&router, &created.id, &update, &sent).await;

        match refusal {
            None => {
                assert_eq!(
                    answer.status, 200,
                    "case {case}: expected HTTP 200 updating with {sent:?}, got {} with {}",
                    answer.status, answer.raw
                );
                assert_updated_summary(&answer, case, &created, &update, "the accepted update");
            }
            Some((status, code)) => {
                assert_eq!(
                    answer.status, *status,
                    "case {case}: expected HTTP {status} updating with {sent:?}, got {} with {}",
                    answer.status, answer.raw
                );
                assert_eq!(
                    answer.error_code(),
                    *code,
                    "case {case}: expected {code}, got {}",
                    answer.raw
                );
                assert_development_unchanged(
                    &router,
                    case,
                    &created.id,
                    &created.etag,
                    &create.comment,
                    &create.code,
                )
                .await;
            }
        }
    }
}

/// The operation addresses the resource by "The connection function ID" the create minted,
/// so every other identifier the label can carry — the name it was created under among them
/// — names no connection function to update: HTTP 404 `EntityNotFound`, and nothing is
/// written. (The ARN the create minted is not among the rows because it carries path
/// separators, so it never reaches this operation at all.)
#[tokio::test]
async fn an_id_no_connection_function_answers_to_is_not_found() {
    let router = emulator();
    let kvs_arn = key_value_store_arn(&router, "connection_function_unknown_id_store").await;
    let create = Create {
        name: "present_connection_function".to_string(),
        ..create_table(&kvs_arn)
            .into_iter()
            .next()
            .expect("the create table carries at least one row")
    };
    let created = create_one(&router, "the connection function that is there", &create).await;
    let update = Create {
        comment: "the update nothing receives".to_string(),
        code: CONNECTION_CODE.as_bytes().to_vec(),
        associations: Vec::new(),
        ..create.clone()
    };

    let unknown = [
        ("the name the create was given", create.name.clone()),
        ("an id of the minted shape nothing holds", {
            let mut other = created.id.clone();
            // Flip the last character of a real id, so the value has the shape this service
            // mints and still names nothing.
            other.pop();
            format!("{other}0")
        }),
        ("a value of no particular shape", "missing".to_string()),
    ];

    for (case, identifier) in unknown {
        let answer = update_connection_function(&router, &identifier, &update, &created.etag).await;
        assert_eq!(
            answer.status, 404,
            "case {case}: expected HTTP 404 updating {identifier:?}, got {} with {}",
            answer.status, answer.raw
        );
        assert_eq!(
            answer.error_code(),
            "EntityNotFound",
            "case {case}: expected EntityNotFound, got {}",
            answer.raw
        );
    }

    // Nothing was written: the connection function that is there still carries what its
    // create stored.
    assert_development_unchanged(
        &router,
        "the connection function that is there",
        &created.id,
        &created.etag,
        &create.comment,
        &create.code,
    )
    .await;
}

/// Code past the published function-code quota is refused with the modelled
/// `EntitySizeLimitExceeded` (HTTP 413), and the stored stage is left untouched — the same
/// quota the create path applies, which is reachable because the modelled `FunctionBlob`
/// bound (40960) is wider than it.
#[tokio::test]
async fn code_past_the_published_quota_is_refused() {
    let router = emulator();
    let kvs_arn = key_value_store_arn(&router, "connection_function_size_store").await;
    let create = Create {
        name: "size_limit_probe".to_string(),
        ..create_table(&kvs_arn)
            .into_iter()
            .next()
            .expect("the create table carries at least one row")
    };
    let created = create_one(&router, "the connection function under update", &create).await;

    // The quota is a byte count, so the rows that matter are the last update within it and
    // the first one past it.
    let within = Create {
        comment: "exactly at the quota".to_string(),
        code: vec![b'x'; MAX_CONNECTION_FUNCTION_CODE_BYTES],
        associations: Vec::new(),
        ..create.clone()
    };
    let accepted = update_connection_function(&router, &created.id, &within, &created.etag).await;
    assert_eq!(
        accepted.status, 200,
        "case code exactly at the published quota: expected HTTP 200, got {} with {}",
        accepted.status, accepted.raw
    );
    let current = accepted.header("etag").to_string();

    let over = Create {
        comment: "one byte past the quota".to_string(),
        code: vec![b'x'; MAX_CONNECTION_FUNCTION_CODE_BYTES + 1],
        associations: Vec::new(),
        ..create.clone()
    };
    let refused = update_connection_function(&router, &created.id, &over, &current).await;
    assert_eq!(
        refused.status, 413,
        "case one byte past the published quota: expected HTTP 413, got {} with {}",
        refused.status, refused.raw
    );
    assert_eq!(
        refused.error_code(),
        "EntitySizeLimitExceeded",
        "case one byte past the published quota: expected EntitySizeLimitExceeded, got {}",
        refused.raw
    );
    assert_development_unchanged(
        &router,
        "one byte past the published quota",
        &created.id,
        &current,
        &within.comment,
        &within.code,
    )
    .await;
}

/// A configuration this service cannot hold is refused with the modelled `InvalidArgument`
/// (HTTP 400) and writes nothing: a `KeyValueStoreAssociations` wrapper whose `Quantity`
/// disagrees with its `Items`, more associations than one function may carry, and an
/// association naming a key value store this service does not hold.
#[tokio::test]
async fn a_configuration_the_service_cannot_hold_is_refused() {
    let router = emulator();
    let held = key_value_store_arn(&router, "connection_update_config_store").await;
    // One more store than a function may be associated with, so the row past the cap names
    // stores that all really exist and is refused for its COUNT alone.
    let mut over_the_cap = Vec::new();
    for index in 0..=MAX_KEY_VALUE_STORE_ASSOCIATIONS {
        over_the_cap.push(key_value_store_arn(&router, &format!("update_cap_{index}")).await);
    }
    // A well-formed key value store ARN that no store in this service holds.
    let absent = format!(
        "arn:aws:cloudfront::{ACCOUNT_ID}:key-value-store/\
         00000000-0000-4000-8000-000000000000"
    );

    let create = creation(
        "refused_config_probe",
        "the stored configuration",
        RUNTIME_2_0,
        CONNECTION_CODE.as_bytes().to_vec(),
    );
    let created = create_one(&router, "the connection function under update", &create).await;

    let overstated = format!(
        "<ConnectionFunctionConfig><Comment>overstated</Comment>\
         <Runtime>{RUNTIME_2_0}</Runtime>\
         <KeyValueStoreAssociations><Quantity>2</Quantity><Items>\
         <KeyValueStoreAssociation><KeyValueStoreARN>{held}</KeyValueStoreARN>\
         </KeyValueStoreAssociation></Items></KeyValueStoreAssociations>\
         </ConnectionFunctionConfig>"
    );
    let understated = format!(
        "<ConnectionFunctionConfig><Comment>understated</Comment>\
         <Runtime>{RUNTIME_2_0}</Runtime>\
         <KeyValueStoreAssociations><Quantity>0</Quantity><Items>\
         <KeyValueStoreAssociation><KeyValueStoreARN>{held}</KeyValueStoreARN>\
         </KeyValueStoreAssociation></Items></KeyValueStoreAssociations>\
         </ConnectionFunctionConfig>"
    );
    let cases: [(&str, String); 4] = [
        ("a Quantity above the Items length", overstated),
        ("a Quantity below the Items length", understated),
        (
            "one association past the published per-function cap",
            associating(create.clone(), over_the_cap.clone()).config_xml(),
        ),
        (
            "an association naming a store that does not exist",
            associating(create.clone(), vec![absent.clone()]).config_xml(),
        ),
    ];

    for (case, config) in cases {
        let body = update_xml_with_config(&config, CONNECTION_CODE.as_bytes());
        let answer = update_with_body(&router, &created.id, &body, &created.etag).await;
        assert_eq!(
            answer.status, 400,
            "case {case}: expected HTTP 400, got {} with {}",
            answer.status, answer.raw
        );
        assert_eq!(
            answer.error_code(),
            "InvalidArgument",
            "case {case}: expected InvalidArgument, got {}",
            answer.raw
        );
        assert_development_unchanged(
            &router,
            case,
            &created.id,
            &created.etag,
            &create.comment,
            &create.code,
        )
        .await;
    }
}

/// An update is not a publish: it writes the `DEVELOPMENT` stage only. A connection function
/// that was never published still has no `LIVE` stage after an update, and a published one
/// keeps the code, the configuration and the version token the publish COPIED into `LIVE`.
#[tokio::test]
async fn an_update_writes_the_development_stage_only() {
    let router = emulator();
    let create = creation(
        "update_is_not_a_publish",
        "the created configuration",
        RUNTIME_1_0,
        b"f".to_vec(),
    );
    let created = create_one(&router, "before any publish", &create).await;

    let first = Create {
        comment: "the first update".to_string(),
        code: CONNECTION_CODE.as_bytes().to_vec(),
        ..create.clone()
    };
    let updated = update_connection_function(&router, &created.id, &first, &created.etag).await;
    assert_eq!(
        updated.status, 200,
        "case before any publish: expected HTTP 200 from the update, got {} with {}",
        updated.status, updated.raw
    );
    let development_etag = updated.header("etag").to_string();

    let live = describe_connection_function(&router, &created.id, Some(STAGE_LIVE)).await;
    assert_eq!(
        live.status, 404,
        "case before any publish: an update must publish nothing, got {} with {}",
        live.status, live.raw
    );

    // Publish what the update wrote, then update the DEVELOPMENT stage again: the LIVE stage
    // holds what the publish copied, so it keeps its own code, configuration and version.
    let published = publish_connection_function(&router, &created.id, &development_etag).await;
    assert_eq!(
        published.status, 200,
        "case after a publish: expected HTTP 200 from the publish, got {} with {}",
        published.status, published.raw
    );
    let live_before = describe_connection_function(&router, &created.id, Some(STAGE_LIVE)).await;
    assert_eq!(
        live_before.status, 200,
        "case after a publish: expected the published stage to be readable, got {} with {}",
        live_before.status, live_before.raw
    );
    let live_etag = live_before.header("etag").to_string();
    assert!(
        is_version_token(&live_etag),
        "case after a publish: expected a version token for the published stage, got {live_etag:?}"
    );

    let second = Create {
        comment: "the second update".to_string(),
        code: b"g".to_vec(),
        ..create.clone()
    };
    let again = update_connection_function(&router, &created.id, &second, &development_etag).await;
    assert_eq!(
        again.status, 200,
        "case after a publish: expected HTTP 200 from the second update, got {} with {}",
        again.status, again.raw
    );

    let live = describe_connection_function(&router, &created.id, Some(STAGE_LIVE)).await;
    assert_eq!(
        live.status, 200,
        "case after a publish: expected the LIVE stage to be readable, got {} with {}",
        live.status, live.raw
    );
    let live_comment = live
        .root()
        .text_at("ConnectionFunctionConfig/Comment")
        .unwrap_or("")
        .to_string();
    assert_eq!(
        live_comment, first.comment,
        "case after a publish: expected the LIVE stage to keep the published configuration, got \
         {live_comment:?}"
    );
    assert_eq!(
        live.header("etag"),
        live_etag,
        "case after a publish: expected the LIVE stage to keep its own version token, got {}",
        live.raw
    );
    let live_code = get_connection_function(&router, &created.id, Some(STAGE_LIVE)).await;
    assert_eq!(
        live_code.body, first.code,
        "case after a publish: expected the LIVE stage to keep the published code, got {} bytes",
        live_code.body.len()
    );

    // The DEVELOPMENT stage carries what the second update wrote.
    let development = describe_connection_function(&router, &created.id, None).await;
    let development_comment = development
        .root()
        .text_at("ConnectionFunctionConfig/Comment")
        .unwrap_or("")
        .to_string();
    assert_eq!(
        development_comment, second.comment,
        "case after a publish: expected the DEVELOPMENT stage to carry the second update, got \
         {development_comment:?}"
    );
    let development_code =
        get_connection_function(&router, &created.id, Some(STAGE_DEVELOPMENT)).await;
    assert_eq!(
        development_code.body, second.code,
        "case after a publish: expected the DEVELOPMENT stage to carry the second update's code, \
         got {} bytes",
        development_code.body.len()
    );
}
