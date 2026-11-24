// SPDX-License-Identifier: CC0-1.0

//! The ORDER Run Command decides a send in, and the pair a per-node read resolves.
//!
//! Two rules, each asserted over a table that spans the class rather than one send:
//!
//! * a send is decided about its DOCUMENT first — the document itself, then the version,
//!   then the values the document declares — and only then about the nodes it names, so a
//!   parameter set the document does not admit is what such a send is refused for whatever
//!   state the nodes it was addressed to are in. The refusal is `InvalidParameters`, worded
//!   in the one fixed sentence that error carries
//!   ([`commands::INVALID_PARAMETERS`]) — the same sentence every operation of this service
//!   that answers that error answers, carrying nothing of the request;
//! * `GetCommandInvocation` resolves the PAIR of command id and node id against the
//!   invocations a send persisted, so a pair that matches none is `InvocationDoesNotExist`
//!   naming that pair — whether the command id resolves no command at all or a command it
//!   does resolve holds no invocation for the node.
//!
//! The documents the tables send are the ones Systems Manager publishes, which exist in
//! every account and Region without anybody creating them
//! (`state::documents::published_catalogue::PUBLISHED_DOCUMENTS`); which parameter each one
//! requires is read off that catalogue's bodies, cited on the constants below.
//!
//! Every case goes through the front door a client uses, over the router this crate's other
//! Run Command suites call through ([`crate::state_manager_associations::service`]).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_ssm_amazonssm::state::{
    commands, ComAmazonawsSsmAmazonssmState, ManagedNodeRecord, MANAGED_NODE_ID_MAX_LENGTH,
    MANAGED_NODE_ID_MIN_LENGTH, MANAGED_NODE_ID_PATTERN, RESOURCE_TYPE_EC2_INSTANCE,
    RESOURCE_TYPE_MANAGED_INSTANCE,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::state_manager_associations::{
    call, call_in_region, error_code, service, OTHER_REGION, REGION,
};

/// An online EC2 node running Linux.
const LINUX: &str = "i-0abc1234";

/// An online EC2 node running Windows.
const WINDOWS: &str = "i-0123456789abcdef0";

/// An online hybrid node running macOS.
const HYBRID: &str = "mi-01234567890123456";

/// A registered node whose agent has stopped reporting, so it cannot take a command.
const OFFLINE: &str = "i-0dead111";

/// A well-formed node id no node is registered under.
const UNREGISTERED: &str = "i-0dead999";

/// A node registered only in [`OTHER_REGION`].
const OTHER_ONLY: &str = "i-0eeee222";

/// `AWS-RunShellScript`, whose body declares `commands` with no default and restricts its
/// steps to Linux and macOS (`state::documents::RUN_SHELL_SCRIPT`).
const SHELL: &str = "AWS-RunShellScript";

/// `AWS-RunPatchBaseline`, whose body declares `Operation` with no default
/// (`state::patch_runs::RUN_PATCH_BASELINE`).
const PATCH: &str = "AWS-RunPatchBaseline";

/// `AWS-RunRemoteScript`, whose body declares `commandLine` with no default
/// (`state::published_command_documents::RUN_REMOTE_SCRIPT`).
const REMOTE: &str = "AWS-RunRemoteScript";

/// `AWS-UpdateSSMAgent`, every parameter of which declares a default, so a send that
/// supplies none satisfies it (`state::documents::UPDATE_SSM_AGENT`).
const AGENT: &str = "AWS-UpdateSSMAgent";

/// A well-formed command id no send minted.
const UNMINTED_COMMAND: &str = "11111111-2222-3333-4444-555555555555";

/// The refusal message of a failure body, from either spelling of the member.
fn error_message(body: &Value) -> String {
    body["Message"]
        .as_str()
        .or_else(|| body["message"].as_str())
        .unwrap_or("<no message>")
        .to_string()
}

/// How many commands the store holds, to pin that a refused send persisted nothing.
fn stored_commands(engine: &Arc<engine::InMemoryEngine>) -> usize {
    engine
        .state::<ComAmazonawsSsmAmazonssmState>()
        .lock()
        .commands
        .len()
}

