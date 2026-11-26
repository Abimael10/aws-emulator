// SPDX-License-Identifier: CC0-1.0

//! Probe: the OpenSearch Service node-maintenance flow a polling client drives, on the
//! ASSEMBLED emulator with request authentication ON — every call signed, as a client
//! signs it.
//!
//! The sequence is the one an AWS-targeting client runs: create a three-data-node domain,
//! wait for it to report itself settled, read its node census, start a maintenance action
//! on a data node it published, poll the action's status until it is terminal, and read the
//! listing back.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use aws_credential_types::Credentials;
use aws_sigv4::http_request::{sign, SignableBody, SignableRequest, SigningSettings};
use aws_sigv4::sign::v4::SigningParams;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// Keep this at or above `boot_serve.rs`'s stack: the same assembly runs here.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The Region every call below is signed for.
const REGION: &str = "us-west-2";

/// Amazon OpenSearch Service's signing name, and the API version its control plane binds
/// every operation under.
const SIGNING_NAME: &str = "es";
const PREFIX: &str = "/2021-01-01";

/// How long the flow waits for a condition before giving up, and how often it looks
/// meanwhile. Generous: what is asserted is that the condition holds at all, never how fast
/// the host runs.
const DEADLINE: Duration = Duration::from_secs(60);
const LOOK_INTERVAL: Duration = Duration::from_millis(200);

/// The key pair this process publishes once request authentication is on.
struct Signer {
    key_id: String,
    secret: String,
}

impl Signer {
    fn sign(&self, mut request: http::Request<Vec<u8>>) -> http::Request<Vec<u8>> {
        let identity = Credentials::new(&self.key_id, &self.secret, None, None, "test").into();
        let params = SigningParams::builder()
            .identity(&identity)
            .region(REGION)
            .name(SIGNING_NAME)
            .time(SystemTime::now())
            .settings(SigningSettings::default())
            .build()
            .expect("the signing parameters are complete")
            .into();
        let (method, uri) = (request.method().to_string(), request.uri().to_string());
        let headers: Vec<(String, String)> = request
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_string(),
                    value.to_str().expect("a text header").to_string(),
                )
            })
            .collect();
        let signable = SignableRequest::new(
            &method,
            &uri,
            headers
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_str())),
            SignableBody::Bytes(request.body()),
        )
        .expect("the request is signable");
        let (instructions, _) = sign(signable, &params)
            .expect("the request signs")
            .into_parts();
        instructions.apply_to_request_http1x(&mut request);
        request
    }
}

/// One signed call, answering the status and the parsed body.
async fn call(
    router: &MultiServiceRouter,
    signer: &Signer,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let host = format!("{SIGNING_NAME}.{REGION}.amazonaws.com");
    let payload = body.map(|value| value.to_string().into_bytes()).unwrap_or_default();
    let built = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", &host)
        .header("content-type", "application/json")
        .body(payload)
        .expect("the probe request is well formed");
    let signed = signer.sign(built);
    let (parts, payload) = signed.into_parts();
    let request = http::Request::from_parts(parts, bytes::Bytes::from(payload));
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    let parsed: Value = serde_json::from_str(&text).unwrap_or(Value::String(text));
    (status, parsed)
}

