// SPDX-License-Identifier: CC0-1.0

//! What a rule's condition VALUES must be to be stored at all: a regular expression this
//! service can compile, and no more wildcards than one rule may carry.
//!
//! Two rules are asserted here, each over a SET of inputs rather than over the one call a
//! recording happens to carry:
//!
//! * `RegexValues` is a member the service COMPILES — "the regular expressions to compare
//!   against the request URL" — so a value that compiles to no program matches no request
//!   and the operations that store a rule refuse it with this API's own `ValidationError`,
//!   whichever field and whichever of the two spellings carries it;
//! * the WILDCARDS of a rule's condition values ("* (matches 0 or more characters) and ?
//!   (matches exactly 1 character)") are bounded for the rule as a whole, so a rule past
//!   that number is refused and one at it is accepted and echoed back unchanged.
//!
//! Both are judged wherever a rule's conditions are SET, so every row is driven through
//! `CreateRule` and through the `ModifyRule` that replaces a stored rule's conditions; after
//! every refusal the listener's rules are read again to prove nothing was stored or changed.
//!
//! What a stored expression then MATCHES is the data plane's half of the same reading, and is
//! pinned in `all/alb_data_plane.rs`.

use crate::listener_rules::{
    add_rule, create_listener, create_load_balancer, create_rule, fixed_response, rules_of, Form,
};
use crate::provisioning_loop::{action, call, create_http_target_group, emulator, members, p, Reply};
use crate::trust_store_lifecycle::message;
use service_com_amazonaws_elasticloadbalancingv2_elasticloadbalancing_v10::state::{
    condition_wildcards_message, CONDITION_REGEX_MESSAGE, CONDITION_WILDCARDS_PER_RULE,
};
use smithy_server_runtime::MultiServiceRouter;

/// The error code this service answers an input its own documented rules refuse with.
const VALIDATION_ERROR: &str = "ValidationError";

/// Where a condition carries the values of one row: the top-level member the model gives
/// some fields, or the field's own configuration shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Spelling {
    /// `Conditions.member.1.<Member>`.
    TopLevel,
    /// `Conditions.member.1.<Config>.<Member>`.
    Configured,
}

/// The configuration shape of one condition field, as the request spells it.
fn config_of(field: &str) -> &'static str {
    match field {
        "host-header" => "HostHeaderConfig",
        "path-pattern" => "PathPatternConfig",
        "http-header" => "HttpHeaderConfig",
        "query-string" => "QueryStringConfig",
        other => panic!("no configuration shape is spelled for the field {other:?}"),
    }
}

/// The request fields of one condition on `field` carrying `values` under `member`, spelled
/// as `spelling` says.
fn condition(field: &str, spelling: Spelling, member: &str, values: &[&str]) -> Form {
    let mut form = vec![p("Conditions.member.1.Field", field)];
    if field == "http-header" {
        // The header a condition matches ON is named inside the configuration shape
        // whichever member carries the values.
        form.push(p(
            "Conditions.member.1.HttpHeaderConfig.HttpHeaderName",
            "x-version",
        ));
    }
    for (index, value) in values.iter().enumerate() {
        let key = match spelling {
            Spelling::TopLevel => format!("Conditions.member.1.{member}.member.{}", index + 1),
            Spelling::Configured => format!(
                "Conditions.member.1.{}.{member}.member.{}",
                config_of(field),
                index + 1
            ),
        };
        form.push(p(&key, value));
    }
    form
}

/// A load balancer, a target group and one HTTP listener on `port`, for a suite that needs
/// somewhere to hang rules.
async fn listener_on(router: &MultiServiceRouter, name: &str, port: i32) -> String {
    let load_balancer = create_load_balancer(router, name).await;
    let group = create_http_target_group(router, &format!("{name}-tg")).await;
    create_listener(router, &load_balancer, &group, port).await
}

/// Replace the conditions of the rule `rule_arn` names with `fields`.
async fn modify_rule(router: &MultiServiceRouter, rule_arn: &str, fields: &Form) -> Reply {
    let mut form = action("ModifyRule");
    form.push(p("RuleArn", rule_arn));
    form.extend(fields.iter().cloned());
    call(router, &form).await
}

