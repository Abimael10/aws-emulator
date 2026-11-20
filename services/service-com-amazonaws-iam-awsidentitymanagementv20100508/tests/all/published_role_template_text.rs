// SPDX-License-Identifier: CC0-1.0

//! The TEXT a published role template reports, end to end over the awsQuery wire.
//!
//! RULE: a role template a service publishes is service-published content — `managedByTypeType`
//! declares the single value `Service` and no operation of this API writes one — so every
//! value a read of it reports that is not computed from the request is the publisher's OWN
//! published content: the template's `Description`, each parameter's `Description`
//! (`ParameterDefinition$Description`) and the `TemplateVersionId` identifying the published
//! version. Each of them belongs to the ENTRY, so two entries differ in them, nothing of them
//! is read off the template's name or the version the request addressed, and every read in
//! every run reports the same text — which is what lets a client show or diff what a template
//! grants, and pin the version it validated against.
//!
//! The suite is a TABLE whose rows differ along what the rule generalises over: which
//! published template is read (the IAM power-user template the issue cited and the AWS Backup
//! default service-role template it did not), which parameter of it carries the description,
//! how the version is addressed (at the template's default minor version and at the minor
//! version named explicitly) and how often it is read. The texts are written out here rather
//! than read back from the response or from the catalog's own constants, so a reworded entry
//! fails here instead of agreeing with itself.
//!
//! The awsQuery harness is the one `tests/all/user_lifecycle.rs` owns.

use smithy_server_runtime::MultiServiceRouter;

use crate::user_lifecycle::{call, element, elements, router};

/// The ARN each published template lives at, as `PublishedRoleTemplate::arn` builds one:
/// `arn:aws:iam::aws:role-template/<managing service>/<name>:<major version>`.
const POWER_USER_TEMPLATE_ARN: &str =
    "arn:aws:iam::aws:role-template/iam.amazonaws.com/PowerUserRoleTemplate:1";
const BACKUP_TEMPLATE_ARN: &str =
    "arn:aws:iam::aws:role-template/backup.amazonaws.com/AWSBackupDefaultServiceRoleTemplate:1";

/// The single minor version each published entry carries, which is also its default — so a
/// request that names it addresses the same version as one that names none.
const PUBLISHED_MINOR_VERSION: i32 = 0;

/// One published template's content: its ARN, the version id it is published under, the
/// description it publishes and the description it publishes for each of its parameters, by
/// parameter name.
///
/// SOURCE: the content each publishing service publishes for the template — the sentence it
/// describes the template with, the sentence it describes each parameter with, and the id of
/// the version it publishes.
struct Published {
    case: &'static str,
    template_arn: &'static str,
    template_version_id: &'static str,
    description: &'static str,
    parameters: &'static [(&'static str, &'static str)],
}

/// Every published template, with the text it reports.
const PUBLISHED: &[Published] = &[
    Published {
        case: "the IAM power-user role template",
        template_arn: POWER_USER_TEMPLATE_ARN,
        template_version_id: "AVTAZKAPJZG4N6BGA67NU",
        description: "Creates a role that grants PowerUser permissions to the calling AWS service, allowing it to create and manage most AWS resources on your behalf. Intended for services that require wide-ranging permissions. The trusted service is configurable via the AWSServiceName parameter.",
        parameters: &[
            ("AWSServiceName", "Service principal of the service"),
            ("RoleName", "Customized name for the role"),
        ],
    },
    Published {
        case: "the AWS Backup default service-role template",
        template_arn: BACKUP_TEMPLATE_ARN,
        template_version_id: "AVTA4KXZQ7JMW2PVNHLRD",
        description: "Standard service role the provides AWS Backup service permission to create and restore backups on your behalf across AWS services",
        parameters: &[
            ("RoleName", "Name of the role to create"),
            ("accountId", "Account that the service acts on behalf of"),
        ],
    },
];

/// One `GetRoleTemplateVersion` read, which must be served.
async fn read(
    router: &MultiServiceRouter,
    case: &str,
    template_arn: &str,
    minor_version: Option<i32>,
) -> String {
    let minor = minor_version.map(|minor| minor.to_string());
    let mut params: Vec<(&str, &str)> = vec![("TemplateArn", template_arn)];
    if let Some(minor) = minor.as_deref() {
        params.push(("MinorVersion", minor));
    }
    let (status, body) = call(router, "GetRoleTemplateVersion", &params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetRoleTemplateVersion must serve a published template, got {status}: \
         {body}"
    );
    body
}

