// SPDX-License-Identifier: CC0-1.0

//! `DeleteTrafficMirrorFilter`, driven over the wire the service actually speaks (`ec2Query`,
//! through the crate's shared [`super::ec2_query`] harness).
//!
//! The rule under test: a delete named by a resource id reads the REQUEST first, then resolves
//! that id against the store, refuses while another stored resource still depends on the
//! resource, answers a `DryRun` probe with the outcome the real call would have had without
//! writing anything, and — when it does succeed — takes the record OUT of the store, so the id
//! stops resolving for every later operation that names one and the client tokens the removed
//! record and the children living on it held name nothing afterwards. Five consequences, one
//! table each:
//!
//! 1. `TrafficMirrorFilterId` is `@required` but modelled `clientOptional`
//!    (`smithy_query(query="operation",operation="DeleteTrafficMirrorFilter")`), so a request
//!    that carries no value for it is the missing parameter it is — answered in the sentence
//!    the validator that resolved the absence words — and nothing of the account changes;
//! 2. an id this account holds no filter for is refused `InvalidTrafficMirrorFilterId.NotFound`
//!    whatever the id looks like, under a `DryRun` probe as well as without one, because the
//!    probe reports what the real call would have answered;
//! 3. a probe naming a filter the delete WOULD have removed is answered 412 `DryRunOperation`
//!    and the filter is reported exactly as before, however much it carries, so the delete
//!    without the flag is what removes it;
//! 4. "You cannot delete a Traffic Mirror filter that is in use by a Traffic Mirror session"
//!    (`DeleteTrafficMirrorFilter`): that refusal wins over the probe too, and the very same
//!    delete succeeds once nothing mirrors by the filter;
//! 5. the id of a deleted filter stops resolving for EVERY operation that names a filter — the
//!    describe, a second delete, the rule create and the network-services modify — while the
//!    filters the account still holds serve each of those calls, and the tokens the filter and
//!    its rules were created under mint fresh resources rather than reporting removed ones.
//!
//! Each row is one input along what the rule generalises over — which parameter is at fault,
//! what shape the unresolvable id has, what the deleted filter carried (a description, tags,
//! rules in either direction, a mirrored network service), how many resources depend on it, and
//! which later operation names the removed id — so no single row is the specification. The
//! recorded units this operation was filed from are the plain delete of a filter nothing
//! depends on, which is ONE row of the table in
//! [`a_deleted_filter_stops_resolving_for_every_operation_that_names_one`].

