// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for the wording and the class of the two refusals a managed
//! permission write earns.
//!
//! Two rules the rows below pin.
//!
//! * A create for a name the Region already holds is refused as
//!   `PermissionAlreadyExistsException` with HTTP 409, and its `message` identifies the
//!   permission that already holds the name by the ARN it is STORED under — fixed text
//!   around that one value, with nothing else from the request in it.
//! * A `policyTemplate` that cannot be read as a policy statement is refused, by every
//!   operation that takes one, as `InvalidPolicyException` with HTTP 400 and one fixed
//!   sentence that interpolates nothing from the request. The template's own rules
//!   (`CreatePermission$policyTemplate`: an `Effect` of `ALLOW`, actions the resource
//!   type supports) are the same error class, reported with the rule that was broken.
//! * A template whose `Action` element names an action the permission's resource type
//!   does not support is refused, by every operation that takes one, as
//!   `InvalidPolicyException` with HTTP 400 and one sentence whose only variable part
//!   is that resource type; a template naming only actions it supports is stored and
//!   reads back.
//!
//! So the rows vary along what the rules generalise over:
//!
//! * WHICH name is taken, in every form `CreatePermission$name` admits (length 1..36,
//!   pattern `^[\w.-]*$`): a single character, a dotted name, a hyphenated one, an
//!   underscored one, a digit-led one, and one at the modelled maximum length;
//! * WHAT the second create changes beside the name: nothing, the resource type, the
//!   template, the tags, a `clientToken` it did not carry before;
//! * HOW a policy document fails to read as a policy statement: not JSON at all, a JSON
//!   scalar, a JSON array, a missing `Effect`, an `Effect` of every other JSON type, a
//!   missing `Action`, an `Action` of a type that names no action, a list with an entry
//!   that is not a name, an empty list, and a `Condition` that is not an object;
//! * WHICH operation takes the template — the create and the version create;
//! * WHY an action is not supported for the resource type it was named for: it controls
//!   a resource of that very type (created, deleted, updated, put over, named in the
//!   singular or the plural), it belongs to another service's namespace, or it names no
//!   namespace at all — against actions of the same services that ARE supported, over
//!   several resource types of the catalog.

use serde_json::{json, Value};
use service_com_amazonaws_ram_amazonresourcesharing::state::{
    resource_type_entry_of, AWS_MANAGED_PERMISSIONS,
};
use smithy_server_runtime::MultiServiceRouter;

// Every suite in this crate calls the service the one way: through `tests/all/harness.rs`.
use super::harness::*;

/// The sentence AWS words a refusal of an unreadable policy document with. Entirely
/// fixed text: the error's only member is `message` (see `InvalidPolicyException` in
/// this service's model, whose `awsQueryError` code is `InvalidPolicy`), and no part of
/// it comes from the request.
const INVALID_POLICY_FORMAT: &str = "InvalidPolicy: Invalid policy format.";

/// A resource type a customer managed permission can be created for, and a second one
/// of the same service, as `CreatePermission$resourceType` and `RESOURCE_TYPES`
/// describe them.
const RESOURCE_TYPE: &str = "codebuild:Project";
const OTHER_RESOURCE_TYPE: &str = "codebuild:ReportGroup";

/// A template that reads as the statement `CreatePermission$policyTemplate` documents.
const GOOD_TEMPLATE: &str = r#"{"Effect":"ALLOW","Action":["codebuild:ListProjects"]}"#;

async fn create_permission(router: &MultiServiceRouter, body: &Value) -> (u16, Value) {
    call(router, "POST", "/createpermission", Some(body)).await
}

async fn create_permission_version(router: &MultiServiceRouter, body: &Value) -> (u16, Value) {
    call(router, "POST", "/createpermissionversion", Some(body)).await
}

