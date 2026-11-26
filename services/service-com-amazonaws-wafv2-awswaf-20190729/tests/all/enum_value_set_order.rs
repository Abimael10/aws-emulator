// SPDX-License-Identifier: CC0-1.0

//! The ORDER the request validator lists an enumerated member's value set in, end to
//! end over the wire.
//!
//! The rule asserted here: a value outside an enumerated member's set is refused with
//! HTTP 400 `ValidationException` and the validator's own clause, which lists that
//! enum's values in the order the SERVICE declares the enum in — for the
//! oversize-handling set the two inspection decisions first and the value that keeps
//! inspecting last, and for every other enum the order the model declares it in. The
//! bracketed list is part of a documented, client-visible string, so it is the same
//! list wherever the enum appears: every member, every nesting depth and every
//! operation that takes the same enum states it the same way.
//!
//! Every call goes through the same front door a real client uses (the `Waf` harness
//! in [`crate::web_acl_lifecycle`]), so what these tests observe is what an SDK
//! observes.

use serde_json::{json, Value};

use crate::rule_group_lifecycle::create_body;
use crate::web_acl_lifecycle::{create_web_acl, ordered, rule, visibility, Waf};

/// The awsJson error code of the request validator's own refusal.
const VALIDATION_EXCEPTION: &str = "ValidationException";

/// A value no enum of this service carries, so every row below is refused for the
/// same reason: the value is outside the set.
const OUTSIDE_EVERY_SET: &str = "NOT_A_REAL_VALUE";

/// The oversize-handling set as WAF lists it.
const OVERSIZE_HANDLING_SET: &str = "MATCH, NO_MATCH, CONTINUE";

/// One member carrying a value outside its enum's set.
struct Case {
    /// What the case is called in an assertion.
    case: &'static str,
    /// The operation the request is sent to.
    operation: &'static str,
    /// The request body, with exactly one member outside its set.
    request: Value,
    /// The lowerCamel path the clause states the member at, with a list element's
    /// position 1-based the way the validator renders it.
    path: &'static str,
    /// The bracketed value set the clause must list, in WAF's order for that enum.
    set: &'static str,
}

/// A `ByteMatchStatement` whose `FieldToMatch` is `field`.
fn byte_match_over(field: Value) -> Value {
    json!({
        "ByteMatchStatement": {
            "SearchString": "YQ==",
            "FieldToMatch": field,
            "TextTransformations": [{ "Priority": 0, "Type": "NONE" }],
            "PositionalConstraint": "CONTAINS",
        }
    })
}

