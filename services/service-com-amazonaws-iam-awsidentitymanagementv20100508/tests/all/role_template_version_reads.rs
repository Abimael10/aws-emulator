// SPDX-License-Identifier: CC0-1.0

//! `GetRoleTemplateVersion`, end to end over the awsQuery wire.
//!
//! Every test drives a `MultiServiceRouter` holding this service over one shared
//! `InMemoryEngine`, exactly as the production router does, so what is asserted is the HTTP
//! behaviour (status, awsQuery error code, XML members) a client on the socket sees. The
//! awsQuery harness is the one `tests/all/user_lifecycle.rs` owns and the other suites in
//! this crate share; `seed` (a template written straight into the account's own store) and
//! `acquire` (the `ReplacementValues` map encoding) are the ones
//! `tests/all/role_template_acquisition.rs` owns.
//!
//! The rules being asserted:
//!
//! * a read reports the role template version the service RESOLVES for the ARN and minor
//!   version the request names — the templates the account recorded itself first, and
//!   otherwise the ones a service publishes — and "if you do not specify a minor version,
//!   the service returns the template's default minor version"
//!   (`GetRoleTemplateVersion$MinorVersion`);
//! * that resolution is the ONE resolution, so the version a read reports is the version a
//!   role is created from;
//! * the version is reported as held, a disabled template included: this operation models
//!   no refusal for one, and refusing an acquisition from it is `AcquireRole`'s half;
//! * a published version is reported from the publishing service's own catalog data — the
//!   opaque version id in IAM's unique-id format, and per parameter its description, its
//!   published default, whether a caller must supply one and whether it can be changed
//!   afterwards;
//! * an ARN no template resolves under names a missing TEMPLATE and a minor version a
//!   resolved template does not carry names a missing VERSION of a template that exists:
//!   both reference "a resource entity that does not exist", so both are refused
//!   `NoSuchEntityException` (HTTP 404, awsQuery code `NoSuchEntity`), each with the sentence
//!   measured for that absence;
//! * a `TemplateArn` whose FORM is not that of a role template's ARN is a value IAM cannot
//!   accept rather than a template that happens to be missing (HTTP 400, awsQuery code
//!   `ValidationError`), worded with the sentence measured for the CLASS of defect it carries
//!   — nothing readable as an ARN, an ARN naming a role template under an account rather than
//!   the `aws` publisher, or any other departure from the form;
//! * IAM decides the call against the caller's identity-based policies before it resolves
//!   the template;
//! * the read is a READ: it leaves the account's templates exactly as it found them.
//!
//! Each suite is a TABLE whose rows differ along what those rules generalise over: whether
//! the template is one the account recorded or one a service publishes, which minor version
//! the request names and whether it names one, whether the template declares a default,
//! which half of the enablement is off, the ARN's shape and both published length bounds of
//! `arnType` (20..2048), and what the caller is permitted.
//!
//! A role template is PUBLISHED by a service — `managedByTypeType` declares the single value
//! `Service` — and no operation of this API creates one, so the rows that need an account's
//! own template seed it directly, as `tests/all/role_template_acquisition.rs` does.

use service_com_amazonaws_iam_awsidentitymanagementv20100508::ops::types::{
    InlinePolicy, ParameterDefinition, RoleTemplateVersion, TagTemplate,
};
use service_com_amazonaws_iam_awsidentitymanagementv20100508::state;

use crate::identity_policy_enforcement::{principal, statement};
use crate::role_template_acquisition::{acquire, seed};
use crate::user_lifecycle::{
    account_id, assert_access_denied_message, call, call_signed_by, element, elements, error_code,
    router_with_engine,
};

/// The published bounds of `arnType`, the type `TemplateArn` is carried through: "len
/// 20..2048" (`smithy_query(query="operation", operation="GetRoleTemplateVersion")`).
const MIN_ARN_CHARACTERS: usize = 20;
const MAX_ARN_CHARACTERS: usize = 2048;

/// The ARN the account's own fixture template is recorded under, in the form every role
/// template's ARN takes:
/// `arn:<partition>:iam::aws:role-template/<publishing service>/<name>:<major version>`.
const RECORDED_ARN: &str =
    "arn:aws:iam::aws:role-template/deploy.amazonaws.com/ReadFixture:1";

/// The publishing service and the name the fixture template's ARN spells, so a row that
/// builds another ARN of the same shape builds it from the same parts.
const RECORDED_PUBLISHER: &str = "deploy.amazonaws.com";

/// The minor version the fixture template declares as its default, with two other minor
/// versions recorded beside it so "the template's default minor version" is a choice rather
/// than the only version held.
const RECORDED_DEFAULT_MINOR_VERSION: i32 = 3;
const RECORDED_MINOR_VERSIONS: &[i32] = &[1, 2, RECORDED_DEFAULT_MINOR_VERSION];

/// The managed policy the fixture template attaches, and the boundary it sets: AWS-managed
/// policies, which belong to AWS rather than to this account and so need no fixture.
const RECORDED_MANAGED_POLICY: &str = "arn:aws:iam::aws:policy/ReadOnlyAccess";
const RECORDED_BOUNDARY: &str = "arn:aws:iam::aws:policy/PowerUserAccess";

/// The maximum session duration the fixture template states, inside the published range of
/// `roleMaxSessionDurationType` (3600..43200).
const RECORDED_MAX_SESSION_DURATION: i32 = 7_200;

/// The ARNs the two role templates this emulator's catalog publishes live at, as
/// `PublishedRoleTemplate::arn` builds one:
/// `arn:aws:iam::aws:role-template/<managing service>/<name>:<major version>`.
///
/// SOURCE: `state::PUBLISHED_ROLE_TEMPLATES`, the catalog every account can create roles
/// from. The rows below name the AWS Backup default service role template — which the issue
/// behind this suite did not cite — beside the IAM power-user one.
const BACKUP_TEMPLATE_ARN: &str =
    "arn:aws:iam::aws:role-template/backup.amazonaws.com/AWSBackupDefaultServiceRoleTemplate:1";
const POWER_USER_TEMPLATE_ARN: &str =
    "arn:aws:iam::aws:role-template/iam.amazonaws.com/PowerUserRoleTemplate:1";

/// The single minor version each catalog entry carries, which is also its default.
const PUBLISHED_MINOR_VERSION: i32 = 0;

