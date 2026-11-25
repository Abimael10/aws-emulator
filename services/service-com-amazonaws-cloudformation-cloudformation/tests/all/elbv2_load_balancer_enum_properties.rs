// SPDX-License-Identifier: CC0-1.0

//! What a stack does with an `AWS::ElasticLoadBalancingV2::LoadBalancer` whose declared
//! property reaches a `CreateLoadBalancer` member that targets a CLOSED enum.
//!
//! RULE under test: **a property that reaches a member of the owning service's create whose
//! target shape names a fixed value set carries only one of those values — a declaration
//! outside the set is REFUSED by that service, which fails the resource (`CREATE_FAILED`,
//! its `ResourceStatusReason` quoting that service's own refusal and the diagnostic its SDK
//! appends) and rolls the stack back with nothing created, while a declaration inside the set
//! deploys and is what the owning service then reports.**
//!
//! Why it matters: `Scheme` decides which clients can reach the load balancer — and with it
//! its public resolvability, its subnet and its internet-gateway requirements — so a stack
//! that reaches `CREATE_COMPLETE` for a scheme Elastic Load Balancing would never have
//! accepted tells a client it deployed a load balancer that cannot exist as described, and
//! template validation in CI, which keys on the stack reaching a failed state, passes.
//!
//! The rows differ along what the rule generalises over: WHICH property carries the value
//! (`Scheme`, `Type`, `IpAddressType` — the create's closed-enum members) and the FORM of the
//! value outside the set (a plausible spelling, a value of the set in another case, a value of
//! another API). The control at the end is the same template with every member inside its set.

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cloudformation_cloudformation::state::status;
// The schemes a load balancer may be given: the whole value set
// `CreateLoadBalancer$Scheme` targets, read from the owning service's own model-generated
// catalogue (its `src/ops/types/shared_types.rs`) rather than restated here — so a refusal of
// a value outside it names exactly these, and a declaration of any of them deploys.
use service_com_amazonaws_elasticloadbalancingv2_elasticloadbalancing_v10::ops::types::LOAD_BALANCER_SCHEME_ENUM_VALUES as SCHEMES;

use crate::elbv2_resource_provisioning::NETWORK;
use crate::harness::{
    assert_sdk_diagnostic_with_attempts, failed_resource_reason, quoted_and_class,
};
use crate::stack_lifecycle::{
    create_stack, elbv2, member, router, settled_stack, xml_text, REGION,
};

/// The logical id of the load balancer every template below declares, and the status it
/// records when the owning service refused its create.
const ALB: &str = "Alb";
const CREATE_FAILED: &str = "CREATE_FAILED";

/// The SDK's own name for Elastic Load Balancing v2, as the exception its SDK raises states
/// it, and the status that service answers a request it will not carry out with.
const ELBV2_SERVICE: &str = "ElasticLoadBalancingV2";
const REFUSED_STATUS: u16 = 400;

/// The `HandlerErrorCode` class a refusal of the REQUEST falls in — one of the model's own
/// `HandlerErrorCode` values.
const INVALID_REQUEST: &str = "InvalidRequest";

/// The code a read of a load balancer that is not there is refused with: the `awsQueryError`
/// of that service's `LoadBalancerNotFoundException`, which is what says the failed resource
/// left nothing behind.
const LOAD_BALANCER_NOT_FOUND: &str = "LoadBalancerNotFound";

/// The fixed text of the constraint clause a value outside a declared value set earns.
const ENUM_CLAUSE: &str = "failed to satisfy constraint: Member must satisfy enum value set:";

/// One declaration the owning service refuses: the stack to deploy, the load balancer's name,
/// the property that carries the value, the member PATH that service's refusal names it by,
/// and the value outside the set.
struct Row {
    case: &'static str,
    stack: &'static str,
    name: &'static str,
    property: &'static str,
    path: &'static str,
    value: &'static str,
}

const ROWS: &[Row] = &[
    Row {
        case: "a load balancer whose declared Scheme is outside the set the member admits",
        stack: "elbv2-scheme-outside",
        name: "scheme-outside-alb",
        property: "Scheme",
        path: "scheme",
        value: "internet-public",
    },
    Row {
        case: "a load balancer whose declared Scheme is a value of the set in upper case",
        stack: "elbv2-scheme-upper",
        name: "scheme-upper-alb",
        property: "Scheme",
        path: "scheme",
        value: "INTERNAL",
    },
    Row {
        case: "a load balancer whose declared Type is outside the families the model names",
        stack: "elbv2-type-outside",
        name: "type-outside-alb",
        property: "Type",
        path: "type",
        value: "classic",
    },
    Row {
        case: "a load balancer whose declared IpAddressType is outside the set the member \
               admits",
        stack: "elbv2-address-outside",
        name: "address-outside-alb",
        property: "IpAddressType",
        path: "ipAddressType",
        value: "ipv6",
    },
];

/// A template declaring the network, and in it one load balancer carrying `properties`.
fn alb_template(name: &str, properties: &[(&str, &str)]) -> String {
    let mut declared = format!(
        "  {ALB}:\n    Type: AWS::ElasticLoadBalancingV2::LoadBalancer\n    Properties:\n      \
         Name: {name}\n"
    );
    for (property, value) in properties {
        declared.push_str(&format!("      {property}: {value}\n"));
    }
    declared.push_str("      Subnets:\n        - !Ref SubnetA\n        - !Ref SubnetB\n");
    ["Resources:\n", NETWORK, &declared].concat()
}

