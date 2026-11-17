// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for how this service READS a boolean request parameter, driven over the
//! wire it speaks: `ec2Query`, an `application/x-www-form-urlencoded` POST answered with XML.
//!
//! The rule under test: a BOOLEAN parameter is read without regard to ASCII case, so a
//! request spelling its value `TRUE`, `True` or `FALSE` carries the boolean it spells and is
//! SERVED — a `DryRun` spelled that way is answered the dry-run verdict, not refused as a
//! body the service could not read. Three things follow, and each is a test below:
//!
//! 1. every spelling of each boolean, on every operation that carries one, is read as the
//!    value it spells — the `true` spellings stop the call at the dry-run verdict and the
//!    `false` spellings do not stop it at all;
//! 2. a value that is no boolean in any case is still refused, so the rule widens the CASE a
//!    boolean may be written in and nothing else;
//! 3. a value of a member that is not a boolean is untouched by it — a filter value that
//!    reads like one is still compared as the request sent it.
//!
//! Every table row is a distinct input along what the rule generalises over — which
//! operation, which boolean, which case — so the request any one recording made is one row
//! rather than the specification.

use service_com_amazonaws_ec2_amazonec2 as ec2;

use super::ec2_query::{call, elements, emulator, error_code, error_message, filters};

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// The HTTP status EC2 answers a dry run that would otherwise have succeeded with.
const DRY_RUN_STATUS: u16 = 412;

/// A `(name, value)` form field.
fn p(name: &str, value: &str) -> (String, String) {
    (name.to_string(), value.to_string())
}

/// One row of the tables below: the case's name, the operation, and the form fields it needs
/// beside its `DryRun`.
type OperationRow = (&'static str, &'static str, Vec<(String, String)>);

/// The operations the tables below drive, each with the parameters it needs beside its
/// `DryRun`: three reports and one create, so the rule is asserted on an operation that
/// WRITES as well as on ones that only report.
fn operations() -> Vec<OperationRow> {
    vec![
        ("a report of the account's VPCs", "DescribeVpcs", Vec::new()),
        ("a report of its subnets", "DescribeSubnets", Vec::new()),
        (
            "a report of its security groups",
            "DescribeSecurityGroups",
            Vec::new(),
        ),
        (
            "a create",
            "CreateVpc",
            vec![p("CidrBlock", "10.7.0.0/16")],
        ),
    ]
}

/// RULE 1: every ASCII case of `true` sets the flag, so the call is answered the dry-run
/// verdict — and nothing it would have written is written.
#[tokio::test]
async fn every_case_of_true_is_read_as_the_flag_it_spells() {
    let (router, _engine) = emulator();

    // Rows: the wire's own spelling and three others of the same value.
    for spelling in ["true", "TRUE", "True", "tRuE"] {
        for (subject, action, extra) in operations() {
            let case = format!("{subject} with DryRun={spelling}");
            let mut params = extra;
            params.push(p("DryRun", spelling));
            let (status, body) = call(&router, REGION, action, &params).await;
            assert_eq!(
                status, DRY_RUN_STATUS,
                "case {case:?}: expected HTTP {DRY_RUN_STATUS} from {action}, got {status} \
                 with body {body}"
            );
            assert_eq!(
                error_code(&body),
                ec2::state::DRY_RUN_OPERATION,
                "case {case:?}: expected {}, got {body}",
                ec2::state::DRY_RUN_OPERATION
            );
            assert_eq!(
                error_message(&body),
                ec2::state::DRY_RUN_OPERATION_MESSAGE,
                "case {case:?}: expected {:?}, got {:?}",
                ec2::state::DRY_RUN_OPERATION_MESSAGE,
                error_message(&body)
            );
        }
    }

    // The create above was dry-run four times and performed none of them: the account holds
    // only the VPC it already had, which is the Region's default one.
    let (status, body) = call(&router, REGION, "DescribeVpcs", &[]).await;
    assert_eq!(
        status, 200,
        "reading the VPCs back: expected HTTP 200, got {status} with body {body}"
    );
    let blocks = elements(&body, "cidrBlock");
    assert!(
        !blocks.iter().any(|block| block == "10.7.0.0/16"),
        "reading the VPCs back: a dry-run create must store nothing, got {blocks:?}"
    );
}

/// RULE 1: every ASCII case of `false` leaves the flag unset, so the call is performed.
#[tokio::test]
async fn every_case_of_false_is_read_as_the_flag_it_spells() {
    let (router, _engine) = emulator();

    for spelling in ["false", "FALSE", "False", "fAlSe"] {
        for (subject, action, extra) in operations() {
            let case = format!("{subject} with DryRun={spelling}");
            let mut params = extra;
            params.push(p("DryRun", spelling));
            let (status, body) = call(&router, REGION, action, &params).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
            );
        }
    }
}

/// RULE 2: a value that is no boolean in ANY case is still refused — the rule is about the
/// case a boolean may be written in, not about accepting anything in a boolean's place.
#[tokio::test]
async fn a_value_that_is_no_boolean_is_still_refused() {
    let (router, _engine) = emulator();

    // Rows: a word, a numeral, a near-miss of a declared spelling and an empty value.
    for value in ["yes", "1", "truthy", ""] {
        let case = format!("DryRun={value:?}");
        let (status, body) = call(&router, REGION, "DescribeVpcs", &[p("DryRun", value)]).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with body {body}"
        );
        // Whatever the code, the call must not have been read as EITHER boolean: a 412 would
        // mean the value was taken for `true`, and a 200 for `false`.
        assert_ne!(
            status, DRY_RUN_STATUS,
            "case {case:?}: a value that is no boolean must not set the flag, got {body}"
        );
    }
}

/// RULE 3: only a BOOLEAN member is read without regard to case. A filter value that reads
/// like a boolean is compared exactly as the request sent it, so the `isDefault` filter
/// selects the default VPC for `true` and nothing at all for `TRUE`.
#[tokio::test]
async fn a_filter_value_that_reads_like_a_boolean_is_untouched() {
    let (router, _engine) = emulator();

    // The default VPC the Region already holds, read with no filter at all: the row the
    // filtered calls below are judged against.
    let (status, body) = call(&router, REGION, "DescribeVpcs", &[]).await;
    assert_eq!(
        status, 200,
        "the unfiltered report: expected HTTP 200, got {status} with body {body}"
    );
    let all = elements(&body, "vpcId");
    assert_eq!(
        all.len(),
        1,
        "the unfiltered report: expected the Region's default VPC alone, got {all:?}"
    );

    // Rows: the lowercase literal the wire writes the flag in, which selects it, and three
    // other cases of the same word, none of which is a value the flag is written in.
    for (value, expected) in [
        ("true", all.clone()),
        ("TRUE", Vec::new()),
        ("True", Vec::new()),
        ("tRuE", Vec::new()),
    ] {
        let case = format!("Filter isDefault={value:?}");
        let (status, body) = call(
            &router,
            REGION,
            "DescribeVpcs",
            &filters(&[("isDefault", &[value])]),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with body {body}"
        );
        let reported = elements(&body, "vpcId");
        assert_eq!(
            reported, expected,
            "case {case:?}: expected {expected:?}, got {reported:?}"
        );
    }
}
