// SPDX-License-Identifier: CC0-1.0

//! The two control-plane walks a client drives around a Managed Microsoft AD on a network
//! Amazon EC2 provisioned: the whole life of a trust relationship, and the refusal that sends
//! a removal of the directory's OWN Region to `DeleteDirectory` instead.
//!
//! Both are driven over the wire protocols a client uses — ec2Query for the network, JSON 1.1
//! for the Directory Service control plane — through one router over ONE shared engine, so
//! every subnet an accepted request names is one EC2 handed out and every address asserted
//! below is one the owner of network interfaces really assigned. Nothing reaches into either
//! store.
//!
//! RULE 1 (the refusal): a call this service refuses changes nothing a later caller can
//! observe. A `RemoveRegion` the directory cannot serve — a directory that does not replicate
//! across Regions at all (a Simple AD, an AD Connector, a Standard-edition Managed Microsoft
//! AD), and the edition that does replicate called in the Region it lives in, whose primary
//! Region holds no replica entry — is refused with the class and the sentence that failure
//! has, leaves the whole description exactly as it was, in service rather than being torn
//! down, and is refused again the same way because nothing moved; and the operation the
//! documentation names instead ("You cannot remove the primary Region with this operation.
//! Instead, use the DeleteDirectory API") is what takes the directory away: the directory
//! leaves service inside the call, while the teardown is what the clock finishes, so the
//! record is read through the modelled `Deleting` stage to the chain's terminal `Deleted` and
//! then out of the account's records — the rule the owning crate states and its own suites
//! pin (`services/service-com-amazonaws-directoryservice-directoryservice-20150416`:
//! `src/ops/delete_directory.rs`, `src/state/directory_deletion.rs`).
//!
//! RULE 2 (the trust walk): a trust id names ONE relationship for as long as this service
//! holds it, and every state of the modelled `TrustState` chain is reached by the clock alone
//! and reported to the client that polls for it — `Creating` then `Created`, `Verifying` then
//! `Verified`, `Deleting` then gone. Once it IS gone, every operation whose subject that id is
//! reports the same absence in the same words: `EntityDoesNotExistException`, HTTP 400, the
//! entity kind and the id the REQUEST named, closed by the call's own request id — whether the
//! id arrives as a `DescribeTrusts` filter (alone, or narrowed by the directory it hung off),
//! as the subject of a repeated `DeleteTrust`, or as the subject of a `VerifyTrust`.
//!
//! The rows differ along what each rule generalises over: for the refusal, the KIND of
//! directory the refusal class is decided by (four creates, two refusal classes, two message
//! templates); for the walk, which of two directories holds the relationship and which one's
//! domain it points at, so each row runs on a different pair of provisioned DNS addresses.
//!
//! Every state either walk waits for is POLLED for through the API the way a client's waiter
//! polls, so no case depends on how fast the host runs.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, form_encode};

/// The Region every request below is signed for, and so the Region the network and the
/// directories live in.
const REGION: &str = "us-west-2";

/// The EC2 API version a client sends in an ec2Query body.
const EC2_API_VERSION: &str = "2016-11-15";

/// The `X-Amz-Target` prefix Directory Service names an operation under: its service shape's
/// name.
const DS_TARGET_PREFIX: &str = "DirectoryService_20150416";

/// A password the modelled `Password` pattern admits.
const PASSWORD: &str = "Passw0rd!2345";

/// A `TrustPassword` the modelled pattern `^(\p{LD}|\p{Punct}| )+$` admits, within its
/// `length 1..128`.
const TRUST_PASSWORD: &str = "Tru5t-P@ssword 1";

/// How long a polled state may take to settle before a case fails: far longer than any span
/// either chain publishes, so a loaded or faster host changes nothing about the outcome.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);

/// How often a poll asks again.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The stages of the modelled `DirectoryStage` enum these walks read
/// (`smithy_query(query="shape", shape="DirectoryStage")`).
const STAGE_ACTIVE: &str = "Active";
const STAGE_DELETING: &str = "Deleting";
const STAGE_DELETED: &str = "Deleted";

