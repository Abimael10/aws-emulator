// SPDX-License-Identifier: CC0-1.0

//! The RETURN CODES a target group's health check may count as success, over the wire.
//!
//! `Matcher` gives a health check two code members, and each publishes the values it
//! accepts (`smithy_query(query="doc",shape="Matcher",member="HttpCode")`):
//!
//! * `HttpCode` — "For Application Load Balancers, you can specify values between 200 and
//!   499 … For Network Load Balancers, you can specify values between 200 and 599 … For
//!   Gateway Load Balancers, this must be "200-399"", so the range belongs to the load
//!   balancer FAMILY the target group's protocol pairs it with;
//! * `GrpcCode` — "You can specify values between 0 and 99", one range for every family.
//!
//! The rule the rows below assert is that every code a matcher names lies inside the range
//! published for its kind of code and the target group's family, on each spelling the
//! member accepts ("You can specify multiple values (for example, "200,202") or a range of
//! values (for example, "200-299")") — and that a value naming one outside it is refused
//! with `ValidationError` quoting that one code, on the operation that first stores a
//! health check and on the one that replaces it alike, with nothing written either way.
//!
//! The rows differ along what the rule generalises over: the family (through the target
//! group protocol), which code member carries the value, the spelling (a single code,
//! several, a range), and where the offending code sits — each published bound of each
//! range, and one past it.
//!
//! The wire harness is the one `provisioning_loop` owns and the target-group helpers are
//! `all/target_group_health_check_port.rs`'s; nothing here restates either.

use crate::provisioning_loop::{action, call, emulator, p, Reply};
use crate::target_group_attributes_and_modification::unescaped;
use crate::target_group_health_check_port::{create_group, describe, health_check_of};

/// The error code this service answers an input rule the model states in prose with.
const VALIDATION_ERROR: &str = "ValidationError";

/// The awsQuery code of `TargetGroupNotFoundException`, which a read of a name no create
/// stored answers.
const TARGET_GROUP_NOT_FOUND: &str = "TargetGroupNotFound";

/// One row: the case, the target group's own protocol (which decides the family, and so
/// the range), the member carrying the codes, its value, and the code the refusal must
/// quote — `None` for a value the service accepts.
struct MatcherRow {
    case: &'static str,
    protocol: &'static str,
    member: &'static str,
    value: &'static str,
    refused_code: Option<&'static str>,
}

/// The `HttpCode` member, and the `GrpcCode` member beside it.
const HTTP_CODE: &str = "HttpCode";
const GRPC_CODE: &str = "GrpcCode";