/// One row of the expression table: the case, the condition field, which spelling carries
/// the expressions, the expressions themselves, and whether the rule is stored.
struct RegexRow {
    case: &'static str,
    field: &'static str,
    spelling: Spelling,
    values: &'static [&'static str],
    accepted: bool,
}

/// The rows span what the rule generalises over: each field the model gives a `RegexValues`
/// spelling, both spellings of the member, the forms a valid expression takes, each way one
/// can fail to compile, where in the list the broken one sits, and how many expressions the
/// condition carries.
const REGEX_ROWS: &[RegexRow] = &[
    RegexRow {
        case: "a path pattern expression, configured",
        field: "path-pattern",
        spelling: Spelling::Configured,
        values: &["^/api/v[0-9]+/items$"],
        accepted: true,
    },
    RegexRow {
        case: "a path pattern expression, top-level",
        field: "path-pattern",
        spelling: Spelling::TopLevel,
        values: &["^/api/(red|green|blue)$"],
        accepted: true,
    },
    RegexRow {
        case: "a host header expression",
        field: "host-header",
        spelling: Spelling::Configured,
        values: &[r"^shop\.[a-z]+\.example\.com$"],
        accepted: true,
    },
    RegexRow {
        case: "an http header expression",
        field: "http-header",
        spelling: Spelling::Configured,
        values: &["^v[0-9]+$"],
        accepted: true,
    },
    RegexRow {
        case: "two expressions, both of which compile",
        field: "path-pattern",
        spelling: Spelling::Configured,
        values: &["^/one$", "^/two/[0-9]+$"],
        accepted: true,
    },
    RegexRow {
        case: "an unclosed group, configured",
        field: "path-pattern",
        spelling: Spelling::Configured,
        values: &["^/bad/(unclosed"],
        accepted: false,
    },
    RegexRow {
        case: "an unclosed group, top-level",
        field: "path-pattern",
        spelling: Spelling::TopLevel,
        values: &["^/bad/(unclosed"],
        accepted: false,
    },
    RegexRow {
        case: "an unclosed character class on a host header",
        field: "host-header",
        spelling: Spelling::Configured,
        values: &["^[a-z"],
        accepted: false,
    },
    RegexRow {
        case: "a repetition with nothing to repeat on an http header",
        field: "http-header",
        spelling: Spelling::Configured,
        values: &["*v1"],
        accepted: false,
    },
    RegexRow {
        case: "a look-around no linear-time engine compiles",
        field: "path-pattern",
        spelling: Spelling::Configured,
        values: &["^(?=/api)/api/items$"],
        accepted: false,
    },
    RegexRow {
        case: "the broken expression second in the list",
        field: "path-pattern",
        spelling: Spelling::Configured,
        values: &["^/one$", "^/two/(unclosed"],
        accepted: false,
    },
];

