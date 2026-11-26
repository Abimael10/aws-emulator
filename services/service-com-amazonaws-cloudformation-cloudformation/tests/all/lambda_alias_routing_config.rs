// SPDX-License-Identifier: CC0-1.0

//! What a stack deploys when an `AWS::Lambda::Alias` declares the traffic split an alias routes
//! with.
//!
//! RULE: a resource type's property may be SHAPED differently from the API member it travels
//! in, and the provisioner sends the member's own shape — `AWS::Lambda::Alias` declares
//! `RoutingConfig.AdditionalVersionWeights` as a LIST of `{FunctionVersion, FunctionWeight}`
//! entries while AWS Lambda models the member as a MAP of version to weight
//! (`smithy_query(query="shape", shape="AliasRoutingConfiguration")`), so every declared entry
//! reaches the call as one key of that map and AWS Lambda reports the weights the declaration
//! asked for. A declaration AWS Lambda itself refuses — a weight naming the version the alias
//! already invokes — fails the resource with THAT service's own refusal and nothing else.
//!
//! Why it matters: alias traffic shifting is the mechanism behind every canary deployment of a
//! Lambda function. A declaration carried through as written reaches a map-typed member as an
//! array, which AWS Lambda cannot deserialize at all, so every template that weights an alias
//! rolls back with a parser message naming no template property.
//!
//! The rows differ along what the rule generalises over: whether the version and the weight
//! are quoted, which version of the two the entry names, and whether the version the entry
//! names is one the alias does not already invoke.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::hotswapped_function_code::lambda;
use crate::stack_lifecycle::{
    acknowledging_iam, call, create_stack, member, resource_event_reason, router_with_engine,
    xml_text,
};

/// The runtime and the handler every function below is declared with.
const RUNTIME: &str = "python3.11";
const HANDLER: &str = "index.handler";
const SOURCE: &str = "def handler(event, context):\n    return {'ok': True}\n";

/// The alias name every stack below declares, which is no version-number spelling (the model's
/// `Alias` pattern `^(?!^[0-9]+$)([a-zA-Z0-9-_]+)$` keeps the two namespaces apart).
const ALIAS_NAME: &str = "live";

/// The logical ids the templates below declare, which is what the stack reports each resource
/// under.
const ALIAS_LOGICAL_ID: &str = "Alias";

/// One row: the weights the update declares for the alias, and what must become of them.
struct Row {
    case: &'static str,
    stack: &'static str,
    /// The `AdditionalVersionWeights` entries the update declares, in the LIST form the
    /// resource type publishes.
    declared: fn() -> Value,
    /// The map AWS Lambda must report for the alias once the update is done, or `None` for a
    /// declaration that service refuses.
    expected: Option<fn() -> Value>,
}

/// The class the rule covers: a weight on a version the alias does not invoke, declared with
/// the scalars spelled either way — and the one declaration AWS Lambda refuses.
const ROWS: &[Row] = &[
    Row {
        case: "one weight on the version the alias does not invoke",
        stack: "cfnalias-weighted",
        declared: || json!([{ "FunctionVersion": "1", "FunctionWeight": 0.5 }]),
        expected: Some(|| json!({ "1": 0.5 })),
    },
    Row {
        case: "the same weight with both scalars spelled as a template may quote them",
        stack: "cfnalias-weighted-quoted",
        declared: || json!([{ "FunctionVersion": 1, "FunctionWeight": "0.25" }]),
        expected: Some(|| json!({ "1": 0.25 })),
    },
    Row {
        case: "a weight naming the version the alias already invokes",
        stack: "cfnalias-weight-on-primary",
        declared: || json!([{ "FunctionVersion": "2", "FunctionWeight": 0.5 }]),
        expected: None,
    },
];

/// The template the rows deploy: the execution role, the function, the versions of it the
/// deployment has published so far and the alias that invokes the LAST of them — with `weights`
/// declared on the alias where the row carries any.
///
/// `versions` is how many `AWS::Lambda::Version` resources the revision declares, so the create
/// publishes version 1 and the update, which also changes the function's code, publishes
/// version 2 and repoints the alias onto it: a row weighting version 1 names a version the
/// alias does NOT invoke and a row weighting version 2 names the one it does.
fn template(stack: &str, source: &str, versions: usize, weights: Option<&Value>) -> String {
    let mut resources = serde_json::Map::new();
    resources.insert(
        "Role".to_string(),
        json!({
            "Type": "AWS::IAM::Role",
            "Properties": {
                "RoleName": format!("{stack}-exec"),
                "AssumeRolePolicyDocument": {
                    "Version": "2012-10-17",
                    "Statement": [{
                        "Effect": "Allow",
                        "Principal": { "Service": "lambda.amazonaws.com" },
                        "Action": "sts:AssumeRole"
                    }]
                }
            }
        }),
    );
    resources.insert(
        "Fn".to_string(),
        json!({
            "Type": "AWS::Lambda::Function",
            "Properties": {
                "FunctionName": format!("{stack}-fn"),
                "Runtime": RUNTIME,
                "Handler": HANDLER,
                "Role": { "Fn::GetAtt": ["Role", "Arn"] },
                "Code": { "ZipFile": source }
            }
        }),
    );
    for ordinal in 1..=versions {
        resources.insert(
            version_logical_id(ordinal),
            json!({
                "Type": "AWS::Lambda::Version",
                "Properties": {
                    "FunctionName": { "Ref": "Fn" },
                    "Description": format!("the revision {ordinal} snapshot")
                }
            }),
        );
    }
    let mut alias = json!({
        "FunctionName": { "Ref": "Fn" },
        "Name": ALIAS_NAME,
        "FunctionVersion": { "Fn::GetAtt": [version_logical_id(versions), "Version"] },
    });
    if let Some(weights) = weights {
        alias["RoutingConfig"] = json!({ "AdditionalVersionWeights": weights });
    }
    resources.insert(
        ALIAS_LOGICAL_ID.to_string(),
        json!({ "Type": "AWS::Lambda::Alias", "Properties": alias }),
    );
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": Value::Object(resources),
    })
    .to_string()
}

