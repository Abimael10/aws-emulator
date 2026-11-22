// SPDX-License-Identifier: CC0-1.0

//! What `DescribeClientVpnEndpoints` reports for a Client VPN endpoint's CONNECTION LOGGING
//! options, driven end to end over the wire the service speaks (`ec2Query`, through the
//! crate's shared [`super::ec2_query`] harness) with Certificate Manager mounted on the SAME
//! engine, because the certificates an endpoint presents are that service's resources.
//!
//! The rule under test: each boolean member of the connection-log options an endpoint is
//! created with is reported with the value the endpoint IS configured with — a flag the create
//! left unset is that setting switched off, so it reads back as `false` rather than being left
//! out of the reported structure — while a member that names a destination is reported only
//! when the endpoint has one. Logging state and the authorization-policy-context flag are
//! independent settings, so the table's rows differ along both of them, and along whether each
//! was asked for explicitly or left to its off state.

use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element};
// The create's own suite already mounts EC2 beside Certificate Manager on one engine, mints a
// certificate through that service's own API and spells an ec2Query parameter: automod puts
// every suite in ONE test binary, so those are reused here rather than written again.
use super::create_client_vpn_endpoint::{certificate, emulator, p};

/// The Region every call here is signed for — the one the shared harness mints certificates in.
const REGION: &str = "us-east-1";

/// The `ClientVpnAuthenticationType` of the one authentication option every create here
/// carries.
const CERTIFICATE_AUTHENTICATION: &str = "certificate-authentication";

/// The element each endpoint's connection-log options arrive in
/// (`ClientVpnEndpoint$ConnectionLogOptions`'s `xmlName`).
const OPTIONS_ELEMENT: &str = "connectionLogOptions";

/// One row of the table: the `ConnectionLogOptions` members the create carries, and the
/// members the describe must then report inside the endpoint's connection-log options —
/// `None` for a member the endpoint must report NO value for.
struct Row {
    case: &'static str,
    options: Vec<(String, String)>,
    expected_enabled: &'static str,
    expected_include: &'static str,
    expected_group: Option<&'static str>,
    expected_stream: Option<&'static str>,
}

/// The rows the rule is written against. They differ along whether connection logging is on,
/// whether the authorization-policy-context flag was asked for and with which value, and
/// whether a log destination was named — the four members
/// `ConnectionLogResponseOptions` carries
/// (`smithy_query(query="shape",shape="ConnectionLogResponseOptions")`).
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "logging off, the context flag left unset",
            options: vec![p("ConnectionLogOptions.Enabled", "false")],
            expected_enabled: "false",
            expected_include: "false",
            expected_group: None,
            expected_stream: None,
        },
        Row {
            case: "logging off, the context flag asked for off",
            options: vec![
                p("ConnectionLogOptions.Enabled", "false"),
                p("ConnectionLogOptions.IncludeAuthorizationPolicyContext", "false"),
            ],
            expected_enabled: "false",
            expected_include: "false",
            expected_group: None,
            expected_stream: None,
        },
        Row {
            case: "logging off, the context flag asked for on",
            options: vec![
                p("ConnectionLogOptions.Enabled", "false"),
                p("ConnectionLogOptions.IncludeAuthorizationPolicyContext", "true"),
            ],
            expected_enabled: "false",
            expected_include: "true",
            expected_group: None,
            expected_stream: None,
        },
        Row {
            case: "logging to a group and stream, the context flag left unset",
            options: vec![
                p("ConnectionLogOptions.Enabled", "true"),
                p("ConnectionLogOptions.CloudwatchLogGroup", "client-vpn"),
                p("ConnectionLogOptions.CloudwatchLogStream", "connections"),
            ],
            expected_enabled: "true",
            expected_include: "false",
            expected_group: Some("client-vpn"),
            expected_stream: Some("connections"),
        },
        Row {
            case: "logging to a group, the context flag asked for on",
            options: vec![
                p("ConnectionLogOptions.Enabled", "true"),
                p("ConnectionLogOptions.CloudwatchLogGroup", "audit"),
                p("ConnectionLogOptions.IncludeAuthorizationPolicyContext", "true"),
            ],
            expected_enabled: "true",
            expected_include: "true",
            expected_group: Some("audit"),
            expected_stream: None,
        },
        Row {
            // `ConnectionLogOptions` is `@required`, so a create names the structure; what a
            // row varies is which of its members it carries. This one carries only the flag,
            // leaving the logging state itself to its off value.
            case: "only the context flag, the logging state left unset",
            options: vec![p(
                "ConnectionLogOptions.IncludeAuthorizationPolicyContext",
                "false",
            )],
            expected_enabled: "false",
            expected_include: "false",
            expected_group: None,
            expected_stream: None,
        },
    ]
}

