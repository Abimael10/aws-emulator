// SPDX-License-Identifier: CC0-1.0

//! The ORDER this service states a COLLECTION in — in a constraint refusal, and in a
//! response member that is a map.
//!
//! One rule, over the two collections a clause of this service embeds: a collection stated
//! inside a constraint-violation message is listed in the order the TABLE holding it is
//! walked — bucket by bucket, the bucket being the low bits of each element's 31-based
//! string hash mixed with its own high half — and never sorted, nor in the order the request
//! wrote it. The two collections are:
//!
//! * the values an enumerated type admits, named by a clause about a value outside that
//!   type;
//! * the entries of a MAP the request submitted, quoted by a clause about that map member.
//!
//! The order is a function of the element TEXT and, for a map, of the entry COUNT (which
//! decides the table's width) — so it is the same on every run and for every member
//! carrying the same collection, which is why it is asserted here rather than taken from
//! one recorded response.
//!
//! The second half of the suite is the same question about a RESPONSE: a response member
//! that is a map reports the entries of the stored map in ONE order, decided by that stored
//! map alone. Two reads of a resource nothing changed write the member's entries in the
//! same order, every operation reporting the same stored map writes them in that order
//! too, and the order a request happened to write its own map in does not move it. WHICH
//! order it is, is not asserted: that it is one order is the rule, and a reader answering a
//! different sequence each time reports a map no caller can diff, cache or compare against
//! a previous read.
//!
//! Every bucket below is arrived at from the rule, not from this emulator's output: a key of
//! one ASCII character hashes to that character's own code point and one of two characters
//! to `31 * <first> + <second>`, neither of which reaches the high half of the hash, so each
//! bucket is that number masked to the table's width. The arithmetic is spelled out beside
//! each table.
//!
//! The harness is `identity_pool_flow`'s: one shared engine behind the same
//! `MultiServiceRouter` the production router builds.

use serde_json::{json, Value};

use crate::identity_pool_flow::{error_code, Emulator};

/// The error code the constraint layer ahead of every handler refuses with — the protocol's
/// own, which no operation of this service declares.
const VALIDATION_EXCEPTION: &str = "ValidationException";

/// The message a refusal states, which every refusal has to carry.
fn message(body: &Value, case: &str) -> String {
    body.get("message")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected a message, got {body}"))
        .to_string()
}

/// The modelled maximum of `SetPrincipalTagAttributeMap$PrincipalTags`
/// (`smithy_query(query="shape",shape="PrincipalTags")`: `length 0..50`).
const PRINCIPAL_TAGS_MAX: usize = 50;

/// The map the rows below submit: one entry per key, all carrying `value`.
fn tags(keys: &[&str], value: &str) -> Value {
    Value::Object(
        keys.iter()
            .map(|key| ((*key).to_string(), json!(value)))
            .collect(),
    )
}

