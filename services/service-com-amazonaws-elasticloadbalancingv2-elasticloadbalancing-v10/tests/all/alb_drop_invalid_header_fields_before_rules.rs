// SPDX-License-Identifier: CC0-1.0

//! Where on the request path an Application Load Balancer carrying
//! `routing.http.drop_invalid_header_fields.enabled` at `true` removes the headers whose names
//! are no header field name — and that the request it removed them from is still SERVED.
//!
//! The rule under test: **the removal happens where the load balancer RECEIVES the request, so
//! nothing behind the load balancer is handed a field it removed — neither the conditions of
//! the rules its listener evaluates nor the target the request is forwarded to — while every
//! header of the same request whose name IS a field name (the alphanumeric characters and the
//! hyphen) travels on untouched and the request is answered out of the action that then
//! matches.** `LoadBalancerAttribute$Key` gives the switch: "`routing.http.drop_invalid_
//! header_fields.enabled` - Indicates whether HTTP headers with invalid header fields are
//! removed by the load balancer (true) or routed to targets (false). The default is false."
//!
//! `tests/all/alb_drop_invalid_header_fields.rs` pins what the TARGET is handed. This suite
//! pins the step BEFORE the target: an `http-header` condition — "the name of the HTTP header
//! field … the allowed characters are specified by RFC 7230"
//! (`HttpHeaderConditionConfig$HttpHeaderName`), whose token grammar admits the underscore,
//! the dot and the other punctuation a header field name is not written in — is met while the
//! load balancer routes such a header to its targets, and is NOT met once the load balancer
//! removes it, so the same request is answered by the listener's default action instead. A
//! condition on a name that IS a field name is met whatever the attribute says.
//!
//! The rows span what the rule generalises over: the character classes a field name can be
//! written in (letters of either case, digits, the hyphen) and one row per class of character
//! RFC 7230's token grammar admits that a field name is not written in (the underscore, the
//! dot, and the other token punctuation), each conditioned on by a rule of its own and each
//! sent under both states of the attribute.
//!
//! Every case drives the real wire path — an HTTP request through the multi-service router to
//! the published `DNSName`, carrying no AWS credential, exactly as a client sends it — and
//! every rule, listener, target group and registration is written through this service's own
//! control-plane operations, through the crate's existing helpers for them rather than a
//! second spelling of each call. The target the default action forwards to is a Lambda
//! function created through Lambda's own API and run by the Lambda emulator mounted on this
//! suite's engine, so a request that reached the target reports the headers the TARGET saw.

use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::alb_data_plane::{create_load_balancer, send, settle};
use crate::alb_drop_invalid_header_fields::{
    emulator, lambda, modify as modify_attribute, package, HANDLER, ROLE, RUNTIME,
};
use crate::alb_lambda_target_published_address::activate_functions;
use crate::alb_target_metric_rate_alarms::lambda_target_group;
use crate::provisioning_loop::{action, call, p};

/// The attribute that decides whether the headers below reach anything behind the load
/// balancer.
const ATTRIBUTE: &str = "routing.http.drop_invalid_header_fields.enabled";

/// The function the listener's DEFAULT action forwards to: it echoes the headers of the event
/// it was handed, so a request that got past every rule reports what the target saw.
const FUNCTION: &str = "alb-rule-header-echo";

/// The value every row's header is sent with, and the value every row's rule condition
/// compares against — so what decides whether the condition is met is the NAME alone.
const SENT_VALUE: &str = "rule-match";

/// The valid-named header every request also carries, which no rule conditions on: it proves
/// the request still travelled with its remaining headers once the invalid-named one was
/// removed.
const KEPT_NAME: &str = "x-rule-keep";

/// The value [`KEPT_NAME`] is sent with.
const KEPT_VALUE: &str = "kept-alongside";

/// One row: the case, the header name both the client sends and one rule conditions on, and
/// whether that name is written in the character set a header field name is written in — which
/// is what decides whether the load balancer removes it.
struct Row {
    case: &'static str,
    name: &'static str,
    valid: bool,
}

/// The rows, spanning the character classes a field name can be written in and the classes of
/// character RFC 7230's token grammar admits that it cannot.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a hyphenated name",
            name: "x-rule-good",
            valid: true,
        },
        Row {
            case: "mixed case and digits",
            name: "X-Rule-42",
            valid: true,
        },
        Row {
            case: "letters only, with no separator at all",
            name: "xrule",
            valid: true,
        },
        Row {
            case: "an underscore, as an application framework spells a variable",
            name: "x_rule_bad",
            valid: false,
        },
        Row {
            case: "a dot",
            name: "x.rule.bad",
            valid: false,
        },
        Row {
            case: "a dollar sign, which RFC 7230's token grammar admits",
            name: "x$rule",
            valid: false,
        },
        Row {
            case: "a tilde, which RFC 7230's token grammar admits",
            name: "x~rule",
            valid: false,
        },
        Row {
            case: "a plus sign, which RFC 7230's token grammar admits",
            name: "x+rule",
            valid: false,
        },
    ]
}

