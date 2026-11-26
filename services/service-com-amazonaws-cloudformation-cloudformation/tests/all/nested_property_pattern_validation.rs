// SPDX-License-Identifier: CC0-1.0

//! The pre-deployment property validation reaching a patterned member NESTED inside a
//! declaration — including one inside a LIST-valued property — end to end over the awsQuery
//! wire.
//!
//! RULE: CloudFormation holds a declared property value to the `pattern` its resource type's
//! schema states for that property WHEREVER the property sits in the declaration, so a value
//! outside it fails the operation before anything is provisioned: the STACK records its own
//! `CREATE_FAILED` carrying the stage's summary over its errors, the detail is the
//! `VALIDATION_ERROR` operation event quoting the rejected value and the pattern at the JSON
//! pointer of the member that carried it — the element's own INDEX included where the member
//! sits inside a list — and no resource of the stack is created in the service that owns it.
//!
//! Why it matters: a client that deploys a template and, on failure, reads
//! `DescribeStackEvents` to learn why must find the stack's own `CREATE_FAILED` row and its
//! reason. A deployment that merely "rolled back" with no stack-level failure row cannot be
//! told from a throttle, a quota or a permissions failure, and CI that prints that reason
//! prints nothing.
//!
//! The rows differ along what the rule generalises over: WHICH member carries the rejected
//! value (a property of the resource itself, a member inside a list-valued property), WHICH
//! part of the published form it breaks (the leading character class, the look-around that
//! forbids two consecutive spaces, the alphabet), WHICH element of the list it sits in (the
//! first, a later one) and how many elements the list carries — plus the control, the same
//! estate with every value inside its form, which deploys and is read back in the service
//! that owns it.
//!
//! `resource_schema_validation.rs` and `validation_failure_reported_at_two_levels.rs` own the
//! two levels for the other questions of that stage; what this suite adds is the patterned
//! member, at the depth a declaration carries it.
//!
//! Nothing here waits out a span: every stack is polled to its terminal status.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::bcm_dashboards_provisioning::{get_dashboard, graph_config, listed_custom_dashboards};
use crate::harness::{
    create_stack, physical_resource_id, router, settled_stack_status, stack_own_events, REGION,
};
use crate::validation_failure_reported_at_two_levels::validation_results;
use service_com_amazonaws_cloudformation_cloudformation::state::{resource_status, status};

/// The type every template below declares. Source: `state::template::BCM_DASHBOARD`.
const BCM_DASHBOARD: &str = "AWS::BCM::Dashboard";

/// The logical id it is declared under.
const LOGICAL_ID: &str = "Dashboard";

/// The validation CloudFormation names for the check that holds a declaration's properties to
/// its type's schema. Source: `state::property_validation::PROPERTY_VALIDATION`.
const PROPERTY_VALIDATION: &str = "PROPERTY_VALIDATION";

/// The `EventType` a validation result is reported under, from the model's `EventType` enum
/// (`smithy_query(query="shape", shape="EventType")`).
const VALIDATION_ERROR: &str = "VALIDATION_ERROR";

/// `StackEvent$DetailedStatus` for an operation a pre-deployment validation failed, from the
/// model's `DetailedStatus` enum (`[CONFIGURATION_COMPLETE, VALIDATION_FAILED]`).
const VALIDATION_FAILED: &str = "VALIDATION_FAILED";

/// The form a widget's title is published in: `Widget$title`'s `@pattern` (source:
/// `smithy_query(query="shape", shape="Widget")` of the
/// `com-amazonaws-bcmdashboards-awsbcmdashboardsservice` model), which the
/// `AWS::BCM::Dashboard` schema states for the `Title` of each of its `Widgets`.
const WIDGET_TITLE_PATTERN: &str = r"^(?!.* {2})[a-zA-Z0-9_-][ a-zA-Z0-9_-]*[a-zA-Z0-9_-]$";

/// ...the form a dashboard's own name is published in: `CreateDashboard$name`'s `@pattern`
/// (source: `smithy_query(query="operation", operation="CreateDashboard")` of the same model).
const DASHBOARD_NAME_PATTERN: &str = r"^(?!.* {2})[a-zA-Z][a-zA-Z0-9 _-]{0,48}[a-zA-Z0-9_-]$";

