// SPDX-License-Identifier: CC0-1.0

//! ONE termination record, whichever path asked Amazon EC2 to terminate an instance.
//!
//! RULE: what a terminated instance's record looks like is Amazon EC2's to state, so every
//! path that terminates one — a caller naming the instance, or another service ending a
//! resource that IS an instance — leaves the SAME record behind: the instance reports
//! `shutting-down` at once with the clock owing it `terminated`, its `StateReason` is the
//! code and the message EC2 records for a shutdown somebody asked for
//! (`Client.UserInitiatedShutdown`, whose message "the message for the state change" spells
//! as that code followed by its description, `StateReason$Message`), and its
//! `Instance$StateTransitionReason` is the `User initiated (<instant>)` wording EC2 words a
//! requested transition with (the `reason` filter's own documentation,
//! `smithy_query(query="doc",shape="DescribeInstancesRequest",member="Filters")`). The
//! termination is IDEMPOTENT: asking a second time leaves the record that is already there,
//! rather than re-stamping it or moving the instance again.
//!
//! The two paths below are the two that can ask for it: Amazon EVS's
//! `DeleteEnvironmentHost`, which ends the bare metal instance an ESX host IS ("An ESX host
//! that runs on an Amazon EC2 bare metal instance", `Host` doc), and Amazon EC2's own
//! `TerminateInstances`. This suite drives one operation from each over the wire a client
//! speaks — awsJson1_0 for Amazon EVS, ec2Query for Amazon EC2 — against BOTH services
//! mounted on one shared engine, and asserts that the record the two leave is one record:
//! same state, same transition it owes, same reasons, and the same answer to being asked
//! again.
//!
//! What the rows span is the PATH that asks, which is what the rule generalises over; the
//! instance each path terminates is launched the way its own service launches one (a host
//! added to a deployed environment, an on-demand `RunInstances`), in its own VPC of the
//! account, and both are brought to the same `running` start state before either is asked
//! to go, so the two records are compared at the same point of an instance's life.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, elements, form_encode, opt_element};

/// The Region every request below is signed for, and so the Region both services serve it
/// in.
const REGION: &str = "us-west-2";

/// The single Availability Zone the network lives in: "Amazon EVS requires that all VPC
/// subnets exist in a single Availability Zone" (`vpcId` doc).
const ZONE: &str = "us-west-2a";

/// The EC2 API version a client sends in an ec2Query body.
const EC2_API_VERSION: &str = "2016-11-15";

/// The AMI the on-demand launch runs from, in the long form EC2 issues AMI ids in.
const AMI: &str = "ami-0123456789abcdef0";

/// The instance type BOTH paths launch, so the two records differ only in the path that
/// asked: the bare metal type an Amazon EVS host runs on.
const INSTANCE_TYPE: &str = "i4i.metal";

/// The VCF version the environment is created with, one the version catalogue offers.
const VCF_VERSION: &str = "VCF-5.2.1";

/// The `Support` plan this account holds: an Amazon EVS environment is created only for an
/// account subscribed to Premium Support, a fact the Support store owns.
const QUALIFYING_SUPPORT_PLAN: &str = "enterprise";

/// The `InstanceStateName` an instance reports while it is on its way out, and the terminal
/// one it reaches, and the one it is terminated FROM here.
const SHUTTING_DOWN: &str = "shutting-down";
/// See [`SHUTTING_DOWN`].
const TERMINATED: &str = "terminated";
/// See [`SHUTTING_DOWN`].
const RUNNING: &str = "running";

/// The `StateReason$Code` EC2 records for a shutdown a caller asked for.
const USER_INITIATED_CODE: &str = "Client.UserInitiatedShutdown";

/// The `StateReason$Message` that goes with it: the code, then its description.
const USER_INITIATED_MESSAGE: &str = "Client.UserInitiatedShutdown: User initiated shutdown";