/// RULE: a condition's `RegexValues` are stored only when every one of them compiles — on
/// the create that first stores a rule's conditions and on the modification that replaces
/// them.
#[tokio::test]
async fn a_condition_expression_is_stored_only_when_it_compiles() {
    let (router, _engine) = emulator();
    let listener = listener_on(&router, "condition-regex", 80).await;
    // One rule to MODIFY, so the same clause is asserted on both operations.
    let existing = add_rule(
        &router,
        "the rule a modification replaces the conditions of",
        &listener,
        1,
        &[
            condition("path-pattern", Spelling::Configured, "Values", &["/keep"]),
            fixed_response("404"),
        ]
        .concat(),
    )
    .await;

    for (index, row) in REGEX_ROWS.iter().enumerate() {
        let priority = i32::try_from(index + 10).expect("a rule priority");
        let fields = [
            condition(row.field, row.spelling, "RegexValues", row.values),
            fixed_response("404"),
        ]
        .concat();
        let before = rules_of(&router, row.case, &listener).await;

        let created = create_rule(&router, &listener, &priority.to_string(), &fields).await;
        if row.accepted {
            assert_eq!(
                created.status, 200,
                "case {} on CreateRule: expected 200, got {} {}",
                row.case, created.status, created.body
            );
            // The expressions come back exactly as they were supplied.
            let stored = members(&created.body, "RegexValues");
            assert_eq!(
                stored,
                row.values
                    .iter()
                    .map(|value| (*value).to_string())
                    .collect::<Vec<String>>(),
                "case {} on CreateRule: expected the expressions echoed back, got {}",
                row.case,
                created.body
            );
        } else {
            assert_eq!(
                (created.status, created.error_code().as_str()),
                (400, VALIDATION_ERROR),
                "case {} on CreateRule: expected 400 {VALIDATION_ERROR}, got {} {}",
                row.case,
                created.status,
                created.body
            );
            assert_eq!(
                message(&created),
                CONDITION_REGEX_MESSAGE,
                "case {} on CreateRule: expected the refusal an expression that compiles \
                 nowhere earns, got {}",
                row.case,
                created.body
            );
            let after = rules_of(&router, row.case, &listener).await;
            assert_eq!(
                after, before,
                "case {} on CreateRule: a refused create stores nothing, got {after:?}",
                row.case
            );
        }

        // The same clause on ModifyRule, which replaces a stored rule's conditions.
        let modified = modify_rule(&router, &existing, &fields).await;
        if row.accepted {
            assert_eq!(
                modified.status, 200,
                "case {} on ModifyRule: expected 200, got {} {}",
                row.case, modified.status, modified.body
            );
        } else {
            assert_eq!(
                (modified.status, modified.error_code().as_str()),
                (400, VALIDATION_ERROR),
                "case {} on ModifyRule: expected 400 {VALIDATION_ERROR}, got {} {}",
                row.case,
                modified.status,
                modified.body
            );
            assert_eq!(
                message(&modified),
                CONDITION_REGEX_MESSAGE,
                "case {} on ModifyRule: expected the refusal an expression that compiles \
                 nowhere earns, got {}",
                row.case,
                modified.body
            );
        }
    }
}

/// One row of the wildcard table: the case, the condition fields the rule carries with the
/// values each matches on, and how many wildcards they hold in total.
struct WildcardRow {
    case: &'static str,
    /// One entry per condition: the field, the member spelling and the values.
    conditions: &'static [(&'static str, Spelling, &'static [&'static str])],
    wildcards: usize,
}

/// The rows span what the rule generalises over: how many wildcards the values hold (none,
/// one, the published limit, one past it and well past it), which character they are, which
/// spelling carries them, how many values one condition matches on, and how many conditions
/// the wildcards are spread across.
const WILDCARD_ROWS: &[WildcardRow] = &[
    WildcardRow {
        case: "no wildcard at all",
        conditions: &[("path-pattern", Spelling::Configured, &["/api/items"])],
        wildcards: 0,
    },
    WildcardRow {
        case: "one wildcard",
        conditions: &[("path-pattern", Spelling::Configured, &["/img/*"])],
        wildcards: 1,
    },
    WildcardRow {
        case: "both wildcard characters",
        conditions: &[("path-pattern", Spelling::Configured, &["/img/?/*"])],
        wildcards: 2,
    },
    WildcardRow {
        case: "the published limit in one value",
        conditions: &[(
            "path-pattern",
            Spelling::Configured,
            &["/w0*w1*w2*w3*w4*w5*w6"],
        )],
        wildcards: 6,
    },
    WildcardRow {
        case: "the published limit spelled top-level",
        conditions: &[("path-pattern", Spelling::TopLevel, &["/w0*w1*w2*w3*w4*w5*w6"])],
        wildcards: 6,
    },
    WildcardRow {
        case: "the published limit spread over two values",
        conditions: &[(
            "path-pattern",
            Spelling::Configured,
            &["/a*b*c", "/d*e*f*g*h"],
        )],
        wildcards: 6,
    },
    WildcardRow {
        case: "the published limit spread over two conditions",
        conditions: &[
            ("path-pattern", Spelling::Configured, &["/a*b*c*d"]),
            ("host-header", Spelling::Configured, &["*.a*.b*.example.com"]),
        ],
        wildcards: 6,
    },
    WildcardRow {
        case: "one past the published limit",
        conditions: &[(
            "path-pattern",
            Spelling::Configured,
            &["/w0*w1*w2*w3*w4*w5*w6*w7"],
        )],
        wildcards: 7,
    },
    WildcardRow {
        case: "one past the published limit, spelled top-level",
        conditions: &[(
            "path-pattern",
            Spelling::TopLevel,
            &["/w0*w1*w2*w3*w4*w5*w6*w7"],
        )],
        wildcards: 7,
    },
    WildcardRow {
        case: "one past the published limit, spread over two conditions",
        conditions: &[
            ("path-pattern", Spelling::Configured, &["/a*b*c*d*e"]),
            ("host-header", Spelling::Configured, &["*.a*.b*.example.com"]),
        ],
        wildcards: 7,
    },
    WildcardRow {
        case: "well past the published limit",
        conditions: &[(
            "path-pattern",
            Spelling::Configured,
            &["/*a*b*c*d*e*f*g*h*i*j*"],
        )],
        wildcards: 11,
    },
];

