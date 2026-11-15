// SPDX-License-Identifier: CC0-1.0

//! WHOSE account an ARN names, end to end over the wire.
//!
//! The rule asserted here: an ARN names the account that owns the resource, and WAF
//! resolves it for the account the call was made FOR — so an ARN whose account segment is
//! another account names an account WAF cannot resolve for this caller, and the call is
//! refused with the account-unresolvable code `UnrecognizedClientException` and the fixed
//! sentence "No account found for the given parameters", before anything is looked up.
//! That is a different answer from "your resource doesn't exist": a client that catches it
//! re-targets its credentials or its account instead of concluding its resource was
//! deleted.
//!
//! It is judged ahead of everything else the ARN says about the resource — the region, the
//! namespace and the kind — because none of those is this caller's to ask about; what it
//! is NOT judged ahead of is whether the value is a wafv2 resource ARN at all, since an
//! ARN WAF cannot read has no account segment to compare.
//!
//! The rows differ along what the rule generalises over: WHICH other account the ARN names
//! (three of them, including the documentation placeholder), what else about the ARN is
//! also wrong (its region, its resource kind), the OPERATION the ARN addresses a resource
//! through (the two readers that take one, `GetWebACL` and `GetRuleGroup`), and the controls
//! that differ from a refused row in the account segment alone — the caller's own ARN,
//! which resolves, and the same foreign ARN with the calling account put back.
//!
//! Every call goes through the same front door a real client uses (the `Waf` harness in
//! [`crate::web_acl_lifecycle`]), so what these tests observe is what an SDK observes.

use serde_json::{json, Value};
use service_com_amazonaws_wafv2_awswaf_20190729::state::SCOPE_REGIONAL;

use crate::rule_group_lifecycle::create_rule_group;
use crate::web_acl_lifecycle::{create_ip_set, create_web_acl, Waf, TEST_REGION};

/// The error code WAF answers a request naming an account it cannot resolve for the
/// caller.
const UNRECOGNIZED_CLIENT: &str = "UnrecognizedClientException";

/// The sentence that refusal carries: entirely fixed text, naming no value the request
/// supplied.
const NO_ACCOUNT_FOUND: &str = "No account found for the given parameters";

/// The `Reason` WAF carries for an ARN it will not accept, which is what an ARN fault of
/// the CALLER'S OWN account is answered with instead.
const ARN_NOT_VALID: &str = "The ARN isn't valid. A valid ARN begins with arn: and \
     includes other information separated by colons or slashes.";

/// The error body's message member, whichever casing the shape models.
fn error_message(body: &Value) -> &str {
    body.get("Message")
        .or_else(|| body.get("message"))
        .and_then(Value::as_str)
        .unwrap_or_default()
}

/// `arn` with its account segment replaced by `account`.
fn with_account(arn: &str, account: &str) -> String {
    let mut segments: Vec<&str> = arn.split(':').collect();
    assert!(
        segments.len() > 4,
        "an ARN has an account segment to replace, got {arn:?}"
    );
    segments[4] = account;
    segments.join(":")
}

/// An ARN naming another account is refused as an account WAF cannot resolve, whatever
/// else the ARN says — and the same ARN with the calling account resolves.
#[tokio::test]
async fn an_arn_naming_another_account_is_refused_as_an_account_that_does_not_resolve() {
    let waf = Waf::new();
    let account = ::engine::EMULATOR_ACCOUNT_ID;
    let (_id, acl_arn, _lock) = create_web_acl(&waf, SCOPE_REGIONAL, "account-acl", json!([])).await;
    let (_set_id, ip_set_arn, _set_lock) = create_ip_set(&waf, SCOPE_REGIONAL, "account-ips").await;

    // The caller's own ARN resolves, so what the rows below change is the account alone.
    let found = waf.ok("GetWebACL", json!({ "ARN": &acl_arn })).await;
    assert_eq!(
        found["WebACL"]["Name"].as_str(),
        Some("account-acl"),
        "the caller's own ARN must resolve, got {found}"
    );

    // Every row names an account that is not the calling one. The rows span the accounts
    // AWS documentation uses for "some other account", and they span what else about the
    // ARN is wrong: its region, and its resource kind — neither of which this caller may
    // ask about in another account, so the account refusal comes first.
    let rows: Vec<(&str, String)> = vec![
        (
            "the documentation placeholder account",
            with_account(&acl_arn, "111122223333"),
        ),
        (
            "another account of the same width",
            with_account(&acl_arn, "999999999999"),
        ),
        (
            "an account one digit from the caller's own",
            with_account(&acl_arn, "000000000001"),
        ),
        (
            "another account's ARN naming another region too",
            with_account(&acl_arn.replace(TEST_REGION, "eu-west-3"), "111122223333"),
        ),
        (
            "another account's ARN naming another resource kind too",
            with_account(&ip_set_arn, "111122223333"),
        ),
    ];
    for (case, arn) in &rows {
        let (status, body) = waf.call("GetWebACL", json!({ "ARN": arn })).await;
        assert_eq!(
            status, 403,
            "case {case:?}: expected the account refusal, got status {status} and body \
             {body}"
        );
        assert!(
            body["__type"]
                .as_str()
                .unwrap_or_default()
                .ends_with(UNRECOGNIZED_CLIENT),
            "case {case:?}: expected the error code {UNRECOGNIZED_CLIENT}, got {body}"
        );
        assert_eq!(
            error_message(&body),
            NO_ACCOUNT_FOUND,
            "case {case:?}: expected WAF's account sentence, got {body}"
        );
    }

    // Put the calling account back into each row's ARN and the refusal is no longer about
    // the account: the two rows that only changed the account resolve, and the two that
    // changed something else are told about THAT.
    let restored = waf
        .ok(
            "GetWebACL",
            json!({ "ARN": with_account(&rows[0].1, account) }),
        )
        .await;
    assert_eq!(
        restored["WebACL"]["Name"].as_str(),
        Some("account-acl"),
        "the same ARN with the calling account must resolve, got {restored}"
    );

    let (status, body) = waf
        .call(
            "GetWebACL",
            json!({ "ARN": with_account(&rows[3].1, account) }),
        )
        .await;
    assert_eq!(
        status, 400,
        "an ARN of the caller's own account naming another region is the ARN refusal, got \
         status {status} and body {body}"
    );
    assert_eq!(
        body["Reason"].as_str(),
        Some(ARN_NOT_VALID),
        "an ARN fault of the caller's own account must be reported as the ARN, got {body}"
    );

    let (status, body) = waf
        .call("GetWebACL", json!({ "ARN": &ip_set_arn }))
        .await;
    assert_eq!(
        status, 400,
        "an ARN of the caller's own account naming another kind is the access-denied \
         refusal, got status {status} and body {body}"
    );
    assert_eq!(
        error_message(&body),
        format!("Invalid ARN provided for GetWebACLRequest {ip_set_arn}"),
        "an ARN of the caller's own account naming another kind must name the request \
         shape and the ARN, got {body}"
    );
}

