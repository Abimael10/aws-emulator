// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `DeleteVpcEndpointServiceConfigurations`, driven over `ec2Query`
//! (README.md / OPERATIONS.md "Wire protocol").
//!
//! The rule under test: the delete is a BULK call — it removes every endpoint service
//! configuration of the account that the ids it resolves name, taking the connections to each
//! service with it, and reports each id it could NOT delete in `Unsuccessful` ("information
//! about the service configurations that were not deleted, if applicable") with that cause's own
//! per-item code and sentence, rather than refusing the whole request for one id. A service an
//! endpoint connection is still attached to is one of those causes: "before you can delete an
//! endpoint service configuration, you must reject any Available or PendingAcceptance interface
//! endpoint connections that are attached to the service". A deleted service then stops being
//! described, a service the request did not name is untouched, and an authorized `DryRun`
//! answers `DryRunOperation` having deleted nothing.
//!
//! Each table row is a distinct input along what that rule generalises over: how many ids one
//! request carries, which of them resolve to a service, WHERE in the list the ones that do not
//! sit, which form an id that resolves to nothing takes, how often one id is repeated, how long
//! the id list is next to the ceiling a caller-chosen list is held to, and which state the
//! connections to a service are in.

use ec2::ops::types::ServiceConfiguration;
use ec2::state::{ComAmazonawsEc2Amazonec2State as Ec2State, VpcEndpointConnectionSpec};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use super::ec2_query::{error_code, filters, indexed, unescaped};
use super::wire::{Param, child, child_text, document, param, set_items};

/// The Region every call here is signed for.
const REGION: &str = "us-west-2";

/// The HTTP status EC2 answers `DryRunOperation` with: the request was refused because a
/// precondition the CALLER set holds, not because it was malformed.
const DRY_RUN_STATUS: u16 = 412;

/// An account other than the emulator's own, as the owner of a seeded endpoint connection.
const CONSUMER_ACCOUNT: &str = "210987654321";

/// The emulator the tests drive, with the engine beside it: a connection to one of the
/// account's own endpoint services is written through the store, no operation of this service
/// creating one today.
fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    super::ec2_query::emulator()
}

/// One `action` call carrying `params`, as its HTTP status and XML body.
async fn call(router: &MultiServiceRouter, action: &str, params: &[Param]) -> (u16, String) {
    super::ec2_query::call(router, REGION, action, params).await
}