/// RULE: a clause quoting a MAP member states the entries bucket by bucket of the table
/// holding them — not sorted by key — with two keys of one bucket in the order they were
/// submitted.
#[tokio::test]
async fn a_clause_about_a_map_states_its_entries_in_the_order_its_table_is_walked() {
    let emulator = Emulator::new();
    let pool_id = emulator
        .pool_with_providers("stated-order-tags", true, &["graph.facebook.com"])
        .await;

    // 51 keys — one past the modelled maximum, so the map's own `@length` is what refuses
    // the call — of two characters each, whose buckets in the 128-bucket table that many
    // entries are held in are `(31 * <letter> + <digit>) & 127` xor `1680 & 127 = 16`
    // (`1680` being the high half of every such hash):
    //
    //   a0..a9 -> 111..120    b0..b9 ->  14..23    c0..c9 ->  45..54
    //   d0..d9 ->  76..85     f0..f9 ->  10..19    g0     ->  41
    //
    // so 'b' and 'f' share the six buckets 14..19, where the two keys of one bucket are
    // stated in key order.
    let keys: Vec<String> = ["a", "b", "c", "d", "f"]
        .iter()
        .flat_map(|letter| (0..10).map(move |digit| format!("{letter}{digit}")))
        .chain(std::iter::once("g0".to_string()))
        .collect();
    let submitted: Vec<&str> = keys.iter().map(String::as_str).collect();
    assert_eq!(
        submitted.len(),
        PRINCIPAL_TAGS_MAX + 1,
        "this case needs one entry past the modelled maximum, got {}",
        submitted.len()
    );
    let stated = [
        "f0", "f1", "f2", "f3", "b0", "f4", "b1", "f5", "b2", "f6", "b3", "f7", "b4", "f8", "b5",
        "f9", "b6", "b7", "b8", "b9", "g0", "c0", "c1", "c2", "c3", "c4", "c5", "c6", "c7", "c8",
        "c9", "d0", "d1", "d2", "d3", "d4", "d5", "d6", "d7", "d8", "d9", "a0", "a1", "a2", "a3",
        "a4", "a5", "a6", "a7", "a8", "a9",
    ];

    // The rows differ along the VALUE the entries carry: the order follows the keys and the
    // entry count, so it does not move when the values do.
    for (case, value) in [
        ("fifty-one tags", "sub"),
        ("the same keys carrying another value", "custom:department"),
    ] {
        let quoted = format!(
            "{{{}}}",
            stated
                .iter()
                .map(|key| format!("{key}={value}"))
                .collect::<Vec<String>>()
                .join(", ")
        );
        let (status, body) = emulator
            .call(
                "SetPrincipalTagAttributeMap",
                &json!({
                    "IdentityPoolId": &pool_id,
                    "IdentityProviderName": "graph.facebook.com",
                    "PrincipalTags": tags(&submitted, value),
                }),
            )
            .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {body}"
        );
        assert_eq!(
            error_code(&body),
            VALIDATION_EXCEPTION,
            "case {case:?}: expected {VALIDATION_EXCEPTION}, got {body}"
        );
        assert_eq!(
            message(&body, case),
            format!(
                "1 validation error detected: Value '{quoted}' at 'principalTags' failed to \
                 satisfy constraint: Member must have length less than or equal to \
                 {PRINCIPAL_TAGS_MAX}"
            ),
            "case {case:?}: expected the map stated bucket by bucket, got {body}"
        );
    }
}

/// An IAM role ARN of this emulator's account, so a role mapping is refused for the thing a
/// case is about rather than for its role.
fn role_arn(name: &str) -> String {
    format!("arn:aws:iam::{}:role/{name}", engine::emulator_account_id())
}