/// The request fields of every condition of one wildcard row, each at its own position.
fn wildcard_conditions(row: &WildcardRow) -> Form {
    let mut form: Form = Vec::new();
    for (position, (field, spelling, values)) in row.conditions.iter().enumerate() {
        let at = position + 1;
        for (key, value) in condition(field, *spelling, "Values", values) {
            form.push((key.replace("Conditions.member.1.", &format!("Conditions.member.{at}.")), value));
        }
    }
    form
}

/// RULE: a rule is stored only when the wildcards of its condition values, counted across
/// every condition it carries, stay inside the published limit — on the create and on the
/// modification alike.
#[tokio::test]
async fn a_rule_holds_at_most_the_published_number_of_condition_wildcards() {
    let (router, _engine) = emulator();
    let listener = listener_on(&router, "condition-wildcards", 80).await;
    let existing = add_rule(
        &router,
        "the rule a modification replaces the conditions of",
        &listener,
        1,
        &[
            condition("path-pattern", Spelling::Configured, "Values", &["/keep"]),
            fixed_response("404"),
        ]
        .concat(),
    )
    .await;

    for (index, row) in WILDCARD_ROWS.iter().enumerate() {
        let priority = i32::try_from(index + 10).expect("a rule priority");
        let fields = [wildcard_conditions(row), fixed_response("404")].concat();
        let accepted = row.wildcards <= CONDITION_WILDCARDS_PER_RULE;
        let before = rules_of(&router, row.case, &listener).await;

        let created = create_rule(&router, &listener, &priority.to_string(), &fields).await;
        if accepted {
            assert_eq!(
                created.status, 200,
                "case {} on CreateRule: expected 200 for {} wildcards, got {} {}",
                row.case, row.wildcards, created.status, created.body
            );
            // An accepted value is echoed back exactly as it was supplied.
            let supplied: Vec<&str> = row
                .conditions
                .iter()
                .flat_map(|(_, _, values)| values.iter().copied())
                .collect();
            assert!(
                supplied.iter().all(|value| created.body.contains(value)),
                "case {} on CreateRule: expected the values {supplied:?} echoed back, got {}",
                row.case,
                created.body
            );
        } else {
            assert_eq!(
                (created.status, created.error_code().as_str()),
                (400, VALIDATION_ERROR),
                "case {} on CreateRule: expected 400 {VALIDATION_ERROR} for {} wildcards, got \
                 {} {}",
                row.case,
                row.wildcards,
                created.status,
                created.body
            );
            assert_eq!(
                message(&created),
                condition_wildcards_message(),
                "case {} on CreateRule: expected the refusal the published limit earns, got {}",
                row.case,
                created.body
            );
            let after = rules_of(&router, row.case, &listener).await;
            assert_eq!(
                after, before,
                "case {} on CreateRule: a refused create stores nothing, got {after:?}",
                row.case
            );
        }

        let modified = modify_rule(&router, &existing, &fields).await;
        assert_eq!(
            modified.status == 200,
            accepted,
            "case {} on ModifyRule: expected accepted={accepted} for {} wildcards, got {} {}",
            row.case,
            row.wildcards,
            modified.status,
            modified.body
        );
        if !accepted {
            assert_eq!(
                (modified.error_code(), message(&modified)),
                (
                    VALIDATION_ERROR.to_string(),
                    condition_wildcards_message()
                ),
                "case {} on ModifyRule: expected the refusal the published limit earns, got {}",
                row.case,
                modified.body
            );
        }
    }
}
