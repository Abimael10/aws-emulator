// SPDX-License-Identifier: CC0-1.0

//! `AWS::AIOps::InvestigationGroup` is a resource type a template may DECLARE, and the
//! catalogue entries that decide what a stack then does with such a declaration are the ones
//! the type's own contract publishes — end to end over the awsQuery wire.
//!
//! The rule under test: **a resource type Amazon Web Services publishes belongs to the
//! catalogue a submitted template is held to, so a body declaring it is admitted and the
//! declaration is judged by that type's own published contract instead.** It is the mirror of
//! `unrecognized_resource_types.rs`, which pins the other direction: a type CloudFormation does
//! NOT know is refused `Template format error: Unrecognized resource types: [<types>]`, and
//! that refusal must never be the answer for a type AWS publishes.
//!
//! What the stack then DOES with an admitted declaration — the call to that service's own
//! `CreateInvestigationGroup`, the ARN it mints, the role it resolves in IAM, the group's
//! removal with the stack and the refusal a role it cannot assume is answered with — needs
//! that service mounted beside this one, so it lives in the router's own cross-service suite
//! (`core/router/tests/all/cloudformation_provisions_aiops_investigation_groups.rs`). This
//! suite asserts admission and the catalogue the admitted declaration is then read through,
//! which is what a client sees before any resource is attempted.
//!
//! The admission rows differ along what the rule generalises over: which of the type's
//! properties the declaration states (the two it requires alone, those plus the retention and
//! the collections, those plus an encryption configuration and a resource policy), the template
//! spelling (JSON and YAML), and whether the declaration stands beside a resource of another
//! type.

