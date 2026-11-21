// SPDX-License-Identifier: CC0-1.0

//! WHERE a global endpoint is announced, over the wire (`awsJson1_1`, `POST /` with
//! `X-Amz-Target: AWSEvents.<Operation>`) through the real `MultiServiceRouter`.
//!
//! The RULE asserted here: `EndpointUrl` is an address a client DIALS to publish its events,
//! so every operation that reports one reports an address THIS emulator answers on — never a
//! host Amazon Web Services publishes its own endpoints at, which resolves nowhere near this
//! process or, worse, at genuine infrastructure.
//!
//! The rows span every operation that reports the member — the single-endpoint read, the
//! listing and the write that answers with the merged record — the endpoints of one account,
//! and the addresses a caller reaches this emulator at: the service's own AWS host (a caller
//! that resolves Amazon Web Services' hostnames, answered the address this process is served
//! at) and this emulator's own endpoint, on a port and on the port `https` defaults to.
//!
//! On the port a client has to dial: `EndpointUrl` is modelled `length 1..256, pattern
//! ^(https://)?[\.\-a-z0-9]+$`
//! (`smithy_query(query="shape",shape="com.amazonaws.eventbridge#EndpointUrl")`), which no
//! `host:port` satisfies — Amazon Web Services serves its global endpoints on 443, so the
//! address it publishes never carries one. An emulator bound anywhere else is reached ONLY by
//! dialling the port it is bound to, so a value kept inside that pattern would publish a port
//! nothing listens on, which is the same defect as publishing a host that resolves nowhere.
//! The port is therefore carried whenever it is not the one `https` defaults to, and a value
//! that carries none is asserted to satisfy the modelled pattern exactly.

use serde_json::{json, Value};

use crate::global_endpoint_health_check::{
    create_health_check, create_request, emulator, error_code, events, events_addressed,
    health_check_arn, http_check, REGION, SECONDARY_REGION,
};

/// Whether `value` satisfies `EndpointUrl`'s modelled constraints (`length 1..256,
/// pattern ^(https://)?[\.\-a-z0-9]+$`).
fn is_modelled_endpoint_url(value: &str) -> bool {
    let host = value.strip_prefix("https://").unwrap_or(value);
    !value.is_empty()
        && value.chars().count() <= 256
        && !host.is_empty()
        && host.chars().all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || matches!(character, '.' | '-')
        })
}

/// Every operation that reports a global endpoint's URL reports the address this emulator is
/// reached at.
#[tokio::test]
async fn every_read_of_an_endpoint_announces_this_emulators_own_address() {
    let router = emulator();
    let check = create_health_check(&router, "events-endpoint-announced-url", http_check()).await;
    let arn = health_check_arn(&check);

    // Two endpoints of the one account, so a URL that encoded something of the endpoint
    // rather than of this emulator would differ between them.
    for name in ["announced-one", "announced-two"] {
        let (status, created) = events(&router, "CreateEndpoint", &create_request(name, &arn)).await;
        assert_eq!(
            status, 200,
            "case {name:?}: CreateEndpoint answered {status} {created}"
        );
    }

    // (case, the host the requests address, the URL every read must report)
    //
    // A test drives the service in process, so this emulator is served on no port of its
    // own: a caller that resolved the service's AWS host is answered the only address left
    // that leads back here, the loopback zone the core binds its listeners on, while a caller
    // that reached one of this emulator's own addresses is answered the one it reached.
    let rows: &[(&str, String, &str)] = &[
        (
            "a caller that resolves AWS's own hostnames",
            format!("events.{REGION}.amazonaws.com"),
            "https://localhost",
        ),
        (
            "a caller that reached this emulator on its port",
            "127.0.0.1:9297".to_string(),
            "https://127.0.0.1:9297",
        ),
        (
            "a caller that reached it by name on another port",
            "localhost:39355".to_string(),
            "https://localhost:39355",
        ),
        (
            "a caller that reached it on the port https defaults to",
            "127.0.0.1:443".to_string(),
            "https://127.0.0.1",
        ),
    ];

    for (case, host, want) in rows {
        let name = "announced-one";
        let update = json!({
            "Name": name,
            "RoutingConfig": {
                "FailoverConfig": {
                    "Primary": { "HealthCheck": arn },
                    "Secondary": { "Route": SECONDARY_REGION },
                },
            },
        });
        let (status, read) =
            events_addressed(&router, host, "DescribeEndpoint", &json!({ "Name": name })).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeEndpoint answered {status} {read}"
        );
        let (status, listed) = events_addressed(&router, host, "ListEndpoints", &json!({})).await;
        assert_eq!(
            status, 200,
            "case {case:?}: ListEndpoints answered {status} {listed}"
        );
        let (status, updated) = events_addressed(&router, host, "UpdateEndpoint", &update).await;
        assert_eq!(
            status, 200,
            "case {case:?}: UpdateEndpoint answered {status} {updated}"
        );

        let endpoints = listed["Endpoints"].as_array().cloned().unwrap_or_else(|| {
            panic!("case {case:?}: ListEndpoints must report the endpoints, got {listed}")
        });
        assert_eq!(
            endpoints.len(),
            2,
            "case {case:?}: expected the two endpoints created above to be listed, got {listed}"
        );
        let mut reported: Vec<(String, Value)> = vec![
            (format!("{case}: DescribeEndpoint"), read["EndpointUrl"].clone()),
            (format!("{case}: UpdateEndpoint"), updated["EndpointUrl"].clone()),
        ];
        for endpoint in &endpoints {
            let listed_name = endpoint["Name"].as_str().unwrap_or_default();
            reported.push((
                format!("{case}: ListEndpoints {listed_name}"),
                endpoint["EndpointUrl"].clone(),
            ));
        }

        for (case, url) in reported {
            let url = url
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: expected an EndpointUrl, got {url}"));
            assert_eq!(
                url, *want,
                "case {case:?}: expected the address this emulator answers on ({want:?}), \
                 got {url:?}"
            );
            assert!(
                !url.contains("amazonaws.com"),
                "case {case:?}: a published URL names no Amazon Web Services host, got {url:?}"
            );
            // A value that carries no port is inside the modelled shape; one that carries a
            // port deliberately exceeds it, for the reason the module docs give.
            if !url.trim_start_matches("https://").contains(':') {
                assert!(
                    is_modelled_endpoint_url(url),
                    "case {case:?}: {url:?} must satisfy EndpointUrl's modelled constraints"
                );
            }
        }
    }

    // A refused read reports no URL at all: there is no endpoint to announce an address for.
    let (status, missing) = events(
        &router,
        "DescribeEndpoint",
        &json!({ "Name": "never-created" }),
    )
    .await;
    assert_eq!(
        status, 400,
        "expected a missing endpoint to be refused, got {status} {missing}"
    );
    assert_eq!(
        error_code(&missing),
        "ResourceNotFoundException",
        "expected the read to report the endpoint missing, got {missing}"
    );
    assert!(
        missing.get("EndpointUrl").is_none(),
        "a refused read produces no response members, got {missing}"
    );
}
