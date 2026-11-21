// SPDX-License-Identifier: CC0-1.0

//! Which listeners are reported WITH an `AlpnPolicy` member, driven over the wire exactly as
//! a client drives it: listeners created with `CreateListener`, changed with
//! `ModifyListener` and read back with `DescribeListeners`.
//!
//! One rule is asserted here, over a SET of listeners rather than over the one call a
//! recording happens to carry: **a listener is reported with `Listener$AlpnPolicy` only
//! where it HOLDS an ALPN policy, and a listener holding none carries no such member at
//! all** — not a present-but-empty one. An absent member and an empty list are different
//! answers on this wire form: the empty one says "a policy is configured, holding no name",
//! which is what a client branching on whether ALPN is configured, or round-tripping a
//! reported listener back into `ModifyListener`, then acts on.
//!
//! Grounded in the model: `Listener$AlpnPolicy` is "[TLS listener] The name of the
//! Application-Layer Protocol Negotiation (ALPN) policy"
//! (`smithy_query(query="shape",shape="Listener")`), targeting a list shape that declares
//! no empty form and carries no default, and the write that sets it takes "one policy name"
//! out of a published value list (`smithy_query(query="doc",shape="CreateListenerInput",
//! member="AlpnPolicy")`).
//!
//! The rows differ along what the rule generalises over: the listener's protocol (one the
//! member is bracketed to and three outside it, across both load balancer families), whether
//! a policy was named at all, WHICH of the published names it is, and how the listener came
//! to hold it (named on the create, or put there by a modification). Every row is asserted
//! on every path that reports a listener — the create's own reply, the modification's reply,
//! a read by listener ARN and a read of the whole load balancer.

use crate::alb_negotiated_application_protocol::{
    create_network_load_balancer, create_tcp_target_group,
};
use crate::forward_action_config_and_mutual_authentication::describe_listener;
use crate::listener_rules::create_load_balancer;
use crate::provisioning_loop::{action, call, create_http_target_group, emulator, members, p, Reply};
use smithy_server_runtime::MultiServiceRouter;

/// A certificate ARN a listener's default certificate resolves to, so a row that needs a
/// TLS-terminating listener reaches the assertion below rather than the certificate refusal
/// pinned in `all/listener_tls_certificates.rs`.
const CERTIFICATE: &str = "arn:aws:acm:us-west-2:000000000000:certificate/alpn-listener";

/// Every name `CreateListenerInput$AlpnPolicy` publishes: "You can specify one policy name.
/// The following are the possible values: `HTTP1Only`, `HTTP2Only`, `HTTP2Optional`,
/// `HTTP2Preferred`, `None`"
/// (`smithy_query(query="doc",shape="CreateListenerInput",member="AlpnPolicy")`). `None` is
/// a policy name like any other — a configured policy that negotiates nothing — so a
/// listener carrying it HOLDS a policy.
const PUBLISHED_POLICIES: &[&str] = &[
    "HTTP1Only",
    "HTTP2Only",
    "HTTP2Optional",
    "HTTP2Preferred",
    "None",
];

/// One row: the listener a case creates, what it is then changed to, and the policy names
/// every reply reporting it must carry.
struct Row {
    case: String,
    /// Whether the listener belongs to the NETWORK load balancer, which is the family whose
    /// listeners terminate TLS under their own ALPN policy; the others belong to the
    /// Application one.
    network: bool,
    protocol: &'static str,
    /// The `AlpnPolicy.member.1` the CREATE carries, if any.
    created_with: Option<&'static str>,
    /// The `AlpnPolicy.member.1` a later `ModifyListener` carries, if any.
    modified_with: Option<&'static str>,
    /// Every policy name the listener then holds, in order — empty for a listener holding
    /// none.
    reports: Vec<&'static str>,
}