use service_com_amazonaws_cloudformation_cloudformation::state::primary_identifier;
use service_com_amazonaws_cloudformation_cloudformation::state::property_validation::bounded_properties::{
    length_failure_reason, Bound, BOUNDED_PROPERTIES,
};
use service_com_amazonaws_cloudformation_cloudformation::state::property_validation::patterned_properties::{
    pattern_failure_reason, PATTERNED_PROPERTIES,
};
use service_com_amazonaws_cloudformation_cloudformation::state::property_validation::{
    missing_property_reason, PROPERTY_VALIDATION, REQUIRED_PROPERTIES,
};
use service_com_amazonaws_cloudformation_cloudformation::state::resource_types::{
    is_published, is_recognized, AWS_RESOURCE_TYPES,
};
use service_com_amazonaws_cloudformation_cloudformation::state::status;
use service_com_amazonaws_cloudformation_cloudformation::state::template::{
    identity_is_service_assigned, name_is_create_only, published_attribute_names,
    replacement_properties, AIOPS_INVESTIGATION_GROUP, IMPLEMENTED_RESOURCE_TYPES,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{
    call, create_stack, member, router, settled_stack_status, xml_blocks, xml_text, REGION,
};

/// The `EventType` a validation result is reported under, from the model's own `EventType`
/// enum (`smithy_query(query="shape", shape="EventType")`).
const VALIDATION_ERROR: &str = "VALIDATION_ERROR";

/// `OperationEvent$ValidationStatus` for a validation the declaration did not pass, and
/// `OperationEvent$ValidationFailureMode` for one whose failure was handled by FAILING the
/// operation.
const FAILED: &str = "FAILED";
const FAIL: &str = "FAIL";

/// The fixed form of the refusal a type CloudFormation does not know is answered with, which
/// no row here may ever see.
const UNRECOGNIZED_PREFIX: &str = "Template format error: Unrecognized resource types: [";

/// The logical id every body below declares its investigation group under.
const GROUP: &str = "Investigations";

/// An IAM role ARN of this account, as a declaration states the role the group gathers its
/// investigation data as. The catalogue stage never resolves it — that is the owning service's
/// own check, exercised in the router suite — so any well-formed ARN serves here.
fn role_arn() -> String {
    format!(
        "arn:aws:iam::{}:role/AIOpsAssumeRole",
        engine::emulator_account_id()
    )
}

/// One admission row: a template body declaring the type, and the name of the stack it is
/// submitted under.
struct Row {
    case: &'static str,
    stack_name: &'static str,
    template: String,
}

fn rows() -> Vec<Row> {
    let role = role_arn();
    vec![
        // The reported case: the two properties the type requires, with the published identity
        // read back through `Outputs`.
        Row {
            case: "the two required properties, with the identity read back",
            stack_name: "aiops-group-admitted",
            template: format!(
                r#"{{"AWSTemplateFormatVersion": "2010-09-09",
                     "Resources": {{"{GROUP}": {{"Type": "{AIOPS_INVESTIGATION_GROUP}",
                       "Properties": {{"Name": "retail-ops", "RoleArn": "{role}"}}}}}},
                     "Outputs": {{"GroupRef": {{"Value": {{"Ref": "{GROUP}"}}}},
                       "GroupArn": {{"Value": {{"Fn::GetAtt": ["{GROUP}", "Arn"]}}}}}}}}"#
            ),
        },
        // The retention, the CloudTrail flag and the two collection properties the type and the
        // owning API spell differently, beside the type's own `Tags`.
        Row {
            case: "the retention, the flag, both collection properties and tags",
            stack_name: "aiops-group-collections",
            template: format!(
                r#"{{"Resources": {{"{GROUP}": {{"Type": "{AIOPS_INVESTIGATION_GROUP}",
                     "Properties": {{"Name": "payments (tier-1)", "RoleArn": "{role}",
                       "RetentionInDays": 7, "IsCloudTrailEventHistoryEnabled": false,
                       "TagKeyBoundaries": ["application", "service"],
                       "CrossAccountConfigurations": [
                         {{"SourceRoleArn": "arn:aws:iam::111122223333:role/Investigator"}}],
                       "ChatbotNotificationChannels": [
                         {{"SNSTopicArn": "arn:aws:sns:us-east-1:111122223333:alerts",
                           "ChatConfigurationArns": [
                             "arn:aws:chatbot::111122223333:chat-configuration/slack-channel/ops"]}}],
                       "Tags": [{{"Key": "team", "Value": "platform"}}]}}}}}}}}"#
            ),
        },
        // The YAML spelling, with an encryption configuration and an inline resource policy —
        // the two properties that reach the owning service through members and a call of their
        // own — and a short-form intrinsic in the outputs.
        Row {
            case: "the YAML spelling, with an encryption configuration and a resource policy",
            stack_name: "aiops-group-yaml",
            template: format!(
                "\
Resources:
  {GROUP}:
    Type: {AIOPS_INVESTIGATION_GROUP}
    Properties:
      Name: fraud-detection
      RoleArn: {role}
      RetentionInDays: 30
      EncryptionConfiguration:
        EncryptionConfigurationType: CUSTOMER_MANAGED_KMS_KEY
        KmsKeyId: arn:aws:kms:us-east-1:111122223333:key/abcd1234
      InvestigationGroupPolicy: '{{\"Version\": \"2012-10-17\"}}'
Outputs:
  GroupArn:
    Value: !GetAtt {GROUP}.Arn
"
            ),
        },
        // The declaration beside a resource of another type: the catalogue admits every type a
        // body declares or none of them.
        Row {
            case: "an investigation group beside a resource of another type",
            stack_name: "aiops-group-mixed",
            template: format!(
                r#"{{"Resources": {{
                     "{GROUP}": {{"Type": "{AIOPS_INVESTIGATION_GROUP}",
                       "Properties": {{"Name": "logistics", "RoleArn": "{role}"}}}},
                     "Topic": {{"Type": "AWS::SNS::Topic",
                       "Properties": {{"TopicName": "aiops-group-topic"}}}}}}}}"#
            ),
        },
    ]
}

/// Assert one answer admitted the body rather than refusing its resource types.
fn assert_admitted(case: &str, status: u16, body: &str) {
    let message = xml_text(body, "Message").unwrap_or_default();
    assert!(
        !message.starts_with(UNRECOGNIZED_PREFIX),
        "case {case:?}: expected a type Amazon Web Services publishes to be recognised, got \
         {message:?}"
    );
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 for a body declaring a published type, got {status} \
         with body {body}"
    );
}

/// Every operation that submits a template for deployment admits a body declaring the type,
/// and so does the operation whose whole answer is a verdict on the body.
#[tokio::test]
async fn every_operation_that_takes_a_template_admits_an_investigation_group() {
    for row in rows() {
        let router = router();
        let (status, body) = create_stack(&router, row.stack_name, &row.template, &[]).await;
        assert_admitted(row.case, status, &body);
        let stack_id = xml_text(&body, "StackId").unwrap_or_default();
        assert!(
            stack_id.contains(":stack/"),
            "case {:?}: expected CreateStack to answer a stack id, got {body}",
            row.case
        );

        let (status, body) = call(
            &router,
            "ValidateTemplate",
            &[member("TemplateBody", &row.template)],
        )
        .await;
        assert_admitted(row.case, status, &body);

        let (status, body) = call(
            &router,
            "CreateChangeSet",
            &[
                member("StackName", &format!("{}-change-set", row.stack_name)),
                member("ChangeSetName", "a-change-set"),
                member("ChangeSetType", "CREATE"),
                member("TemplateBody", &row.template),
            ],
        )
        .await;
        assert_admitted(row.case, status, &body);
    }
}

