// SPDX-License-Identifier: CC0-1.0

//! The RETURN CODES a target group's health check counts as success, and the range each
//! kind of code may name.
//!
//! `Matcher` gives a health check two code members, and each publishes the values it
//! accepts (`smithy_query(query="doc",shape="Matcher",member="HttpCode")`):
//!
//! * `HttpCode` — "For Application Load Balancers, you can specify values between 200 and
//!   499 … For Network Load Balancers, you can specify values between 200 and 599 … For
//!   Gateway Load Balancers, this must be "200-399"." So the range belongs to the load
//!   balancer FAMILY the target group serves ([`super::LoadBalancerFamily`]), not to the
//!   protocol the check speaks.
//! * `GrpcCode` — "You can specify values between 0 and 99", one range for every family.
//!
//! Both members carry a list rather than a number — "You can specify multiple values (for
//! example, "200,202") or a range of values (for example, "200-299")" — so every code the
//! value names is judged, and a value naming one outside the published range is refused
//! before the check is stored: a health check whose success criterion the load balancer
//! could never observe can never match, so a caller that relied on the API to say so would
//! ship a target group that is permanently unhealthy.
//!
//! The model declares no error shape for either rule, so the refusal is this service's own
//! `ValidationError`, the code every value rule the model states in prose answers under.

use std::ops::RangeInclusive;

use super::{LoadBalancerFamily, MatcherRecord};

/// The HTTP codes a health check of a target group serving `family` may count as success,
/// as `Matcher$HttpCode` publishes them for each family.
pub fn health_check_http_codes(family: LoadBalancerFamily) -> RangeInclusive<u32> {
    match family {
        LoadBalancerFamily::Application => 200..=499,
        LoadBalancerFamily::Network => 200..=599,
        LoadBalancerFamily::Gateway => 200..=399,
    }
}

/// The gRPC codes a health check may count as success, as `Matcher$GrpcCode` publishes
/// them: "You can specify values between 0 and 99" — one range, whatever the family.
pub fn health_check_grpc_codes() -> RangeInclusive<u32> {
    0..=99
}

/// Whether `matcher` counts an HTTP response code of `status` as a SUCCESSFUL health check
/// — "the HTTP or gRPC codes to use when checking for a successful response from a target".
///
/// The member carries a list rather than a number — "you can specify multiple values (for
/// example, "200,202") or a range of values (for example, "200-299")" — so the code is
/// counted as success when it is one of the values named or lies inside one of the ranges
/// named, which is the same spelling [`check_health_check_matcher`] judges the bounds of.
///
/// A matcher naming no HTTP code at all states no criterion an HTTP response code could
/// meet, so it counts none as success: what such a check is judged on is not a code the
/// response carries.
///
/// Read where a health check's ANSWER is read — the invocation a check of a `lambda` target
/// group is ([`crate::service::lambda_health_check`]) — so the codes a check counts as
/// success and the codes a write of one is accepted at come from one reading of the member.
pub fn matcher_admits(matcher: &MatcherRecord, status: u16) -> bool {
    let Some(value) = matcher.http_code.as_deref() else {
        return false;
    };
    let status = u32::from(status);
    value.split(',').any(|entry| match entry.split_once('-') {
        Some((low, high)) => {
            match (low.trim().parse::<u32>(), high.trim().parse::<u32>()) {
                (Ok(low), Ok(high)) => (low..=high).contains(&status),
                _ => false,
            }
        }
        None => entry.trim().parse::<u32>().is_ok_and(|code| code == status),
    })
}

/// How a refusal names the kind of code it judged: the spelling Elastic Load Balancing
/// uses in the sentence, which is the protocol the code belongs to rather than the member
/// that carried it.
const HTTP_CODE: &str = "HTTP";
const GRPC_CODE: &str = "GRPC";