/// RULE: a clause about a value outside an enumerated type names that type's admissible
/// values bucket by bucket of the table holding them — not sorted, and not in the order the
/// model declares them.
#[tokio::test]
async fn a_clause_about_an_enumerated_type_states_its_values_in_the_order_its_table_is_walked() {
    let emulator = Emulator::new();
    let pool_id = emulator.pool("stated-order-enums", true).await;
    let provider = "graph.facebook.com";
    let mapped = role_arn("Cognito_statedOrderMapped_Role");

    // The three enumerated types a `SetIdentityPoolRoles` request carries, each with the
    // bucket its values fall in. A value's bucket is its 31-based hash reduced to the
    // 32-bucket table of admissible values, which for these values is the alternating sum of
    // their UTF-16 code units (31 ≡ -1, so every other unit is subtracted), taken from the
    // last unit:
    //
    //   MappingRuleMatchType          Equals 31, Contains 31, StartsWith 23, NotEqual 1
    //   RoleMappingType               Token 25, Rules 23
    //   AmbiguousRoleResolutionType   AuthenticatedRole 5, Deny 12
    //
    // `Equals` and `Contains` share bucket 31, where the chain is read from the head each
    // value was pushed onto, so the later-declared `Contains` is stated first.
    //
    // (case, the member path the clause names, the rejected value, the request, the value
    // set as stated)
    let rows: Vec<(&str, &str, &str, Value, &str)> = vec![
        (
            "a rule's MatchType outside MappingRuleMatchType",
            "roleMappings.graph.facebook.com.member.rulesConfiguration.rules.1.member.matchType",
            "Matches",
            json!({
                provider: {
                    "Type": "Rules",
                    "AmbiguousRoleResolution": "Deny",
                    "RulesConfiguration": {"Rules": [{
                        "Claim": "isAdmin",
                        "MatchType": "Matches",
                        "Value": "yes",
                        "RoleARN": &mapped,
                    }]},
                },
            }),
            "NotEqual, StartsWith, Contains, Equals",
        ),
        (
            "a mapping's Type outside RoleMappingType",
            "roleMappings.graph.facebook.com.member.type",
            "RulesEngine",
            json!({
                provider: {"Type": "RulesEngine", "AmbiguousRoleResolution": "Deny"},
            }),
            "Rules, Token",
        ),
        (
            "a mapping's AmbiguousRoleResolution outside AmbiguousRoleResolutionType",
            "roleMappings.graph.facebook.com.member.ambiguousRoleResolution",
            "Allow",
            json!({
                provider: {"Type": "Token", "AmbiguousRoleResolution": "Allow"},
            }),
            "AuthenticatedRole, Deny",
        ),
    ];

    for (case, path, rejected, role_mappings, set) in rows {
        let (status, body) = emulator
            .call(
                "SetIdentityPoolRoles",
                &json!({
                    "IdentityPoolId": &pool_id,
                    "Roles": {"authenticated": role_arn("Cognito_statedOrderAuth_Role")},
                    "RoleMappings": role_mappings,
                }),
            )
            .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {body}"
        );
        assert_eq!(
            error_code(&body),
            VALIDATION_EXCEPTION,
            "case {case:?}: expected {VALIDATION_EXCEPTION}, got {body}"
        );
        assert_eq!(
            message(&body, case),
            format!(
                "1 validation error detected: Value '{rejected}' at '{path}' failed to satisfy \
                 constraint: Member must satisfy enum value set: [{set}]"
            ),
            "case {case:?}: expected the value set stated bucket by bucket, got {body}"
        );
    }

    // A value INSIDE each of those types is served, so the refusals above are about the
    // value being outside the type and not about the request's shape.
    let (status, body) = emulator
        .call(
            "SetIdentityPoolRoles",
            &json!({
                "IdentityPoolId": &pool_id,
                "Roles": {"authenticated": role_arn("Cognito_statedOrderAuth_Role")},
                "RoleMappings": {
                    provider: {
                        "Type": "Rules",
                        "AmbiguousRoleResolution": "AuthenticatedRole",
                        "RulesConfiguration": {"Rules": [{
                            "Claim": "isAdmin",
                            "MatchType": "Equals",
                            "Value": "yes",
                            "RoleARN": &mapped,
                        }]},
                    },
                },
            }),
        )
        .await;
    assert_eq!(
        status, 200,
        "case \"every value inside its type\": expected HTTP 200, got {status} with {body}"
    );
}

/// RULE: a role mapping that states no `AmbiguousRoleResolution` is refused with the
/// sentence AWS words that condition with, over the `Type` that requires it and nothing else
/// of the request.
#[tokio::test]
async fn a_mapping_with_no_ambiguous_role_resolution_names_the_type_that_requires_it() {
    let emulator = Emulator::new();
    let pool_id = emulator.pool("stated-order-ambiguous", true).await;
    let mapped = role_arn("Cognito_statedOrderMapped_Role");

    // The rows span both values of `RoleMappingType` — the model's
    // `RoleMapping$AmbiguousRoleResolution` states "If you specify Token or Rules as the
    // Type, AmbiguousRoleResolution is required" — and, within `Rules`, the provider the
    // mapping is keyed by and how many mappings the call carries, which the sentence names
    // nothing of.
    let rows: Vec<(&str, &str, Value)> = vec![
        (
            "a Token mapping",
            "Token",
            json!({"graph.facebook.com": {"Type": "Token"}}),
        ),
        (
            "a Rules mapping",
            "Rules",
            json!({
                "graph.facebook.com": {
                    "Type": "Rules",
                    "RulesConfiguration": {"Rules": [{
                        "Claim": "isAdmin",
                        "MatchType": "Equals",
                        "Value": "yes",
                        "RoleARN": &mapped,
                    }]},
                },
            }),
        ),
        (
            "a Rules mapping keyed by a user pool, beside a complete one",
            "Rules",
            json!({
                "accounts.google.com": {
                    "Type": "Token",
                    "AmbiguousRoleResolution": "Deny",
                },
                "cognito-idp.us-east-1.amazonaws.com/us-east-1_stated1:statedclient": {
                    "Type": "Rules",
                    "RulesConfiguration": {"Rules": [{
                        "Claim": "isAdmin",
                        "MatchType": "Equals",
                        "Value": "yes",
                        "RoleARN": &mapped,
                    }]},
                },
            }),
        ),
    ];

    for (case, role_mapping_type, role_mappings) in rows {
        let (status, body) = emulator
            .call(
                "SetIdentityPoolRoles",
                &json!({
                    "IdentityPoolId": &pool_id,
                    "Roles": {"authenticated": role_arn("Cognito_statedOrderAuth_Role")},
                    "RoleMappings": role_mappings,
                }),
            )
            .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {body}"
        );
        assert_eq!(
            error_code(&body),
            "InvalidParameterException",
            "case {case:?}: expected InvalidParameterException, got {body}"
        );
        assert_eq!(
            message(&body, case),
            format!("AmbiguousRoleResolution must be specified for {role_mapping_type}"),
            "case {case:?}: expected the sentence over the Type, got {body}"
        );
    }
}

