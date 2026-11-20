// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the status a `ResolverRule` projection describes:
//! the `Status`/`StatusMessage` pair `CreateResolverRule`, `GetResolverRule`,
//! `ListResolverRules`, `UpdateResolverRule` and `DeleteResolverRule` answer
//! with.
//!
//! Every case goes over the wire the service actually speaks (awsJson1_1,
//! `POST /` with `X-Amz-Target: Route53Resolver.<OperationName>`), through the
//! shared [`super::harness`], so what is asserted is the HTTP behaviour and not
//! a handler called directly.
//!
//! What these cases pin is the rule the slice implements: a stored Resolver
//! rule carries the sentence describing the lifecycle step its LAST write
//! reached — "a detailed description of the status of a Resolver rule", traced
//! to the request that reached that step — and EVERY projection of the rule
//! reports it, because create, read, list, update and delete all answer through
//! the one `ResolverRule` serializer. So the member is never absent from a
//! listed rule, a reader never reports a rule the listing describes
//! differently, and a rule a caller has changed describes its status by that
//! change rather than by its creation.

use serde_json::{json, Value};
use service_common::xray_trace::is_trace_id;
use smithy_server_runtime::MultiServiceRouter;

use super::harness::{call_tracing, emulator, ok, string_member, tracing_chain, Emulator};

/// The sentences Route 53 Resolver states for the settled step a write left a
/// rule in: the step its create reached, and the step an update left its new
/// settings in. Each is fixed text for that step, with no part of the request
/// in it.
const CREATED_SENTENCE: &str = "Successfully created Resolver Rule.";
const UPDATED_SENTENCE: &str = "Successfully updated Resolver Rule.";

/// `StatusMessage`'s modelled maximum length, from the member's `length` trait
/// (0..255) on `ResolverRule$StatusMessage`
/// (`smithy_query(query="shape",shape="StatusMessage")`).
const STATUS_MESSAGE_MAX: usize = 255;

/// The settled `ResolverRuleStatus` a stored rule reports, out of the model's
/// enum (`smithy_query(query="shape",shape="ResolverRuleStatus")`).
const SETTLED: &str = "COMPLETE";

/// The members of a create request whose values the step sentence must not
/// quote: what it describes is the step, not what the caller asked for.
const REQUEST_MEMBERS: &[&str] = &[
    "CreatorRequestId",
    "Name",
    "DomainName",
    "DelegationRecord",
    "ResolverEndpointId",
];

/// The `ResolverRule` of a create/get/update/delete response.
fn rule(body: &Value) -> &Value {
    body.get("ResolverRule")
        .expect("the response carries a ResolverRule")
}

/// Call an operation that must succeed, carrying the tracing chain `tracing`
/// when the case sends one, and return its body.
async fn ok_traced(
    router: &MultiServiceRouter,
    operation: &str,
    body: Value,
    tracing: Option<&str>,
) -> Value {
    let (status, parsed) = call_tracing(router, operation, body, tracing).await;
    assert_eq!(
        status, 200,
        "{operation} must succeed, got {status}: {parsed}"
    );
    parsed
}

/// The trace id and the sentence `message` carries, when it is a Resolver rule's
/// status message: the trace clause of the call that reached the step, in
/// brackets, then the sentence describing the step. `None` for text in any other
/// shape, so a case that expects one reports what it got instead.
///
/// The trace id is read through the one reading of X-Ray's own form the services
/// of this emulator share, so a well-formed id is judged well-formed here too.
fn traced_step(message: &str) -> Option<(&str, &str)> {
    let (trace, stated) = message.strip_prefix("[Trace id: ")?.split_once("] ")?;
    is_trace_id(trace).then_some((trace, stated))
}

/// Create one outbound Resolver endpoint a FORWARD rule can route through, and
/// answer its id. Its addresses sit in subnets of the caller's VPC, which the
/// create resolves against the account's own Amazon EC2 store, so those subnets
/// are registered there first.
async fn outbound_endpoint(router: &Emulator, creator: &str) -> String {
    let subnets = [format!("subnet-{creator}-a"), format!("subnet-{creator}-b")];
    router.hold_subnets(&subnets);
    let created = ok(
        router,
        "CreateResolverEndpoint",
        json!({
            "CreatorRequestId": creator,
            "Direction": "OUTBOUND",
            "SecurityGroupIds": ["sg-00000000000000001"],
            "IpAddresses": [
                { "SubnetId": subnets[0], "Ip": "10.0.0.10" },
                { "SubnetId": subnets[1], "Ip": "10.0.1.10" },
            ],
        }),
    )
    .await;
    string_member(
        created
            .get("ResolverEndpoint")
            .expect("the create answers a ResolverEndpoint"),
        "Id",
    )
}

