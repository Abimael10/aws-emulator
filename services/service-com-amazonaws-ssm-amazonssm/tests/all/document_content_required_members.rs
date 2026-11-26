// SPDX-License-Identifier: CC0-1.0

//! The top-level members a document BODY has to declare, decided by the schema version the
//! body itself declares.
//!
//! RULE under test: Systems Manager validates the content a `CreateDocument` or an
//! `UpdateDocument` carries against the schema that content declares, so a document whose
//! body states what the service RUNS has to carry the member that generation of the schema
//! names for its steps — `mainSteps` in the 2.x and 0.3 schemas, `runtimeConfig` in the 1.2
//! one — and a body carrying none is refused HTTP 400 `InvalidDocumentContent` with the
//! message `Missing "<member>" in the document.`, registering nothing; a document whose body
//! declares no steps at all (a session document, a Distributor package, a template) is
//! accepted without one, and so is a body whose type carries no Systems Manager document
//! schema.
//!
//! `InvalidDocumentContent` is the modelled client error for this
//! (`smithy_query(query="operation", operation="CreateDocument,UpdateDocument",
//! model_path="services/service-com-amazonaws-ssm-amazonssm/smithy-model/\
//! com-amazonaws-ssm-amazonssm.json")`: errorKind client, httpError 400), and the schema
//! generations and their steps members come from the document schemas of the AWS Systems
//! Manager User Guide ("Systems Manager document schemas and features", with each schema's
//! top-level elements under "Creating SSM document content").
//!
//! The tables' rows differ along what the rule generalises over: the document TYPE, the
//! schema version its body declares, the `DocumentFormat` the body is written in, which
//! member is missing, and whether the body carries the OTHER generation's steps member
//! instead of its own — rather than the single schema 2.2 command body a recorded unit
//! happened to send.
//!
//! Every case goes through the front door a client uses — the shared harness in
//! `all/parameter_store.rs` ([`router`], [`call`], [`error_code`], [`error_message`])
//! dispatches `POST /` with `X-Amz-Target: AmazonSSM.<Operation>` through the multi-service
//! router — and each refusal is followed by the read that shows nothing was registered.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::parameter_store::{call, error_code, error_message, router};

/// The steps one accepted `Command` body declares, as the 2.x schema spells them.
const COMMAND_STEPS: &str = r#"[{"action": "aws:runShellScript", "name": "runShell",
    "inputs": {"runCommand": ["echo hi"]}}]"#;

/// The plugin one accepted schema 1.2 `Command` body declares, as that schema spells what
/// the document runs.
const RUNTIME_CONFIG: &str = r#"{"aws:runShellScript": {"properties": [{"id":
    "0.aws:runShellScript", "runCommand": ["echo hi"]}]}}"#;

/// The step one accepted schema 0.3 body declares.
const AUTOMATION_STEPS: &str = r#"[{"name": "wait", "action": "aws:sleep",
    "inputs": {"Duration": "PT0S"}}]"#;

/// How this service words a document read's miss: fixed text around the name the request
/// asked for.
fn read_miss_message(name: &str) -> String {
    format!("Document with name {name} does not exist.")
}

/// The sentence a body missing a mandatory top-level member is refused with: fixed text
/// naming that member, with nothing of the request in it.
fn missing_member_message(member: &str) -> String {
    format!("Missing \"{member}\" in the document.")
}

/// `CreateDocument` of `name` with `content`, as the type and format the row states.
async fn create(
    router: &MultiServiceRouter,
    name: &str,
    document_type: &str,
    format: &str,
    content: &str,
) -> (u16, Value) {
    call(
        router,
        "CreateDocument",
        &json!({
            "Name": name,
            "DocumentType": document_type,
            "DocumentFormat": format,
            "Content": content,
        }),
    )
    .await
}

/// One row of the refusal table: a body that declares no member for its steps, and the
/// member its own schema version requires.
struct Missing {
    case: &'static str,
    name: &'static str,
    document_type: &'static str,
    format: &'static str,
    content: String,
    member: &'static str,
}