// --- the order a RESPONSE writes a map member's entries in -------------------------------
//
// Everything below drives the service through the same harness, with one difference: a
// request body is handed over as TEXT and the response is read as TEXT
// (`Emulator::ok_text`), so a case decides the order its own map members are written in and
// the order the SERVICE wrote is the one asserted, rather than the order a JSON parser kept.

/// One map's entries as a request writes them: `(key, the value as JSON text)`.
type Entries = Vec<(String, String)>;

/// One row of the pool-member table: the case, its `SupportedLoginProviders` and its
/// `IdentityPoolTags`, each in the order the request writes them.
type PoolRow<'a> = (&'a str, Entries, Entries);

/// One JSON string, as a request writes it. Every key and value below is spelled with
/// characters JSON carries unescaped.
fn quoted(text: &str) -> String {
    format!("\"{text}\"")
}

/// A JSON object of `entries`, written in exactly the order given. Each entry's value is
/// JSON text already, so an entry can carry a string or a nested object.
fn json_object(entries: &[(String, String)]) -> String {
    let members: Vec<String> = entries
        .iter()
        .map(|(key, value)| format!("{}:{value}", quoted(key)))
        .collect();
    format!("{{{}}}", members.join(","))
}

/// `entries` the other way round, for the call that submits the same map reversed.
fn reversed(entries: &[(String, String)]) -> Entries {
    let mut reversed = entries.to_vec();
    reversed.reverse();
    reversed
}

/// The keys of `entries`, in the order they are written.
fn member_keys(entries: &[(String, String)]) -> Vec<String> {
    entries.iter().map(|(key, _)| key.clone()).collect()
}

/// One map entry carrying a plain string value.
fn string_entry(key: &str, value: &str) -> (String, String) {
    (key.to_string(), quoted(value))
}

/// A scalar member of a response, read through a parser — only the ORDER of a map member is
/// read off the text.
fn scalar(text: &str, member: &str, case: &str) -> String {
    let parsed: Value = serde_json::from_str(text)
        .unwrap_or_else(|error| panic!("case {case:?}: expected JSON ({error}): {text}"));
    parsed[member]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected the response to carry {member}: {text}"))
        .to_string()
}

/// The order `member`'s entries are written in within the response `text`: each key's
/// position is taken from the text after the member's own name, so the sequence is the one
/// the service serialized.
///
/// A key the member does not carry fails the read itself, which is what also makes this the
/// assertion that the member reports the whole stored map.
fn reported_order(text: &str, member: &str, expected_keys: &[String], case: &str) -> Vec<String> {
    let member_at = text.find(&quoted(member)).unwrap_or_else(|| {
        panic!("case {case:?}: expected the response to carry {member}, got {text}")
    });
    let written = &text[member_at..];
    let mut placed: Vec<(usize, String)> = expected_keys
        .iter()
        .map(|key| {
            let at = written.find(&quoted(key)).unwrap_or_else(|| {
                panic!("case {case:?}: expected {member} to carry the entry {key:?}, got {text}")
            });
            (at, key.clone())
        })
        .collect();
    placed.sort_by_key(|(at, _)| *at);
    placed.into_iter().map(|(_, key)| key).collect()
}