/// One registered node, inserted the way its own registration would.
fn register(
    engine: &Arc<engine::InMemoryEngine>,
    region: &str,
    id: &str,
    resource_type: &str,
    platform: &str,
    ping_status: &str,
) {
    let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let mut record = ManagedNodeRecord::registering(id, resource_type, at);
    record.properties.platform_type = Some(platform.to_string());
    record.properties.ping_status = Some(ping_status.to_string());
    record.properties.computer_name = Some(format!("{id}.compute.internal"));
    record.tags = BTreeMap::new();
    assert!(
        engine
            .state::<ComAmazonawsSsmAmazonssmState>()
            .lock()
            .insert_managed_node(region, record),
        "a seeded node must carry the node id it is stored under"
    );
}

/// The inventory every table below sends against: three online nodes spanning node kind and
/// platform, one whose agent has stopped reporting, and one of another Region.
fn seed_inventory(engine: &Arc<engine::InMemoryEngine>) {
    register(engine, REGION, LINUX, RESOURCE_TYPE_EC2_INSTANCE, "Linux", "Online");
    register(
        engine,
        REGION,
        WINDOWS,
        RESOURCE_TYPE_EC2_INSTANCE,
        "Windows",
        "Online",
    );
    register(
        engine,
        REGION,
        HYBRID,
        RESOURCE_TYPE_MANAGED_INSTANCE,
        "MacOS",
        "Online",
    );
    register(
        engine,
        REGION,
        OFFLINE,
        RESOURCE_TYPE_EC2_INSTANCE,
        "Linux",
        "ConnectionLost",
    );
    register(
        engine,
        OTHER_REGION,
        OTHER_ONLY,
        RESOURCE_TYPE_EC2_INSTANCE,
        "Linux",
        "Online",
    );
}

/// A router with the node inventory seeded.
async fn sending() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let (router, engine) = service();
    seed_inventory(&engine);
    (router, engine)
}

/// RULE: the values a send supplies are decided against the document BEFORE any node it
/// names is resolved, so a parameter set the document does not admit is refused
/// `InvalidParameters` however the nodes stand — and the refusal is the one fixed sentence
/// that error carries, which repeats no part of the request.
#[tokio::test]
async fn a_parameter_set_the_document_rejects_is_refused_before_the_nodes_are_resolved() {
    let (router, engine) = sending().await;

    // One row per (way the parameters are wrong) x (way the nodes are wrong): a required
    // parameter left out and a parameter the document does not declare, including one whose
    // name differs from a declared one only in case, against an unregistered node, a node
    // whose agent has stopped reporting, the same node named twice, a node of another
    // Region, and a node whose platform the document's steps exclude.
    let rows: Vec<(&str, Value)> = vec![
        (
            "a patch runbook missing its one required parameter, sent to an unregistered node",
            json!({"DocumentName": PATCH, "InstanceIds": [UNREGISTERED]}),
        ),
        (
            "a shell document given a parameter it does not declare, sent to a node whose \
             agent has stopped reporting",
            json!({
                "DocumentName": SHELL,
                "InstanceIds": [OFFLINE],
                "Parameters": {"commands": ["echo hello"], "nope": ["x"]},
            }),
        ),
        (
            "a shell document missing its required parameter, sent to the same node twice",
            json!({"DocumentName": SHELL, "InstanceIds": [LINUX, LINUX]}),
        ),
        (
            "a remote-script document missing its required parameter, sent to a node of \
             another Region",
            json!({
                "DocumentName": REMOTE,
                "InstanceIds": [OTHER_ONLY],
                "Parameters": {"sourceType": ["S3"]},
            }),
        ),
        (
            "a shell document given a parameter it does not declare, sent to a node whose \
             platform its steps exclude",
            json!({
                "DocumentName": SHELL,
                "InstanceIds": [WINDOWS],
                "Parameters": {"commands": ["echo hello"], "workingDir": ["/tmp"]},
            }),
        ),
        (
            "a parameter whose name differs from a declared one only in case",
            json!({
                "DocumentName": SHELL,
                "InstanceIds": [UNREGISTERED],
                "Parameters": {"Commands": ["echo hello"]},
            }),
        ),
        (
            "a patch runbook given a parameter it does not declare, described by a target \
             that selects no node",
            json!({
                "DocumentName": PATCH,
                "Targets": [{"Key": "tag:env", "Values": ["nothing-carries-this"]}],
                "Parameters": {"Operation": ["Scan"], "Nope": ["x"]},
            }),
        ),
        (
            "an operation the patch runbook's own declaration does not allow, sent to an \
             unregistered node",
            json!({
                "DocumentName": PATCH,
                "InstanceIds": [UNREGISTERED],
                "Parameters": {"Operation": ["Reboot"]},
            }),
        ),
        (
            "two values for a parameter the document declares as one string, sent to a node \
             whose agent has stopped reporting",
            json!({
                "DocumentName": PATCH,
                "InstanceIds": [OFFLINE],
                "Parameters": {"Operation": ["Scan", "Install"]},
            }),
        ),
    ];

    for (case, body) in rows {
        let (status, parsed) = call(&router, "SendCommand", &body).await;
        assert_eq!(
            (status, error_code(&parsed).as_str()),
            (400, "InvalidParameters"),
            "case {case:?}: expected 400 InvalidParameters, got {status} with {parsed:?}"
        );
        let message = error_message(&parsed);
        assert_eq!(
            message,
            commands::INVALID_PARAMETERS,
            "case {case:?}: expected the fixed sentence, got {message:?}"
        );
        assert_eq!(
            stored_commands(&engine),
            0,
            "case {case:?}: a refused send persists no command, got {parsed:?}"
        );
    }
}