fn missing_rows() -> Vec<Missing> {
    vec![
        Missing {
            case: "a schema 2.2 Command body carrying only a description",
            name: "Missing22CommandDocument",
            document_type: "Command",
            format: "JSON",
            content: r#"{"schemaVersion": "2.2", "description": "no mainSteps here"}"#
                .to_string(),
            member: "mainSteps",
        },
        Missing {
            case: "a schema 2.0 Command body, the other 2.x generation",
            name: "Missing20CommandDocument",
            document_type: "Command",
            format: "JSON",
            content: r#"{"schemaVersion": "2.0", "description": "nothing to run"}"#.to_string(),
            member: "mainSteps",
        },
        Missing {
            case: "a schema 2.2 Command body written in YAML",
            name: "Missing22YamlCommandDocument",
            document_type: "Command",
            format: "YAML",
            content: "schemaVersion: '2.2'\ndescription: no steps at all\n".to_string(),
            member: "mainSteps",
        },
        Missing {
            case: "a schema 2.2 Command body declaring only parameters",
            name: "Missing22ParametersDocument",
            document_type: "Command",
            format: "JSON",
            content: r#"{"schemaVersion": "2.2",
                "parameters": {"Message": {"type": "String", "default": "hi"}}}"#
                .to_string(),
            member: "mainSteps",
        },
        Missing {
            case: "a schema 2.2 Command body carrying the 1.2 generation's steps member",
            name: "Missing22RuntimeConfigDocument",
            document_type: "Command",
            format: "JSON",
            content: format!(r#"{{"schemaVersion": "2.2", "runtimeConfig": {RUNTIME_CONFIG}}}"#),
            member: "mainSteps",
        },
        Missing {
            case: "a schema 1.2 Command body carrying no plugins",
            name: "Missing12CommandDocument",
            document_type: "Command",
            format: "JSON",
            content: r#"{"schemaVersion": "1.2", "description": "no runtimeConfig here"}"#
                .to_string(),
            member: "runtimeConfig",
        },
        Missing {
            case: "a schema 1.2 Command body carrying the 2.x generation's steps member",
            name: "Missing12MainStepsDocument",
            document_type: "Command",
            format: "JSON",
            content: format!(r#"{{"schemaVersion": "1.2", "mainSteps": {COMMAND_STEPS}}}"#),
            member: "runtimeConfig",
        },
        Missing {
            case: "a schema 0.3 Automation body carrying only a description",
            name: "Missing03AutomationDocument",
            document_type: "Automation",
            format: "JSON",
            content: r#"{"schemaVersion": "0.3", "description": "nothing to run"}"#.to_string(),
            member: "mainSteps",
        },
        Missing {
            case: "a schema 0.3 Automation body written in YAML",
            name: "Missing03YamlAutomationDocument",
            document_type: "Automation",
            format: "YAML",
            content: "schemaVersion: '0.3'\nassumeRole: '{{ AutomationAssumeRole }}'\n"
                .to_string(),
            member: "mainSteps",
        },
        Missing {
            case: "a schema 0.3 change template body",
            name: "Missing03ChangeTemplateDocument",
            document_type: "Automation.ChangeTemplate",
            format: "JSON",
            content: r#"{"schemaVersion": "0.3", "description": "a template with no steps"}"#
                .to_string(),
            member: "mainSteps",
        },
        Missing {
            case: "a schema 2.0 inventory policy body",
            name: "Missing20PolicyDocument",
            document_type: "Policy",
            format: "JSON",
            content: r#"{"schemaVersion": "2.0", "description": "collect nothing"}"#.to_string(),
            member: "mainSteps",
        },
    ]
}

/// A body that declares none of the steps member its schema version requires is refused 400
/// `InvalidDocumentContent` naming that member, and no document is registered for it.
#[tokio::test]
async fn a_body_missing_the_member_its_schema_requires_is_refused() {
    let router = router();
    for row in missing_rows() {
        let case = row.case;
        let (status, response) =
            create(&router, row.name, row.document_type, row.format, &row.content).await;
        assert_eq!(
            (status, error_code(&response).as_str()),
            (400, "InvalidDocumentContent"),
            "case {case:?}: expected 400 InvalidDocumentContent, got {status} {response}"
        );
        assert_eq!(
            error_message(&response),
            missing_member_message(row.member),
            "case {case:?}: expected the sentence naming {:?}, got {response}",
            row.member
        );

        // The refusal registered nothing: the name resolves to no document at all, which is
        // what a client reads after a deploy that failed on this body.
        for read in ["GetDocument", "DescribeDocument"] {
            let (status, response) = call(&router, read, &json!({ "Name": row.name })).await;
            assert_eq!(
                (
                    status,
                    error_code(&response).as_str(),
                    error_message(&response)
                ),
                (400, "InvalidDocument", read_miss_message(row.name)),
                "case {case:?}: {read} must report no such document, got {status} {response}"
            );
        }
    }
}

/// One row of the acceptance table: a body that carries everything its schema version
/// requires, or whose type requires nothing of it.
struct Accepted {
    case: &'static str,
    name: &'static str,
    document_type: &'static str,
    format: &'static str,
    content: String,
}

fn accepted_rows() -> Vec<Accepted> {
    vec![
        Accepted {
            case: "a schema 2.2 Command body declaring one step",
            name: "Accepted22CommandDocument",
            document_type: "Command",
            format: "JSON",
            content: format!(
                r#"{{"schemaVersion": "2.2", "description": "echo", "mainSteps": {COMMAND_STEPS}}}"#
            ),
        },
        Accepted {
            case: "a schema 2.2 Command body whose mainSteps declare no step",
            name: "Accepted22EmptyStepsDocument",
            document_type: "Command",
            format: "JSON",
            content: r#"{"schemaVersion": "2.2", "mainSteps": []}"#.to_string(),
        },
        Accepted {
            case: "a schema 2.2 Command body written in YAML",
            name: "Accepted22YamlCommandDocument",
            document_type: "Command",
            format: "YAML",
            content: "schemaVersion: '2.2'\nmainSteps:\n  - name: runShell\n    action: \
                      'aws:runShellScript'\n    inputs:\n      runCommand:\n        - echo hi\n"
                .to_string(),
        },
        Accepted {
            case: "a schema 1.2 Command body declaring its plugins",
            name: "Accepted12CommandDocument",
            document_type: "Command",
            format: "JSON",
            content: format!(r#"{{"schemaVersion": "1.2", "runtimeConfig": {RUNTIME_CONFIG}}}"#),
        },
        Accepted {
            case: "a schema 0.3 Automation body declaring one step",
            name: "Accepted03AutomationDocument",
            document_type: "Automation",
            format: "JSON",
            content: format!(
                r#"{{"schemaVersion": "0.3", "mainSteps": {AUTOMATION_STEPS}}}"#
            ),
        },
        Accepted {
            case: "a schema 0.3 change template declaring one step",
            name: "Accepted03ChangeTemplateDocument",
            document_type: "Automation.ChangeTemplate",
            format: "JSON",
            content: format!(
                r#"{{"schemaVersion": "0.3", "mainSteps": {AUTOMATION_STEPS}}}"#
            ),
        },
        Accepted {
            case: "a schema 2.0 inventory policy declaring one step",
            name: "Accepted20PolicyDocument",
            document_type: "Policy",
            format: "JSON",
            content: r#"{"schemaVersion": "2.0", "mainSteps": [{"action":
                "aws:softwareInventory", "name": "collect", "inputs": {"applications":
                "Enabled"}}]}"#
                .to_string(),
        },
        Accepted {
            case: "a session document, whose body declares a session rather than steps",
            name: "AcceptedSessionDocument",
            document_type: "Session",
            format: "JSON",
            content: r#"{"schemaVersion": "1.0", "sessionType": "Standard_Stream",
                "inputs": {"idleSessionTimeout": "20"}}"#
                .to_string(),
        },
        Accepted {
            case: "a Distributor package, whose body declares packages rather than steps",
            name: "AcceptedPackageDocument",
            document_type: "Package",
            format: "JSON",
            content: r#"{"schemaVersion": "2.0", "publisher": "Me", "version": "1.0"}"#
                .to_string(),
        },
        Accepted {
            case: "a type carrying no Systems Manager document schema at all",
            name: "AcceptedTemplateDocument",
            document_type: "CloudFormation",
            format: "JSON",
            content: r#"{"schemaVersion": "2.2", "description": "a template body"}"#.to_string(),
        },
    ]
}