/// The three sentences IAM refuses a value whose form is not a role template's ARN with, one
/// per CLASS of defect, written out here rather than read from the implementation's constants:
/// what this suite pins is the SENTENCES, so a reworded constant has to fail here rather than
/// agree with itself.
const TEMPLATE_ARN_NOT_VALID: &str =
    "The specified role template ARN is not valid. Expected format is: \
     arn:{partition}:iam::aws:role-template/{servicePrincipal}/{templateName}:{majorVersion} \
     (majorVersion must be a positive integer with no leading zeros).";

/// What a value nothing can be read as an ARN from is refused with: IAM's own
/// unparseable-ARN sentence, whose empty value slot renders between the template's two
/// spaces.
const ARN_NOT_PARSEABLE: &str = "Invalid ARN:  Could not be parsed!";

/// What an ARN naming a role template under an account rather than the `aws` publisher is
/// refused with — fixed text, the example ARN included, quoting neither the account nor the
/// value.
const TEMPLATE_ARN_NOT_PUBLISHED_BY_AWS: &str =
    "The specified role template ARN is not valid. The ARN must use 'aws' as the account \
     field (e.g., arn:aws:iam::aws:role-template/...).";

/// AWS's sentence for an ARN that resolves no role template at all: the
/// `<Entity> … cannot be found.` form, with the template named by the ARN the request
/// supplied.
fn template_not_found(template_arn: &str) -> String {
    format!("Role Template {template_arn} cannot be found.")
}

/// AWS's sentence for a template that resolves but carries no version the request
/// addresses: the `<entity> not found: <identifier>` form, with the template named by the
/// ARN the request supplied.
fn template_version_not_found(template_arn: &str) -> String {
    format!("Role template not found: {template_arn}")
}

/// The `<member>` blocks of the `ParametersDefinition` list the body carries, in document
/// order, so each parameter's own members can be read without a member absent from one
/// parameter shifting another's position.
fn parameter_members(body: &str) -> Vec<String> {
    section(body, "ParametersDefinition")
        .split("<member>")
        .skip(1)
        .map(|member| {
            member
                .split("</member>")
                .next()
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

/// The trust policy template the fixture carries, with a placeholder in it so the rendering
/// the round-trip suite drives is observable.
fn trust_policy_template() -> String {
    format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Principal":{{"AWS":"arn:aws:iam::{}:root"}},"Action":"sts:AssumeRole","Condition":{{"StringEquals":{{"sts:ExternalId":"@{{Team}}"}}}}}}]}}"#,
        account_id()
    )
}

/// The fixture role template version, at the minor version `minor_version`, declaring
/// `default_minor_version` as its default (or none at all).
fn recorded_template(minor_version: i32, default_minor_version: Option<i32>) -> RoleTemplateVersion
{
    RoleTemplateVersion {
        template_arn: Some(RECORDED_ARN.to_string()),
        template_name: Some("ReadFixture".to_string()),
        template_version_id: Some(format!("roletemplateversionread{minor_version}")),
        description: Some("read fixture for a recorded role template".to_string()),
        major_version: Some(1),
        default_minor_version,
        managed_by_type: Some("Service".to_string()),
        managed_by_value: Some("deploy.amazonaws.com".to_string()),
        enabled: true,
        minor_version: Some(minor_version),
        role_name_pattern: Some(format!("Partner-@{{Team}}-v{minor_version}")),
        role_path_pattern: Some("/partners/@{Team}/".to_string()),
        role_description_pattern: Some("Deployment role for @{Team} in @{Regions}".to_string()),
        assume_role_policy_document_template: Some(trust_policy_template()),
        inline_policy_templates: Some(vec![InlinePolicy {
            policy_name: "@{Team}-drop".to_string(),
            policy_document:
                r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"s3:PutObject","Resource":"arn:aws:s3:::drop-@{Team}/*"}]}"#
                    .to_string(),
        }]),
        managed_policy_arns: Some(vec![RECORDED_MANAGED_POLICY.to_string()]),
        permission_boundary_arn: Some(RECORDED_BOUNDARY.to_string()),
        parameters_definition: Some(vec![
            ParameterDefinition {
                name: "Team".to_string(),
                r#type: "String".to_string(),
                sub_type: None,
                description: Some("the team the role is created for".to_string()),
                is_required: true,
                default_value: None,
                immutable: true,
            },
            ParameterDefinition {
                name: "Regions".to_string(),
                r#type: "StringList".to_string(),
                sub_type: None,
                description: Some("the Regions the role is deployed in".to_string()),
                is_required: false,
                default_value: Some("us-east-1".to_string()),
                immutable: false,
            },
        ]),
        role_tags_template: Some(vec![TagTemplate {
            key: "Team".to_string(),
            value: "@{Team}".to_string(),
        }]),
        max_session_duration: Some(RECORDED_MAX_SESSION_DURATION),
        version_enabled: true,
        create_timestamp: Some(state::now()),
        update_timestamp: Some(state::now()),
    }
}

/// Seed the account's own fixture template: every minor version in `RECORDED_MINOR_VERSIONS`,
/// each declaring `RECORDED_DEFAULT_MINOR_VERSION` as the template's default.
fn seed_recorded(engine: &engine::InMemoryEngine) {
    for minor_version in RECORDED_MINOR_VERSIONS {
        seed(
            engine,
            RECORDED_ARN,
            &recorded_template(*minor_version, Some(RECORDED_DEFAULT_MINOR_VERSION)),
        );
    }
}

/// One `GetRoleTemplateVersion` call: the template ARN, and the minor version where a row
/// names one.
async fn read(
    router: &smithy_server_runtime::MultiServiceRouter,
    akid: Option<&str>,
    template_arn: &str,
    minor_version: Option<i32>,
) -> (u16, String) {
    let minor = minor_version.map(|minor| minor.to_string());
    let mut params: Vec<(&str, &str)> = vec![("TemplateArn", template_arn)];
    if let Some(minor) = minor.as_deref() {
        params.push(("MinorVersion", minor));
    }
    call_signed_by(router, akid, "GetRoleTemplateVersion", &params).await
}

