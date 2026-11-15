// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the target slice (`RegisterTargets` + `ListTargets`).
//!
//! Every case drives the service the way a client does — a signed restJson1 request
//! through the router that mounts it — and reads what was registered back out of a LATER
//! request, so nothing here can pass on a canned body: a target that is never stored
//! fails the read-back, and a response echoed from the request fails the port-defaulting
//! and status assertions.
//!
//! The harness (`Lattice`, `Answer`, `as_label`, …) is the one the service-slice suite
//! already builds; nothing is duplicated here.
//!
//! The rules under test, each asserted over a TABLE of inputs rather than one row:
//!
//! 1. `RegisterTargets` stores every submitted target on the target group it names and
//!    reports it under `successful`; `ListTargets` then reports exactly what was stored,
//!    through either identifier form (the bare id or the ARN).
//! 2. A target is reached on the port the registration sent, or on the target group's own
//!    configured port — the default `Target.port` documents for the protocol the targets
//!    are reached over — and on no port at all where a port does not apply to the target.
//! 3. What a target's `id` is follows the target group's TYPE (`Target`'s own `id`
//!    documentation); an id of another form is a 400 `ValidationException` that stores
//!    nothing.
//! 4. A registration that would leave a target group holding more targets than its type
//!    allows one is a 400 `ValidationException` over the maximum that type allows, and
//!    the targets already registered stay registered. The recorded LAMBDA case is one row
//!    of it.
//! 5. A target is its id TOGETHER with the port it is reached on: registering the same
//!    pair twice holds one target.
//! 6. A target's status is derived from the stored records and the clock: `UNUSED` while
//!    nothing routes to its target group, `UNAVAILABLE` where the group health-checks
//!    nothing, and otherwise `INITIAL` until the configured initial health checking is
//!    due and `HEALTHY` from then on.
//! 7. A listing is paged and filterable: `maxResults` bounds a page, `nextToken` pages the
//!    rest exactly once each, and a `targets` filter keeps the targets it names.
//! 8. An identifier that names no persisted target group is a 404
//!    `ResourceNotFoundException`, and a registration against a target group whose
//!    deletion is already under way is a 409 `ConflictException`.

use std::time::{Duration, Instant};

use service_com_amazonaws_vpclattice_mercurycontrolplane::state;

use crate::listeners::{a_service, create_listener};
use crate::resource_not_found_messages::{absent_id, assert_not_found};
use crate::rules::assert_refused;
use crate::target_groups::{create_target_group, get_target_group, A_VPC};
use crate::vpclattice_service_slice::{as_label, message_of, Answer, Lattice};

/// How long a wait sleeps between two looks at the resource.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// How long a wait looks before it gives up. Generous on purpose: a case waits for the
/// CONDITION it needs, never for a fixed time, so a loaded host only makes the loop go
/// round more often.
const POLL_DEADLINE: Duration = Duration::from_secs(120);

/// A Lambda function ARN — what a target of a `LAMBDA` target group is ("If the target
/// group type is `LAMBDA`, this is the ARN of a Lambda function", `Target`'s `id`). In the
/// CALLER's own account ([`state::account_id`]), which is what this service registers a
/// target of; a function of another account is reported per target (see the
/// `per_target_failures_and_deregistration` suite, which pins that the account here is the
/// caller's). Whether the function exists is Lambda's state, not this service's.
pub(crate) const A_LAMBDA: &str =
    "arn:aws:lambda:us-west-2:000000000000:function:order-handler";

/// A second, distinct Lambda function ARN; see [`A_LAMBDA`].
pub(crate) const ANOTHER_LAMBDA: &str =
    "arn:aws:lambda:us-west-2:000000000000:function:refund-handler";

/// An Application Load Balancer ARN — what a target of an `ALB` target group is, in the
/// caller's own account; see [`A_LAMBDA`].
pub(crate) const AN_ALB: &str =
    "arn:aws:elasticloadbalancing:us-west-2:000000000000:loadbalancer/app/edge/50dc6c495c0c9188";