/// Assert every observation of `member` reports the one order the first observation
/// reported. Each observation is `(what reported it, the order it reported)`.
fn one_order(case: &str, member: &str, observed: &[(String, Vec<String>)]) {
    let (first_from, first) = &observed[0];
    for (from, order) in &observed[1..] {
        assert_eq!(
            order, first,
            "case {case:?}: {member} reported by {from} expected the order {first:?} that \
             {first_from} reported, got {order:?}"
        );
    }
}

/// The public providers an identity pool declares under `SupportedLoginProviders`, as AWS's
/// own documentation of that member names them. The member is modelled `length 0..10`
/// (`smithy_query(query="shape",shape="IdentityProviders")`), so no row asks for more.
const PUBLIC_PROVIDERS: &[&str] = &[
    "graph.facebook.com",
    "accounts.google.com",
    "www.amazon.com",
    "appleid.apple.com",
    "api.twitter.com",
];

/// `count` entries of a pool's `SupportedLoginProviders`, carrying app ids the value shape
/// admits (`IdentityProviderId`, pattern `^[\w.;_/-]+$`, from the same query).
fn provider_entries(count: usize) -> Entries {
    assert!(
        count <= PUBLIC_PROVIDERS.len(),
        "this suite names {} providers, asked for {count}",
        PUBLIC_PROVIDERS.len()
    );
    PUBLIC_PROVIDERS
        .iter()
        .take(count)
        .enumerate()
        .map(|(index, provider)| string_entry(provider, &format!("app-id-{index}")))
        .collect()
}

/// `count` tag entries of a pool, keyed so their sorted order is neither the order they are
/// generated in nor its reverse.
fn pool_tag_entries(count: usize) -> Entries {
    (0..count)
        .map(|index| {
            string_entry(
                &format!("tag-{}-{index}", (b'a' + (index as u8 * 7) % 26) as char),
                &format!("value-{index}"),
            )
        })
        .collect()
}

/// `count` principal-tag entries — a claim mapped onto a session tag — keyed the same way.
/// `PrincipalTags` is modelled `length 0..50`
/// (`smithy_query(query="shape",shape="PrincipalTags")`).
fn principal_tag_entries(count: usize) -> Entries {
    (0..count)
        .map(|index| {
            string_entry(
                &format!("principal-{}-{index}", (b'a' + (index as u8 * 5) % 26) as char),
                &format!("claim-{index}"),
            )
        })
        .collect()
}

/// The `CreateIdentityPool` / `UpdateIdentityPool` request for one configuration, with each
/// map member written in the order the row gives it.
fn pool_request(
    pool_id: Option<&str>,
    name: &str,
    providers: &[(String, String)],
    pool_tags: &[(String, String)],
) -> String {
    let mut members: Vec<String> = Vec::new();
    if let Some(pool_id) = pool_id {
        members.push(format!("\"IdentityPoolId\":{}", quoted(pool_id)));
    }
    members.push(format!("\"IdentityPoolName\":{}", quoted(name)));
    members.push("\"AllowUnauthenticatedIdentities\":true".to_string());
    members.push(format!(
        "\"SupportedLoginProviders\":{}",
        json_object(providers)
    ));
    members.push(format!("\"IdentityPoolTags\":{}", json_object(pool_tags)));
    format!("{{{}}}", members.join(","))
}

