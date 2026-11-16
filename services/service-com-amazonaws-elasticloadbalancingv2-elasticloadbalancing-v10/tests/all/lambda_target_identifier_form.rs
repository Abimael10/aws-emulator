// SPDX-License-Identifier: CC0-1.0

//! What a target of a `lambda` target group may be WRITTEN as, and how this API refuses a
//! value that is not.
//!
//! RULE: `TargetDescription$Id` is "if the target type is lambda, specify the ARN of the
//! Lambda function", so a value that is not a Lambda function ARN names no target of any
//! account — and Elastic Load Balancing refuses it with `ValidationError` (400) and the
//! ENTIRELY FIXED sentence [`INVALID_LAMBDA_FUNCTION_ARN_MESSAGE`], which quotes nothing of
//! the request. The verdict is on the FORM of the id, so it does not depend on what is
//! being asked of it: the registration that adds a target and the deregistration that
//! removes one are refused alike.
//!
//! The rows span what the rule generalises over: a bare name, an ARN of another vendor, an
//! ARN of another resource type of the same vendor, an ARN missing fields, and the empty
//! value — each over both operations — against the two forms a Lambda function ARN takes
//! (with and without a version qualifier), which must be accepted.

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_elasticloadbalancingv2_elasticloadbalancing_v10::state::INVALID_LAMBDA_FUNCTION_ARN_MESSAGE;

use crate::provisioning_loop::{account, action, call, p, Reply, REGION};
use crate::target_group_attributes_and_modification::unescaped;
use crate::wire::router;

/// The code this API answers an input it cannot accept with.
const VALIDATION_ERROR: &str = "ValidationError";

/// The two operations that take a `Targets` list, which the rule reaches alike.
const OPERATIONS: [&str; 2] = ["RegisterTargets", "DeregisterTargets"];

/// Create a `lambda` target group and answer its ARN.
async fn create_lambda_target_group(router: &MultiServiceRouter, name: &str) -> String {
    let mut form = action("CreateTargetGroup");
    form.push(p("Name", name));
    form.push(p("TargetType", "lambda"));
    let reply = call(router, &form).await;
    assert_eq!(
        reply.status, 200,
        "case {name:?}: CreateTargetGroup must answer 200, got {} {}",
        reply.status, reply.body
    );
    reply
        .text("TargetGroupArn")
        .unwrap_or_else(|| panic!("case {name:?}: no TargetGroupArn in {}", reply.body))
}

/// Name `id` as the single target of `group` through `operation`.
async fn with_target(
    router: &MultiServiceRouter,
    operation: &str,
    group: &str,
    id: &str,
) -> Reply {
    let mut form = action(operation);
    form.push(p("TargetGroupArn", group));
    form.push(p("Targets.member.1.Id", id));
    call(router, &form).await
}

/// A Lambda function ARN of this account, in `REGION`.
fn function(name: &str) -> String {
    format!("arn:aws:lambda:{REGION}:{}:function:{name}", account())
}

#[tokio::test]
async fn a_lambda_target_that_is_no_function_arn_is_refused_in_the_words_aws_states() {
    let router = router();
    // (case, the id the request carries)
    let refused: Vec<(&str, String)> = vec![
        ("a bare function name", "greeter".to_string()),
        (
            "an ARN of another vendor",
            format!("arn:aws:states:{REGION}:{}:function:greeter", account()),
        ),
        (
            "an ARN of another resource type of the same vendor",
            format!("arn:aws:lambda:{REGION}:{}:layer:greeter", account()),
        ),
        (
            "an ARN missing its resource field",
            format!("arn:aws:lambda:{REGION}:{}", account()),
        ),
        ("an instance id", "i-0123456789abcdef0".to_string()),
        ("an IP address", "10.0.1.7".to_string()),
    ];

    for (index, (case, id)) in refused.iter().enumerate() {
        for operation in OPERATIONS {
            // A FRESH group per row and operation, so no row is answered for a group
            // another row filled: a `lambda` group holds one target.
            let group =
                create_lambda_target_group(&router, &format!("lambda-bad-{index}-{operation}"))
                    .await;
            let reply = with_target(&router, operation, &group, id).await;
            let stated = unescaped(&reply.text("Message").unwrap_or_default());
            assert_eq!(
                (reply.status, reply.error_code(), stated.as_str()),
                (
                    400,
                    VALIDATION_ERROR.to_string(),
                    INVALID_LAMBDA_FUNCTION_ARN_MESSAGE
                ),
                "case {case:?} on {operation}: expected 400 {VALIDATION_ERROR} \
                 {INVALID_LAMBDA_FUNCTION_ARN_MESSAGE:?}, got {}",
                reply.body
            );
            // The sentence carries nothing of the request: it is the form that was wrong,
            // not one value the caller can be shown back.
            assert!(
                !stated.contains(id.as_str()),
                "case {case:?} on {operation}: the sentence must quote nothing of the \
                 request, got {stated:?}"
            );
            // And nothing was registered for it.
            let mut health = action("DescribeTargetHealth");
            health.push(p("TargetGroupArn", &group));
            let reported = call(&router, &health).await;
            assert!(
                crate::provisioning_loop::members(&reported.body, "TargetHealthDescriptions")
                    .is_empty(),
                "case {case:?} on {operation}: a refused call must leave the group empty, \
                 got {}",
                reported.body
            );
        }
    }
}

#[tokio::test]
async fn a_lambda_function_arn_is_registered_and_deregistered() {
    let router = router();
    // The two forms `TargetDescription$Id` admits for a `lambda` group: the function's ARN,
    // and that ARN with a version qualifier.
    let accepted: [(&str, String); 2] = [
        ("a function ARN", function("greeter")),
        (
            "a function ARN with a version qualifier",
            format!("{}:3", function("greeter")),
        ),
    ];
    for (index, (case, id)) in accepted.iter().enumerate() {
        let group =
            create_lambda_target_group(&router, &format!("lambda-good-{index}")).await;
        let registered = with_target(&router, "RegisterTargets", &group, id).await;
        assert_eq!(
            registered.status, 200,
            "case {case:?}: a function ARN must be registered, got {} {}",
            registered.status, registered.body
        );

        // Read back where a client reads it: the registration the group now holds.
        let mut health = action("DescribeTargetHealth");
        health.push(p("TargetGroupArn", &group));
        let reported = call(&router, &health).await;
        let ids: Vec<String> =
            crate::provisioning_loop::members(&reported.body, "TargetHealthDescriptions")
                .iter()
                .filter_map(|member| crate::provisioning_loop::first_text(member, "Id"))
                .collect();
        assert_eq!(
            ids,
            vec![id.clone()],
            "case {case:?}: the group must report the target registered, got {}",
            reported.body
        );

        let deregistered = with_target(&router, "DeregisterTargets", &group, id).await;
        assert_eq!(
            deregistered.status, 200,
            "case {case:?}: the same ARN must be deregistered, got {} {}",
            deregistered.status, deregistered.body
        );
    }
}
