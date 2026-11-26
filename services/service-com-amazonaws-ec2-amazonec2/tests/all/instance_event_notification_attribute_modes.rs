// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the MODES of the instance event notification tag attribute — which
//! member of `InstanceTagAttribute` decides what a register or a deregister applies — driven
//! over the wire the service actually speaks: `ec2Query`, through the crate's shared
//! [`super::ec2_query`] harness.
//!
//! The rule under test: the attribute requires AT LEAST ONE of its two members, so a request
//! that names both is served rather than refused. The flag carrying the value that ASKS for
//! something — `true` on a register ("specify `true` to register all tag keys"), `false` on a
//! deregister ("specify `false` to deregister all tag keys") — applies to every tag key of the
//! Region; named KEYS are what a request that names them asks for, so a request carrying both
//! applies to exactly those keys on a deregister and is subsumed by the register's include-all
//! mode. Only a request that names no key and no flag asking for all of them asks for nothing.
//!
//! The rows differ along what the rule generalises over: which member the request names, which
//! value the flag carries, whether keys are named beside it, how many, and what the Region
//! held first — so no row is one recorded request.

use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, elements, error_code, indexed};
use super::wire::{child_text, document, param, Param};

/// The Region these calls are signed for; a registration is a fact of one Region.
const REGION: &str = "us-west-2";

/// A second Region, so a row proves one Region's registration is not another's.
const OTHER_REGION: &str = "eu-central-1";

/// `RegisterInstanceEventNotificationAttributes`.
const REGISTER: &str = "RegisterInstanceEventNotificationAttributes";

/// `DeregisterInstanceEventNotificationAttributes`.
const DEREGISTER: &str = "DeregisterInstanceEventNotificationAttributes";

/// `DescribeInstanceEventNotificationAttributes`.
const DESCRIBE: &str = "DescribeInstanceEventNotificationAttributes";

/// The form fields naming `keys` individually: ec2Query flattens `InstanceTagKeys` under its
/// `InstanceTagKey` XML name, inside the `InstanceTagAttribute` structure.
fn keys(keys: &[&str]) -> Vec<Param> {
    indexed("InstanceTagAttribute.InstanceTagKey", keys)
}

/// The form field naming the include-all flag with `value`.
fn flag(value: &str) -> Param {
    param("InstanceTagAttribute.IncludeAllTagsOfInstance", value)
}