/// The body the rule conditioned on `name` answers with, which is how a met condition is told
/// apart from every other row's.
fn matched_body(name: &str) -> String {
    format!("matched {name}")
}

/// Write one state of the attribute through the control plane, which must be accepted — the
/// same write the sibling suite makes, through its own helper rather than a second copy.
async fn modify(router: &MultiServiceRouter, lb_arn: &str, enabled: bool) {
    let value = enabled.to_string();
    let (status, body) = modify_attribute(router, lb_arn, &value).await;
    assert_eq!(
        status, 200,
        "setup ModifyLoadBalancerAttributes to {value:?}: expected 200, got {status} body {body}"
    );
}

#[tokio::test]
async fn an_invalid_header_field_is_removed_before_the_listener_rules_are_evaluated() {
    let (router, engine) = emulator();

    // The target the listener's default action forwards to: a function created through
    // Lambda's own API, echoing the headers of the event it is handed.
    let (status, created) = lambda(
        &router,
        "POST",
        "/2015-03-31/functions",
        json!({
            "FunctionName": FUNCTION,
            "Role": ROLE,
            "Runtime": RUNTIME,
            "Handler": HANDLER,
            "Timeout": 30,
            "Code": { "ZipFile": aws_smithy_types::base64::encode(package()) },
        }),
    )
    .await;
    assert_eq!(
        status, 201,
        "setup CreateFunction: expected 201, got {status} {created}"
    );
    let function_arn = created["FunctionArn"]
        .as_str()
        .unwrap_or_else(|| panic!("setup CreateFunction: no FunctionArn in {created}"))
        .to_string();
    // The activation span Lambda owes the function, placed rather than slept for, through the
    // crate's one reading of it.
    activate_functions(&engine);

    let (lb_arn, dns_name) = create_load_balancer(&router, "rule-headers-alb").await;

    // The `lambda` target group, the registration and the grant the invocation needs: the
    // crate's one spelling of those three calls.
    let tg_arn = lambda_target_group(&router, "rule-headers-tg", FUNCTION, &function_arn).await;

    let mut form = action("CreateListener");
    form.push(p("LoadBalancerArn", &lb_arn));
    form.push(p("Protocol", "HTTP"));
    form.push(p("Port", "80"));
    form.push(p("DefaultActions.member.1.Type", "forward"));
    form.push(p("DefaultActions.member.1.TargetGroupArn", &tg_arn));
    let reply = call(&router, &form).await;
    assert_eq!(
        reply.status, 200,
        "setup CreateListener: expected 200, got {} body {}",
        reply.status, reply.body
    );
    let listener_arn = reply
        .text("ListenerArn")
        .unwrap_or_else(|| panic!("setup CreateListener: no ListenerArn in {}", reply.body));

    let rows = rows();

    // One rule per row, each conditioned on that row's header name alone and answering a body
    // naming it, so a met condition is told apart from every other row's and from the default
    // action's forward.
    for (index, row) in rows.iter().enumerate() {
        let priority = (index + 1) * 10;
        let mut form = action("CreateRule");
        form.push(p("ListenerArn", &listener_arn));
        form.push(p("Priority", &priority.to_string()));
        form.push(p("Conditions.member.1.Field", "http-header"));
        form.push(p(
            "Conditions.member.1.HttpHeaderConfig.HttpHeaderName",
            row.name,
        ));
        form.push(p(
            "Conditions.member.1.HttpHeaderConfig.Values.member.1",
            SENT_VALUE,
        ));
        form.push(p("Actions.member.1.Type", "fixed-response"));
        form.push(p("Actions.member.1.FixedResponseConfig.StatusCode", "503"));
        form.push(p(
            "Actions.member.1.FixedResponseConfig.MessageBody",
            &matched_body(row.name),
        ));
        let reply = call(&router, &form).await;
        assert_eq!(
            reply.status, 200,
            "setup CreateRule for {:?}: expected 200, got {} body {}",
            row.case, reply.status, reply.body
        );
    }

    // The move to `active` the clock owes the load balancer, placed rather than slept for.
    settle(&engine, &lb_arn, SystemTime::now() - Duration::from_secs(1));

    // Each state of the attribute in turn, written through the control plane so the legs differ
    // in nothing but the attribute's value. The last leg writes it back OFF, where the rows the
    // enabled leg saw forwarded meet their conditions again: the removal is read per request
    // from what the attribute holds, so turning it on does not latch.
    for enabled in [false, true, false] {
        modify(&router, &lb_arn, enabled).await;
        for row in &rows {
            // The row's header, plus one valid-named header no rule conditions on.
            let headers = [(row.name, SENT_VALUE), (KEPT_NAME, KEPT_VALUE)];
            let answered = send(&router, "GET", &dns_name, "/", &headers).await;
            // A header the load balancer removed is not there for the condition to meet, so
            // the rule is passed over and the listener's default action forwards instead.
            let removed = enabled && !row.valid;
            if removed {
                assert_eq!(
                    answered.status, 200,
                    "case {:?} with {ATTRIBUTE} at {enabled}: the removed header meets no \
                     condition, so the default forward answers the target's 200, got {} with \
                     body {:?}",
                    row.case, answered.status, answered.body
                );
                let echoed: Value = serde_json::from_str(&answered.body).unwrap_or_else(|error| {
                    panic!(
                        "case {:?} with {ATTRIBUTE} at {enabled}: the target's echo is not JSON \
                         ({error}) in {:?}",
                        row.case, answered.body
                    )
                });
                let seen = match echoed {
                    Value::Object(headers) => headers,
                    other => panic!(
                        "case {:?} with {ATTRIBUTE} at {enabled}: the target's echo is no \
                         object, got {other}",
                        row.case
                    ),
                };
                let lowered = row.name.to_ascii_lowercase();
                let delivered = seen.get(&lowered).and_then(Value::as_str);
                assert_eq!(
                    delivered, None,
                    "case {:?} with {ATTRIBUTE} at {enabled}: the removed header reaches no \
                     target either, expected {lowered:?} absent, got {delivered:?} in {seen:?}",
                    row.case
                );
                let kept = seen.get(KEPT_NAME).and_then(Value::as_str);
                assert_eq!(
                    kept,
                    Some(KEPT_VALUE),
                    "case {:?} with {ATTRIBUTE} at {enabled}: the valid-named header of the \
                     same request travels on untouched, expected {KEPT_VALUE:?}, got {kept:?} \
                     in {seen:?}",
                    row.case
                );
            } else {
                assert_eq!(
                    answered.status, 503,
                    "case {:?} with {ATTRIBUTE} at {enabled}: the header reaches the \
                     condition, so its rule answers, got {} with body {:?}",
                    row.case, answered.status, answered.body
                );
                let expected = matched_body(row.name);
                assert_eq!(
                    answered.body, expected,
                    "case {:?} with {ATTRIBUTE} at {enabled}: expected the rule conditioned on \
                     that name to answer {expected:?}, got {:?}",
                    row.case, answered.body
                );
            }
        }
    }
}

