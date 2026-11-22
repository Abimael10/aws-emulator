// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the FORM a minted resource id's suffix wears — which alphabet it
//! is drawn from and which character it begins with — and for every value that embeds
//! such an id.
//!
//! Every case drives the emulator the way a client does — a restJson1 request through the
//! router that mounts the service — and reads what it asserts back out of a LATER request
//! (`GetService`, `GetListener`, `GetServiceNetworkServiceAssociation`, the listing, the
//! delete acknowledgement), so an id minted on the create path and an id echoed on a read
//! path are both under test.
//!
//! The rule under test, asserted over a TABLE spanning every kind this service mints an
//! id for and several mints of each: a minted id is its kind's prefix plus a 17-character
//! suffix of the form `0` followed by 16 characters of the LOWERCASE HEX alphabet, and
//! that suffix travels unchanged into every value that embeds it — the resource ARNs
//! (including the ones that nest under a parent), the `resourceId` of a conflict, and the
//! ids a read or a listing echoes.
//!
//! The modelled patterns are wider than that (`^svc-[0-9a-z]{17}$`,
//! `^listener-[0-9a-z]{17}$`, `^sn-[0-9a-z]{17}$`, `^snsa-[0-9a-z]{17}$`,
//! `^rule-[0-9a-z]{17}$`, `^tg-[0-9a-z]{17}$` —
//! `smithy_query(query="shape",shape="ServiceId,ListenerId,ServiceNetworkId,TargetGroupId,RuleId")`),
//! so this form satisfies each of them while a base-36 suffix, or a hex suffix with
//! another leading character, is a value the real service never hands out.
//!
//! A RULE id has its own suite below, because a listener's rules are reported by four
//! operations and one of them is minted without any request naming it: the implicit
//! default rule a listener is created with, and a created rule, both as `CreateRule`,
//! `GetRule`, `UpdateRule` and `ListRules` report their `id` and their nested `arn`.

use crate::default_rule_and_rule_update::update_rule;
use crate::list_rules::{default_rule_id, items_of, list_page};
use crate::listeners::{a_service, create_listener, listener_kind, target_group};
use crate::rules::{a_listener, create_rule, fixed_response_rule, get_rule, rule_kind};
use crate::target_groups::{create_target_group, get_target_group, type_catalog_rows};
use crate::vpclattice_service_slice::{as_label, expected_arn, message_of, Answer, Lattice};
use service_com_amazonaws_vpclattice_mercurycontrolplane::state;

/// How many characters a minted id carries after its prefix, as every modelled id pattern
/// of this service spells it (`[0-9a-z]{17}`).
const ID_SUFFIX_LEN: usize = 17;

/// How many mints of one kind each row of the table asks for: one id could be hex by luck,
/// a run of them cannot.
const MINTS_PER_KIND: usize = 4;

/// Every id prefix this service mints an id under (`state::PREFIX_*`).
const ID_PREFIXES: &[&str] = &[
    state::PREFIX_SERVICE_NETWORK,
    state::PREFIX_SERVICE,
    state::PREFIX_SERVICE_ASSOCIATION,
    state::PREFIX_LISTENER,
    state::PREFIX_RULE,
    state::PREFIX_TARGET_GROUP,
    state::PREFIX_RESOURCE_CONFIGURATION,
];

