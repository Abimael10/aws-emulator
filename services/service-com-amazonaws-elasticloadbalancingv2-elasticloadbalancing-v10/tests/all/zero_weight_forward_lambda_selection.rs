// SPDX-License-Identifier: CC0-1.0

//! WHICH target's own answer a weighted `forward` over `lambda` target groups serves, end to
//! end over the wire, when the groups of the action carry DIFFERENT targets.
//!
//! The rules under test:
//!
//! 1. **A `forward` action distributes the requests it is matched for across the target groups
//!    it names in proportion to each group's `Weight`, so a group whose weight is 0 is never
//!    selected while another group of the same action carries a positive one — and the answer
//!    the client receives is the one produced by the target REGISTERED IN the group that was
//!    selected, so shifting the weights shifts which target answers and nothing else.**
//! 2. **Those weights are relative to one another, so a forward whose named groups' weights
//!    total zero can select no group at all: `ModifyListener` refuses it with HTTP 400 and
//!    `ValidationError`, and the listener keeps the weighting it held, which keeps serving.**
//!
//! What this suite adds to `all/weighted_forward_and_target_group_stickiness.rs`, which pins
//! the same two rules with ONE function registered in every group: here each group registers
//! its OWN function, each answering with its own marker, so the body a row reads can only have
//! come from the function the SELECTED group registers. A forward that chose the right group
//! but invoked another group's target would answer the wrong marker, and a forward that
//! answered without invoking anything would answer no marker at all.
//!
//! Every case drives the real wire path: an HTTP request to the name the load balancer
//! published, carrying no AWS credential, exactly as a client of that name sends it. The
//! functions are real deployment packages created through Lambda's own API and RUN by the
//! Lambda emulator mounted beside this service, and every weighting is written through
//! `ModifyListener` and read back through `DescribeListeners`.
//!
//! The rows differ along what rule 1 generalises over: WHERE the zero weight sits among the
//! groups the action names (second, first), and the positive weight the surviving group carries
//! (the lowest published, the highest published, and none stated at all). The SHARE two
//! positive weights each receive is not a row here: it is pinned draw by draw in
//! `src/service/target_group_selection.rs` and over the wire in the sibling suite above, and a
//! row of it would rest on a draw rather than on the certainty a zero weight gives.

use std::io::Write as _;
use std::time::{Duration, SystemTime};

use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use crate::alb_data_plane::{create_load_balancer, send, settle};
use crate::alb_lambda_target::emulator;
use crate::alb_lambda_target_published_address::{activate_functions, lambda, HANDLER, RUNTIME};
use crate::provisioning_loop::{action, call, first_text, members, p, Reply};

/// A well-formed execution role ARN, satisfying the model's `RoleArn` pattern. No IAM is
/// mounted here, so nothing is known about it and nothing is refused for it.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// The error code this service answers a weighting it refuses with.
const VALIDATION_ERROR: &str = "ValidationError";

/// The refusal a forward whose weights leave no group to select earns, which is about the
/// action's weights as a whole rather than about any one value of the request.
const WEIGHTS_MESSAGE: &str = "The sum of the weights of target groups must be greater than 0";

/// The weight a forward's target group is distributed at when the request states none for it,
/// which is what a row leaving the member out is REPORTED at. Pinned on its own in
/// `all/forward_action_config_and_mutual_authentication.rs`.
const UNSTATED_WEIGHT: i32 = 1;

/// The groups of this suite, each with the function it registers and the marker that function
/// answers with. The marker is the group's own, so the body of an answer names the group whose
/// target produced it and nothing else.
const DEPLOYED: &[(&str, &str, &str)] = &[
    ("zero-weight-first", "zero-weight-fn-first", "answered-by-first"),
    ("zero-weight-second", "zero-weight-fn-second", "answered-by-second"),
];

/// The handler one function runs: it answers `marker` in the ALB Lambda-target response
/// shape, so the body the client receives is this function's own.
fn handler_source(marker: &str) -> String {
    format!(
        r#"
def lambda_handler(event, context):
    return {{
        "statusCode": 200,
        "statusDescription": "200 OK",
        "headers": {{"content-type": "text/plain"}},
        "body": "{marker}",
        "isBase64Encoded": False,
    }}
"#
    )
}

