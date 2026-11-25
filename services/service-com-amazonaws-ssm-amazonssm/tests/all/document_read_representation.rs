// SPDX-License-Identifier: CC0-1.0

//! How a document READ represents what the store holds: the body Systems Manager serves
//! for an Amazon-owned document, the YAML a format conversion writes, the refusal a
//! version name no version carries is answered with, and the status a document is in once
//! the call that minted it has returned.
//!
//! Every case goes through the front door a client uses — the shared harness in
//! `all/parameter_store.rs` dispatches `POST /` with `X-Amz-Target:
//! AmazonSSM.<Operation>` through the multi-service router — so a response that is not
//! computed from persisted state cannot pass.
//!
//! The suites are tables whose rows differ along what each rule generalises over: the
//! plugin an Amazon-owned schema 1.2 document declares, the shape of the content being
//! converted, the way a request names a version, and the call that mints a document
//! version.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_ssm_amazonssm::state::{
    ComAmazonawsSsmAmazonssmState, ManagedNodeRecord, RESOURCE_TYPE_EC2_INSTANCE,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::parameter_store::{call, error_code, router, router_with_engine, REGION};

/// `CreateDocument` with `body`, asserting it succeeded, and returning the
/// `DocumentDescription` it reported.
async fn create_ok(router: &MultiServiceRouter, case: &str, body: &Value) -> Value {
    let (status, response) = call(router, "CreateDocument", body).await;
    assert_eq!(
        status, 200,
        "case {case}: CreateDocument must succeed, got {status} {response}"
    );
    response["DocumentDescription"].clone()
}

/// `GetDocument` with `body`, returning `(status, body)`.
async fn get(router: &MultiServiceRouter, body: &Value) -> (u16, Value) {
    call(router, "GetDocument", body).await
}

/// `GetDocument` with `body`, asserting the read succeeded.
async fn get_ok(router: &MultiServiceRouter, case: &str, body: &Value) -> Value {
    let (status, response) = get(router, body).await;
    assert_eq!(
        status, 200,
        "case {case}: GetDocument {body} must succeed, got {status} {response}"
    );
    response
}

/// The `Content` a read reported, parsed as the JSON object it is.
fn content_tree(case: &str, response: &Value) -> Value {
    let content = response["Content"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case}: a read reports Content, got {response}"));
    serde_json::from_str(content)
        .unwrap_or_else(|why| panic!("case {case}: expected JSON content, got {content:?} ({why})"))
}

/// RULE: an Amazon-owned document is served in the schema Systems Manager publishes it
/// in, and a schema 1.2 document declares the plugin Run Command invokes in a TOP-LEVEL
/// `runtimeConfig` map keyed by the plugin's name — which is what makes the document
/// runnable — beside the `parameters` the plugin takes its inputs from.
///
/// One row per schema 1.2 document of the published catalogue
/// (`crate::state::PUBLISHED_DOCUMENTS`, whose bodies are the entries read here), so the
/// rule is asserted of the catalogue rather than of the one document a recorded unit read.
#[tokio::test]
async fn an_amazon_owned_schema_1_2_document_carries_its_plugin_map() {
    let router = router();
    // (case, document name, the plugin its runtimeConfig declares, a parameter it takes)
    let rows: [(&str, &str, &str, &str); 3] = [
        (
            "the shell-script command document",
            "AWS-RunShellScript",
            "aws:runShellScript",
            "commands",
        ),
        (
            "the PowerShell command document",
            "AWS-RunPowerShellScript",
            "aws:runPowerShellScript",
            "commands",
        ),
        (
            "the CloudWatch agent configuration document",
            "AWS-ConfigureCloudWatch",
            "aws:cloudWatch",
            "status",
        ),
    ];
    for (case, name, plugin, parameter) in rows {
        let read = get_ok(&router, case, &json!({ "Name": name })).await;
        assert_eq!(
            read["DocumentFormat"],
            json!("JSON"),
            "case {case}: expected the published body's own format, got {read}"
        );
        assert_eq!(
            read["Status"],
            json!("Active"),
            "case {case}: a published document is already registered, got {read}"
        );
        let content = content_tree(case, &read);
        assert_eq!(
            content["schemaVersion"],
            json!("1.2"),
            "case {case}: expected the schema version it is published at, got {content}"
        );
        let plugins = &content["runtimeConfig"];
        assert!(
            plugins.is_object(),
            "case {case}: a schema 1.2 body carries a runtimeConfig plugin map, got {content}"
        );
        assert!(
            plugins.get(plugin).is_some(),
            "case {case}: expected the plugin {plugin:?} in {plugins}"
        );
        assert!(
            content["parameters"].get(parameter).is_some(),
            "case {case}: expected the parameter {parameter:?} in {content}"
        );
    }
}