/// The ARN of the permission a create reported.
fn created_arn(case: &str, body: &Value) -> String {
    body.get("permission")
        .and_then(|permission| permission.get("arn"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: the create reported no permission arn, got {body:?}"))
        .to_string()
}

/// One row of the name-clash table: the name to take, and what the second create
/// changes beside it.
struct ClashCase {
    case: &'static str,
    name: String,
    second: fn(&str) -> Value,
}

/// The second create: the very same request again.
fn same_request(name: &str) -> Value {
    json!({"name": name, "resourceType": RESOURCE_TYPE, "policyTemplate": GOOD_TEMPLATE})
}

/// The second create: the same name for another resource type.
fn other_resource_type(name: &str) -> Value {
    json!({"name": name, "resourceType": OTHER_RESOURCE_TYPE, "policyTemplate": GOOD_TEMPLATE})
}

/// The second create: the same name carrying another template.
fn other_template(name: &str) -> Value {
    json!({
        "name": name,
        "resourceType": RESOURCE_TYPE,
        "policyTemplate": r#"{"Effect":"ALLOW","Action":["codebuild:BatchGetProjects"]}"#,
    })
}

/// The second create: the same name carrying tags the first did not.
fn with_tags(name: &str) -> Value {
    json!({
        "name": name,
        "resourceType": RESOURCE_TYPE,
        "policyTemplate": GOOD_TEMPLATE,
        "tags": [{"key": "env", "value": "test"}],
    })
}

/// The second create: the same name under a `clientToken` nothing was created with, so
/// the clash is reached rather than the idempotent path.
fn with_fresh_token(name: &str) -> Value {
    json!({
        "name": name,
        "resourceType": RESOURCE_TYPE,
        "policyTemplate": GOOD_TEMPLATE,
        "clientToken": format!("token-for-{name}"),
    })
}

fn clash_cases() -> Vec<ClashCase> {
    vec![
        ClashCase {
            case: "the same request again",
            name: "TakenPlainName".to_string(),
            second: same_request,
        },
        ClashCase {
            case: "a one-character name, the modelled minimum length",
            name: "A".to_string(),
            second: same_request,
        },
        ClashCase {
            case: "a dotted name, for another resource type",
            name: "taken.dotted.name".to_string(),
            second: other_resource_type,
        },
        ClashCase {
            case: "a hyphenated name, carrying another template",
            name: "taken-hyphen-name".to_string(),
            second: other_template,
        },
        ClashCase {
            case: "an underscored name, now carrying tags",
            name: "taken_underscore_name".to_string(),
            second: with_tags,
        },
        ClashCase {
            case: "a digit-led name, under a fresh clientToken",
            name: "9taken".to_string(),
            second: with_fresh_token,
        },
        ClashCase {
            // 36 characters: the maximum `CreatePermission$name` admits.
            case: "a name at the modelled maximum length",
            name: "T".repeat(36),
            second: same_request,
        },
    ]
}

#[tokio::test]
async fn a_name_clash_names_the_stored_permission_by_its_arn() {
    let router = emulator();
    for ClashCase { case, name, second } in clash_cases() {
        let (status, created) = create_permission(
            &router,
            &json!({
                "name": name,
                "resourceType": RESOURCE_TYPE,
                "policyTemplate": GOOD_TEMPLATE,
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the first create expected 200, got {status} with {created:?}"
        );
        // The ARN the refusal must name is the one the permission is STORED under, as
        // the create itself reported it — not a value this test composed.
        let stored = created_arn(case, &created);

        let (status, refused) = create_permission(&router, &second(&name)).await;
        assert_eq!(
            (status, error_code(&refused)),
            (409, "PermissionAlreadyExistsException".to_string()),
            "case {case:?}: expected 409 PermissionAlreadyExistsException, got {status} with \
             {refused:?}"
        );
        let expected = format!("The permission {stored} already exists.");
        let message = error_message(&refused);
        assert_eq!(
            message, expected,
            "case {case:?}: expected the message {expected:?}, got {message:?}"
        );
        // The message is that ARN and nothing else from the request: a second create
        // that changed the resource type, the template or the tags reports the same
        // sentence as one that changed nothing.
        for leaked in [OTHER_RESOURCE_TYPE, "env", "clientToken", "Region"] {
            assert!(
                !message.contains(leaked),
                "case {case:?}: the message must not carry {leaked:?}, got {message:?}"
            );
        }
    }
}

/// A policy document that cannot be read as a policy statement, and how it fails to.
struct FormatCase {
    case: &'static str,
    template: String,
}

fn format_cases() -> Vec<FormatCase> {
    let rows: Vec<(&'static str, String)> = vec![
        ("not JSON at all", "allow everything".to_string()),
        ("an empty document", String::new()),
        ("a JSON string", json!("ALLOW").to_string()),
        ("a JSON number", "7".to_string()),
        ("a JSON null", "null".to_string()),
        ("a JSON array of statements", json!([{"Effect": "ALLOW"}]).to_string()),
        (
            "no Effect element",
            json!({"Action": ["codebuild:ListProjects"]}).to_string(),
        ),
        (
            "an Effect that is a number",
            json!({"Effect": 1, "Action": ["codebuild:ListProjects"]}).to_string(),
        ),
        (
            "an Effect that is a boolean",
            json!({"Effect": true, "Action": ["codebuild:ListProjects"]}).to_string(),
        ),
        (
            "an Effect that is a list",
            json!({"Effect": ["ALLOW"], "Action": ["codebuild:ListProjects"]}).to_string(),
        ),
        (
            "an Effect that is an object",
            json!({"Effect": {"value": "ALLOW"}, "Action": ["codebuild:ListProjects"]}).to_string(),
        ),
        (
            "an Effect that is null",
            json!({"Effect": null, "Action": ["codebuild:ListProjects"]}).to_string(),
        ),
        ("no Action element", json!({"Effect": "ALLOW"}).to_string()),
        (
            "an Action that is a number",
            json!({"Effect": "ALLOW", "Action": 5}).to_string(),
        ),
        (
            "an Action that is an object",
            json!({"Effect": "ALLOW", "Action": {"name": "codebuild:ListProjects"}}).to_string(),
        ),
        (
            "an Action list with an entry that is not a name",
            json!({"Effect": "ALLOW", "Action": ["codebuild:ListProjects", 7]}).to_string(),
        ),
        (
            "an empty Action list",
            json!({"Effect": "ALLOW", "Action": []}).to_string(),
        ),
        (
            "a Condition that is not an object",
            json!({
                "Effect": "ALLOW",
                "Action": ["codebuild:ListProjects"],
                "Condition": "always",
            })
            .to_string(),
        ),
    ];
    rows.into_iter()
        .map(|(case, template)| FormatCase { case, template })
        .collect()
}

#[tokio::test]
async fn an_unreadable_policy_document_is_refused_as_ram_words_it() {
    let router = emulator();

    // The permission the version creates are made against, so both operations that
    // take a `policyTemplate` are driven with every row.
    let (status, created) = create_permission(
        &router,
        &json!({
            "name": "TemplateFormatSubject",
            "resourceType": RESOURCE_TYPE,
            "policyTemplate": GOOD_TEMPLATE,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"setup\": the create expected 200, got {status} with {created:?}"
    );
    let subject = created_arn("setup", &created);

    let mut reported: Vec<(&str, String)> = Vec::new();
    for FormatCase { case, template } in format_cases() {
        let (status, refused) = create_permission(
            &router,
            &json!({
                "name": "FormatRefused",
                "resourceType": RESOURCE_TYPE,
                "policyTemplate": template,
            }),
        )
        .await;
        assert_eq!(
            (status, error_code(&refused)),
            (400, "InvalidPolicyException".to_string()),
            "case {case:?}: CreatePermission expected 400 InvalidPolicyException, got {status} \
             with {refused:?}"
        );
        let message = error_message(&refused);
        assert_eq!(
            message, INVALID_POLICY_FORMAT,
            "case {case:?}: CreatePermission expected the message {INVALID_POLICY_FORMAT:?}, got \
             {message:?}"
        );
        reported.push((case, message));

        let (status, refused) = create_permission_version(
            &router,
            &json!({"permissionArn": subject, "policyTemplate": template}),
        )
        .await;
        assert_eq!(
            (status, error_code(&refused)),
            (400, "InvalidPolicyException".to_string()),
            "case {case:?}: CreatePermissionVersion expected 400 InvalidPolicyException, got \
             {status} with {refused:?}"
        );
        let message = error_message(&refused);
        assert_eq!(
            message, INVALID_POLICY_FORMAT,
            "case {case:?}: CreatePermissionVersion expected the message \
             {INVALID_POLICY_FORMAT:?}, got {message:?}"
        );
        reported.push((case, message));
    }

    // The sentence carries nothing from the request, so every row of either operation
    // reported the very same bytes.
    let distinct: std::collections::BTreeSet<&str> = reported
        .iter()
        .map(|(_, message)| message.as_str())
        .collect();
    assert_eq!(
        distinct.len(),
        1,
        "case \"every unreadable document\": expected one message across all rows, got {reported:?}"
    );
    assert!(
        reported.len() >= 36,
        "case \"every unreadable document\": expected both operations on every row, got \
         {reported:?}"
    );

    // A refused create stored nothing, and the subject still carries only the version
    // the setup create wrote.
    let (status, read) = call(
        &router,
        "POST",
        "/getpermission",
        Some(&json!({"permissionArn": subject})),
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"the subject permission\": expected 200, got {status} with {read:?}"
    );
    let version = read
        .get("permission")
        .and_then(|permission| permission.get("version"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case \"the subject permission\": no version, got {read:?}"));
    assert_eq!(
        version, "1",
        "case \"the subject permission\": a refused version create must write nothing, got \
         {read:?}"
    );
}

/// A document that READS as a policy statement but breaks one of the template's own
/// rules is the same error class, reported with the rule it broke rather than with the
/// format sentence.
#[tokio::test]
async fn a_readable_statement_that_breaks_a_template_rule_says_which_rule() {
    let router = emulator();
    let cases = [
        (
            "an Effect that denies",
            json!({"Effect": "DENY", "Action": ["codebuild:ListProjects"]}).to_string(),
        ),
        (
            "an Effect that is neither allow nor deny",
            json!({"Effect": "Maybe", "Action": ["codebuild:ListProjects"]}).to_string(),
        ),
        (
            "an action of another service's namespace",
            json!({"Effect": "ALLOW", "Action": ["s3:GetObject"]}).to_string(),
        ),
        (
            "an action naming no namespace at all",
            json!({"Effect": "ALLOW", "Action": "*"}).to_string(),
        ),
    ];
    for (case, template) in cases {
        let (status, refused) = create_permission(
            &router,
            &json!({
                "name": "RuleRefused",
                "resourceType": RESOURCE_TYPE,
                "policyTemplate": template,
            }),
        )
        .await;
        assert_eq!(
            (status, error_code(&refused)),
            (400, "InvalidPolicyException".to_string()),
            "case {case:?}: expected 400 InvalidPolicyException, got {status} with {refused:?}"
        );
        let message = error_message(&refused);
        assert_ne!(
            message, INVALID_POLICY_FORMAT,
            "case {case:?}: a readable statement must not be reported as unreadable, got \
             {message:?}"
        );
        assert!(
            !message.is_empty(),
            "case {case:?}: the refusal must say which rule was broken, got {refused:?}"
        );
    }
}

/// The sentence a refusal of an action the resource type does not support carries:
/// fixed text and one variable part, the resource type the permission applies to.
fn action_not_allowed(resource_type: &str) -> String {
    format!(
        "InvalidPolicy: Input policy containing action that is not allowed for resource type \
         {resource_type}"
    )
}

/// A template allowing exactly `action`, which is either one action name or a list of
/// them — both forms `CreatePermission$policyTemplate` admits for the `Action` element.
fn template_allowing(action: &Value) -> String {
    json!({"Effect": "ALLOW", "Action": action}).to_string()
}

/// One row of the supported-action tables: the resource type a permission applies to,
/// and the `Action` element a write for it carries.
///
/// A row's actions are read against the resource type's own resource code, so a row may
/// name an action of that form (`<Verb><ResourceCode>`) whichever service owns the type.
struct ActionCase {
    case: &'static str,
    resource_type: &'static str,
    action: Value,
}

/// Rows whose `Action` names something the resource type does not support.
fn unsupported_action_cases() -> Vec<ActionCase> {
    vec![
        ActionCase {
            case: "a delete of a resource of the shared type",
            resource_type: "codebuild:Project",
            action: json!(["codebuild:DeleteProject"]),
        },
        ActionCase {
            case: "an update of a resource of the shared type",
            resource_type: "ssm-incidents:ResponsePlan",
            action: json!(["ssm-incidents:UpdateResponsePlan"]),
        },
        ActionCase {
            case: "a create of a resource of the shared type",
            resource_type: "resource-groups:Group",
            action: json!(["resource-groups:CreateGroup"]),
        },
        ActionCase {
            case: "a put over a resource of the shared type",
            resource_type: "glue:Catalog",
            action: json!(["glue:PutCatalog"]),
        },
        ActionCase {
            case: "a modify of a resource of the shared type",
            resource_type: "rds:Cluster",
            action: json!(["rds:ModifyCluster"]),
        },
        ActionCase {
            case: "a delete naming the shared type in the plural",
            resource_type: "codebuild:ReportGroup",
            action: json!(["codebuild:DeleteReportGroups"]),
        },
        ActionCase {
            case: "a delete of the shared type cased differently",
            resource_type: "cloudtrail:EventDataStore",
            action: json!(["cloudtrail:deleteeventdatastore"]),
        },
        ActionCase {
            case: "one action the type supports beside one it does not",
            resource_type: "codebuild:Project",
            action: json!(["codebuild:BatchGetProjects", "codebuild:DeleteProject"]),
        },
        ActionCase {
            case: "a delete of the shared type named as a bare string",
            resource_type: "network-firewall:FirewallPolicy",
            action: json!("network-firewall:DeleteFirewallPolicy"),
        },
        ActionCase {
            case: "an action of another service's namespace",
            resource_type: "codebuild:Project",
            action: json!(["s3:GetObject"]),
        },
        ActionCase {
            case: "an action of another service's namespace, cased differently",
            resource_type: "codebuild:Project",
            action: json!(["IAM:PassRole"]),
        },
        ActionCase {
            case: "an action naming no namespace at all",
            resource_type: "codebuild:Project",
            action: json!("*"),
        },
    ]
}

/// Rows whose `Action` names only actions the resource type does support.
fn supported_action_cases() -> Vec<ActionCase> {
    vec![
        ActionCase {
            case: "a read of the shared resource",
            resource_type: "glue:Table",
            action: json!(["glue:GetTable"]),
        },
        ActionCase {
            case: "a write of a resource BESIDE the shared one",
            resource_type: "glue:Database",
            action: json!(["glue:CreateTable", "glue:DeleteTable", "glue:UpdateTable"]),
        },
        ActionCase {
            case: "a create whose object merely starts with the shared type's code",
            resource_type: "vpc-lattice:ServiceNetwork",
            action: json!(["vpc-lattice:CreateServiceNetworkVpcAssociation"]),
        },
        ActionCase {
            case: "a use of the shared resource",
            resource_type: "codebuild:Project",
            action: json!(["codebuild:StartBuild"]),
        },
        ActionCase {
            case: "an association with the shared resource",
            resource_type: "route53resolver:ResolverRule",
            action: json!([
                "route53resolver:AssociateResolverRule",
                "route53resolver:DisassociateResolverRule",
            ]),
        },
        ActionCase {
            case: "a prefixed wildcard, which names no single action",
            resource_type: "imagebuilder:Image",
            action: json!(["imagebuilder:Get*"]),
        },
        ActionCase {
            case: "a whole-namespace wildcard",
            resource_type: "appmesh:Mesh",
            action: json!(["appmesh:*"]),
        },
        ActionCase {
            case: "one action named as a bare string",
            resource_type: "outposts:Site",
            action: json!("outposts:GetSite"),
        },
    ]
}

/// A permission of `resource_type` to write new versions of, created with a template
/// the type supports: a wildcard over its own service's namespace.
async fn version_subject(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    resource_type: &str,
) -> String {
    let service = resource_type
        .split(':')
        .next()
        .expect("every resource type names a service code");
    let (status, created) = create_permission(
        router,
        &json!({
            "name": name,
            "resourceType": resource_type,
            "policyTemplate": template_allowing(&json!([format!("{service}:*")])),
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the subject create expected 200, got {status} with {created:?}"
    );
    created_arn(case, &created)
}

/// The `permission` member of a read or a write, and one of its members as a string.
fn member(case: &str, body: &Value, name: &str) -> String {
    body.get("permission")
        .and_then(|permission| permission.get(name))
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: no permission {name}, got {body:?}"))
        .to_string()
}

async fn get_permission(router: &MultiServiceRouter, arn: &str) -> (u16, Value) {
    call(
        router,
        "POST",
        "/getpermission",
        Some(&json!({"permissionArn": arn})),
    )
    .await
}

#[tokio::test]
async fn an_action_the_resource_type_does_not_support_is_refused_by_both_writes() {
    let router = emulator();
    for (index, ActionCase { case, resource_type, action }) in
        unsupported_action_cases().into_iter().enumerate()
    {
        let expected = action_not_allowed(resource_type);
        let template = template_allowing(&action);

        let (status, refused) = create_permission(
            &router,
            &json!({
                "name": format!("Unsupported{index}"),
                "resourceType": resource_type,
                "policyTemplate": template,
            }),
        )
        .await;
        assert_eq!(
            (status, error_code(&refused)),
            (400, "InvalidPolicyException".to_string()),
            "case {case:?}: CreatePermission expected 400 InvalidPolicyException, got {status} \
             with {refused:?}"
        );
        assert_eq!(
            error_message(&refused),
            expected,
            "case {case:?}: CreatePermission expected the message {expected:?}, got {refused:?}"
        );
        // Nothing was stored: the name is still free, so the version subject below can
        // be created under its own name without clashing with a refused create.
        let (status, missing) = get_permission(
            &router,
            &format!("arn:aws:ram:{REGION}:{ACCOUNT}:permission/Unsupported{index}"),
        )
        .await;
        assert_eq!(
            (status, error_code(&missing)),
            (400, "UnknownResourceException".to_string()),
            "case {case:?}: a refused create must store nothing, got {status} with {missing:?}"
        );

        let subject = version_subject(
            &router,
            case,
            &format!("UnsupportedSubject{index}"),
            resource_type,
        )
        .await;
        let (status, refused) = create_permission_version(
            &router,
            &json!({"permissionArn": subject, "policyTemplate": template}),
        )
        .await;
        assert_eq!(
            (status, error_code(&refused)),
            (400, "InvalidPolicyException".to_string()),
            "case {case:?}: CreatePermissionVersion expected 400 InvalidPolicyException, got \
             {status} with {refused:?}"
        );
        assert_eq!(
            error_message(&refused),
            expected,
            "case {case:?}: CreatePermissionVersion expected the message {expected:?}, got \
             {refused:?}"
        );
        // The refused version create wrote nothing: the subject still carries the one
        // version its create wrote, and that version is still the default.
        let (status, read) = get_permission(&router, &subject).await;
        assert_eq!(
            status, 200,
            "case {case:?}: reading the subject back expected 200, got {status} with {read:?}"
        );
        assert_eq!(
            member(case, &read, "version"),
            "1",
            "case {case:?}: a refused version create must write no version, got {read:?}"
        );
    }
}

#[tokio::test]
async fn an_action_the_resource_type_supports_is_stored_by_both_writes() {
    let router = emulator();
    for (index, ActionCase { case, resource_type, action }) in
        supported_action_cases().into_iter().enumerate()
    {
        let template = template_allowing(&action);

        let (status, created) = create_permission(
            &router,
            &json!({
                "name": format!("Supported{index}"),
                "resourceType": resource_type,
                "policyTemplate": template,
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreatePermission expected 200, got {status} with {created:?}"
        );
        // The create reports a summary, which carries no policy document, so what was
        // stored is read back through `GetPermission`, which reports the detail.
        let stored = created_arn(case, &created);
        let (status, read) = get_permission(&router, &stored).await;
        assert_eq!(
            status, 200,
            "case {case:?}: reading the create back expected 200, got {status} with {read:?}"
        );
        assert_eq!(
            member(case, &read, "permission"),
            template,
            "case {case:?}: the create stored another document, got {read:?}"
        );

        // The same template as a new version of a permission of that type, read back
        // through `GetPermission`: the version the write reported is the default one.
        let subject = version_subject(
            &router,
            case,
            &format!("SupportedSubject{index}"),
            resource_type,
        )
        .await;
        let (status, written) = create_permission_version(
            &router,
            &json!({"permissionArn": subject, "policyTemplate": template}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreatePermissionVersion expected 200, got {status} with {written:?}"
        );
        assert_eq!(
            member(case, &written, "version"),
            "2",
            "case {case:?}: the version create reported another version, got {written:?}"
        );

        let (status, read) = get_permission(&router, &subject).await;
        assert_eq!(
            status, 200,
            "case {case:?}: reading the version back expected 200, got {status} with {read:?}"
        );
        assert_eq!(
            (
                member(case, &read, "version"),
                member(case, &read, "permission"),
            ),
            ("2".to_string(), template),
            "case {case:?}: the default version must be the one just written, got {read:?}"
        );
    }
}

#[tokio::test]
async fn every_action_a_published_permission_grants_is_supported_for_its_resource_type() {
    let router = emulator();
    // The invariant over the WHOLE published catalog rather than over the rows above:
    // the actions the permissions this service publishes for a resource type grant are
    // actions that type supports, so a customer managed permission may grant them too.
    // Resource types that take no customer managed permission are refused earlier, on
    // the resource type itself, so only the types that take one are driven here.
    for (index, entry) in AWS_MANAGED_PERMISSIONS.iter().enumerate() {
        let case = entry.name;
        if !resource_type_entry_of(entry.resource_type)
            .is_some_and(|known| known.customer_managed_permissions)
        {
            continue;
        }
        let template = template_allowing(&json!(entry.actions));
        let (status, created) = create_permission(
            &router,
            &json!({
                "name": format!("Published{index}"),
                "resourceType": entry.resource_type,
                "policyTemplate": template,
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: a template granting what {:?} grants expected 200, got {status} with \
             {created:?}",
            entry.resource_type
        );
        let (status, read) = get_permission(&router, &created_arn(case, &created)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: reading the create back expected 200, got {status} with {read:?}"
        );
        assert_eq!(
            member(case, &read, "permission"),
            template,
            "case {case:?}: the create stored another document, got {read:?}"
        );
    }
}