/// The published range of each family, as `Matcher$HttpCode` states it: 200-499 for an
/// Application Load Balancer's target group (HTTP, HTTPS), 200-599 for a Network Load
/// Balancer's (TCP and its siblings), 200-399 for a Gateway Load Balancer's (GENEVE).
/// `GrpcCode` publishes one range, 0-99, whatever the family.
const ROWS: &[MatcherRow] = &[
    // The Application range: both published bounds, and one past each.
    MatcherRow {
        case: "the Application range's low bound",
        protocol: "HTTP",
        member: HTTP_CODE,
        value: "200",
        refused_code: None,
    },
    MatcherRow {
        case: "the Application range's high bound",
        protocol: "HTTPS",
        member: HTTP_CODE,
        value: "499",
        refused_code: None,
    },
    MatcherRow {
        case: "one under the Application range",
        protocol: "HTTP",
        member: HTTP_CODE,
        value: "199",
        refused_code: Some("199"),
    },
    MatcherRow {
        case: "one past the Application range",
        protocol: "HTTP",
        member: HTTP_CODE,
        value: "500",
        refused_code: Some("500"),
    },
    // The spellings the member accepts, with the offending code in each position.
    MatcherRow {
        case: "several codes, all inside the range",
        protocol: "HTTP",
        member: HTTP_CODE,
        value: "200,202",
        refused_code: None,
    },
    MatcherRow {
        case: "a range of codes inside the range",
        protocol: "HTTP",
        member: HTTP_CODE,
        value: "200-299",
        refused_code: None,
    },
    MatcherRow {
        case: "the second of several codes outside the range",
        protocol: "HTTP",
        member: HTTP_CODE,
        value: "200,600",
        refused_code: Some("600"),
    },
    MatcherRow {
        case: "the upper bound of a range outside the range",
        protocol: "HTTP",
        member: HTTP_CODE,
        value: "200-600",
        refused_code: Some("600"),
    },
    MatcherRow {
        case: "a value that names no code at all",
        protocol: "HTTP",
        member: HTTP_CODE,
        value: "OK",
        refused_code: Some("OK"),
    },
    // The Network range reaches further, so a code the Application range refuses is one it
    // accepts — and its own bound is enforced on the same footing.
    MatcherRow {
        case: "a Network target group inside the wider range",
        protocol: "TCP",
        member: HTTP_CODE,
        value: "500",
        refused_code: None,
    },
    MatcherRow {
        case: "the Network range's high bound",
        protocol: "TLS",
        member: HTTP_CODE,
        value: "599",
        refused_code: None,
    },
    MatcherRow {
        case: "one past the Network range",
        protocol: "TCP",
        member: HTTP_CODE,
        value: "600",
        refused_code: Some("600"),
    },
    // The Gateway range is the narrowest of the three, so a code the other two accept is
    // one it refuses.
    MatcherRow {
        case: "the Gateway range's high bound",
        protocol: "GENEVE",
        member: HTTP_CODE,
        value: "200-399",
        refused_code: None,
    },
    MatcherRow {
        case: "one past the Gateway range",
        protocol: "GENEVE",
        member: HTTP_CODE,
        value: "400",
        refused_code: Some("400"),
    },
    // The gRPC member, judged against the one range it publishes.
    MatcherRow {
        case: "the gRPC range's low bound",
        protocol: "HTTP",
        member: GRPC_CODE,
        value: "0",
        refused_code: None,
    },
    MatcherRow {
        case: "the gRPC range's high bound",
        protocol: "HTTP",
        member: GRPC_CODE,
        value: "99",
        refused_code: None,
    },
    MatcherRow {
        case: "one past the gRPC range",
        protocol: "HTTP",
        member: GRPC_CODE,
        value: "100",
        refused_code: Some("100"),
    },
    MatcherRow {
        case: "the second of several gRPC codes outside the range",
        protocol: "HTTP",
        member: GRPC_CODE,
        value: "0,100",
        refused_code: Some("100"),
    },
];

/// The range the refusal states for one row, which is the range the member publishes for
/// that kind of code and that family — the text of the rule, not of one run.
fn published_range(row: &MatcherRow) -> &'static str {
    match (row.member, row.protocol) {
        (GRPC_CODE, _) => "0-99",
        (_, "GENEVE") => "200-399",
        (_, "TCP" | "TLS" | "UDP" | "TCP_UDP" | "QUIC" | "TCP_QUIC") => "200-599",
        _ => "200-499",
    }
}

/// How a refusal names the kind of code it judged.
fn code_kind(member: &str) -> &'static str {
    match member {
        GRPC_CODE => "GRPC",
        _ => "HTTP",
    }
}

/// The message the service answers a code outside its published range with: fixed text,
/// with the offending code and the range it had to lie in as its only variable parts.
fn expected_refusal(row: &MatcherRow, code: &str) -> String {
    format!(
        "Health check matcher {} code '{code}' must be within '{}' inclusive",
        code_kind(row.member),
        published_range(row)
    )
}

/// The status, code and message of a refusal, as a caller reads them.
fn refusal(reply: &Reply) -> (u16, String, String) {
    (
        reply.status,
        reply.error_code(),
        unescaped(&reply.text("Message").unwrap_or_default()),
    )
}

/// The request fields of a target group of `protocol`, the shape each row starts from.
///
/// A matcher belongs to an HTTP-semantic health check — `Matcher` is documented
/// "[HTTP/HTTPS health checks] The HTTP or gRPC codes to use when checking for a successful
/// response from a target" — so a target group whose own protocol is not an HTTP one is
/// health-checked over HTTP, which is the shape a matcher is configurable on at all. The
/// RANGE still follows the target group's family rather than the protocol the check speaks.
///
/// A gRPC matcher belongs to a gRPC protocol version (`HealthCheckPath` brackets that form
/// to "[GRPC protocol version]"), so a row naming `GrpcCode` carries that version.
fn group_fields(row: &MatcherRow) -> Vec<(String, String)> {
    let mut fields = vec![
        p("Protocol", row.protocol),
        // "If the protocol is GENEVE, the supported port is 6081"; every other protocol
        // here takes the port its own traffic arrives on.
        p("Port", if row.protocol == "GENEVE" { "6081" } else { "443" }),
        p("VpcId", "vpc-1"),
        p("TargetType", "ip"),
    ];
    if !matches!(row.protocol, "HTTP" | "HTTPS") {
        fields.push(p("HealthCheckProtocol", "HTTP"));
        fields.push(p("HealthCheckPath", "/health-check"));
    }
    if row.member == GRPC_CODE {
        fields.push(p("ProtocolVersion", "GRPC"));
    }
    fields
}