/// Register one online EC2 node of `platform` in [`REGION`], the way the node's own
/// registration would — a node enters the inventory only by registering, and no public
/// operation of this service registers one.
fn register_node(engine: &Arc<engine::InMemoryEngine>, id: &str, platform: &str) {
    let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let mut record = ManagedNodeRecord::registering(id, RESOURCE_TYPE_EC2_INSTANCE, at);
    record.properties.platform_type = Some(platform.to_string());
    record.properties.ping_status = Some("Online".to_string());
    assert!(
        engine
            .state::<ComAmazonawsSsmAmazonssmState>()
            .lock()
            .insert_managed_node(REGION, record),
        "a seeded node must carry the node id it is stored under"
    );
}

/// RULE: the plugin a schema 1.2 document declares is the step a node's agent performs, so
/// the invocation reports it under the plugin's own name and runs the script the plugin's
/// `runCommand` property names — the same send that drives a schema 2.x document's
/// `mainSteps`.
#[tokio::test]
async fn a_schema_1_2_plugin_is_the_step_a_send_reports() {
    let (router, engine) = router_with_engine();
    let node = "i-0abc1234";
    register_node(&engine, node, "Linux");
    let name = "Schema12PluginDocument";
    // A document of this account written the way a schema 1.2 document is: its plugin
    // entry carries the script in `runCommand`, and the plugin decides where it runs.
    let content = json!({
        "schemaVersion": "1.2",
        "description": "Run a shell script through a schema 1.2 plugin.",
        "parameters": {
            "commands": { "type": "StringList", "description": "(Required) the script" }
        },
        "runtimeConfig": {
            "aws:runShellScript": {
                "properties": [{
                    "id": "0.aws:runShellScript",
                    "runCommand": "{{ commands }}",
                }]
            }
        }
    })
    .to_string();
    let created = create_ok(
        &router,
        "a schema 1.2 document of this account",
        &json!({ "Name": name, "Content": content, "DocumentType": "Command" }),
    )
    .await;
    assert_eq!(
        created["SchemaVersion"],
        json!("1.2"),
        "the schema version is read off the content, got {created}"
    );
    assert_eq!(
        created["PlatformTypes"],
        json!(["Linux", "MacOS"]),
        "the plugin is what restricts the document to a platform, got {created}"
    );

    // A command history read asks for one step's results BY NAME, and the name of a
    // schema 1.2 step is its plugin's — so the plugin resolves and anything else does not.
    let (status, sent) = call(
        &router,
        "SendCommand",
        &json!({
            "DocumentName": name,
            "InstanceIds": [node],
            "Parameters": { "commands": ["echo hello"] },
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "a send of a schema 1.2 document is served, got {status} {sent}"
    );
    let command_id = sent["Command"]["CommandId"]
        .as_str()
        .unwrap_or_else(|| panic!("the send reports a CommandId, got {sent}"))
        .to_string();
    for (case, plugin, expected) in [
        ("the plugin the content declares", "aws:runShellScript", None),
        (
            "a step name the content declares nothing of",
            "runShellScriptOnLinux",
            Some("InvalidPluginName"),
        ),
    ] {
        let (status, response) = call(
            &router,
            "GetCommandInvocation",
            &json!({
                "CommandId": command_id,
                "InstanceId": node,
                "PluginName": plugin,
            }),
        )
        .await;
        match expected {
            None => {
                assert_eq!(
                    status, 200,
                    "case {case}: the declared step resolves, got {status} {response}"
                );
                assert_eq!(
                    response["PluginName"],
                    json!(plugin),
                    "case {case}: the reported step is the one asked for, got {response}"
                );
            }
            Some(code) => assert_eq!(
                (status, error_code(&response).as_str()),
                (400, code),
                "case {case}: expected {code}, got {status} {response}"
            ),
        }
    }
}

/// RULE: a JSON-authored document asked for as YAML is converted, and the conversion is
/// written in one serializer style: a `---` document-start marker, every string scalar in
/// double quotes, a number and a boolean unquoted, and a block sequence's `- ` indicator
/// at the indentation of the key the sequence belongs to.
///
/// The rows differ along the shape of the content being converted — one scalar, a nested
/// mapping, a sequence of scalars, a sequence of mappings with a sequence inside, and
/// values that are not strings — rather than along the one document a recorded unit
/// created.
#[tokio::test]
async fn a_json_document_asked_for_as_yaml_is_converted_in_one_style() {
    let router = router();
    // (case, document name, JSON content, the YAML it converts to)
    let rows: [(&str, &str, &str, &str); 5] = [
        (
            "one string scalar",
            "YamlScalarDocument",
            r#"{"schemaVersion": "2.2"}"#,
            "---\nschemaVersion: \"2.2\"\n",
        ),
        (
            "a nested mapping",
            "YamlMappingDocument",
            r#"{"schemaVersion": "2.2", "parameters": {"Message": {"type": "String"}}}"#,
            "---\nschemaVersion: \"2.2\"\nparameters:\n  Message:\n    type: \"String\"\n",
        ),
        (
            "a sequence of scalars",
            "YamlSequenceDocument",
            r#"{"schemaVersion": "2.2", "commands": ["echo one", "echo two"]}"#,
            "---\nschemaVersion: \"2.2\"\ncommands:\n- \"echo one\"\n- \"echo two\"\n",
        ),
        (
            "a sequence of mappings with a sequence inside",
            "YamlStepsDocument",
            r#"{"schemaVersion": "2.2", "description": "yaml conversion document",
                "mainSteps": [{"action": "aws:runShellScript", "name": "run",
                "inputs": {"runCommand": ["echo hello"]}}]}"#,
            "---\nschemaVersion: \"2.2\"\ndescription: \"yaml conversion document\"\n\
             mainSteps:\n- action: \"aws:runShellScript\"\n  name: \"run\"\n  inputs:\n    \
             runCommand:\n    - \"echo hello\"\n",
        ),
        (
            "values that are not strings",
            "YamlScalarsDocument",
            r#"{"schemaVersion": "2.2", "parameters": {"Count": {"minItems": 1,
                "required": true, "default": null}}}"#,
            "---\nschemaVersion: \"2.2\"\nparameters:\n  Count:\n    minItems: 1\n    \
             required: true\n    default: null\n",
        ),
    ];
    for (case, name, content, expected) in rows {
        create_ok(
            &router,
            case,
            &json!({
                "Name": name,
                "Content": content,
                "DocumentType": "Command",
                "DocumentFormat": "JSON",
            }),
        )
        .await;
        let read = get_ok(
            &router,
            case,
            &json!({ "Name": name, "DocumentFormat": "YAML" }),
        )
        .await;
        assert_eq!(
            read["DocumentFormat"],
            json!("YAML"),
            "case {case}: the read reports the format it answered in, got {read}"
        );
        assert_eq!(
            read["Content"],
            json!(expected),
            "case {case}: expected the conversion {expected:?}, got {}",
            read["Content"]
        );
        // The conversion is YAML this service reads back as the same document: a second
        // document created from it declares the tree the first one declared.
        let round_trip = format!("{name}FromYaml");
        create_ok(
            &router,
            case,
            &json!({
                "Name": round_trip,
                "Content": expected,
                "DocumentType": "Command",
                "DocumentFormat": "YAML",
            }),
        )
        .await;
        let reread = get_ok(
            &router,
            case,
            &json!({ "Name": round_trip, "DocumentFormat": "JSON" }),
        )
        .await;
        assert_eq!(
            content_tree(case, &reread),
            serde_json::from_str::<Value>(content).expect("the row's content is JSON"),
            "case {case}: the tree must survive JSON → YAML → JSON"
        );
    }
}

