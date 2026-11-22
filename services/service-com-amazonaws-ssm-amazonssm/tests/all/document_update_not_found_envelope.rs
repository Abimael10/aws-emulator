// SPDX-License-Identifier: CC0-1.0

//! The payload `UpdateDocument` refuses an unresolvable document name with.
//!
//! The rule under test: **`UpdateDocument`'s document-existence precondition answers a
//! `Name` that resolves to no document of this account and Region with 400
//! `InvalidDocument` carrying NO `Message` member at all — however the name came to
//! resolve to nothing and whatever the rest of the request says — while the document READS
//! of the same name keep stating the service's own not-found sentence, and the refused name
//! stays free to create.** `InvalidDocument$Message` is an optional member
//! (`smithy_query(query="shape",shape="InvalidDocument")`), so which refusal words it is
//! each operation's own; this one words none.
//!
//! The table's rows differ along what the rule generalises over: the FORM of the name
//! (a plain one, the shortest and the longest the modelled `DocumentName` pattern allows,
//! one carrying every punctuation character that pattern permits), HOW the name comes to
//! resolve to nothing (nothing ever created it; a create was deleted; a create that an
//! update had already appended versions to was deleted; it is held only at another Region's
//! endpoint), and the OTHER members the update carries (`DocumentVersion` omitted, `$LATEST`
//! or a number, `DocumentFormat`, `DisplayName`, `VersionName`, `TargetType`) — rather than
//! the single deleted name a recorded unit happened to update.
//!
//! Every case goes through the front door a client uses — the shared harness in
//! `all/parameter_store.rs` ([`router`], [`call`], [`call_in_region`], [`error_code`])
//! dispatches `POST /` with `X-Amz-Target: AmazonSSM.<Operation>` through the multi-service
//! router — and a document the store DOES hold takes an update in the same pass, so no row
//! passes against a handler that refuses everything.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::parameter_store::{call, call_in_region, error_code, router, REGION};

/// The bounds of the modelled `DocumentName` pattern, `^[a-zA-Z0-9_\-.]{3,128}$`
/// (`smithy_query(query="shape",shape="DocumentName")`): the shortest and longest name a
/// request may carry at all, so the refusal is exercised at both ends of the class.
const NAME_MIN: usize = 3;

/// See [`NAME_MIN`].
const NAME_MAX: usize = 128;

/// Another Region's endpoint, which holds none of the documents [`REGION`] does.
const OTHER_REGION: &str = "eu-central-1";

/// A minimal schemaVersion 2.2 `Command` document with one `aws:runShellScript` step,
/// parameterised by its description so two contents never hash alike.
fn command_content(description: &str) -> String {
    format!(
        r#"{{
  "schemaVersion": "2.2",
  "description": "{description}",
  "mainSteps": [
    {{
      "name": "theStep",
      "action": "aws:runShellScript",
      "inputs": {{ "runCommand": ["echo {description}"] }}
    }}
  ]
}}"#
    )
}

/// How a name comes to resolve to no document at the endpoint the update is signed for.
#[derive(Debug, Clone, Copy)]
enum Miss {
    /// Nothing ever created it.
    Absent,
    /// A create, `appended` updates on top of it, then a delete of the whole document.
    Deleted { appended: usize },
    /// Created at [`REGION`], updated at [`OTHER_REGION`]: a document is reached through
    /// the endpoint it was created against.
    OtherRegion,
}