/// RULE, from the other side: the nodes a send NAMES — in `InstanceIds` or in a target
/// keyed by node id, which is the other spelling of the same list — are still resolved:
/// once the document and its parameters are satisfied, a node that cannot take the command
/// is what the send is refused for, while a target that DESCRIBES nodes by a criterion
/// selects what matches and refuses nothing ([`crate::run_command`]).
#[tokio::test]
async fn nodes_that_cannot_take_the_command_are_refused_once_the_parameters_satisfy_the_document(
) {
    let (router, engine) = sending().await;

    // (case, the send, the error code it is refused with, the message that error carries)
    let rows: Vec<(&str, Value, &str, Option<&str>)> = vec![
        (
            "an unregistered node id",
            json!({
                "DocumentName": PATCH,
                "InstanceIds": [UNREGISTERED],
                "Parameters": {"Operation": ["Scan"]},
            }),
            "InvalidInstanceId",
            Some(commands::NODES_NOT_IN_A_VALID_STATE),
        ),
        (
            "a node whose agent has stopped reporting",
            json!({"DocumentName": AGENT, "InstanceIds": [OFFLINE]}),
            "InvalidInstanceId",
            Some(commands::NODES_NOT_IN_A_VALID_STATE),
        ),
        (
            "a node registered only in another Region",
            json!({
                "DocumentName": SHELL,
                "InstanceIds": [OTHER_ONLY],
                "Parameters": {"commands": ["echo hello"]},
            }),
            "InvalidInstanceId",
            Some(commands::NODES_NOT_IN_A_VALID_STATE),
        ),
        (
            "the same node named twice",
            json!({"DocumentName": AGENT, "InstanceIds": [LINUX, LINUX]}),
            "DuplicateInstanceId",
            None,
        ),
        (
            "a node whose platform the document's steps exclude",
            json!({
                "DocumentName": SHELL,
                "InstanceIds": [WINDOWS],
                "Parameters": {"commands": ["echo hello"]},
            }),
            "UnsupportedPlatformType",
            None,
        ),
        (
            "a target that NAMES an unregistered node id",
            json!({
                "DocumentName": AGENT,
                "Targets": [{"Key": "InstanceIds", "Values": [UNREGISTERED]}],
            }),
            "InvalidInstanceId",
            Some(commands::NODES_NOT_IN_A_VALID_STATE),
        ),
        (
            "a target that names a registered node beside an unregistered one",
            json!({
                "DocumentName": AGENT,
                "Targets": [{"Key": "InstanceIds", "Values": [LINUX, UNREGISTERED]}],
            }),
            "InvalidInstanceId",
            Some(commands::NODES_NOT_IN_A_VALID_STATE),
        ),
        (
            "a target that names a node registered only in another Region",
            json!({
                "DocumentName": AGENT,
                "Targets": [{"Key": "AWS::EC2::Instance", "Values": [OTHER_ONLY]}],
            }),
            "InvalidInstanceId",
            Some(commands::NODES_NOT_IN_A_VALID_STATE),
        ),
        (
            "a target that names a node whose agent has stopped reporting",
            json!({
                "DocumentName": AGENT,
                "Targets": [{"Key": "InstanceIds", "Values": [OFFLINE]}],
            }),
            "InvalidInstanceId",
            Some(commands::NODES_NOT_IN_A_VALID_STATE),
        ),
    ];

    for (case, body, code, says) in rows {
        let (status, parsed) = call(&router, "SendCommand", &body).await;
        assert_eq!(
            (status, error_code(&parsed).as_str()),
            (400, code),
            "case {case:?}: expected 400 {code}, got {status} with {parsed:?}"
        );
        if let Some(says) = says {
            let message = error_message(&parsed);
            assert_eq!(
                message, says,
                "case {case:?}: expected {says:?}, got {message:?}"
            );
        }
        assert_eq!(
            stored_commands(&engine),
            0,
            "case {case:?}: a refused send persists no command, got {parsed:?}"
        );
    }
}