/// One row of the table: the create it is driven by, and whether that create
/// carries a tracing chain of its own.
struct RuleCase {
    case: &'static str,
    /// `{endpoint}` in `ResolverEndpointId` is replaced by the id of the stored
    /// OUTBOUND endpoint.
    request: Value,
    /// Whether the create sends a tracing header naming a trace — the chain a
    /// caller starts, which the step sentence has to be reported under; a create
    /// that sends none is reported under a trace this service mints for it.
    traced: bool,
}

/// The rows the rule is exercised over.
///
/// They differ along what the rule generalises over: the rule's TYPE (the values
/// a caller may create, from `RuleTypeOption`
/// (`smithy_query(query="shape",shape="RuleTypeOption")`) minus `RECURSIVE`,
/// which only Resolver itself creates), whether the rule carries a `Name`, tags,
/// target addresses and in which address family, and whether the create itself
/// was traced — none of which is something the step a rule stands in depends on.
fn rule_cases() -> Vec<RuleCase> {
    vec![
        RuleCase {
            case: "SYSTEM, a named subdomain rule, the create traced by its caller",
            request: json!({
                "CreatorRequestId": "status-system",
                "Name": "status-apex",
                "RuleType": "SYSTEM",
                "DomainName": "apex.example.com",
            }),
            traced: true,
        },
        RuleCase {
            case: "SYSTEM, no Name, the create carrying no tracing header",
            request: json!({
                "CreatorRequestId": "status-system-untraced",
                "RuleType": "SYSTEM",
                "DomainName": "untraced.example.com",
            }),
            traced: false,
        },
        RuleCase {
            case: "FORWARD, one IPv4 target through an outbound endpoint",
            request: json!({
                "CreatorRequestId": "status-forward",
                "Name": "status-forward",
                "RuleType": "FORWARD",
                "DomainName": "forward.example.com",
                "ResolverEndpointId": "{endpoint}",
                "TargetIps": [{ "Ip": "192.0.2.10" }],
            }),
            traced: true,
        },
        RuleCase {
            case: "FORWARD, an IPv6 target over DoH, with tags",
            request: json!({
                "CreatorRequestId": "status-forward-v6",
                "RuleType": "FORWARD",
                "DomainName": "v6.example.com",
                "ResolverEndpointId": "{endpoint}",
                "TargetIps": [{
                    "Ipv6": "2001:db8::53",
                    "Port": 443,
                    "Protocol": "DoH",
                    "ServerNameIndication": "doh.example.com",
                }],
                "Tags": [{ "Key": "team", "Value": "networking" }],
            }),
            traced: false,
        },
        RuleCase {
            case: "DELEGATE, keyed on a delegation record",
            request: json!({
                "CreatorRequestId": "status-delegate",
                "RuleType": "DELEGATE",
                "DomainName": "delegated.example.com",
                "DelegationRecord": "ns.example.com",
            }),
            traced: true,
        },
    ]
}

/// The element of `ResolverRules` whose `Arn` is `arn`, out of every page of a
/// `ListResolverRules` for `request`.
async fn listed_rule(
    router: &MultiServiceRouter,
    request: &Value,
    arn: &str,
    case: &str,
) -> Option<Value> {
    let mut next_token: Option<String> = None;
    let mut pages = 0;
    loop {
        let mut body = request.clone();
        if let Some(token) = &next_token {
            body["NextToken"] = json!(token);
        }
        let page = ok(router, "ListResolverRules", body).await;
        pages += 1;
        assert!(
            pages <= 200,
            "case {case:?}: paging did not terminate after {pages} pages"
        );
        let items = page
            .get("ResolverRules")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: the page carries ResolverRules, got {page}"));
        if let Some(found) = items
            .into_iter()
            .find(|item| string_member(item, "Arn") == arn)
        {
            return Some(found);
        }
        next_token = page
            .get("NextToken")
            .and_then(Value::as_str)
            .map(str::to_string);
        next_token.as_ref()?;
    }
}