/// A body that carries the member its schema version requires — and one whose type requires
/// none — is accepted and reads back as the document it registered.
#[tokio::test]
async fn a_body_that_carries_what_its_schema_requires_is_accepted() {
    let router = router();
    for row in accepted_rows() {
        let case = row.case;
        let (status, created) =
            create(&router, row.name, row.document_type, row.format, &row.content).await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateDocument must succeed, got {status} {created}"
        );

        // The round trip: the read resolves the document the create registered, with the
        // content and the type the create supplied, asked for in the format it was written
        // in so the bytes are the stored ones rather than a conversion of them.
        let (status, read) = call(
            &router,
            "GetDocument",
            &json!({ "Name": row.name, "DocumentFormat": row.format }),
        )
        .await;
        assert_eq!(
            (status, read["Name"].as_str(), read["Content"].as_str()),
            (200, Some(row.name), Some(row.content.as_str())),
            "case {case:?}: GetDocument must report the registered document, got {status} {read}"
        );
        let (status, described) =
            call(&router, "DescribeDocument", &json!({ "Name": row.name })).await;
        assert_eq!(
            (
                status,
                described["Document"]["DocumentType"].as_str(),
                described["Document"]["Status"].as_str()
            ),
            (200, Some(row.document_type), Some("Active")),
            "case {case:?}: the document is registered and ready, got {status} {described}"
        );
    }
}