use ec2::ops::types::{Tag, TrafficMirrorSession};
use ec2::state::{
    ComAmazonawsEc2Amazonec2State as Ec2State, DEPENDENCY_VIOLATION, DRY_RUN_OPERATION,
    DRY_RUN_OPERATION_MESSAGE, DRY_RUN_OPERATION_STATUS,
    INVALID_TRAFFIC_MIRROR_FILTER_ID_NOT_FOUND, MISSING_PARAMETER, TRAFFIC_DIRECTION_EGRESS,
    TRAFFIC_DIRECTION_INGRESS, TRAFFIC_MIRROR_FILTER_TAGGABLE_RESOURCE,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{element, error_code, error_message, indexed};
use super::wire::{param, Param};

/// The Region every call here is signed for. A Traffic Mirror filter's modelled shape names no
/// Region, so the Region only selects the envelope.
const REGION: &str = "us-west-2";

/// The network services a filter can be asked to mirror, as the model declares them
/// (`crate::ops::types::TRAFFIC_MIRROR_NETWORK_SERVICE_VALUES`).
const NETWORK_SERVICES: &[&str] = ec2::ops::types::TRAFFIC_MIRROR_NETWORK_SERVICE_VALUES;

/// The rule actions a filter rule can take, as the model declares them
/// (`crate::ops::types::TRAFFIC_MIRROR_RULE_ACTION_VALUES`).
const RULE_ACTIONS: &[&str] = ec2::ops::types::TRAFFIC_MIRROR_RULE_ACTION_VALUES;

/// The sentence a request whose `TrafficMirrorFilterId` arrived EMPTY is refused with: the
/// member is modelled `clientOptional`, so a value the request did carry — an empty one —
/// reaches the handler, which names the parameter as the ec2Query wire spells it. Spelled here
/// independently of the service, so the suite pins the WIRE text (the sibling suite
/// `absent_required_parameter_wording` records both of EC2's `MissingParameter` sentences).
const EMPTY_FILTER_ID_MESSAGE: &str = "The request must contain the parameter trafficMirrorFilterId";

/// The sentence a request that carried NO `TrafficMirrorFilterId` field at all is refused with:
/// the boundary resolves the `@required` member as absent and this surface's own input validation
/// names the member as the model declares it.
const ABSENT_FILTER_ID_MESSAGE: &str = "The request must include the TrafficMirrorFilterId \
     parameter. Add the required parameter and retry the request.";

/// One rule to put on a filter before it is deleted: `(the direction it sits in, its number)`.
type RuleSpec = (&'static str, i32);

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

/// One element of a response body, or a panic naming the case.
fn reported(case: &str, body: &str, name: &str) -> String {
    element(body, name).unwrap_or_else(|| panic!("case {case:?}: no <{name}> in {body}"))
}

/// Pin that `action` refused `params` with HTTP `status`, `code` and `message`.
async fn refused(
    case: &str,
    router: &MultiServiceRouter,
    action: &str,
    params: &[Param],
    status: u16,
    code: &str,
    message: &str,
) {
    let (answered, body) = call(router, action, params).await;
    assert_eq!(
        answered, status,
        "case {case:?}: expected HTTP {status} from {action}, got {answered} with body {body}"
    );
    let answered_code = error_code(&body);
    assert_eq!(
        answered_code, code,
        "case {case:?}: expected the code {code} from {action}, got {answered_code:?} in {body}"
    );
    let answered_message = error_message(&body);
    assert_eq!(
        answered_message, message,
        "case {case:?}: expected the message {message:?} from {action}, got \
         {answered_message:?}"
    );
}

/// Create one Traffic Mirror filter from `params` and answer the id it was minted with.
async fn created_filter(case: &str, router: &MultiServiceRouter, params: &[Param]) -> String {
    let body = ok(case, router, "CreateTrafficMirrorFilter", params).await;
    reported(case, &body, "trafficMirrorFilterId")
}

/// The delete's own form fields for the filter `id`.
fn delete_params(id: &str) -> Vec<Param> {
    vec![param("TrafficMirrorFilterId", id)]
}

/// The form fields of one `CreateTrafficMirrorFilterRule` on the filter `id`: the members that
/// create declares required, with the direction and the number the caller picks.
fn rule_params(id: &str, traffic_direction: &str, rule_number: i32) -> Vec<Param> {
    vec![
        param("TrafficMirrorFilterId", id),
        param("TrafficDirection", traffic_direction),
        param("RuleNumber", &rule_number.to_string()),
        param("RuleAction", RULE_ACTIONS[0]),
        param("DestinationCidrBlock", "10.0.0.0/24"),
        param("SourceCidrBlock", "10.0.1.0/24"),
    ]
}

/// The form fields of one `ModifyTrafficMirrorFilterNetworkServices` that asks the filter `id`
/// to mirror every network service the model declares.
fn network_services_params(id: &str) -> Vec<Param> {
    [
        delete_params(id),
        indexed("AddNetworkService", NETWORK_SERVICES),
    ]
    .concat()
}

/// The `trafficMirrorFilterSet` `DescribeTrafficMirrorFilters` reports for `params` — the
/// reported filters without the envelope's own per-call members, so two readings of an
/// unchanged store compare equal.
async fn described_set(case: &str, router: &MultiServiceRouter, params: &[Param]) -> String {
    let body = ok(case, router, "DescribeTrafficMirrorFilters", params).await;
    reported(case, &body, "trafficMirrorFilterSet")
}

/// Seed one Traffic Mirror session mirroring by `traffic_mirror_filter_id` and answer its id.
///
/// `CreateTrafficMirrorSession` is still a scaffold stub with an issue of its own, so the
/// session is written through the store's own write path — the one that create will use.
fn seed_session(
    case: &str,
    engine: &engine::InMemoryEngine,
    traffic_mirror_filter_id: &str,
    index: usize,
) -> String {
    let session = TrafficMirrorSession {
        traffic_mirror_session_id: None,
        traffic_mirror_target_id: Some(format!("tmt-{index:017x}")),
        traffic_mirror_filter_id: Some(traffic_mirror_filter_id.to_string()),
        network_interface_id: Some(format!("eni-{index:017x}")),
        owner_id: Some(::engine::emulator_account_id().to_string()),
        packet_length: None,
        session_number: Some(1 + i32::try_from(index).unwrap_or_default()),
        virtual_network_id: None,
        description: None,
        tags: Some(vec![Tag {
            key: Some("Suite".to_string()),
            value: Some("delete-traffic-mirror-filter".to_string()),
        }]),
    };
    engine
        .state::<Ec2State>()
        .lock()
        .insert_traffic_mirror_session(session)
        .unwrap_or_else(|| panic!("case {case:?}: the store must have room for a seeded session"))
}

/// RULE 1: a request that carries no value for the one parameter this delete requires is
/// refused `MissingParameter`, in the sentence the validator that resolved the absence words,
/// and nothing of the account changes.
///
/// The rows differ along HOW the value is missing — left out entirely, sent empty, and sent
/// empty beside either setting of the probe — and so along which of EC2's two
/// `MissingParameter` sentences answers: a field the request never sent is resolved at the
/// boundary, an empty one is a value the handler reads.
#[tokio::test]
async fn the_filter_id_the_delete_requires_is_refused_when_the_request_carries_none() {
    let rows: Vec<(&str, Vec<Param>, &str)> = vec![
        ("no filter id at all", Vec::new(), ABSENT_FILTER_ID_MESSAGE),
        (
            "an empty filter id",
            vec![param("TrafficMirrorFilterId", "")],
            EMPTY_FILTER_ID_MESSAGE,
        ),
        (
            "an empty filter id under a dry run",
            vec![param("TrafficMirrorFilterId", ""), param("DryRun", "true")],
            EMPTY_FILTER_ID_MESSAGE,
        ),
        (
            "an empty filter id with the probe off",
            vec![param("TrafficMirrorFilterId", ""), param("DryRun", "false")],
            EMPTY_FILTER_ID_MESSAGE,
        ),
    ];

    for (case, params, message) in rows {
        let (router, _engine) = super::ec2_query::emulator();
        let filter_id = created_filter(case, &router, &[param("Description", "kept")]).await;
        let before = described_set(case, &router, &[]).await;

        refused(
            case,
            &router,
            "DeleteTrafficMirrorFilter",
            &params,
            400,
            MISSING_PARAMETER,
            message,
        )
        .await;

        let after = described_set(case, &router, &[]).await;
        assert_eq!(
            after, before,
            "case {case:?}: a refused delete must leave the account's filters as they were, \
             expected {before}, got {after}"
        );
        assert!(
            after.contains(&filter_id),
            "case {case:?}: {filter_id} must still be reported, got {after}"
        );
    }
}

/// RULE 2: an id this account holds no Traffic Mirror filter for is refused
/// `InvalidTrafficMirrorFilterId.NotFound` naming the id the request gave, whatever the id looks
/// like — and under a `DryRun` probe as well, because the probe answers what the real call
/// would have answered.
///
/// The rows differ along what shape the unresolvable id has: both EC2 id lengths of this
/// resource's own prefix, another Traffic Mirror resource's prefix, this resource's prefix in
/// upper case, and a string that reads like no id at all. `TrafficMirrorFilterId` publishes no
/// pattern (`smithy_query(query="shape",shape="TrafficMirrorFilterId")`), so one code covers
/// them all.
#[tokio::test]
async fn an_id_the_account_holds_no_filter_for_is_refused_under_a_probe_and_without_one() {
    let unresolvable = [
        ("the long EC2 id form", "tmf-0123456789abcdef0"),
        ("the short EC2 id form", "tmf-0a1b2c3d"),
        ("a Traffic Mirror target's id", "tmt-0123456789abcdef0"),
        ("a Traffic Mirror filter rule's id", "tmfr-0123456789abcdef0"),
        ("this prefix in upper case", "TMF-0123456789ABCDEF0"),
        ("a string that is no id at all", "the-web-tier-filter"),
    ];

    for (shape, id) in unresolvable {
        for probe in [None, Some("true"), Some("false")] {
            let case = match probe {
                Some(flag) => format!("{shape} with DryRun={flag}"),
                None => format!("{shape} with no DryRun"),
            };
            let case = case.as_str();
            let (router, _engine) = super::ec2_query::emulator();

            // One filter the account DOES hold, so the refusal is about the id the request
            // named rather than about an account that holds nothing.
            let held = created_filter(case, &router, &[]).await;
            assert_ne!(
                held, id,
                "case {case:?}: the unresolvable id must not be the one the account holds"
            );

            let mut params = delete_params(id);
            if let Some(flag) = probe {
                params.push(param("DryRun", flag));
            }
            refused(
                case,
                &router,
                "DeleteTrafficMirrorFilter",
                &params,
                400,
                INVALID_TRAFFIC_MIRROR_FILTER_ID_NOT_FOUND,
                &format!("{id} does not exist"),
            )
            .await;

            let reported_set = described_set(case, &router, &[]).await;
            assert!(
                reported_set.contains(&held),
                "case {case:?}: the filter the account holds must still be reported, got \
                 {reported_set}"
            );
        }
    }
}

/// RULE 3: a probe naming a filter the delete WOULD have removed is answered 412
/// `DryRunOperation` with that answer's whole fixed sentence, and the filter is reported exactly
/// as it was — twice over, since a probe is not a write — while the same delete without the flag
/// removes it.
///
/// The rows differ along what the probed filter carries: nothing of its own, a description and
/// tags, rules in both directions, and a mirrored network service; and along whether the real
/// delete that follows states the flag as false or leaves it out, which are the same request.
#[tokio::test]
async fn a_dry_run_delete_answers_the_probe_and_leaves_the_filter_stored() {
    /// `(case, the create's own fields, the rules to add, whether it mirrors a network
    /// service, the flag the real delete states)`
    type Row = (
        &'static str,
        Vec<Param>,
        Vec<RuleSpec>,
        bool,
        Option<&'static str>,
    );

    let rows: Vec<Row> = vec![
        (
            "a filter carrying nothing of its own",
            Vec::new(),
            Vec::new(),
            false,
            None,
        ),
        (
            "a filter with a description and two tags",
            [
                vec![param("Description", "mirror: ingress/egress — grün")],
                super::ec2_query::tag_specification(
                    TRAFFIC_MIRROR_FILTER_TAGGABLE_RESOURCE,
                    &[("Env", "staging"), ("Owner", "net-eng")],
                ),
            ]
            .concat(),
            Vec::new(),
            false,
            Some("false"),
        ),
        (
            "a filter with a rule in each direction",
            Vec::new(),
            vec![(TRAFFIC_DIRECTION_INGRESS, 100), (TRAFFIC_DIRECTION_EGRESS, 7)],
            false,
            None,
        ),
        (
            "a filter mirroring every network service the model declares",
            Vec::new(),
            vec![(TRAFFIC_DIRECTION_INGRESS, 1)],
            true,
            Some("false"),
        ),
    ];

    for (case, create_fields, rules, mirrors_network_service, real_delete_flag) in rows {
        let (router, _engine) = super::ec2_query::emulator();
        let filter_id = created_filter(case, &router, &create_fields).await;
        for (traffic_direction, rule_number) in &rules {
            ok(
                case,
                &router,
                "CreateTrafficMirrorFilterRule",
                &rule_params(&filter_id, traffic_direction, *rule_number),
            )
            .await;
        }
        if mirrors_network_service {
            ok(
                case,
                &router,
                "ModifyTrafficMirrorFilterNetworkServices",
                &network_services_params(&filter_id),
            )
            .await;
        }

        let by_id = indexed("TrafficMirrorFilterId", &[filter_id.as_str()]);
        let before = described_set(case, &router, &by_id).await;

        for probe in 1..=2usize {
            let probed = format!("{case}, probe {probe}");
            let probed = probed.as_str();
            let mut params = delete_params(&filter_id);
            params.push(param("DryRun", "true"));
            refused(
                probed,
                &router,
                "DeleteTrafficMirrorFilter",
                &params,
                DRY_RUN_OPERATION_STATUS,
                DRY_RUN_OPERATION,
                DRY_RUN_OPERATION_MESSAGE,
            )
            .await;

            let after = described_set(probed, &router, &by_id).await;
            assert_eq!(
                after, before,
                "case {probed:?}: a probe must leave the filter reported as it was, expected \
                 {before}, got {after}"
            );
        }

        let mut params = delete_params(&filter_id);
        if let Some(flag) = real_delete_flag {
            params.push(param("DryRun", flag));
        }
        let body = ok(case, &router, "DeleteTrafficMirrorFilter", &params).await;
        let answered = reported(case, &body, "trafficMirrorFilterId");
        assert_eq!(
            answered, filter_id,
            "case {case:?}: expected the delete to report {filter_id}, got {answered:?}"
        );
        refused(
            case,
            &router,
            "DescribeTrafficMirrorFilters",
            &by_id,
            400,
            INVALID_TRAFFIC_MIRROR_FILTER_ID_NOT_FOUND,
            &format!("{filter_id} does not exist"),
        )
        .await;
    }
}

/// RULE 4: "You cannot delete a Traffic Mirror filter that is in use by a Traffic Mirror
/// session" (`DeleteTrafficMirrorFilter`) — a refusal that reads the store wins over the probe
/// as well, naming the filter THE REQUEST named, and the very same delete succeeds once nothing
/// mirrors by the filter.
///
/// The rows differ along how many sessions depend on the filter and along whether the delete
/// states the flag, which is what tells a probe's answer from the dependency's.
#[tokio::test]
async fn a_filter_a_session_mirrors_by_is_refused_under_a_probe_as_well() {
    for sessions_held in 1..=2usize {
        let case = format!("a filter {sessions_held} session(s) mirror by");
        let case = case.as_str();
        let (router, engine) = super::ec2_query::emulator();
        let filter_id = created_filter(case, &router, &[]).await;
        let sessions: Vec<String> = (0..sessions_held)
            .map(|index| seed_session(case, &engine, &filter_id, index))
            .collect();

        let dependency_message =
            format!("The trafficMirrorFilter '{filter_id}' has dependencies and cannot be deleted.");
        for probe in [None, Some("true"), Some("false")] {
            let probed = match probe {
                Some(flag) => format!("{case} with DryRun={flag}"),
                None => format!("{case} with no DryRun"),
            };
            let mut params = delete_params(&filter_id);
            if let Some(flag) = probe {
                params.push(param("DryRun", flag));
            }
            refused(
                &probed,
                &router,
                "DeleteTrafficMirrorFilter",
                &params,
                400,
                DEPENDENCY_VIOLATION,
                &dependency_message,
            )
            .await;
        }

        let by_id = indexed("TrafficMirrorFilterId", &[filter_id.as_str()]);
        let still_reported = described_set(case, &router, &by_id).await;
        assert!(
            still_reported.contains(&filter_id),
            "case {case:?}: a refused delete must leave {filter_id} stored, got {still_reported}"
        );

        for session_id in &sessions {
            ok(
                case,
                &router,
                "DeleteTrafficMirrorSession",
                &[param("TrafficMirrorSessionId", session_id)],
            )
            .await;
        }

        // With nothing mirroring by it, the probe answers the outcome the real call then has.
        let mut probe_params = delete_params(&filter_id);
        probe_params.push(param("DryRun", "true"));
        refused(
            case,
            &router,
            "DeleteTrafficMirrorFilter",
            &probe_params,
            DRY_RUN_OPERATION_STATUS,
            DRY_RUN_OPERATION,
            DRY_RUN_OPERATION_MESSAGE,
        )
        .await;
        let body = ok(
            case,
            &router,
            "DeleteTrafficMirrorFilter",
            &delete_params(&filter_id),
        )
        .await;
        let answered = reported(case, &body, "trafficMirrorFilterId");
        assert_eq!(
            answered, filter_id,
            "case {case:?}: expected the delete to report {filter_id}, got {answered:?}"
        );
    }
}

/// RULE 5: a deleted filter's id stops resolving for EVERY operation that names a Traffic
/// Mirror filter, while the filter the account still holds serves each of those very calls — so
/// the refusal follows from the record having left the store rather than from the operation
/// having stopped working. The client tokens the removed filter and the rules living on it were
/// created under name nothing afterwards: a create carrying one mints a fresh resource.
///
/// The rows differ along how many rules the deleted filter carried and whether it mirrored a
/// network service, and the table spans the four operations that resolve a filter id.
#[tokio::test]
async fn a_deleted_filter_stops_resolving_for_every_operation_that_names_one() {
    /// `(case, the rules the deleted filter carried, whether it mirrored a network service)`
    type Row = (&'static str, Vec<RuleSpec>, bool);

    let rows: Vec<Row> = vec![
        ("a filter with no rule of its own", Vec::new(), false),
        (
            "a filter with one ingress rule",
            vec![(TRAFFIC_DIRECTION_INGRESS, 100)],
            false,
        ),
        (
            "a filter with a rule in each direction, mirroring a network service",
            vec![(TRAFFIC_DIRECTION_INGRESS, 25), (TRAFFIC_DIRECTION_EGRESS, 50)],
            true,
        ),
    ];

    for (case, rules, mirrors_network_service) in rows {
        let (router, _engine) = super::ec2_query::emulator();
        let filter_token = "token/deleted-filter";
        let deleted = created_filter(case, &router, &[param("ClientToken", filter_token)]).await;

        let mut rule_tokens = Vec::new();
        let mut rule_ids = Vec::new();
        for (position, (traffic_direction, rule_number)) in rules.iter().enumerate() {
            let token = format!("token/rule-{position}");
            let mut params = rule_params(&deleted, traffic_direction, *rule_number);
            params.push(param("ClientToken", &token));
            let body = ok(case, &router, "CreateTrafficMirrorFilterRule", &params).await;
            rule_ids.push(reported(case, &body, "trafficMirrorFilterRuleId"));
            rule_tokens.push((token, *traffic_direction, *rule_number));
        }
        if mirrors_network_service {
            ok(
                case,
                &router,
                "ModifyTrafficMirrorFilterNetworkServices",
                &network_services_params(&deleted),
            )
            .await;
        }

        let body = ok(
            case,
            &router,
            "DeleteTrafficMirrorFilter",
            &delete_params(&deleted),
        )
        .await;
        let answered = reported(case, &body, "trafficMirrorFilterId");
        assert_eq!(
            answered, deleted,
            "case {case:?}: expected the delete to report {deleted}, got {answered:?}"
        );

        // Every operation that resolves a filter id: the deleted id is refused, an id the
        // account holds is served. Each row gets a filter of its own to be served for, so a
        // row whose operation is itself the delete cannot take the next row's resource away.
        let not_found = format!("{deleted} does not exist");
        for (position, action) in [
            "DescribeTrafficMirrorFilters",
            "DeleteTrafficMirrorFilter",
            "CreateTrafficMirrorFilterRule",
            "ModifyTrafficMirrorFilterNetworkServices",
        ]
        .into_iter()
        .enumerate()
        {
            let named = format!("{case}, named by {action}");
            let named = named.as_str();
            let survivor = created_filter(named, &router, &[param("Description", "kept")]).await;
            // One rule number per position, so the call against the survivor is never refused
            // for asking a number that filter already holds a rule at.
            let rule_number = 500 + i32::try_from(position).unwrap_or_default();
            let for_id = |id: &str| -> Vec<Param> {
                match action {
                    "DescribeTrafficMirrorFilters" => indexed("TrafficMirrorFilterId", &[id]),
                    "CreateTrafficMirrorFilterRule" => {
                        rule_params(id, TRAFFIC_DIRECTION_INGRESS, rule_number)
                    }
                    "ModifyTrafficMirrorFilterNetworkServices" => network_services_params(id),
                    _ => delete_params(id),
                }
            };

            refused(
                named,
                &router,
                action,
                &for_id(&deleted),
                400,
                INVALID_TRAFFIC_MIRROR_FILTER_ID_NOT_FOUND,
                &not_found,
            )
            .await;
            let served = ok(named, &router, action, &for_id(&survivor)).await;
            assert!(
                served.contains(&survivor),
                "case {named:?}: {action} must still serve {survivor}, got {served}"
            );
        }

        // The token the removed filter was created under names nothing: the create mints a new
        // filter rather than reporting the one that left the store.
        let reborn = created_filter(case, &router, &[param("ClientToken", filter_token)]).await;
        assert_ne!(
            reborn, deleted,
            "case {case:?}: a create carrying the removed filter's token must mint a new \
             filter, got {reborn}"
        );

        // And so does each token one of its rules was created under.
        for (token, traffic_direction, rule_number) in &rule_tokens {
            let mut params = rule_params(&reborn, traffic_direction, *rule_number);
            params.push(param("ClientToken", token));
            let body = ok(case, &router, "CreateTrafficMirrorFilterRule", &params).await;
            let rule_id = reported(case, &body, "trafficMirrorFilterRuleId");
            assert!(
                !rule_ids.contains(&rule_id),
                "case {case:?}: a create carrying a removed rule's token {token} must mint a \
                 new rule, got {rule_id}"
            );
            let on_filter = reported(case, &body, "trafficMirrorFilterId");
            assert_eq!(
                on_filter, reborn,
                "case {case:?}: the new rule must live on {reborn}, got {on_filter:?}"
            );
        }
    }
}