/// Whether `suffix` is the 17 characters a minted id wears: the fixed leading `0` and 16
/// lowercase-hex digits.
fn is_hex_suffix(suffix: &str) -> bool {
    suffix.len() == ID_SUFFIX_LEN
        && suffix.starts_with('0')
        && suffix
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

/// Assert `id` is `prefix` plus that suffix.
fn assert_hex_id(case: &str, id: &str, prefix: &str) {
    let suffix = id
        .strip_prefix(prefix)
        .unwrap_or_else(|| panic!("case {case}: expected an id beginning {prefix:?}, got {id:?}"));
    assert!(
        is_hex_suffix(suffix),
        "case {case}: expected {prefix} plus `0` and 16 characters from [0-9a-f], \
         got {id:?} (suffix {suffix:?})"
    );
}

/// Assert that every id EMBEDDED anywhere in a response — bare, inside an ARN, or as the
/// label of a DNS name — wears that suffix. This walks the whole body, so a member no row
/// names explicitly is covered too.
fn assert_embedded_ids_are_hex(case: &str, value: &serde_json::Value) {
    match value {
        serde_json::Value::String(text) => {
            for prefix in ID_PREFIXES {
                let mut rest = text.as_str();
                while let Some(at) = rest.find(prefix) {
                    let tail = &rest[at + prefix.len()..];
                    let suffix: String = tail
                        .chars()
                        .take_while(char::is_ascii_alphanumeric)
                        .collect();
                    assert!(
                        is_hex_suffix(&suffix),
                        "case {case}: {text:?} embeds a {prefix} id whose suffix is \
                         {suffix:?}, expected `0` and 16 characters from [0-9a-f]"
                    );
                    rest = tail;
                }
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                assert_embedded_ids_are_hex(case, item);
            }
        }
        serde_json::Value::Object(members) => {
            for member in members.values() {
                assert_embedded_ids_are_hex(case, member);
            }
        }
        _ => {}
    }
}

/// The answer of a call that must have been served, with every id it embeds checked.
fn served(case: &str, expected_status: u16, answer: &Answer) {
    assert_eq!(
        answer.status, expected_status,
        "case {case}: expected {expected_status}, got {} / {}",
        answer.status, answer.body
    );
    assert_embedded_ids_are_hex(case, &answer.body);
}

/// The `dnsEntry.domainName` of a response.
fn domain_name_of(case: &str, answer: &Answer) -> String {
    answer.body["dnsEntry"]["domainName"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "case {case}: expected a dnsEntry.domainName, got {}",
                answer.body
            )
        })
        .to_string()
}

/// Every kind this service mints an id for, several mints each: the id wears AWS's suffix
/// form and the ARN the same call answers embeds exactly that id.
#[tokio::test]
async fn every_minted_id_and_arn_carries_a_lowercase_hex_suffix() {
    let lattice = Lattice::new();

    // A service network and a service to hang the nested kinds off, and a second service
    // to associate.
    let mut network_ids: Vec<String> = Vec::new();
    for mint in 0..MINTS_PER_KIND {
        let case = format!("service network mint {mint}");
        let created = lattice
            .create_service_network(serde_json::json!({ "name": format!("hex-net-{mint}") }))
            .await;
        served(&case, 201, &created);
        let id = created.str_field("id");
        assert_hex_id(&case, &id, state::PREFIX_SERVICE_NETWORK);
        assert_eq!(
            created.str_field("arn"),
            expected_arn(state::KIND_SERVICE_NETWORK, &id),
            "case {case}: the ARN must embed the minted id {id:?}, got {}",
            created.body
        );
        network_ids.push(id);
    }

    let mut service_ids: Vec<String> = Vec::new();
    for mint in 0..MINTS_PER_KIND {
        let case = format!("service mint {mint}");
        let created = lattice
            .create_service(serde_json::json!({ "name": format!("hex-host-{mint}") }))
            .await;
        served(&case, 201, &created);
        let id = created.str_field("id");
        assert_hex_id(&case, &id, state::PREFIX_SERVICE);
        assert_eq!(
            created.str_field("arn"),
            expected_arn(state::KIND_SERVICE, &id),
            "case {case}: the ARN must embed the minted id {id:?}, got {}",
            created.body
        );
        service_ids.push(id);
    }

    let host = &service_ids[0];
    let mut listener_ids: Vec<String> = Vec::new();
    for mint in 0..MINTS_PER_KIND {
        let case = format!("listener mint {mint}");
        let created = create_listener(
            &lattice,
            host,
            serde_json::json!({
                "name": format!("hex-front-{mint}"),
                "protocol": "HTTP",
                // A listener's port is unique within its service, so each mint takes its
                // own.
                "port": 30_000 + mint as i64,
                "defaultAction": { "fixedResponse": { "statusCode": 404 } },
            }),
        )
        .await;
        served(&case, 201, &created);
        let id = created.str_field("id");
        assert_hex_id(&case, &id, state::PREFIX_LISTENER);
        assert_eq!(
            created.str_field("arn"),
            expected_arn(&listener_kind(host), &id),
            "case {case}: the listener ARN must nest under its service and embed {id:?}, \
             got {}",
            created.body
        );
        listener_ids.push(id);
    }

    let listener = &listener_ids[0];
    for mint in 0..MINTS_PER_KIND {
        let case = format!("rule mint {mint}");
        let created = create_rule(
            &lattice,
            host,
            listener,
            fixed_response_rule(&format!("hex-route-{mint}"), mint as i64 + 1),
        )
        .await;
        served(&case, 201, &created);
        let id = created.str_field("id");
        assert_hex_id(&case, &id, state::PREFIX_RULE);
        assert_eq!(
            created.str_field("arn"),
            expected_arn(&rule_kind(host, listener), &id),
            "case {case}: the rule ARN must nest under its listener and embed {id:?}, got {}",
            created.body
        );
    }

    for mint in 0..MINTS_PER_KIND {
        let case = format!("target group mint {mint}");
        let created = create_target_group(
            &lattice,
            serde_json::json!({ "name": format!("hex-pool-{mint}"), "type": "LAMBDA" }),
        )
        .await;
        served(&case, 201, &created);
        let id = created.str_field("id");
        assert_hex_id(&case, &id, state::PREFIX_TARGET_GROUP);
        assert_eq!(
            created.str_field("arn"),
            expected_arn(state::KIND_TARGET_GROUP, &id),
            "case {case}: the ARN must embed the minted id {id:?}, got {}",
            created.body
        );
    }

    // One association per (network, service) pair the table built: a pair may be
    // associated only once, so each mint takes a fresh pair.
    for mint in 0..MINTS_PER_KIND {
        let case = format!("association mint {mint}");
        let created = lattice
            .associate(&service_ids[mint], &network_ids[mint])
            .await;
        served(&case, 200, &created);
        let id = created.str_field("id");
        assert_hex_id(&case, &id, state::PREFIX_SERVICE_ASSOCIATION);
        assert_eq!(
            created.str_field("arn"),
            expected_arn(state::KIND_SERVICE_ASSOCIATION, &id),
            "case {case}: the ARN must embed the minted id {id:?}, got {}",
            created.body
        );
    }
}