/// The connection-log options the describe reports for `endpoint_id`, read off the TREE so a
/// member is never read out of another structure of the endpoint that shares its name.
async fn reported_options(
    router: &MultiServiceRouter,
    case: &str,
    endpoint_id: &str,
) -> Vec<(String, String)> {
    let (status, body) = call(
        router,
        REGION,
        "DescribeClientVpnEndpoints",
        &[p("ClientVpnEndpointId.1", endpoint_id)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeClientVpnEndpoints must report {endpoint_id}, got {status} with \
         body {body}"
    );
    let parsed = super::wire::document(case, &body);
    let endpoint = parsed
        .descendants()
        .find(|node| {
            node.is_element() && super::wire::child(*node, "clientVpnEndpointId").is_some()
        })
        .unwrap_or_else(|| panic!("case {case:?}: no reported endpoint in {body}"));
    let options = super::wire::child(endpoint, OPTIONS_ELEMENT).unwrap_or_else(|| {
        panic!("case {case:?}: no <{OPTIONS_ELEMENT}> reported for {endpoint_id}: {body}")
    });
    options
        .children()
        .filter(roxmltree::Node::is_element)
        .map(|member| {
            (
                member.tag_name().name().to_string(),
                member.text().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

/// The value reported for `member`, or `None` when the structure carries no such member.
fn reported(members: &[(String, String)], member: &str) -> Option<String> {
    members
        .iter()
        .find(|(name, _)| name == member)
        .map(|(_, value)| value.clone())
}

/// Every boolean member of the connection-log options reads back with the value the endpoint
/// is configured with, whether or not the create asked for it.
#[tokio::test]
async fn connection_log_options_report_every_boolean_they_carry() {
    for row in rows() {
        let router = emulator();
        let server = certificate(&router, row.case, "vpn.example.com").await;
        let client_root = certificate(&router, row.case, "clients.example.com").await;
        let mut params = vec![
            p("ClientCidrBlock", "10.199.0.0/22"),
            p("ServerCertificateArn", server.clone()),
            p("Authentication.1.Type", CERTIFICATE_AUTHENTICATION),
            p(
                "Authentication.1.MutualAuthentication.ClientRootCertificateChainArn",
                client_root.clone(),
            ),
        ];
        params.extend(row.options.iter().cloned());

        let (status, body) = call(&router, REGION, "CreateClientVpnEndpoint", &params).await;
        assert_eq!(
            status, 200,
            "case {:?}: CreateClientVpnEndpoint must answer 200, got {status} with body {body}",
            row.case
        );
        let endpoint_id = element(&body, "clientVpnEndpointId")
            .unwrap_or_else(|| panic!("case {:?}: no clientVpnEndpointId in {body}", row.case));

        let members = reported_options(&router, row.case, &endpoint_id).await;
        for (member, expected) in [
            ("Enabled", Some(row.expected_enabled.to_string())),
            (
                "IncludeAuthorizationPolicyContext",
                Some(row.expected_include.to_string()),
            ),
            ("CloudwatchLogGroup", row.expected_group.map(str::to_string)),
            (
                "CloudwatchLogStream",
                row.expected_stream.map(str::to_string),
            ),
        ] {
            let actual = reported(&members, member);
            assert_eq!(
                actual, expected,
                "case {:?}: expected <{member}> {expected:?} in the reported \
                 {OPTIONS_ELEMENT}, got {actual:?} among {members:?}",
                row.case
            );
        }

        // A read reports the same structure every time: nothing about the endpoint's logging
        // was materialised by the first describe.
        let again = reported_options(&router, row.case, &endpoint_id).await;
        assert_eq!(
            again, members,
            "case {:?}: a second describe must report the same {OPTIONS_ELEMENT}, got \
             {again:?} after {members:?}",
            row.case
        );
    }
}