/// RULE: a `VersionName` no version of a document carries addresses no document, so a
/// document read naming one is refused 400 `InvalidDocument` with `Document with name
/// <name> with version <versionName> does not exist.`; a numeric `DocumentVersion` the
/// document holds no version of is the invalid version it is, refused 400
/// `InvalidDocumentVersion`.
///
/// The rows differ along the way a request names a version — a version name alone, one
/// beside a selector that does resolve, a numeric version past the latest — and are
/// asserted of both document reads, since both resolve a version the same way.
#[tokio::test]
async fn a_version_name_no_version_carries_is_a_missing_document() {
    let router = router();
    let name = "VersionNameRefusalDocument";
    let content = r#"{"schemaVersion": "2.2", "description": "versioned"}"#;
    create_ok(
        &router,
        "a document with one named version",
        &json!({ "Name": name, "Content": content, "VersionName": "release-one" }),
    )
    .await;

    // (case, the members naming a version, the code, the message)
    let rows: [(&str, Value, &str, String); 4] = [
        (
            "a version name no version carries",
            json!({ "VersionName": "release-two" }),
            "InvalidDocument",
            format!("Document with name {name} with version release-two does not exist."),
        ),
        (
            "another version name no version carries",
            json!({ "VersionName": "9.9" }),
            "InvalidDocument",
            format!("Document with name {name} with version 9.9 does not exist."),
        ),
        (
            "a version name beside a selector that resolves",
            json!({ "DocumentVersion": "1", "VersionName": "release-two" }),
            "InvalidDocument",
            format!("Document with name {name} with version release-two does not exist."),
        ),
        (
            "a numeric version the document holds none of",
            json!({ "DocumentVersion": "99" }),
            "InvalidDocumentVersion",
            "Document version: 99 is invalid, latest version is 1".to_string(),
        ),
    ];
    for (case, naming, code, message) in rows {
        let mut body = json!({ "Name": name });
        for (member, value) in naming.as_object().expect("the row's members") {
            body[member] = value.clone();
        }
        let (status, response) = get(&router, &body).await;
        assert_eq!(
            (status, error_code(&response).as_str()),
            (400, code),
            "case {case}: expected {code}, got {status} {response}"
        );
        assert_eq!(
            response["Message"],
            json!(message),
            "case {case}: expected the message {message:?}, got {response}"
        );
        // The version name is resolved the same way by the other document read, so the
        // class it is refused with is the same there.
        let (status, described) = call(&router, "DescribeDocument", &body).await;
        assert_eq!(
            (status, error_code(&described).as_str()),
            (400, code),
            "case {case}: DescribeDocument expected {code}, got {status} {described}"
        );
    }

    // The version name the create DID supply still resolves, so the refusals above are
    // about the name asked for and not about the member being carried at all.
    let read = get_ok(
        &router,
        "the version name the create supplied",
        &json!({ "Name": name, "VersionName": "release-one" }),
    )
    .await;
    assert_eq!(
        read["VersionName"],
        json!("release-one"),
        "the stored version name selects its version, got {read}"
    );
}