/// RULE: the two map members of the `IdentityPool` shape are reported in one order — by the
/// call that stored the configuration, by every read of it afterwards, and by a later call
/// that submits the very same entries the other way round.
#[tokio::test]
async fn a_pool_reports_its_map_members_in_one_order_on_every_read() {
    let emulator = Emulator::new();

    // The rows differ in the COUNT of entries each member carries (one, several, and the
    // modelled maximum of `SupportedLoginProviders`) and in the order the request writes
    // them in — the two members of a row are written in opposite directions, so no single
    // submitted order can be the one both responses report.
    let rows: Vec<PoolRow<'_>> = vec![
        (
            "one provider and one tag",
            provider_entries(1),
            pool_tag_entries(1),
        ),
        (
            "three providers submitted ascending, eight tags submitted descending",
            provider_entries(3),
            reversed(&pool_tag_entries(8)),
        ),
        (
            "five providers submitted descending, twelve tags submitted ascending",
            reversed(&provider_entries(5)),
            pool_tag_entries(12),
        ),
    ];

    for (index, (case, providers, pool_tags)) in rows.into_iter().enumerate() {
        let name = format!("map-order-pool-{index}");
        let created = emulator
            .ok_text(
                "CreateIdentityPool",
                &pool_request(None, &name, &providers, &pool_tags),
            )
            .await;
        let pool_id = scalar(&created, "IdentityPoolId", case);
        let read = format!("{{\"IdentityPoolId\":{}}}", quoted(&pool_id));

        let mut provider_orders = vec![(
            "CreateIdentityPool".to_string(),
            reported_order(
                &created,
                "SupportedLoginProviders",
                &member_keys(&providers),
                case,
            ),
        )];
        let mut tag_orders = vec![(
            "CreateIdentityPool".to_string(),
            reported_order(&created, "IdentityPoolTags", &member_keys(&pool_tags), case),
        )];

        // Two reads of a pool nothing changed, then a call storing the same two maps written
        // the other way round, then a read after it.
        for pass in 1..=2 {
            let reported = emulator.ok_text("DescribeIdentityPool", &read).await;
            provider_orders.push((
                format!("DescribeIdentityPool read {pass}"),
                reported_order(
                    &reported,
                    "SupportedLoginProviders",
                    &member_keys(&providers),
                    case,
                ),
            ));
            tag_orders.push((
                format!("DescribeIdentityPool read {pass}"),
                reported_order(&reported, "IdentityPoolTags", &member_keys(&pool_tags), case),
            ));
        }

        let updated = emulator
            .ok_text(
                "UpdateIdentityPool",
                &pool_request(
                    Some(&pool_id),
                    &name,
                    &reversed(&providers),
                    &reversed(&pool_tags),
                ),
            )
            .await;
        provider_orders.push((
            "UpdateIdentityPool, submitted reversed".to_string(),
            reported_order(
                &updated,
                "SupportedLoginProviders",
                &member_keys(&providers),
                case,
            ),
        ));
        tag_orders.push((
            "UpdateIdentityPool, submitted reversed".to_string(),
            reported_order(&updated, "IdentityPoolTags", &member_keys(&pool_tags), case),
        ));

        let after = emulator.ok_text("DescribeIdentityPool", &read).await;
        provider_orders.push((
            "DescribeIdentityPool after the reversed call".to_string(),
            reported_order(
                &after,
                "SupportedLoginProviders",
                &member_keys(&providers),
                case,
            ),
        ));
        tag_orders.push((
            "DescribeIdentityPool after the reversed call".to_string(),
            reported_order(&after, "IdentityPoolTags", &member_keys(&pool_tags), case),
        ));

        one_order(case, "SupportedLoginProviders", &provider_orders);
        one_order(case, "IdentityPoolTags", &tag_orders);
    }
}

/// The `Roles` map, written in the order given: `RolesMap`'s key shape admits the two role
/// types alone (`RoleType`, pattern `^(un)?authenticated$`, from
/// `smithy_query(query="shape",shape="RolesMap")`), so a row can only differ in which of
/// them the request writes first.
fn role_entries(name: &str, authenticated_first: bool) -> Entries {
    let entries = vec![
        string_entry("authenticated", &role_arn(&format!("{name}-auth"))),
        string_entry("unauthenticated", &role_arn(&format!("{name}-unauth"))),
    ];
    if authenticated_first {
        entries
    } else {
        reversed(&entries)
    }
}