/// One row of the table: a name, how it misses, and the members the update carries beside
/// `Name` and `Content`.
struct Row {
    case: &'static str,
    name: String,
    miss: Miss,
    extra: Value,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a plain name nothing created, the update naming no version",
            name: "NoSuchUpdatedDocument".to_string(),
            miss: Miss::Absent,
            extra: json!({}),
        },
        Row {
            case: "the shortest name the pattern allows, nothing created",
            name: "a".repeat(NAME_MIN),
            miss: Miss::Absent,
            extra: json!({ "DocumentVersion": "$LATEST" }),
        },
        Row {
            case: "the longest name the pattern allows, nothing created",
            name: format!("L{}", "o".repeat(NAME_MAX - 1)),
            miss: Miss::Absent,
            extra: json!({ "DocumentVersion": "1", "DocumentFormat": "JSON" }),
        },
        Row {
            case: "a name carrying every punctuation character the pattern allows",
            name: "a_b-c.d".to_string(),
            miss: Miss::Absent,
            extra: json!({ "DisplayName": "A Display Name", "VersionName": "1.0" }),
        },
        Row {
            case: "a document that was created and then deleted",
            name: "DeletedUpdatedDocument".to_string(),
            miss: Miss::Deleted { appended: 0 },
            extra: json!({ "DocumentVersion": "$LATEST" }),
        },
        Row {
            case: "a document that held appended versions before it was deleted",
            name: "DeletedVersionedUpdatedDocument".to_string(),
            miss: Miss::Deleted { appended: 2 },
            extra: json!({ "DocumentVersion": "$LATEST", "TargetType": "/AWS::EC2::Instance" }),
        },
        Row {
            case: "a document held only at another Region's endpoint",
            name: "RegionalUpdatedDocument".to_string(),
            miss: Miss::OtherRegion,
            extra: json!({ "DocumentFormat": "YAML" }),
        },
    ]
}

/// `CreateDocument` of `name` at `region`, asserting it succeeded.
async fn create_ok(router: &MultiServiceRouter, case: &str, region: &str, name: &str, body: &str) {
    let (status, response) = call_in_region(
        router,
        region,
        "CreateDocument",
        &json!({ "Name": name, "DocumentType": "Command", "Content": body }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case}: CreateDocument {name} at {region} must succeed, got {status} {response}"
    );
}

/// `UpdateDocument` of `name` at `region`, with `extra`'s members folded into the request.
async fn update(
    router: &MultiServiceRouter,
    region: &str,
    name: &str,
    content: &str,
    extra: &Value,
) -> (u16, Value) {
    let mut body = json!({ "Name": name, "Content": content });
    if let (Some(target), Some(members)) = (body.as_object_mut(), extra.as_object()) {
        for (member, value) in members {
            target.insert(member.clone(), value.clone());
        }
    }
    call_in_region(router, region, "UpdateDocument", &body).await
}

/// The sentence this service words a document READ's miss with: fixed text around the name
/// the request asked for, with a trailing full stop.
fn read_miss_message(name: &str) -> String {
    format!("Document with name {name} does not exist.")
}

/// The message a refusal carries, reading either spelling, and whether the member was
/// present at all.
fn message_of(body: &Value) -> (bool, String) {
    let member = body.get("Message").or_else(|| body.get("message"));
    (
        member.is_some(),
        member
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
    )
}