/// The catalogue entries the rule rests on: the type is published and recognised, a near-miss
/// spelling of it still is not, and the type reports the identity, the return value and the
/// create-only properties its own contract publishes.
#[test]
fn the_catalogue_publishes_the_investigation_group_type_and_what_it_returns() {
    assert!(
        AWS_RESOURCE_TYPES.contains(&AIOPS_INVESTIGATION_GROUP),
        "expected {AIOPS_INVESTIGATION_GROUP:?} in the published catalogue, it is absent"
    );
    assert!(
        is_published(AIOPS_INVESTIGATION_GROUP) && is_recognized(AIOPS_INVESTIGATION_GROUP),
        "expected {AIOPS_INVESTIGATION_GROUP:?} to be a type a template may declare, it is not"
    );
    assert!(
        IMPLEMENTED_RESOURCE_TYPES.contains(&AIOPS_INVESTIGATION_GROUP),
        "expected the type's published return values to be carried, they are not"
    );
    // The catalogue is a list of names, not a shape test: a name it does not carry stays
    // unrecognised however close it is to one it does.
    for misspelled in [
        "AWS::AIOps::InvestigationGroups",
        "AWS::AIOps::Investigation",
        "AWS::AiOps::InvestigationGroup",
        "AWS::CloudWatch::InvestigationGroup",
    ] {
        assert!(
            !is_recognized(misspelled),
            "expected {misspelled:?} to stay unrecognised, it is recognised"
        );
    }
    // That service mints the identifier inside the ARN a group is addressed by, so the identity
    // in force is the one it answered rather than a name computed from the template — and the
    // ARN is the one return value these naming rules compute.
    assert!(
        identity_is_service_assigned(AIOPS_INVESTIGATION_GROUP),
        "expected the type's identity to be the one that service assigns, it is not"
    );
    assert_eq!(
        published_attribute_names(AIOPS_INVESTIGATION_GROUP),
        &["Arn"],
        "expected the ARN to be the return value these naming rules compute, got {:?}",
        published_attribute_names(AIOPS_INVESTIGATION_GROUP)
    );
    assert_eq!(
        primary_identifier::properties(AIOPS_INVESTIGATION_GROUP),
        vec!["Arn".to_string()],
        "expected the ARN to be the type's primary identifier, got {:?}",
        primary_identifier::properties(AIOPS_INVESTIGATION_GROUP)
    );
    // `UpdateInvestigationGroup` takes neither the group's name nor its retention, so a
    // declaration restating either asks for a DIFFERENT group.
    assert_eq!(
        replacement_properties(AIOPS_INVESTIGATION_GROUP),
        &["RetentionInDays"],
        "expected the retention to be the type's create-only property, got {:?}",
        replacement_properties(AIOPS_INVESTIGATION_GROUP)
    );
    assert!(
        name_is_create_only(AIOPS_INVESTIGATION_GROUP),
        "expected the group's name to be fixed when it is created, it is not"
    );
    // ...and a group gathers its investigation data AS a role, so a declaration stating none is
    // one the stage below refuses.
    let required = REQUIRED_PROPERTIES
        .iter()
        .find(|row| row.resource_type == AIOPS_INVESTIGATION_GROUP)
        .map(|row| row.required)
        .unwrap_or_default();
    assert_eq!(
        required,
        &["RoleArn"],
        "expected the role to be the type's required property, got {required:?}"
    );
}

/// The pattern the type publishes for one of its properties, read from the catalogue the stage
/// judges against rather than restated here, so the suite and the rule cannot drift apart.
fn published_pattern(property: &str) -> &'static str {
    PATTERNED_PROPERTIES
        .iter()
        .find(|row| {
            row.resource_type == AIOPS_INVESTIGATION_GROUP && row.property == [property]
        })
        .map(|row| row.pattern)
        .unwrap_or_else(|| panic!("the type's {property} pattern is catalogued"))
}