/// The inner XML of the single `<tag>` element the body carries, so a list of SCALARS can be
/// read with [`elements`] without the structure lists elsewhere in the response being
/// mistaken for its members.
fn section(body: &str, tag: &str) -> String {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = body
        .find(&open)
        .unwrap_or_else(|| panic!("<{tag}> is missing from {body}"));
    let rest = &body[start + open.len()..];
    let end = rest
        .find(&close)
        .unwrap_or_else(|| panic!("<{tag}> is never closed in {body}"));
    rest[..end].to_string()
}

/// The templates the account holds, rendered, so a read can be shown to leave them alone.
fn held_templates(engine: &engine::InMemoryEngine) -> String {
    let store = state::service_state(engine).expect("the IAM store");
    format!("{:?}", store.lock().role_templates)
}

/// One row of the reporting table: the template the request addresses, the minor version it
/// names, and the version the resolution must report.
struct Reported {
    case: &'static str,
    /// Whether the account's own fixture template is seeded for this row.
    seeded: bool,
    template_arn: &'static str,
    minor_version: Option<i32>,
    template_name: &'static str,
    managed_by_value: &'static str,
    reported_minor_version: i32,
    default_minor_version: i32,
    role_name_pattern: String,
    role_path_pattern: &'static str,
    max_session_duration: i32,
    managed_policy_arns: Vec<&'static str>,
    /// The parameter names the version defines, in the order it carries them.
    parameters: Vec<&'static str>,
}

/// RULE: a read reports the role template version the service resolves for the ARN and minor
/// version the request names — a template the account recorded first, otherwise one a service
/// publishes — and a request that names no minor version addresses the template's default.
///
/// Rows differ along what the resolution generalises over: a recorded template addressed at
/// its default and at a named non-default version, and each published template of the
/// catalog, addressed with and without a minor version.
#[tokio::test]
async fn a_read_reports_the_role_template_version_the_service_resolves() {
    let rows = vec![
        Reported {
            case: "a recorded template, no minor version named, so its default",
            seeded: true,
            template_arn: RECORDED_ARN,
            minor_version: None,
            template_name: "ReadFixture",
            managed_by_value: "deploy.amazonaws.com",
            reported_minor_version: RECORDED_DEFAULT_MINOR_VERSION,
            default_minor_version: RECORDED_DEFAULT_MINOR_VERSION,
            role_name_pattern: format!("Partner-@{{Team}}-v{RECORDED_DEFAULT_MINOR_VERSION}"),
            role_path_pattern: "/partners/@{Team}/",
            max_session_duration: RECORDED_MAX_SESSION_DURATION,
            managed_policy_arns: vec![RECORDED_MANAGED_POLICY],
            parameters: vec!["Team", "Regions"],
        },
        Reported {
            case: "a recorded template at the non-default minor version the request names",
            seeded: true,
            template_arn: RECORDED_ARN,
            minor_version: Some(1),
            template_name: "ReadFixture",
            managed_by_value: "deploy.amazonaws.com",
            reported_minor_version: 1,
            default_minor_version: RECORDED_DEFAULT_MINOR_VERSION,
            role_name_pattern: "Partner-@{Team}-v1".to_string(),
            role_path_pattern: "/partners/@{Team}/",
            max_session_duration: RECORDED_MAX_SESSION_DURATION,
            managed_policy_arns: vec![RECORDED_MANAGED_POLICY],
            parameters: vec!["Team", "Regions"],
        },
        Reported {
            case: "the published AWS Backup template, in an account that recorded nothing",
            seeded: false,
            template_arn: BACKUP_TEMPLATE_ARN,
            minor_version: None,
            template_name: "AWSBackupDefaultServiceRoleTemplate",
            managed_by_value: "backup.amazonaws.com",
            reported_minor_version: PUBLISHED_MINOR_VERSION,
            default_minor_version: PUBLISHED_MINOR_VERSION,
            role_name_pattern: "@{RoleName}".to_string(),
            role_path_pattern: "/service-role/",
            max_session_duration: 3600,
            managed_policy_arns: vec![
                "arn:aws:iam::aws:policy/service-role/AWSBackupServiceRolePolicyForBackup",
                "arn:aws:iam::aws:policy/service-role/AWSBackupServiceRolePolicyForRestores",
            ],
            parameters: vec!["RoleName", "accountId"],
        },
        Reported {
            case: "the published power-user template at the minor version the request names",
            seeded: false,
            template_arn: POWER_USER_TEMPLATE_ARN,
            minor_version: Some(PUBLISHED_MINOR_VERSION),
            template_name: "PowerUserRoleTemplate",
            managed_by_value: "iam.amazonaws.com",
            reported_minor_version: PUBLISHED_MINOR_VERSION,
            default_minor_version: PUBLISHED_MINOR_VERSION,
            role_name_pattern: "@{RoleName}".to_string(),
            role_path_pattern: "/",
            max_session_duration: 3600,
            managed_policy_arns: vec!["arn:aws:iam::aws:policy/PowerUserAccess"],
            parameters: vec!["AWSServiceName", "RoleName"],
        },
    ];

    for row in rows {
        let (router, engine) = router_with_engine();
        if row.seeded {
            seed_recorded(&engine);
        }

        let (status, body) = read(&router, None, row.template_arn, row.minor_version).await;
        assert_eq!(
            status, 200,
            "case {:?}: expected the version to be reported, got {status}: {body}",
            row.case
        );
        for (tag, expected) in [
            ("TemplateArn", row.template_arn.to_string()),
            ("TemplateName", row.template_name.to_string()),
            ("ManagedByType", "Service".to_string()),
            ("ManagedByValue", row.managed_by_value.to_string()),
            ("MinorVersion", row.reported_minor_version.to_string()),
            (
                "DefaultMinorVersion",
                row.default_minor_version.to_string(),
            ),
            ("RoleNamePattern", row.role_name_pattern.clone()),
            ("RolePathPattern", row.role_path_pattern.to_string()),
            (
                "MaxSessionDuration",
                row.max_session_duration.to_string(),
            ),
            ("Enabled", "true".to_string()),
            ("VersionEnabled", "true".to_string()),
        ] {
            assert_eq!(
                element(&body, tag),
                expected,
                "case {:?}: expected <{tag}> to report {expected:?}, got {:?}",
                row.case,
                element(&body, tag)
            );
        }
        assert_eq!(
            elements(&section(&body, "ManagedPolicyArns"), "member"),
            row.managed_policy_arns,
            "case {:?}: expected the template's managed policies {:?}, got {:?}",
            row.case,
            row.managed_policy_arns,
            elements(&section(&body, "ManagedPolicyArns"), "member")
        );
        assert_eq!(
            elements(&section(&body, "ParametersDefinition"), "Name"),
            row.parameters,
            "case {:?}: expected the parameters {:?}, got {:?}",
            row.case,
            row.parameters,
            elements(&section(&body, "ParametersDefinition"), "Name")
        );
        // A version the catalog assigns an identifier to reports one of the modelled
        // `idType` (len 16..128, `^[\w]+$`), so a client can tell two versions apart.
        let version_id = element(&body, "TemplateVersionId");
        assert!(
            (16..=128).contains(&version_id.chars().count())
                && version_id
                    .chars()
                    .all(|character| character.is_alphanumeric() || character == '_'),
            "case {:?}: expected a TemplateVersionId of the modelled idType, got {version_id:?}",
            row.case
        );
    }
}