/// One call that has to succeed.
async fn ok(case: &str, router: &MultiServiceRouter, region: &str, action: &str, params: &[Param]) -> String {
    let (status, body) = call(router, region, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// What `region` holds now: the include-all flag and the registered keys, sorted so the
/// comparison is about the SET the service holds.
async fn held(case: &str, router: &MultiServiceRouter, region: &str) -> (bool, Vec<String>) {
    let body = ok(case, router, region, DESCRIBE, &[]).await;
    let parsed = document(case, &body);
    let attribute = parsed
        .descendants()
        .find(|node| node.is_element() && node.tag_name().name() == "instanceTagAttribute")
        .unwrap_or_else(|| panic!("case {case:?}: no <instanceTagAttribute> in {body}"));
    let include_all = child_text(attribute, "includeAllTagsOfInstance")
        .unwrap_or_else(|| panic!("case {case:?}: no <includeAllTagsOfInstance> in {body}"));
    let mut registered = elements(&body, "item");
    registered.sort();
    (include_all == "true", registered)
}

/// The keys of `values`, sorted — what [`held`] is compared against.
fn sorted(values: &[&str]) -> Vec<String> {
    let mut sorted: Vec<String> = values.iter().map(|value| (*value).to_string()).collect();
    sorted.sort();
    sorted
}

/// One row of the register table: the case, the request's form fields, the include-all mode it
/// leaves on and the keys it leaves registered.
type RegisterRow = (&'static str, Vec<Param>, bool, Vec<String>);

/// One row of the deregister table: the case, what is registered first, the deregister's form
/// fields and the keys left afterwards.
type DeregisterRow = (&'static str, Vec<Param>, Vec<Param>, Vec<String>);

/// A register applies the member that ASKS for something, whichever members the request
/// names.
#[tokio::test]
async fn a_register_applies_the_member_that_asks_for_something() {
    let cases: [RegisterRow; 5] = [
        ("one key", keys(&["Name"]), false, sorted(&["Name"])),
        (
            "three keys at once",
            keys(&["Name", "Stage", "cost.center/team-1"]),
            false,
            sorted(&["Name", "Stage", "cost.center/team-1"]),
        ),
        ("every tag key", vec![flag("true")], true, Vec::new()),
        (
            "the flag's other value beside keys, which leaves the keys to decide",
            {
                let mut params = vec![flag("false")];
                params.extend(keys(&["Stage"]));
                params
            },
            false,
            sorted(&["Stage"]),
        ),
        (
            "every tag key AND a key, which the flag subsumes",
            {
                let mut params = vec![flag("true")];
                params.extend(keys(&["Stage"]));
                params
            },
            true,
            Vec::new(),
        ),
    ];

    for (case, params, include_all, expected) in cases {
        let (router, _engine) = super::ec2_query::emulator();
        ok(case, &router, REGION, REGISTER, &params).await;
        let (held_all, held_keys) = held(case, &router, REGION).await;
        assert_eq!(
            (held_all, held_keys.clone()),
            (include_all, expected.clone()),
            "case {case:?}: expected the Region to hold ({include_all}, {expected:?}), got \
             ({held_all}, {held_keys:?})"
        );
        // One Region's registration is not another's: the Region nothing registered in holds
        // nothing at all.
        let (other_all, other_keys) = held(case, &router, OTHER_REGION).await;
        assert_eq!(
            (other_all, other_keys.clone()),
            (false, Vec::<String>::new()),
            "case {case:?}: another Region must hold nothing, got ({other_all}, {other_keys:?})"
        );
    }
}

/// A deregister applies the member that ASKS for something, whichever members the request
/// names — and the keys it reports are the ones it took out.
#[tokio::test]
async fn a_deregister_applies_the_member_that_asks_for_something() {
    let cases: [DeregisterRow; 6] = [
        (
            "the one key that was registered",
            keys(&["Name"]),
            keys(&["Name"]),
            Vec::new(),
        ),
        (
            "one of three keys",
            keys(&["Name", "Stage", "Tier"]),
            keys(&["Stage"]),
            sorted(&["Name", "Tier"]),
        ),
        (
            "a key nothing registered",
            keys(&["Name"]),
            keys(&["Absent"]),
            sorted(&["Name"]),
        ),
        (
            "every tag key, asked for with the flag's documented value",
            keys(&["Name", "Stage"]),
            vec![flag("false")],
            Vec::new(),
        ),
        (
            "the flag beside a key, which takes out the key it named and no other",
            keys(&["Name", "Stage"]),
            {
                let mut params = vec![flag("false")];
                params.extend(keys(&["Name"]));
                params
            },
            sorted(&["Stage"]),
        ),
        (
            "the flag's other value beside a key, which the key decides",
            keys(&["Name", "Stage"]),
            {
                let mut params = vec![flag("true")];
                params.extend(keys(&["Stage"]));
                params
            },
            sorted(&["Name"]),
        ),
    ];

    for (case, seed, params, expected) in cases {
        let (router, _engine) = super::ec2_query::emulator();
        ok(case, &router, REGION, REGISTER, &seed).await;
        ok(case, &router, REGION, DEREGISTER, &params).await;
        let (held_all, held_keys) = held(case, &router, REGION).await;
        assert_eq!(
            held_keys, expected,
            "case {case:?}: expected the Region to be left holding {expected:?}, got \
             {held_keys:?}"
        );
        assert!(
            !held_all,
            "case {case:?}: a deregister never turns the include-all mode on, got {held_all}"
        );
    }
}

/// A request that names NEITHER member of the attribute asks for nothing at all, and is
/// refused with EC2's code for a parameter the request left out — which is the one refusal
/// either operation makes about the attribute's members. The flag carrying the value that
/// asks for nothing on that operation names nothing either.
#[tokio::test]
async fn a_request_naming_neither_member_is_refused() {
    let (router, _engine) = super::ec2_query::emulator();
    // (case, the action, the fields)
    let cases: [(&str, &str, Vec<Param>); 5] = [
        ("a register with no attribute at all", REGISTER, Vec::new()),
        ("a deregister with no attribute at all", DEREGISTER, Vec::new()),
        (
            "a register whose flag asks for nothing and names no key",
            REGISTER,
            vec![flag("false")],
        ),
        (
            "a deregister whose flag asks for nothing and names no key",
            DEREGISTER,
            vec![flag("true")],
        ),
        (
            "a register with an empty key list and no flag",
            REGISTER,
            indexed("InstanceTagAttribute.InstanceTagKey", &[]),
        ),
    ];
    for (case, action, params) in cases {
        let (status, body) = call(&router, REGION, action, &params).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 from {action}, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            ec2::state::MISSING_PARAMETER,
            "case {case:?}: expected {} from {action}, got {body}",
            ec2::state::MISSING_PARAMETER
        );
    }
}