/// The logical id of the `ordinal`-th version a revision declares.
fn version_logical_id(ordinal: usize) -> String {
    format!("Version{ordinal}")
}

/// The alias AWS Lambda itself holds for `function`, read through `GetAlias`.
async fn alias_of(router: &MultiServiceRouter, function: &str) -> Value {
    let answered = lambda(
        router,
        "GET",
        &format!("/2015-03-31/functions/{function}/aliases/{ALIAS_NAME}"),
        &[],
        &[],
    )
    .await;
    assert_eq!(
        answered.status,
        200,
        "GetAlias({function}) must answer 200, got {} {}",
        answered.status,
        answered.body
    );
    answered.body
}

/// RULE: every declared `AdditionalVersionWeights` entry reaches AWS Lambda as one key of the
/// map that member is, so the alias a stack deploys really routes the traffic its template
/// declared — and a weight AWS Lambda refuses fails the resource with that service's own
/// refusal.
#[tokio::test]
async fn an_alias_routes_with_the_weights_its_template_declares() {
    for row in ROWS {
        let case = row.case;
        let (router, _engine) = router_with_engine();
        let function = format!("{}-fn", row.stack);

        // ---- The deployment the weighted update is made against: one published version, and
        // the alias invoking it.
        let (status_code, body) = create_stack(
            &router,
            row.stack,
            &template(row.stack, SOURCE, 1, None),
            &acknowledging_iam(&[]),
        )
        .await;
        assert_eq!(
            status_code, 200,
            "case {case:?}: expected CreateStack to be accepted, got {status_code} {body}"
        );
        let settled = crate::stack_lifecycle::settled_stack(&router, case, row.stack).await;
        let created = xml_text(&settled, "StackStatus").unwrap_or_default();
        assert_eq!(
            created, "CREATE_COMPLETE",
            "case {case:?}: expected the create to complete, got {created:?}"
        );

        // ---- The update: new code and a second version resource, so the deployment publishes
        // version 2, repoints the alias onto it and declares the row's own weights.
        let revised = "def handler(event, context):\n    return {'ok': 2}\n";
        let updated = template(row.stack, revised, 2, Some(&(row.declared)()));
        let mut members = vec![
            member("StackName", row.stack),
            member("TemplateBody", &updated),
        ];
        members.extend(acknowledging_iam(&[]));
        let (status_code, body) = call(&router, "UpdateStack", &members).await;
        assert_eq!(
            status_code, 200,
            "case {case:?}: expected UpdateStack to be accepted, got {status_code} {body}"
        );
        let settled = crate::stack_lifecycle::settled_stack(&router, case, row.stack).await;
        let status = xml_text(&settled, "StackStatus").unwrap_or_default();

        let Some(expected) = row.expected else {
            // AWS Lambda refuses a weight on the version the alias already invokes, so the
            // resource fails and the stack rolls itself back — carrying THAT service's own
            // refusal and not a wire-level parser message.
            assert_eq!(
                status, "UPDATE_ROLLBACK_COMPLETE",
                "case {case:?}: expected the refused weight to roll the stack back, got \
                 {status:?} ({settled})"
            );
            let reason =
                resource_event_reason(&router, row.stack, ALIAS_LOGICAL_ID, "UPDATE_FAILED").await;
            assert!(
                reason.contains("Invalid function version"),
                "case {case:?}: expected the resource's own event to carry AWS Lambda's refusal \
                 of the version already in the routing configuration, got {reason:?}"
            );
            continue;
        };

        // The resource's own event, read for the assertion's sake: a deployment that did NOT
        // complete says why on the resource that failed, so the message names it.
        let reported =
            resource_event_reason(&router, row.stack, ALIAS_LOGICAL_ID, "UPDATE_FAILED").await;
        assert_eq!(
            status, "UPDATE_COMPLETE",
            "case {case:?}: expected the weighted alias to deploy, got {status:?} \
             ({reported:?}) ({settled})"
        );
        let alias = alias_of(&router, &function).await;
        let weights = alias
            .get("RoutingConfig")
            .and_then(|config| config.get("AdditionalVersionWeights"))
            .cloned()
            .unwrap_or(Value::Null);
        assert_eq!(
            weights,
            expected(),
            "case {case:?}: expected AWS Lambda to report the declared weights as {}, got \
             {weights} ({alias})",
            expected()
        );
        let invoked = alias.get("FunctionVersion").and_then(Value::as_str);
        assert_eq!(
            invoked,
            Some("2"),
            "case {case:?}: expected the alias to invoke the version the update published, got \
             {invoked:?}"
        );
    }
}