/// The same judgement on the sibling by-ARN reader: `GetRuleGroup` is addressed by an ARN
/// too, so an ARN naming another account is the same account refusal there, and the rule
/// group's own ARN still resolves.
#[tokio::test]
async fn the_sibling_by_arn_reader_refuses_another_account_the_same_way() {
    let waf = Waf::new();
    let (_id, group_arn, _lock) =
        create_rule_group(&waf, SCOPE_REGIONAL, "account-group", 100, json!([])).await;

    let found = waf.ok("GetRuleGroup", json!({ "ARN": &group_arn })).await;
    assert_eq!(
        found["RuleGroup"]["Name"].as_str(),
        Some("account-group"),
        "the caller's own ARN must resolve, got {found}"
    );

    for (case, account) in [
        ("the documentation placeholder account", "111122223333"),
        ("another account of the same width", "999999999999"),
    ] {
        let arn = with_account(&group_arn, account);
        let (status, body) = waf.call("GetRuleGroup", json!({ "ARN": &arn })).await;
        assert_eq!(
            status, 403,
            "case {case:?}: expected the account refusal, got status {status} and body \
             {body}"
        );
        assert!(
            body["__type"]
                .as_str()
                .unwrap_or_default()
                .ends_with(UNRECOGNIZED_CLIENT),
            "case {case:?}: expected the error code {UNRECOGNIZED_CLIENT}, got {body}"
        );
        assert_eq!(
            error_message(&body),
            NO_ACCOUNT_FOUND,
            "case {case:?}: expected WAF's account sentence, got {body}"
        );
    }
}

/// A value WAF cannot read as a wafv2 resource ARN has no account segment to compare, so
/// it is still the ARN refusal — the account judgement does not swallow the malformed-ARN
/// class.
#[tokio::test]
async fn a_value_that_is_no_resource_arn_is_still_refused_as_an_arn() {
    let waf = Waf::new();
    create_web_acl(&waf, SCOPE_REGIONAL, "malformed-acl", json!([])).await;

    // Each value is past `ResourceArn`'s modelled length floor of 20, so it reaches the
    // service rather than the request boundary.
    for (case, arn) in [
        ("a value that is no ARN", "not-an-arn-but-long-enough"),
        ("another service's ARN", "arn:aws:s3:::a-bucket-named-like-this"),
        (
            "a wafv2 ARN with no namespace segment wafv2 has",
            "arn:aws:wafv2:us-west-2:111122223333:worldwide/webacl/absent/1111",
        ),
        (
            "a wafv2 ARN with too few resource segments",
            "arn:aws:wafv2:us-west-2:111122223333:regional/webacl/absent",
        ),
    ] {
        let body = waf
            .refused(
                "GetWebACL",
                json!({ "ARN": arn }),
                "WAFInvalidParameterException",
                case,
            )
            .await;
        assert_eq!(
            (body["Parameter"].as_str(), body["Reason"].as_str()),
            (Some(arn), Some(ARN_NOT_VALID)),
            "case {case:?}: expected the ARN refusal echoing the value, got {body}"
        );
    }
}
