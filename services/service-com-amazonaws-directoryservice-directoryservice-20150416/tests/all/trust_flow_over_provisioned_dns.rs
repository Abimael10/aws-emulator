// SPDX-License-Identifier: CC0-1.0

//! The whole trust flow between two provisioned Managed Microsoft AD directories, driven end
//! to end over HTTP with the owner of subnets mounted beside this service.
//!
//! RULE: an Active Managed Microsoft AD reports the addresses its DNS servers answer at —
//! `DirectoryDescription$DnsIpAddrs`, one per subnet the directory was placed in — so a caller
//! that configures a conditional forwarder or a trust from a describe has the addresses it
//! needs, and the create -> verify -> delete flow those addresses exist for runs to its end:
//! the relationship settles on `Created`, then on `Verified`, and once the teardown the delete
//! owes has run out it is gone from this service — after which the id names nothing this
//! service holds, so a listing that asks for it and a repeat of the delete are both refused
//! with the one wording this service states for a relationship it does not hold.
//!
//! The rows differ along what the rule generalises over: the directory the trust hangs off and
//! the one its remote domain names (so each row's addresses are a different set, out of
//! subnets with different blocks), and the direction the relationship is established in.
//!
//! A subnet is an Amazon EC2 resource, so EC2 is mounted beside this service on ONE engine
//! (`crate::directory_dns_addresses::network`) and every subnet a case names was MINTED by it:
//! the addresses the flow is driven with are the ones this service provisioned out of blocks
//! their owner reports, not identifiers written here. Nothing reaches into either store, and
//! every state the flow waits for is POLLED through the API until it settles.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::directory_dns_addresses::network;
use crate::directory_lifecycle::{call, ok, string, PASSWORD, STAGE_ACTIVE};

/// How long a polled state may take to settle before a case fails: far longer than any span
/// this service publishes, and well inside what a client's waiter allows.
const SETTLE_DEADLINE: Duration = Duration::from_secs(30);

/// How often one of the waits below asks again.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// A `TrustPassword` the modelled pattern `^(\p{LD}|\p{Punct}| )+$` admits, within its
/// `length 1..128`.
const TRUST_PASSWORD: &str = "Tru5t-P@ssword 1";

/// The `TrustState` values of the modelled enum this suite waits for
/// (`smithy_query(query="shape", shape="TrustState")`).
const TRUST_STATE_CREATED: &str = "Created";
const TRUST_STATE_VERIFIED: &str = "Verified";

/// The modelled error a call naming a relationship this service does not hold is refused
/// with: `EntityDoesNotExistException` ("The specified entity could not be found", HTTP 400,
/// members `Message` + `RequestId`).
const ENTITY_DOES_NOT_EXIST: &str = "EntityDoesNotExistException";

/// That refusal's whole message: the kind of entity, the id the REQUEST sent and "does not
/// exist", closed with the suffix every refusal of this service carries — the id of the call
/// being answered, read off the response's own `RequestId` member rather than written here,
/// since it is a per-call value.
fn no_such_trust_message(trust_id: &str, request_id: &str) -> String {
    format!("Trust: {trust_id} does not exist : RequestId: {request_id}")
}

/// Assert that `operation` answers `body` with the refusal a relationship this service no
/// longer holds gets: HTTP 400, the modelled code, and the sentence above.
async fn refused_as_gone(
    router: &MultiServiceRouter,
    operation: &str,
    body: Value,
    trust_id: &str,
    case: &str,
) {
    let (status, parsed) = call(router, operation, body).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected {operation} on a relationship this service no longer holds \
         to be refused, got {status} with {parsed}"
    );
    let wire_type = parsed["__type"].as_str().unwrap_or_default();
    assert!(
        wire_type.contains(ENTITY_DOES_NOT_EXIST),
        "case {case:?}: expected {ENTITY_DOES_NOT_EXIST} from {operation}, got {parsed}"
    );
    let request_id = parsed["RequestId"].as_str().unwrap_or_default();
    assert!(
        !request_id.is_empty(),
        "case {case:?}: expected {operation}'s refusal to report its own request id, got \
         {parsed}"
    );
    // The modelled error reports its diagnostic in the `Message` member the model declares;
    // the lowercase spelling is read too, so the assertion is about the sentence rather than
    // about which spelling the envelope happens to use.
    let message = parsed["Message"]
        .as_str()
        .or_else(|| parsed["message"].as_str())
        .unwrap_or_default();
    assert_eq!(
        message,
        no_such_trust_message(trust_id, request_id),
        "case {case:?}: expected {operation}'s refusal to name the relationship the request \
         asked for, got {message:?}"
    );
}