/// RULE: a template whose versions declare no default is addressed at the highest minor
/// version held — the only version a single-version template has, and the latest of several.
#[tokio::test]
async fn a_template_declaring_no_default_is_addressed_at_its_highest_minor_version() {
    let (router, engine) = router_with_engine();
    for minor_version in [4, 9, 7] {
        seed(&engine, RECORDED_ARN, &recorded_template(minor_version, None));
    }

    let (status, body) = read(&router, None, RECORDED_ARN, None).await;
    assert_eq!(
        status, 200,
        "a template with no declared default must still be addressed, got {status}: {body}"
    );
    assert_eq!(
        element(&body, "MinorVersion"),
        "9",
        "expected the highest held minor version, got {:?}",
        element(&body, "MinorVersion")
    );
}

/// RULE: the resolution a read uses is the ONE resolution, so the version a read reports is
/// the version a role created from that template takes its configuration from.
///
/// The expected role is DERIVED from what the read reported: each pattern it answered with
/// has its `@{parameter}` placeholders replaced with the values the acquisition supplies —
/// "the pattern can include `@{parameter}` placeholders that are replaced with the values you
/// supply in the `ReplacementValues` parameter of AcquireRole"
/// (`RoleTemplateVersion$RoleNamePattern`) — so a read answering from another version of the
/// template would fail these assertions.
///
/// Rows differ along where the template comes from: one the account recorded (whose name
/// pattern carries its own minor version, so the row also pins WHICH version both calls
/// resolved) and one a service publishes.
#[tokio::test]
async fn the_version_a_read_reports_is_the_version_a_role_is_created_from() {
    /// One row: where the template comes from, the replacement values the acquisition
    /// supplies, and the placeholder substitutions those values make in the patterns the
    /// read reported.
    struct RoundTrip {
        case: &'static str,
        /// Whether the account's own fixture template is seeded for this row.
        seeded: bool,
        template_arn: &'static str,
        values: &'static [(&'static str, &'static [&'static str])],
        substitutions: &'static [(&'static str, &'static str)],
    }

    let rows = [
        RoundTrip {
            case: "a recorded template",
            seeded: true,
            template_arn: RECORDED_ARN,
            values: &[("Team", &["Alpha"])],
            substitutions: &[("@{Team}", "Alpha")],
        },
        RoundTrip {
            case: "the published power-user template",
            seeded: false,
            template_arn: POWER_USER_TEMPLATE_ARN,
            values: &[
                ("AWSServiceName", &["lambda.amazonaws.com"]),
                ("RoleName", &["read-roundtrip"]),
            ],
            substitutions: &[("@{RoleName}", "read-roundtrip")],
        },
    ];

    for row in rows {
        let RoundTrip {
            case,
            seeded,
            template_arn,
            values,
            substitutions,
        } = row;
        let (router, engine) = router_with_engine();
        if seeded {
            seed_recorded(&engine);
        }

        let (status, reported) = read(&router, None, template_arn, None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the version to be reported, got {status}: {reported}"
        );
        let rendered = |pattern: String| {
            substitutions
                .iter()
                .fold(pattern, |rendered, (placeholder, value)| {
                    rendered.replace(placeholder, value)
                })
        };
        let expected_name = rendered(element(&reported, "RoleNamePattern"));
        let expected_path = rendered(element(&reported, "RolePathPattern"));
        let max_session_duration = element(&reported, "MaxSessionDuration");

        let (status, created) = acquire(&router, None, template_arn, None, values).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the role to be created, got {status}: {created}"
        );
        assert_eq!(
            element(&created, "RoleName"),
            expected_name,
            "case {case:?}: expected the name rendered from the pattern the read reported \
             {expected_name:?}, got {:?}",
            element(&created, "RoleName")
        );

        let (status, role) = call(&router, "GetRole", &[("RoleName", &expected_name)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the created role must be readable, got {status}: {role}"
        );
        assert_eq!(
            element(&role, "Path"),
            expected_path,
            "case {case:?}: expected the role at the path rendered from what the read \
             reported {expected_path:?}, got {:?}",
            element(&role, "Path")
        );
        assert_eq!(
            element(&role, "MaxSessionDuration"),
            max_session_duration,
            "case {case:?}: expected the maximum session duration the read reported \
             {max_session_duration:?}, got {:?}",
            element(&role, "MaxSessionDuration")
        );
    }
}

/// RULE: the version is reported AS HELD, a disabled template and a disabled minor version
/// included — this operation models no refusal for one, and reporting it is how a caller
/// learns it is disabled, while refusing an acquisition from it is `AcquireRole`'s half.
///
/// Rows differ along which half of the enablement is off: the template, the minor version,
/// and both.
#[tokio::test]
async fn a_disabled_template_is_reported_rather_than_refused() {
    let rows = [
        ("the template is disabled", false, true),
        ("this minor version is disabled", true, false),
        ("both are disabled", false, false),
    ];

    for (case, enabled, version_enabled) in rows {
        let (router, engine) = router_with_engine();
        let mut version = recorded_template(RECORDED_DEFAULT_MINOR_VERSION, None);
        version.enabled = enabled;
        version.version_enabled = version_enabled;
        seed(&engine, RECORDED_ARN, &version);

        let (status, body) = read(&router, None, RECORDED_ARN, None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected a disabled template to be reported, got {status}: {body}"
        );
        assert_eq!(
            element(&body, "Enabled"),
            enabled.to_string(),
            "case {case:?}: expected <Enabled> to report {enabled}, got {:?}",
            element(&body, "Enabled")
        );
        assert_eq!(
            element(&body, "VersionEnabled"),
            version_enabled.to_string(),
            "case {case:?}: expected <VersionEnabled> to report {version_enabled}, got {:?}",
            element(&body, "VersionEnabled")
        );

        // The contrast: creating FROM it is what the disablement refuses.
        let (status, refused) =
            acquire(&router, None, RECORDED_ARN, None, &[("Team", &["Alpha"])]).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the acquisition to be refused, got {status}: {refused}"
        );
        assert_eq!(
            error_code(&refused),
            "RoleTemplateDisabled",
            "case {case:?}: expected RoleTemplateDisabled, got {:?}",
            error_code(&refused)
        );
    }
}