/// The minted suffix travels: a read, a listing, a delete acknowledgement and a conflict
/// all carry the same minted ids.
#[tokio::test]
async fn the_minted_suffix_travels_into_reads_listings_deletes_and_conflicts() {
    let lattice = Lattice::new();
    let network = {
        let created = lattice
            .create_service_network(serde_json::json!({ "name": "travel-net" }))
            .await;
        served("the fixture service network", 201, &created);
        created.str_field("id")
    };

    // Rows differing in the service name: the shortest the `ServiceName` pattern admits,
    // one with digits, one with inner hyphens, and one at the length a probe uses.
    let names = [
        "abc",
        "travel7",
        "travel-host-two",
        "travel-probe-host-00000000",
    ];
    let mut created_services: Vec<(String, String, String)> = Vec::new();
    for name in names {
        let case = format!("service {name:?}");
        let created = lattice
            .create_service(serde_json::json!({ "name": name }))
            .await;
        served(&case, 201, &created);
        let id = created.str_field("id");
        assert_hex_id(&case, &id, state::PREFIX_SERVICE);
        let domain_name = domain_name_of(&case, &created);

        // The read path echoes exactly what the create minted.
        let read = lattice.get_service(&id).await;
        served(&format!("{case} read back"), 200, &read);
        assert_eq!(
            read.str_field("arn"),
            created.str_field("arn"),
            "case {case}: GetService must echo the stored ARN, got {}",
            read.body
        );
        assert_eq!(
            domain_name_of(&format!("{case} read back"), &read),
            domain_name,
            "case {case}: GetService must announce the same host the create did, got {}",
            read.body
        );
        created_services.push((name.to_string(), id, domain_name));
    }

    // A listener on the first service, read back, then a duplicate name so the conflict
    // reports the stored listener's id.
    let (_, host, _) = &created_services[0];
    let listener = {
        let created = create_listener(
            &lattice,
            host,
            serde_json::json!({
                "name": "travel-front",
                "protocol": "HTTP",
                "defaultAction": { "fixedResponse": { "statusCode": 404 } },
            }),
        )
        .await;
        served("the listener", 201, &created);
        let id = created.str_field("id");
        assert_hex_id("the listener", &id, state::PREFIX_LISTENER);
        assert_eq!(
            created.str_field("serviceArn"),
            expected_arn(state::KIND_SERVICE, host),
            "case the listener: expected the parent service's ARN, got {}",
            created.body
        );
        id
    };
    let read = lattice
        .call(
            "GET",
            &format!("/services/{host}/listeners/{listener}"),
            None,
        )
        .await;
    served("the listener read back", 200, &read);
    assert_eq!(
        read.str_field("arn"),
        expected_arn(&listener_kind(host), &listener),
        "case the listener read back: expected the nested listener ARN, got {}",
        read.body
    );
    assert_eq!(
        read.str_field("serviceArn"),
        expected_arn(state::KIND_SERVICE, host),
        "case the listener read back: expected the parent service's ARN, got {}",
        read.body
    );

    let conflict = create_listener(
        &lattice,
        host,
        serde_json::json!({
            "name": "travel-front",
            "protocol": "HTTP",
            "port": 8080,
            "defaultAction": { "fixedResponse": { "statusCode": 404 } },
        }),
    )
    .await;
    assert_eq!(
        conflict.status, 409,
        "case the duplicate listener name: expected 409, got {} / {}",
        conflict.status, conflict.body
    );
    assert_embedded_ids_are_hex("the duplicate listener name", &conflict.body);
    assert_eq!(
        conflict.str_field("resourceId"),
        listener,
        "case the duplicate listener name: the conflict must name the stored listener, \
         got {}",
        conflict.body
    );

    // The association of the second service: create, read, list and delete all carry the
    // same minted ids, and the association reports the service's own domain name.
    let (_, associated, associated_domain) = &created_services[1];
    let created = lattice.associate(associated, &network).await;
    served("the association", 200, &created);
    let association = created.str_field("id");
    assert_hex_id("the association", &association, state::PREFIX_SERVICE_ASSOCIATION);
    assert_eq!(
        domain_name_of("the association", &created),
        *associated_domain,
        "case the association: expected the associated service's domain name, got {}",
        created.body
    );

    let read = lattice
        .call(
            "GET",
            &format!("/servicenetworkserviceassociations/{association}"),
            None,
        )
        .await;
    served("the association read back", 200, &read);
    for (member, expected) in [
        ("id", association.clone()),
        (
            "arn",
            expected_arn(state::KIND_SERVICE_ASSOCIATION, &association),
        ),
        ("serviceId", associated.clone()),
        ("serviceArn", expected_arn(state::KIND_SERVICE, associated)),
        ("serviceNetworkId", network.clone()),
        (
            "serviceNetworkArn",
            expected_arn(state::KIND_SERVICE_NETWORK, &network),
        ),
    ] {
        assert_eq!(
            read.str_field(member),
            expected,
            "case the association read back, member {member}: expected {expected:?}, got {}",
            read.body
        );
    }
    assert_eq!(
        domain_name_of("the association read back", &read),
        *associated_domain,
        "case the association read back: expected the service's domain name, got {}",
        read.body
    );

    let listed = lattice
        .call(
            "GET",
            &format!("/servicenetworkserviceassociations?serviceNetworkIdentifier={network}"),
            None,
        )
        .await;
    served("the association listing", 200, &listed);
    let items = listed.body["items"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        items.len(),
        1,
        "case the association listing: expected the one persisted association, got {}",
        listed.body
    );
    for (member, expected) in [
        ("id", association.clone()),
        (
            "arn",
            expected_arn(state::KIND_SERVICE_ASSOCIATION, &association),
        ),
        ("serviceId", associated.clone()),
        ("serviceArn", expected_arn(state::KIND_SERVICE, associated)),
        ("serviceNetworkId", network.clone()),
        (
            "serviceNetworkArn",
            expected_arn(state::KIND_SERVICE_NETWORK, &network),
        ),
    ] {
        assert_eq!(
            items[0][member].as_str(),
            Some(expected.as_str()),
            "case the association listing, member {member}: expected {expected:?}, got {}",
            items[0]
        );
    }
    assert_eq!(
        items[0]["dnsEntry"]["domainName"].as_str(),
        Some(associated_domain.as_str()),
        "case the association listing: expected the service's domain name, got {}",
        items[0]
    );

    let deleted = lattice
        .call(
            "DELETE",
            &format!("/servicenetworkserviceassociations/{association}"),
            None,
        )
        .await;
    served("the association delete", 200, &deleted);
    assert_eq!(
        deleted.str_field("id"),
        association,
        "case the association delete: expected the association's id, got {}",
        deleted.body
    );
    assert_eq!(
        deleted.str_field("arn"),
        expected_arn(state::KIND_SERVICE_ASSOCIATION, &association),
        "case the association delete: expected the association's ARN, got {}",
        deleted.body
    );

    // The delete acknowledgement of a service echoes its ARN with the same suffix.
    let (_, doomed, _) = &created_services[2];
    let deleted = lattice
        .call("DELETE", &format!("/services/{doomed}"), None)
        .await;
    served("the service delete", 200, &deleted);
    assert_eq!(
        deleted.str_field("arn"),
        expected_arn(state::KIND_SERVICE, doomed),
        "case the service delete: expected the deleted service's ARN, got {}",
        deleted.body
    );
}

