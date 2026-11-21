// SPDX-License-Identifier: CC0-1.0

//! The response headers an Application Load Balancer's LISTENER sends and removes, end to
//! end: written through `ModifyListenerAttributes`, read back through
//! `DescribeListenerAttributes`, and observed on the answer a client receives on the name
//! the load balancer published.
//!
//! The rule under test: **the answer a listener sends carries the load balancer's own
//! `Server` header while `routing.http.response.server.enabled` allows it and carries no
//! such header once a caller removes it, and carries each response header one of its
//! `routing.http.response.<header>.header_value` attributes holds a value for — with the
//! value the attribute holds — under the header name that attribute's middle spells. An
//! attribute at the empty string, which is the value the collection reports until a caller
//! writes one, sends no header at all. It is the LISTENER's answer that is decided, so the
//! same headers are applied whatever produced it.**
//!
//! The rows span what the rule generalises over: every one of the ten response-header
//! attributes the catalog carries (`crate::state::LISTENER_ATTRIBUTES`, from the model's
//! `ListenerAttribute$Key` documentation) rather than only the two a recording happens to
//! name; the two states of the `Server` switch; the three states a header-value attribute
//! can be in (never written, written, written back to the empty string); and two actions
//! that produce the answer — a `fixed-response` and a `redirect`, whose status and
//! `Location` must survive the rewriting untouched.
//!
//! Every case drives the real wire path: the control plane through this service's own
//! awsQuery API, and the data plane as an HTTP request to the published `DNSName` carrying
//! no AWS credential, exactly as a client sends it.

use std::time::{Duration, SystemTime};

use smithy_server_runtime::MultiServiceRouter;

use crate::alb_data_plane::{create_load_balancer, send, settle};
use crate::provisioning_loop::{action, attribute_sequence, call, emulator, p};
use crate::target_group_attributes_and_modification::unescaped;

/// The attribute that decides whether the load balancer sends its own `Server` header.
const SERVER_ENABLED: &str = "routing.http.response.server.enabled";

/// What an Application Load Balancer names itself in that header.
const LOAD_BALANCER_SERVER: &str = "awselb/2.0";

/// The body the listener's default `fixed-response` action answers with.
const DEFAULT_BODY: &str = "default-action";

/// Where the rule's `redirect` action sends a client.
const REDIRECT_PATH: &str = "/moved";
const REDIRECT_HOST: &str = "elsewhere.example.com";

/// One row of the table: the attribute key, the header it configures, and the value a
/// caller writes it with.
///
/// The keys are every `routing.http.response.*.header_value` attribute the service's own
/// catalog carries (`crate::state::LISTENER_ATTRIBUTES`, from `ListenerAttribute$Key`),
/// and each header name is the one that attribute's documentation names.
struct Row {
    attribute: &'static str,
    header: &'static str,
    value: &'static str,
}

/// The whole family, each with a value of the kind the header it configures carries.
fn rows() -> Vec<Row> {
    vec![
        Row {
            attribute: "routing.http.response.strict_transport_security.header_value",
            header: "strict-transport-security",
            value: "max-age=31536000; includeSubDomains",
        },
        Row {
            attribute: "routing.http.response.access_control_allow_origin.header_value",
            header: "access-control-allow-origin",
            value: "https://app.example.com",
        },
        Row {
            attribute: "routing.http.response.access_control_allow_methods.header_value",
            header: "access-control-allow-methods",
            value: "GET,POST,OPTIONS",
        },
        Row {
            attribute: "routing.http.response.access_control_allow_headers.header_value",
            header: "access-control-allow-headers",
            value: "x-requested-with",
        },
        Row {
            attribute: "routing.http.response.access_control_allow_credentials.header_value",
            header: "access-control-allow-credentials",
            value: "true",
        },
        Row {
            attribute: "routing.http.response.access_control_expose_headers.header_value",
            header: "access-control-expose-headers",
            value: "x-amzn-trace-id",
        },
        Row {
            attribute: "routing.http.response.access_control_max_age.header_value",
            header: "access-control-max-age",
            value: "600",
        },
        Row {
            attribute: "routing.http.response.content_security_policy.header_value",
            header: "content-security-policy",
            value: "default-src 'self'",
        },
        Row {
            attribute: "routing.http.response.x_content_type_options.header_value",
            header: "x-content-type-options",
            value: "nosniff",
        },
        Row {
            attribute: "routing.http.response.x_frame_options.header_value",
            header: "x-frame-options",
            value: "SAMEORIGIN",
        },
    ]
}

