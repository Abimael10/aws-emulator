// SPDX-License-Identifier: CC0-1.0

//! The CONNECTED (Payments) ACCOUNT a monetization configuration names, end to end over
//! the wire.
//!
//! The rule under test: a save that configures monetization settles its payments into
//! the connected accounts `MonetizationConfig$ConnectedAccountConfigs` names, and WAF
//! RESOLVES each of them before it writes the document — a reference that resolves to
//! nothing is the modelled `WAFNonexistentItemException` (client, HTTP 400, bound by
//! every one of the four operations that carry the configuration:
//! `smithy_query(query="operation",operation="CreateWebACL")` and its three siblings),
//! carrying WAF's fixed sentence for that condition, and nothing is stored. A connected
//! account is a Payments resource, created in that console and only ever resolved here;
//! this emulator serves no Payments control plane, so the set a save resolves against
//! holds none and every reference resolves to nothing — while a configuration that names
//! NO connected account is stored and reads back verbatim, whatever else the request
//! carries.
//!
//! The rows differ along what the rule generalises over: the ARN FORM a configuration
//! carries (partition, region — including the empty one of a partition-global resource —
//! account, resource-id spelling, and both the `type/id` and `type:id` resource forms),
//! HOW MANY accounts the configuration names and which of them resolves, what else the
//! configuration carries beside them, and WHICH of the four saving operations is asked —
//! a create and an update, of a web ACL and of a rule group. The complement rows are
//! configurations that name no account at all, including requests that carry a Payments
//! ARN in a member that has nothing to do with settlement, which are created normally.
//!
//! Every call goes through the same front door a real client uses (the `Waf` harness in
//! [`crate::web_acl_lifecycle`]), so what these tests observe is what an SDK observes.

use serde_json::{json, Value};

use crate::rule_group_lifecycle::create_rule_group;
use crate::web_acl_lifecycle::{visibility, Waf};

/// The one namespace monetization is served in: the `Monetize` action "is available only
/// for web ACLs associated with Amazon CloudFront distributions"
/// (`smithy_query(query="shape",shape="MonetizeAction")`), which is the CLOUDFRONT scope.
const MONETIZED_SCOPE: &str = "CLOUDFRONT";

/// The other namespace, which serves no monetization at all.
const REGIONAL_SCOPE: &str = "REGIONAL";

/// The wire error code (`__type`) of the modelled refusal for something the request names
/// that does not exist.
const NONEXISTENT_ITEM_CODE: &str = "WAFNonexistentItemException";

/// The wire error code of the refusal for an operation that does not exist in the
/// namespace it was asked for, which is what monetization asked for regionally earns.
const INVALID_OPERATION_CODE: &str = "WAFInvalidOperationException";

/// The sentence WAF answers a save whose connected account resolves to nothing with, as
/// the recorded WAF responses word it: fixed text in full, naming the member a caller
/// corrects rather than echoing the reference, so the same sentence answers every such
/// save.
const CONNECTED_ACCOUNT_NOT_FOUND: &str = "The specified connected account does not \
     exist. Verify the ConnectedAccountArn. If the account was deleted and recreated, it \
     has a new ARN - use the current one from the Payments console.";

/// The sentence WAF answers a monetization setting asked for in the regional namespace
/// with (`crate::refusal_classes` spans the same sentence over the monetization reads).
const MONETIZATION_NOT_AVAILABLE: &str = "Monetization is not available for regional \
     resources. Use CloudFront scope to configure monetization.";

/// A well-formed `MonetizationConfig` naming no connected account: one production payment
/// network with a wallet address and a price per request, in the matching production
/// currency mode.
fn crypto_config() -> Value {
    json!({
        "CryptoConfig": {
            "PaymentNetworks": [{
                "Chain": "BASE",
                "WalletAddress": "0x00000000000000000000000000000000000000a1",
                "Prices": [{ "Amount": "0.01", "Currency": "USDC" }],
            }]
        },
        "CurrencyMode": "REAL",
    })
}

/// One row: a `MonetizationConfig` whose `ConnectedAccountConfigs` name accounts, and how
/// those references are written.
struct ConnectedAccountRow {
    case: &'static str,
    config: Value,
}

