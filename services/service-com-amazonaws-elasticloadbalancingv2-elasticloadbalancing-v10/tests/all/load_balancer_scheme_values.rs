// SPDX-License-Identifier: CC0-1.0

//! Which values `CreateLoadBalancer` admits for a member whose target shape is a CLOSED
//! enum, over the wire.
//!
//! One rule: a member that targets an enum shape admits only the values that shape names, so
//! a request carrying anything else is refused `400 ValidationError` with the constraint
//! clause naming the member, echoing the value it sent and listing the whole value set — and
//! NOTHING is minted for it, so the name the refused request asked for is still free. A value
//! INSIDE the set is accepted and is what a later read reports.
//!
//! `Scheme` (`LoadBalancerSchemeEnum`) is the member these rows are written for — it decides
//! which clients can reach the load balancer, so a load balancer stored with a scheme outside
//! that set is one that could not exist — and `Type` (`LoadBalancerTypeEnum`) and
//! `IpAddressType` (`IpAddressType`) are the create's other closed-enum members, which the
//! same rule covers and which the rows below cross.
//!
//! The rows differ along what the rule generalises over: WHICH member carries the value, and
//! the FORM of the value outside the set (a value of the set in another case, a plausible
//! synonym, a near-miss spelling, a value of another API, and the empty string). The value
//! sets themselves are read from the model's own generated catalogues
//! (`src/ops/types/shared_types.rs`), never restated here.
//!
//! The wire harness (`emulator`, `call`, `action`, `p`) comes from `provisioning_loop`, the
//! creator of a load balancer from `load_balancer_attributes` and the message reader from
//! `target_group_attributes_and_modification`; nothing here restates them.

use service_com_amazonaws_elasticloadbalancingv2_elasticloadbalancing_v10::ops::types::{
    IP_ADDRESS_TYPE_VALUES, LOAD_BALANCER_SCHEME_ENUM_VALUES, LOAD_BALANCER_TYPE_ENUM_VALUES,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::load_balancer_attributes::create_load_balancer;
use crate::provisioning_loop::{action, call, emulator, p};
use crate::target_group_attributes_and_modification::unescaped;

/// The awsQuery error code Elastic Load Balancing answers a value outside a declared value
/// set with, and the status it answers it under.
const VALIDATION_ERROR: &str = "ValidationError";
const REFUSED_STATUS: u16 = 400;

/// The code a read of a load balancer that is not there is refused with — the `awsQueryError`
/// of `LoadBalancerNotFoundException`, which is what says a refused create minted nothing.
const LOAD_BALANCER_NOT_FOUND: &str = "LoadBalancerNotFound";

/// The fixed text of the constraint clause an enum member's refusal carries, as the boundary
/// validator words it for every member of every operation.
const ENUM_CLAUSE: &str = "failed to satisfy constraint: Member must satisfy enum value set:";

/// One refusal: the case, the member the value was sent on, the member PATH the clause names
/// it by (the member's own name as the walk reports it), the value outside the set, and the
/// set that member admits.
struct Row {
    case: &'static str,
    member: &'static str,
    path: &'static str,
    value: &'static str,
    admits: &'static [&'static str],
}

const ROWS: &[Row] = &[
    Row {
        case: "a scheme of the set in upper case",
        member: "Scheme",
        path: "scheme",
        value: "INTERNET-FACING",
        admits: LOAD_BALANCER_SCHEME_ENUM_VALUES,
    },
    Row {
        case: "a scheme of the set in mixed case",
        member: "Scheme",
        path: "scheme",
        value: "Internal",
        admits: LOAD_BALANCER_SCHEME_ENUM_VALUES,
    },
    Row {
        case: "a plausible synonym of a scheme",
        member: "Scheme",
        path: "scheme",
        value: "public",
        admits: LOAD_BALANCER_SCHEME_ENUM_VALUES,
    },
    Row {
        case: "a scheme spelled with an underscore",
        member: "Scheme",
        path: "scheme",
        value: "internet_facing",
        admits: LOAD_BALANCER_SCHEME_ENUM_VALUES,
    },
    Row {
        case: "a scheme no API of this kind publishes",
        member: "Scheme",
        path: "scheme",
        value: "edge-optimized",
        admits: LOAD_BALANCER_SCHEME_ENUM_VALUES,
    },
    Row {
        case: "an empty scheme",
        member: "Scheme",
        path: "scheme",
        value: "",
        admits: LOAD_BALANCER_SCHEME_ENUM_VALUES,
    },
    Row {
        case: "a type outside the families the model names",
        member: "Type",
        path: "type",
        value: "classic",
        admits: LOAD_BALANCER_TYPE_ENUM_VALUES,
    },
    Row {
        case: "an address type outside the set the member admits",
        member: "IpAddressType",
        path: "ipAddressType",
        value: "ipv6",
        admits: IP_ADDRESS_TYPE_VALUES,
    },
];

