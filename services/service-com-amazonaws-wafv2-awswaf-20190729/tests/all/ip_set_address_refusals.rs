// SPDX-License-Identifier: CC0-1.0

//! WHAT an IP set's refusal for an address says, end to end over the wire.
//!
//! The rule asserted here: an `Addresses` entry WAF cannot read as a CIDR block of
//! the family the set declares is refused with HTTP 400
//! `WAFInvalidParameterException` carrying `Field` IP_ADDRESS, `Parameter` the entry
//! exactly as the request wrote it and ONE `Reason` for the whole class — the
//! sentence WAF answers for a parameter whose text is not in the form that parameter
//! takes — whatever makes the entry unreadable: text that is no block at all, a
//! prefix length outside the family's range, and a well-formed block of the OTHER
//! family alike. The refusal says nothing else about the entry, so no family name or
//! per-entry diagnosis leaks into it, and the message is composed from those three
//! members.
//!
//! `IPSet$Addresses` states the requirement the refusal is about: "All addresses must
//! be specified using Classless Inter-Domain Routing (CIDR) notation. WAF supports
//! all IPv4 and IPv6 CIDR ranges except for /0", with `IPAddressVersion` fixing the
//! family.
//!
//! Every call goes through the same front door a real client uses (the `Waf` harness
//! in [`crate::web_acl_lifecycle`]), so what these tests observe is what an SDK
//! observes.

use serde_json::{json, Value};

use crate::web_acl_lifecycle::Waf;

/// The `ParameterExceptionField` value naming an IP address
/// (`smithy_query(query="shape",shape="ParameterExceptionField")` lists `IP_ADDRESS`
/// among its values).
const FIELD_IP_ADDRESS: &str = "IP_ADDRESS";

/// The `Reason` WAF carries for a parameter whose text it cannot read in the form
/// that parameter has to take, as the recorded refusals of that class word it.
const FORMAT_NOT_VALID: &str = "The parameter contains formatting that is not valid.";

/// One entry an IP set of `version` cannot hold.
struct Refused {
    /// What the case is called in an assertion.
    case: &'static str,
    /// The family the set declares.
    version: &'static str,
    /// The entry the request submits.
    address: &'static str,
}

/// The entries every test here drives.
///
/// The rows differ along what the rule generalises over — WHY the entry is
/// unreadable (text that is no address, an address with no prefix, a prefix that is
/// not a number, a prefix past each family's published ceiling, the `/0` the doc
/// excludes, and a well-formed block of the other family, in both directions) — and
/// over both families, because the refusal is the same one for each.
fn refused() -> Vec<Refused> {
    vec![
        Refused {
            case: "text that is no address at all, with a prefix",
            version: "IPV4",
            address: "not-an-ip-address/8",
        },
        Refused {
            case: "an IPv4 address with no prefix",
            version: "IPV4",
            address: "192.0.2.0",
        },
        Refused {
            case: "an IPv4 address whose prefix is not a number",
            version: "IPV4",
            address: "192.0.2.0/eight",
        },
        Refused {
            case: "the /0 range the doc excludes",
            version: "IPV4",
            address: "0.0.0.0/0",
        },
        Refused {
            case: "one past the IPv4 prefix ceiling",
            version: "IPV4",
            address: "192.0.2.0/33",
        },
        Refused {
            case: "an IPv6 block in an IPV4 set",
            version: "IPV4",
            address: "2001:db8::/32",
        },
        Refused {
            case: "an IPv6 host address in an IPV4 set",
            version: "IPV4",
            address: "2001:db8::1/128",
        },
        Refused {
            // Inside `IPAddress`'s own `@length(1..50)` and `@pattern \S`
            // (`smithy_query(query="shape",shape="IPAddress")`), so the entry reaches
            // the operation and the judgement below is the service's own.
            case: "a prefix with no address in front of it",
            version: "IPV4",
            address: "/24",
        },
        Refused {
            case: "the /0 range of the other family",
            version: "IPV6",
            address: "2001:db8::/0",
        },
        Refused {
            case: "one past the IPv6 prefix ceiling",
            version: "IPV6",
            address: "2001:db8::/129",
        },
        Refused {
            case: "an IPv4 block in an IPV6 set",
            version: "IPV6",
            address: "192.0.2.0/24",
        },
        Refused {
            case: "an IPv6 address with no prefix",
            version: "IPV6",
            address: "2001:db8::1",
        },
    ]
}

/// Every address an accepted set holds, for the positive control of each family.
fn accepted(version: &str) -> &'static str {
    if version == "IPV6" {
        "2001:db8::/32"
    } else {
        "192.0.2.0/24"
    }
}