/// The validation RESULTS the operation's own events report for `stack`: one tuple per event
/// carrying a validation, in the order they are reported.
async fn validation_results(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
) -> Vec<(String, String, String, String, String, String)> {
    let (code, body) = call(router, "DescribeEvents", &[member("StackName", stack)]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: the operation's events must be readable, got {code} {body}"
    );
    xml_blocks(&body, "member")
        .iter()
        .filter(|event| xml_text(event, "EventType").as_deref() == Some(VALIDATION_ERROR))
        .map(|event| {
            (
                xml_text(event, "LogicalResourceId").unwrap_or_default(),
                xml_text(event, "ValidationName").unwrap_or_default(),
                xml_text(event, "ValidationStatus").unwrap_or_default(),
                xml_text(event, "ValidationFailureMode").unwrap_or_default(),
                xml_text(event, "ValidationStatusReason").unwrap_or_default(),
                xml_text(event, "ValidationPath").unwrap_or_default(),
            )
        })
        .collect()
}

/// What the stage must make of one declared value: the verdict the type's published contract
/// owes it.
enum Verdict {
    /// Every condition the type publishes for the property admits the value.
    Admitted,
    /// The value is outside the SPELLING the property publishes.
    Pattern,
    /// The value is shorter than the property's published `minLength`, which is that bound.
    TooShort(usize),
    /// ...and longer than its published `maxLength`.
    TooLong(usize),
}

/// One value row: which property the declaration states, the value it states for it, and the
/// verdict the type's published contract owes that value.
struct ValueRow {
    case: &'static str,
    stack_name: &'static str,
    property: &'static str,
    value: String,
    verdict: Verdict,
}

/// The length bounds the type publishes for its two scalar properties, read from the catalogue
/// the stage judges against rather than restated here. Source: `Name`'s `@length 1..512` and
/// `RoleArn`'s `@length 20..2048` in that service's model, which the type's schema states as
/// each member's `minLength`/`maxLength`.
fn published_bounds(property: &str) -> (usize, usize) {
    let row = BOUNDED_PROPERTIES
        .iter()
        .find(|row| row.resource_type == AIOPS_INVESTIGATION_GROUP && row.property == [property])
        .unwrap_or_else(|| panic!("the type's {property} bounds are catalogued"));
    (
        row.min_length.expect("a published minimum"),
        row.max_length.expect("a published maximum"),
    )
}

/// An ARN exactly `length` characters long, for a row that probes a published length bound: the
/// shortest well-formed ARN this emulator's own account can spell, padded to the width asked for
/// so what the row varies is the LENGTH alone and not whether the value is an ARN.
fn arn_of_length(length: usize) -> String {
    let prefix = "arn:aws:iam::000000000000:role/";
    if length <= prefix.len() {
        return prefix[..length].to_string();
    }
    format!("{prefix}{}", "a".repeat(length - prefix.len()))
}