/// Every projection of a rule describes the step it reports: a write answers
/// the step's sentence under the trace of the call that reached it, and the
/// read and the listing (unpaged, paged and filtered) report that same sentence
/// — the listing is not a reduced projection of its own. A write that changes
/// nothing reaches no new step, so it restates the one the rule stands in.
#[tokio::test]
async fn every_rule_projection_describes_the_step_it_reports() {
    for RuleCase {
        case,
        request,
        traced,
    } in rule_cases()
    {
        // One emulator per row, so the store holds exactly this row's rule.
        let router = emulator();
        let mut request = request;
        if request.get("ResolverEndpointId").is_some() {
            let endpoint = outbound_endpoint(&router, "status-endpoint").await;
            request["ResolverEndpointId"] = json!(endpoint);
        }

        let (create_chain, create_root) = tracing_chain(41);
        let created = ok_traced(
            &router,
            "CreateResolverRule",
            request.clone(),
            traced.then_some(create_chain.as_str()),
        )
        .await;
        let made = rule(&created).clone();
        let id = string_member(&made, "Id");
        let arn = string_member(&made, "Arn");

        assert_eq!(
            string_member(&made, "Status"),
            SETTLED,
            "case {case:?}: a created rule stands in its settled step, got {made}"
        );
        let message = string_member(&made, "StatusMessage");
        let Some((trace, stated)) = traced_step(&message) else {
            panic!(
                "case {case:?}: the create must describe the rule's status as the step's sentence \
                 under the trace of the call that reached it, got {message:?}"
            );
        };
        assert_eq!(
            stated, CREATED_SENTENCE,
            "case {case:?}: expected the created step's sentence, got {message:?}"
        );
        if traced {
            assert_eq!(
                trace, create_root,
                "case {case:?}: the step must be reported under the trace the caller started, got \
                 {message:?}"
            );
        }
        assert!(
            message.len() <= STATUS_MESSAGE_MAX,
            "case {case:?}: StatusMessage must satisfy its modelled length 0..{STATUS_MESSAGE_MAX}, \
             got {message:?}"
        );
        for member in REQUEST_MEMBERS {
            if let Some(value) = request.get(member).and_then(Value::as_str) {
                assert!(
                    !message.contains(value),
                    "case {case:?}: the step sentence must not quote {member} ({value:?}), got \
                     {message:?}"
                );
            }
        }

        // The read describes the step the rule stands in, not the step the
        // READING call stands in: a get under a different trace reports the very
        // same sentence.
        let (read_chain, _) = tracing_chain(42);
        let fetched = ok_traced(
            &router,
            "GetResolverRule",
            json!({ "ResolverRuleId": id.clone() }),
            Some(read_chain.as_str()),
        )
        .await;
        assert_eq!(
            string_member(rule(&fetched), "StatusMessage"),
            message,
            "case {case:?}: the read must describe the rule's status exactly as the create did, \
             got {fetched}"
        );

        // The listing reports the same projection, however the page was cut and
        // whichever filter selected the rule.
        let listings = [
            ("unpaged", json!({})),
            ("one rule per page", json!({ "MaxResults": 1 })),
            (
                "filtered by CreatorRequestId",
                json!({
                    "Filters": [{
                        "Name": "CreatorRequestId",
                        "Values": [string_member(&made, "CreatorRequestId")],
                    }],
                }),
            ),
        ];
        for (listing, body) in listings {
            let case = format!("{case} / {listing}");
            let listed = listed_rule(&router, &body, &arn, &case)
                .await
                .unwrap_or_else(|| panic!("case {case:?}: the listing must report the rule"));
            assert_eq!(
                string_member(&listed, "StatusMessage"),
                message,
                "case {case:?}: a listed rule must describe its status as the read does, got \
                 {listed}"
            );
            assert_eq!(
                listed,
                *rule(&fetched),
                "case {case:?}: the listing must report the same rule the read does, got {listed}"
            );
        }

        // An update writes the rule's settings, which are in effect when it
        // answers, so the rule stands in the settled step THAT call reached and
        // describes its status by the update, under the trace of the updating
        // call.
        let (update_chain, update_root) = tracing_chain(43);
        let updated = ok_traced(
            &router,
            "UpdateResolverRule",
            json!({
                "ResolverRuleId": id.clone(),
                "Config": { "Name": "status-renamed" },
            }),
            Some(update_chain.as_str()),
        )
        .await;
        assert_eq!(
            string_member(rule(&updated), "Status"),
            SETTLED,
            "case {case:?}: an update leaves the rule settled, got {updated}"
        );
        let after_update = string_member(rule(&updated), "StatusMessage");
        let Some((trace, stated)) = traced_step(&after_update) else {
            panic!(
                "case {case:?}: the update must describe the rule's status as the step's sentence \
                 under the trace of the call that reached it, got {after_update:?}"
            );
        };
        assert_eq!(
            stated, UPDATED_SENTENCE,
            "case {case:?}: expected the updated step's sentence, got {after_update:?}"
        );
        assert_eq!(
            trace, update_root,
            "case {case:?}: the step must be reported under the trace of the updating call, got \
             {after_update:?}"
        );
        assert!(
            after_update.len() <= STATUS_MESSAGE_MAX,
            "case {case:?}: StatusMessage must satisfy its modelled length \
             0..{STATUS_MESSAGE_MAX}, got {after_update:?}"
        );

        // And a later read reports that same sentence: the step the rule stands
        // in is the one its last write left it in, for every projection of it.
        let reread = ok(
            &router,
            "GetResolverRule",
            json!({ "ResolverRuleId": id.clone() }),
        )
        .await;
        assert_eq!(
            string_member(rule(&reread), "StatusMessage"),
            after_update,
            "case {case:?}: the read after the update must describe the step the update left the \
             rule in, got {reread}"
        );
        let listed = listed_rule(&router, &json!({}), &arn, case)
            .await
            .unwrap_or_else(|| panic!("case {case:?}: the listing must report the rule"));
        assert_eq!(
            string_member(&listed, "StatusMessage"),
            after_update,
            "case {case:?}: a listed rule must describe its status as the read does, got {listed}"
        );

        // An update that names no new setting writes nothing, so it reaches no
        // new step and leaves the sentence the last write stamped.
        let (idle_chain, _) = tracing_chain(44);
        let idle = ok_traced(
            &router,
            "UpdateResolverRule",
            json!({ "ResolverRuleId": id.clone(), "Config": {} }),
            Some(idle_chain.as_str()),
        )
        .await;
        assert_eq!(
            string_member(rule(&idle), "StatusMessage"),
            after_update,
            "case {case:?}: an update that changes nothing must leave the step the rule stands \
             in, got {idle}"
        );

        // And so does the delete, which answers the rule it took out as the
        // store held it, with the status that request leaves it in.
        let deleted = ok(
            &router,
            "DeleteResolverRule",
            json!({ "ResolverRuleId": id.clone() }),
        )
        .await;
        assert_eq!(
            string_member(rule(&deleted), "StatusMessage"),
            after_update,
            "case {case:?}: the delete must answer the stored rule's status description, got \
             {deleted}"
        );
    }
}