#[tokio::test]
async fn create_target_group_takes_a_matcher_only_inside_its_published_range() {
    let (router, _engine) = emulator();

    for (index, row) in ROWS.iter().enumerate() {
        let name = format!("ctg-matcher-{index}");
        let mut form = action("CreateTargetGroup");
        form.push(p("Name", &name));
        form.extend(group_fields(row));
        form.push(p(&format!("Matcher.{}", row.member), row.value));
        let reply = call(&router, &form).await;

        match row.refused_code {
            Some(code) => {
                assert_eq!(
                    refusal(&reply),
                    (
                        400,
                        VALIDATION_ERROR.to_string(),
                        expected_refusal(row, code)
                    ),
                    "case {}: {:?} must be refused naming {code:?}, got {}",
                    row.case,
                    row.value,
                    reply.body
                );
                // A refused create persists nothing, so a later read by that name finds
                // no target group at all.
                let mut read = action("DescribeTargetGroups");
                read.push(p("Names.member.1", &name));
                let read = call(&router, &read).await;
                assert_eq!(
                    (read.status, read.error_code()),
                    (400, TARGET_GROUP_NOT_FOUND.to_string()),
                    "case {}: a refused create must store no target group, got {}",
                    row.case,
                    read.body
                );
            }
            None => {
                assert_eq!(
                    reply.status,
                    200,
                    "case {}: {:?} is inside the published range and must be accepted, got {} \
                     body {}",
                    row.case,
                    row.value,
                    reply.status,
                    reply.body
                );
                // An accepted matcher round-trips as the request wrote it.
                assert_eq!(
                    reply.text(row.member).as_deref(),
                    Some(row.value),
                    "case {}: the stored matcher must report the codes the request named, \
                     got {}",
                    row.case,
                    reply.body
                );
            }
        }
    }
}

#[tokio::test]
async fn modify_target_group_judges_a_matcher_by_the_same_ranges() {
    // The rule, and so its wording, belongs to the MEMBER: the operation that replaces a
    // health check judges its matcher exactly as the one that first stored it does, and a
    // refused modification leaves the stored check alone.
    let (router, _engine) = emulator();

    for (index, row) in ROWS.iter().enumerate() {
        let name = format!("mtg-matcher-{index}");
        let arn = create_group(&router, row.case, &name, &group_fields(row)).await;
        let described = describe(&router, &arn).await;
        let before = health_check_of(&described);
        let before_member = described.text(row.member);

        let mut form = action("ModifyTargetGroup");
        form.push(p("TargetGroupArn", &arn));
        form.push(p(&format!("Matcher.{}", row.member), row.value));
        let reply = call(&router, &form).await;

        match row.refused_code {
            Some(code) => {
                assert_eq!(
                    refusal(&reply),
                    (
                        400,
                        VALIDATION_ERROR.to_string(),
                        expected_refusal(row, code)
                    ),
                    "case {}: {:?} must be refused naming {code:?}, got {}",
                    row.case,
                    row.value,
                    reply.body
                );
                let described = describe(&router, &arn).await;
                assert_eq!(
                    (health_check_of(&described), described.text(row.member)),
                    (before, before_member),
                    "case {}: a refused modify must change nothing, got {}",
                    row.case,
                    described.body
                );
            }
            None => {
                assert_eq!(
                    reply.status,
                    200,
                    "case {}: {:?} is inside the published range and must be accepted, got {} \
                     body {}",
                    row.case,
                    row.value,
                    reply.status,
                    reply.body
                );
                assert_eq!(
                    describe(&router, &arn).await.text(row.member).as_deref(),
                    Some(row.value),
                    "case {}: a stored matcher must be read back as written",
                    row.case
                );
            }
        }
    }
}