/// `count` complete `Token` role mappings, keyed by the identity provider each resolves a
/// role for — spanning both forms a mapping is keyed by: a public provider's own name, and a
/// user-pool provider with its app client id. `RoleMappingMap` is modelled `length 0..10`
/// (`smithy_query(query="shape",shape="RoleMappingMap")`).
fn mapping_entries(count: usize) -> Entries {
    (0..count)
        .map(|index| {
            let provider = if index % 2 == 0 {
                format!("provider-{index}.example.com")
            } else {
                format!("cognito-idp.us-east-1.amazonaws.com/us-east-1_Pool{index}:client{index}")
            };
            let resolution = if index % 2 == 0 {
                "AuthenticatedRole"
            } else {
                "Deny"
            };
            (
                provider,
                format!(
                    "{{\"Type\":\"Token\",\"AmbiguousRoleResolution\":{}}}",
                    quoted(resolution)
                ),
            )
        })
        .collect()
}

/// The `SetIdentityPoolRoles` request for one configuration, each map written in the order
/// the row gives it.
fn set_roles_request(
    pool_id: &str,
    roles: &[(String, String)],
    mappings: &[(String, String)],
) -> String {
    format!(
        "{{\"IdentityPoolId\":{},\"Roles\":{},\"RoleMappings\":{}}}",
        quoted(pool_id),
        json_object(roles),
        json_object(mappings)
    )
}

/// RULE, over the role configuration's two maps: `GetIdentityPoolRoles` reports `Roles` and
/// `RoleMappings` in one order on every read of a configuration nothing changed, and storing
/// the same entries the other way round does not move it.
#[tokio::test]
async fn a_pools_role_configuration_reports_its_maps_in_one_order_on_every_read() {
    let emulator = Emulator::new();

    // The rows differ in the COUNT of mapping entries (one, a few, and the modelled
    // maximum) and in which role type the `Roles` map is written first in.
    let rows: Vec<(&str, bool, usize)> = vec![
        ("one mapping, authenticated written first", true, 1),
        ("four mappings, unauthenticated written first", false, 4),
        ("ten mappings, the modelled maximum", true, 10),
    ];

    for (index, (case, authenticated_first, mappings)) in rows.into_iter().enumerate() {
        let name = format!("map-order-roles-{index}");
        let pool_id = emulator.pool(&name, true).await;

        let roles = role_entries(&name, authenticated_first);
        let mappings = mapping_entries(mappings);
        emulator
            .ok_text(
                "SetIdentityPoolRoles",
                &set_roles_request(&pool_id, &roles, &mappings),
            )
            .await;

        let read = format!("{{\"IdentityPoolId\":{}}}", quoted(&pool_id));
        let mut role_orders: Vec<(String, Vec<String>)> = Vec::new();
        let mut mapping_orders: Vec<(String, Vec<String>)> = Vec::new();
        for pass in 1..=2 {
            let reported = emulator.ok_text("GetIdentityPoolRoles", &read).await;
            role_orders.push((
                format!("GetIdentityPoolRoles read {pass}"),
                reported_order(&reported, "Roles", &member_keys(&roles), case),
            ));
            mapping_orders.push((
                format!("GetIdentityPoolRoles read {pass}"),
                reported_order(&reported, "RoleMappings", &member_keys(&mappings), case),
            ));
        }

        // The same configuration submitted the other way round: a read reports the entries
        // in the same order, because the order is the stored map's and not the request's.
        emulator
            .ok_text(
                "SetIdentityPoolRoles",
                &set_roles_request(&pool_id, &reversed(&roles), &reversed(&mappings)),
            )
            .await;
        let after = emulator.ok_text("GetIdentityPoolRoles", &read).await;
        role_orders.push((
            "GetIdentityPoolRoles after the reversed call".to_string(),
            reported_order(&after, "Roles", &member_keys(&roles), case),
        ));
        mapping_orders.push((
            "GetIdentityPoolRoles after the reversed call".to_string(),
            reported_order(&after, "RoleMappings", &member_keys(&mappings), case),
        ));

        one_order(case, "Roles", &role_orders);
        one_order(case, "RoleMappings", &mapping_orders);
    }
}