/// A listing describes EVERY rule it reports, each with the sentence its own
/// create answered — so no rule in the collection is served as a reduced
/// projection, whatever page it falls on.
///
/// The rows differ in how many rules the store holds and how the pages are cut,
/// which is what a list projection generalises over.
#[tokio::test]
async fn a_listing_describes_every_rule_it_reports() {
    for (case, count, page_size) in [
        ("one rule, unpaged", 1usize, None),
        ("four rules, unpaged", 4usize, None),
        ("four rules, one per page", 4usize, Some(1i64)),
        ("four rules, three per page", 4usize, Some(3i64)),
    ] {
        let router = emulator();

        // Each rule is created under its own tracing chain, so a projection that
        // reported one rule's sentence for another would be caught.
        let mut expected: Vec<(String, String)> = Vec::new();
        for index in 0..count {
            let (chain, _) = tracing_chain(u32::try_from(index).unwrap_or_default() + 100);
            let created = ok_traced(
                &router,
                "CreateResolverRule",
                json!({
                    "CreatorRequestId": format!("listing-status-{index}"),
                    "Name": format!("listing-status-{index}"),
                    "RuleType": "SYSTEM",
                    "DomainName": format!("listing-{index}.example.com"),
                }),
                Some(chain.as_str()),
            )
            .await;
            let made = rule(&created);
            expected.push((
                string_member(made, "Arn"),
                string_member(made, "StatusMessage"),
            ));
        }

        let request = match page_size {
            Some(size) => json!({ "MaxResults": size }),
            None => json!({}),
        };
        for (arn, message) in &expected {
            let listed = listed_rule(&router, &request, arn, case)
                .await
                .unwrap_or_else(|| panic!("case {case:?}: the listing must report {arn}"));
            let reported = string_member(&listed, "StatusMessage");
            assert_eq!(
                &reported, message,
                "case {case:?}: the listed rule {arn} must carry the status description its \
                 create answered, got {listed}"
            );
            let Some((_, stated)) = traced_step(&reported) else {
                panic!("case {case:?}: {arn} must describe its step, got {reported:?}");
            };
            assert_eq!(
                stated, CREATED_SENTENCE,
                "case {case:?}: {arn} stands in its created step, got {reported:?}"
            );
        }
    }
}
