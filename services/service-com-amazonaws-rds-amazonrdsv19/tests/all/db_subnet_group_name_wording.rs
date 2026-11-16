// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the SENTENCE `CreateDBSubnetGroup` refuses a `DBSubnetGroupName`
//! with, and for the names it accepts.
//!
//! Two rules, each asserted over a TABLE spanning what it generalises over rather than over
//! the one value a report cited:
//!
//! 1. A value outside the constraints `DBSubnetGroupName` documents — it does not begin with
//!    a letter, it carries a character outside the letters, numbers, periods, underscores,
//!    spaces and hyphens the member names, or it is longer than the member's own bound — is
//!    refused `InvalidParameterValue` at HTTP 400 by the SUBJECT Amazon RDS names that member
//!    by, echoing the value exactly as the request spelled it and naming no reason
//!    (`Invalid subnet group name:  <value>`, whose two spaces after the colon are part of
//!    it). That is the wording this front door's other named members carry
//!    (`Invalid database cluster identifier:  <value>`, `Invalid option group name:
//!    <value>`), which is why the sentence is built in ONE place for all of them.
//! 2. The ONE name the member reserves — "Must not be default", the DB subnet group AWS
//!    provides alongside the account's default VPC — is refused with the sentence that states
//!    the name is taken and asks for another, whichever case the request spells it in, since
//!    the name "is stored as a lowercase string".
//!
//! The positive control is the other side of rule 1: a name the constraints DO admit — the
//! member's wider alphabet, a value at the length bound, a mixed-case spelling — is created
//! and reads back in the folded spelling, and a refused name leaves no group behind.
//!
//! Every call goes over the wire through the harness in `db_instances.rs`, so each assertion
//! reads the XML this emulator itself answered with. The subnets come from EC2, the service
//! that owns them, through the account in `db_subnet_group_ec2_zones.rs`.

use crate::db_instances::{call_in, expect_error, expect_ok, text, texts, REGION};
use crate::db_subnet_group_ec2_zones::Network;

/// The Query API error code every refusal below carries: a value outside what its own member
/// accepts.
const INVALID_PARAMETER_VALUE: &str = "InvalidParameterValue";

/// The sentence Amazon RDS refuses a `DBSubnetGroupName` outside its member's constraints
/// with. Spelled out here so the wording is pinned by the test rather than by the code under
/// test.
fn invalid_name_message(value: &str) -> String {
    format!("Invalid subnet group name:  {value}")
}

/// The sentence Amazon RDS refuses the one reserved name with, pinned the same way.
fn reserved_name_message(value: &str) -> String {
    format!("Subnet group name {value} is reserved. Please specify another name.")
}

/// The longest `DBSubnetGroupName` the member admits: "Must contain no more than 255 letters,
/// numbers, periods, underscores, spaces, or hyphens"
/// (`smithy_query(query="doc", shape="CreateDBSubnetGroupMessage",
/// member="DBSubnetGroupName")`).
const MAX_NAME_LEN: usize = 255;

/// A name of exactly `length` characters, beginning with a letter so only the LENGTH clause
/// is under test.
fn name_of_length(length: usize) -> String {
    let mut name = String::from("e");
    name.extend(std::iter::repeat_n('m', length - 1));
    name
}

/// One value under test and the clause of the name rule it breaks.
struct NameCase {
    case: &'static str,
    value: String,
}

/// The values rule 1 runs over: one per clause of the published rule, in both the leading-
/// character and the alphabet direction, plus the length bound and a mixed-case value whose
/// case has to survive the echo.
fn refused_names() -> Vec<NameCase> {
    vec![
        NameCase {
            case: "a leading digit",
            value: "1emu-name-wording".to_string(),
        },
        NameCase {
            case: "a leading hyphen",
            value: "-emu-name-wording".to_string(),
        },
        NameCase {
            case: "a leading period",
            value: ".emu-name-wording".to_string(),
        },
        NameCase {
            case: "a leading space",
            value: " emu-name-wording".to_string(),
        },
        NameCase {
            case: "a character outside the member's alphabet",
            value: "emu-name/wording".to_string(),
        },
        NameCase {
            case: "a non-ASCII letter",
            value: "emu-nàme-wording".to_string(),
        },
        NameCase {
            case: "a mixed-case value with a leading digit, whose case the echo keeps",
            value: "9EMU-Name-Wording".to_string(),
        },
        NameCase {
            case: "one character past the member's own bound",
            value: name_of_length(MAX_NAME_LEN + 1),
        },
    ]
}