/// One `action` call that has to succeed, and the body it answered.
async fn ok(case: &str, router: &MultiServiceRouter, action: &str, params: &[Param]) -> String {
    let (status, body) = call(router, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// One element of a response body, or a panic naming the case and showing the body.
fn member(case: &str, body: &str, name: &str) -> String {
    super::ec2_query::element(body, name)
        .unwrap_or_else(|| panic!("case {case:?}: no <{name}> in {body}"))
}

/// One entry of a response's `Unsuccessful` set, as a client reads it.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Reported {
    /// `ResourceId`: the id the request named.
    resource_id: String,
    /// `Error.Code`.
    code: String,
    /// `Error.Message`, with the XML entities resolved.
    message: String,
}

/// The `Unsuccessful` entries of a delete's body, in the order it reported them.
///
/// Read through the XML parser from the response ENVELOPE's own `unsuccessful` child, so an
/// entry is the envelope's rather than a set of the same shape nested inside something else,
/// and an empty report (no set at all, or a set with no items) reads as no entries.
fn unsuccessful(case: &str, body: &str) -> Vec<Reported> {
    let parsed = document(case, body);
    set_items(parsed.root_element(), "unsuccessful")
        .into_iter()
        .map(|item| {
            let error = child(item, "error").unwrap_or_else(|| {
                panic!("case {case:?}: an unsuccessful item carries no <error> in {body}")
            });
            Reported {
                resource_id: child_text(item, "resourceId").unwrap_or_default(),
                code: child_text(error, "code").unwrap_or_default(),
                message: unescaped(&child_text(error, "message").unwrap_or_default()),
            }
        })
        .collect()
}

/// What a well-formed id this account offers no endpoint service under must be reported as: the
/// code every operation of this service answers an unresolvable `ServiceId` with, and the
/// sentence that cause carries — the resource named as AWS names it, around the id from the
/// request.
fn not_found_report(service_id: &str) -> Reported {
    Reported {
        resource_id: service_id.to_string(),
        code: ec2::state::INVALID_VPC_ENDPOINT_SERVICE_ID_NOT_FOUND.to_string(),
        message: ec2::state::id_not_found_message(
            ec2::state::VPC_ENDPOINT_SERVICE_ID_KIND,
            service_id,
        ),
    }
}

/// What a value that is not a service id of this kind must be reported as: EC2's one
/// `Invalid id: "<value>"` sentence, with the value from the request in double quotes.
fn malformed_report(value: &str) -> Reported {
    Reported {
        resource_id: value.to_string(),
        code: ec2::state::INVALID_VPC_ENDPOINT_SERVICE_ID_MALFORMED.to_string(),
        message: ec2::state::invalid_id_message(value),
    }
}

/// A well-formed VPC endpoint service id, distinct per `slot`, far above anything this store's
/// id counter can reach — so it resolves to no service however many services a test created.
fn unresolvable_id(slot: usize) -> String {
    format!(
        "{prefix}-{suffix:017x}",
        prefix = ec2::state::VPC_ENDPOINT_SERVICE_ID_PREFIX,
        suffix = 0x0f00_0000_0000_0000_u64 + slot as u64,
    )
}

/// The forms a value that is NO endpoint service id takes, one per slot that needs one.
///
/// Both are values a client plausibly sends: the service prefix with a suffix that is not
/// hexadecimal, and a VPC ENDPOINT id — the shorter form of the same `vpce-` family, which
/// names an endpoint rather than a service.
const MALFORMED_VALUES: [&str; 2] = ["vpce-svc-not-hex", "vpce-0a1b2c3d4e5f6a7b8"];

/// The ARN of the load balancer `name` of `kind` — `net` for a Network Load Balancer and `gwy`
/// for a Gateway Load Balancer, the resource paths Elastic Load Balancing gives each.
///
/// A load balancer is a resource of another service, so the ARN is only ever a well-formed ARN
/// here: what this crate stores is the ARN the request named.
fn load_balancer_arn(kind: &str, name: &str) -> String {
    format!(
        "arn:aws:elasticloadbalancing:{REGION}:{account}:loadbalancer/{kind}/{name}/0123456789abcdef",
        account = engine::emulator_account_id(),
    )
}

/// Configure one endpoint service of the account through the API, behind a Network Load Balancer
/// named `name`, and answer the `ServiceId` the create minted.
async fn create_service(case: &str, router: &MultiServiceRouter, name: &str) -> String {
    let body = ok(
        case,
        router,
        "CreateVpcEndpointServiceConfiguration",
        &[param(
            "NetworkLoadBalancerArn.1",
            &load_balancer_arn("net", name),
        )],
    )
    .await;
    member(case, &body, "serviceId")
}

/// Whether `DescribeVpcEndpointServiceConfigurations` still resolves `service_id`.
///
/// The describe refuses a `ServiceId` the account offers no service under
/// (`InvalidVpcEndpointServiceId.NotFound`), which is how a deleted service reads as gone
/// through the operation that reports the store rather than through the store itself.
async fn still_described(case: &str, router: &MultiServiceRouter, service_id: &str) -> bool {
    let (status, body) = call(
        router,
        "DescribeVpcEndpointServiceConfigurations",
        &[param("ServiceId.1", service_id)],
    )
    .await;
    match status {
        200 => {
            assert_eq!(
                super::ec2_query::elements(&body, "serviceId"),
                vec![service_id.to_string()],
                "case {case:?}: the describe of {service_id} must report that one service, got \
                 {body}"
            );
            true
        }
        _ => {
            assert_eq!(
                error_code(&body),
                ec2::state::INVALID_VPC_ENDPOINT_SERVICE_ID_NOT_FOUND,
                "case {case:?}: a service that is gone must be refused as unresolvable, got \
                 {body}"
            );
            false
        }
    }
}

/// Whether `DescribeVpcEndpointConnections` still reports the connection of `vpc_endpoint_id`.
async fn connection_reported(
    case: &str,
    router: &MultiServiceRouter,
    vpc_endpoint_id: &str,
) -> bool {
    let body = ok(
        case,
        router,
        "DescribeVpcEndpointConnections",
        &filters(&[("vpc-endpoint-id", &[vpc_endpoint_id])]),
    )
    .await;
    !super::ec2_query::elements(&body, "vpcEndpointId").is_empty()
}

/// Write one connection to `service_id` in `connection_state`, as a consumer account's endpoint
/// to that service, and answer the `VpcEndpointId` it is keyed by.
///
/// Written through the store's one connection writer: no operation of this service creates a
/// connection today, and what the delete is judged on is the state a connection is IN.
fn seed_connection(
    engine: &engine::InMemoryEngine,
    service_id: &str,
    connection_state: &str,
) -> String {
    let store = engine.state::<Ec2State>();
    let mut guard = store.lock();
    let vpc_endpoint_id = guard.next_id(ec2::state::VPC_ENDPOINT_ID_PREFIX);
    guard.insert_vpc_endpoint_connection(VpcEndpointConnectionSpec {
        service_id: service_id.to_string(),
        vpc_endpoint_id: vpc_endpoint_id.clone(),
        vpc_endpoint_owner: CONSUMER_ACCOUNT.to_string(),
        vpc_endpoint_state: connection_state.to_string(),
        creation_timestamp: SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000),
        vpc_endpoint_region: REGION.to_string(),
        ip_address_type: Some(ec2::state::IP_ADDRESS_TYPE_IPV4.to_string()),
        dns_entries: Vec::new(),
        network_load_balancer_arns: Vec::new(),
        gateway_load_balancer_arns: Vec::new(),
        payer_responsibilities: Vec::new(),
        tags: Vec::new(),
    });
    vpc_endpoint_id
}

