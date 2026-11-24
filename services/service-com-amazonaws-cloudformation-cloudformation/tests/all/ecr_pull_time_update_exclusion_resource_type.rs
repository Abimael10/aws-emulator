// SPDX-License-Identifier: CC0-1.0

//! `AWS::ECR::PullTimeUpdateExclusion` is a resource type a template may DECLARE, and the
//! principal a stack declares is REGISTERED on the private Amazon ECR registry's
//! pull-time-update exclusion list.
//!
//! The rule under test: **a resource type Amazon Web Services publishes belongs to the
//! catalogue a submitted template is held to, so a body declaring it is admitted and never
//! answered `Template format error: Unrecognized resource types: [<types>]`; a deployment of
//! such a type then calls the owning service's own create API once per declared resource, so
//! the resource EXISTS there under the physical id the stack reports and is removed again with
//! the stack.** Both halves are read here because this crate's test router mounts Amazon ECR
//! beside CloudFormation on one engine, which is how a stack reaches that registry at all.
//!
//! The exclusion is the IAM principal it names: `RegisterPullTimeUpdateExclusion` takes that
//! `principalArn` and no identifier of its own, the deregister addresses the record by the
//! same ARN, and `ListPullTimeUpdateExclusions` is the only read of them that registry
//! publishes — so the declared `PrincipalArn` is the resource's physical id, and the listing
//! is where a client finds what the stack registered.
//!
//! The rows differ along what the rule generalises over: WHICH principal the declaration names
//! (a role, a user, a role under an IAM path, a name built from the punctuation the modelled
//! pattern admits, a principal of another partition), the template spelling (JSON and YAML),
//! how many exclusions one body declares and whether the declaration stands beside a resource
//! of another type — plus the two verdicts Amazon ECR can give a register.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cloudformation_cloudformation::state::early_validation::{
    already_exists, validation_failure_summary,
};
use service_com_amazonaws_cloudformation_cloudformation::state::resource_types::{
    is_published, is_recognized, AWS_RESOURCE_TYPES,
};
use service_com_amazonaws_cloudformation_cloudformation::state::template::{
    name_property_candidates, published_attribute_names, ECR_PULL_TIME_UPDATE_EXCLUSION,
};
use service_com_amazonaws_cloudformation_cloudformation::state::{
    primary_identifier, resource_status, status,
};

use crate::harness::{
    failed_resource_reason, physical_resource_id, settled_stack, settled_stack_status,
    stack_own_events,
};
use crate::provisioned_ecr_repository::ecr;
use crate::stack_lifecycle::{
    call, create_stack, member, router, xml_blocks, xml_text, ACCOUNT_ID, REGION,
};

/// The fixed form of the refusal a type CloudFormation does not know is answered with, which
/// no row here may ever see.
const UNRECOGNIZED_PREFIX: &str = "Template format error: Unrecognized resource types: [";

/// The type's ONE property, which is the member `RegisterPullTimeUpdateExclusion` takes
/// (`smithy_query(query="operation", operation="RegisterPullTimeUpdateExclusion")`).
const NAME_PROPERTY: &str = "PrincipalArn";

/// The member `ListPullTimeUpdateExclusions` answers the registry's exclusion list in.
const REPORTED_EXCLUSIONS: &str = "pullTimeUpdateExclusions";

/// The logical id every template below declares its first exclusion under.
const EXCLUSION_LOGICAL_ID: &str = "Exclusion";

/// The IAM principals the rows name, each inside `PrincipalArn`'s modelled pattern
/// `^arn:aws(-[a-z]+)*:iam::[0-9]{12}:(role|user)/[\w+=,.@-]+(/[\w+=,.@-]+)*$`
/// (`smithy_query(query="operation", operation="RegisterPullTimeUpdateExclusion")`): a plain
/// role, a user, a role under an IAM path, a role name built from the punctuation that
/// character class admits, and a role of another partition, which the `aws(-[a-z]+)*` field
/// spells.
fn role(name: &str) -> String {
    format!("arn:aws:iam::{ACCOUNT_ID}:role/{name}")
}

fn user(name: &str) -> String {
    format!("arn:aws:iam::{ACCOUNT_ID}:user/{name}")
}

/// A template declaring ONE exclusion for `principal`, in JSON, publishing its `Ref`.
fn one_exclusion(principal: &str) -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {EXCLUSION_LOGICAL_ID: {
            "Type": ECR_PULL_TIME_UPDATE_EXCLUSION,
            "Properties": {NAME_PROPERTY: principal},
        }},
        "Outputs": {"Excluded": {"Value": {"Ref": EXCLUSION_LOGICAL_ID}}},
    })
    .to_string()
}

