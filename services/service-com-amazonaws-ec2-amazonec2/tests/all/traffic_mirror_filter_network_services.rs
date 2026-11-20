// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `ModifyTrafficMirrorFilterNetworkServices`, driven over the wire the
//! service actually speaks — `ec2Query`, through the crate's shared [`super::ec2_query`]
//! harness — and read back through `DescribeTrafficMirrorFilters`, so what the modify reports
//! is what the store holds.
//!
//! The rules under test are:
//!
//! 1. the call ALLOWS and RESTRICTS a filter's network services as a SET: `AddNetworkServices`
//!    inserts, `RemoveNetworkServices` deletes, naming a service twice or naming one the
//!    filter already mirrors leaves one entry, removing one the filter does not mirror changes
//!    nothing, a request that names neither list leaves the set as it stands, and the set the
//!    modify reports is the set the describe reports;
//! 2. the call touches NOTHING ELSE about the filter: its description, its tags and its rule
//!    sets read back unchanged;
//! 3. a filter id this account holds no filter for is refused with
//!    `InvalidTrafficMirrorFilterId.NotFound`, whatever the id looks like, and a request that
//!    carries no filter id at all is EC2's `MissingParameter`;
//! 4. a network service outside the modelled `TrafficMirrorNetworkService` enum is refused
//!    with EC2's `InvalidParameterValue` naming the value it could not read, on either list —
//!    and writes nothing;
//! 5. `DryRun` answers the verdict the request WOULD have earned — `DryRunOperation` for one
//!    that would have worked, the request's own refusal for one that would not — and writes
//!    nothing either way.
//!
//! Each table row is a distinct input along what the rule generalises over: how many values
//! one list carries, which list carries them, how many calls the sequence takes, which shape
//! of unknown id, which spelling outside the enum. The recorded unit this suite was filed from
//! is ONE row of the table in [`every_modification_round_trips_through_the_describe`].

use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, error_code, error_message, indexed};

/// The Region every call here is signed for. A Traffic Mirror filter is a Region-scoped
/// resource whose modelled shape names no Region, so the Region only selects the envelope.
const REGION: &str = "us-west-2";

/// The network services a Traffic Mirror filter can be asked to mirror, as the modelled
/// `TrafficMirrorNetworkService` enum declares them — the crate's published catalog
/// (`smithy_query(query="shape",shape="TrafficMirrorNetworkService")`), so the table below
/// covers every member the model carries rather than one word.
const NETWORK_SERVICES: &[&str] = ec2::ops::types::TRAFFIC_MIRROR_NETWORK_SERVICE_VALUES;

/// One `(key, value)` request parameter.
fn p(key: &str, value: impl Into<String>) -> (String, String) {
    (key.to_string(), value.into())
}

fn emulator() -> MultiServiceRouter {
    super::ec2_query::emulator().0
}

/// One `ModifyTrafficMirrorFilterNetworkServices` call.
async fn modify(router: &MultiServiceRouter, params: &[(String, String)]) -> (u16, String) {
    call(
        router,
        REGION,
        "ModifyTrafficMirrorFilterNetworkServices",
        params,
    )
    .await
}

/// The form fields of one modify request: the filter, the services to add and the ones to
/// remove, as ec2Query flattens each list into indexed keys.
fn modify_params(
    traffic_mirror_filter_id: &str,
    add: &[&str],
    remove: &[&str],
) -> Vec<(String, String)> {
    [
        vec![p("TrafficMirrorFilterId", traffic_mirror_filter_id)],
        indexed("AddNetworkService", add),
        indexed("RemoveNetworkService", remove),
    ]
    .concat()
}

/// Create one Traffic Mirror filter with `params` and answer the id it was minted with.
async fn created_id(
    router: &MultiServiceRouter,
    case: &str,
    params: &[(String, String)],
) -> String {
    let (status, body) = call(router, REGION, "CreateTrafficMirrorFilter", params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateTrafficMirrorFilter, got {status} \
         with body {body}"
    );
    element(&body, "trafficMirrorFilterId").unwrap_or_else(|| {
        panic!("case {case:?}: the create must report the filter's id, got {body}")
    })
}