/// RULE: an ARN no role template resolves under names a resource that does not exist, so the
/// call is refused `NoSuchEntityException` (HTTP 404, awsQuery code `NoSuchEntity`) with the
/// sentence naming the template that cannot be found.
///
/// Rows span the ARN's shape — including the published MAXIMUM length of `arnType`
/// (20..2048) — the recorded template's own publisher and the published catalog, each
/// addressed at a name, a case and a major version nothing resolves.
#[tokio::test]
async fn an_arn_no_template_resolves_under_is_refused_as_a_missing_entity() {
    let name_padding = MAX_ARN_CHARACTERS
        - format!("arn:aws:iam::aws:role-template/{RECORDED_PUBLISHER}/:1").chars().count();
    let longest = format!(
        "arn:aws:iam::aws:role-template/{RECORDED_PUBLISHER}/{}:1",
        "L".repeat(name_padding)
    );
    assert_eq!(
        longest.chars().count(),
        MAX_ARN_CHARACTERS,
        "the longest row must sit on the published maximum, got {} characters",
        longest.chars().count()
    );

    let rows: Vec<(&str, String)> = vec![
        ("the published maximum length", longest),
        (
            "another template of the same publisher",
            format!("arn:aws:iam::aws:role-template/{RECORDED_PUBLISHER}/NeverRecorded:1"),
        ),
        (
            "a template of a publisher nothing publishes for",
            "arn:aws:iam::aws:role-template/nowhere.amazonaws.com/ReadFixture:1".to_string(),
        ),
        (
            "the recorded template's ARN in another partition",
            format!("arn:aws-us-gov:iam::aws:role-template/{RECORDED_PUBLISHER}/ReadFixture:1"),
        ),
        (
            "the recorded template's ARN in another case",
            format!("arn:aws:iam::aws:role-template/{RECORDED_PUBLISHER}/readfixture:1"),
        ),
        (
            "the recorded template at another major version",
            format!("arn:aws:iam::aws:role-template/{RECORDED_PUBLISHER}/ReadFixture:2"),
        ),
        (
            "a published template addressed at another major version",
            "arn:aws:iam::aws:role-template/iam.amazonaws.com/PowerUserRoleTemplate:2".to_string(),
        ),
    ];

    for (case, template_arn) in rows {
        let (router, engine) = router_with_engine();
        seed_recorded(&engine);

        let (status, body) = read(&router, None, &template_arn, None).await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected the missing template to be refused, got {status}: {body}"
        );
        assert_eq!(
            error_code(&body),
            "NoSuchEntity",
            "case {case:?}: expected NoSuchEntity, got {:?}: {body}",
            error_code(&body)
        );
        let expected = template_not_found(&template_arn);
        assert_eq!(
            element(&body, "Message"),
            expected,
            "case {case:?}: expected the refusal to read {expected:?}, got {:?}",
            element(&body, "Message")
        );
    }
}

/// RULE: a minor version a resolved role template does not carry is a different missing
/// resource from a template nothing resolves — the version of a template that DOES exist —
/// so the call is refused `NoSuchEntityException` (HTTP 404, awsQuery code `NoSuchEntity`)
/// with the sentence for a version that was not found, naming the template the request
/// addressed.
///
/// Rows span where the resolved template comes from — the account's own record and each
/// published catalog entry — and which version is missing: one well past the versions held,
/// the one just past the highest, and the one just below the published entry's only version.
#[tokio::test]
async fn a_minor_version_a_resolved_template_does_not_carry_is_refused_as_missing() {
    let rows: Vec<(&str, &str, i32)> = vec![
        (
            "a minor version the recorded template does not carry",
            RECORDED_ARN,
            7,
        ),
        (
            "the minor version just past the recorded template's highest",
            RECORDED_ARN,
            RECORDED_DEFAULT_MINOR_VERSION + 1,
        ),
        (
            "a minor version the published AWS Backup template does not carry",
            BACKUP_TEMPLATE_ARN,
            PUBLISHED_MINOR_VERSION + 1,
        ),
        (
            "a minor version the published power-user template does not carry",
            POWER_USER_TEMPLATE_ARN,
            PUBLISHED_MINOR_VERSION + 9,
        ),
    ];

    for (case, template_arn, minor_version) in rows {
        let (router, engine) = router_with_engine();
        seed_recorded(&engine);

        let (status, body) = read(&router, None, template_arn, Some(minor_version)).await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected the missing version to be refused, got {status}: {body}"
        );
        assert_eq!(
            error_code(&body),
            "NoSuchEntity",
            "case {case:?}: expected NoSuchEntity, got {:?}: {body}",
            error_code(&body)
        );
        let expected = template_version_not_found(template_arn);
        assert_eq!(
            element(&body, "Message"),
            expected,
            "case {case:?}: expected the refusal to read {expected:?}, got {:?}",
            element(&body, "Message")
        );
    }
}