/// What one slot of a request's `ServiceIds` list carries.
#[derive(Clone, Copy, Debug)]
enum Slot {
    /// A service this row configured, which the delete has to remove.
    Real,
    /// A well-formed id this account offers no service under.
    Unresolvable,
    /// A value that is not a service id of this kind at all.
    Malformed,
}

/// One request naming any mix of slots deletes every service it resolves and reports each id it
/// could not, with that id's own cause — and the service the request never named is untouched.
#[tokio::test]
async fn each_named_service_is_deleted_or_reported_with_its_own_cause() {
    // Rows: the list the request carries, differing in length, in which slots resolve, and in
    // where the ones that do not sit.
    let rows: [(&str, &[Slot]); 7] = [
        (
            "two services with an id naming nothing after them",
            &[Slot::Real, Slot::Real, Slot::Unresolvable],
        ),
        (
            "one service with an id naming nothing after it",
            &[Slot::Real, Slot::Unresolvable],
        ),
        (
            "the ids naming nothing around the one service",
            &[Slot::Unresolvable, Slot::Real, Slot::Unresolvable],
        ),
        (
            "two services with a value that is no id between them",
            &[Slot::Real, Slot::Malformed, Slot::Real, Slot::Unresolvable],
        ),
        (
            "nothing but ids naming nothing",
            &[Slot::Unresolvable, Slot::Unresolvable],
        ),
        (
            "nothing but values that are no ids at all",
            &[Slot::Malformed, Slot::Malformed],
        ),
        ("one service alone", &[Slot::Real]),
    ];

    for (case, slots) in rows {
        let (router, _engine) = emulator();

        // The service the request does NOT name, configured first so it is in the store
        // throughout: a bulk delete acts on the ids it was given and on nothing else.
        let untouched = create_service(case, &router, "untouched").await;

        let mut named: Vec<String> = Vec::new();
        let mut real: Vec<String> = Vec::new();
        let mut expected: Vec<Reported> = Vec::new();
        for (slot_index, slot) in slots.iter().enumerate() {
            match slot {
                Slot::Real => {
                    let service_id =
                        create_service(case, &router, &format!("real-{slot_index}")).await;
                    real.push(service_id.clone());
                    named.push(service_id);
                }
                Slot::Unresolvable => {
                    let service_id = unresolvable_id(slot_index);
                    expected.push(not_found_report(&service_id));
                    named.push(service_id);
                }
                Slot::Malformed => {
                    let value = MALFORMED_VALUES[expected.len() % MALFORMED_VALUES.len()];
                    expected.push(malformed_report(value));
                    named.push(value.to_string());
                }
            }
        }

        // Every service the row configured is readable before the delete.
        for service_id in &real {
            assert!(
                still_described(case, &router, service_id).await,
                "case {case:?}: the configured service {service_id} must be readable before \
                 the delete"
            );
        }

        let references: Vec<&str> = named.iter().map(String::as_str).collect();
        let body = ok(
            case,
            &router,
            "DeleteVpcEndpointServiceConfigurations",
            &indexed("ServiceId", &references),
        )
        .await;
        let reported = unsuccessful(case, &body);
        assert_eq!(
            reported, expected,
            "case {case:?}: expected exactly the ids that could not be deleted, each with its \
             own cause, got {reported:?} in {body}"
        );

        for service_id in &real {
            assert!(
                !still_described(case, &router, service_id).await,
                "case {case:?}: the service {service_id} the delete resolved must be gone"
            );
        }
        assert!(
            still_described(case, &router, &untouched).await,
            "case {case:?}: the service {untouched} the request never named must be untouched"
        );
    }
}