/// ...and the form every description of that service is published in, which the type's schema
/// states for the dashboard's `Description` and for each widget's.
const DESCRIPTION_PATTERN: &str = r"^(?!.* {2})[ a-zA-Z0-9.,!?;:@#$%&\-_/\\]*$";

/// What a stack reports for a failure of ANY of CloudFormation's pre-deployment validations:
/// the count of errors the stage found and where their detail is read. The sentence AWS words
/// it in; the crate's own spelling of it is
/// `state::early_validation::validation_failure_summary`.
fn summary(errors: usize) -> String {
    format!(
        "Validation failed with {errors} error(s). Call DescribeEvents to retrieve the full \
         list of issues with resource and property details, resolve each error, then retry \
         the operation."
    )
}

/// How the stage words one error of this class: the value the declaration carried and the
/// pattern it does not match. Source:
/// `state::property_validation::patterned_properties::pattern_failure_reason`.
fn pattern_failure(value: &str, pattern: &str) -> String {
    format!("Property value [{value}] does not match pattern: {pattern}")
}

/// What the validation must report about the refused declaration: the value it rejected, the
/// published form that value is held to, and the member of the template it was carried at.
struct Refusal {
    rejected: String,
    pattern: &'static str,
    /// The path under the resource's `Properties` the rejected value sits at, indices and all.
    at: &'static str,
}

/// One row: the dashboard declaration a stack is created from, and the refusal the validation
/// must report — or `None` for the control, every value of which is inside its form.
struct Row {
    case: &'static str,
    stack: String,
    /// The dashboard's own `Name`, which the control reads the deployed dashboard back by.
    name: &'static str,
    properties: Value,
    refusal: Option<Refusal>,
}

/// A widget carrying `title` and whatever else the row states, with one config of the kind a
/// widget is deployed with.
fn widget(title: &str, description: Option<&str>) -> Value {
    let mut widget = json!({
        "Title": title,
        "Configs": [graph_config("UnblendedCost", "LINE", "MONTHLY")],
    });
    if let Some(description) = description {
        widget["Description"] = json!(description);
    }
    widget
}

/// The rows, in the order the test drives them.
fn rows() -> Vec<Row> {
    let stack = |index: usize| format!("nested-pattern-validation-{index}");
    vec![
        Row {
            case: "the only widget's title starts with a space, which its leading character \
                   class forbids",
            stack: stack(1),
            name: "Cost Overview One",
            properties: json!({
                "Name": "Cost Overview One",
                "Widgets": [widget(" Leading Space", None)],
            }),
            refusal: Some(Refusal {
                rejected: " Leading Space".to_string(),
                pattern: WIDGET_TITLE_PATTERN,
                at: "Widgets/0/Title",
            }),
        },
        Row {
            // The SECOND of three widgets, so the path the refusal names is that element's own
            // rather than the list's or the first element's.
            case: "the second of three widget titles carries two consecutive spaces, which the \
                   form's look-around forbids",
            stack: stack(2),
            name: "Cost Overview Two",
            properties: json!({
                "Name": "Cost Overview Two",
                "Widgets": [
                    widget("Monthly Spend", None),
                    widget("Monthly  Spend", None),
                    widget("Usage Table", None),
                ],
            }),
            refusal: Some(Refusal {
                rejected: "Monthly  Spend".to_string(),
                pattern: WIDGET_TITLE_PATTERN,
                at: "Widgets/1/Title",
            }),
        },
        Row {
            case: "a widget title over a character its alphabet does not carry",
            stack: stack(3),
            name: "Cost Overview Three",
            properties: json!({
                "Name": "Cost Overview Three",
                "Widgets": [widget("Monthly Spend", None), widget("Spend/Usage", None)],
            }),
            refusal: Some(Refusal {
                rejected: "Spend/Usage".to_string(),
                pattern: WIDGET_TITLE_PATTERN,
                at: "Widgets/1/Title",
            }),
        },
        Row {
            // The OTHER patterned member of the same nested structure: the same rule, one
            // member along.
            case: "a widget description carrying a character its published form forbids",
            stack: stack(4),
            name: "Cost Overview Four",
            properties: json!({
                "Name": "Cost Overview Four",
                "Widgets": [widget("Monthly Spend", Some("Spend by (service)"))],
            }),
            refusal: Some(Refusal {
                rejected: "Spend by (service)".to_string(),
                pattern: DESCRIPTION_PATTERN,
                at: "Widgets/0/Description",
            }),
        },
        Row {
            // ...and a property of the RESOURCE itself, to pin that the depth is what varies
            // and not the rule.
            case: "the dashboard's own name starting with a digit, which its form forbids",
            stack: stack(5),
            name: "2026 Costs",
            properties: json!({
                "Name": "2026 Costs",
                "Widgets": [widget("Monthly Spend", None)],
            }),
            refusal: Some(Refusal {
                rejected: "2026 Costs".to_string(),
                pattern: DASHBOARD_NAME_PATTERN,
                at: "Name",
            }),
        },
        Row {
            case: "the dashboard's own description carrying two consecutive spaces",
            stack: stack(6),
            name: "Cost Overview Six",
            properties: json!({
                "Name": "Cost Overview Six",
                "Description": "Spend  and usage.",
                "Widgets": [widget("Monthly Spend", None)],
            }),
            refusal: Some(Refusal {
                rejected: "Spend  and usage.".to_string(),
                pattern: DESCRIPTION_PATTERN,
                at: "Description",
            }),
        },
        Row {
            // The control: the same estate with every value inside the form its member
            // publishes, which deploys and is read back where it really lives.
            case: "every name, description and widget title inside its published form",
            stack: stack(7),
            name: "Cost Overview Seven",
            properties: json!({
                "Name": "Cost Overview Seven",
                "Description": "Spend and usage.",
                "Widgets": [
                    widget("Monthly Spend", Some("Amortized cost by month.")),
                    widget("Usage_By-Service 2", None),
                ],
            }),
            refusal: None,
        },
    ]
}