/// A .zip deployment package holding one marker's handler as `lambda_function.py`, written in
/// memory the way a caller uploads one.
fn package(marker: &str) -> Vec<u8> {
    let mut buffer = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buffer);
        let options: zip::write::FileOptions<'_, '_, ()> = zip::write::FileOptions::default();
        writer
            .start_file("lambda_function.py", options)
            .expect("the entry starts");
        writer
            .write_all(handler_source(marker).as_bytes())
            .expect("the entry's bytes");
        writer.finish().expect("a finished archive");
    }
    buffer.into_inner()
}

/// The weighting one row writes onto the listener's default action: one entry per target group
/// it names, holding that group's index below and the `Weight` the request states for it —
/// `None` leaving the member out.
type Weighting = &'static [(usize, Option<i32>)];

/// The request fields of one weighted forward as a listener's default action.
fn forward_fields(weighting: Weighting, groups: &[String]) -> Vec<(String, String)> {
    let prefix = "DefaultActions.member.1.";
    let mut form = vec![p(&format!("{prefix}Type"), "forward")];
    for (member, (index, weight)) in weighting.iter().enumerate() {
        let at = member + 1;
        form.push(p(
            &format!("{prefix}ForwardConfig.TargetGroups.member.{at}.TargetGroupArn"),
            &groups[*index],
        ));
        if let Some(weight) = weight {
            form.push(p(
                &format!("{prefix}ForwardConfig.TargetGroups.member.{at}.Weight"),
                &weight.to_string(),
            ));
        }
    }
    form
}

/// Ask `ModifyListener` to write `weighting` onto the listener, the way a client shifts
/// traffic between two groups, and answer what the call answered.
async fn modify_reply(
    router: &MultiServiceRouter,
    listener_arn: &str,
    groups: &[String],
    weighting: Weighting,
) -> Reply {
    let mut form = action("ModifyListener");
    form.push(p("ListenerArn", listener_arn));
    form.extend(forward_fields(weighting, groups));
    call(router, &form).await
}

/// The `(TargetGroupArn, Weight)` pairs the listener's one default action is REPORTED at, in
/// the order it reports them.
async fn reported_weights(
    router: &MultiServiceRouter,
    case: &str,
    listener_arn: &str,
) -> Vec<(String, String)> {
    let mut form = action("DescribeListeners");
    form.push(p("ListenerArns.member.1", listener_arn));
    let reply = call(router, &form).await;
    assert_eq!(
        reply.status, 200,
        "case {case:?}: reading the listener back: expected 200, got {} body {}",
        reply.status, reply.body
    );
    let listeners = members(&reply.body, "Listeners");
    let actions = listeners
        .first()
        .map(|listener| members(listener, "DefaultActions"))
        .unwrap_or_default();
    let reported = actions
        .first()
        .unwrap_or_else(|| panic!("case {case:?}: no default action in {}", reply.body));
    members(reported, "TargetGroups")
        .iter()
        .map(|tuple| {
            (
                first_text(tuple, "TargetGroupArn").unwrap_or_default(),
                first_text(tuple, "Weight").unwrap_or_default(),
            )
        })
        .collect()
}

/// The pairs a weighting must be reported at once it is accepted: the groups in the order the
/// request named them, each at the weight it stated or at [`UNSTATED_WEIGHT`].
fn expected_weights(weighting: Weighting, groups: &[String]) -> Vec<(String, String)> {
    weighting
        .iter()
        .map(|(index, weight)| {
            (
                groups[*index].clone(),
                weight.unwrap_or(UNSTATED_WEIGHT).to_string(),
            )
        })
        .collect()
}

/// One row of the weighting table.
struct Row {
    case: &'static str,
    weighting: Weighting,
    /// How many requests the row sends.
    requests: usize,
    /// The groups, by index, whose marker may answer any of them.
    may_answer: &'static [usize],
    /// The groups, by index, whose marker must answer at least one of them.
    must_answer: &'static [usize],
}