/// A rule body a row of the rule table asks for, and the update it then sends.
type RuleRow = (&'static str, serde_json::Value, serde_json::Value);

/// The rules each listener of the table below gets, differing in the match form, the
/// action kind and the priority — at each published bound of `RulePriority`'s
/// `@range(1, 2000)` and in between — plus the update each one then takes. Every row is a
/// fresh mint of a rule id, and the update is a member that must NOT disturb the id the
/// create minted.
fn rule_rows() -> Vec<RuleRow> {
    vec![
        (
            "exact path, fixed response, priority 1 (the range floor)",
            serde_json::json!({
                "name": "form-exact-path",
                "priority": 1,
                "match": {
                    "httpMatch": {
                        "pathMatch": { "match": { "exact": "/orders" }, "caseSensitive": true },
                    },
                },
                "action": { "fixedResponse": { "statusCode": 404 } },
            }),
            // A priority no other rule of the listener holds, so the update is served.
            serde_json::json!({ "priority": 5 }),
        ),
        (
            "method and prefix path, forward to one target group, priority 1000",
            serde_json::json!({
                "name": "form-prefix-path",
                "priority": 1000,
                "match": {
                    "httpMatch": { "method": "GET", "pathMatch": { "match": { "prefix": "/api/" } } },
                },
                "action": {
                    "forward": { "targetGroups": [{ "targetGroupIdentifier": target_group("1") }] },
                },
            }),
            serde_json::json!({ "action": { "fixedResponse": { "statusCode": 500 } } }),
        ),
        (
            "header matches, two weighted target groups, priority 2000 (the range ceiling)",
            serde_json::json!({
                "name": "form-header-matches",
                "priority": 2000,
                "match": {
                    "httpMatch": {
                        "headerMatches": [
                            { "name": "x-version", "match": { "exact": "v2" } },
                            { "name": "x-tenant", "match": { "prefix": "acme" }, "caseSensitive": false },
                        ],
                    },
                },
                "action": {
                    "forward": {
                        "targetGroups": [
                            { "targetGroupIdentifier": target_group("2"), "weight": 30 },
                            { "targetGroupIdentifier": target_group("3"), "weight": 70 },
                        ],
                    },
                },
            }),
            serde_json::json!({
                "match": { "httpMatch": { "pathMatch": { "match": { "prefix": "/v2/" } } } },
            }),
        ),
        (
            "an httpMatch with no member, fixed response, priority 999",
            serde_json::json!({
                "name": "form-match-anything",
                "priority": 999,
                "match": { "httpMatch": {} },
                "action": { "fixedResponse": { "statusCode": 404 } },
            }),
            // An update supplying no member at all still answers with the stored rule.
            serde_json::json!({}),
        ),
    ]
}

/// The summary `ListRules` reports for one rule of a listener.
///
/// The page asks for `maxResults` at the ceiling of its modelled `@range(1, 100)`
/// (`smithy_query(query="operation",operation="ListRules")`), so one page carries every rule
/// a listener of this suite holds and a rule the listing does not report is a missing rule
/// rather than a rule on a later page.
async fn listed_summary(
    lattice: &Lattice,
    case: &str,
    service_id: &str,
    listener_id: &str,
    rule_id: &str,
) -> serde_json::Value {
    let listed = list_page(
        lattice,
        service_id,
        listener_id,
        &[("maxResults", "100".to_string())],
    )
    .await;
    served(case, 200, &listed);
    items_of(case, &listed)
        .into_iter()
        .find(|item| item["id"] == serde_json::json!(rule_id))
        .unwrap_or_else(|| {
            panic!("case {case}: the listing reports no rule {rule_id:?}: {}", listed.body)
        })
}

/// A RULE id keeps the minted suffix through every operation that reports one.
///
/// Two kinds of rule id are under test: the implicit DEFAULT rule's, minted when a
/// listener is created, and a created rule's. Each is then read back through `GetRule`,
/// acknowledged by `UpdateRule` and reported by `ListRules`, and in each place both the
/// bare `id` and the `arn` that nests it under the listener and the service must carry
/// exactly the suffix the mint produced.
///
/// The rows span the listener the rules hang off (protocol, whether a port is sent, the
/// kind of default action) and, under each, the rules of [`rule_rows`] — so one run mints
/// a dozen rule ids rather than one, which no leading character could satisfy by luck.
///
/// What this adds over [`every_minted_id_and_arn_carries_a_lowercase_hex_suffix`], which
/// already asserts the suffix form of a created rule: the DEFAULT rule, whose id no request
/// names because the listener create mints it, and the three operations that report a stored
/// rule id back — `GetRule`, `UpdateRule` and the `ListRules` summary — each with the ARN
/// that nests the id under its listener and its service.
#[tokio::test]
async fn every_rule_operation_reports_the_minted_rule_suffix() {
    let lattice = Lattice::new();
    // A fixture name carrying no id prefix of this service, so the walker that checks
    // every embedded id cannot mistake a name for one.
    let (service_id, _) = a_service(&lattice, "idform-host").await;

    // Each row is a listener whose create materialises a default rule: a different
    // protocol (`state`'s LISTENER_PROTOCOL_VALUES: HTTP | HTTPS | TLS_PASSTHROUGH), a
    // different port, and a different kind of default action.
    let listeners: Vec<(&str, serde_json::Value)> = vec![
        (
            "HTTP on its documented default port, fixed response",
            serde_json::json!({
                "name": "idform-http",
                "protocol": "HTTP",
                "defaultAction": { "fixedResponse": { "statusCode": 404 } },
            }),
        ),
        (
            "HTTPS on an explicit port, fixed response",
            serde_json::json!({
                "name": "idform-https",
                "protocol": "HTTPS",
                "port": 8443,
                "defaultAction": { "fixedResponse": { "statusCode": 500 } },
            }),
        ),
        (
            "TLS_PASSTHROUGH forwarding to one target group",
            serde_json::json!({
                "name": "idform-passthrough",
                "protocol": "TLS_PASSTHROUGH",
                "port": 9443,
                "defaultAction": {
                    "forward": {
                        "targetGroups": [{ "targetGroupIdentifier": target_group("1") }],
                    },
                },
            }),
        ),
    ];

    let mut minted: Vec<String> = Vec::new();
    for (listener_case, listener_body) in listeners {
        let created = create_listener(&lattice, &service_id, listener_body).await;
        served(listener_case, 201, &created);
        let listener_id = created.str_field("id");
        assert_hex_id(listener_case, &listener_id, state::PREFIX_LISTENER);
        assert_eq!(
            created.str_field("arn"),
            expected_arn(&listener_kind(&service_id), &listener_id),
            "case {listener_case}: the listener ARN must embed the minted id {listener_id:?}, \
             got {}",
            created.body
        );
        let kind = rule_kind(&service_id, &listener_id);

        // The DEFAULT rule, minted by the listener create and never by a rule request: the
        // listing reports it, and `GetRule` reads it back under the same id and ARN.
        let case = format!("{listener_case}, the default rule");
        let default_id = default_rule_id(&lattice, &service_id, &listener_id).await;
        assert_hex_id(&case, &default_id, state::PREFIX_RULE);
        let default_arn = expected_arn(&kind, &default_id);
        let summary = listed_summary(&lattice, &case, &service_id, &listener_id, &default_id).await;
        assert_eq!(
            summary["arn"].as_str(),
            Some(default_arn.as_str()),
            "case {case}: the summary must embed the minted id {default_id:?}, got {summary}"
        );
        let read = get_rule(&lattice, &service_id, &listener_id, &default_id).await;
        served(&case, 200, &read);
        for (member, expected) in [("id", default_id.clone()), ("arn", default_arn.clone())] {
            assert_eq!(
                read.str_field(member),
                expected,
                "case {case}, member {member}: expected {expected:?}, got {}",
                read.body
            );
        }
        minted.push(default_id);

        for (rule_case, body, update) in rule_rows() {
            let case = format!("{listener_case}, {rule_case}");
            let created = create_rule(&lattice, &service_id, &listener_id, body).await;
            served(&case, 201, &created);
            let rule_id = created.str_field("id");
            assert_hex_id(&case, &rule_id, state::PREFIX_RULE);
            let rule_arn = expected_arn(&kind, &rule_id);
            assert_eq!(
                created.str_field("arn"),
                rule_arn,
                "case {case}: the rule ARN must nest under its listener and embed {rule_id:?}, \
                 got {}",
                created.body
            );

            // The read, the update acknowledgement and the listing all report the id the
            // create minted, and the ARN built from it.
            let read = get_rule(&lattice, &service_id, &listener_id, &rule_id).await;
            served(&format!("{case}, read back"), 200, &read);
            let updated =
                update_rule(&lattice, &service_id, &listener_id, &rule_id, update).await;
            served(&format!("{case}, updated"), 200, &updated);
            let summary =
                listed_summary(&lattice, &case, &service_id, &listener_id, &rule_id).await;
            for (reported, from) in [
                (read.str_field("id"), "GetRule id"),
                (updated.str_field("id"), "UpdateRule id"),
            ] {
                assert_eq!(
                    reported, rule_id,
                    "case {case}: {from} must be the minted id {rule_id:?}, got {reported:?}"
                );
            }
            for (reported, from) in [
                (read.str_field("arn"), "GetRule arn"),
                (updated.str_field("arn"), "UpdateRule arn"),
                (
                    summary["arn"]
                        .as_str()
                        .unwrap_or_else(|| {
                            panic!(
                                "case {case}: the ListRules summary of {rule_id:?} must carry \
                                 the required arn member, got {summary}"
                            )
                        })
                        .to_string(),
                    "the ListRules summary arn",
                ),
            ] {
                assert_eq!(
                    reported, rule_arn,
                    "case {case}: {from} must embed the minted id {rule_id:?}, \
                     expected {rule_arn:?}, got {reported:?}"
                );
            }
            minted.push(rule_id);
        }
    }

    // Every mint of the run is distinct: the suffix is a fresh value behind the fixed
    // leading character, not a constant.
    let mut distinct = minted.clone();
    distinct.sort();
    distinct.dedup();
    assert_eq!(
        distinct.len(),
        minted.len(),
        "expected {} distinct rule ids, got {:?}",
        minted.len(),
        minted
    );
}

/// A STORED id keeps the minted suffix on the two paths that report one rather than mint
/// it: the read that echoes a stored resource, and the `resourceId` of the 409 a duplicate
/// NAME is refused with — for a target group and for a listener rule alike.
///
/// What this adds over the cases above, which assert the suffix a create answers with:
/// `GetTargetGroup` and `GetRule` in both modelled identifier forms (the bare id and the
/// ARN as a label), and `ConflictException.resourceId`, which is the id of the resource
/// ALREADY holding the name and so comes from the store rather than from a fresh mint. A
/// base-36 suffix satisfies every modelled pattern (`^tg-[0-9a-z]{17}$`,
/// `^rule-[0-9a-z]{17}$`), so only these assertions pin the narrower form the service
/// hands out.
///
/// The target-group rows are the shared `type`-catalog table of this crate's target-group
/// suite ([`crate::target_groups::type_catalog_rows`]) and the rule rows the shared
/// [`rule_rows`] of this file, so neither catalog is enumerated twice; the name's length
/// is not a dimension of the minted suffix, so the bounds of `TargetGroupName`'s
/// `@length` stay with the suite that tests them.
///
/// The rule half repeats the create and `GetRule` assertions of
/// [`every_rule_operation_reports_the_minted_rule_suffix`] only as the setup its new legs
/// need: the read in the ARN identifier form, and the duplicate-NAME conflict.
#[tokio::test]
async fn reads_and_duplicate_name_conflicts_report_the_minted_hex_suffix() {
    let lattice = Lattice::new();

    for (case, body) in type_catalog_rows("hexform") {
        let created = create_target_group(&lattice, body.clone()).await;
        served(&case, 201, &created);
        let id = created.str_field("id");
        assert_hex_id(&case, &id, state::PREFIX_TARGET_GROUP);
        let arn = created.str_field("arn");
        assert_eq!(
            arn,
            expected_arn(state::KIND_TARGET_GROUP, &id),
            "case {case}: the ARN must embed the minted id {id:?}, got {}",
            created.body
        );

        // Both modelled identifier forms read the stored record back, and each echoes the
        // minted id and the ARN built from it unchanged.
        for (form, identifier) in [("the bare id", id.clone()), ("the ARN", as_label(&arn))] {
            let read_case = format!("{case}, read by {form}");
            let read = get_target_group(&lattice, &identifier).await;
            served(&read_case, 200, &read);
            assert_hex_id(&read_case, &read.str_field("id"), state::PREFIX_TARGET_GROUP);
            for (member, expected) in [("id", id.clone()), ("arn", arn.clone())] {
                assert_eq!(
                    read.str_field(member),
                    expected,
                    "case {read_case}, member {member}: expected the stored {expected:?}, got {}",
                    read.body
                );
            }
        }

        // The name is taken, so a second create under it is refused with the id of the
        // target group that holds it.
        let conflict_case = format!("{case}, the name taken a second time");
        let conflict = create_target_group(&lattice, body.clone()).await;
        served(&conflict_case, 409, &conflict);
        assert_eq!(
            conflict.error_code.as_deref(),
            Some("ConflictException"),
            "case {conflict_case}: expected a ConflictException, got {:?}",
            conflict.error_code
        );
        // `ConflictException` declares `resourceId` and `resourceType` required members
        // (`smithy_query(query="shape",shape="ConflictException")`), and the refusal is
        // the one over the taken NAME: it names the name and the scope it is taken in. The
        // token is the wire literal recorded vpc-lattice errors carry for this kind, not a
        // value read back out of the service.
        assert_eq!(
            conflict.str_field("resourceType"),
            "TARGET_GROUP",
            "case {conflict_case}: expected the target-group resource type, got {}",
            conflict.body
        );
        let taken_name = body["name"]
            .as_str()
            .expect("every row of the catalog carries a name");
        let message = message_of(&conflict_case, &conflict);
        assert!(
            message.contains(taken_name),
            "case {conflict_case}: the refusal must name the taken name {taken_name:?}, \
             got {message:?}"
        );
        let reported = conflict.str_field("resourceId");
        assert_hex_id(&conflict_case, &reported, state::PREFIX_TARGET_GROUP);
        assert_eq!(
            reported, id,
            "case {conflict_case}: resourceId must be the stored target group {id:?}, \
             got {reported:?}"
        );
    }

    // The same on the rule namespace, over the rule table of this file: a rule's name is
    // unique within its listener, so a second create under a taken name reports the stored
    // rule's id. The second create carries a priority no rule of the listener holds, so the
    // refusal under test is the one over the NAME and not the one over the priority.
    let (service_id, _) = a_service(&lattice, "hexform-conflict-host").await;
    let listener_id = a_listener(&lattice, &service_id, "hexform-conflict-listener").await;
    for (row, (case, body, _)) in rule_rows().into_iter().enumerate() {
        let created = create_rule(&lattice, &service_id, &listener_id, body.clone()).await;
        served(case, 201, &created);
        let id = created.str_field("id");
        assert_hex_id(case, &id, state::PREFIX_RULE);
        let arn = created.str_field("arn");
        assert_eq!(
            arn,
            expected_arn(&rule_kind(&service_id, &listener_id), &id),
            "case {case}: the rule ARN must nest under its listener and embed {id:?}, got {}",
            created.body
        );

        // The same read-back as the target-group half: both modelled identifier forms
        // report the stored id and the ARN built from it.
        for (form, identifier) in [("the bare id", id.clone()), ("the ARN", as_label(&arn))] {
            let read_case = format!("{case}, read by {form}");
            let read = get_rule(&lattice, &service_id, &listener_id, &identifier).await;
            served(&read_case, 200, &read);
            assert_hex_id(&read_case, &read.str_field("id"), state::PREFIX_RULE);
            for (member, expected) in [("id", id.clone()), ("arn", arn.clone())] {
                assert_eq!(
                    read.str_field(member),
                    expected,
                    "case {read_case}, member {member}: expected the stored {expected:?}, got {}",
                    read.body
                );
            }
        }

        let conflict_case = format!("{case}, the name taken a second time");
        let taken_name = body["name"]
            .as_str()
            .expect("every row of the rule table carries a name")
            .to_string();
        // A priority distinct per row and held by no row of the table, so the second
        // create collides on the NAME alone; the table is the authority on which
        // priorities are taken, so the choice is checked against it rather than assumed.
        let free_priority = 1500 + i64::try_from(row).expect("the row count fits an i64");
        assert!(
            rule_rows()
                .iter()
                .all(|(_, row_body, _)| row_body["priority"] != serde_json::json!(free_priority)),
            "case {case}: priority {free_priority} is held by a row of the rule table, so a \
             second create under it would collide on the priority instead of the name"
        );
        let conflict = create_rule(
            &lattice,
            &service_id,
            &listener_id,
            serde_json::json!({
                "name": taken_name,
                "priority": free_priority,
                "match": { "httpMatch": {} },
                "action": { "fixedResponse": { "statusCode": 404 } },
            }),
        )
        .await;
        served(&conflict_case, 409, &conflict);
        assert_eq!(
            conflict.error_code.as_deref(),
            Some("ConflictException"),
            "case {conflict_case}: expected a ConflictException, got {:?}",
            conflict.error_code
        );
        assert_eq!(
            conflict.str_field("resourceType"),
            "RULE",
            "case {conflict_case}: expected the rule resource type, got {}",
            conflict.body
        );
        // The refusal over the taken name names the name and the listener it is taken in,
        // which is how it differs from the refusal over a taken priority.
        let message = message_of(&conflict_case, &conflict);
        assert!(
            message.contains(&taken_name) && message.contains(&listener_id),
            "case {conflict_case}: the refusal must name {taken_name:?} and the listener \
             {listener_id:?}, got {message:?}"
        );
        let reported = conflict.str_field("resourceId");
        assert_hex_id(&conflict_case, &reported, state::PREFIX_RULE);
        assert_eq!(
            reported, id,
            "case {conflict_case}: resourceId must be the stored rule {id:?}, got {reported:?}"
        );
    }
}