fn value_rows() -> Vec<ValueRow> {
    let own_account = engine::emulator_account_id();
    let (role_min, role_max) = published_bounds("RoleArn");
    let (name_min, name_max) = published_bounds("Name");
    vec![
        // The `RoleArn` rows: the form the type publishes admits any ARN inside its length
        // bounds, so the rows differ along what makes a value an ARN and along its length —
        // each published bound and one character past it.
        ValueRow {
            case: "an IAM role ARN of this account",
            stack_name: "aiops-role-iam",
            property: "RoleArn",
            value: format!("arn:aws:iam::{own_account}:role/AIOpsAssumeRole"),
            verdict: Verdict::Admitted,
        },
        ValueRow {
            case: "a role ARN of another partition",
            stack_name: "aiops-role-partition",
            property: "RoleArn",
            value: format!("arn:aws-us-gov:iam::{own_account}:role/AIOpsAssumeRole"),
            verdict: Verdict::Admitted,
        },
        ValueRow {
            case: "an ARN of another service, which the published form still admits",
            stack_name: "aiops-role-other-service",
            property: "RoleArn",
            value: format!("arn:aws:sqs:us-east-1:{own_account}:a-queue"),
            verdict: Verdict::Admitted,
        },
        ValueRow {
            case: "an ARN at the published lower length bound",
            stack_name: "aiops-role-shortest",
            property: "RoleArn",
            value: arn_of_length(role_min),
            verdict: Verdict::Admitted,
        },
        ValueRow {
            case: "an ARN at the published upper length bound",
            stack_name: "aiops-role-longest",
            property: "RoleArn",
            value: arn_of_length(role_max),
            verdict: Verdict::Admitted,
        },
        ValueRow {
            case: "an ARN one character short of the lower bound",
            stack_name: "aiops-role-too-short",
            property: "RoleArn",
            value: arn_of_length(role_min - 1),
            verdict: Verdict::TooShort(role_min),
        },
        ValueRow {
            case: "an ARN one character past the upper bound",
            stack_name: "aiops-role-too-long",
            property: "RoleArn",
            value: arn_of_length(role_max + 1),
            verdict: Verdict::TooLong(role_max),
        },
        ValueRow {
            // The recorded class: a short value that is no ARN either. The LENGTH bound is what
            // the stage reports, which is the order its conditions are judged in.
            case: "a bare role name, too short to be an ARN at all",
            stack_name: "aiops-role-bare-name",
            property: "RoleArn",
            value: "bad-role".to_string(),
            verdict: Verdict::TooShort(role_min),
        },
        ValueRow {
            case: "a long value that is no ARN",
            stack_name: "aiops-role-not-an-arn",
            property: "RoleArn",
            value: "definitely-not-an-arn-at-all".to_string(),
            verdict: Verdict::Pattern,
        },
        ValueRow {
            case: "a value whose ARN prefix is mis-cased",
            stack_name: "aiops-role-miscased",
            property: "RoleArn",
            value: format!("ARN:aws:iam::{own_account}:role/AIOpsAssumeRole"),
            verdict: Verdict::Pattern,
        },
        ValueRow {
            case: "a value carrying the ARN prefix after something else",
            stack_name: "aiops-role-prefixed",
            property: "RoleArn",
            value: format!("role=arn:aws:iam::{own_account}:role/AIOpsAssumeRole"),
            verdict: Verdict::Pattern,
        },
        // The `Name` rows: the published alphabet is letters, digits, the space and the
        // punctuation the pattern names, so the rows differ along which characters a name uses
        // and along its length — each published bound and one character past it.
        ValueRow {
            case: "a name over the whole published alphabet",
            stack_name: "aiops-name-alphabet",
            property: "Name",
            value: "retail-ops_2026 [tier].(1){a}:b".to_string(),
            verdict: Verdict::Admitted,
        },
        ValueRow {
            case: "a name at the published lower length bound",
            stack_name: "aiops-name-shortest",
            property: "Name",
            value: "a".repeat(name_min),
            verdict: Verdict::Admitted,
        },
        ValueRow {
            case: "a name at the published upper length bound",
            stack_name: "aiops-name-longest",
            property: "Name",
            value: "n".repeat(name_max),
            verdict: Verdict::Admitted,
        },
        ValueRow {
            case: "a name one character short of the lower bound",
            stack_name: "aiops-name-empty",
            property: "Name",
            value: "a".repeat(name_min - 1),
            verdict: Verdict::TooShort(name_min),
        },
        ValueRow {
            case: "a name one character past the upper bound",
            stack_name: "aiops-name-too-long",
            property: "Name",
            value: "n".repeat(name_max + 1),
            verdict: Verdict::TooLong(name_max),
        },
        ValueRow {
            case: "a character outside the published alphabet",
            stack_name: "aiops-name-punctuated",
            property: "Name",
            value: "retail!ops".to_string(),
            verdict: Verdict::Pattern,
        },
        ValueRow {
            case: "a path separator, which the alphabet does not carry",
            stack_name: "aiops-name-slash",
            property: "Name",
            value: "retail/ops".to_string(),
            verdict: Verdict::Pattern,
        },
    ]
}

/// The sentence the stage reports for one refused row, as the published contract words it.
fn refusal_reason(row: &ValueRow) -> Option<String> {
    let length = row.value.chars().count();
    match row.verdict {
        Verdict::Admitted => None,
        Verdict::Pattern => Some(pattern_failure_reason(
            &row.value,
            published_pattern(row.property),
        )),
        Verdict::TooShort(bound) => Some(length_failure_reason(
            &[row.property],
            Bound::Minimum(bound),
            length,
        )),
        Verdict::TooLong(bound) => Some(length_failure_reason(
            &[row.property],
            Bound::Maximum(bound),
            length,
        )),
    }
}