/// Create the HTTP:80 listener whose default action is a `fixed-response`, and answer its
/// ARN.
async fn create_listener(router: &MultiServiceRouter, lb_arn: &str) -> String {
    let mut form = action("CreateListener");
    form.push(p("LoadBalancerArn", lb_arn));
    form.push(p("Protocol", "HTTP"));
    form.push(p("Port", "80"));
    form.push(p("DefaultActions.member.1.Type", "fixed-response"));
    form.push(p(
        "DefaultActions.member.1.FixedResponseConfig.StatusCode",
        "200",
    ));
    form.push(p(
        "DefaultActions.member.1.FixedResponseConfig.MessageBody",
        DEFAULT_BODY,
    ));
    let reply = call(router, &form).await;
    assert_eq!(
        reply.status, 200,
        "setup CreateListener: expected 200, got {} body {}",
        reply.status, reply.body
    );
    reply
        .text("ListenerArn")
        .unwrap_or_else(|| panic!("setup CreateListener: no ListenerArn in {}", reply.body))
}

/// Add the rule whose `redirect` action answers the second addressing of every case.
async fn create_redirect_rule(router: &MultiServiceRouter, listener_arn: &str) {
    let mut form = action("CreateRule");
    form.push(p("ListenerArn", listener_arn));
    form.push(p("Priority", "10"));
    form.push(p("Conditions.member.1.Field", "path-pattern"));
    form.push(p("Conditions.member.1.Values.member.1", REDIRECT_PATH));
    form.push(p("Actions.member.1.Type", "redirect"));
    form.push(p("Actions.member.1.RedirectConfig.Host", REDIRECT_HOST));
    form.push(p("Actions.member.1.RedirectConfig.StatusCode", "HTTP_301"));
    let reply = call(router, &form).await;
    assert_eq!(
        reply.status, 200,
        "setup CreateRule: expected 200, got {} body {}",
        reply.status, reply.body
    );
}

/// Write `writes` onto the listener's attributes, which must be accepted.
async fn modify(
    router: &MultiServiceRouter,
    case: &str,
    listener_arn: &str,
    writes: &[(&str, &str)],
) {
    let mut form = action("ModifyListenerAttributes");
    form.push(p("ListenerArn", listener_arn));
    for (index, (key, value)) in writes.iter().enumerate() {
        let member = index + 1;
        form.push(p(&format!("Attributes.member.{member}.Key"), key));
        form.push(p(&format!("Attributes.member.{member}.Value"), value));
    }
    let reply = call(router, &form).await;
    assert_eq!(
        reply.status, 200,
        "case {case:?}: expected 200 from ModifyListenerAttributes, got {} body {}",
        reply.status, reply.body
    );
}

/// What `DescribeListenerAttributes` reports one attribute at.
async fn described(router: &MultiServiceRouter, case: &str, listener_arn: &str, key: &str) -> String {
    let mut form = action("DescribeListenerAttributes");
    form.push(p("ListenerArn", listener_arn));
    let reply = call(router, &form).await;
    assert_eq!(
        reply.status, 200,
        "case {case:?}: expected 200 from DescribeListenerAttributes, got {} body {}",
        reply.status, reply.body
    );
    attribute_sequence(&reply.body)
        .into_iter()
        .find(|(held, _)| held == key)
        // The attribute values are a caller's own text, which the response carries XML
        // escaped: what the caller wrote is what it unescapes to.
        .map(|(_, value)| unescaped(&value))
        .unwrap_or_else(|| panic!("case {case:?}: no {key} in {}", reply.body))
}