/// The rows: where the zero weight sits, and what the surviving group carries.
///
/// Every row leaves exactly ONE group with a positive weight, so the group that must answer is
/// certain and the drained group is missed with CERTAINTY if the rule holds — a few requests
/// settle each row, and no row rests on a draw. A forward that ignored the weights would answer
/// the drained group's marker within a few requests, and one that invoked the wrong group's
/// target would answer the wrong marker on the first.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the drained group named second",
            weighting: &[(0, Some(1)), (1, Some(0))],
            requests: 3,
            may_answer: &[0],
            must_answer: &[0],
        },
        Row {
            // The shift a client makes to move traffic between two groups: the same two
            // groups, the weights swapped.
            case: "the mirror: the weights shifted onto the second group",
            weighting: &[(0, Some(0)), (1, Some(1))],
            requests: 3,
            may_answer: &[1],
            must_answer: &[1],
        },
        Row {
            // `TargetGroupTuple$Weight` is "[t]he weight. The range is 0 to 999", so this
            // carries the surviving group at the highest published weight.
            case: "the published weight ceiling beside a drained group",
            weighting: &[(0, Some(999)), (1, Some(0))],
            requests: 3,
            may_answer: &[0],
            must_answer: &[0],
        },
        Row {
            case: "a group whose weight the request leaves out, beside a drained one",
            weighting: &[(0, None), (1, Some(0))],
            requests: 3,
            may_answer: &[0],
            must_answer: &[0],
        },
    ]
}

/// The group, by index, whose marker the answer carries, or `None` for an answer no target
/// produced.
fn answered_group(body: &str) -> Option<usize> {
    DEPLOYED
        .iter()
        .position(|(_, _, marker)| body.trim() == *marker)
}