/// The table every assertion below is made over.
fn table() -> Vec<Row> {
    let mut rows = vec![
        Row {
            case: "an HTTP listener, which no ALPN policy is named on".to_string(),
            network: false,
            protocol: "HTTP",
            created_with: None,
            modified_with: None,
            reports: Vec::new(),
        },
        Row {
            case: "an HTTPS listener, which no ALPN policy is named on".to_string(),
            network: false,
            protocol: "HTTPS",
            created_with: None,
            modified_with: None,
            reports: Vec::new(),
        },
        Row {
            case: "a TCP listener, which no ALPN policy is named on".to_string(),
            network: true,
            protocol: "TCP",
            created_with: None,
            modified_with: None,
            reports: Vec::new(),
        },
        Row {
            case: "a TLS listener no ALPN policy was named on".to_string(),
            network: true,
            protocol: "TLS",
            created_with: None,
            modified_with: None,
            reports: Vec::new(),
        },
        Row {
            case: "a TLS listener a modification named a policy on".to_string(),
            network: true,
            protocol: "TLS",
            created_with: None,
            modified_with: Some("HTTP2Preferred"),
            reports: vec!["HTTP2Preferred"],
        },
        Row {
            case: "a TLS listener a modification named another policy on".to_string(),
            network: true,
            protocol: "TLS",
            created_with: Some("HTTP1Only"),
            modified_with: Some("None"),
            reports: vec!["None"],
        },
    ];
    // One row per published policy name: a create that names it holds it, whichever it is.
    for policy in PUBLISHED_POLICIES {
        rows.push(Row {
            case: format!("a TLS listener created with the {policy} policy"),
            network: true,
            protocol: "TLS",
            created_with: Some(policy),
            modified_with: None,
            reports: vec![policy],
        });
    }
    rows
}

/// Whether one reported listener carries the `AlpnPolicy` member at all, which is a
/// different answer from carrying it empty (`<AlpnPolicy></AlpnPolicy>` or `<AlpnPolicy/>`).
fn carries_alpn_policy_member(listener: &str) -> bool {
    listener.contains("<AlpnPolicy>") || listener.contains("<AlpnPolicy/>")
}

/// What every path reporting a listener must say about its ALPN policy: the names it holds
/// where it holds any, and no member at all where it holds none.
fn assert_reported(case: &str, where_from: &str, listener: &str, expected: &[&str]) {
    if expected.is_empty() {
        assert!(
            !carries_alpn_policy_member(listener),
            "case {case}: {where_from} must leave the AlpnPolicy member out of a listener \
             holding no ALPN policy, got {listener}"
        );
        return;
    }
    assert!(
        carries_alpn_policy_member(listener),
        "case {case}: {where_from} must report the AlpnPolicy member of a listener holding \
         {expected:?}, got {listener}"
    );
    let reported = members(listener, "AlpnPolicy");
    assert_eq!(
        reported, expected,
        "case {case}: {where_from} expected the ALPN policy {expected:?}, got {reported:?} in \
         {listener}"
    );
}

/// The one listener a reply reports, as its XML.
fn the_listener(case: &str, where_from: &str, reply: &Reply) -> String {
    assert_eq!(
        reply.status, 200,
        "case {case}: expected 200 from {where_from}, got {} {}",
        reply.status, reply.body
    );
    let listeners = members(&reply.body, "Listeners");
    assert_eq!(
        listeners.len(),
        1,
        "case {case}: {where_from} answers with the one listener it names, got {}",
        reply.body
    );
    listeners.into_iter().next().unwrap_or_default()
}