/// The template one row deploys: its dashboard, alone under [`LOGICAL_ID`].
fn template(properties: &Value) -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {LOGICAL_ID: {"Type": BCM_DASHBOARD, "Properties": properties}},
    })
    .to_string()
}

/// Every row: a value outside its member's published form fails the create at the
/// pre-deployment stage — the stack's own `CREATE_FAILED` carries the stage's summary, the
/// operation event carries the value, the pattern and the member's own path, and the service
/// that owns the type holds nothing — while the control's dashboard is deployed and read back
/// there.
#[tokio::test]
async fn a_patterned_member_outside_its_form_fails_the_stack_before_anything_is_created() {
    for row in rows() {
        // A router of its own per row: what each case is about is its own operation, not what
        // another row left behind.
        let router = router();
        let case = row.case;
        let stack = row.stack.as_str();

        let (code, created) = create_stack(&router, stack, &template(&row.properties), &[]).await;
        assert_eq!(
            code, 200,
            "case {case:?}: CreateStack must answer 200 — the submission is well formed and the \
             type is one a template may declare — got {code} {created}"
        );
        let settled = settled_stack_status(case, &router, REGION, stack).await;
        let expected_status = match row.refusal {
            Some(_) => status::ROLLBACK_COMPLETE,
            None => status::CREATE_COMPLETE,
        };
        assert_eq!(
            settled, expected_status,
            "case {case:?}: expected the stack to settle {expected_status}, got {settled:?}"
        );

        // The STACK's own failure: one `CREATE_FAILED` row carrying the stage's summary over
        // its errors, which is what a client reading `DescribeStackEvents` acts on.
        let own_events = stack_own_events(&router, case, stack).await;
        let failures: Vec<(String, String)> = own_events
            .iter()
            .filter(|event| event.resource_status == resource_status::CREATE_FAILED)
            .map(|event| (event.detailed_status.clone(), event.reason.clone()))
            .collect();
        let expected_failures: Vec<(String, String)> = row
            .refusal
            .as_ref()
            .map(|_| vec![(VALIDATION_FAILED.to_string(), summary(1))])
            .unwrap_or_default();
        assert_eq!(
            failures, expected_failures,
            "case {case:?}: expected the stack's own failure event(s) {expected_failures:?}, got \
             {own_events:?}"
        );

        // ...and the DETAIL: the operation event naming the validation, its status, how its
        // failure was handled, the value it rejected beside the form that value is held to, and
        // the member of the template it was carried at.
        let results = validation_results(&router, case, stack).await;
        let expected_results: Vec<Vec<String>> = row
            .refusal
            .as_ref()
            .map(|refusal| {
                vec![vec![
                    LOGICAL_ID.to_string(),
                    BCM_DASHBOARD.to_string(),
                    VALIDATION_ERROR.to_string(),
                    PROPERTY_VALIDATION.to_string(),
                    "FAILED".to_string(),
                    "FAIL".to_string(),
                    pattern_failure(&refusal.rejected, refusal.pattern),
                    format!("/Resources/{LOGICAL_ID}/Properties/{}", refusal.at),
                ]]
            })
            .unwrap_or_default();
        assert_eq!(
            results, expected_results,
            "case {case:?}: expected the validation results {expected_results:?}, got {results:?}"
        );

        // The service that OWNS the type is the proof: a refused deployment asked it for
        // nothing, and the control's dashboard is really there, under the handle the stack
        // published, with the widgets the declaration asked for.
        let listed = listed_custom_dashboards(&router).await;
        if row.refusal.is_some() {
            assert!(
                listed.is_empty(),
                "case {case:?}: expected the refused create to leave no dashboard, got {listed:?}"
            );
            continue;
        }
        assert_eq!(
            listed.len(),
            1,
            "case {case:?}: expected the deployed stack's one dashboard, got {listed:?}"
        );
        let arn = physical_resource_id(&router, case, stack, LOGICAL_ID).await;
        let (code, dashboard) = get_dashboard(&router, &arn).await;
        assert_eq!(
            code, 200,
            "case {case:?}: GetDashboard({arn}) must answer 200, got {code} {dashboard}"
        );
        assert_eq!(
            dashboard.get("name").and_then(Value::as_str),
            Some(row.name),
            "case {case:?}: expected the dashboard to be held under {:?}, got {dashboard}",
            row.name
        );
        let titles: Vec<&str> = dashboard
            .get("widgets")
            .and_then(Value::as_array)
            .map(|widgets| {
                widgets
                    .iter()
                    .filter_map(|widget| widget.get("title").and_then(Value::as_str))
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(
            titles,
            vec!["Monthly Spend", "Usage_By-Service 2"],
            "case {case:?}: expected the declared widget titles, got {dashboard}"
        );
    }
}

/// The boundary of the stage: a declaration the request DISABLES the pre-deployment
/// validations for is deployed as written, so the same value is refused by the service that
/// owns the resource instead — on the RESOURCE, in that service's own words.
///
/// "Set to true to disable pre-deployment validations in changeset or stack operations"
/// (`CreateStackInput$DisableValidation`), so what the switch moves is WHERE the value is
/// refused, never whether it is.
#[tokio::test]
async fn disabling_the_validations_leaves_the_same_value_to_the_owning_service() {
    let case = "a widget title outside its form, with the validations disabled";
    let router: MultiServiceRouter = router();
    let stack = "nested-pattern-validation-disabled";
    let properties = json!({
        "Name": "Cost Overview Disabled",
        "Widgets": [widget(" Leading Space", None)],
    });
    let (code, created) = create_stack(
        &router,
        stack,
        &template(&properties),
        &[crate::harness::member("DisableValidation", "true")],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: CreateStack must answer 200, got {code} {created}"
    );
    let settled = settled_stack_status(case, &router, REGION, stack).await;
    assert_eq!(
        settled,
        status::ROLLBACK_COMPLETE,
        "case {case:?}: expected the stack to roll back, got {settled:?}"
    );

    // No validation result at all: the stage did not run, so nothing of it is reported.
    let results = validation_results(&router, case, stack).await;
    assert!(
        results.is_empty(),
        "case {case:?}: expected no validation result where the stage is disabled, got \
         {results:?}"
    );
    // ...and the RESOURCE is what failed, in the owning service's own words naming the member
    // that broke the rule.
    let reason = crate::harness::failed_resource_reason(
        case,
        &router,
        REGION,
        stack,
        LOGICAL_ID,
        resource_status::CREATE_FAILED,
    )
    .await;
    assert!(
        reason.contains("title"),
        "case {case:?}: expected the owning service's refusal to name the member that broke the \
         rule, got {reason:?}"
    );
    assert!(
        listed_custom_dashboards(&router).await.is_empty(),
        "case {case:?}: expected the refused create to leave no dashboard"
    );
}