/// The members every enumerated row is driven through.
///
/// The rows differ along what the rule generalises over: which MEMBER carries the
/// enum (the four request components that take an oversize-handling setting, which
/// the report named one of), how deep in the request it sits, which OPERATION
/// carries it, and which ENUM it is — the oversize-handling set, whose stated order
/// is not the model's, beside three enums whose stated order is the model's
/// (`smithy_query(query="shape",shape="OversizeHandling,BodyParsingFallbackBehavior,Scope,IPAddressVersion")`
/// → `CONTINUE, MATCH, NO_MATCH`; `MATCH, NO_MATCH, EVALUATE_AS_STRING`;
/// `CLOUDFRONT, REGIONAL`; `IPV4, IPV6`).
fn cases(web_acl_id: &str, lock_token: &str) -> Vec<Case> {
    vec![
        Case {
            case: "a web ACL rule inspecting the body",
            operation: "CreateWebACL",
            request: json!({
                "Name": "enum-order-acl",
                "Scope": "REGIONAL",
                "DefaultAction": { "Allow": {} },
                "VisibilityConfig": visibility("enum-order-acl"),
                "Rules": [rule(
                    "oversize",
                    byte_match_over(json!({ "Body": { "OversizeHandling": OUTSIDE_EVERY_SET } })),
                )],
            }),
            path: "rules.1.member.statement.byteMatchStatement.fieldToMatch.body.oversizeHandling",
            set: OVERSIZE_HANDLING_SET,
        },
        Case {
            case: "the second rule of a web ACL update, inspecting the body",
            operation: "UpdateWebACL",
            request: json!({
                "Name": "enum-order-acl-live",
                "Scope": "REGIONAL",
                "Id": web_acl_id,
                "LockToken": lock_token,
                "DefaultAction": { "Allow": {} },
                "VisibilityConfig": visibility("enum-order-acl-live"),
                "Rules": ordered(json!([
                    rule("plain", byte_match_over(json!({ "UriPath": {} }))),
                    rule(
                        "oversize",
                        byte_match_over(
                            json!({ "Body": { "OversizeHandling": OUTSIDE_EVERY_SET } }),
                        ),
                    ),
                ])),
            }),
            path: "rules.2.member.statement.byteMatchStatement.fieldToMatch.body.oversizeHandling",
            set: OVERSIZE_HANDLING_SET,
        },
        Case {
            case: "a rule group rule inspecting the headers",
            operation: "CreateRuleGroup",
            request: create_body(
                "enum-order-group",
                "REGIONAL",
                500,
                None,
                &json!([rule(
                    "oversize",
                    byte_match_over(json!({
                        "Headers": {
                            "MatchPattern": { "All": {} },
                            "MatchScope": "ALL",
                            "OversizeHandling": OUTSIDE_EVERY_SET,
                        }
                    })),
                )]),
                None,
            ),
            path: "rules.1.member.statement.byteMatchStatement.fieldToMatch.headers.\
                   oversizeHandling",
            set: OVERSIZE_HANDLING_SET,
        },
        Case {
            case: "a priced rule inspecting the cookies",
            operation: "CheckCapacity",
            request: json!({
                "Scope": "REGIONAL",
                "Rules": [rule(
                    "oversize",
                    byte_match_over(json!({
                        "Cookies": {
                            "MatchPattern": { "All": {} },
                            "MatchScope": "ALL",
                            "OversizeHandling": OUTSIDE_EVERY_SET,
                        }
                    })),
                )],
            }),
            path: "rules.1.member.statement.byteMatchStatement.fieldToMatch.cookies.\
                   oversizeHandling",
            set: OVERSIZE_HANDLING_SET,
        },
        Case {
            case: "a nested rule inspecting the JSON body",
            operation: "CheckCapacity",
            request: json!({
                "Scope": "REGIONAL",
                "Rules": [rule(
                    "oversize",
                    json!({
                        "NotStatement": {
                            "Statement": byte_match_over(json!({
                                "JsonBody": {
                                    "MatchPattern": { "All": {} },
                                    "MatchScope": "ALL",
                                    "OversizeHandling": OUTSIDE_EVERY_SET,
                                }
                            })),
                        }
                    }),
                )],
            }),
            path: "rules.1.member.statement.notStatement.statement.byteMatchStatement.\
                   fieldToMatch.jsonBody.oversizeHandling",
            set: OVERSIZE_HANDLING_SET,
        },
        // An enum of the SAME size whose values are not the oversize-handling set:
        // the clause lists it in the model's declared order, which is the order WAF's
        // recorded messages show for every enum but the one above.
        Case {
            case: "the JSON body's parsing fallback",
            operation: "CheckCapacity",
            request: json!({
                "Scope": "REGIONAL",
                "Rules": [rule(
                    "fallback",
                    byte_match_over(json!({
                        "JsonBody": {
                            "MatchPattern": { "All": {} },
                            "MatchScope": "ALL",
                            "InvalidFallbackBehavior": OUTSIDE_EVERY_SET,
                        }
                    })),
                )],
            }),
            path: "rules.1.member.statement.byteMatchStatement.fieldToMatch.jsonBody.\
                   invalidFallbackBehavior",
            set: "MATCH, NO_MATCH, EVALUATE_AS_STRING",
        },
        Case {
            case: "the namespace a list is asked for",
            operation: "ListWebACLs",
            request: json!({ "Scope": OUTSIDE_EVERY_SET }),
            path: "scope",
            set: "CLOUDFRONT, REGIONAL",
        },
        Case {
            case: "an IP set's address family",
            operation: "CreateIPSet",
            request: json!({
                "Name": "enum-order-ips",
                "Scope": "REGIONAL",
                "IPAddressVersion": OUTSIDE_EVERY_SET,
                "Addresses": ["192.0.2.0/24"],
            }),
            path: "iPAddressVersion",
            set: "IPV4, IPV6",
        },
    ]
}