#[tokio::test]
async fn a_zero_weight_target_group_is_never_the_one_whose_target_answers() {
    let (router, engine) = emulator();

    let (lb_arn, dns_name) = create_load_balancer(&router, "zero-weight-alb").await;

    // One `lambda` target group per deployed function: the function is created through
    // Lambda's own API, registered as that group's single target, and grants the load
    // balancer the invocation for THAT group alone.
    let mut groups: Vec<String> = Vec::new();
    for (group_name, function, marker) in DEPLOYED {
        let (status, created) = lambda(
            &router,
            "POST",
            "/2015-03-31/functions",
            json!({
                "FunctionName": function,
                "Role": ROLE,
                "Runtime": RUNTIME,
                "Handler": HANDLER,
                "Timeout": 30,
                "Code": { "ZipFile": aws_smithy_types::base64::encode(package(marker)) },
            }),
        )
        .await;
        assert_eq!(
            status, 201,
            "setup CreateFunction {function}: expected 201, got {status} {created}"
        );
        let function_arn = created["FunctionArn"]
            .as_str()
            .unwrap_or_else(|| panic!("setup CreateFunction {function}: no FunctionArn in {created}"))
            .to_string();

        let mut form = action("CreateTargetGroup");
        form.push(p("Name", group_name));
        form.push(p("TargetType", "lambda"));
        let reply = call(&router, &form).await;
        assert_eq!(
            reply.status, 200,
            "setup CreateTargetGroup {group_name}: expected 200, got {} body {}",
            reply.status, reply.body
        );
        let tg_arn = reply.text("TargetGroupArn").unwrap_or_else(|| {
            panic!(
                "setup CreateTargetGroup {group_name}: no TargetGroupArn in {}",
                reply.body
            )
        });

        let mut form = action("RegisterTargets");
        form.push(p("TargetGroupArn", &tg_arn));
        form.push(p("Targets.member.1.Id", &function_arn));
        let reply = call(&router, &form).await;
        assert_eq!(
            reply.status, 200,
            "setup RegisterTargets {group_name}: expected 200, got {} body {}",
            reply.status, reply.body
        );

        let (status, granted) = lambda(
            &router,
            "POST",
            &format!("/2015-03-31/functions/{function}/policy"),
            json!({
                "StatementId": format!("elbv2-invoke-{group_name}"),
                "Action": "lambda:InvokeFunction",
                "Principal": "elasticloadbalancing.amazonaws.com",
                "SourceArn": tg_arn,
            }),
        )
        .await;
        assert_eq!(
            status, 201,
            "setup AddPermission {function}: expected 201, got {status} {granted}"
        );
        groups.push(tg_arn);
    }

    // The activation span Lambda owes a freshly created function, and the move to `active`
    // the clock owes the load balancer: both placed in the past rather than slept for.
    activate_functions(&engine);

    // The HTTP:80 listener whose default action forwards to the first two groups, the first
    // weighting the rows then replace.
    let mut form = action("CreateListener");
    form.push(p("LoadBalancerArn", &lb_arn));
    form.push(p("Protocol", "HTTP"));
    form.push(p("Port", "80"));
    form.extend(forward_fields(&[(0, Some(1)), (1, Some(0))], &groups));
    let reply = call(&router, &form).await;
    assert_eq!(
        reply.status, 200,
        "setup CreateListener: expected 200, got {} body {}",
        reply.status, reply.body
    );
    let listener_arn = reply
        .text("ListenerArn")
        .unwrap_or_else(|| panic!("setup CreateListener: no ListenerArn in {}", reply.body));

    settle(&engine, &lb_arn, SystemTime::now() - Duration::from_secs(1));

    for row in rows() {
        let written = modify_reply(&router, &listener_arn, &groups, row.weighting).await;
        assert_eq!(
            written.status, 200,
            "case {:?}: expected ModifyListener to answer 200, got {} body {}",
            row.case, written.status, written.body
        );
        let stored = reported_weights(&router, row.case, &listener_arn).await;
        let expected = expected_weights(row.weighting, &groups);
        assert_eq!(
            stored, expected,
            "case {:?}: the listener must report the weighting it was given, expected \
             {expected:?}, got {stored:?}",
            row.case
        );

        let mut answered_by: Vec<Option<usize>> = Vec::new();
        for _ in 0..row.requests {
            let answered = send(&router, "GET", &dns_name, "/", &[]).await;
            assert_eq!(
                answered.status, 200,
                "case {:?}: expected HTTP 200 from the published name, got {} with body {:?}",
                row.case, answered.status, answered.body
            );
            answered_by.push(answered_group(&answered.body));
        }
        for group in row.must_answer {
            assert!(
                answered_by.contains(&Some(*group)),
                "case {:?}: the target of group {group} must answer at least one of {} \
                 requests, got {answered_by:?}",
                row.case,
                row.requests
            );
        }
        for answered in &answered_by {
            let Some(group) = answered else {
                panic!(
                    "case {:?}: every answer must carry the marker of a group's own target, \
                     got {answered_by:?}",
                    row.case
                );
            };
            assert!(
                row.may_answer.contains(group),
                "case {:?}: the target of group {group} may answer none of these requests, \
                 got {answered_by:?}",
                row.case
            );
        }
    }

    // RULE 2: with the last row's weighting stored, a modification that drains EVERY group of
    // the action is refused, and the listener keeps the weighting it held — and keeps serving
    // it.
    let drained_last = rows().pop().expect("the table has rows");
    let all_drained: Weighting = &[(0, Some(0)), (1, Some(0))];
    let refused = modify_reply(&router, &listener_arn, &groups, all_drained).await;
    assert_eq!(
        refused.status, 400,
        "case every weight zero: expected HTTP 400, got {} body {}",
        refused.status, refused.body
    );
    assert_eq!(
        refused.error_code(),
        VALIDATION_ERROR,
        "case every weight zero: expected the error code {VALIDATION_ERROR}, got {}",
        refused.body
    );
    assert_eq!(
        refused.text("Message").unwrap_or_default(),
        WEIGHTS_MESSAGE,
        "case every weight zero: expected the message {WEIGHTS_MESSAGE:?}, got {}",
        refused.body
    );
    let kept = reported_weights(&router, "every weight zero", &listener_arn).await;
    let expected = expected_weights(drained_last.weighting, &groups);
    assert_eq!(
        kept, expected,
        "case every weight zero: the refused change reaches no listener, expected the stored \
         weights {expected:?}, got {kept:?}"
    );
    let answered = send(&router, "GET", &dns_name, "/", &[]).await;
    let group = answered_group(&answered.body);
    assert_eq!(
        answered.status, 200,
        "case every weight zero: the listener keeps serving the weighting it held, got {} with \
         body {:?}",
        answered.status, answered.body
    );
    assert!(
        group.is_some_and(|group| drained_last.may_answer.contains(&group)),
        "case every weight zero: the weighting the listener kept must keep serving, expected \
         the target of one of {:?}, got {:?}",
        drained_last.may_answer,
        answered.body
    );
}