/// Ask `CreateLoadBalancer` for a load balancer called `name` with one extra member set.
///
/// The placement is the two subnets every family can be placed in, exactly as the crate's
/// shared creator uses ([`create_load_balancer`]); what a row varies is the member under
/// test.
async fn create_with(
    router: &MultiServiceRouter,
    name: &str,
    member: &str,
    value: &str,
) -> crate::provisioning_loop::Reply {
    let mut form = action("CreateLoadBalancer");
    form.push(p("Name", name));
    form.push(p("Subnets.member.1", "subnet-11111111"));
    form.push(p("Subnets.member.2", "subnet-22222222"));
    form.push(p(member, value));
    call(router, &form).await
}

/// What `DescribeLoadBalancers` reports for the load balancer called `name`: its
/// `(Scheme, Type)`, or the error code the read was refused with.
async fn described(
    router: &MultiServiceRouter,
    name: &str,
) -> Result<(String, String), (u16, String)> {
    let mut form = action("DescribeLoadBalancers");
    form.push(p("Names.member.1", name));
    let reply = call(router, &form).await;
    if reply.status != 200 {
        return Err((reply.status, reply.error_code()));
    }
    Ok((
        reply.text("Scheme").unwrap_or_default(),
        reply.text("Type").unwrap_or_default(),
    ))
}

#[tokio::test]
async fn a_value_outside_a_closed_enum_is_refused_and_mints_nothing() {
    let (router, _engine) = emulator();

    for row in ROWS {
        let case = row.case;
        let name = "scheme-rules-lb";
        let reply = create_with(&router, name, row.member, row.value).await;
        assert_eq!(
            (reply.status, reply.error_code().as_str()),
            (REFUSED_STATUS, VALIDATION_ERROR),
            "case {case:?}: expected {REFUSED_STATUS} {VALIDATION_ERROR}, got {} body {}",
            reply.status,
            reply.body
        );
        let stated = reply
            .text("Message")
            .map(|text| unescaped(&text))
            .unwrap_or_else(|| panic!("case {case:?}: no message in {}", reply.body));
        assert!(
            stated.contains(&format!("Value '{}' at '{}' {ENUM_CLAUSE}", row.value, row.path)),
            "case {case:?}: expected the clause naming the member and echoing the value, got \
             {stated:?}"
        );
        for admitted in row.admits {
            assert!(
                stated.contains(admitted),
                "case {case:?}: expected the clause to list {admitted:?}, got {stated:?}"
            );
        }

        // Nothing was minted for the refused request: the name it asked for answers no read,
        // and so is still free for the create below.
        assert_eq!(
            described(&router, name).await,
            Err((REFUSED_STATUS, LOAD_BALANCER_NOT_FOUND.to_string())),
            "case {case:?}: a refused create must mint no load balancer"
        );
    }

    // And the name every refusal left free takes a load balancer whose members are all inside
    // the sets they target.
    let arn = create_load_balancer(
        &router,
        "scheme-rules-lb",
        &[("Scheme", "internal"), ("Type", "application")],
    )
    .await;
    assert!(
        arn.contains("loadbalancer/app/scheme-rules-lb/"),
        "case after the refusals: expected the create to mint the load balancer, got {arn:?}"
    );
}

#[tokio::test]
async fn every_scheme_the_model_names_is_accepted_and_reported() {
    let (router, _engine) = emulator();

    // The whole value set, from the model's own catalogue: each is accepted, and each is what
    // a later read reports for the load balancer it was created with.
    for scheme in LOAD_BALANCER_SCHEME_ENUM_VALUES {
        let name = format!("scheme-{}", scheme.replace('-', ""));
        let arn = create_load_balancer(&router, &name, &[("Scheme", scheme)]).await;
        assert!(
            !arn.is_empty(),
            "case scheme {scheme:?}: expected an ARN for the created load balancer"
        );
        assert_eq!(
            described(&router, &name).await,
            Ok(((*scheme).to_string(), "application".to_string())),
            "case scheme {scheme:?}: expected the stored scheme to be the one asked for"
        );
    }
}