/// RULE 1: a `DBSubnetGroupName` outside the constraints its member documents is refused
/// `InvalidParameterValue` (400) by the subject Amazon RDS names that member by, echoing the
/// value as the request spelled it — and nothing is stored for it.
#[tokio::test]
async fn a_name_outside_its_member_s_constraints_is_refused_by_its_subject() {
    let network = Network::build().await;
    // A list that covers the two zones a group must cover, so only the NAME can be what the
    // request is refused for.
    let subnets = [
        network.first_zone[0].clone(),
        network.second_zone.clone(),
    ];

    for NameCase { case, value } in refused_names() {
        let body = expect_error(
            case,
            network.account.create_group(&value, &subnets).await,
            400,
            INVALID_PARAMETER_VALUE,
        );
        let message = text(&body, "Message");
        let expected = invalid_name_message(&value);
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
        assert_no_group_named(case, &network, &value).await;
    }
}

/// Every spelling of the reserved name: the rule is about the name the member reserves, which
/// is matched in the folded spelling the group is stored under, so each of these names it.
const RESERVED_SPELLINGS: [&str; 4] = ["default", "DEFAULT", "Default", "dEfAuLt"];

/// RULE 2: the one name `DBSubnetGroupName` reserves is refused `InvalidParameterValue` (400)
/// with the sentence that states the name is reserved, whichever case the request spells it
/// in, and the group of that name is left alone.
#[tokio::test]
async fn the_reserved_name_is_refused_as_reserved_in_every_spelling() {
    let network = Network::build().await;
    let subnets = [
        network.first_zone[0].clone(),
        network.second_zone.clone(),
    ];

    for spelling in RESERVED_SPELLINGS {
        let case = format!("the reserved name spelled {spelling:?}");
        let body = expect_error(
            &case,
            network.account.create_group(spelling, &subnets).await,
            400,
            INVALID_PARAMETER_VALUE,
        );
        let message = text(&body, "Message");
        let expected = reserved_name_message(spelling);
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
    }

    // The group AWS provides for the account's default VPC is still the one that name
    // resolves to: the refusals above neither created nor replaced it.
    let described = expect_ok(
        "the account's default DB subnet group after the refusals",
        network.account.describe_group("default").await,
    );
    let reported = text(&described, "DBSubnetGroupName");
    assert_eq!(
        reported, "default",
        "case \"the account's default DB subnet group after the refusals\": expected the \
         reserved group to still resolve, got {described}"
    );
}

/// One accepted name and the spelling it is stored and read back under.
struct AcceptedName {
    case: &'static str,
    value: String,
}

/// The names rule 1's positive control runs over: the member's wider alphabet (periods,
/// underscores, spaces), a value at the length bound, and a mixed-case value, which "is
/// stored as a lowercase string".
fn accepted_names() -> Vec<AcceptedName> {
    vec![
        AcceptedName {
            case: "the member's wider alphabet: periods, underscores, spaces and hyphens",
            value: "emu.name_wording with spaces-1".to_string(),
        },
        AcceptedName {
            case: "a name of exactly the length the member admits",
            value: name_of_length(MAX_NAME_LEN),
        },
        AcceptedName {
            case: "a mixed-case name, which is stored folded",
            value: "EMU-Name-Folded".to_string(),
        },
        AcceptedName {
            case: "a name whose only hyphen is in the middle",
            value: "e-2".to_string(),
        },
    ]
}

/// The other side of rule 1: a name the member's constraints admit is created, and the group
/// reads back under the folded spelling.
#[tokio::test]
async fn a_name_the_member_admits_is_created_and_reads_back_folded() {
    let network = Network::build().await;
    let subnets = [
        network.first_zone[0].clone(),
        network.second_zone.clone(),
    ];

    for AcceptedName { case, value } in accepted_names() {
        let created = expect_ok(case, network.account.create_group(&value, &subnets).await);
        let folded = value.to_ascii_lowercase();
        let reported = text(&created, "DBSubnetGroupName");
        assert_eq!(
            reported, folded,
            "case {case:?}: expected the group to be stored as {folded:?}, got {reported:?}"
        );
        // The round trip: the created group is the one the folded name resolves to.
        let described = expect_ok(case, network.account.describe_group(&value).await);
        let read_back = text(&described, "DBSubnetGroupName");
        assert_eq!(
            read_back, folded,
            "case {case:?}: expected DescribeDBSubnetGroups to report {folded:?}, got \
             {read_back:?}"
        );
    }
}

/// Assert no DB subnet group of `name` (in the folded spelling it would be stored under) is
/// in the Region's groups — the whole list is read, so a group stored under the name the
/// request spelled would be seen too.
async fn assert_no_group_named(case: &str, network: &Network, name: &str) {
    let all = expect_ok(
        case,
        call_in(
            &network.account.router,
            REGION,
            "DescribeDBSubnetGroups",
            &[] as &[(String, String)],
        )
        .await,
    );
    let stored = texts(&all, "DBSubnetGroupName");
    let folded = name.to_ascii_lowercase();
    assert!(
        !stored.contains(&folded) && !stored.contains(&name.to_string()),
        "case {case:?}: a refused name must leave no group behind, got {stored:?}"
    );
}