/// The content of the document the value table below sends: one parameter per kind of
/// restriction a `parameters` entry can state.
///
/// SOURCE: the `parameters` top-level element of the SSM document schema (AWS Systems
/// Manager User Guide, "Creating SSM document content"), which states `type`,
/// `allowedValues`, `allowedPattern`, `minChars`/`maxChars` and `minItems`/`maxItems` — the
/// same keywords the documents Systems Manager publishes are written with.
const CONSTRAINED_DOCUMENT: &str = r#"{
  "schemaVersion": "2.2",
  "description": "A document declaring one parameter per kind of restriction.",
  "parameters": {
    "mode": {
      "type": "String",
      "description": "(Optional) Drawn from an enumeration.",
      "allowedValues": ["fast", "slow"],
      "default": "fast"
    },
    "port": {
      "type": "String",
      "description": "(Optional) Matching an expression.",
      "allowedPattern": "^[0-9]{1,5}$",
      "default": "80"
    },
    "label": {
      "type": "String",
      "description": "(Optional) Between two and six characters.",
      "minChars": 2,
      "maxChars": 6,
      "default": "ok"
    },
    "hosts": {
      "type": "StringList",
      "description": "(Optional) One or two values.",
      "minItems": 1,
      "maxItems": 2,
      "default": ["localhost"]
    }
  },
  "mainSteps": [
    {
      "name": "runShellScript",
      "action": "aws:runShellScript",
      "inputs": {
        "commands": ["echo {{ mode }} {{ port }} {{ label }}"]
      }
    }
  ]
}"#;

/// The name the document above is created under. Not one of the prefixes the service
/// reserves for the documents it publishes, so a create may carry it.
const CONSTRAINED: &str = "ValueConstraints";