/// RULE: a value supplied for a member documented as the ARN of a role template is read for
/// its FORM before anything is resolved, and a value whose form is not that ARN is one IAM
/// cannot accept rather than a template that happens to be missing: HTTP 400 with the
/// awsQuery code `ValidationError` — the code IAM answers a value it cannot accept with — and
/// the sentence measured for the CLASS of defect the value carries, so a caller can tell the
/// classes apart: a value nothing can be read as an ARN from answers IAM's unparseable-ARN
/// sentence, an ARN naming a role template of IAM's under an account rather than the `aws`
/// publisher answers the namespace sentence, and anything else answers the sentence stating
/// the whole form.
///
/// Rows span every way the form can be wrong, each paired with the sentence its class earns:
/// a value nothing can be read as an ARN from (no ARN at all, colons alone, too few fields),
/// an ARN of another service, an ARN of another resource type in IAM's own namespace (at the
/// published MINIMUM length of `arnType`, 20), a role-template ARN whose account field is not
/// the publisher `aws` (an account number, another account's number, no account at all, and
/// another publisher's name), one that names a Region, and a role-template ARN whose identity
/// is not `<service principal>/<template name>:<major version>` — a single segment, no major
/// version, an empty part, and a major version that is not a positive integer without
/// leading zeros — with one row carrying two defects at once, which pins which class is
/// reported when a value is both in an account's namespace and of the wrong shape.
#[tokio::test]
async fn a_template_arn_whose_form_is_not_a_role_templates_is_refused_as_a_bad_value() {
    let shortest = "arn:aws:iam:::rt/aaa".to_string();
    assert_eq!(
        shortest.chars().count(),
        MIN_ARN_CHARACTERS,
        "the shortest row must sit on the published minimum, got {shortest:?}"
    );

    let rows = [
        (
            "a value that is no ARN at all",
            "not-an-arn-but-long-enough".to_string(),
            ARN_NOT_PARSEABLE,
        ),
        (
            "a value of colons alone",
            "::::::::::::::::::::".to_string(),
            ARN_NOT_PARSEABLE,
        ),
        (
            "a value carrying fewer fields than an ARN has",
            "arn:aws:iam:us-east-1".to_string(),
            ARN_NOT_PARSEABLE,
        ),
        (
            "a well-formed ARN of another service",
            "arn:aws:s3:::role-template/ReadFixture".to_string(),
            TEMPLATE_ARN_NOT_VALID,
        ),
        (
            "a well-formed ARN of another identity service",
            format!("arn:aws:sts::aws:role-template/{RECORDED_PUBLISHER}/ReadFixture:1"),
            TEMPLATE_ARN_NOT_VALID,
        ),
        (
            "an ARN of another resource type in IAM's own namespace",
            "arn:aws:iam::aws:policy/ReadOnlyAccess".to_string(),
            TEMPLATE_ARN_NOT_VALID,
        ),
        (
            "an ARN naming an IAM role rather than a role template",
            format!("arn:aws:iam::{}:role/ReadFixture", account_id()),
            TEMPLATE_ARN_NOT_VALID,
        ),
        (
            "another resource type at the published minimum length",
            shortest,
            TEMPLATE_ARN_NOT_VALID,
        ),
        (
            "a role-template ARN carrying this account's number",
            format!(
                "arn:aws:iam::{}:role-template/{RECORDED_PUBLISHER}/ReadFixture:1",
                account_id()
            ),
            TEMPLATE_ARN_NOT_PUBLISHED_BY_AWS,
        ),
        (
            "a role-template ARN carrying another account's number",
            format!("arn:aws:iam::000000000000:role-template/{RECORDED_PUBLISHER}/ReadFixture:1"),
            TEMPLATE_ARN_NOT_PUBLISHED_BY_AWS,
        ),
        // Both wrong at once: the namespace is read first, so a value that is in an account's
        // namespace AND of the wrong shape is told about the field that put it in the wrong
        // namespace rather than about the shape.
        (
            "a role-template ARN of this account that also names no major version",
            format!(
                "arn:aws:iam:us-east-1:{}:role-template/{RECORDED_PUBLISHER}/ReadFixture",
                account_id()
            ),
            TEMPLATE_ARN_NOT_PUBLISHED_BY_AWS,
        ),
        (
            "a role-template ARN naming no account at all",
            format!("arn:aws:iam:::role-template/{RECORDED_PUBLISHER}/ReadFixture:1"),
            TEMPLATE_ARN_NOT_PUBLISHED_BY_AWS,
        ),
        (
            "a role-template ARN whose account field names another publisher",
            format!("arn:aws:iam::amazonaws:role-template/{RECORDED_PUBLISHER}/ReadFixture:1"),
            TEMPLATE_ARN_NOT_PUBLISHED_BY_AWS,
        ),
        (
            "a role-template ARN naming a Region",
            format!("arn:aws:iam:us-east-1:aws:role-template/{RECORDED_PUBLISHER}/ReadFixture:1"),
            TEMPLATE_ARN_NOT_VALID,
        ),
        (
            "a role-template ARN of a single trailing segment",
            "arn:aws:iam::aws:role-template/nonexistent-role-template".to_string(),
            TEMPLATE_ARN_NOT_VALID,
        ),
        (
            "a role-template ARN naming no major version",
            format!("arn:aws:iam::aws:role-template/{RECORDED_PUBLISHER}/ReadFixture"),
            TEMPLATE_ARN_NOT_VALID,
        ),
        (
            "a role-template ARN naming no template",
            format!("arn:aws:iam::aws:role-template/{RECORDED_PUBLISHER}/:1"),
            TEMPLATE_ARN_NOT_VALID,
        ),
        (
            "a role-template ARN naming no publishing service",
            "arn:aws:iam::aws:role-template//ReadFixture:1".to_string(),
            TEMPLATE_ARN_NOT_VALID,
        ),
        (
            "a role-template ARN carrying a further segment",
            format!("arn:aws:iam::aws:role-template/{RECORDED_PUBLISHER}/team/ReadFixture:1"),
            TEMPLATE_ARN_NOT_VALID,
        ),
        (
            "a major version of zero",
            format!("arn:aws:iam::aws:role-template/{RECORDED_PUBLISHER}/ReadFixture:0"),
            TEMPLATE_ARN_NOT_VALID,
        ),
        (
            "a major version with a leading zero",
            format!("arn:aws:iam::aws:role-template/{RECORDED_PUBLISHER}/ReadFixture:01"),
            TEMPLATE_ARN_NOT_VALID,
        ),
        (
            "a negative major version",
            format!("arn:aws:iam::aws:role-template/{RECORDED_PUBLISHER}/ReadFixture:-1"),
            TEMPLATE_ARN_NOT_VALID,
        ),
        (
            "a major version that is no number",
            format!("arn:aws:iam::aws:role-template/{RECORDED_PUBLISHER}/ReadFixture:v1"),
            TEMPLATE_ARN_NOT_VALID,
        ),
        (
            "an empty major version",
            format!("arn:aws:iam::aws:role-template/{RECORDED_PUBLISHER}/ReadFixture:"),
            TEMPLATE_ARN_NOT_VALID,
        ),
    ];

    for (case, template_arn, expected) in rows {
        let (router, engine) = router_with_engine();
        seed_recorded(&engine);

        let (status, body) = read(&router, None, &template_arn, None).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the bad value to be refused, got {status}: {body}"
        );
        assert_eq!(
            error_code(&body),
            "ValidationError",
            "case {case:?}: expected ValidationError, got {:?}: {body}",
            error_code(&body)
        );
        assert_eq!(
            element(&body, "Message"),
            expected,
            "case {case:?}: expected the refusal to read {expected:?}, got {:?}",
            element(&body, "Message")
        );
    }
}