/// Repeating one id asks for it ONCE: a real id named several times deletes that service and
/// reports nothing, and an id that resolves to nothing is reported once however often it is
/// named — never once as deleted and once as missing.
#[tokio::test]
async fn one_id_named_several_times_is_asked_for_once() {
    // Rows: how often the id is repeated, and whether it resolves to a service.
    let rows: [(&str, usize, bool); 4] = [
        ("a service named twice", 2, true),
        ("a service named three times", 3, true),
        ("an id naming nothing, named twice", 2, false),
        ("an id naming nothing, named four times", 4, false),
    ];

    for (case, repeats, resolves) in rows {
        let (router, _engine) = emulator();
        let service_id = create_service(case, &router, "repeated").await;
        let named = if resolves {
            service_id.clone()
        } else {
            unresolvable_id(0)
        };

        let references: Vec<&str> = (0..repeats).map(|_| named.as_str()).collect();
        let body = ok(
            case,
            &router,
            "DeleteVpcEndpointServiceConfigurations",
            &indexed("ServiceId", &references),
        )
        .await;
        let reported = unsuccessful(case, &body);
        let expected: Vec<Reported> = if resolves {
            Vec::new()
        } else {
            vec![not_found_report(&named)]
        };
        assert_eq!(
            reported, expected,
            "case {case:?}: expected the id named {repeats} times to be reported once at most, \
             got {reported:?} in {body}"
        );
        assert_eq!(
            still_described(case, &router, &service_id).await,
            !resolves,
            "case {case:?}: expected the service {service_id} gone exactly when the request \
             resolved it"
        );
    }
}

/// An authorized `DryRun` is answered `DryRunOperation` and deletes nothing — "checks whether
/// you have the required permissions for the action, without actually making the request"
/// (`DryRun`) — whether or not the ids it carries resolve, because a per-item cause is not a
/// refusal of the call.
#[tokio::test]
async fn a_dry_run_reports_dry_run_operation_and_deletes_nothing() {
    // Rows: the list the dry run carries.
    let rows: [(&str, &[Slot]); 4] = [
        ("the one service", &[Slot::Real]),
        (
            "the service beside an id naming nothing",
            &[Slot::Real, Slot::Unresolvable],
        ),
        ("an id naming nothing alone", &[Slot::Unresolvable]),
        ("a value that is no id at all", &[Slot::Malformed]),
    ];

    for (case, slots) in rows {
        let (router, _engine) = emulator();
        let service_id = create_service(case, &router, "dry-run").await;

        let named: Vec<String> = slots
            .iter()
            .enumerate()
            .map(|(slot_index, slot)| match slot {
                Slot::Real => service_id.clone(),
                Slot::Unresolvable => unresolvable_id(slot_index),
                Slot::Malformed => MALFORMED_VALUES[0].to_string(),
            })
            .collect();
        let references: Vec<&str> = named.iter().map(String::as_str).collect();
        let mut params = indexed("ServiceId", &references);
        params.push(param("DryRun", "true"));

        let (status, body) = call(&router, "DeleteVpcEndpointServiceConfigurations", &params).await;
        assert_eq!(
            status, DRY_RUN_STATUS,
            "case {case:?}: expected HTTP {DRY_RUN_STATUS} from an authorized dry run, got \
             {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            ec2::state::DRY_RUN_OPERATION,
            "case {case:?}: expected DryRunOperation, got {body}"
        );
        assert!(
            still_described(case, &router, &service_id).await,
            "case {case:?}: a dry run must leave the service {service_id} in place"
        );
    }
}