/// The `<member>` blocks of the response's `ParametersDefinition` list, in document order, so
/// one parameter's members cannot be read as another's.
fn parameter_members(body: &str) -> Vec<String> {
    let opened = body
        .split("<ParametersDefinition>")
        .nth(1)
        .unwrap_or_else(|| panic!("the response must carry a ParametersDefinition: {body}"));
    let listed = opened
        .split("</ParametersDefinition>")
        .next()
        .unwrap_or_default();
    listed
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

/// RULE over the table: each published template reports the publisher's own description, the
/// publisher's own description per parameter and the id of the version it publishes — however
/// the version is addressed.
#[tokio::test]
async fn a_published_template_reports_the_publishers_own_text() {
    let router = router();

    // The ways one version is addressed: by the template's default, and by the minor version
    // named explicitly. The text must not depend on which was used.
    for addressed in [None, Some(PUBLISHED_MINOR_VERSION)] {
        for template in PUBLISHED {
            let case = format!("{} addressed by {addressed:?}", template.case);
            let body = read(&router, &case, template.template_arn, addressed).await;

            assert_eq!(
                element(&body, "TemplateVersionId"),
                template.template_version_id,
                "case {case:?}: expected the id of the published version {:?}, got {:?}",
                template.template_version_id,
                element(&body, "TemplateVersionId")
            );
            // The template's own `Description` precedes the parameter descriptions the same
            // tag names inside `ParametersDefinition`.
            let descriptions = elements(&body, "Description");
            assert_eq!(
                descriptions.first().map(String::as_str),
                Some(template.description),
                "case {case:?}: expected the published description {:?}, got {:?}",
                template.description,
                descriptions.first()
            );

            let members = parameter_members(&body);
            assert_eq!(
                members.len(),
                template.parameters.len(),
                "case {case:?}: expected one member per published parameter, got {members:?}"
            );
            for (member, (name, description)) in members.iter().zip(template.parameters) {
                let case = format!("{case} parameter {name}");
                assert_eq!(
                    element(member, "Name"),
                    *name,
                    "case {case:?}: expected the published parameters in order, got {:?}",
                    element(member, "Name")
                );
                assert_eq!(
                    element(member, "Description"),
                    *description,
                    "case {case:?}: expected the published description {description:?}, got \
                     {:?}",
                    element(member, "Description")
                );
            }
        }
    }
}

/// RULE, the two invariants that make the text the ENTRY's own: no two published entries
/// share a description, a version id or a parameter description for the same parameter name,
/// and none of the text spells the template's name or the version addressed.
#[tokio::test]
async fn published_text_belongs_to_the_entry_and_not_to_the_catalog() {
    let router = router();

    let mut descriptions: Vec<String> = Vec::new();
    let mut version_ids: Vec<String> = Vec::new();
    // One parameter name is published by BOTH entries, which is what makes this a test of the
    // text belonging to the entry rather than to the parameter name.
    let mut shared_parameter: Vec<String> = Vec::new();

    for template in PUBLISHED {
        let body = read(&router, template.case, template.template_arn, None).await;
        let description = elements(&body, "Description")
            .into_iter()
            .next()
            .unwrap_or_else(|| panic!("case {:?}: a description must be reported", template.case));
        let version_id = element(&body, "TemplateVersionId");
        let name = template
            .template_arn
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .split(':')
            .next()
            .unwrap_or_default();
        assert!(
            !version_id.to_lowercase().contains(&name.to_lowercase()),
            "case {:?}: the published id must carry none of the template's name, got \
             {version_id:?}",
            template.case
        );
        assert!(
            !description.contains("@{"),
            "case {:?}: a description is published prose rather than a pattern a creation \
             renders, got {description:?}",
            template.case
        );

        for member in parameter_members(&body) {
            if element(&member, "Name") == "RoleName" {
                shared_parameter.push(element(&member, "Description"));
            }
        }
        descriptions.push(description);
        version_ids.push(version_id);

        // Read twice: published content is the same for every reader and every read.
        let again = read(&router, template.case, template.template_arn, None).await;
        for tag in ["Description", "TemplateVersionId"] {
            assert_eq!(
                elements(&again, tag),
                elements(&body, tag),
                "case {:?}: a second read must report the same <{tag}>, got {:?}",
                template.case,
                elements(&again, tag)
            );
        }
    }

    for (member, mut values) in [
        ("Description", descriptions),
        ("TemplateVersionId", version_ids),
        ("the RoleName parameter's Description", shared_parameter),
    ] {
        let reported = values.len();
        assert!(
            reported > 1,
            "case {member:?}: the catalog must publish more than one entry to compare, got \
             {reported}"
        );
        values.sort();
        values.dedup();
        assert_eq!(
            values.len(),
            reported,
            "case {member:?}: expected each entry's own text, got {values:?} across {reported} \
             templates"
        );
    }
}