/// RULE: a published role template version is reported from the publishing service's OWN
/// catalog data — the opaque unique id of the version, minted in IAM's unique-id format, and
/// per parameter its description, the value the service fills it with where it publishes one,
/// whether a caller must supply one and whether the value can be changed afterwards — rather
/// than from values derived from the template's name or stamped on every entry alike.
///
/// Rows are EVERY entry of the published catalog (`state::PUBLISHED_ROLE_TEMPLATES`), so an
/// entry added later is held to the same rule, and each entry's every parameter is checked
/// against the catalog row the publisher states.
#[tokio::test]
async fn a_published_version_reports_the_publishers_own_catalog_data() {
    /// The characters an IAM unique id's suffix is drawn from: base32, as the uppercase
    /// `AROA`/`ANPA`/`AIDA` ids this service mints are.
    const BASE32: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    /// How many of them follow the prefix in an id of this family.
    const SUFFIX_CHARACTERS: usize = 17;

    let (router, _engine) = router_with_engine();
    let mut reported_ids = Vec::new();

    // The one parameter description AWS's own wording is published for, pinned as a literal
    // beside the catalog comparison below, so a reworded entry fails here rather than
    // agreeing with itself.
    let (status, power_user) = read(&router, None, POWER_USER_TEMPLATE_ARN, None).await;
    assert_eq!(
        status, 200,
        "expected the published power-user version to be reported, got {status}: {power_user}"
    );
    let principal_parameter = parameter_members(&power_user)
        .into_iter()
        .find(|member| element(member, "Name") == "AWSServiceName")
        .expect("the published power-user template defines the service-principal parameter");
    assert_eq!(
        element(&principal_parameter, "Description"),
        "Service principal of the service",
        "expected the publisher's own wording for the service-principal parameter, got {:?}",
        element(&principal_parameter, "Description")
    );

    for template in state::PUBLISHED_ROLE_TEMPLATES {
        let case = format!("{}/{}", template.managed_by_value, template.template_name);
        let template_arn = template.arn();
        let (status, body) = read(&router, None, &template_arn, None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the published version to be reported, got {status}: {body}"
        );

        // The version id: IAM's unique-id format, carrying none of the template's own text,
        // so a client that validates or persists it is handling the kind of value IAM hands
        // it everywhere else.
        let version_id = element(&body, "TemplateVersionId");
        let suffix = version_id
            .strip_prefix(state::ROLE_TEMPLATE_VERSION_ID_PREFIX)
            .unwrap_or_default();
        assert!(
            suffix.chars().count() == SUFFIX_CHARACTERS
                && suffix.chars().all(|character| BASE32.contains(character)),
            "case {case:?}: expected a {} id of {SUFFIX_CHARACTERS} base32 characters, got \
             {version_id:?}",
            state::ROLE_TEMPLATE_VERSION_ID_PREFIX
        );
        assert!(
            !version_id
                .to_lowercase()
                .contains(&template.template_name.to_lowercase()),
            "case {case:?}: expected an opaque id carrying none of the template's name, got \
             {version_id:?}"
        );
        reported_ids.push(version_id);

        // The version's own `Description`, which precedes the parameter descriptions the
        // same tag names inside `ParametersDefinition`.
        let descriptions = elements(&body, "Description");
        assert_eq!(
            descriptions.first().map(String::as_str),
            Some(template.description),
            "case {case:?}: expected the publisher's description {:?}, got {:?}",
            template.description,
            descriptions.first()
        );

        let members = parameter_members(&body);
        assert_eq!(
            members.len(),
            template.parameters.len(),
            "case {case:?}: expected one member per published parameter, got {members:?}"
        );
        for (member, parameter) in members.iter().zip(template.parameters) {
            let case = format!("{case} parameter {}", parameter.name);
            assert_eq!(
                element(member, "Name"),
                parameter.name,
                "case {case:?}: expected the parameters in the order the catalog carries \
                 them, got {:?}",
                element(member, "Name")
            );
            assert_eq!(
                element(member, "Description"),
                parameter.description,
                "case {case:?}: expected the publisher's description {:?}, got {:?}",
                parameter.description,
                element(member, "Description")
            );
            assert_eq!(
                element(member, "IsRequired"),
                parameter.is_required.to_string(),
                "case {case:?}: expected IsRequired {}, got {:?}",
                parameter.is_required,
                element(member, "IsRequired")
            );
            assert_eq!(
                element(member, "Immutable"),
                parameter.immutable.to_string(),
                "case {case:?}: expected Immutable {}, got {:?}",
                parameter.immutable,
                element(member, "Immutable")
            );
            assert_eq!(
                elements(member, "DefaultValue"),
                parameter
                    .default_value
                    .map(str::to_string)
                    .into_iter()
                    .collect::<Vec<_>>(),
                "case {case:?}: expected the published default {:?}, got {:?}",
                parameter.default_value,
                elements(member, "DefaultValue")
            );
            // The invariant between the two members: a parameter the service fills from a
            // published default is never one a caller must supply.
            assert!(
                parameter.default_value.is_none() || !parameter.is_required,
                "case {case:?}: a parameter with a published default must not be required"
            );
        }
    }

    reported_ids.sort();
    let distinct = reported_ids.len();
    reported_ids.dedup();
    assert_eq!(
        reported_ids.len(),
        distinct,
        "every published version must carry its own id, got {reported_ids:?}"
    );
}