/// Every row is refused with the one clause, listing its enum's set in WAF's order.
#[tokio::test]
async fn an_out_of_set_value_is_refused_with_the_enums_own_stated_order() {
    let waf = Waf::new();
    let (web_acl_id, _arn, lock_token) =
        create_web_acl(&waf, "REGIONAL", "enum-order-acl-live", json!([])).await;

    for Case {
        case,
        operation,
        request,
        path,
        set,
    } in cases(&web_acl_id, &lock_token)
    {
        let (status, body) = waf.call(operation, request).await;
        assert_eq!(
            status, 400,
            "case {case:?}: {operation} must answer 400, got status {status} and body \
             {body}"
        );
        assert!(
            body["__type"]
                .as_str()
                .unwrap_or_default()
                .ends_with(VALIDATION_EXCEPTION),
            "case {case:?}: expected {VALIDATION_EXCEPTION}, got {body}"
        );
        let expected = format!(
            "1 validation error detected: Value '{OUTSIDE_EVERY_SET}' at '{path}' failed \
             to satisfy constraint: Member must satisfy enum value set: [{set}]"
        );
        let message = body["message"]
            .as_str()
            .or_else(|| body["Message"].as_str())
            .unwrap_or_default();
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?} in body {body}"
        );
    }
}

/// The value set the clause lists is the enum's, not the request's: the same member
/// refused for two different values states one set, and a value INSIDE the set is
/// served.
#[tokio::test]
async fn the_stated_set_does_not_depend_on_the_value_that_was_refused() {
    let waf = Waf::new();
    let name = "enum-order-values";

    for refused in ["", "continue", "MATCH_ALL", "NO MATCH"] {
        let request = json!({
            "Scope": "REGIONAL",
            "Rules": [rule(
                "oversize",
                byte_match_over(json!({ "Body": { "OversizeHandling": refused } })),
            )],
        });
        let (status, body) = waf.call("CheckCapacity", request).await;
        assert_eq!(
            status, 400,
            "case {refused:?}: expected 400, got status {status} and body {body}"
        );
        let message = body["message"]
            .as_str()
            .or_else(|| body["Message"].as_str())
            .unwrap_or_default();
        assert_eq!(
            message,
            format!(
                "1 validation error detected: Value '{refused}' at \
                 'rules.1.member.statement.byteMatchStatement.fieldToMatch.body.\
                 oversizeHandling' failed to satisfy constraint: Member must satisfy \
                 enum value set: [{OVERSIZE_HANDLING_SET}]"
            ),
            "case {refused:?}: the clause must state the enum's set, got {message:?} in \
             body {body}"
        );
    }

    // Each value the set DOES carry is served, and the rule reads back with it: the
    // clause above is about the value being outside the set, nothing else.
    for value in ["CONTINUE", "MATCH", "NO_MATCH"] {
        let acl_name = format!("{name}-{value}");
        let rules = json!([rule(
            "oversize",
            byte_match_over(json!({ "Body": { "OversizeHandling": value } })),
        )]);
        let (acl_id, _arn, _lock) =
            create_web_acl(&waf, "REGIONAL", &acl_name, rules.clone()).await;
        let got = waf
            .ok(
                "GetWebACL",
                json!({ "Name": &acl_name, "Scope": "REGIONAL", "Id": &acl_id }),
            )
            .await;
        assert_eq!(
            got["WebACL"]["Rules"], rules,
            "case {value:?}: a value the set carries must be stored, got {got}"
        );
    }
}