/// The id list is capped BEFORE any per-item work: a list one id past the ceiling a
/// caller-chosen id list is held to is refused `InvalidParameterValue` naming that ceiling and
/// deletes nothing, while a list AT the ceiling is carried out in full.
#[tokio::test]
async fn an_id_list_past_the_cap_is_refused_before_anything_is_deleted() {
    let maximum = ec2::state::MAX_RESOURCE_IDS;
    // Rows: the length of the list, and whether it is within the cap.
    let rows: [(&str, usize, bool); 2] = [
        ("one id past the ceiling", maximum + 1, false),
        ("the list at the ceiling", maximum, true),
    ];

    for (case, length, within) in rows {
        let (router, _engine) = emulator();
        let service_id = create_service(case, &router, "capped").await;

        // The real service first, then one distinct well-formed id per remaining slot, so
        // nothing but the LENGTH decides.
        let mut named = vec![service_id.clone()];
        named.extend((1..length).map(unresolvable_id));
        let references: Vec<&str> = named.iter().map(String::as_str).collect();
        let (status, body) = call(
            &router,
            "DeleteVpcEndpointServiceConfigurations",
            &indexed("ServiceId", &references),
        )
        .await;

        if within {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the list at the ceiling to be carried out, got \
                 {status} with body {body}"
            );
            assert_eq!(
                unsuccessful(case, &body).len(),
                length - 1,
                "case {case:?}: expected one report per id that resolved nothing, got {body}"
            );
        } else {
            assert_eq!(
                status, 400,
                "case {case:?}: expected the list past the ceiling to be refused, got {status} \
                 with body {body}"
            );
            assert_eq!(
                error_code(&body),
                "InvalidParameterValue",
                "case {case:?}: expected InvalidParameterValue, got {body}"
            );
            assert!(
                body.contains(&maximum.to_string()),
                "case {case:?}: the refusal must name the ceiling {maximum}, got {body}"
            );
        }
        assert_eq!(
            still_described(case, &router, &service_id).await,
            !within,
            "case {case:?}: expected the service deleted exactly when the request was carried \
             out"
        );
    }
}