/// The same rule holds for the call that adds a VERSION to a document: a new body missing
/// the member its schema requires is refused and appends nothing, and the document keeps
/// the version it had.
#[tokio::test]
async fn an_update_whose_body_drops_the_steps_member_is_refused() {
    let router = router();
    let name = "UpdatedStepsDocument";
    let first = format!(
        r#"{{"schemaVersion": "2.2", "description": "first", "mainSteps": {COMMAND_STEPS}}}"#
    );
    let (status, created) = create(&router, name, "Command", "JSON", &first).await;
    assert_eq!(
        status, 200,
        "the document to update must be created, got {status} {created}"
    );

    // (case, the body the update carries, the member it is missing)
    let rows: Vec<(&str, String, &str)> = vec![
        (
            "a second version carrying only a description",
            r#"{"schemaVersion": "2.2", "description": "second"}"#.to_string(),
            "mainSteps",
        ),
        (
            "a second version carrying the 1.2 generation's steps member",
            format!(r#"{{"schemaVersion": "2.2", "runtimeConfig": {RUNTIME_CONFIG}}}"#),
            "mainSteps",
        ),
    ];
    for (case, content, member) in rows {
        let (status, response) = call(
            &router,
            "UpdateDocument",
            &json!({ "Name": name, "Content": content, "DocumentVersion": "$LATEST" }),
        )
        .await;
        assert_eq!(
            (status, error_code(&response).as_str()),
            (400, "InvalidDocumentContent"),
            "case {case:?}: expected 400 InvalidDocumentContent, got {status} {response}"
        );
        assert_eq!(
            error_message(&response),
            missing_member_message(member),
            "case {case:?}: expected the sentence naming {member:?}, got {response}"
        );

        // Nothing was appended: the document still holds the one version the create made,
        // and the content it resolves to is the one that version carries.
        let (status, read) = call(&router, "DescribeDocument", &json!({ "Name": name })).await;
        assert_eq!(
            (
                status,
                read["Document"]["LatestVersion"].as_str(),
                read["Document"]["DefaultVersion"].as_str()
            ),
            (200, Some("1"), Some("1")),
            "case {case:?}: the refused update appended no version, got {status} {read}"
        );
        let (status, content_read) =
            call(&router, "GetDocument", &json!({ "Name": name })).await;
        assert_eq!(
            (status, content_read["Content"].as_str()),
            (200, Some(first.as_str())),
            "case {case:?}: the stored content is untouched, got {status} {content_read}"
        );
    }

    // A body that does carry the member is a version of the same document, so the refusals
    // above are about the missing member and not about the update being refused at all.
    let second = format!(
        r#"{{"schemaVersion": "2.2", "description": "second", "mainSteps": {COMMAND_STEPS}}}"#
    );
    let (status, updated) = call(
        &router,
        "UpdateDocument",
        &json!({ "Name": name, "Content": second, "DocumentVersion": "$LATEST" }),
    )
    .await;
    assert_eq!(
        (
            status,
            updated["DocumentDescription"]["DocumentVersion"].as_str()
        ),
        (200, Some("2")),
        "the update that carries the member appends a version, got {status} {updated}"
    );
}