/// The exclusion list the registry reports, which is the whole of it: the list is bounded at
/// `PullTimeUpdateExclusionList`'s own `@length(max: 100)` and a page carries up to that many,
/// so one read answers every entry.
async fn registered(router: &MultiServiceRouter, case: &str) -> Vec<String> {
    let (code, body) = ecr(router, "ListPullTimeUpdateExclusions", &json!({})).await;
    assert_eq!(
        code, 200,
        "case {case:?}: ListPullTimeUpdateExclusions must answer 200, got {code}: {body}"
    );
    body[REPORTED_EXCLUSIONS]
        .as_array()
        .map(|listed| {
            listed
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// The reasons of the validation RESULTS `DescribeEvents` reports for `stack`: the per-resource
/// diagnosis a stage that ran before provisioning recorded, which is the detail the stack's own
/// summary points a client at.
async fn validation_reasons(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
) -> Vec<String> {
    let (code, body) = call(router, "DescribeEvents", &[member("StackName", stack)]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: DescribeEvents({stack}) must answer 200, got {code} {body}"
    );
    xml_blocks(&body, "member")
        .iter()
        .filter(|event| xml_text(event, "ValidationName").is_some())
        .map(|event| xml_text(event, "ValidationStatusReason").unwrap_or_default())
        .collect()
}

/// Assert one answer admitted the body rather than refusing its resource types.
fn assert_admitted(case: &str, code: u16, body: &str) {
    let message = xml_text(body, "Message").unwrap_or_default();
    assert!(
        !message.starts_with(UNRECOGNIZED_PREFIX),
        "case {case:?}: expected a type Amazon Web Services publishes to be recognised, got \
         {message:?}"
    );
    assert_eq!(
        code, 200,
        "case {case:?}: expected HTTP 200 for a body declaring a published type, got {code} \
         with body {body}"
    );
}

/// The catalogue entries the rule rests on: the type is published and recognised, a near-miss
/// spelling of it still is not, and the type is identified by the principal it excludes.
#[test]
fn the_catalogue_publishes_the_pull_time_update_exclusion_type_and_how_it_is_identified() {
    assert!(
        AWS_RESOURCE_TYPES.contains(&ECR_PULL_TIME_UPDATE_EXCLUSION),
        "expected {ECR_PULL_TIME_UPDATE_EXCLUSION:?} in the published catalogue, it is absent"
    );
    assert!(
        is_published(ECR_PULL_TIME_UPDATE_EXCLUSION)
            && is_recognized(ECR_PULL_TIME_UPDATE_EXCLUSION),
        "expected {ECR_PULL_TIME_UPDATE_EXCLUSION:?} to be a type a template may declare, it \
         is not"
    );
    // The catalogue is a list of names, not a shape test: a name it does not carry stays
    // unrecognised however close it is to one it does. Each of these is a spelling AWS's
    // resource reference does not publish — a plural of the type, a dropped word, and the
    // ECR Public namespace, which publishes no exclusion type at all.
    for misspelled in [
        "AWS::ECR::PullTimeUpdateExclusions",
        "AWS::ECR::PullTimeExclusion",
        "AWS::ECRPublic::PullTimeUpdateExclusion",
    ] {
        assert!(
            !is_recognized(misspelled),
            "expected {misspelled:?} to stay unrecognised, it is recognised"
        );
    }
    // The principal IS the resource, so it is the property a client would name an existing
    // exclusion by, and the value the type's `Ref` returns.
    assert_eq!(
        primary_identifier::properties(ECR_PULL_TIME_UPDATE_EXCLUSION),
        vec![NAME_PROPERTY.to_string()],
        "expected the exclusion to be identified by its principal, got {:?}",
        primary_identifier::properties(ECR_PULL_TIME_UPDATE_EXCLUSION)
    );
    assert_eq!(
        name_property_candidates(ECR_PULL_TIME_UPDATE_EXCLUSION)
            .first()
            .map(String::as_str),
        Some(NAME_PROPERTY),
        "expected the name to be read from {NAME_PROPERTY:?}, got {:?}",
        name_property_candidates(ECR_PULL_TIME_UPDATE_EXCLUSION)
    );
    // The type publishes no `Fn::GetAtt` attribute at all — its `Ref` is the whole of what a
    // stack reports for one.
    assert!(
        published_attribute_names(ECR_PULL_TIME_UPDATE_EXCLUSION).is_empty(),
        "expected the type to publish no Fn::GetAtt attribute, got {:?}",
        published_attribute_names(ECR_PULL_TIME_UPDATE_EXCLUSION)
    );
}

/// RULE (admission): every operation that submits a template for deployment admits a body
/// declaring the type, and so does the operation whose whole answer is a verdict on the body.
#[tokio::test]
async fn every_operation_that_takes_a_template_admits_a_pull_time_update_exclusion() {
    // (case, the stack name, the template)
    let rows: Vec<(&str, &str, String)> = vec![
        (
            "a role principal, with Ref read back",
            "ecr-exclusion-admitted",
            one_exclusion(&role("BuildRunner")),
        ),
        (
            "a user principal",
            "ecr-exclusion-user",
            one_exclusion(&user("release-bot")),
        ),
        (
            "a role under an IAM path",
            "ecr-exclusion-path",
            one_exclusion(&role("platform/ci/Deployer")),
        ),
        (
            // The punctuation `[\w+=,.@-]` admits, which an IAM role name may carry.
            "a role name built from the punctuation the pattern admits",
            "ecr-exclusion-punctuation",
            one_exclusion(&role("team+ops=eu,build.1@corp-x")),
        ),
        (
            "the YAML spelling, a principal of another partition",
            "ecr-exclusion-yaml",
            format!(
                "\
Resources:
  {EXCLUSION_LOGICAL_ID}:
    Type: {ECR_PULL_TIME_UPDATE_EXCLUSION}
    Properties:
      {NAME_PROPERTY}: arn:aws-cn:iam::{ACCOUNT_ID}:role/Deployer
Outputs:
  Excluded:
    Value: !Ref {EXCLUSION_LOGICAL_ID}
"
            ),
        ),
        (
            "two exclusions beside a resource of another type",
            "ecr-exclusion-mixed",
            json!({
                "Resources": {
                    EXCLUSION_LOGICAL_ID: {
                        "Type": ECR_PULL_TIME_UPDATE_EXCLUSION,
                        "Properties": {NAME_PROPERTY: role("FirstRunner")},
                    },
                    "Second": {
                        "Type": ECR_PULL_TIME_UPDATE_EXCLUSION,
                        "Properties": {NAME_PROPERTY: user("second-runner")},
                    },
                    "Images": {
                        "Type": "AWS::ECR::Repository",
                        "Properties": {"RepositoryName": "exclusion-mixed-images"},
                    },
                },
            })
            .to_string(),
        ),
    ];

    for (case, stack_name, template) in rows {
        let router = router();
        let (code, body) = create_stack(&router, stack_name, &template, &[]).await;
        assert_admitted(case, code, &body);
        let stack_id = xml_text(&body, "StackId").unwrap_or_default();
        assert!(
            stack_id.contains(":stack/"),
            "case {case:?}: expected CreateStack to answer a stack id, got {body}"
        );

        let (code, body) = call(
            &router,
            "ValidateTemplate",
            &[member("TemplateBody", &template)],
        )
        .await;
        assert_admitted(case, code, &body);

        let (code, body) = call(
            &router,
            "CreateChangeSet",
            &[
                member("StackName", &format!("{stack_name}-change-set")),
                member("ChangeSetName", "a-change-set"),
                member("ChangeSetType", "CREATE"),
                member("TemplateBody", &template),
            ],
        )
        .await;
        assert_admitted(case, code, &body);
    }
}

/// RULE (provisioning): the deployment registers every declared principal on the registry's
/// exclusion list, reports that principal as the resource's physical id, and the stack's own
/// deletion deregisters it again.
#[tokio::test]
async fn a_declared_exclusion_is_registered_in_amazon_ecr_and_removed_with_the_stack() {
    // (case, the template, every principal it must register)
    let rows: Vec<(&str, String, Vec<String>)> = vec![
        (
            "a role principal",
            one_exclusion(&role("BuildRunner")),
            vec![role("BuildRunner")],
        ),
        (
            "a user principal",
            one_exclusion(&user("release-bot")),
            vec![user("release-bot")],
        ),
        (
            "a role under an IAM path",
            one_exclusion(&role("platform/ci/Deployer")),
            vec![role("platform/ci/Deployer")],
        ),
        (
            "a role name built from the punctuation the pattern admits",
            one_exclusion(&role("team+ops=eu,build.1@corp-x")),
            vec![role("team+ops=eu,build.1@corp-x")],
        ),
        (
            "the YAML spelling, a principal of another partition",
            format!(
                "\
Resources:
  {EXCLUSION_LOGICAL_ID}:
    Type: {ECR_PULL_TIME_UPDATE_EXCLUSION}
    Properties:
      {NAME_PROPERTY}: arn:aws-cn:iam::{ACCOUNT_ID}:role/Deployer
"
            ),
            vec![format!("arn:aws-cn:iam::{ACCOUNT_ID}:role/Deployer")],
        ),
        (
            "two exclusions in one body, beside a repository of the same registry",
            json!({
                "Resources": {
                    EXCLUSION_LOGICAL_ID: {
                        "Type": ECR_PULL_TIME_UPDATE_EXCLUSION,
                        "Properties": {NAME_PROPERTY: role("FirstRunner")},
                    },
                    "Second": {
                        "Type": ECR_PULL_TIME_UPDATE_EXCLUSION,
                        "Properties": {NAME_PROPERTY: user("second-runner")},
                    },
                    "Images": {
                        "Type": "AWS::ECR::Repository",
                        "Properties": {"RepositoryName": "exclusion-estate-images"},
                    },
                },
            })
            .to_string(),
            vec![role("FirstRunner"), user("second-runner")],
        ),
    ];

    for (index, (case, template, expected)) in rows.iter().enumerate() {
        let router = router();
        let stack = format!("ecr-exclusion-estate-{index}");
        let (code, body) = create_stack(&router, &stack, template, &[]).await;
        assert_eq!(
            code, 200,
            "case {case:?}: CreateStack({stack}) answered {code}: {body}"
        );
        let stack_id = xml_text(&body, "StackId").unwrap_or_default();
        let settled = settled_stack_status(case, &router, REGION, &stack).await;
        assert_eq!(
            settled, status::CREATE_COMPLETE,
            "case {case:?}: expected the create to complete, got {settled:?}"
        );

        // The principal the template declared IS the resource's physical id, which is the
        // handle every call of that registry on the exclusion takes.
        let reported =
            physical_resource_id(&router, case, &stack, EXCLUSION_LOGICAL_ID).await;
        assert_eq!(
            reported, expected[0],
            "case {case:?}: expected the declared principal as the physical id, got \
             {reported:?}"
        );

        // The registry really holds every declared principal: this is Amazon ECR's own read
        // of its exclusion list, not CloudFormation's record of the declaration.
        let mut held = registered(&router, case).await;
        held.sort();
        let mut wanted = expected.clone();
        wanted.sort();
        assert_eq!(
            held, wanted,
            "case {case:?}: expected Amazon ECR to exclude exactly {wanted:?}, got {held:?}"
        );

        let (code, body) = call(&router, "DeleteStack", &[member("StackName", &stack)]).await;
        assert_eq!(
            code, 200,
            "case {case:?}: DeleteStack({stack}) answered {code}: {body}"
        );
        let deleted = settled_stack(case, &router, REGION, &stack_id).await;
        assert_eq!(
            xml_text(&deleted, "StackStatus").as_deref(),
            Some(status::DELETE_COMPLETE),
            "case {case:?}: expected the deletion to complete, got {deleted}"
        );
        let after = registered(&router, case).await;
        assert!(
            after.is_empty(),
            "case {case:?}: expected the registry to exclude nothing once the stack that \
             registered {wanted:?} is gone, got {after:?}"
        );
    }
}

/// RULE (refusal before provisioning): a declaration whose principal the registry ALREADY
/// excludes is a resource whose create could not be carried out, so CloudFormation's own
/// pre-deployment existence check refuses the deployment — the stack reaches `CREATE_FAILED`
/// with the validation summary, the resource's own validation result names the identifier that
/// is taken, and nothing of the registry's exclusion list is touched.
#[tokio::test]
async fn a_principal_the_registry_already_excludes_fails_the_deployment() {
    // The rows differ along the FORM of the principal the registry already holds — a role and
    // a user — because what decides the refusal is that the owning service holds the
    // identifier, not how the identifier reads.
    for (index, principal) in [role("AlreadyExcluded"), user("already-excluded")]
        .iter()
        .enumerate()
    {
        let case = format!("a principal the registry already excludes: {principal}");
        let case = case.as_str();
        let router = router();
        let (code, body) = ecr(
            &router,
            "RegisterPullTimeUpdateExclusion",
            &json!({"principalArn": principal}),
        )
        .await;
        assert_eq!(
            code, 200,
            "case {case:?}: the registry must accept the first register, got {code}: {body}"
        );

        let stack = format!("ecr-exclusion-taken-{index}");
        let (code, body) = create_stack(&router, &stack, &one_exclusion(principal), &[]).await;
        assert_eq!(
            code, 200,
            "case {case:?}: CreateStack({stack}) answered {code}: {body}"
        );
        settled_stack_status(case, &router, REGION, &stack).await;

        // The stack's own account of the operation: the deployment was refused before
        // anything was provisioned, which CloudFormation reports as the summary over the
        // stage's errors on the stack's own `CREATE_FAILED`.
        let own = stack_own_events(&router, case, &stack).await;
        let failed: Vec<&str> = own
            .iter()
            .filter(|event| event.resource_status == status::CREATE_FAILED)
            .map(|event| event.reason.as_str())
            .collect();
        assert_eq!(
            failed,
            vec![validation_failure_summary(1).as_str()],
            "case {case:?}: expected the stack's CREATE_FAILED to carry the validation \
             summary, got {own:?}"
        );
        // ...and the detail of that one error, recorded against the resource the check was
        // applied to: which identifier of which type is already there.
        let failures = validation_reasons(&router, case, &stack).await;
        assert!(
            failures.contains(&already_exists(ECR_PULL_TIME_UPDATE_EXCLUSION, principal)),
            "case {case:?}: expected the existence check's own diagnosis of {principal:?}, \
             got {failures:?}"
        );
        // Nothing was asked of the registry, so its exclusion list is exactly what it was.
        let held = registered(&router, case).await;
        assert_eq!(
            held,
            vec![principal.clone()],
            "case {case:?}: expected the registry to still exclude exactly {principal:?}, \
             got {held:?}"
        );
    }
}

/// RULE (refusal at provisioning): an exclusion Amazon ECR REFUSES is not registered, so the
/// deployment fails and the failing resource's event carries that registry's own refusal under
/// the handler class its error code falls in.
#[tokio::test]
async fn an_exclusion_amazon_ecr_refuses_fails_the_deployment() {
    // `PrincipalArn`'s modelled pattern admits a `role/…` and a `user/…` path and nothing
    // else, so each of these is a principal the registry will not take: an IAM GROUP, an IAM
    // resource of another kind, and an account field that is not twelve digits.
    let rows: Vec<(&str, String)> = vec![
        (
            "an ARN that names neither a role nor a user",
            format!("arn:aws:iam::{ACCOUNT_ID}:group/admins"),
        ),
        (
            "an ARN naming an IAM resource of another kind",
            format!("arn:aws:iam::{ACCOUNT_ID}:policy/ReadOnly"),
        ),
        (
            "an ARN whose account field is not twelve digits",
            "arn:aws:iam::42:role/ShortAccount".to_string(),
        ),
    ];

    for (index, (case, principal)) in rows.iter().enumerate() {
        let router = router();
        let stack = format!("ecr-exclusion-refused-{index}");
        let (code, body) = create_stack(&router, &stack, &one_exclusion(principal), &[]).await;
        assert_eq!(
            code, 200,
            "case {case:?}: CreateStack({stack}) answered {code}: {body}"
        );
        let settled = settled_stack_status(case, &router, REGION, &stack).await;
        assert_eq!(
            settled,
            status::ROLLBACK_COMPLETE,
            "case {case:?}: expected the failed create to be rolled back, got {settled:?}"
        );
        let reason = failed_resource_reason(
            case,
            &router,
            REGION,
            &stack,
            EXCLUSION_LOGICAL_ID,
            resource_status::CREATE_FAILED,
        )
        .await;
        // `InvalidParameterException` is the code that registry answers a `principalArn`
        // outside its modelled pattern, which falls in the class for a refused REQUEST.
        assert!(
            reason.contains("HandlerErrorCode: InvalidRequest"),
            "case {case:?}: expected the refused-request handler class on the failed \
             resource, got {reason:?}"
        );
        assert!(
            reason.contains("principalArn"),
            "case {case:?}: expected the registry's own refusal of the member quoted on the \
             failed resource, got {reason:?}"
        );
        // The refused principal is on no exclusion list: a refusal registers nothing.
        let held = registered(&router, case).await;
        assert!(
            held.is_empty(),
            "case {case:?}: expected Amazon ECR to exclude nothing after the refusal, got \
             {held:?}"
        );
    }
}