async fn probe() {
    let engine = aws_emulator_router::new_engine();
    let mut router = aws_emulator_router::build_router(Arc::clone(&engine));
    router.set_request_authentication(true);
    let published = ::emulator_server::SigV4Authenticator::minted_credentials()
        .expect("turning authentication on publishes this process's keys");
    let signer = Signer {
        key_id: published.access_key_id.clone(),
        secret: published.secret_access_key.clone(),
    };

    let name = "probe-maintained-domain";
    let (status, body) = call(
        &router,
        &signer,
        "POST",
        &format!("{PREFIX}/opensearch/domain"),
        Some(&json!({
            "DomainName": name,
            "ClusterConfig": { "InstanceType": "t3.small.search", "InstanceCount": 3 },
            "EBSOptions": { "EBSEnabled": true, "VolumeType": "gp3", "VolumeSize": 10 },
            "AccessPolicies": "{\"Version\":\"2012-10-17\",\"Statement\":[{\"Effect\":\"Allow\",\"Principal\":{\"AWS\":\"arn:aws:iam::123456789012:root\"},\"Action\":\"es:*\",\"Resource\":\"arn:aws:es:us-west-2:123456789012:domain/probe-maintained-domain/*\"}]}",
        })),
    )
    .await;
    assert_eq!(status, 200, "CreateDomain: expected 200, got {status} with {body}");

    // Settled, as a client waits for it.
    let deadline = Instant::now() + DEADLINE;
    loop {
        let (status, body) = call(
            &router,
            &signer,
            "GET",
            &format!("{PREFIX}/opensearch/domain/{name}"),
            None,
        )
        .await;
        assert_eq!(status, 200, "DescribeDomain: expected 200, got {status} with {body}");
        if body["DomainStatus"]["Processing"].as_bool() == Some(false)
            && body["DomainStatus"]["Created"].as_bool() == Some(true)
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "DescribeDomain: the domain never reported itself settled, last read {body}"
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }

    // The census, as a client reads it.
    let deadline = Instant::now() + DEADLINE;
    let node_id = loop {
        let (status, body) = call(
            &router,
            &signer,
            "GET",
            &format!("{PREFIX}/opensearch/domain/{name}/nodes"),
            None,
        )
        .await;
        let found = body["DomainNodesStatusList"]
            .as_array()
            .and_then(|nodes| nodes.first())
            .and_then(|node| node["NodeId"].as_str())
            .map(str::to_string);
        if let Some(found) = found.filter(|_| status == 200) {
            break found;
        }
        assert!(
            Instant::now() < deadline,
            "DescribeDomainNodes: no node id was ever published, last answer {status} with {body}"
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    };

    let (status, body) = call(
        &router,
        &signer,
        "POST",
        &format!("{PREFIX}/opensearch/domain/{name}/domainMaintenance"),
        Some(&json!({ "Action": "RESTART_SEARCH_PROCESS", "NodeId": node_id })),
    )
    .await;
    assert_eq!(
        status, 200,
        "StartDomainMaintenance({node_id}): expected 200, got {status} with {body}"
    );
    let maintenance_id = body["MaintenanceId"]
        .as_str()
        .unwrap_or_else(|| panic!("StartDomainMaintenance: expected a MaintenanceId, got {body}"))
        .to_string();

    // The read that follows the call that started the action reports it under a status the
    // action is still under way in: the clock owes it its run.
    let (status, body) = call(
        &router,
        &signer,
        "GET",
        &format!(
            "{PREFIX}/opensearch/domain/{name}/domainMaintenance?maintenanceId={maintenance_id}"
        ),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "GetDomainMaintenanceStatus: expected 200 from the read after the start, got {status} \
         with {body}"
    );
    assert!(
        matches!(body["Status"].as_str(), Some("PENDING" | "IN_PROGRESS")),
        "GetDomainMaintenanceStatus: expected the action to be read running, got {body}"
    );

    // The poll a client makes, and NOTHING else.
    let deadline = Instant::now() + DEADLINE;
    let terminal = loop {
        let (status, body) = call(
            &router,
            &signer,
            "GET",
            &format!(
                "{PREFIX}/opensearch/domain/{name}/domainMaintenance?maintenanceId={maintenance_id}"
            ),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "GetDomainMaintenanceStatus: expected 200 from every poll, got {status} with {body}"
        );
        let reported = body["Status"].as_str().unwrap_or_default().to_string();
        if !matches!(reported.as_str(), "PENDING" | "IN_PROGRESS") {
            break reported;
        }
        assert!(
            Instant::now() < deadline,
            "GetDomainMaintenanceStatus: the action never reached a terminal status, last {body}"
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    };
    assert_eq!(
        terminal, "SUCCESS",
        "GetDomainMaintenanceStatus: expected the terminal status a finished action reports"
    );

    let (status, body) = call(
        &router,
        &signer,
        "GET",
        &format!("{PREFIX}/opensearch/domain/{name}/domainMaintenances"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "ListDomainMaintenances: expected 200, got {status} with {body}"
    );
    let listed = body["DomainMaintenances"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        listed.iter().any(|row| {
            row["MaintenanceId"].as_str() == Some(maintenance_id.as_str())
                && row["Status"].as_str() == Some("SUCCESS")
        }),
        "ListDomainMaintenances: expected the started action listed as finished, got {listed:?}"
    );
}

/// RULE: on the assembled emulator, with request signatures verified, a node maintenance
/// action a client starts on a published data node becomes terminal on the status reads that
/// client makes, and the listing reports the same action under the same terminal status.
#[test]
fn an_accepted_maintenance_action_becomes_terminal_on_an_authenticating_router() {
    std::thread::Builder::new()
        .name("opensearch-maintenance-probe".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .expect("build a current-thread runtime");
            runtime.block_on(probe());
        })
        .expect("spawn the sized thread")
        .join()
        .expect("the suite thread runs to completion");
}