/// The network services the one filter reported in `body` carries, in the order it reported
/// them: the `networkServiceSet` of the element that has a `trafficMirrorFilterId` of its
/// own, read through the parser so a member is never taken out of a nested shape.
fn reported_network_services(case: &str, body: &str) -> Vec<String> {
    let document = super::wire::document(case, body);
    let filter = document
        .descendants()
        .find(|node| {
            node.is_element() && super::wire::child(*node, "trafficMirrorFilterId").is_some()
        })
        .unwrap_or_else(|| panic!("case {case:?}: no reported Traffic Mirror filter in {body}"));
    super::wire::set_items(filter, "networkServiceSet")
        .into_iter()
        .map(|item| item.text().unwrap_or_default().to_string())
        .collect()
}

/// The network services `DescribeTrafficMirrorFilters` reports for the one filter
/// `traffic_mirror_filter_id` names.
async fn described_network_services(
    router: &MultiServiceRouter,
    case: &str,
    traffic_mirror_filter_id: &str,
) -> Vec<String> {
    let (status, body) = call(
        router,
        REGION,
        "DescribeTrafficMirrorFilters",
        &indexed("TrafficMirrorFilterId", &[traffic_mirror_filter_id]),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeTrafficMirrorFilters must report {traffic_mirror_filter_id}, \
         got {status} with body {body}"
    );
    reported_network_services(case, &body)
}