/// What Elastic Load Balancing reports for the load balancer called `name`: the body of the
/// read, or the error code it refused the read with.
async fn described(
    case: &str,
    router: &MultiServiceRouter,
    name: &str,
) -> Result<String, (u16, String)> {
    let (code, body) = elbv2(
        router,
        "DescribeLoadBalancers",
        &[member("Names.member.1", name)],
    )
    .await;
    if code == 200 {
        return Ok(body);
    }
    let reported = xml_text(&body, "Code").unwrap_or_else(|| {
        panic!("case {case:?}: expected an awsQuery error code, got {code} {body}")
    });
    Err((code, reported))
}

#[tokio::test]
async fn a_declared_value_outside_a_closed_enum_fails_the_resource_and_rolls_the_stack_back() {
    for row in ROWS {
        let case = row.case;
        let router = router();
        let template = alb_template(row.name, &[(row.property, row.value)]);
        let (code, created) = create_stack(&router, row.stack, &template, &[]).await;
        assert_eq!(
            code, 200,
            "case {case:?}: CreateStack must answer 200, got {code} {created}"
        );
        let described_stack = settled_stack(&router, case, row.stack).await;
        assert_eq!(
            xml_text(&described_stack, "StackStatus").as_deref(),
            Some(status::ROLLBACK_COMPLETE),
            "case {case:?}: expected the refused create to roll back, got {described_stack}"
        );

        // The failed resource reports the refusal the owning service answered: its own
        // constraint clause, naming the member and echoing the value the template declared,
        // inside the diagnostic that service's SDK appends and under the class for a refused
        // request.
        let reason =
            failed_resource_reason(case, &router, REGION, row.stack, ALB, CREATE_FAILED).await;
        let (quoted, class) = quoted_and_class(case, &reason);
        assert!(
            quoted.contains(&format!("Value '{}' at '{}' {ENUM_CLAUSE}", row.value, row.path)),
            "case {case:?}: expected the owning service's clause naming the member and the \
             value it refused, got {quoted:?}"
        );
        assert_eq!(
            class, INVALID_REQUEST,
            "case {case:?}: expected the {INVALID_REQUEST} class, got {class:?} in {reason:?}"
        );
        assert_sdk_diagnostic_with_attempts(case, &quoted, ELBV2_SERVICE, REFUSED_STATUS);

        // And nothing was created for it: the load balancer the template named is in no read
        // of the service that would own it.
        assert_eq!(
            described(case, &router, row.name).await,
            Err((REFUSED_STATUS, LOAD_BALANCER_NOT_FOUND.to_string())),
            "case {case:?}: a failed resource must leave no load balancer behind"
        );
    }
}

#[tokio::test]
async fn the_refusal_of_a_scheme_outside_the_set_names_every_scheme_a_load_balancer_may_have() {
    let case = "the schemes the refusal names";
    let router = router();
    let stack = "elbv2-scheme-set";
    let template = alb_template("scheme-set-alb", &[("Scheme", "internet-public")]);
    let (code, created) = create_stack(&router, stack, &template, &[]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: CreateStack must answer 200, got {code} {created}"
    );
    let described_stack = settled_stack(&router, case, stack).await;
    assert_eq!(
        xml_text(&described_stack, "StackStatus").as_deref(),
        Some(status::ROLLBACK_COMPLETE),
        "case {case:?}: expected the refused create to roll back, got {described_stack}"
    );
    let reason = failed_resource_reason(case, &router, REGION, stack, ALB, CREATE_FAILED).await;
    let (quoted, _) = quoted_and_class(case, &reason);
    for scheme in SCHEMES {
        assert!(
            quoted.contains(scheme),
            "case {case:?}: expected the refusal to name {scheme:?}, got {quoted:?}"
        );
    }
}

#[tokio::test]
async fn a_declared_scheme_inside_the_set_deploys_and_is_what_the_service_reports() {
    // Both values of the set, each on its own stack: the declaration is accepted and the load
    // balancer the stack created carries the scheme it asked for.
    for scheme in SCHEMES {
        let case = scheme;
        let router = router();
        let stack = format!("elbv2-scheme-{}", scheme.replace('-', ""));
        let name = format!("scheme-in-{}-alb", scheme.replace('-', ""));
        let template = alb_template(&name, &[("Scheme", scheme), ("Type", "application")]);
        let (code, created) = create_stack(&router, &stack, &template, &[]).await;
        assert_eq!(
            code, 200,
            "case {case:?}: CreateStack must answer 200, got {code} {created}"
        );
        let described_stack = settled_stack(&router, case, &stack).await;
        assert_eq!(
            xml_text(&described_stack, "StackStatus").as_deref(),
            Some(status::CREATE_COMPLETE),
            "case {case:?}: expected the declaration to deploy, got {described_stack}"
        );
        let reported = described(case, &router, &name)
            .await
            .unwrap_or_else(|refused| {
                panic!("case {case:?}: expected the deployed load balancer, got {refused:?}")
            });
        assert_eq!(
            xml_text(&reported, "Scheme").as_deref(),
            Some(*scheme),
            "case {case:?}: expected the declared scheme to be the stored one, got {reported}"
        );
    }
}