/// The ARN forms a connected-account configuration can carry, each written into a
/// monetization configuration that differs in what it carries beside them.
///
/// Every value is `ResourceArn` (`@length(20..2048)`, `@pattern \S`:
/// `smithy_query(query="shape",shape="ResourceArn")`), which is what
/// `ConnectedAccountConfig$ConnectedAccountArn` targets, so each row reaches the
/// precondition rather than the request validator.
fn connected_account_rows() -> Vec<ConnectedAccountRow> {
    vec![
        ConnectedAccountRow {
            case: "one account, alone on the configuration",
            config: json!({
                "ConnectedAccountConfigs": [{
                    "ConnectedAccountArn":
                        "arn:aws:payments:us-east-1:000000000000:connected-account/acct-1",
                }],
            }),
        },
        ConnectedAccountRow {
            case: "one account beside a well-formed crypto configuration",
            config: {
                let mut config = crypto_config();
                config["ConnectedAccountConfigs"] = json!([{
                    "ConnectedAccountArn":
                        "arn:aws:payments:us-east-1:111122223333:connected-account/\
                         8f1d2c3b4a5e6f70",
                }]);
                config
            },
        },
        ConnectedAccountRow {
            case: "an ARN of the type:id resource form, beside the currency mode",
            config: json!({
                "CurrencyMode": "TEST",
                "ConnectedAccountConfigs": [{
                    "ConnectedAccountArn":
                        "arn:aws:payments:eu-west-1:210987654321:connected-account:acct_T",
                }],
            }),
        },
        ConnectedAccountRow {
            case: "an ARN of another partition with no region",
            config: json!({
                "ConnectedAccountConfigs": [{
                    "ConnectedAccountArn":
                        "arn:aws-us-gov:payments::000000000000:connected-account/gov-1",
                }],
            }),
        },
        ConnectedAccountRow {
            case: "an ARN naming another service's resource",
            config: json!({
                "ConnectedAccountConfigs": [{
                    "ConnectedAccountArn":
                        "arn:aws:wafv2:ap-southeast-2:000000000000:global/webacl/none/0",
                }],
            }),
        },
        ConnectedAccountRow {
            case: "two accounts, the second of which is the one that resolves to nothing",
            config: json!({
                "CurrencyMode": "REAL",
                "ConnectedAccountConfigs": [
                    { "ConnectedAccountArn":
                        "arn:aws:payments:us-east-1:000000000000:connected-account/first" },
                    { "ConnectedAccountArn":
                        "arn:aws:payments:us-east-1:000000000000:connected-account/second" },
                ],
            }),
        },
        ConnectedAccountRow {
            case: "an account beside a configuration entry that names none",
            config: json!({
                "ConnectedAccountConfigs": [
                    {},
                    { "ConnectedAccountArn":
                        "arn:aws:payments:ap-south-1:000000000000:connected-account/only" },
                ],
            }),
        },
    ]
}

/// A `MonetizationConfig` that names NO connected account, by row: the well-formed
/// configuration, the currency mode alone, a testnet network, an empty list of
/// connected-account configurations and a configuration entry that names no account —
/// none of them names an account, so none engages the precondition.
fn unreferencing_rows() -> Vec<(&'static str, Value)> {
    vec![
        ("a well-formed crypto configuration", crypto_config()),
        ("the currency mode alone", json!({ "CurrencyMode": "TEST" })),
        (
            "a testnet network with a wallet address and its price",
            json!({
                "CryptoConfig": {
                    "PaymentNetworks": [{
                        "Chain": "SOLANA_DEVNET",
                        "WalletAddress": "So11111111111111111111111111111111111111112",
                        "Prices": [{ "Amount": "0.25", "Currency": "USDC" }],
                    }]
                },
                "CurrencyMode": "TEST",
            }),
        ),
        (
            "an empty list of connected-account configurations",
            json!({ "CurrencyMode": "REAL", "ConnectedAccountConfigs": [] }),
        ),
        (
            "a connected-account configuration that names no account",
            json!({ "ConnectedAccountConfigs": [{}] }),
        ),
    ]
}

/// Assert that `status`/`body` is the refusal a save naming a connected account that does
/// not exist earns: the modelled code and WAF's whole fixed sentence.
fn assert_connected_account_not_found(status: u16, body: &Value, case: &str) {
    assert_eq!(
        status, 400,
        "case {case:?}: expected the save to be refused with 400, got status {status} and \
         body {body}"
    );
    let code = body["__type"].as_str().unwrap_or_default();
    assert!(
        code.ends_with(NONEXISTENT_ITEM_CODE),
        "case {case:?}: expected the error code {NONEXISTENT_ITEM_CODE}, got {code:?} in \
         body {body}"
    );
    assert_eq!(
        body["Message"].as_str(),
        Some(CONNECTED_ACCOUNT_NOT_FOUND),
        "case {case:?}: expected WAF's sentence for a connected account that does not \
         exist, got {:?}",
        body["Message"]
    );
}