/// A service an endpoint connection is still ATTACHED to is reported rather than deleted —
/// "before you can delete an endpoint service configuration, you must reject any Available or
/// PendingAcceptance interface endpoint connections that are attached to the service" — while a
/// service whose connections have all been acted on is deleted, and its connections go with it.
#[tokio::test]
async fn a_service_a_connection_is_attached_to_is_reported_rather_than_deleted() {
    // Rows: the state the one connection to the service is in, and whether the owner still has
    // it to reject. The states are the catalog the filter that selects on this very state
    // publishes (`ec2::state::VPC_ENDPOINT_STATE_VALUES`).
    let rows: [(&str, &str, bool); 7] = [
        (
            "a connection waiting for the owner to accept it",
            ec2::state::VPC_ENDPOINT_STATE_PENDING_ACCEPTANCE,
            true,
        ),
        (
            "a connection in service",
            ec2::state::VPC_ENDPOINT_STATE_AVAILABLE,
            true,
        ),
        (
            "a connection whose acceptance is being carried out",
            ec2::state::VPC_ENDPOINT_STATE_PENDING,
            true,
        ),
        ("a connection the owner rejected", "rejected", false),
        ("a connection being removed", "deleting", false),
        ("a connection that is gone", "deleted", false),
        ("a connection that failed", "failed", false),
    ];

    for (case, connection_state, still_attached) in rows {
        let (router, engine) = emulator();
        let service_id = create_service(case, &router, "connected").await;
        let vpc_endpoint_id = seed_connection(&engine, &service_id, connection_state);

        let body = ok(
            case,
            &router,
            "DeleteVpcEndpointServiceConfigurations",
            &indexed("ServiceId", &[service_id.as_str()]),
        )
        .await;
        let reported = unsuccessful(case, &body);

        if still_attached {
            assert_eq!(
                reported.len(),
                1,
                "case {case:?}: expected the service reported as undeletable, got \
                 {reported:?} in {body}"
            );
            assert_eq!(
                (reported[0].resource_id.as_str(), reported[0].code.as_str()),
                (
                    service_id.as_str(),
                    ec2::state::EXISTING_VPC_ENDPOINT_CONNECTIONS
                ),
                "case {case:?}: expected the service's own id with the existing-connections \
                 cause, got {reported:?}"
            );
            assert!(
                reported[0].message.contains(connection_state)
                    && reported[0].message.contains(&service_id),
                "case {case:?}: expected the report to name the service and the state its \
                 connection is in, got {:?}",
                reported[0].message
            );
            assert!(
                still_described(case, &router, &service_id).await,
                "case {case:?}: the service {service_id} must still be offered"
            );
            assert!(
                connection_reported(case, &router, &vpc_endpoint_id).await,
                "case {case:?}: the connection {vpc_endpoint_id} must still be reported"
            );
        } else {
            assert_eq!(
                reported,
                Vec::new(),
                "case {case:?}: expected nothing reported unsuccessful, got {body}"
            );
            assert!(
                !still_described(case, &router, &service_id).await,
                "case {case:?}: the service {service_id} must be gone"
            );
            assert!(
                !connection_reported(case, &router, &vpc_endpoint_id).await,
                "case {case:?}: the connection {vpc_endpoint_id} cannot outlive the service it \
                 was to"
            );
        }
    }
}

/// The sentence each per-item cause carries, spelled out rather than built the way the handler
/// builds it: EC2 names the resource `VpcEndpointService Id` and quotes the id from the REQUEST
/// in single quotes for an id it resolves nothing for, and words a value that is no id of this
/// kind as `Invalid id: "<value>"`, in double quotes.
#[tokio::test]
async fn the_per_item_report_words_each_cause_as_aws_does() {
    let case = "the two causes a bulk delete reports";
    let (router, _engine) = emulator();
    let absent = unresolvable_id(0);
    let malformed = MALFORMED_VALUES[0];

    let body = ok(
        case,
        &router,
        "DeleteVpcEndpointServiceConfigurations",
        &indexed("ServiceId", &[absent.as_str(), malformed]),
    )
    .await;
    assert_eq!(
        unsuccessful(case, &body),
        vec![
            Reported {
                resource_id: absent.clone(),
                code: "InvalidVpcEndpointServiceId.NotFound".to_string(),
                message: format!("The VpcEndpointService Id '{absent}' does not exist"),
            },
            Reported {
                resource_id: malformed.to_string(),
                code: "InvalidVpcEndpointServiceId.Malformed".to_string(),
                message: format!("Invalid id: \"{malformed}\""),
            },
        ],
        "case {case:?}: expected EC2's own code and sentence per cause, got {body}"
    );
}

/// What one row of the byte-budget table configures: the caller-chosen data the configuration
/// retains, which is what the delete has to give back.
struct BudgetCase {
    case: &'static str,
    /// The Network Load Balancers behind the service, by name.
    load_balancers: &'static [&'static str],
    /// `PrivateDnsName`, or `None` to leave the member out.
    private_dns_name: Option<&'static str>,
    /// `SupportedRegions`, which may be empty.
    regions: &'static [&'static str],
    /// The tags to assign, as key/value pairs.
    tags: &'static [(&'static str, &'static str)],
}