/// RULE, over a provider's attribute map and a pool's tags: every operation that reports one
/// of them writes its entries in one order, whichever operation stored the map and whichever
/// order that call wrote it in.
#[tokio::test]
async fn a_providers_attribute_map_and_a_pools_tags_report_in_one_order() {
    let emulator = Emulator::new();
    let provider = PUBLIC_PROVIDERS[0];

    // The rows differ in the COUNT of entries each map carries and in whether the request
    // writes them in the order the keys sort in or the other way round.
    let rows: Vec<(&str, usize, usize, bool)> = vec![
        ("one principal tag and one pool tag", 1, 1, false),
        ("six principal tags and nine pool tags", 6, 9, true),
        ("fifteen principal tags and three pool tags", 15, 3, false),
    ];

    for (index, (case, principal_tags, pool_tags, submit_reversed)) in rows.into_iter().enumerate()
    {
        let name = format!("map-order-tags-{index}");
        let created = emulator
            .ok_text(
                "CreateIdentityPool",
                &pool_request(None, &name, &provider_entries(1), &[]),
            )
            .await;
        let pool_id = scalar(&created, "IdentityPoolId", case);
        let pool_arn = scalar(&created, "IdentityPoolArn", case);

        let principal = principal_tag_entries(principal_tags);
        let submitted_principal = if submit_reversed {
            reversed(&principal)
        } else {
            principal.clone()
        };
        let stored = emulator
            .ok_text(
                "SetPrincipalTagAttributeMap",
                &format!(
                    "{{\"IdentityPoolId\":{},\"IdentityProviderName\":{},\"PrincipalTags\":{}}}",
                    quoted(&pool_id),
                    quoted(provider),
                    json_object(&submitted_principal)
                ),
            )
            .await;
        let mut principal_orders = vec![(
            "SetPrincipalTagAttributeMap".to_string(),
            reported_order(&stored, "PrincipalTags", &member_keys(&principal), case),
        )];
        let read_mapping = format!(
            "{{\"IdentityPoolId\":{},\"IdentityProviderName\":{}}}",
            quoted(&pool_id),
            quoted(provider)
        );
        for pass in 1..=2 {
            let reported = emulator
                .ok_text("GetPrincipalTagAttributeMap", &read_mapping)
                .await;
            principal_orders.push((
                format!("GetPrincipalTagAttributeMap read {pass}"),
                reported_order(&reported, "PrincipalTags", &member_keys(&principal), case),
            ));
        }
        let re_stored = emulator
            .ok_text(
                "SetPrincipalTagAttributeMap",
                &format!(
                    "{{\"IdentityPoolId\":{},\"IdentityProviderName\":{},\"PrincipalTags\":{}}}",
                    quoted(&pool_id),
                    quoted(provider),
                    json_object(&reversed(&submitted_principal))
                ),
            )
            .await;
        principal_orders.push((
            "SetPrincipalTagAttributeMap, submitted reversed".to_string(),
            reported_order(&re_stored, "PrincipalTags", &member_keys(&principal), case),
        ));
        one_order(case, "PrincipalTags", &principal_orders);

        // A pool's tags are reported by two operations — the tagging read and the pool read
        // — so one order covers both.
        let stored_tags = pool_tag_entries(pool_tags);
        let submitted_tags = if submit_reversed {
            reversed(&stored_tags)
        } else {
            stored_tags.clone()
        };
        emulator
            .ok_text(
                "TagResource",
                &format!(
                    "{{\"ResourceArn\":{},\"Tags\":{}}}",
                    quoted(&pool_arn),
                    json_object(&submitted_tags)
                ),
            )
            .await;
        let list = format!("{{\"ResourceArn\":{}}}", quoted(&pool_arn));
        let mut tag_orders: Vec<(String, Vec<String>)> = Vec::new();
        for pass in 1..=2 {
            let reported = emulator.ok_text("ListTagsForResource", &list).await;
            tag_orders.push((
                format!("ListTagsForResource read {pass}"),
                reported_order(&reported, "Tags", &member_keys(&stored_tags), case),
            ));
        }
        let described = emulator
            .ok_text(
                "DescribeIdentityPool",
                &format!("{{\"IdentityPoolId\":{}}}", quoted(&pool_id)),
            )
            .await;
        tag_orders.push((
            "DescribeIdentityPool".to_string(),
            reported_order(
                &described,
                "IdentityPoolTags",
                &member_keys(&stored_tags),
                case,
            ),
        ));
        one_order(case, "the tags of a pool", &tag_orders);
    }
}