/// RULE: the metadata a read reports is the metadata the acquisition is held to, so a
/// parameter the read reports as not required, with a published default, is one `AcquireRole`
/// renders with that default when the request supplies no value for it.
#[tokio::test]
async fn a_parameter_the_read_reports_a_default_for_is_rendered_with_it() {
    let (router, _engine) = router_with_engine();

    let (status, reported) = read(&router, None, POWER_USER_TEMPLATE_ARN, None).await;
    assert_eq!(
        status, 200,
        "expected the published version to be reported, got {status}: {reported}"
    );
    let defaulted = parameter_members(&reported)
        .into_iter()
        .find(|member| element(member, "IsRequired") == "false")
        .expect("the published power-user template defines a parameter with a default");
    let name = element(&defaulted, "Name");
    let default_value = element(&defaulted, "DefaultValue");

    // Only the parameters the read reports as required are supplied, so the rendering has
    // nothing but the published default to fill this one with.
    let (status, created) = acquire(
        &router,
        None,
        POWER_USER_TEMPLATE_ARN,
        None,
        &[("AWSServiceName", &["lambda.amazonaws.com"])],
    )
    .await;
    assert_eq!(
        status, 200,
        "expected the role to be created from the published defaults, got {status}: {created}"
    );
    let expected_name = element(&reported, "RoleNamePattern").replace(
        &format!("@{{{name}}}"),
        &default_value,
    );
    assert_eq!(
        element(&created, "RoleName"),
        expected_name,
        "expected the name rendered with the published default {expected_name:?}, got {:?}",
        element(&created, "RoleName")
    );
}

/// RULE: IAM decides the call against the caller's identity-based policies before it
/// resolves the template, so a caller no policy allows the action for is refused HTTP 403
/// `AccessDenied` naming the principal, the action and the TEMPLATE the request addressed —
/// whether or not the account holds that template. The same call under a statement that
/// names its action is served.
#[tokio::test]
async fn a_caller_whose_policies_do_not_allow_the_read_is_refused() {
    let rows = vec![
        (
            "no allow at all",
            vec![(
                "unrelated".to_string(),
                statement("Allow", "iam:GetRole", "*"),
            )],
            "because no identity-based policy allows the iam:GetRoleTemplateVersion action"
                .to_string(),
        ),
        (
            "an explicit deny over the allow",
            vec![
                (
                    "allow".to_string(),
                    statement("Allow", "iam:GetRoleTemplateVersion", "*"),
                ),
                (
                    "deny".to_string(),
                    statement("Deny", "iam:GetRoleTemplateVersion", "*"),
                ),
            ],
            "with an explicit deny in an identity-based policy".to_string(),
        ),
    ];

    for (row, (case, policies, reason)) in rows.into_iter().enumerate() {
        let (router, engine) = router_with_engine();
        seed_recorded(&engine);
        let user_name = format!("reader{row}");
        let access_key_id = principal(&router, case, &user_name, &policies).await;

        // Both a template the account holds and one it does not: a refused caller learns
        // which action it may not take, never which templates the account holds.
        let unresolved =
            format!("arn:aws:iam::aws:role-template/{RECORDED_PUBLISHER}/NeverRecorded:1");
        for template_arn in [RECORDED_ARN, unresolved.as_str()] {
            let case = format!("{case} reading {template_arn}");
            let (status, body) = read(&router, Some(&access_key_id), template_arn, None).await;
            assert_eq!(
                status, 403,
                "case {case:?}: expected 403, got {status}: {body}"
            );
            assert_eq!(
                error_code(&body),
                "AccessDenied",
                "case {case:?}: expected AccessDenied, got {:?}: {body}",
                error_code(&body)
            );
            assert_access_denied_message(
                &case,
                &element(&body, "Message"),
                &format!("arn:aws:iam::{}:user/{user_name}", account_id()),
                "iam:GetRoleTemplateVersion",
                &format!("role template {template_arn}"),
                &reason,
            );
        }
    }

    let (router, engine) = router_with_engine();
    seed_recorded(&engine);
    let access_key_id = principal(
        &router,
        "a granted caller",
        "reader-granted",
        &[(
            "read".to_string(),
            statement("Allow", "iam:GetRoleTemplateVersion", RECORDED_ARN),
        )],
    )
    .await;
    let (status, body) = read(&router, Some(&access_key_id), RECORDED_ARN, None).await;
    assert_eq!(
        status, 200,
        "a caller a statement allows the action for must be served, got {status}: {body}"
    );
    assert_eq!(
        element(&body, "MinorVersion"),
        RECORDED_DEFAULT_MINOR_VERSION.to_string(),
        "expected the granted read to report the default minor version, got {:?}",
        element(&body, "MinorVersion")
    );
}

/// RULE: the operation is a READ — it reports the version and changes nothing a later caller
/// sees, so the account's templates are exactly what they were and a second read answers the
/// same version.
///
/// Rows differ along which template is read and whether the request names a minor version,
/// so the invariant is asserted over the class rather than over one call.
#[tokio::test]
async fn reading_a_template_version_changes_nothing_a_later_caller_sees() {
    let rows: Vec<(&str, &str, Option<i32>)> = vec![
        ("a recorded template at its default", RECORDED_ARN, None),
        ("a recorded template at a named version", RECORDED_ARN, Some(1)),
        (
            "the published AWS Backup template",
            BACKUP_TEMPLATE_ARN,
            None,
        ),
        (
            "an ARN no template resolves under",
            "arn:aws:iam::aws:role-template/nowhere.amazonaws.com/NeverRecorded:1",
            None,
        ),
        (
            "a value whose form is not a role template's ARN",
            "arn:aws:iam::aws:role-template/nonexistent-role-template",
            None,
        ),
    ];

    for (case, template_arn, minor_version) in rows {
        let (router, engine) = router_with_engine();
        seed_recorded(&engine);
        let before = held_templates(&engine);

        let (first_status, first) = read(&router, None, template_arn, minor_version).await;
        let after = held_templates(&engine);
        assert_eq!(
            after, before,
            "case {case:?}: a read must leave the account's templates alone, got {after:?}"
        );

        let (second_status, second) = read(&router, None, template_arn, minor_version).await;
        assert_eq!(
            second_status, first_status,
            "case {case:?}: a second read must answer the same status, got {second_status}: \
             {second}"
        );
        for tag in ["MinorVersion", "TemplateArn", "Message"] {
            assert_eq!(
                elements(&second, tag),
                elements(&first, tag),
                "case {case:?}: a second read must report the same <{tag}>, got {:?}",
                elements(&second, tag)
            );
        }
    }
}