#[tokio::test]
async fn a_listeners_attributes_decide_the_response_headers_a_client_is_answered_with() {
    let (router, engine) = emulator();
    let (lb_arn, dns_name) = create_load_balancer(&router, "response-headers-alb").await;
    let listener_arn = create_listener(&router, &lb_arn).await;
    create_redirect_rule(&router, &listener_arn).await;
    // The move to `active` the clock owes the load balancer, placed rather than slept for.
    settle(&engine, &lb_arn, SystemTime::now() - Duration::from_secs(1));

    let rows = rows();

    // The collection as it is carried, with nothing written: the load balancer sends its
    // own `Server` header, and no configured header at all.
    let carried = described(&router, "nothing written", &listener_arn, SERVER_ENABLED).await;
    assert_eq!(
        carried, "true",
        "case \"nothing written\": expected {SERVER_ENABLED} at \"true\", got {carried:?}"
    );
    for addressing in [("/", 200u16), (REDIRECT_PATH, 301)] {
        let (target, status) = addressing;
        let answered = send(&router, "GET", &dns_name, target, &[]).await;
        assert_eq!(
            answered.status, status,
            "case \"nothing written\" at {target:?}: expected {status}, got {} body {:?}",
            answered.status, answered.body
        );
        assert_eq!(
            answered.header("server"),
            Some(LOAD_BALANCER_SERVER),
            "case \"nothing written\" at {target:?}: expected the load balancer's own Server \
             header, got {:?}",
            answered.header("server")
        );
        for row in &rows {
            let sent = answered.header(row.header);
            assert_eq!(
                sent, None,
                "case \"nothing written\" at {target:?}: expected no {:?}, got {sent:?}",
                row.header
            );
        }
    }

    // Every header-value attribute written, and the `Server` switch removed: each header is
    // sent with the value the attribute holds, no `Server` header is sent, and what the
    // action itself produced survives.
    let mut writes: Vec<(&str, &str)> = rows
        .iter()
        .map(|row| (row.attribute, row.value))
        .collect();
    writes.push((SERVER_ENABLED, "false"));
    modify(&router, "every attribute written", &listener_arn, &writes).await;
    for (key, expected) in &writes {
        let reported = described(&router, "every attribute written", &listener_arn, key).await;
        assert_eq!(
            reported, *expected,
            "case \"every attribute written\": expected {key} reported at {expected:?}, got \
             {reported:?}"
        );
    }
    for (target, status) in [("/", 200u16), (REDIRECT_PATH, 301)] {
        let answered = send(&router, "GET", &dns_name, target, &[]).await;
        assert_eq!(
            answered.status, status,
            "case \"every attribute written\" at {target:?}: expected {status}, got {} body {:?}",
            answered.status, answered.body
        );
        for row in &rows {
            let sent = answered.header(row.header);
            assert_eq!(
                sent,
                Some(row.value),
                "case \"every attribute written\" at {target:?}: expected {:?} at {:?}, got \
                 {sent:?}",
                row.header,
                row.value
            );
        }
        let server = answered.header("server");
        assert_eq!(
            server, None,
            "case \"every attribute written\" at {target:?}: the switch is off, expected no \
             Server header, got {server:?}"
        );
    }
    // What the actions produced is untouched by the rewriting.
    let answered = send(&router, "GET", &dns_name, "/", &[]).await;
    assert_eq!(
        answered.body, DEFAULT_BODY,
        "case \"every attribute written\": expected the fixed response's own body, got {:?}",
        answered.body
    );
    let answered = send(&router, "GET", &dns_name, REDIRECT_PATH, &[]).await;
    let location = answered.header("location").unwrap_or_default();
    assert!(
        location.contains(REDIRECT_HOST) && location.ends_with(REDIRECT_PATH),
        "case \"every attribute written\": expected the redirect's own Location to {REDIRECT_HOST}\
         {REDIRECT_PATH}, got {location:?}"
    );

    // One attribute written back to the empty string the collection carries it at, and the
    // switch allowed again: that header is no longer sent, every other one still is, and the
    // `Server` header is back.
    let emptied = &rows[0];
    modify(
        &router,
        "one attribute emptied",
        &listener_arn,
        &[(emptied.attribute, ""), (SERVER_ENABLED, "true")],
    )
    .await;
    let answered = send(&router, "GET", &dns_name, "/", &[]).await;
    let sent = answered.header(emptied.header);
    assert_eq!(
        sent, None,
        "case \"one attribute emptied\": expected no {:?}, got {sent:?}",
        emptied.header
    );
    assert_eq!(
        answered.header("server"),
        Some(LOAD_BALANCER_SERVER),
        "case \"one attribute emptied\": the switch is on again, expected the load balancer's \
         own Server header, got {:?}",
        answered.header("server")
    );
    for row in rows.iter().skip(1) {
        let sent = answered.header(row.header);
        assert_eq!(
            sent,
            Some(row.value),
            "case \"one attribute emptied\": expected {:?} still at {:?}, got {sent:?}",
            row.header,
            row.value
        );
    }
}