/// The names a namespace's listing holds, for asserting that a refused create stored
/// nothing.
async fn names_in_scope(waf: &Waf, operation: &str, member: &str, scope: &str) -> Vec<String> {
    let listed = waf.ok(operation, json!({ "Scope": scope })).await;
    listed[member]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item["Name"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// A CREATE whose monetization configuration names a connected account is refused with
/// WAF's fixed sentence, and nothing is created — for a web ACL and for a rule group, over
/// every ARN form the member can carry.
#[tokio::test]
async fn a_create_naming_a_connected_account_is_refused_and_stores_nothing() {
    let waf = Waf::new();

    for (index, row) in connected_account_rows().iter().enumerate() {
        let acl_name = format!("connected-acl-{index}");
        let case = format!("CreateWebACL with {}", row.case);
        let (status, body) = waf
            .call(
                "CreateWebACL",
                json!({
                    "Name": &acl_name,
                    "Scope": MONETIZED_SCOPE,
                    "DefaultAction": { "Allow": {} },
                    "VisibilityConfig": visibility(&acl_name),
                    "MonetizationConfig": row.config.clone(),
                }),
            )
            .await;
        assert_connected_account_not_found(status, &body, &case);
        let stored = names_in_scope(&waf, "ListWebACLs", "WebACLs", MONETIZED_SCOPE).await;
        assert!(
            !stored.contains(&acl_name),
            "case {case:?}: a refused create must store no web ACL, got {stored:?}"
        );

        let group_name = format!("connected-group-{index}");
        let case = format!("CreateRuleGroup with {}", row.case);
        let (status, body) = waf
            .call(
                "CreateRuleGroup",
                json!({
                    "Name": &group_name,
                    "Scope": MONETIZED_SCOPE,
                    "Capacity": 1_000,
                    "VisibilityConfig": visibility(&group_name),
                    "MonetizationConfig": row.config.clone(),
                }),
            )
            .await;
        assert_connected_account_not_found(status, &body, &case);
        let stored = names_in_scope(&waf, "ListRuleGroups", "RuleGroups", MONETIZED_SCOPE).await;
        assert!(
            !stored.contains(&group_name),
            "case {case:?}: a refused create must store no rule group, got {stored:?}"
        );
    }
}

/// An UPDATE is a save too, so it resolves the account before it writes: the refusal is
/// the same, the stored document keeps its `LockToken` and carries no monetization
/// configuration afterwards.
#[tokio::test]
async fn an_update_naming_a_connected_account_is_refused_and_writes_nothing() {
    let waf = Waf::new();

    let acl_name = "connected-update-acl";
    let created = waf
        .ok(
            "CreateWebACL",
            json!({
                "Name": acl_name,
                "Scope": MONETIZED_SCOPE,
                "DefaultAction": { "Allow": {} },
                "VisibilityConfig": visibility(acl_name),
            }),
        )
        .await;
    let acl_id = created["Summary"]["Id"]
        .as_str()
        .expect("Summary.Id")
        .to_string();
    let acl_lock = created["Summary"]["LockToken"]
        .as_str()
        .expect("Summary.LockToken")
        .to_string();

    let group_name = "connected-update-group";
    let (group_id, _group_arn, group_lock) =
        create_rule_group(&waf, MONETIZED_SCOPE, group_name, 1_000, json!([])).await;

    for row in connected_account_rows() {
        let case = format!("UpdateWebACL with {}", row.case);
        let (status, body) = waf
            .call(
                "UpdateWebACL",
                json!({
                    "Name": acl_name,
                    "Scope": MONETIZED_SCOPE,
                    "Id": &acl_id,
                    "LockToken": &acl_lock,
                    "DefaultAction": { "Allow": {} },
                    "VisibilityConfig": visibility(acl_name),
                    "MonetizationConfig": row.config.clone(),
                }),
            )
            .await;
        assert_connected_account_not_found(status, &body, &case);
        let got = waf
            .ok(
                "GetWebACL",
                json!({ "Name": acl_name, "Scope": MONETIZED_SCOPE, "Id": &acl_id }),
            )
            .await;
        assert_eq!(
            got["LockToken"].as_str(),
            Some(acl_lock.as_str()),
            "case {case:?}: a refused update must not rotate the LockToken, got {got}"
        );
        assert!(
            got["WebACL"].get("MonetizationConfig").is_none(),
            "case {case:?}: a refused update must store no MonetizationConfig, got {got}"
        );

        let case = format!("UpdateRuleGroup with {}", row.case);
        let (status, body) = waf
            .call(
                "UpdateRuleGroup",
                json!({
                    "Name": group_name,
                    "Scope": MONETIZED_SCOPE,
                    "Id": &group_id,
                    "LockToken": &group_lock,
                    "VisibilityConfig": visibility(group_name),
                    "MonetizationConfig": row.config.clone(),
                }),
            )
            .await;
        assert_connected_account_not_found(status, &body, &case);
        let got = waf
            .ok(
                "GetRuleGroup",
                json!({ "Name": group_name, "Scope": MONETIZED_SCOPE, "Id": &group_id }),
            )
            .await;
        assert_eq!(
            got["LockToken"].as_str(),
            Some(group_lock.as_str()),
            "case {case:?}: a refused update must not rotate the LockToken, got {got}"
        );
        assert!(
            got["RuleGroup"].get("MonetizationConfig").is_none(),
            "case {case:?}: a refused update must store no MonetizationConfig, got {got}"
        );
    }
}

/// The complement, which is what makes the rule about the member and not about
/// monetization: a configuration that does not name a connected account is accepted,
/// stored and read back verbatim — for a web ACL and for a rule group.
#[tokio::test]
async fn a_configuration_naming_no_connected_account_is_stored_and_reads_back() {
    let waf = Waf::new();

    for (index, (case, config)) in unreferencing_rows().iter().enumerate() {
        let acl_name = format!("unreferencing-acl-{index}");
        let created = waf
            .ok(
                "CreateWebACL",
                json!({
                    "Name": &acl_name,
                    "Scope": MONETIZED_SCOPE,
                    "DefaultAction": { "Allow": {} },
                    "VisibilityConfig": visibility(&acl_name),
                    "MonetizationConfig": config.clone(),
                }),
            )
            .await;
        let acl_id = created["Summary"]["Id"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected a Summary.Id, got {created}"))
            .to_string();
        let got = waf
            .ok(
                "GetWebACL",
                json!({ "Name": &acl_name, "Scope": MONETIZED_SCOPE, "Id": &acl_id }),
            )
            .await;
        assert_eq!(
            got["WebACL"]["MonetizationConfig"],
            *config,
            "case {case:?}: the stored MonetizationConfig must be the one the save \
             carried, got {got}"
        );

        let group_name = format!("unreferencing-group-{index}");
        let created = waf
            .ok(
                "CreateRuleGroup",
                json!({
                    "Name": &group_name,
                    "Scope": MONETIZED_SCOPE,
                    "Capacity": 1_000,
                    "VisibilityConfig": visibility(&group_name),
                    "MonetizationConfig": config.clone(),
                }),
            )
            .await;
        let group_id = created["Summary"]["Id"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected a Summary.Id, got {created}"))
            .to_string();
        let got = waf
            .ok(
                "GetRuleGroup",
                json!({ "Name": &group_name, "Scope": MONETIZED_SCOPE, "Id": &group_id }),
            )
            .await;
        assert_eq!(
            got["RuleGroup"]["MonetizationConfig"],
            *config,
            "case {case:?}: the stored MonetizationConfig must be the one the save \
             carried, got {got}"
        );
    }
}

/// The precondition is about the SETTLEMENT ACCOUNT the configuration names, not about
/// Payments ARNs in general: a save that carries one in a member with nothing to do with
/// settlement — a tag's value, a custom response body's content — names no connected
/// account, so it is created normally and reads back.
#[tokio::test]
async fn a_payments_arn_in_an_unrelated_member_names_no_connected_account() {
    let waf = Waf::new();
    let unrelated = "arn:aws:payments:us-east-1:111122223333:invoice/42";

    let acl_name = "unrelated-payments-arn-acl";
    let created = waf
        .ok(
            "CreateWebACL",
            json!({
                "Name": acl_name,
                "Scope": MONETIZED_SCOPE,
                "DefaultAction": { "Allow": {} },
                "VisibilityConfig": visibility(acl_name),
                "MonetizationConfig": crypto_config(),
                "Tags": [{ "Key": "owner", "Value": unrelated }],
                "CustomResponseBodies": {
                    "settled": { "ContentType": "TEXT_PLAIN", "Content": unrelated },
                },
            }),
        )
        .await;
    let acl_id = created["Summary"]["Id"]
        .as_str()
        .unwrap_or_else(|| panic!("expected a Summary.Id, got {created}"))
        .to_string();
    let got = waf
        .ok(
            "GetWebACL",
            json!({ "Name": acl_name, "Scope": MONETIZED_SCOPE, "Id": &acl_id }),
        )
        .await;
    assert_eq!(
        got["WebACL"]["MonetizationConfig"],
        crypto_config(),
        "the web ACL must be stored with the configuration the save carried, got {got}"
    );
    assert_eq!(
        got["WebACL"]["CustomResponseBodies"]["settled"]["Content"].as_str(),
        Some(unrelated),
        "the unrelated member must be stored as it was written, got {got}"
    );

    let group_name = "unrelated-payments-arn-group";
    let created = waf
        .ok(
            "CreateRuleGroup",
            json!({
                "Name": group_name,
                "Scope": MONETIZED_SCOPE,
                "Capacity": 1_000,
                "VisibilityConfig": visibility(group_name),
                "MonetizationConfig": crypto_config(),
                "Tags": [{ "Key": "owner", "Value": unrelated }],
            }),
        )
        .await;
    let group_id = created["Summary"]["Id"]
        .as_str()
        .unwrap_or_else(|| panic!("expected a Summary.Id, got {created}"))
        .to_string();
    let got = waf
        .ok(
            "GetRuleGroup",
            json!({ "Name": group_name, "Scope": MONETIZED_SCOPE, "Id": &group_id }),
        )
        .await;
    assert_eq!(
        got["RuleGroup"]["MonetizationConfig"],
        crypto_config(),
        "the rule group must be stored with the configuration the save carried, got {got}"
    );
}

/// The member keeps its modelled constraint, which is a different refusal: a value
/// shorter than the `ResourceArn` minimum is refused by the request boundary as a member
/// constraint rather than by the resolution, so a caller can tell a malformed reference
/// from one that resolves to nothing.
#[tokio::test]
async fn a_reference_outside_the_members_constraint_is_a_member_refusal() {
    let waf = Waf::new();
    let acl_name = "short-connected-account-acl";
    let (status, body) = waf
        .call(
            "CreateWebACL",
            json!({
                "Name": acl_name,
                "Scope": MONETIZED_SCOPE,
                "DefaultAction": { "Allow": {} },
                "VisibilityConfig": visibility(acl_name),
                "MonetizationConfig": {
                    "ConnectedAccountConfigs": [
                        { "ConnectedAccountArn": "arn:aws:payments" },
                    ],
                },
            }),
        )
        .await;
    assert_eq!(
        status, 400,
        "expected a value under the member's minimum length to be refused, got status \
         {status} and body {body}"
    );
    let code = body["__type"].as_str().unwrap_or_default();
    assert!(
        !code.ends_with(NONEXISTENT_ITEM_CODE),
        "expected the member-constraint refusal rather than the resolution's, got {body}"
    );
    let stored = names_in_scope(&waf, "ListWebACLs", "WebACLs", MONETIZED_SCOPE).await;
    assert!(
        !stored.contains(&acl_name.to_string()),
        "a refused create must store no web ACL, got {stored:?}"
    );
}

/// The ORDER the monetization judgements are taken in: the namespace comes first.
/// Monetization is not served regionally at all, so a regional save naming a connected
/// account is refused for the NAMESPACE — the account of a feature that does not exist
/// there is never resolved — and nothing is stored either way.
#[tokio::test]
async fn a_regional_save_naming_a_connected_account_is_refused_for_the_namespace() {
    let waf = Waf::new();

    for (index, row) in connected_account_rows().iter().enumerate() {
        let acl_name = format!("regional-connected-acl-{index}");
        let case = format!("REGIONAL CreateWebACL with {}", row.case);
        let (status, body) = waf
            .call(
                "CreateWebACL",
                json!({
                    "Name": &acl_name,
                    "Scope": REGIONAL_SCOPE,
                    "DefaultAction": { "Allow": {} },
                    "VisibilityConfig": visibility(&acl_name),
                    "MonetizationConfig": row.config.clone(),
                }),
            )
            .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the save to be refused with 400, got status {status} \
             and body {body}"
        );
        let code = body["__type"].as_str().unwrap_or_default();
        assert!(
            code.ends_with(INVALID_OPERATION_CODE),
            "case {case:?}: expected the error code {INVALID_OPERATION_CODE}, got \
             {code:?} in body {body}"
        );
        assert_eq!(
            body["Message"].as_str(),
            Some(MONETIZATION_NOT_AVAILABLE),
            "case {case:?}: expected WAF's monetization sentence for the regional \
             namespace, got {:?}",
            body["Message"]
        );
        let stored = names_in_scope(&waf, "ListWebACLs", "WebACLs", REGIONAL_SCOPE).await;
        assert!(
            !stored.contains(&acl_name),
            "case {case:?}: a refused create must store no web ACL, got {stored:?}"
        );
    }
}