/// RULE: a document version is registered by the call that mints it — the mint's own
/// response reports the transient status (`Creating` for a create, `Updating` for an
/// update) and the version is `Active` for every read that comes after, with nobody
/// polling.
///
/// The rows differ along the call that mints the version and the document type it mints
/// it for, and each is read back through BOTH document reads.
#[tokio::test]
async fn a_minted_document_version_is_active_on_the_next_read() {
    let router = router();
    let command = r#"{"schemaVersion": "2.2", "description": "mint"}"#;
    let automation = r#"{"schemaVersion": "0.3", "description": "mint",
        "mainSteps": [{"name": "wait", "action": "aws:sleep",
        "inputs": {"Duration": "PT0S"}}]}"#;
    // (case, name, the create's body, the transient status the create reports)
    let rows: [(&str, &str, Value); 2] = [
        (
            "a Command document",
            "MintedCommandDocument",
            json!({ "Content": command, "DocumentType": "Command" }),
        ),
        (
            "an Automation runbook",
            "MintedAutomationDocument",
            json!({ "Content": automation, "DocumentType": "Automation" }),
        ),
    ];
    for (case, name, extra) in rows {
        let mut request = json!({ "Name": name });
        for (member, value) in extra.as_object().expect("the row's members") {
            request[member] = value.clone();
        }
        let created = create_ok(&router, case, &request).await;
        assert_eq!(
            created["Status"],
            json!("Creating"),
            "case {case}: the create reports the transient status, got {created}"
        );
        for read in ["GetDocument", "DescribeDocument"] {
            let (status, response) = call(&router, read, &json!({ "Name": name })).await;
            assert_eq!(
                status, 200,
                "case {case}: {read} must succeed, got {status} {response}"
            );
            let document = if read == "DescribeDocument" {
                response["Document"].clone()
            } else {
                response
            };
            assert_eq!(
                document["Status"],
                json!("Active"),
                "case {case}: {read} after the create reports Active, got {document}"
            );
        }

        // An update mints the next version the same way: the update's own response
        // reports `Updating` and the next read reports the version ready to use.
        let updated_content = request["Content"]
            .as_str()
            .expect("the row's content")
            .replace("\"mint\"", "\"minted again\"");
        let (status, response) = call(
            &router,
            "UpdateDocument",
            &json!({
                "Name": name,
                "Content": updated_content,
                "DocumentVersion": "$LATEST",
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case}: UpdateDocument must succeed, got {status} {response}"
        );
        assert_eq!(
            response["DocumentDescription"]["Status"],
            json!("Updating"),
            "case {case}: the update reports the transient status, got {response}"
        );
        let read = get_ok(
            &router,
            case,
            &json!({ "Name": name, "DocumentVersion": "2" }),
        )
        .await;
        assert_eq!(
            read["Status"],
            json!("Active"),
            "case {case}: the read after the update reports Active, got {read}"
        );
    }
}