/// RULE: a value supplied for a parameter the document DOES declare has to be one that
/// parameter's own declaration admits — the enumeration, the expression, the lengths and the
/// counts it states — since a value outside them is not a value the document defined that
/// parameter to take.
#[tokio::test]
async fn a_value_the_parameters_own_declaration_excludes_is_refused() {
    let (router, _engine) = sending().await;

    let (status, created) = call(
        &router,
        "CreateDocument",
        &json!({
            "Name": CONSTRAINED,
            "DocumentType": "Command",
            "DocumentFormat": "JSON",
            "Content": CONSTRAINED_DOCUMENT,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the document under test must be created, got {status} with {created:?}"
    );

    // One row per restriction, satisfied and broken: each bound of a length and a count is
    // admitted, and one past each is not.
    let rows: Vec<(&str, &str, Value, bool)> = vec![
        ("a value the enumeration holds", CONSTRAINED, json!({"mode": ["slow"]}), true),
        (
            "a value the enumeration does not hold",
            CONSTRAINED,
            json!({"mode": ["medium"]}),
            false,
        ),
        (
            "a value differing from an admitted one only in case",
            CONSTRAINED,
            json!({"mode": ["Fast"]}),
            false,
        ),
        (
            "two values for a parameter declared as one string",
            CONSTRAINED,
            json!({"mode": ["fast", "slow"]}),
            false,
        ),
        ("a value the expression matches", CONSTRAINED, json!({"port": ["443"]}), true),
        (
            "a value the expression does not match",
            CONSTRAINED,
            json!({"port": ["https"]}),
            false,
        ),
        ("a value at the shortest length", CONSTRAINED, json!({"label": ["ab"]}), true),
        ("a value at the longest length", CONSTRAINED, json!({"label": ["abcdef"]}), true),
        (
            "a value one character shorter than the shortest",
            CONSTRAINED,
            json!({"label": ["a"]}),
            false,
        ),
        (
            "a value one character longer than the longest",
            CONSTRAINED,
            json!({"label": ["abcdefg"]}),
            false,
        ),
        ("one value for a list that takes one", CONSTRAINED, json!({"hosts": ["a"]}), true),
        (
            "two values for a list that takes two",
            CONSTRAINED,
            json!({"hosts": ["a", "b"]}),
            true,
        ),
        (
            "one value more than the list takes",
            CONSTRAINED,
            json!({"hosts": ["a", "b", "c"]}),
            false,
        ),
        // A published document the issue this suite was written for did not name, whose own
        // `action` parameter states an enumeration
        // (`state::published_command_documents::CONFIGURE_AWS_PACKAGE`).
        (
            "a published document's enumerated parameter, satisfied",
            "AWS-ConfigureAWSPackage",
            json!({"action": ["Uninstall"], "name": ["AmazonCloudWatchAgent"]}),
            true,
        ),
        (
            "a published document's enumerated parameter, broken",
            "AWS-ConfigureAWSPackage",
            json!({"action": ["Reinstall"], "name": ["AmazonCloudWatchAgent"]}),
            false,
        ),
    ];

    for (case, document, parameters, admitted) in rows {
        let (status, parsed) = call(
            &router,
            "SendCommand",
            &json!({
                "DocumentName": document,
                "InstanceIds": [LINUX],
                "Parameters": parameters,
            }),
        )
        .await;
        if admitted {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the send served, got {status} with {parsed:?}"
            );
        } else {
            assert_eq!(
                (status, error_code(&parsed).as_str()),
                (400, "InvalidParameters"),
                "case {case:?}: expected 400 InvalidParameters, got {status} with {parsed:?}"
            );
            let message = error_message(&parsed);
            assert_eq!(
                message,
                commands::INVALID_PARAMETERS,
                "case {case:?}: expected the fixed sentence, got {message:?}"
            );
        }
    }
}

/// RULE: a document the catalogue carries resolves for a send that names it exactly, with
/// the parameters that document declares — so a Run Command flow built on an AWS-owned
/// runbook is served, and the command it mints reports the document it ran.
#[tokio::test]
async fn a_send_naming_a_published_document_with_its_declared_parameters_is_served() {
    let (router, engine) = sending().await;

    // One row per published document the inventory can run: the one whose steps are
    // restricted to Linux and macOS, the patch runbook, the one every platform runs, and
    // the one addressed by a target rather than by a node id.
    let rows: Vec<(&str, Value, &str, usize)> = vec![
        (
            "the shell document on the Linux node",
            json!({
                "DocumentName": SHELL,
                "InstanceIds": [LINUX],
                "Parameters": {"commands": ["echo hello"]},
            }),
            SHELL,
            1,
        ),
        (
            "the shell document on the Linux and macOS nodes",
            json!({
                "DocumentName": SHELL,
                "InstanceIds": [LINUX, HYBRID],
                "Parameters": {"commands": ["echo hello"]},
            }),
            SHELL,
            2,
        ),
        (
            "the patch runbook on the Linux node",
            json!({
                "DocumentName": PATCH,
                "InstanceIds": [LINUX],
                "Parameters": {"Operation": ["Scan"]},
            }),
            PATCH,
            1,
        ),
        (
            "the agent-update document on the Windows node, with no parameters supplied",
            json!({"DocumentName": AGENT, "InstanceIds": [WINDOWS]}),
            AGENT,
            1,
        ),
        (
            "the agent-update document addressed by a node-id target",
            json!({
                "DocumentName": AGENT,
                "Targets": [{"Key": "InstanceIds", "Values": [LINUX]}],
            }),
            AGENT,
            1,
        ),
    ];

    let mut served = 0;
    for (case, body, document, targets) in rows {
        let (status, parsed) = call(&router, "SendCommand", &body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the send served, got {status} with {parsed:?}"
        );
        let command = parsed["Command"].clone();
        assert_eq!(
            command["DocumentName"].as_str(),
            Some(document),
            "case {case:?}: expected the document the send named, got {command:?}"
        );
        assert_eq!(
            command["TargetCount"].as_i64(),
            Some(targets as i64),
            "case {case:?}: expected {targets} target(s), got {command:?}"
        );
        served += 1;
        assert_eq!(
            stored_commands(&engine),
            served,
            "case {case:?}: every served send persists its command, got {command:?}"
        );
    }
}

/// RULE: a send has to say WHICH nodes to run on, and a request that neither names nor
/// describes any is refused with the sentence that states the two ways of saying it —
/// carrying nothing of the request, so it is the same text whichever way was left out.
#[tokio::test]
async fn a_send_that_names_no_node_at_all_is_refused_with_the_sentence_that_states_both_ways() {
    let (router, engine) = sending().await;

    // One row per way a send says nothing about which nodes: neither member, each member
    // present but empty, and both.
    let rows: Vec<(&str, Value)> = vec![
        ("neither member", json!({"DocumentName": AGENT})),
        (
            "an empty node-id list",
            json!({"DocumentName": AGENT, "InstanceIds": []}),
        ),
        (
            "an empty target list",
            json!({"DocumentName": AGENT, "Targets": []}),
        ),
        (
            "both members empty",
            json!({"DocumentName": AGENT, "InstanceIds": [], "Targets": []}),
        ),
    ];

    for (case, body) in rows {
        let (status, parsed) = call(&router, "SendCommand", &body).await;
        assert_eq!(
            (status, error_code(&parsed).as_str()),
            (400, "ValidationException"),
            "case {case:?}: expected 400 ValidationException, got {status} with {parsed:?}"
        );
        let message = error_message(&parsed);
        assert_eq!(
            message,
            commands::NO_NODES_NAMED_OR_DESCRIBED,
            "case {case:?}: expected the sentence stating both ways, got {message:?}"
        );
        assert_eq!(
            stored_commands(&engine),
            0,
            "case {case:?}: a refused send persists no command, got {parsed:?}"
        );
    }
}

/// RULE: document names are case-sensitive and a name no document of the endpoint resolves
/// is refused with the error code alone — `InvalidDocument` carrying no message member at
/// all, in either spelling.
#[tokio::test]
async fn a_name_no_document_resolves_is_refused_with_the_code_alone() {
    let (router, _engine) = sending().await;

    for case in [
        "aws-runpatchbaseline",
        "AWS-runshellscript",
        "AWS-UPDATESSMAGENT",
        "No-Such-Document",
    ] {
        let (status, parsed) = call(
            &router,
            "SendCommand",
            &json!({
                "DocumentName": case,
                "InstanceIds": [LINUX],
                "Parameters": {"commands": ["echo hello"]},
            }),
        )
        .await;
        assert_eq!(
            (status, error_code(&parsed).as_str()),
            (400, "InvalidDocument"),
            "case {case:?}: expected 400 InvalidDocument, got {status} with {parsed:?}"
        );
        for member in ["Message", "message"] {
            assert!(
                parsed.get(member).is_none(),
                "case {case:?}: the refusal carries no {member} member, got {parsed:?}"
            );
        }
    }
}

/// RULE: the per-node read states the node id's EXPRESSION before the band its length has to
/// fall in — the form of the value first, then the measurement of it — and states a clause
/// for every requirement the value broke.
#[tokio::test]
async fn the_per_node_read_states_the_node_ids_expression_before_its_band() {
    let (router, _engine) = sending().await;

    let must_match =
        format!("Member must satisfy regular expression pattern: {MANAGED_NODE_ID_PATTERN}");
    let at_least =
        format!("Member must have length greater than or equal to {MANAGED_NODE_ID_MIN_LENGTH}");
    let at_most =
        format!("Member must have length less than or equal to {MANAGED_NODE_ID_MAX_LENGTH}");

    // One row per set of requirements a node id can break: the expression alone, and the
    // expression beside each bound of the band — an EC2 id and a hybrid id either side of it.
    let rows: Vec<(&str, String, Vec<&String>)> = vec![
        (
            "an id shorter than the band and outside the expression",
            "i-zzz".to_string(),
            vec![&must_match, &at_least],
        ),
        (
            "a hybrid id shorter than the band and outside the expression",
            "mi-x".to_string(),
            vec![&must_match, &at_least],
        ),
        (
            "an id inside the band and outside the expression",
            "i-zzzz-zzzz".to_string(),
            vec![&must_match],
        ),
        (
            "an id longer than the band and outside the expression",
            format!("i-{}", "z".repeat(usize::try_from(MANAGED_NODE_ID_MAX_LENGTH).unwrap())),
            vec![&must_match, &at_most],
        ),
    ];

    for (case, instance_id, requirements) in rows {
        let clauses: Vec<String> = requirements
            .iter()
            .map(|requirement| {
                format!(
                    "Value '{instance_id}' at 'instanceId' failed to satisfy constraint: \
                     {requirement}"
                )
            })
            .collect();
        let expected = match clauses.len() {
            1 => format!("1 validation error detected: {}", clauses[0]),
            n => format!("{n} validation errors detected: {}", clauses.join("; ")),
        };
        let (status, parsed) = call(
            &router,
            "GetCommandInvocation",
            &json!({"CommandId": UNMINTED_COMMAND, "InstanceId": instance_id}),
        )
        .await;
        assert_eq!(
            (status, error_code(&parsed).as_str()),
            (400, "ValidationException"),
            "case {case:?}: expected 400 ValidationException, got {status} with {parsed:?}"
        );
        let message = error_message(&parsed);
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
    }
}

/// RULE: every operation of this service that answers `InvalidParameters` for a document's
/// declared parameters answers the same fixed sentence — it is the error class's own wording
/// and carries nothing of the request.
#[tokio::test]
async fn every_operation_words_the_invalid_parameters_refusal_the_same_way() {
    let (router, engine) = sending().await;

    // An association to update, created with a document whose every parameter is defaulted.
    let (status, created) = call(&router, "CreateAssociation", &json!({"Name": AGENT})).await;
    assert_eq!(
        status, 200,
        "the association under test must be created, got {status} with {created:?}"
    );
    let association_id = created["AssociationDescription"]["AssociationId"]
        .as_str()
        .expect("the create reports the association's id")
        .to_string();

    // (case, operation, the request whose parameters the document does not admit)
    let rows: Vec<(&str, &str, Value)> = vec![
        (
            "a send missing a required parameter",
            "SendCommand",
            json!({"DocumentName": SHELL, "InstanceIds": [LINUX]}),
        ),
        (
            "a send supplying an undeclared parameter",
            "SendCommand",
            json!({
                "DocumentName": PATCH,
                "InstanceIds": [LINUX],
                "Parameters": {"Operation": ["Scan"], "Nope": ["x"]},
            }),
        ),
        (
            "an association created with a required parameter missing",
            "CreateAssociation",
            json!({"Name": SHELL}),
        ),
        (
            "an association created with an undeclared parameter",
            "CreateAssociation",
            json!({"Name": AGENT, "Parameters": {"nope": ["x"]}}),
        ),
        (
            "an association updated with an undeclared parameter",
            "UpdateAssociation",
            json!({
                "AssociationId": association_id,
                "Parameters": {"nope": ["x"]},
            }),
        ),
    ];

    for (case, operation, body) in rows {
        let (status, parsed) = call(&router, operation, &body).await;
        assert_eq!(
            (status, error_code(&parsed).as_str()),
            (400, "InvalidParameters"),
            "case {case:?}: expected 400 InvalidParameters from {operation}, got {status} \
             with {parsed:?}"
        );
        let message = error_message(&parsed);
        assert_eq!(
            message,
            commands::INVALID_PARAMETERS,
            "case {case:?}: expected the fixed sentence, got {message:?}"
        );
        for carried in ["nope", "Nope", "commands", SHELL, PATCH, AGENT] {
            assert!(
                !message.contains(carried),
                "case {case:?}: the fixed sentence repeats no part of the request, yet it \
                 carries {carried:?}: {message:?}"
            );
        }
    }
    assert_eq!(
        stored_commands(&engine),
        0,
        "no refused send persisted a command"
    );
}

/// RULE: `GetCommandInvocation` resolves the PAIR the request names against the invocations
/// a send persisted, so a pair that matches none is `InvocationDoesNotExist` naming that
/// pair — the command id first, then the node id.
#[tokio::test]
async fn a_pair_that_matches_no_invocation_is_refused_naming_the_pair() {
    let (router, _engine) = sending().await;

    let (status, sent) = call(
        &router,
        "SendCommand",
        &json!({
            "DocumentName": SHELL,
            "InstanceIds": [LINUX],
            "Parameters": {"commands": ["echo hello"]},
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the command under test must be sent, got {status} with {sent:?}"
    );
    let minted = sent["Command"]["CommandId"]
        .as_str()
        .expect("the send reports the command id it minted")
        .to_string();

    // The pair the send DID create an invocation for reads back.
    let (status, reported) = call(
        &router,
        "GetCommandInvocation",
        &json!({"CommandId": minted, "InstanceId": LINUX}),
    )
    .await;
    assert_eq!(
        status, 200,
        "the pair the send created an invocation for reads back, got {status} with \
         {reported:?}"
    );

    // One row per way a pair matches no invocation: a command id nothing minted, the id of a
    // command another Region holds, a registered node the command did not run on, and a node
    // id nothing is registered under — the node inventory is no part of resolving a pair, so
    // the last two are refused alike.
    let rows: Vec<(&str, &str, &str, &str)> = vec![
        ("a command id nothing minted", REGION, UNMINTED_COMMAND, LINUX),
        (
            "an id nothing minted, for a node nothing is registered under",
            REGION,
            UNMINTED_COMMAND,
            UNREGISTERED,
        ),
        (
            "the command read through another Region's endpoint",
            OTHER_REGION,
            minted.as_str(),
            LINUX,
        ),
        (
            "a registered node the command did not run on",
            REGION,
            minted.as_str(),
            WINDOWS,
        ),
        (
            "a hybrid node the command did not run on",
            REGION,
            minted.as_str(),
            HYBRID,
        ),
        (
            "a node id nothing is registered under",
            REGION,
            minted.as_str(),
            UNREGISTERED,
        ),
    ];

    for (case, region, command_id, instance_id) in rows {
        let (status, parsed) = call_in_region(
            &router,
            region,
            "GetCommandInvocation",
            &json!({"CommandId": command_id, "InstanceId": instance_id}),
        )
        .await;
        assert_eq!(
            (status, error_code(&parsed).as_str()),
            (400, "InvocationDoesNotExist"),
            "case {case:?}: expected 400 InvocationDoesNotExist, got {status} with {parsed:?}"
        );
        let message = error_message(&parsed);
        let expected = commands::invocation_not_found(command_id, instance_id);
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
    }
}