/// Every way a name resolves to no document: the update is refused 400 `InvalidDocument`
/// with no message, the read of the same name still words the miss, and the name is free.
#[tokio::test]
async fn update_document_refuses_an_unresolvable_name_with_no_message() {
    let router = router();

    // A document the store DOES hold, whose update is served — so a handler that refused
    // every name could not pass this suite.
    let held = "HeldUpdatedDocument";
    create_ok(&router, "the held document", REGION, held, &command_content("held")).await;
    let (status, response) = update(
        &router,
        REGION,
        held,
        &command_content("held, second version"),
        &json!({ "DocumentVersion": "$LATEST" }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case the held document: its update is served, got {status} {response}"
    );

    for row in rows() {
        let case = row.case;
        let name = row.name.as_str();
        // The endpoint the refused update is signed for.
        let endpoint = match row.miss {
            Miss::OtherRegion => OTHER_REGION,
            _ => REGION,
        };
        match row.miss {
            Miss::Absent => {}
            Miss::Deleted { appended } => {
                create_ok(&router, case, REGION, name, &command_content("first")).await;
                for version in 0..appended {
                    let content = command_content(&format!("appended {version}"));
                    let (status, response) =
                        update(&router, REGION, name, &content, &json!({})).await;
                    assert_eq!(
                        status, 200,
                        "case {case}: the appended version {version} must be persisted, \
                         got {status} {response}"
                    );
                }
                let (status, response) =
                    call(&router, "DeleteDocument", &json!({ "Name": name })).await;
                assert_eq!(
                    status, 200,
                    "case {case}: DeleteDocument {name} must succeed, got {status} {response}"
                );
            }
            Miss::OtherRegion => {
                create_ok(&router, case, REGION, name, &command_content("regional")).await;
            }
        }

        // The rule: 400 InvalidDocument with NO message. An absent member and an empty one
        // are the same empty message at a client, so either passes and any sentence fails.
        let content = command_content("the refused update");
        let (status, response) = update(&router, endpoint, name, &content, &row.extra).await;
        let (present, message) = message_of(&response);
        assert_eq!(
            (status, error_code(&response).as_str(), message.as_str()),
            (400, "InvalidDocument", ""),
            "case {case}: UpdateDocument {name} at {endpoint} answers 400 InvalidDocument \
             with no message, got {status} {response}"
        );
        assert!(
            !present || message.is_empty(),
            "case {case}: the refusal carries no Message member, got {response}"
        );

        // The contrast that makes the rule asymmetric by operation: the READ of the very
        // same name at the very same endpoint still words the miss.
        let (status, read) = call_in_region(&router, endpoint, "GetDocument", &json!({ "Name": name })).await;
        let (_, read_message) = message_of(&read);
        assert_eq!(
            (status, error_code(&read).as_str(), read_message.as_str()),
            (400, "InvalidDocument", read_miss_message(name).as_str()),
            "case {case}: GetDocument {name} at {endpoint} words the miss, got {status} {read}"
        );

        // The refusal left the name free: a create of it at that endpoint succeeds and the
        // document reads back.
        create_ok(&router, case, endpoint, name, &command_content("created again")).await;
        let (status, recreated) =
            call_in_region(&router, endpoint, "GetDocument", &json!({ "Name": name })).await;
        assert_eq!(
            (status, recreated["Name"].as_str()),
            (200, Some(name)),
            "case {case}: the freed name is created again and reads back, got {status} {recreated}"
        );
    }
}

/// The other operations of the document slice are unchanged: for one deleted name, each
/// READ still answers `InvalidDocument` worded with the name it was asked about, and only
/// the update answers it worded with nothing.
#[tokio::test]
async fn the_document_reads_still_word_the_same_miss() {
    let router = router();
    let name = "WordedMissDocument";
    create_ok(&router, "the document to delete", REGION, name, &command_content("one")).await;
    let (status, response) = call(&router, "DeleteDocument", &json!({ "Name": name })).await;
    assert_eq!(
        status, 200,
        "DeleteDocument {name} must succeed, got {status} {response}"
    );

    // The reads that resolve a document by name and report a miss as `InvalidDocument`
    // (`smithy_query(query="operation",operation="GetDocument,DescribeDocument,ListDocumentVersions")`).
    for operation in ["GetDocument", "DescribeDocument", "ListDocumentVersions"] {
        let (status, body) = call(&router, operation, &json!({ "Name": name })).await;
        let (_, message) = message_of(&body);
        assert_eq!(
            (status, error_code(&body).as_str(), message.as_str()),
            (400, "InvalidDocument", read_miss_message(name).as_str()),
            "case {operation}: the read words the miss, got {status} {body}"
        );
    }

    let (status, body) = update(
        &router,
        REGION,
        name,
        &command_content("two"),
        &json!({ "DocumentVersion": "$LATEST" }),
    )
    .await;
    let (_, message) = message_of(&body);
    assert_eq!(
        (status, error_code(&body).as_str(), message.as_str()),
        (400, "InvalidDocument", ""),
        "case UpdateDocument: the update words nothing, got {status} {body}"
    );
}

/// The rule is about the refusal a name resolving to NOTHING draws, not about every refusal
/// of this operation: a name Systems Manager publishes resolves to a document that is not
/// the account's to version, which is a different error and keeps its own sentence.
#[tokio::test]
async fn a_published_name_is_refused_as_before() {
    let router = router();
    for published in ["AWS-RunShellScript", "AWS-UpdateSSMAgent"] {
        let (status, body) = update(
            &router,
            REGION,
            published,
            &command_content("mine now"),
            &json!({ "DocumentVersion": "$LATEST" }),
        )
        .await;
        let (_, message) = message_of(&body);
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "InvalidDocumentOperation"),
            "case {published}: a published document is not the account's to version, \
             got {status} {body}"
        );
        assert!(
            message.contains(published),
            "case {published}: that refusal names the document it was aimed at, got {body}"
        );
    }
}