/// What an `Instance$StateTransitionReason` of a requested transition opens with, and the
/// zone its instant is named by — `User initiated (2026-01-01 00:00:00 GMT)`.
const USER_INITIATED_PREFIX: &str = "User initiated (";
/// See [`USER_INITIATED_PREFIX`].
const USER_INITIATED_SUFFIX: &str = " GMT)";

/// The ten `InitialVlans` members an environment is provisioned with, in the order the
/// model declares them.
const VLAN_MEMBERS: [&str; 10] = [
    "vmkManagement",
    "vmManagement",
    "vMotion",
    "vSan",
    "vTep",
    "edgeVTep",
    "nsxUplink",
    "hcx",
    "expansionVlan1",
    "expansionVlan2",
];

/// How long a poll waits for a state the clock owes before it fails the test: a generous
/// deadline, not a measurement — nothing below asserts how long a transition took.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll waits between two looks.
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// One emulator carrying Amazon EVS and Amazon EC2 over ONE shared engine, as the
/// production router mounts them — the whole point, because the instance an Amazon EVS host
/// runs on is a record of the account's own EC2 store.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // These suites exercise behaviour, not signing.
    router.set_request_authentication(false);
    // The account's Support plan, in the store that owns it: an operator grants it out of
    // band, and Amazon EVS reads it from there when it creates an environment.
    service_com_amazonaws_support_awssupport_20130415::state::service_state(&*engine)
        .expect("the in-memory engine carries the Support store")
        .lock()
        .support_plan = Some(QUALIFYING_SUPPORT_PLAN.to_string());
    router.register_service(
        service_com_amazonaws_evs_amazonelasticvmwareservice::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        engine,
    ));
    router
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name`, which is
/// what routes the request and which Region it is served in; the signature is never
/// checked.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}\
         /aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// POST one ec2Query action, as its HTTP status and XML body.
async fn ec2_call(
    router: &MultiServiceRouter,
    action: &str,
    params: &[(&str, &str)],
) -> (u16, String) {
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
        .expect("a well-formed ec2Query request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// One ec2Query action that must be served, as its XML body.
async fn ec2(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let (status, body) = ec2_call(router, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: EC2 {action} expected HTTP 200, got {status} with {body}"
    );
    body
}

/// POST one Amazon EVS operation over awsJson1_0, as its HTTP status and parsed body.
async fn evs_call(
    router: &MultiServiceRouter,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("evs.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header(
            "x-amz-target",
            format!("AmazonElasticVMwareService.{operation}"),
        )
        .header("authorization", authorization("evs"))
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed awsJson1_0 request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One Amazon EVS operation that must be served, as its parsed body.
async fn evs(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let (status, parsed) = evs_call(router, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: EVS {operation} expected HTTP 200, got {status} with {parsed}"
    );
    parsed
}

/// What `DescribeInstances` reports about ONE instance's state right now: the whole
/// termination record, as a client reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Record {
    /// `Instance$State$Name` — the state word.
    state: String,
    /// `Instance$StateTransitionReason`, empty where the instance carries none.
    transition_reason: String,
    /// `Instance$StateReason` as `(code, message)`, absent where it carries none.
    state_reason: Option<(String, String)>,
}

impl Record {
    /// This record with the INSTANT inside its transition reason replaced by a placeholder.
    ///
    /// The instant is the one the termination was asked for at, so two terminations asked
    /// for at different moments carry different ones — and everything else about the two
    /// records has to be identical.
    fn without_the_instant(&self) -> Record {
        let transition_reason = match self.transition_reason.split_once('(') {
            Some((before, _)) => format!("{before}(<instant>)"),
            None => self.transition_reason.clone(),
        };
        Record {
            state: self.state.clone(),
            transition_reason,
            state_reason: self.state_reason.clone(),
        }
    }
}

/// Whether `reason` is the wording EC2 words a requested transition with, whatever instant
/// it names: `User initiated (<civil UTC date and time> GMT)`.
fn is_user_initiated_reason(reason: &str) -> bool {
    let Some(instant) = reason
        .strip_prefix(USER_INITIATED_PREFIX)
        .and_then(|rest| rest.strip_suffix(USER_INITIATED_SUFFIX))
    else {
        return false;
    };
    // `YYYY-MM-DD HH:MM:SS`: digits where the form has digits, and its own separators
    // everywhere else.
    let form = "0000-00-00 00:00:00";
    instant.len() == form.len()
        && instant
            .chars()
            .zip(form.chars())
            .all(|(got, expected)| match expected {
                '0' => got.is_ascii_digit(),
                separator => got == separator,
            })
}

/// The record `DescribeInstances` reports for `instance_id`, read through a describe
/// FILTERED to that id, so every member below comes from that one instance.
async fn record(router: &MultiServiceRouter, case: &str, instance_id: &str) -> Record {
    let xml = ec2(
        router,
        case,
        "DescribeInstances",
        &[("InstanceId.1", instance_id)],
    )
    .await;
    let reported: BTreeSet<String> = elements(&xml, "instanceId").into_iter().collect();
    assert_eq!(
        reported,
        BTreeSet::from([instance_id.to_string()]),
        "case {case:?}: the describe is filtered to {instance_id}, so it must report that \
         instance and no other, got {reported:?} in {xml}"
    );
    let instance_state = element(&xml, "instanceState");
    let state = element(&instance_state, "name");
    let transition_reason = opt_element(&xml, "reason").unwrap_or_default();
    let state_reason = opt_element(&xml, "stateReason").map(|reason| {
        (
            opt_element(&reason, "code").unwrap_or_default(),
            opt_element(&reason, "message").unwrap_or_default(),
        )
    });
    Record {
        state,
        transition_reason,
        state_reason,
    }
}

/// Look at `instance_id` again and again until EC2 reports it in `wanted`, and answer the
/// record it reports there.
///
/// This is how the suite waits for a move the clock owes an instance: it waits for the
/// state to BE what it waits for, with [`POLL_DEADLINE`] of room, rather than for a span of
/// wall clock a faster or busier host measures differently.
async fn poll_until_state(
    router: &MultiServiceRouter,
    case: &str,
    instance_id: &str,
    wanted: &str,
) -> Record {
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let got = record(router, case, instance_id).await;
        if got.state == wanted {
            return got;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: {instance_id} never reached {wanted:?}; last record {got:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// A VPC and one subnet inside it, created through Amazon EC2 ITSELF, so both ids name
/// resources the account really holds; `base` is the first two octets of the VPC's range,
/// so each path gets its own address space.
async fn create_network(router: &MultiServiceRouter, case: &str, base: &str) -> (String, String) {
    let created = ec2(
        router,
        case,
        "CreateVpc",
        &[("CidrBlock", &format!("{base}.0.0/16"))],
    )
    .await;
    let vpc_id = element(&created, "vpcId");
    let subnet = ec2(
        router,
        case,
        "CreateSubnet",
        &[
            ("VpcId", &vpc_id),
            // High in the VPC's range, clear of the VLAN blocks an environment is
            // provisioned with below.
            ("CidrBlock", &format!("{base}.200.0/24")),
            ("AvailabilityZone", ZONE),
        ],
    )
    .await;
    let subnet_id = element(&subnet, "subnetId");
    (vpc_id, subnet_id)
}

/// The ten VLAN subnet blocks an environment is created with, each in its own `/24` of the
/// VPC's range so none overlaps another or the service access subnet.
fn initial_vlans(base: &str) -> Value {
    let mut vlans = json!({});
    for (index, member) in VLAN_MEMBERS.iter().enumerate() {
        vlans[*member] = json!({ "cidr": format!("{base}.{index}.0/28") });
    }
    vlans
}

/// An Amazon EVS environment deployed into that network, waited for until it reports the
/// `CREATED` state a host can be added to ("This action can only be used after the Amazon
/// EVS environment is deployed", `CreateEnvironmentHost` doc).
async fn deployed_environment(
    router: &MultiServiceRouter,
    case: &str,
    base: &str,
    vpc_id: &str,
    subnet_id: &str,
) -> String {
    let created = evs(
        router,
        case,
        "CreateEnvironment",
        &json!({
            "environmentName": "termination-record",
            "vpcId": vpc_id,
            "serviceAccessSubnetId": subnet_id,
            "vcfVersion": VCF_VERSION,
            "termsAccepted": true,
            "initialVlans": initial_vlans(base),
        }),
    )
    .await;
    let environment_id = created["environment"]["environmentId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no environmentId in {created}"))
        .to_string();

    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let got = evs(
            router,
            case,
            "GetEnvironment",
            &json!({ "environmentId": &environment_id }),
        )
        .await;
        let state = got["environment"]["environmentState"]
            .as_str()
            .unwrap_or("<none>")
            .to_string();
        if state == "CREATED" {
            return environment_id;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the environment {environment_id} never reached CREATED; last \
             state {state:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Add one host to that environment and answer the id of the EC2 instance it runs on.
async fn create_host(
    router: &MultiServiceRouter,
    case: &str,
    environment_id: &str,
    host_name: &str,
) -> String {
    let created = evs(
        router,
        case,
        "CreateEnvironmentHost",
        &json!({
            "environmentId": environment_id,
            "host": {
                "hostName": host_name,
                "keyName": "evs-key",
                "instanceType": INSTANCE_TYPE,
            },
        }),
    )
    .await;
    created["host"]["ec2InstanceId"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: a host reports the instance it runs on, got {created}")
        })
        .to_string()
}

/// Launch one on-demand instance through Amazon EC2 itself and answer its id.
async fn run_instance(router: &MultiServiceRouter, case: &str, subnet_id: &str) -> String {
    let launched = ec2(
        router,
        case,
        "RunInstances",
        &[
            ("ImageId", AMI),
            ("MinCount", "1"),
            ("MaxCount", "1"),
            ("InstanceType", INSTANCE_TYPE),
            ("SubnetId", subnet_id),
        ],
    )
    .await;
    element(&launched, "instanceId")
}

/// One row: an instance of the account, and the operation that asks for its termination.
struct Row {
    /// What the row is, for every assertion message.
    case: &'static str,
    /// The instance the row's operation terminates.
    instance_id: String,
    /// The Amazon EVS environment and host the operation names, for the row whose path is
    /// a host deletion.
    host: Option<(String, String)>,
}

impl Row {
    /// Ask this row's path to terminate its instance. Must be served, both the first time
    /// and every later time: the termination is idempotent, not a one-shot.
    async fn ask_to_terminate(&self, router: &MultiServiceRouter) {
        match &self.host {
            Some((environment_id, host_name)) => {
                evs(
                    router,
                    self.case,
                    "DeleteEnvironmentHost",
                    &json!({ "environmentId": environment_id, "hostName": host_name }),
                )
                .await;
            }
            None => {
                ec2(
                    router,
                    self.case,
                    "TerminateInstances",
                    &[("InstanceId.1", &self.instance_id)],
                )
                .await;
            }
        }
    }
}

#[tokio::test]
async fn every_path_that_terminates_an_instance_leaves_one_record() {
    let router = emulator();

    // Row one: the instance an Amazon EVS host IS, launched by adding a host to a deployed
    // environment of its own VPC.
    let host_case = "DeleteEnvironmentHost (Amazon EVS)";
    let (vpc_id, subnet_id) = create_network(&router, host_case, "10.0").await;
    let environment_id =
        deployed_environment(&router, host_case, "10.0", &vpc_id, &subnet_id).await;
    let host_name = "esx-termination-record";
    let host_instance = create_host(&router, host_case, &environment_id, host_name).await;

    // Row two: an on-demand instance of the same account in its own VPC, launched and
    // terminated through Amazon EC2 itself.
    let ec2_case = "TerminateInstances (Amazon EC2)";
    let (_ec2_vpc_id, ec2_subnet_id) = create_network(&router, ec2_case, "10.1").await;
    let launched_instance = run_instance(&router, ec2_case, &ec2_subnet_id).await;

    let rows = [
        Row {
            case: host_case,
            instance_id: host_instance,
            host: Some((environment_id.clone(), host_name.to_string())),
        },
        Row {
            case: ec2_case,
            instance_id: launched_instance,
            host: None,
        },
    ];

    // Both instances are terminated from the SAME start state — the `running` their own
    // launches owed them — so the two records are compared at the same point of a life.
    for row in &rows {
        poll_until_state(&router, row.case, &row.instance_id, RUNNING).await;
    }

    // Each path asks, and each leaves a record.
    let mut terminating: Vec<Record> = Vec::new();
    for row in &rows {
        row.ask_to_terminate(&router).await;
        terminating.push(record(&router, row.case, &row.instance_id).await);
    }

    for (row, got) in rows.iter().zip(&terminating) {
        let case = row.case;
        assert_eq!(
            got.state, SHUTTING_DOWN,
            "case {case:?}: the instance must be on its way out at once, got {got:?}"
        );
        assert_eq!(
            got.state_reason,
            Some((
                USER_INITIATED_CODE.to_string(),
                USER_INITIATED_MESSAGE.to_string()
            )),
            "case {case:?}: expected the reason EC2 records for a requested shutdown, got \
             {got:?}"
        );
        assert!(
            is_user_initiated_reason(&got.transition_reason),
            "case {case:?}: expected a transition reason of the form \
             {USER_INITIATED_PREFIX}<instant>{USER_INITIATED_SUFFIX}, got {:?}",
            got.transition_reason
        );
    }

    // ONE record: the two paths differ in the instant the termination was asked for and in
    // nothing else.
    assert_eq!(
        terminating[0].without_the_instant(),
        terminating[1].without_the_instant(),
        "the two paths must leave the same record: {:?} from {:?} against {:?} from {:?}",
        terminating[0],
        rows[0].case,
        terminating[1],
        rows[1].case
    );

    // The clock owes each instance `terminated`, and the reasons survive the move: the
    // terminated record is kept, so a later read still says what ended the instance.
    let mut settled: Vec<Record> = Vec::new();
    for row in &rows {
        settled.push(poll_until_state(&router, row.case, &row.instance_id, TERMINATED).await);
    }
    for (row, got) in rows.iter().zip(&settled) {
        let case = row.case;
        assert_eq!(
            got.state_reason,
            Some((
                USER_INITIATED_CODE.to_string(),
                USER_INITIATED_MESSAGE.to_string()
            )),
            "case {case:?}: a terminated instance keeps the reason it ended for, got {got:?}"
        );
        assert!(
            is_user_initiated_reason(&got.transition_reason),
            "case {case:?}: a terminated instance keeps its transition reason, got {:?}",
            got.transition_reason
        );
    }
    assert_eq!(
        settled[0].without_the_instant(),
        settled[1].without_the_instant(),
        "the two paths must settle on the same record: {:?} from {:?} against {:?} from {:?}",
        settled[0],
        rows[0].case,
        settled[1],
        rows[1].case
    );

    // IDEMPOTENT: asking again, at the terminal state, leaves the record exactly as it is —
    // neither path re-stamps the reason nor moves the instance a second time.
    for (row, before) in rows.iter().zip(&settled) {
        row.ask_to_terminate(&router).await;
        let after = record(&router, row.case, &row.instance_id).await;
        assert_eq!(
            &after, before,
            "case {case:?}: a repeated termination must leave the record it found, got \
             {after:?} instead of {before:?}",
            case = row.case
        );
    }
}