/// The one `DirectoryDescription` `DescribeDirectories` reports for `directory_id`.
async fn describe_one(router: &MultiServiceRouter, directory_id: &str, case: &str) -> Value {
    let answer = ok(
        router,
        "DescribeDirectories",
        json!({ "DirectoryIds": [directory_id] }),
        case,
    )
    .await;
    answer["DirectoryDescriptions"][0].clone()
}

/// Wait until `DescribeDirectories` reports `directory_id` in service, the way a client with a
/// waiter does — ask, and ask again until the stage chain the create owes has settled or a
/// generous deadline passes — and answer the DNS addresses it then reports.
async fn dns_addresses_in_service(
    router: &MultiServiceRouter,
    directory_id: &str,
    case: &str,
) -> Vec<String> {
    let started = Instant::now();
    loop {
        let described = describe_one(router, directory_id, case).await;
        if described["Stage"] == json!(STAGE_ACTIVE) {
            let reported = described["DnsIpAddrs"].as_array().unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected an Active directory to report the addresses its \
                     DNS servers answer at, got {described}"
                )
            });
            return reported
                .iter()
                .map(|address| {
                    address
                        .as_str()
                        .unwrap_or_else(|| {
                            panic!("case {case:?}: expected an address, got {address}")
                        })
                        .to_string()
                })
                .collect();
        }
        assert!(
            started.elapsed() < SETTLE_DEADLINE,
            "case {case:?}: expected {directory_id} to report itself in service within \
             {SETTLE_DEADLINE:?}, last observed {described}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// A `CreateMicrosoftAD` body placing a directory named `name` in `subnets` of the VPC `vpc`.
fn microsoft_ad_in(name: &str, vpc: &str, subnets: &[String]) -> Value {
    json!({
        "Name": name,
        "Password": PASSWORD,
        "VpcSettings": { "VpcId": vpc, "SubnetIds": subnets },
    })
}

/// The state `trust_id` is in, as `DescribeTrusts` reports it.
async fn trust_state(router: &MultiServiceRouter, trust_id: &str, case: &str) -> String {
    let answer = ok(
        router,
        "DescribeTrusts",
        json!({ "TrustIds": [trust_id] }),
        case,
    )
    .await;
    answer["Trusts"][0]["TrustState"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a TrustState, got {answer}"))
        .to_string()
}

/// Wait until the relationship `trust_id` reports `expected`, polling the way a client does.
async fn settles_on(router: &MultiServiceRouter, trust_id: &str, expected: &str, case: &str) {
    let started = Instant::now();
    loop {
        let state = trust_state(router, trust_id, case).await;
        if state == expected {
            return;
        }
        assert!(
            started.elapsed() < SETTLE_DEADLINE,
            "case {case:?}: expected the relationship to reach {expected} within \
             {SETTLE_DEADLINE:?}, last observed {state:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The trusts `DescribeTrusts` reports for one directory.
async fn trusts_of(router: &MultiServiceRouter, directory_id: &str, case: &str) -> Vec<Value> {
    let answer = ok(
        router,
        "DescribeTrusts",
        json!({ "DirectoryId": directory_id }),
        case,
    )
    .await;
    answer["Trusts"].as_array().cloned().unwrap_or_default()
}

/// Wait until the directory holds no trust at all — the teardown `DeleteTrust` starts ends
/// with the relationship leaving this service, which a client polls for.
async fn gone_from(router: &MultiServiceRouter, directory_id: &str, case: &str) {
    let started = Instant::now();
    loop {
        let listed = trusts_of(router, directory_id, case).await;
        if listed.is_empty() {
            return;
        }
        assert!(
            started.elapsed() < SETTLE_DEADLINE,
            "case {case:?}: expected the relationship to be gone within {SETTLE_DEADLINE:?}, \
             still reported: {listed:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

#[tokio::test]
async fn the_trust_flow_runs_on_the_addresses_a_provisioned_directory_reports() {
    let network = network().await;
    let router = &network.router;

    // Two directories, each placed in a different pair of the account's own subnets, so the
    // addresses each one reports are a set of its own.
    let placements: [(&str, &str, [usize; 2]); 2] = [
        ("the first directory", "one.sim2real.net", [0, 1]),
        ("the second directory", "two.sim2real.net", [2, 3]),
    ];
    let mut directories: Vec<(&str, &str, String, usize)> = Vec::new();
    for (case, name, picked) in placements {
        let subnets: Vec<String> = picked
            .iter()
            .map(|index| network.client[*index].id.clone())
            .collect();
        let created = ok(
            router,
            "CreateMicrosoftAD",
            microsoft_ad_in(name, &network.vpc, &subnets),
            case,
        )
        .await;
        directories.push((case, name, string(&created, "DirectoryId", case), subnets.len()));
    }

    // Every create's stage chain is already running, so each directory is polled in turn.
    let mut provisioned: Vec<(&str, &str, String, Vec<String>)> = Vec::new();
    for (case, name, directory_id, subnet_count) in &directories {
        let addresses = dns_addresses_in_service(router, directory_id, case).await;
        assert_eq!(
            addresses.len(),
            *subnet_count,
            "case {case:?}: expected one DNS address per subnet the directory was placed in, \
             got {addresses:?}"
        );
        provisioned.push((case, name, directory_id.clone(), addresses));
    }

    // One row per direction: the directory the relationship hangs off, and the one whose
    // domain it points at — whose own reported addresses are the remote DNS servers the
    // conditional forwarder the trust needs is created with.
    let rows: [(usize, usize); 2] = [(0, 1), (1, 0)];
    for (holder, remote) in rows {
        let (holder_case, _, holder_id, _) = &provisioned[holder];
        let (_, remote_domain, _, remote_addresses) = &provisioned[remote];
        let case = format!("{holder_case} trusting {remote_domain}");

        let created = ok(
            router,
            "CreateTrust",
            json!({
                "DirectoryId": holder_id,
                "RemoteDomainName": remote_domain,
                "TrustPassword": TRUST_PASSWORD,
                "TrustDirection": "Two-Way",
                "ConditionalForwarderIpAddrs": remote_addresses,
            }),
            &case,
        )
        .await;
        let trust_id = string(&created, "TrustId", &case);

        // The relationship is established against the peer the remote domain has, then
        // verified — each transition owed by the clock, so each is polled for.
        settles_on(router, &trust_id, TRUST_STATE_CREATED, &case).await;
        ok(
            router,
            "VerifyTrust",
            json!({ "TrustId": &trust_id }),
            &case,
        )
        .await;
        settles_on(router, &trust_id, TRUST_STATE_VERIFIED, &case).await;

        // The forwarder the trust created carries exactly the addresses the remote directory
        // reports, so the flow really ran on what the describe answered.
        let forwarders = ok(
            router,
            "DescribeConditionalForwarders",
            json!({ "DirectoryId": holder_id, "RemoteDomainNames": [remote_domain] }),
            &case,
        )
        .await;
        assert_eq!(
            forwarders["ConditionalForwarders"][0]["DnsIpAddrs"],
            json!(remote_addresses),
            "case {case:?}: expected the forwarder to carry the remote directory's own DNS \
             addresses, got {forwarders}"
        );

        // And the teardown runs to its end: the relationship leaves this service, so the
        // directory holds none again.
        ok(
            router,
            "DeleteTrust",
            json!({
                "TrustId": &trust_id,
                "DeleteAssociatedConditionalForwarder": true,
            }),
            &case,
        )
        .await;
        gone_from(router, holder_id, &case).await;

        // ...and once it has left, the id names nothing this service holds: the listing that
        // asks for it by id is refused rather than answered with an empty page, and a SECOND
        // delete of the same id is refused the same way instead of reporting a teardown it
        // did not start. Both are the one wording this service states for a relationship it
        // does not hold, with the id taken from the request.
        refused_as_gone(
            router,
            "DescribeTrusts",
            json!({ "DirectoryId": holder_id, "TrustIds": [&trust_id] }),
            &trust_id,
            &case,
        )
        .await;
        refused_as_gone(
            router,
            "DeleteTrust",
            json!({ "TrustId": &trust_id }),
            &trust_id,
            &case,
        )
        .await;
    }
}