/// The WORDING each of the two sentences carries, pinned literally so the rows above — which
/// build their expectation through the same functions the stage reports with — cannot both drift
/// at once.
///
/// A length failure is the schema validator's own `#/<pointer>: expected <keyword>: <bound>,
/// actual: <length>`, and a spelling failure the form CloudFormation reports a value outside its
/// pattern with. Every variable part is what was judged: the property, its published bound, the
/// value's own length, and the pattern the property is held to.
#[test]
fn the_two_refusal_sentences_are_worded_as_the_contract_words_them() {
    let rows: [(&str, String, String); 3] = [
        (
            "a value under the published minimum",
            length_failure_reason(&["RoleArn"], Bound::Minimum(20), 8),
            "#/RoleArn: expected minLength: 20, actual: 8".to_string(),
        ),
        (
            "a value over the published maximum",
            length_failure_reason(&["Name"], Bound::Maximum(512), 513),
            "#/Name: expected maxLength: 512, actual: 513".to_string(),
        ),
        (
            "a value outside the published spelling",
            pattern_failure_reason("retail/ops", r"^[\-_A-Za-z0-9\[\]\(\)\{\}\.: ]+$"),
            r"Property value [retail/ops] does not match pattern: ^[\-_A-Za-z0-9\[\]\(\)\{\}\.: ]+$"
                .to_string(),
        ),
    ];
    for (case, reported, expected) in rows {
        assert_eq!(
            reported, expected,
            "case {case:?}: expected {expected:?}, got {reported:?}"
        );
    }
}

/// A value outside the form the type publishes for one of its properties fails the stage that
/// runs before any provider is invoked — naming the resource, the condition it broke and the
/// property's own path — while a value that form admits passes it, whatever the stack then does
/// about the service that owns the resource.
#[tokio::test]
async fn a_value_outside_the_types_published_form_fails_property_validation() {
    for row in value_rows() {
        let case = row.case;
        // A router of its own per row: what each case is about is its own declaration, not what
        // another row left behind.
        let router = router();
        let mut properties = ::serde_json::json!({
            "Name": "retail-ops",
            "RoleArn": role_arn(),
        });
        properties[row.property] = ::serde_json::Value::String(row.value.clone());
        let template = ::serde_json::json!({
            "Resources": {GROUP: {"Type": AIOPS_INVESTIGATION_GROUP, "Properties": properties}}
        })
        .to_string();

        let (code, created) = create_stack(&router, row.stack_name, &template, &[]).await;
        assert_eq!(
            code, 200,
            "case {case:?}: CreateStack must answer 200 — the body declares a type AWS \
             publishes — got {code} {created}"
        );

        let settled = settled_stack_status(case, &router, REGION, row.stack_name).await;
        let results = validation_results(&router, case, row.stack_name).await;
        let Some(reason) = refusal_reason(&row) else {
            assert!(
                results.is_empty(),
                "case {case:?}: expected a value the published form admits to pass the stage, \
                 got {results:?}"
            );
            continue;
        };
        assert_eq!(
            settled,
            status::ROLLBACK_COMPLETE,
            "case {case:?}: expected a value outside the published form to roll the stack back, \
             got {settled:?}"
        );
        assert_eq!(
            results,
            vec![(
                GROUP.to_string(),
                PROPERTY_VALIDATION.to_string(),
                FAILED.to_string(),
                FAIL.to_string(),
                reason,
                format!("/Resources/{GROUP}/Properties/{}", row.property),
            )],
            "case {case:?}: expected the one validation result the refused value owes, got \
             {results:?}"
        );
    }
}

/// A declaration stating no role describes a group that could investigate nothing, so the same
/// stage refuses it — naming the property the type requires.
#[tokio::test]
async fn a_declaration_stating_no_role_fails_property_validation() {
    let case = "a declaration stating no RoleArn";
    let router = router();
    let template = ::serde_json::json!({
        "Resources": {
            GROUP: {
                "Type": AIOPS_INVESTIGATION_GROUP,
                "Properties": {"Name": "retail-ops"},
            }
        }
    })
    .to_string();
    let (code, created) = create_stack(&router, "aiops-group-no-role", &template, &[]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: CreateStack must answer 200 — the body declares a type AWS publishes — \
         got {code} {created}"
    );
    let settled = settled_stack_status(case, &router, REGION, "aiops-group-no-role").await;
    assert_eq!(
        settled,
        status::ROLLBACK_COMPLETE,
        "case {case:?}: expected a declaration missing a required property to roll the stack \
         back, got {settled:?}"
    );
    let results = validation_results(&router, case, "aiops-group-no-role").await;
    assert_eq!(
        results,
        vec![(
            GROUP.to_string(),
            PROPERTY_VALIDATION.to_string(),
            FAILED.to_string(),
            FAIL.to_string(),
            missing_property_reason("RoleArn"),
            format!("/Resources/{GROUP}/Properties"),
        )],
        "case {case:?}: expected the one validation result the missing role owes, got {results:?}"
    );
}