/// Create the listener of one row and answer its ARN and the XML the create reported.
async fn create(
    router: &MultiServiceRouter,
    row: &Row,
    load_balancer: &str,
    target_group: &str,
    port: i32,
) -> (String, String) {
    let mut form = action("CreateListener");
    form.push(p("LoadBalancerArn", load_balancer));
    form.push(p("Protocol", row.protocol));
    form.push(p("Port", &port.to_string()));
    if row.protocol == "HTTPS" || row.protocol == "TLS" {
        form.push(p("Certificates.member.1.CertificateArn", CERTIFICATE));
    }
    if let Some(policy) = row.created_with {
        form.push(p("AlpnPolicy.member.1", policy));
    }
    form.push(p("DefaultActions.member.1.Type", "forward"));
    form.push(p("DefaultActions.member.1.TargetGroupArn", target_group));
    let reply = call(router, &form).await;
    let listener = the_listener(&row.case, "CreateListener", &reply);
    let arn = crate::provisioning_loop::first_text(&listener, "ListenerArn")
        .unwrap_or_else(|| panic!("case {}: no ListenerArn in {}", row.case, reply.body));
    (arn, listener)
}

/// A listener reports the ALPN policy it holds on every path that reports it, and no
/// `AlpnPolicy` member at all when it holds none.
#[tokio::test]
async fn a_listener_reports_an_alpn_policy_only_where_it_holds_one() {
    let (router, _engine) = emulator();
    let application = create_load_balancer(&router, "alpn-member-alb").await;
    let http_group = create_http_target_group(&router, "alpn-member-http-tg").await;
    let (network, _published) = create_network_load_balancer(&router, "alpn-member-nlb").await;
    let tcp_group = create_tcp_target_group(&router, "alpn-member-tcp-tg").await;

    let rows = table();
    let mut created = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let (load_balancer, target_group) = if row.network {
            (&network, &tcp_group)
        } else {
            (&application, &http_group)
        };
        // A port nothing else on that load balancer listens on, so each row is its own
        // listener rather than the duplicate the service refuses.
        let port = 30_000 + index as i32;
        let (arn, listener) = create(&router, row, load_balancer, target_group, port).await;

        // The create's OWN reply reports the listener it left behind.
        let expected_from_create = row.created_with.map(|policy| vec![policy]).unwrap_or_default();
        assert_reported(
            &row.case,
            "CreateListener",
            &listener,
            &expected_from_create,
        );

        if let Some(policy) = row.modified_with {
            let mut form = action("ModifyListener");
            form.push(p("ListenerArn", &arn));
            form.push(p("AlpnPolicy.member.1", policy));
            let reply = call(&router, &form).await;
            let modified = the_listener(&row.case, "ModifyListener", &reply);
            assert_reported(&row.case, "ModifyListener", &modified, &row.reports);
        }

        // The read by ARN reports the same listener as the writes left it.
        let read = describe_listener(&router, &arn).await;
        let listener = the_listener(&row.case, "DescribeListeners by ARN", &read);
        assert_reported(
            &row.case,
            "DescribeListeners by ARN",
            &listener,
            &row.reports,
        );
        created.push((arn, row));
    }

    // And the read of each WHOLE load balancer reports every one of its listeners the same
    // way, so the member follows the listener rather than the shape of the request that
    // selected it.
    for (load_balancer, network_family) in [(&application, false), (&network, true)] {
        let mut form = action("DescribeListeners");
        form.push(p("LoadBalancerArn", load_balancer));
        let read = call(&router, &form).await;
        assert_eq!(
            read.status, 200,
            "case the whole load balancer: expected 200 from DescribeListeners, got {} {}",
            read.status, read.body
        );
        let reported = members(&read.body, "Listeners");
        for (arn, row) in created.iter().filter(|(_, row)| row.network == network_family) {
            let listener = reported
                .iter()
                .find(|listener| {
                    crate::provisioning_loop::first_text(listener, "ListenerArn").as_deref()
                        == Some(arn.as_str())
                })
                .unwrap_or_else(|| {
                    panic!(
                        "case {}: the load balancer must report the listener it holds, got {}",
                        row.case, read.body
                    )
                });
            assert_reported(
                &row.case,
                "DescribeListeners by load balancer",
                listener,
                &row.reports,
            );
        }
    }
}