/// The byte budget the store charges caller-supplied data against is given back by the delete:
/// configuring services and then deleting them leaves the store holding what it held before,
/// whatever the configurations carried.
#[tokio::test]
async fn deleting_a_service_gives_its_caller_supplied_bytes_back() {
    let (router, engine) = emulator();
    let stored_bytes = || engine.state::<Ec2State>().lock().stored_bytes();
    let before = stored_bytes();

    // Rows: configurations differing in what caller-chosen data they retain — the load
    // balancers behind them, the private DNS name assigned, the Regions offered and the tags.
    let rows: [BudgetCase; 4] = [
        BudgetCase {
            case: "one load balancer and nothing else",
            load_balancers: &["alpha"],
            private_dns_name: None,
            regions: &[],
            tags: &[],
        },
        BudgetCase {
            case: "two load balancers and a private DNS name",
            load_balancers: &["beta", "gamma"],
            private_dns_name: Some("payments.example.com"),
            regions: &[],
            tags: &[],
        },
        BudgetCase {
            case: "offered from two other Regions",
            load_balancers: &["delta"],
            private_dns_name: None,
            regions: &["us-east-1", "eu-west-1"],
            tags: &[],
        },
        BudgetCase {
            case: "tagged, named and offered elsewhere",
            load_balancers: &["epsilon"],
            private_dns_name: Some("shared.example.org"),
            regions: &["us-east-2"],
            tags: &[("Owner", "TeamA"), ("Tier", "shared")],
        },
    ];

    for row in rows {
        let BudgetCase {
            case,
            load_balancers,
            private_dns_name,
            regions,
            tags,
        } = row;
        let arns: Vec<String> = load_balancers
            .iter()
            .map(|name| load_balancer_arn("net", name))
            .collect();
        let mut params = indexed(
            "NetworkLoadBalancerArn",
            &arns.iter().map(String::as_str).collect::<Vec<&str>>(),
        );
        if let Some(name) = private_dns_name {
            params.push(param("PrivateDnsName", name));
        }
        params.extend(indexed("SupportedRegion", regions));
        for (index, (key, value)) in tags.iter().enumerate() {
            params.push(param("TagSpecification.1.ResourceType", "vpc-endpoint-service"));
            params.push(param(
                &format!("TagSpecification.1.Tag.{}.Key", index + 1),
                key,
            ));
            params.push(param(
                &format!("TagSpecification.1.Tag.{}.Value", index + 1),
                value,
            ));
        }
        params.push(param("ClientToken", &format!("token-{case}")));

        let body = ok(
            case,
            &router,
            "CreateVpcEndpointServiceConfiguration",
            &params,
        )
        .await;
        let service_id = member(case, &body, "serviceId");
        let while_held = stored_bytes();
        assert!(
            while_held > before,
            "case {case:?}: the configuration must be charged against the budget, which held \
             {before} bytes and now holds {while_held}"
        );

        let deleted = ok(
            case,
            &router,
            "DeleteVpcEndpointServiceConfigurations",
            &indexed("ServiceId", &[service_id.as_str()]),
        )
        .await;
        assert_eq!(
            unsuccessful(case, &deleted),
            Vec::new(),
            "case {case:?}: expected the delete to report nothing unsuccessful, got {deleted}"
        );
        let after = stored_bytes();
        assert_eq!(
            after, before,
            "case {case:?}: the delete must give every byte back: the store held {before} \
             bytes before the configuration and holds {after} after deleting it"
        );
    }
}

/// A service seeded into the store with no name of its own is still deletable: the delete reads
/// the store, not the strings a create derived — so a record every writer can produce resolves.
#[tokio::test]
async fn a_stored_configuration_without_derived_names_is_deletable() {
    let (router, engine) = emulator();
    let service_id = {
        let store = engine.state::<Ec2State>();
        let mut guard = store.lock();
        guard
            .insert_vpc_endpoint_service(ServiceConfiguration {
                service_state: Some(ec2::state::SERVICE_STATE_AVAILABLE.to_string()),
                acceptance_required: Some(true),
                ..Default::default()
            })
            .expect("the store holds the seeded endpoint service")
    };

    let body = ok(
        "a seeded configuration",
        &router,
        "DeleteVpcEndpointServiceConfigurations",
        &indexed("ServiceId", &[service_id.as_str()]),
    )
    .await;
    assert_eq!(
        unsuccessful("a seeded configuration", &body),
        Vec::new(),
        "a stored configuration must delete without a report, got {body}"
    );
    assert!(
        !engine
            .state::<Ec2State>()
            .lock()
            .vpc_endpoint_services
            .contains_key(&service_id),
        "the seeded configuration {service_id} must be out of the store"
    );
}