/// One row of the registration table: the case, the target-group type, the config the
/// group is created with, the targets the registration sends, and the (id, port) pairs
/// the service has to PERSIST.
type RegistrationCase = (
    &'static str,
    &'static str,
    Option<serde_json::Value>,
    serde_json::Value,
    Vec<(&'static str, Option<i64>)>,
);

/// One row of the status table: the case, the target-group type, the config the group is
/// created with, whether a listener forwards to it, the target registered with it, and
/// the `TargetStatus` that target has to report.
type StatusCase = (
    &'static str,
    &'static str,
    Option<serde_json::Value>,
    bool,
    &'static str,
    &'static str,
);

/// The config an `IP` or `INSTANCE` target group cannot be created without, over `HTTP`
/// with no port, so the port a target gets is the one the protocol documents.
pub(crate) fn vpc_config(protocol: &str, port: Option<i64>) -> serde_json::Value {
    let mut config = serde_json::json!({ "protocol": protocol, "vpcIdentifier": A_VPC });
    if let Some(port) = port {
        config["port"] = serde_json::json!(port);
    }
    config
}

/// `POST /targetgroups/{targetGroupIdentifier}/registertargets`.
pub(crate) async fn register_targets(
    lattice: &Lattice,
    identifier: &str,
    targets: serde_json::Value,
) -> Answer {
    lattice
        .call(
            "POST",
            &format!("/targetgroups/{identifier}/registertargets"),
            Some(serde_json::json!({ "targets": targets })),
        )
        .await
}

/// `POST /targetgroups/{targetGroupIdentifier}/listtargets`, with the query members and
/// the `targets` filter this case sends.
pub(crate) async fn list_targets(
    lattice: &Lattice,
    identifier: &str,
    query: &[(&str, String)],
    filter: Option<serde_json::Value>,
) -> Answer {
    let mut path = format!("/targetgroups/{identifier}/listtargets");
    if !query.is_empty() {
        let pairs: Vec<String> = query
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect();
        path.push('?');
        path.push_str(&pairs.join("&"));
    }
    let body = match filter {
        Some(targets) => serde_json::json!({ "targets": targets }),
        None => serde_json::json!({}),
    };
    lattice.call("POST", &path, Some(body)).await
}

/// Create a target group that must succeed, answering its id and ARN.
pub(crate) async fn a_target_group(
    lattice: &Lattice,
    name: &str,
    group_type: &str,
    config: Option<serde_json::Value>,
) -> (String, String) {
    let mut body = serde_json::json!({ "name": name, "type": group_type });
    if let Some(config) = config {
        body["config"] = config;
    }
    let created = create_target_group(lattice, body.clone()).await;
    assert_eq!(
        created.status, 201,
        "case fixture target group {body}: expected 201, got {} / {}",
        created.status, created.body
    );
    (created.str_field("id"), created.str_field("arn"))
}

/// The (id, port) pairs a member of a response carries, sorted so two answers compare by
/// content rather than by order.
pub(crate) fn pairs_of(case: &str, answer: &Answer, member: &str) -> Vec<(String, Option<i64>)> {
    let entries = answer.body[member].as_array().cloned().unwrap_or_else(|| {
        panic!(
            "case {case:?}: expected the member {member} to be a list, got {}",
            answer.body
        )
    });
    let mut pairs: Vec<(String, Option<i64>)> = entries
        .iter()
        .map(|entry| {
            (
                entry["id"]
                    .as_str()
                    .unwrap_or_else(|| panic!("case {case:?}: a target without an id: {entry}"))
                    .to_string(),
                entry["port"].as_i64(),
            )
        })
        .collect();
    pairs.sort();
    pairs
}

/// The (id, port) pairs `ListTargets` reports for `identifier`, with no filter and no
/// paging.
pub(crate) async fn listed_pairs(
    lattice: &Lattice,
    case: &str,
    identifier: &str,
) -> Vec<(String, Option<i64>)> {
    let listed = list_targets(lattice, identifier, &[], None).await;
    assert_eq!(
        listed.status, 200,
        "case {case:?}: expected 200 from ListTargets, got {} / {}",
        listed.status, listed.body
    );
    pairs_of(case, &listed, "items")
}

/// The status `ListTargets` reports for the one target of `identifier`.
async fn only_target_status(lattice: &Lattice, case: &str, identifier: &str) -> String {
    let listed = list_targets(lattice, identifier, &[], None).await;
    assert_eq!(
        listed.status, 200,
        "case {case:?}: expected 200 from ListTargets, got {} / {}",
        listed.status, listed.body
    );
    let items = listed.body["items"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        items.len(),
        1,
        "case {case:?}: expected exactly one target, got {}",
        listed.body
    );
    items[0]["status"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: a TargetSummary without a status: {}", items[0]))
        .to_string()
}

/// A listener of `service_identifier` whose default action forwards to `target_group_id`,
/// so something routes to that target group.
pub(crate) async fn forward_to(
    lattice: &Lattice,
    service_identifier: &str,
    name: &str,
    port: i64,
    target_group_id: &str,
) {
    let created = create_listener(
        lattice,
        service_identifier,
        serde_json::json!({
            "name": name,
            "protocol": "HTTP",
            "port": port,
            "defaultAction": {
                "forward": { "targetGroups": [{ "targetGroupIdentifier": target_group_id }] },
            },
        }),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "case fixture listener {name:?}: expected 201, got {} / {}",
        created.status, created.body
    );
}

/// Rules 1 and 2, over a table that varies the target-group type, what each target is,
/// how many are registered at once and whether the registration sends a port.
#[tokio::test]
async fn registering_targets_persists_them_and_both_identifier_forms_read_them_back() {
    let lattice = Lattice::new();

    // (case, type, config, submitted targets, the (id, port) pairs that must be PERSISTED)
    let cases: Vec<RegistrationCase> = vec![
        (
            "LAMBDA: one function ARN, no port",
            "LAMBDA",
            None,
            serde_json::json!([{ "id": A_LAMBDA }]),
            vec![(A_LAMBDA, None)],
        ),
        (
            "LAMBDA: a port does not apply to a function target",
            "LAMBDA",
            None,
            serde_json::json!([{ "id": A_LAMBDA, "port": 443 }]),
            vec![(A_LAMBDA, None)],
        ),
        (
            "IP over HTTP: an omitted port takes the protocol's documented default",
            "IP",
            Some(vpc_config("HTTP", None)),
            serde_json::json!([{ "id": "10.0.0.1" }, { "id": "10.0.0.2", "port": 8080 }]),
            vec![("10.0.0.1", Some(80)), ("10.0.0.2", Some(8080))],
        ),
        (
            "IP over HTTPS on a configured port, IPv6 address",
            "IP",
            Some(vpc_config("HTTPS", Some(8443))),
            serde_json::json!([{ "id": "2001:db8::1" }]),
            vec![("2001:db8::1", Some(8443))],
        ),
        (
            "INSTANCE: both documented instance-id lengths",
            "INSTANCE",
            Some(vpc_config("HTTP", Some(9000))),
            serde_json::json!([{ "id": "i-0123456789abcdef0" }, { "id": "i-0a1b2c3d" }]),
            vec![
                ("i-0123456789abcdef0", Some(9000)),
                ("i-0a1b2c3d", Some(9000)),
            ],
        ),
        (
            "ALB: a load balancer ARN on a port of its own",
            "ALB",
            Some(vpc_config("HTTPS", Some(443))),
            serde_json::json!([{ "id": AN_ALB, "port": 8443 }]),
            vec![(AN_ALB, Some(8443))],
        ),
    ];

    for (index, (case, group_type, config, targets, expected)) in cases.into_iter().enumerate() {
        let (id, arn) = a_target_group(
            &lattice,
            &format!("targets-{index}"),
            group_type,
            config.clone(),
        )
        .await;
        let mut expected: Vec<(String, Option<i64>)> = expected
            .into_iter()
            .map(|(target, port)| (target.to_string(), port))
            .collect();
        expected.sort();

        let registered = register_targets(&lattice, &id, targets.clone()).await;
        assert_eq!(
            registered.status, 200,
            "case {case:?}: expected 200, got {} / {}",
            registered.status, registered.body
        );
        assert_eq!(
            pairs_of(case, &registered, "successful"),
            expected,
            "case {case:?}: successful must report the targets as persisted, got {}",
            registered.body
        );
        assert_eq!(
            pairs_of(case, &registered, "unsuccessful"),
            Vec::new(),
            "case {case:?}: no target of this registration fails, got {}",
            registered.body
        );

        // Read back through BOTH identifier forms the model admits.
        for identifier in [id.clone(), as_label(&arn)] {
            let listed = listed_pairs(&lattice, case, &identifier).await;
            assert_eq!(
                listed, expected,
                "case {case:?}: ListTargets through {identifier} must report what was \
                 registered, got {listed:?}"
            );
        }
    }
}

/// Rule 3, over a table that varies the type and the way the submitted id departs from
/// the form that type documents. The CATALOG of forms is `Target`'s own `id`
/// documentation (`smithy_query(query="doc",shape="Target",member="id")`), so a type the
/// issue did not cite is covered too.
#[tokio::test]
async fn a_target_id_of_another_form_than_its_type_documents_is_refused() {
    let lattice = Lattice::new();

    // (case, type, config, the submitted id)
    let cases: Vec<(&str, &str, Option<serde_json::Value>, &str)> = vec![
        (
            "LAMBDA: an instance id is not a function ARN",
            "LAMBDA",
            None,
            "i-0123456789abcdef0",
        ),
        (
            "LAMBDA: an ARN of another service",
            "LAMBDA",
            None,
            AN_ALB,
        ),
        (
            "IP: not an address at all",
            "IP",
            Some(vpc_config("HTTP", None)),
            "not-an-address",
        ),
        (
            "IP: an octet past the last an address can carry",
            "IP",
            Some(vpc_config("HTTP", None)),
            "10.0.0.256",
        ),
        (
            "INSTANCE: a suffix that is not hexadecimal",
            "INSTANCE",
            Some(vpc_config("HTTP", None)),
            "i-0123456789abcdefg",
        ),
        (
            "INSTANCE: a suffix of neither documented length",
            "INSTANCE",
            Some(vpc_config("HTTP", None)),
            "i-0123456789",
        ),
        (
            "ALB: a Lambda function ARN",
            "ALB",
            Some(vpc_config("HTTPS", Some(443))),
            A_LAMBDA,
        ),
    ];

    for (index, (case, group_type, config, target)) in cases.into_iter().enumerate() {
        let (id, _) = a_target_group(&lattice, &format!("bad-form-{index}"), group_type, config)
            .await;
        let answer =
            register_targets(&lattice, &id, serde_json::json!([{ "id": target }])).await;
        assert_refused(&answer, case);
        let message = message_of(case, &answer);
        assert!(
            message.contains(target) && message.contains(group_type),
            "case {case:?}: the refusal must name the submitted id and the type, got \
             {message:?}"
        );
        assert_eq!(
            answer.body["fieldList"][0]["name"].as_str(),
            Some("id"),
            "case {case:?}: the refusal must name the member it constrains, got {}",
            answer.body
        );
        // A refused registration stores nothing.
        let listed = listed_pairs(&lattice, case, &id).await;
        assert_eq!(
            listed,
            Vec::new(),
            "case {case:?}: a refused registration must store nothing, got {listed:?}"
        );
    }
}

/// Rules 4 and 5: the maximum a target group's TYPE allows it, the targets already
/// registered surviving a refusal, and a repeated registration holding one target.
#[tokio::test]
async fn a_registration_past_the_maximum_its_type_allows_is_refused_and_keeps_what_is_held() {
    let lattice = Lattice::new();
    let lambda_maximum = state::documented_target_maximum("LAMBDA")
        .expect("the LAMBDA target-group type documents a maximum");

    // One target already registered, then a SECOND, distinct one: the refusal names the
    // target group, its type and the maximum that type allows.
    let case = "a second function on a LAMBDA target group";
    let (first_id, _) = a_target_group(&lattice, "lambda-one", "LAMBDA", None).await;
    let accepted =
        register_targets(&lattice, &first_id, serde_json::json!([{ "id": A_LAMBDA }])).await;
    assert_eq!(
        accepted.status, 200,
        "case {case:?}: the first registration must be accepted, got {} / {}",
        accepted.status, accepted.body
    );
    let refused = register_targets(
        &lattice,
        &first_id,
        serde_json::json!([{ "id": ANOTHER_LAMBDA }]),
    )
    .await;
    assert_refused(&refused, case);
    assert_eq!(
        message_of(case, &refused),
        format!(
            "Target group {first_id} with type LAMBDA can only contain a maximum of \
             {lambda_maximum} targets"
        ),
        "case {case:?}: unexpected refusal message, got {}",
        refused.body
    );
    assert_eq!(
        listed_pairs(&lattice, case, &first_id).await,
        vec![(A_LAMBDA.to_string(), None)],
        "case {case:?}: the target already registered must still be registered"
    );

    // The same refusal when ONE call carries more targets than the type allows, and
    // nothing of that call is stored.
    let case = "two functions in one registration";
    let (second_id, _) = a_target_group(&lattice, "lambda-two", "LAMBDA", None).await;
    let refused = register_targets(
        &lattice,
        &second_id,
        serde_json::json!([{ "id": A_LAMBDA }, { "id": ANOTHER_LAMBDA }]),
    )
    .await;
    assert_refused(&refused, case);
    assert_eq!(
        message_of(case, &refused),
        format!(
            "Target group {second_id} with type LAMBDA can only contain a maximum of \
             {lambda_maximum} targets"
        ),
        "case {case:?}: unexpected refusal message, got {}",
        refused.body
    );
    assert_eq!(
        listed_pairs(&lattice, case, &second_id).await,
        Vec::new(),
        "case {case:?}: a refused registration must store nothing"
    );

    // Rule 5: the SAME target registered twice is one target, and the retry is accepted.
    let case = "the same function registered twice";
    let retried =
        register_targets(&lattice, &first_id, serde_json::json!([{ "id": A_LAMBDA }])).await;
    assert_eq!(
        retried.status, 200,
        "case {case:?}: expected 200, got {} / {}",
        retried.status, retried.body
    );
    assert_eq!(
        listed_pairs(&lattice, case, &first_id).await,
        vec![(A_LAMBDA.to_string(), None)],
        "case {case:?}: registering the same target twice must hold one target"
    );

    // …while the same address on a DIFFERENT port is a different target, and a type that
    // documents no maximum holds more than one target.
    let case = "an IP target group holds several targets";
    let (ip_id, _) = a_target_group(
        &lattice,
        "ip-many",
        "IP",
        Some(vpc_config("HTTP", Some(80))),
    )
    .await;
    let accepted = register_targets(
        &lattice,
        &ip_id,
        serde_json::json!([
            { "id": "10.0.0.1" },
            { "id": "10.0.0.1", "port": 8080 },
            { "id": "10.0.0.2", "port": 8080 },
        ]),
    )
    .await;
    assert_eq!(
        accepted.status, 200,
        "case {case:?}: expected 200, got {} / {}",
        accepted.status, accepted.body
    );
    assert_eq!(
        state::documented_target_maximum("IP"),
        None,
        "case {case:?}: the IP target-group type documents no maximum"
    );
    assert_eq!(
        listed_pairs(&lattice, case, &ip_id).await,
        vec![
            ("10.0.0.1".to_string(), Some(80)),
            ("10.0.0.1".to_string(), Some(8080)),
            ("10.0.0.2".to_string(), Some(8080)),
        ],
        "case {case:?}: a target is its id together with the port it is reached on"
    );
}

/// Rule 6, over a table of the states the statuses describe: nothing routing to the
/// target group, a group that health-checks nothing, and a group whose initial health
/// checking is still due.
#[tokio::test]
async fn a_target_reports_the_status_the_stored_records_and_the_clock_give_it() {
    let lattice = Lattice::new();
    let (service_id, _) = a_service(&lattice, "target-status-host").await;

    // (case, type, config, whether a listener forwards to it, the target, the status)
    let cases: Vec<StatusCase> = vec![
        (
            "nothing routes to the target group",
            "IP",
            Some(vpc_config("HTTP", None)),
            false,
            "10.0.0.1",
            state::TARGET_STATUS_UNUSED,
        ),
        (
            "a LAMBDA target group health-checks nothing",
            "LAMBDA",
            None,
            true,
            A_LAMBDA,
            state::TARGET_STATUS_UNAVAILABLE,
        ),
        (
            "health checking turned off",
            "IP",
            Some(serde_json::json!({
                "protocol": "HTTP",
                "vpcIdentifier": A_VPC,
                "healthCheck": { "enabled": false },
            })),
            true,
            "10.0.0.2",
            state::TARGET_STATUS_UNAVAILABLE,
        ),
        (
            "initial health checking still due",
            "IP",
            Some(serde_json::json!({
                "protocol": "HTTP",
                "vpcIdentifier": A_VPC,
                // The narrowest span the members' own documentation admits (interval
                // 5–300 s, healthy threshold 2–10), so the case waits as little as the
                // documented rule allows.
                "healthCheck": {
                    "enabled": true,
                    "healthCheckIntervalSeconds": 5,
                    "healthyThresholdCount": 2,
                },
            })),
            true,
            "10.0.0.3",
            state::TARGET_STATUS_INITIAL,
        ),
    ];

    let mut healthy_later: Vec<(&str, String)> = Vec::new();
    for (index, (case, group_type, config, forwarded, target, expected)) in
        cases.into_iter().enumerate()
    {
        let (id, _) =
            a_target_group(&lattice, &format!("status-{index}"), group_type, config).await;
        if forwarded {
            forward_to(
                &lattice,
                &service_id,
                &format!("status-listener-{index}"),
                8080 + index as i64,
                &id,
            )
            .await;
        }
        let registered = register_targets(&lattice, &id, serde_json::json!([{ "id": target }]))
            .await;
        assert_eq!(
            registered.status, 200,
            "case {case:?}: expected 200, got {} / {}",
            registered.status, registered.body
        );
        let status = only_target_status(&lattice, case, &id).await;
        assert_eq!(
            status, expected,
            "case {case:?}: expected status {expected}, got {status}"
        );
        if expected == state::TARGET_STATUS_INITIAL {
            healthy_later.push((case, id));
        }
    }

    // `INITIAL` is non-terminal, so the clock owes the target a terminal status: look
    // again until it reports it, never for a fixed time.
    for (case, id) in healthy_later {
        let started = Instant::now();
        loop {
            let status = only_target_status(&lattice, case, &id).await;
            if status == state::TARGET_STATUS_HEALTHY {
                break;
            }
            assert_eq!(
                status,
                state::TARGET_STATUS_INITIAL,
                "case {case:?}: a target still health-checking must report \
                 {} until it is healthy, got {status}",
                state::TARGET_STATUS_INITIAL
            );
            assert!(
                started.elapsed() < POLL_DEADLINE,
                "case {case:?}: {id} never reported {}",
                state::TARGET_STATUS_HEALTHY
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
}

/// A list of targets longer than the modelled `@length` of the member it is sent at is
/// refused, on each member that carries one: the registration's `targets` (1..100) and the
/// listing's filter (0..20), each tested at its published bound and one past it.
#[tokio::test]
async fn a_list_of_targets_longer_than_the_model_admits_is_refused() {
    let lattice = Lattice::new();
    let (id, _) = a_target_group(
        &lattice,
        "bounded",
        "IP",
        Some(vpc_config("HTTP", Some(80))),
    )
    .await;

    // An address list of `count` distinct targets, which an IP target group admits.
    let addresses = |count: usize| -> serde_json::Value {
        serde_json::Value::Array(
            (0..count)
                .map(|index| {
                    serde_json::json!({ "id": format!("10.{}.{}.1", index / 250, index % 250) })
                })
                .collect(),
        )
    };

    // (case, how many targets the registration carries, whether the service accepts it)
    let cases: Vec<(&str, usize, bool)> = vec![
        ("at the published maximum", state::MAX_TARGETS_PER_REGISTRATION, true),
        (
            "one past the published maximum",
            state::MAX_TARGETS_PER_REGISTRATION + 1,
            false,
        ),
    ];
    for (case, count, accepted) in cases {
        let answer = register_targets(&lattice, &id, addresses(count)).await;
        if accepted {
            assert_eq!(
                answer.status, 200,
                "case {case:?}: expected 200, got {} / {}",
                answer.status, answer.body
            );
        } else {
            assert_refused(&answer, case);
        }
    }

    for (case, count, accepted) in [
        ("a filter at the published maximum", state::MAX_TARGET_FILTER, true),
        (
            "a filter one past the published maximum",
            state::MAX_TARGET_FILTER + 1,
            false,
        ),
    ] {
        let answer = list_targets(&lattice, &id, &[], Some(addresses(count))).await;
        if accepted {
            assert_eq!(
                answer.status, 200,
                "case {case:?}: expected 200, got {} / {}",
                answer.status, answer.body
            );
        } else {
            assert_refused(&answer, case);
        }
    }
}

/// Rule 7: `maxResults` bounds a page, `nextToken` pages the rest exactly once each, a
/// `targets` filter keeps the targets it names, and a token this service never issued is
/// refused.
#[tokio::test]
async fn a_listing_pages_the_targets_and_filters_by_target() {
    let lattice = Lattice::new();
    let (id, _) = a_target_group(
        &lattice,
        "paged",
        "IP",
        Some(vpc_config("HTTP", Some(80))),
    )
    .await;
    let addresses: Vec<String> = (1..=5).map(|last| format!("10.0.0.{last}")).collect();
    let registered = register_targets(
        &lattice,
        &id,
        serde_json::Value::Array(
            addresses
                .iter()
                .map(|address| serde_json::json!({ "id": address }))
                .collect(),
        ),
    )
    .await;
    assert_eq!(
        registered.status, 200,
        "case fixture registration: expected 200, got {} / {}",
        registered.status, registered.body
    );

    // (case, maxResults)
    for (case, max_results) in [("one page", None), ("two at a time", Some(2))] {
        let mut seen: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        // One page per registered target is the worst case; the bound stops a broken
        // token round-trip from looping forever instead of failing.
        for _ in 0..16 {
            let mut query: Vec<(&str, String)> = Vec::new();
            if let Some(max) = max_results {
                query.push(("maxResults", max.to_string()));
            }
            if let Some(next) = &token {
                query.push(("nextToken", next.clone()));
            }
            let page = list_targets(&lattice, &id, &query, None).await;
            assert_eq!(
                page.status, 200,
                "case {case:?}: expected 200, got {} / {}",
                page.status, page.body
            );
            let items = page.body["items"].as_array().cloned().unwrap_or_default();
            if let Some(max) = max_results {
                assert!(
                    items.len() <= max,
                    "case {case:?}: a page carried {} items, past maxResults={max}",
                    items.len()
                );
            }
            for item in &items {
                seen.push(
                    item["id"]
                        .as_str()
                        .unwrap_or_else(|| {
                            panic!("case {case:?}: a TargetSummary without an id: {item}")
                        })
                        .to_string(),
                );
            }
            token = page.body["nextToken"].as_str().map(str::to_string);
            if token.is_none() {
                break;
            }
        }
        assert!(
            token.is_none(),
            "case {case:?}: the listing never stopped handing back a nextToken"
        );
        let mut sorted = seen.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(
            sorted, addresses,
            "case {case:?}: every registered target exactly once, got {seen:?}"
        );
    }

    // (case, the filter, the ids it must keep)
    let filters: Vec<(&str, serde_json::Value, Vec<&str>)> = vec![
        (
            "by id alone",
            serde_json::json!([{ "id": "10.0.0.2" }]),
            vec!["10.0.0.2"],
        ),
        (
            "by id and the port the target is reached on",
            serde_json::json!([{ "id": "10.0.0.4", "port": 80 }, { "id": "10.0.0.5" }]),
            vec!["10.0.0.4", "10.0.0.5"],
        ),
        (
            "by a port no target is reached on",
            serde_json::json!([{ "id": "10.0.0.4", "port": 8080 }]),
            vec![],
        ),
        (
            "by an id nothing registered",
            serde_json::json!([{ "id": "10.9.9.9" }]),
            vec![],
        ),
    ];
    for (case, filter, expected) in filters {
        let listed = list_targets(&lattice, &id, &[], Some(filter)).await;
        assert_eq!(
            listed.status, 200,
            "case {case:?}: expected 200, got {} / {}",
            listed.status, listed.body
        );
        let kept: Vec<String> = pairs_of(case, &listed, "items")
            .into_iter()
            .map(|(target, _)| target)
            .collect();
        let expected: Vec<String> = expected.into_iter().map(str::to_string).collect();
        assert_eq!(
            kept, expected,
            "case {case:?}: the filter must keep the targets it names, got {kept:?}"
        );
    }

    let case = "a token this service never issued";
    let refused = list_targets(
        &lattice,
        &id,
        &[("nextToken", "not-a-token".to_string())],
        None,
    )
    .await;
    assert_refused(&refused, case);
}

/// Rule 8: an identifier naming no persisted target group, and one naming a target group
/// whose asynchronous deletion is already under way.
#[tokio::test]
async fn an_absent_or_deleting_target_group_refuses_the_registration() {
    let lattice = Lattice::new();
    let label = state::ResourceKind::TargetGroup.message_name();

    // (case, the identifier as the request sends it, the identifier the refusal reports)
    let absent = absent_id(state::PREFIX_TARGET_GROUP, "9");
    let absent_arn = state::arn_for("us-west-2", state::KIND_TARGET_GROUP, &absent);
    for (case, sent, reported) in [
        ("an id nothing created", absent.clone(), absent.clone()),
        (
            // An ARN is sent as an `@httpLabel`, so its `/` travels as `%2F`; the refusal
            // names the identifier the service decoded from it.
            "an ARN nothing created",
            as_label(&absent_arn),
            absent_arn.clone(),
        ),
    ] {
        let registered =
            register_targets(&lattice, &sent, serde_json::json!([{ "id": A_LAMBDA }])).await;
        assert_not_found(&registered, case, label, &reported);
        let listed = list_targets(&lattice, &sent, &[], None).await;
        assert_not_found(&listed, case, label, &reported);
    }

    let case = "a target group whose deletion is under way";
    let (id, _) = a_target_group(&lattice, "deleting", "LAMBDA", None).await;
    // A delete is refused while the creation span is still running, so wait for the
    // target group to be ACTIVE before deleting it.
    let started = Instant::now();
    loop {
        let read = get_target_group(&lattice, &id).await;
        if read.body["status"].as_str() == Some("ACTIVE") {
            break;
        }
        assert!(
            started.elapsed() < POLL_DEADLINE,
            "case {case:?}: {id} never reached ACTIVE; last answer {} / {}",
            read.status,
            read.body
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    let deleted = lattice
        .call("DELETE", &format!("/targetgroups/{id}"), None)
        .await;
    assert_eq!(
        deleted.status, 200,
        "case {case:?}: expected 200 from the delete, got {} / {}",
        deleted.status, deleted.body
    );
    let refused =
        register_targets(&lattice, &id, serde_json::json!([{ "id": A_LAMBDA }])).await;
    assert_eq!(
        refused.status, 409,
        "case {case:?}: expected 409, got {} / {}",
        refused.status, refused.body
    );
    assert_eq!(
        refused.error_code.as_deref(),
        Some("ConflictException"),
        "case {case:?}: expected the ConflictException wire code, got {:?} / {}",
        refused.error_code,
        refused.body
    );
    assert!(
        message_of(case, &refused).contains(&id),
        "case {case:?}: the refusal must name the target group, got {}",
        refused.body
    );
}