/// Whether every code `matcher` names is one a health check of a target group serving
/// `family` may count as success, and — when one is not — the rule it broke.
///
/// Read by every operation that WRITES a health check (`CreateTargetGroup` for the check a
/// target group starts with, `ModifyTargetGroup` for the one that replaces it), so one
/// reading of the published ranges serves both.
pub fn check_health_check_matcher(
    matcher: &MatcherRecord,
    family: LoadBalancerFamily,
) -> Result<(), String> {
    if let Some(http_code) = matcher.http_code.as_deref() {
        check_codes(http_code, HTTP_CODE, &health_check_http_codes(family))?;
    }
    if let Some(grpc_code) = matcher.grpc_code.as_deref() {
        check_codes(grpc_code, GRPC_CODE, &health_check_grpc_codes())?;
    }
    Ok(())
}

/// The message a code outside its published range is refused with: the kind of code, the
/// offending code as the request wrote it, and the range it has to lie in.
///
/// Recorded behaviour for the HTTP member, whose text is fixed but for those two parts —
/// the member's value is a LIST, so what is quoted is the one code that broke the rule
/// rather than the whole value the request carried.
pub fn matcher_code_message(kind: &str, code: &str, codes: &RangeInclusive<u32>) -> String {
    format!(
        "Health check matcher {kind} code '{code}' must be within '{}-{}' inclusive",
        codes.start(),
        codes.end()
    )
}