/// The refusal must carry the three members the model gives
/// `WAFInvalidParameterException`, and the message WAF composes from exactly those
/// three.
fn assert_address_refusal(body: &Value, address: &str, case: &str) {
    assert_eq!(
        (
            body["Field"].as_str(),
            body["Parameter"].as_str(),
            body["Reason"].as_str()
        ),
        (
            Some(FIELD_IP_ADDRESS),
            Some(address),
            Some(FORMAT_NOT_VALID)
        ),
        "case {case:?}: the refusal must name the settings, the offending entry and \
         the one reason, got {body}"
    );
    assert_eq!(
        body["message"].as_str().unwrap_or_default(),
        format!(
            "Error reason: {FORMAT_NOT_VALID}, field: {FIELD_IP_ADDRESS}, \
             parameter: {address}"
        ),
        "case {case:?}: the message must read the way WAF words it, got {body}"
    );
}

/// `CreateIPSet` refuses every unreadable entry with the one sentence, and stores
/// nothing.
///
/// The offending entry is submitted at a different position on alternate rows —
/// alone, and after an entry of the family that IS readable — so the refusal is shown
/// to report the entry at fault rather than the list's first element.
#[tokio::test]
async fn create_ip_set_refuses_an_unreadable_address_with_wafs_one_sentence() {
    let waf = Waf::new();
    for (index, Refused { case, version, address }) in refused().iter().enumerate() {
        let name = format!("address-refusal-{index}");
        let addresses: Vec<&str> = if index % 2 == 0 {
            vec![address]
        } else {
            vec![accepted(version), address]
        };
        let body = waf
            .refused(
                "CreateIPSet",
                json!({
                    "Name": &name,
                    "Scope": "REGIONAL",
                    "IPAddressVersion": version,
                    "Addresses": addresses,
                }),
                "WAFInvalidParameterException",
                case,
            )
            .await;
        assert_address_refusal(&body, address, case);
        assert!(
            body["Summary"].is_null(),
            "case {case:?}: a refusal must carry no Summary, got {body}"
        );

        // The refused create stored nothing, so the same name with a readable entry
        // is a create: the difference is the entry, not the name or the family.
        let created = waf
            .ok(
                "CreateIPSet",
                json!({
                    "Name": &name,
                    "Scope": "REGIONAL",
                    "IPAddressVersion": version,
                    "Addresses": [accepted(version)],
                }),
            )
            .await;
        assert!(
            created["Summary"]["Id"].as_str().is_some(),
            "case {case:?}: the control create must answer a Summary, got {created}"
        );
    }
}

/// `UpdateIPSet` makes the same judgement against the family the STORED set declares,
/// and a refused update changes nothing: the stored membership stands and the token
/// the caller holds still works.
#[tokio::test]
async fn update_ip_set_refuses_an_unreadable_address_with_wafs_one_sentence() {
    let waf = Waf::new();
    // One live set per family, so each row is judged against the family its stored
    // set declares — the member an update does not carry.
    let mut live: Vec<(&str, String, String)> = Vec::new();
    for version in ["IPV4", "IPV6"] {
        let name = format!("address-refusal-live-{version}");
        let created = waf
            .ok(
                "CreateIPSet",
                json!({
                    "Name": &name,
                    "Scope": "REGIONAL",
                    "IPAddressVersion": version,
                    "Addresses": [accepted(version)],
                }),
            )
            .await;
        live.push((
            version,
            created["Summary"]["Id"]
                .as_str()
                .expect("Summary.Id")
                .to_string(),
            created["Summary"]["LockToken"]
                .as_str()
                .expect("Summary.LockToken")
                .to_string(),
        ));
    }

    for Refused { case, version, address } in refused() {
        let (_, id, lock_token) = live
            .iter()
            .find(|(family, _, _)| *family == version)
            .expect("a live set of each family");
        let name = format!("address-refusal-live-{version}");
        let body = waf
            .refused(
                "UpdateIPSet",
                json!({
                    "Name": &name,
                    "Scope": "REGIONAL",
                    "Id": id,
                    "LockToken": lock_token,
                    "Addresses": [accepted(version), address],
                }),
                "WAFInvalidParameterException",
                case,
            )
            .await;
        assert_address_refusal(&body, address, case);
        assert!(
            body["NextLockToken"].is_null(),
            "case {case:?}: a refusal must carry no NextLockToken, got {body}"
        );

        let got = waf
            .ok(
                "GetIPSet",
                json!({ "Name": &name, "Scope": "REGIONAL", "Id": id }),
            )
            .await;
        assert_eq!(
            got["IPSet"]["Addresses"].as_array().map(Vec::len),
            Some(1),
            "case {case:?}: a refused update must leave the stored membership as it \
             was, got {got}"
        );
        assert_eq!(
            got["LockToken"].as_str(),
            Some(lock_token.as_str()),
            "case {case:?}: a refused update must not rotate the token, got {got}"
        );
    }
}