/// One step of a sequence: what the request adds, what it removes, and the set the filter
/// must mirror once that request has been answered.
type Step<'a> = (&'a [&'a str], &'a [&'a str], &'a [&'a str]);

/// RULE 1: a filter's network services are a SET the call inserts into and deletes from, and
/// the set the modify reports is the set the describe reports.
#[tokio::test]
async fn every_modification_round_trips_through_the_describe() {
    // One member is all the modelled enum carries, so the rows generalise over the SHAPE of
    // the request instead: how many values one list carries, which list carries them, whether
    // the same service is named on both lists at once, and how many calls the sequence takes.
    let one = NETWORK_SERVICES;
    let twice: Vec<&str> = NETWORK_SERVICES
        .iter()
        .chain(NETWORK_SERVICES.iter())
        .copied()
        .collect();
    let none: &[&str] = &[];
    let rows: Vec<(&str, Vec<Step<'_>>)> = vec![
        ("one add names the service", vec![(one, none, one)]),
        (
            "the same service twice in one add is one entry",
            vec![(&twice, none, one)],
        ),
        (
            "a second add of the same service changes nothing",
            vec![(one, none, one), (one, none, one)],
        ),
        (
            "a remove of a service the filter does not mirror changes nothing",
            vec![(none, one, none)],
        ),
        (
            "an add then a remove leaves the filter mirroring none",
            vec![(one, none, one), (none, one, none)],
        ),
        (
            "a remove and an add in one request leave the service mirrored",
            vec![(one, one, one)],
        ),
        (
            "a request naming neither list leaves the set as it stands",
            vec![(one, none, one), (none, none, one)],
        ),
        (
            "a request naming neither list on a filter mirroring none leaves it none",
            vec![(none, none, none)],
        ),
        (
            "removing twice after one add leaves the filter mirroring none",
            vec![(one, none, one), (&twice, none, one), (none, &twice, none)],
        ),
    ];

    for (case, steps) in rows {
        let router = emulator();
        let filter_id = created_id(&router, case, &[p("Description", case)]).await;
        // A freshly created filter mirrors no network service at all ("By default, Amazon DNS
        // network services are not eligible for Traffic Mirror").
        assert_eq!(
            described_network_services(&router, case, &filter_id).await,
            Vec::<String>::new(),
            "case {case:?}: a fresh filter must mirror no network service"
        );

        for (index, (add, remove, expected)) in steps.iter().enumerate() {
            let case = format!("{case}, call {}", index + 1);
            let (status, body) = modify(&router, &modify_params(&filter_id, add, remove)).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200, got {status} with body {body}"
            );
            let expected: Vec<String> = expected.iter().map(|value| (*value).to_string()).collect();
            let reported = reported_network_services(&case, &body);
            assert_eq!(
                reported, expected,
                "case {case:?}: expected the filter to mirror {expected:?}, got {reported:?} \
                 in {body}"
            );
            let described = described_network_services(&router, &case, &filter_id).await;
            assert_eq!(
                described, expected,
                "case {case:?}: the describe must report what the modify reported \
                 ({expected:?}), got {described:?}"
            );
            // The call names the filter it modified, so a caller can tell which record it
            // changed.
            assert_eq!(
                element(&body, "trafficMirrorFilterId"),
                Some(filter_id.clone()),
                "case {case:?}: expected the modify to report {filter_id}, got {body}"
            );
        }
    }
}

/// RULE 2: the call changes the network services and nothing else — the description, the tags
/// and the rule sets of the filter read back exactly as they were.
#[tokio::test]
async fn the_modification_leaves_the_rest_of_the_filter_alone() {
    let router = emulator();
    let case = "a filter with a description, tags and a rule";
    let filter_id = created_id(
        &router,
        case,
        &[
            vec![
                p("Description", "mirror the DNS traffic"),
                p(
                    "TagSpecification.1.ResourceType",
                    ec2::state::TRAFFIC_MIRROR_FILTER_TAGGABLE_RESOURCE,
                ),
                p("TagSpecification.1.Tag.1.Key", "Team"),
                p("TagSpecification.1.Tag.1.Value", "platform"),
            ],
        ]
        .concat(),
    )
    .await;

    let (status, body) = call(
        &router,
        REGION,
        "CreateTrafficMirrorFilterRule",
        &[
            p("TrafficMirrorFilterId", filter_id.clone()),
            p("TrafficDirection", "ingress"),
            p("RuleNumber", "100"),
            p("RuleAction", "accept"),
            p("DestinationCidrBlock", "10.0.0.0/16"),
            p("SourceCidrBlock", "0.0.0.0/0"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the rule create must succeed, got {status} with body {body}"
    );
    let rule_id = element(&body, "trafficMirrorFilterRuleId").unwrap_or_default();

    /// Every member of the one reported filter this rule holds fixed.
    fn kept(case: &str, body: &str) -> (Option<String>, Vec<(String, String)>, Vec<String>) {
        let document = super::wire::document(case, body);
        let filter = document
            .descendants()
            .find(|node| {
                node.is_element() && super::wire::child(*node, "trafficMirrorFilterId").is_some()
            })
            .unwrap_or_else(|| panic!("case {case:?}: no reported filter in {body}"));
        let tags = super::wire::set_items(filter, "tagSet")
            .into_iter()
            .map(|item| {
                (
                    super::wire::child_text(item, "key").unwrap_or_default(),
                    super::wire::child_text(item, "value").unwrap_or_default(),
                )
            })
            .collect();
        let rules = super::wire::set_items(filter, "ingressFilterRuleSet")
            .into_iter()
            .map(|item| {
                super::wire::child_text(item, "trafficMirrorFilterRuleId").unwrap_or_default()
            })
            .collect();
        (super::wire::child_text(filter, "description"), tags, rules)
    }

    let (status, before) = call(
        &router,
        REGION,
        "DescribeTrafficMirrorFilters",
        &indexed("TrafficMirrorFilterId", &[&filter_id]),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the describe must succeed, got {status} with body {before}"
    );
    let expected = (
        Some("mirror the DNS traffic".to_string()),
        vec![("Team".to_string(), "platform".to_string())],
        vec![rule_id.clone()],
    );
    assert_eq!(
        kept(case, &before),
        expected,
        "case {case:?}: the filter must be stored as created, got {before}"
    );

    // Both directions of the change, so neither an add nor a remove can be the one that
    // disturbs another member.
    for (step, add, remove) in [
        ("after the add", NETWORK_SERVICES, &[] as &[&str]),
        ("after the remove", &[], NETWORK_SERVICES),
    ] {
        let (status, body) = modify(&router, &modify_params(&filter_id, add, remove)).await;
        assert_eq!(
            status, 200,
            "case {step:?}: expected HTTP 200, got {status} with body {body}"
        );
        assert_eq!(
            kept(step, &body),
            expected,
            "case {step:?}: the modify must report every other member unchanged, got {body}"
        );
        let (status, described) = call(
            &router,
            REGION,
            "DescribeTrafficMirrorFilters",
            &indexed("TrafficMirrorFilterId", &[&filter_id]),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {step:?}: the describe must succeed, got {status} with body {described}"
        );
        assert_eq!(
            kept(step, &described),
            expected,
            "case {step:?}: the store must hold every other member unchanged, got {described}"
        );
    }
}

/// RULE 3: a filter id the account holds no filter for is refused whatever shape it has, and a
/// request carrying no filter id at all is the missing parameter it is.
#[tokio::test]
async fn an_unknown_or_absent_filter_id_is_refused() {
    let router = emulator();
    let held = created_id(&router, "held", &[]).await;
    let (status, body) = modify(&router, &modify_params(&held, NETWORK_SERVICES, &[])).await;
    assert_eq!(
        status, 200,
        "the held filter must accept the add, got {status} with body {body}"
    );

    // The rows differ along the SHAPE of the unknown id: the service's own id form at a value
    // nothing was minted at, both EC2 id lengths, another resource's prefix, and strings that
    // are no id at all.
    for unknown in [
        "tmf-00000000",
        "tmf-0123456789abcdef0",
        "tmf-ZZZZZZZZ",
        "tmf-",
        "vpc-0123456789abcdef0",
        "not-an-id",
        "  ",
    ] {
        let (status, body) = modify(&router, &modify_params(unknown, NETWORK_SERVICES, &[])).await;
        assert_eq!(
            status, 400,
            "case {unknown:?}: expected HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            "InvalidTrafficMirrorFilterId.NotFound",
            "case {unknown:?}: expected InvalidTrafficMirrorFilterId.NotFound, got {body}"
        );
        let message = error_message(&body);
        // The one sentence this crate words an unresolvable Traffic Mirror filter id with,
        // naming the id the REQUEST gave (`state::invalid_traffic_mirror_filter_id_not_found`).
        let expected = format!("{unknown} does not exist");
        assert_eq!(
            message, expected,
            "case {unknown:?}: expected the message {expected:?}, got {message:?}"
        );
    }

    // No filter id at all: `@required` but modelled `clientOptional`, so an omitted member is
    // refused at the boundary and an empty one by the handler — both in the one sentence this
    // crate words an absent required member with, so the two sides of dispatch agree.
    let absent = ec2::state::missing_required_parameter_message("TrafficMirrorFilterId");
    for (case, params) in [
        ("no filter id at all", Vec::new()),
        ("an empty filter id", vec![p("TrafficMirrorFilterId", "")]),
    ] {
        let params = [params, indexed("AddNetworkService", NETWORK_SERVICES)].concat();
        let (status, body) = modify(&router, &params).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            "MissingParameter",
            "case {case:?}: expected MissingParameter, got {body}"
        );
        let message = error_message(&body);
        assert_eq!(
            message, absent,
            "case {case:?}: expected the message {absent:?}, got {message:?}"
        );
    }

    // Nothing a refused request named was written, and the held filter kept what it had.
    let expected: Vec<String> = NETWORK_SERVICES
        .iter()
        .map(|value| (*value).to_string())
        .collect();
    let described = described_network_services(&router, "after the refusals", &held).await;
    assert_eq!(
        described, expected,
        "a refused request must leave the held filter mirroring {expected:?}, got {described:?}"
    );
}

/// RULE 4: a network service outside the modelled enum is refused with EC2's
/// `InvalidParameterValue` on either list, naming the value it could not read, and the refusal
/// happens before anything is written.
#[tokio::test]
async fn a_network_service_outside_the_enum_is_refused() {
    let router = emulator();
    let filter_id = created_id(&router, "enum", &[]).await;

    // The rows differ along HOW the spelling falls outside the enum: the wrong case, the
    // wrong separator, a prefix and a suffix of a member, another word entirely, and a value
    // the request carried empty. Each is asserted on both lists, so neither can be read more
    // loosely than the other.
    let rejected = [
        "Amazon-DNS",
        "AMAZON-DNS",
        "amazon_dns",
        "amazon-dns ",
        "amazon",
        "amazon-dnsx",
        "dns",
        "",
    ];
    for spelling in rejected {
        // Both lists carry the same parameter type, so each is refused in the same words.
        for list in ["AddNetworkService", "RemoveNetworkService"] {
            let case = format!("{spelling:?} on {list}");
            assert!(
                !NETWORK_SERVICES.contains(&spelling),
                "case {case:?}: the row must be outside the enum {NETWORK_SERVICES:?}"
            );
            let params = [
                vec![p("TrafficMirrorFilterId", filter_id.clone())],
                indexed(list, &[spelling]),
            ]
            .concat();
            let (status, body) = modify(&router, &params).await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected HTTP 400, got {status} with body {body}"
            );
            assert_eq!(
                error_code(&body),
                "InvalidParameterValue",
                "case {case:?}: expected InvalidParameterValue, got {body}"
            );
            let message = error_message(&body);
            let expected = format!("'{spelling}' is not a valid network service");
            assert_eq!(
                message, expected,
                "case {case:?}: expected the message {expected:?}, got {message:?}"
            );
        }
    }

    // A member of the enum is accepted on the same lists, so the refusals above are about the
    // spelling and not about the request's form.
    let (status, body) = modify(
        &router,
        &modify_params(&filter_id, NETWORK_SERVICES, &[]),
    )
    .await;
    assert_eq!(
        status, 200,
        "a member of the enum must be accepted, got {status} with body {body}"
    );

    // A refused request wrote nothing: the only set the filter holds is the one the accepted
    // call left behind.
    let expected: Vec<String> = NETWORK_SERVICES
        .iter()
        .map(|value| (*value).to_string())
        .collect();
    let described = described_network_services(&router, "after the refusals", &filter_id).await;
    assert_eq!(
        described, expected,
        "a refused request must write nothing, got {described:?}"
    );
}

/// One row of the dry-run table: the case, the request under test, and the verdict `DryRun`
/// must answer it with — `None` for a request that would have succeeded.
type DryRunRow = (&'static str, Vec<(String, String)>, Option<(u16, &'static str)>);

/// RULE 5: `DryRun` answers the verdict the request would have earned and writes nothing —
/// `DryRunOperation` (HTTP 412) for a request that would have worked, and the request's own
/// refusal for one that would not.
#[tokio::test]
async fn a_dry_run_answers_the_verdict_and_writes_nothing() {
    let router = emulator();
    let filter_id = created_id(&router, "dry run", &[]).await;

    // Rows: the request under test, and the verdict `DryRun` must answer it with — `None` for
    // one that would have succeeded.
    let rows: Vec<DryRunRow> = vec![
        (
            "an add that would have worked",
            modify_params(&filter_id, NETWORK_SERVICES, &[]),
            None,
        ),
        (
            "a remove that would have worked",
            modify_params(&filter_id, &[], NETWORK_SERVICES),
            None,
        ),
        (
            "a request naming neither list",
            modify_params(&filter_id, &[], &[]),
            None,
        ),
        (
            "an add naming a filter the account does not hold",
            modify_params("tmf-00000000", NETWORK_SERVICES, &[]),
            Some((400, "InvalidTrafficMirrorFilterId.NotFound")),
        ),
        (
            "an add naming no filter at all",
            indexed("AddNetworkService", NETWORK_SERVICES),
            Some((400, "MissingParameter")),
        ),
        (
            "an add outside the enum",
            [
                vec![p("TrafficMirrorFilterId", filter_id.clone())],
                indexed("AddNetworkService", &["amazon-dnsx"]),
            ]
            .concat(),
            Some((400, "InvalidParameterValue")),
        ),
    ];

    for (case, params, refusal) in rows {
        let params = [params, vec![p("DryRun", "true")]].concat();
        let (status, body) = modify(&router, &params).await;
        let (expected_status, expected_code) = refusal.unwrap_or((412, "DryRunOperation"));
        assert_eq!(
            status, expected_status,
            "case {case:?}: expected HTTP {expected_status}, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            expected_code,
            "case {case:?}: expected {expected_code}, got {body}"
        );
        let described = described_network_services(&router, case, &filter_id).await;
        assert_eq!(
            described,
            Vec::<String>::new(),
            "case {case:?}: a dry run must write nothing, got {described:?}"
        );
    }

    // The same add without the flag does write, so the dry runs above were refused for the
    // flag rather than for the request.
    let (status, body) = modify(&router, &modify_params(&filter_id, NETWORK_SERVICES, &[])).await;
    assert_eq!(
        status, 200,
        "the same add without DryRun must succeed, got {status} with body {body}"
    );
    let expected: Vec<String> = NETWORK_SERVICES
        .iter()
        .map(|value| (*value).to_string())
        .collect();
    let described = described_network_services(&router, "after the dry runs", &filter_id).await;
    assert_eq!(
        described, expected,
        "the add without DryRun must store {expected:?}, got {described:?}"
    );
}