/// The same load balancer, the same client request, and a listener whose ONLY rule conditions
/// on a valid-named header: the attribute removes nothing that condition reads, so the rule
/// answers under both of its states. The leg that proves the attribute is about the NAME and
/// not about conditioning on headers at all.
#[tokio::test]
async fn a_valid_header_field_still_meets_its_condition_under_both_states_of_the_attribute() {
    let (router, engine) = emulator();
    let (lb_arn, dns_name) = create_load_balancer(&router, "rule-valid-header-alb").await;

    let mut form = action("CreateListener");
    form.push(p("LoadBalancerArn", &lb_arn));
    form.push(p("Protocol", "HTTP"));
    form.push(p("Port", "80"));
    form.push(p("DefaultActions.member.1.Type", "fixed-response"));
    form.push(p(
        "DefaultActions.member.1.FixedResponseConfig.StatusCode",
        "404",
    ));
    form.push(p(
        "DefaultActions.member.1.FixedResponseConfig.MessageBody",
        "no-match",
    ));
    let reply = call(&router, &form).await;
    assert_eq!(
        reply.status, 200,
        "setup CreateListener: expected 200, got {} body {}",
        reply.status, reply.body
    );
    let listener_arn = reply
        .text("ListenerArn")
        .unwrap_or_else(|| panic!("setup CreateListener: no ListenerArn in {}", reply.body));

    let mut form = action("CreateRule");
    form.push(p("ListenerArn", &listener_arn));
    form.push(p("Priority", "10"));
    form.push(p("Conditions.member.1.Field", "http-header"));
    form.push(p(
        "Conditions.member.1.HttpHeaderConfig.HttpHeaderName",
        KEPT_NAME,
    ));
    form.push(p(
        "Conditions.member.1.HttpHeaderConfig.Values.member.1",
        KEPT_VALUE,
    ));
    form.push(p("Actions.member.1.Type", "fixed-response"));
    form.push(p("Actions.member.1.FixedResponseConfig.StatusCode", "503"));
    form.push(p(
        "Actions.member.1.FixedResponseConfig.MessageBody",
        &matched_body(KEPT_NAME),
    ));
    let reply = call(&router, &form).await;
    assert_eq!(
        reply.status, 200,
        "setup CreateRule: expected 200, got {} body {}",
        reply.status, reply.body
    );

    settle(&engine, &lb_arn, SystemTime::now() - Duration::from_secs(1));

    // Both states of the attribute: the condition reads a name the attribute never removes, so
    // the same rule answers either way.
    for enabled in [false, true] {
        modify(&router, &lb_arn, enabled).await;
        let answered = send(
            &router,
            "GET",
            &dns_name,
            "/",
            &[(KEPT_NAME, KEPT_VALUE), ("x_rule_bad", SENT_VALUE)],
        )
        .await;
        let expected = matched_body(KEPT_NAME);
        assert_eq!(
            answered.status, 503,
            "case a valid-named condition with {ATTRIBUTE} at {enabled}: expected the rule to \
             answer 503, got {} with body {:?}",
            answered.status, answered.body
        );
        assert_eq!(
            answered.body, expected,
            "case a valid-named condition with {ATTRIBUTE} at {enabled}: expected {expected:?}, \
             got {:?}",
            answered.body
        );
    }
}