/// Judge one member's whole value: every code it names, whether written on its own, as one
/// of several separated by commas, or as the two bounds of a range.
///
/// A bound that is no number at all is outside every range, so it earns the same refusal as
/// a number past one: in both cases the value the caller has to replace is that one entry.
fn check_codes(value: &str, kind: &str, codes: &RangeInclusive<u32>) -> Result<(), String> {
    for entry in value.split(',') {
        // "a range of values (for example, "200-299")" — both of its bounds are codes the
        // check accepts, so both are judged.
        let bounds = match entry.split_once('-') {
            Some((low, high)) => [Some(low), Some(high)],
            None => [Some(entry), None],
        };
        for bound in bounds.into_iter().flatten() {
            let bound = bound.trim();
            if !bound.parse::<u32>().is_ok_and(|code| codes.contains(&code)) {
                return Err(matcher_code_message(kind, bound, codes));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A matcher carrying one HTTP code value.
    fn http(code: &str) -> MatcherRecord {
        MatcherRecord {
            http_code: Some(code.to_string()),
            grpc_code: None,
        }
    }

    /// Every family's published range is enforced on its own bounds, one past each of
    /// them, and on each spelling the member accepts.
    #[test]
    fn an_http_code_lies_inside_the_range_its_family_publishes() {
        // (the family, the value, whether a health check may carry it)
        let rows: &[(LoadBalancerFamily, &str, bool)] = &[
            // The Application range's own bounds, and one past each.
            (LoadBalancerFamily::Application, "200", true),
            (LoadBalancerFamily::Application, "499", true),
            (LoadBalancerFamily::Application, "199", false),
            (LoadBalancerFamily::Application, "500", false),
            // The Network range reaches further, so a code the Application range refuses
            // is one it accepts.
            (LoadBalancerFamily::Network, "500", true),
            (LoadBalancerFamily::Network, "599", true),
            (LoadBalancerFamily::Network, "600", false),
            (LoadBalancerFamily::Network, "199", false),
            // The Gateway range is the narrowest of the three.
            (LoadBalancerFamily::Gateway, "399", true),
            (LoadBalancerFamily::Gateway, "400", false),
            // The spellings the member accepts: several values, and a range.
            (LoadBalancerFamily::Application, "200,202", true),
            (LoadBalancerFamily::Application, "200-299", true),
            (LoadBalancerFamily::Application, "200,600", false),
            (LoadBalancerFamily::Application, "200-600", false),
            // Values that name no code at all.
            (LoadBalancerFamily::Application, "", false),
            (LoadBalancerFamily::Application, "OK", false),
            (LoadBalancerFamily::Application, "-200", false),
        ];
        for (family, value, accepted) in rows {
            let answer = check_health_check_matcher(&http(value), *family);
            assert_eq!(
                answer.is_ok(),
                *accepted,
                "case {family:?} {value:?}: expected accepted={accepted}, got {answer:?}"
            );
        }
    }

    /// The refusal quotes the ONE code that broke the rule and the range it had to lie in.
    #[test]
    fn the_refusal_quotes_the_offending_code_and_the_published_range() {
        assert_eq!(
            check_health_check_matcher(&http("200,600"), LoadBalancerFamily::Application),
            Err("Health check matcher HTTP code '600' must be within '200-499' inclusive"
                .to_string()),
            "case one value of several outside the range: unexpected refusal"
        );
        assert_eq!(
            check_health_check_matcher(&http("600"), LoadBalancerFamily::Network),
            Err("Health check matcher HTTP code '600' must be within '200-599' inclusive"
                .to_string()),
            "case the same code against the wider range: unexpected refusal"
        );
    }

    /// RULE: a matcher counts a response code as success exactly when its `HttpCode` names
    /// that code, on its own or among several, or a range it names contains it.
    ///
    /// The rows span each spelling the member accepts (one value, several, a range), the
    /// bounds of a range and one past each of them, a value nothing admits, and a matcher
    /// naming no HTTP code at all — none of which the issue this answers cited.
    #[test]
    fn a_matcher_counts_the_codes_its_http_member_names_as_success() {
        // (the matcher's `HttpCode`, the status, whether the check succeeded)
        let rows: &[(Option<&str>, u16, bool)] = &[
            (Some("200"), 200, true),
            (Some("200"), 201, false),
            (Some("200,202"), 202, true),
            (Some("200,202"), 201, false),
            (Some("200-299"), 200, true),
            (Some("200-299"), 250, true),
            (Some("200-299"), 299, true),
            (Some("200-299"), 199, false),
            (Some("200-299"), 300, false),
            (Some("200-399"), 302, true),
            (Some("200,300-399"), 404, false),
            (Some("404"), 404, true),
            // A value naming no code at all counts nothing as success.
            (Some(""), 200, false),
            (Some("OK"), 200, false),
            // A matcher naming no HTTP code states no criterion a status can meet.
            (None, 200, false),
        ];
        for (http_code, status, expected) in rows {
            let matcher = MatcherRecord {
                http_code: http_code.map(str::to_string),
                grpc_code: None,
            };
            let admitted = matcher_admits(&matcher, *status);
            assert_eq!(
                admitted, *expected,
                "case {http_code:?} against the status {status}: expected success={expected}, \
                 got {admitted}"
            );
        }
    }

    /// The gRPC member is judged against its own range, which is the same for every
    /// family.
    #[test]
    fn a_grpc_code_lies_inside_the_one_range_that_member_publishes() {
        // (the value, whether a health check may carry it)
        let rows: &[(&str, bool)] = &[
            ("0", true),
            ("12", true),
            ("99", true),
            ("0,1", true),
            ("0-5", true),
            ("100", false),
            ("0,100", false),
        ];
        for (value, accepted) in rows {
            let matcher = MatcherRecord {
                http_code: None,
                grpc_code: Some(value.to_string()),
            };
            for family in [
                LoadBalancerFamily::Application,
                LoadBalancerFamily::Network,
                LoadBalancerFamily::Gateway,
            ] {
                let answer = check_health_check_matcher(&matcher, family);
                assert_eq!(
                    answer.is_ok(),
                    *accepted,
                    "case {family:?} gRPC {value:?}: expected accepted={accepted}, got \
                     {answer:?}"
                );
            }
        }
        assert_eq!(
            check_health_check_matcher(
                &MatcherRecord {
                    http_code: None,
                    grpc_code: Some("100".to_string()),
                },
                LoadBalancerFamily::Application
            ),
            Err("Health check matcher GRPC code '100' must be within '0-99' inclusive".to_string()),
            "case a gRPC code past its range: unexpected refusal"
        );
    }
}