/// The states of the modelled `TrustState` enum the walk below reads
/// (`smithy_query(query="shape", shape="TrustState")`).
const TRUST_STATE_CREATING: &str = "Creating";
const TRUST_STATE_CREATED: &str = "Created";
const TRUST_STATE_VERIFYING: &str = "Verifying";
const TRUST_STATE_VERIFIED: &str = "Verified";
const TRUST_STATE_DELETING: &str = "Deleting";

/// The modelled error shape an id this service no longer holds is reported with
/// (`smithy_query(query="shape", shape="EntityDoesNotExistException")`): a client error at
/// HTTP 400 carrying `Message` and `RequestId`.
const ENTITY_DOES_NOT_EXIST: &str = "EntityDoesNotExistException";

/// The suffix this service appends to every refusal message, before the per-run request id the
/// call is correlated by.
const REQUEST_ID_SEPARATOR: &str = " : RequestId: ";

/// The FIXED sentence a multi-Region call on a directory that does not replicate across
/// Regions is refused with: the rule the directory broke, with neither the directory nor the
/// Region interpolated into it.
const MULTI_REGION_ENTERPRISE_ONLY: &str = "Multi-region replication is only supported for \
                                            AWS Managed Microsoft AD (Enterprise edition) \
                                            directories.";

/// Amazon EC2 and Directory Service over ONE shared engine, as the production router mounts
/// them.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_directoryservice_directoryservice_20150416::EmulatedService::new(
            engine,
        ),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name`, which is what
/// routes the request here; the signature is never checked.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}\
         /aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One ec2Query action that must succeed, answered with its XML body.
async fn ec2(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> String {
    let mut body = format!("Action={action}&Version={EC2_API_VERSION}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("ec2.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("ec2"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "setup: EC2 {action} must succeed, got {status} with {text}"
    );
    text
}

/// One Directory Service control-plane call: its status and its parsed body.
async fn ds(router: &MultiServiceRouter, operation: &str, body: Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("ds.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{DS_TARGET_PREFIX}.{operation}"))
        .header("authorization", authorization("ds"))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let raw = response.into_body();
    let parsed = if raw.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&raw).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One Directory Service call that must succeed, answered with its body.
async fn ds_ok(router: &MultiServiceRouter, operation: &str, body: Value, case: &str) -> Value {
    let (status, parsed) = ds(router, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected {operation} to answer 200, got {status} with {parsed}"
    );
    parsed
}

/// A required string member of a response body.
fn string(body: &Value, member: &str, case: &str) -> String {
    body[member]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a string {member}, got {body}"))
        .to_string()
}

/// Poll `probe` until the value it answers satisfies `settled`, and answer that value — or
/// fail naming the case, what was waited for and the last value observed.
///
/// The CONDITION is what is waited for rather than a span of time, which is how a client's
/// waiter drives a chain the clock owes.
async fn poll_until<T, P, F>(
    case: &str,
    awaited: &str,
    probe: P,
    settled: impl Fn(&T) -> bool,
) -> T
where
    T: std::fmt::Debug,
    P: Fn() -> F,
    F: std::future::Future<Output = T>,
{
    let started = Instant::now();
    loop {
        let observed = probe().await;
        if settled(&observed) {
            return observed;
        }
        assert!(
            started.elapsed() < SETTLE_DEADLINE,
            "case {case:?}: expected {awaited} within {SETTLE_DEADLINE:?}, last observed \
             {observed:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One VPC of the account carrying one subnet per entry of `zones`, created through EC2's own
/// operations, as `(vpc id, subnet ids)`.
async fn ec2_vpc(
    router: &MultiServiceRouter,
    second_octet: u8,
    zones: &[&str],
) -> (String, Vec<String>) {
    let cidr = format!("10.{second_octet}.0.0/16");
    let vpc_id = element(
        &ec2(router, "CreateVpc", &[("CidrBlock", &cidr)]).await,
        "vpcId",
    );
    let mut subnets = Vec::new();
    for (index, zone) in zones.iter().enumerate() {
        let subnet_cidr = format!("10.{second_octet}.{}.0/24", index + 1);
        subnets.push(element(
            &ec2(
                router,
                "CreateSubnet",
                &[
                    ("VpcId", vpc_id.as_str()),
                    ("CidrBlock", subnet_cidr.as_str()),
                    ("AvailabilityZone", zone),
                ],
            )
            .await,
            "subnetId",
        ));
    }
    (vpc_id, subnets)
}

/// The one `DirectoryDescription` the reporting operation gives for `directory_id`, or `None`
/// once the account holds no record for it.
async fn description(
    router: &MultiServiceRouter,
    directory_id: &str,
    case: &str,
) -> Option<Value> {
    let answer = ds_ok(
        router,
        "DescribeDirectories",
        json!({ "DirectoryIds": [directory_id] }),
        case,
    )
    .await;
    let page = answer["DirectoryDescriptions"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: expected DirectoryDescriptions, got {answer}"));
    assert!(
        page.len() <= 1,
        "case {case:?}: expected at most one description for {directory_id}, got {answer}"
    );
    page.first().cloned()
}

/// The `Stage` that description reports, or `None` once there is no description.
async fn stage(router: &MultiServiceRouter, directory_id: &str, case: &str) -> Option<String> {
    description(router, directory_id, case)
        .await
        .map(|described| string(&described, "Stage", case))
}

/// Wait until `directory_id` reports itself in service, and answer the description it then
/// has.
async fn in_service(router: &MultiServiceRouter, directory_id: &str, case: &str) -> Value {
    poll_until(
        case,
        &format!("{directory_id} to report itself in service"),
        || description(router, directory_id, case),
        |observed: &Option<Value>| {
            observed
                .as_ref()
                .is_some_and(|described| described["Stage"] == json!(STAGE_ACTIVE))
        },
    )
    .await
    .expect("the poll above only settles on a description")
}

/// One row of the refusal table: the create that makes the directory, and the refusal its
/// `RemoveRegion` must get.
struct Refused {
    case: &'static str,
    operation: &'static str,
    /// The create body, given the VPC and the subnets EC2 minted.
    body: fn(&str, &[String]) -> Value,
    /// The modelled error shape the refusal is answered in.
    code: &'static str,
    /// The diagnostic that refusal states, given the id the create minted.
    diagnostic: fn(&str) -> String,
}

/// The fixed sentence, whatever directory the request named.
fn multi_region_enterprise_only(_directory_id: &str) -> String {
    MULTI_REGION_ENTERPRISE_ONLY.to_string()
}

/// The directory-lookup sentence this service states for an id it cannot act on, built from
/// the id the REQUEST named.
fn no_such_directory(directory_id: &str) -> String {
    format!("Directory: '{directory_id}' does not exist.")
}

/// The refusal table: one row per kind of directory `RemoveRegion` refuses, spanning both
/// refusal classes the operation has for a directory that IS stored.
fn refusals() -> [Refused; 4] {
    [
        Refused {
            case: "a Simple AD",
            operation: "CreateDirectory",
            body: |vpc, subnets| {
                json!({
                    "Name": "walk-simple.sim2real.net",
                    "Password": PASSWORD,
                    "Size": "Small",
                    "VpcSettings": { "VpcId": vpc, "SubnetIds": subnets },
                })
            },
            code: "UnsupportedOperationException",
            diagnostic: multi_region_enterprise_only,
        },
        Refused {
            case: "an AD Connector",
            operation: "ConnectDirectory",
            body: |vpc, subnets| {
                json!({
                    "Name": "walk-connected.sim2real.net",
                    "Password": PASSWORD,
                    "Size": "Small",
                    "ConnectSettings": {
                        "VpcId": vpc,
                        "SubnetIds": subnets,
                        "CustomerDnsIps": ["10.0.0.10", "10.0.1.10"],
                        "CustomerUserName": "Admin",
                    },
                })
            },
            code: "UnsupportedOperationException",
            diagnostic: multi_region_enterprise_only,
        },
        Refused {
            case: "a Standard-edition Managed Microsoft AD",
            operation: "CreateMicrosoftAD",
            body: |vpc, subnets| {
                json!({
                    "Name": "walk-standard.sim2real.net",
                    "Password": PASSWORD,
                    "Edition": "Standard",
                    "VpcSettings": { "VpcId": vpc, "SubnetIds": subnets },
                })
            },
            code: "UnsupportedOperationException",
            diagnostic: multi_region_enterprise_only,
        },
        Refused {
            // The edition that DOES replicate, called in the Region the directory itself lives
            // in: its primary Region holds no replica entry, which is the lookup the
            // documentation sends to `DeleteDirectory` instead.
            case: "an Enterprise-edition Managed Microsoft AD in its own primary Region",
            operation: "CreateMicrosoftAD",
            body: |vpc, subnets| {
                json!({
                    "Name": "walk-enterprise.sim2real.net",
                    "Password": PASSWORD,
                    "VpcSettings": { "VpcId": vpc, "SubnetIds": subnets },
                })
            },
            code: "DirectoryDoesNotExistException",
            diagnostic: no_such_directory,
        },
    ]
}

/// `RemoveRegion` for `directory_id`, asserting it is refused in this row's class with this
/// row's sentence closed by the call's own request id, and answer that request id.
async fn refused_removal(
    router: &MultiServiceRouter,
    row: &Refused,
    directory_id: &str,
) -> String {
    let case = row.case;
    let (status, parsed) = ds(
        router,
        "RemoveRegion",
        json!({ "DirectoryId": directory_id }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected RemoveRegion to be refused, got {status} with {parsed}"
    );
    let wire_type = parsed["__type"].as_str().unwrap_or_default();
    assert!(
        wire_type.contains(row.code),
        "case {case:?}: expected {code}, got {parsed}",
        code = row.code
    );
    let request_id = string(&parsed, "RequestId", case);
    assert_eq!(
        string(&parsed, "Message", case),
        format!(
            "{diagnostic}{REQUEST_ID_SEPARATOR}{request_id}",
            diagnostic = (row.diagnostic)(directory_id)
        ),
        "case {case:?}: expected the sentence for this refusal class, closed by the call's own \
         request id, got {parsed}"
    );
    request_id
}

#[tokio::test]
async fn a_refused_region_removal_is_inert_and_delete_directory_takes_the_directory_away() {
    let router = emulator();
    // The network the directories sit on, built by EC2 itself: two subnets in two zones, as
    // every placement requires.
    let (vpc, subnets) = ec2_vpc(&router, 80, &["us-west-2a", "us-west-2b"]).await;

    let rows = refusals();
    let mut created: Vec<(&Refused, String)> = Vec::new();
    for row in &rows {
        let answer = ds_ok(&router, row.operation, (row.body)(&vpc, &subnets), row.case).await;
        created.push((row, string(&answer, "DirectoryId", row.case)));
    }

    for (row, directory_id) in &created {
        let case = row.case;
        let before = in_service(&router, directory_id, case).await;

        let first = refused_removal(&router, row, directory_id).await;

        // Nothing about the directory moved: the whole record is reported exactly as it was,
        // and the stage it is in is the serving one rather than either stage of a teardown.
        let after = description(&router, directory_id, case).await;
        assert_eq!(
            after.as_ref(),
            Some(&before),
            "case {case:?}: expected the refused removal to leave the description alone, got \
             {after:?} instead of {before}"
        );
        let reported = after
            .as_ref()
            .map(|described| string(described, "Stage", case));
        assert_eq!(
            reported.as_deref(),
            Some(STAGE_ACTIVE),
            "case {case:?}: expected the directory to still be in service after the refusal, \
             got stage {reported:?}"
        );
        for not_now in [STAGE_DELETING, STAGE_DELETED] {
            assert_ne!(
                reported.as_deref(),
                Some(not_now),
                "case {case:?}: expected the refusal to start no teardown, got stage \
                 {reported:?}"
            );
        }

        // And because nothing moved, the identical call is refused the same way — its own
        // request id, the same class, the same sentence, the same record afterwards.
        let second = refused_removal(&router, row, directory_id).await;
        assert_ne!(
            first, second,
            "case {case:?}: expected each refusal to carry its own request id, got {first} twice"
        );
        let again = description(&router, directory_id, case).await;
        assert_eq!(
            again.as_ref(),
            Some(&before),
            "case {case:?}: expected the second refusal to leave the description alone, got \
             {again:?}"
        );

        // The operation the documentation names instead is what takes the directory away. The
        // directory leaves SERVICE inside the call — everything it was given is released in
        // the services that own it before the answer — while the teardown itself is what the
        // clock finishes: the record stays READABLE in the modelled `Deleting` stage, reaches
        // the chain's terminal `Deleted` a transition span later and leaves the account's
        // records after that, which is the walk a client polls its own delete through (the
        // rule and its spans are the owning crate's: `src/ops/delete_directory.rs`,
        // `src/state/directory_deletion.rs`).
        let deleted = ds_ok(
            &router,
            "DeleteDirectory",
            json!({ "DirectoryId": directory_id }),
            case,
        )
        .await;
        assert_eq!(
            deleted["DirectoryId"],
            json!(directory_id),
            "case {case:?}: expected the delete to report the id it was asked about, got \
             {deleted}"
        );
        // The FIRST read after the delete, with nothing waited for: the teardown's own stage,
        // which is what a client that reads the directory straight after the call observes.
        assert_eq!(
            stage(&router, directory_id, case).await.as_deref(),
            Some(STAGE_DELETING),
            "case {case:?}: expected the teardown's first stage straight after the delete"
        );

        // The stage after that one is the terminal stage of the modelled chain, reached by the
        // clock alone — the polls only read — and the end of the teardown is the record
        // leaving the store.
        let settled = poll_until(
            case,
            &format!("{directory_id} to leave {STAGE_DELETING}"),
            || stage(&router, directory_id, case),
            |observed: &Option<String>| observed.as_deref() != Some(STAGE_DELETING),
        )
        .await;
        assert_eq!(
            settled.as_deref(),
            Some(STAGE_DELETED),
            "case {case:?}: expected the teardown to settle on the chain's terminal stage, got \
             {settled:?}"
        );
        poll_until(
            case,
            &format!("{directory_id} to leave the store"),
            || stage(&router, directory_id, case),
            |observed: &Option<String>| observed.is_none(),
        )
        .await;
    }
}

/// Assert `trust_id` has the form the model gives a `TrustId`
/// (`smithy_query(query="shape", shape="TrustId")`: pattern `^t-[0-9a-f]{10}$`).
fn assert_trust_id(trust_id: &str, case: &str) {
    let digits = trust_id.strip_prefix("t-").unwrap_or_default();
    let hexadecimal = digits
        .chars()
        .all(|digit| digit.is_ascii_digit() || ('a'..='f').contains(&digit));
    assert!(
        digits.len() == 10 && hexadecimal,
        "case {case:?}: expected a TrustId matching ^t-[0-9a-f]{{10}}$, got {trust_id:?}"
    );
}

/// The `TrustState` `DescribeTrusts` reports for `trust_id`, or `None` once the id names no
/// relationship this service holds.
async fn trust_state(
    router: &MultiServiceRouter,
    trust_id: &str,
    case: &str,
) -> Option<String> {
    let (status, parsed) = ds(router, "DescribeTrusts", json!({ "TrustIds": [trust_id] })).await;
    if status == 400 {
        return None;
    }
    assert_eq!(
        status, 200,
        "case {case:?}: expected DescribeTrusts to answer, got {status} with {parsed}"
    );
    Some(string(&parsed["Trusts"][0], "TrustState", case))
}

/// Wait until the relationship reports `expected`.
async fn settles_on(router: &MultiServiceRouter, trust_id: &str, expected: &str, case: &str) {
    poll_until(
        case,
        &format!("{trust_id} to report itself {expected}"),
        || trust_state(router, trust_id, case),
        |observed: &Option<String>| observed.as_deref() == Some(expected),
    )
    .await;
}

/// The addresses an Active directory reports its DNS servers answer at.
fn dns_addresses(described: &Value, case: &str) -> Vec<String> {
    described["DnsIpAddrs"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: expected a DnsIpAddrs list, got {described}"))
        .iter()
        .map(|address| {
            address
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: expected an address, got {address}"))
                .to_string()
        })
        .collect()
}

/// The probes a client makes about a trust id, as `(operation, body)` — every implemented
/// operation of this service whose subject is one trust id, plus the two shapes the
/// `DescribeTrusts` filter arrives in.
fn trust_probes(directory_id: &str, trust_id: &str) -> Vec<(&'static str, Value)> {
    vec![
        ("DescribeTrusts", json!({ "TrustIds": [trust_id] })),
        (
            "DescribeTrusts",
            json!({ "DirectoryId": directory_id, "TrustIds": [trust_id] }),
        ),
        ("VerifyTrust", json!({ "TrustId": trust_id })),
        ("DeleteTrust", json!({ "TrustId": trust_id })),
    ]
}

#[tokio::test]
async fn the_trust_walk_runs_to_its_end_and_a_gone_trust_is_one_absence_for_every_operation() {
    let router = emulator();
    // Four subnets across two zones, so each directory is placed in a pair of its own and the
    // addresses it reports come out of blocks EC2 holds.
    let (vpc, subnets) = ec2_vpc(
        &router,
        81,
        &["us-west-2a", "us-west-2b", "us-west-2a", "us-west-2b"],
    )
    .await;

    // Two Managed Microsoft AD directories, each the peer the other's relationship points at.
    let placements: [(&str, &str, [usize; 2]); 2] = [
        ("the first directory", "one.sim2real.net", [0, 1]),
        ("the second directory", "two.sim2real.net", [2, 3]),
    ];
    let mut directories: Vec<(&str, &str, String, usize)> = Vec::new();
    for (case, name, picked) in placements {
        let chosen: Vec<String> = picked.iter().map(|index| subnets[*index].clone()).collect();
        let answer = ds_ok(
            &router,
            "CreateMicrosoftAD",
            json!({
                "Name": name,
                "Password": PASSWORD,
                "VpcSettings": { "VpcId": &vpc, "SubnetIds": &chosen },
            }),
            case,
        )
        .await;
        directories.push((case, name, string(&answer, "DirectoryId", case), chosen.len()));
    }

    // Each build is polled to its end, and an Active directory reports the addresses its DNS
    // servers answer at — one per subnet it was placed in, which is what a client configures
    // a forwarder or a trust from.
    let mut provisioned: Vec<(&str, &str, String, Vec<String>)> = Vec::new();
    for (case, name, directory_id, subnet_count) in &directories {
        let described = in_service(&router, directory_id, case).await;
        let addresses = dns_addresses(&described, case);
        assert_eq!(
            addresses.len(),
            *subnet_count,
            "case {case:?}: expected one DNS address per subnet the directory was placed in, \
             got {addresses:?}"
        );
        provisioned.push((case, name, directory_id.clone(), addresses));
    }

    // One row per direction the relationship is established in: the directory it hangs off,
    // and the one whose domain it points at — whose own reported addresses are the remote DNS
    // servers the conditional forwarder is created with.
    for (holder, remote) in [(0usize, 1usize), (1, 0)] {
        let (holder_case, _, holder_id, _) = &provisioned[holder];
        let (_, remote_domain, _, remote_addresses) = &provisioned[remote];
        let case = format!("{holder_case} trusting {remote_domain}");

        ds_ok(
            &router,
            "CreateConditionalForwarder",
            json!({
                "DirectoryId": holder_id,
                "RemoteDomainName": remote_domain,
                "DnsIpAddrs": remote_addresses,
            }),
            &case,
        )
        .await;

        // The create is answered while the Amazon Web Services side is still being
        // established, and the relationship settles on `Created` by the clock alone.
        let created = ds_ok(
            &router,
            "CreateTrust",
            json!({
                "DirectoryId": holder_id,
                "RemoteDomainName": remote_domain,
                "TrustPassword": TRUST_PASSWORD,
                "TrustDirection": "Two-Way",
            }),
            &case,
        )
        .await;
        let trust_id = string(&created, "TrustId", &case);
        assert_trust_id(&trust_id, &case);
        assert_eq!(
            trust_state(&router, &trust_id, &case).await.as_deref(),
            Some(TRUST_STATE_CREATING),
            "case {case:?}: expected the create to answer before the relationship is \
             established"
        );
        settles_on(&router, &trust_id, TRUST_STATE_CREATED, &case).await;

        // The verification is the same: answered in the transient state, settled by the clock.
        let verified = ds_ok(
            &router,
            "VerifyTrust",
            json!({ "TrustId": &trust_id }),
            &case,
        )
        .await;
        assert_eq!(
            verified["TrustId"],
            json!(&trust_id),
            "case {case:?}: expected the verification to name the relationship it ran on, got \
             {verified}"
        );
        assert_eq!(
            trust_state(&router, &trust_id, &case).await.as_deref(),
            Some(TRUST_STATE_VERIFYING),
            "case {case:?}: expected the verification to answer while it is running"
        );
        settles_on(&router, &trust_id, TRUST_STATE_VERIFIED, &case).await;

        // And so is the teardown: the delete is answered with the relationship still being
        // taken out of service, and the end of that chain is the relationship being gone.
        let deleted = ds_ok(
            &router,
            "DeleteTrust",
            json!({ "TrustId": &trust_id }),
            &case,
        )
        .await;
        assert_eq!(
            deleted["TrustId"],
            json!(&trust_id),
            "case {case:?}: expected the delete to name the relationship it tore down, got \
             {deleted}"
        );
        assert_eq!(
            trust_state(&router, &trust_id, &case).await.as_deref(),
            Some(TRUST_STATE_DELETING),
            "case {case:?}: expected the delete to answer while the teardown is under way"
        );
        poll_until(
            &case,
            &format!("{trust_id} to leave the service"),
            || trust_state(&router, &trust_id, &case),
            |observed: &Option<String>| observed.is_none(),
        )
        .await;

        // One absence, reported the same way by every operation whose subject the id is.
        for (operation, body) in trust_probes(holder_id, &trust_id) {
            let probe = format!("{case}, through {operation}");
            let (status, parsed) = ds(&router, operation, body).await;
            assert_eq!(
                status, 400,
                "case {probe:?}: expected the gone relationship to be refused, got {status} \
                 with {parsed}"
            );
            let wire_type = parsed["__type"].as_str().unwrap_or_default();
            assert!(
                wire_type.contains(ENTITY_DOES_NOT_EXIST),
                "case {probe:?}: expected {ENTITY_DOES_NOT_EXIST}, got {parsed}"
            );
            let request_id = string(&parsed, "RequestId", &probe);
            assert_eq!(
                string(&parsed, "Message", &probe),
                format!("Trust: {trust_id} does not exist{REQUEST_ID_SEPARATOR}{request_id}"),
                "case {probe:?}: expected the not-found sentence for the id the request named, \
                 closed by the call's own request id, got {parsed}"
            );
        }

        // The directory it hung off is still in service and holds no relationship again, so
        // the absence is the relationship's rather than the directory's.
        let left = ds_ok(
            &router,
            "DescribeTrusts",
            json!({ "DirectoryId": holder_id }),
            &case,
        )
        .await;
        assert_eq!(
            left["Trusts"],
            json!([]),
            "case {case:?}: expected the directory to hold no relationship after the teardown, \
             got {left}"
        );
        assert_eq!(
            stage(&router, holder_id, &case).await.as_deref(),
            Some(STAGE_ACTIVE),
            "case {case:?}: expected the directory to still be in service"
        );
    }
}
