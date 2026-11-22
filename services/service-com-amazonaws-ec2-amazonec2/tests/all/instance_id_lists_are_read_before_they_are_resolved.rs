// SPDX-License-Identifier: CC0-1.0

//! The ORDER in which a describe that takes an `InstanceIds` list answers the two refusals an
//! id can earn, driven over the wire the service speaks through the crate's shared
//! [`super::ec2_query`] harness.
//!
//! The rules under test are:
//!
//! 1. every id a request names is read against the id form the operation accepts BEFORE any id
//!    of that request is resolved, so an id the operation cannot read is refused as malformed
//!    wherever it sits among ids that merely name no instance — syntax is a fact of the
//!    request, where existence is a fact of the account;
//! 2. the reading each operation applies is its own: the describes that DECODE a long-form id
//!    before any lookup cannot read a long-form id this service never issued, while the ones
//!    that LOOK an id UP read every value inside the published forms and report one that names
//!    nothing as absent;
//! 3. a list whose ids are ALL readable still earns the not-found refusal, naming the first id
//!    that resolves to nothing;
//! 4. `DescribeInstanceStatus` refuses a page size beside an explicit id list with the fixed
//!    sentence EC2 words that pair with, naming the two parameters as this operation's own
//!    refusal spells them.
//!
//! Each row is a distinct input along what the rules generalise over — which operation is
//! called, which reading it takes, where the unreadable id sits in the list, how many readable
//! ids sit beside it, and which form each id is in — so no row is the specification.

use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, emulator, error_code, error_message};

/// The Region every call here is signed for.
const REGION: &str = "us-west-2";

/// A LONG-form instance id in the form EC2 publishes whose identifier no mint of this service
/// has reached: the long form is an obfuscated identifier, so a describe that decodes one
/// before any lookup cannot read this value at all.
const UNDECODABLE_LONG_ID: &str = "i-0123456789abcdef0";

/// A second such value, so no row depends on one spelling.
const OTHER_UNDECODABLE_LONG_ID: &str = "i-0fedcba987654321f";

/// The retired SHORT form, which carries no identifier to decode: a well-formed id every
/// reading accepts and nothing was ever launched for.
const ABSENT_SHORT_ID: &str = "i-0abc1234";

/// A second absent short-form id, for the rows that name several.
const OTHER_ABSENT_SHORT_ID: &str = "i-0abc1235";

/// A value in no instance-id form at all, which no reading can read.
const NO_ID_AT_ALL: &str = "not-an-id";

/// The describes whose `InstanceIds` reader DECODES a long-form id before it looks anything up.
const DECODING_DESCRIBES: [&str; 4] = [
    "DescribeInstances",
    "DescribeInstanceStatus",
    "DescribeInstanceCreditSpecifications",
    "DescribeInstanceImageMetadata",
];

/// The describes whose `InstanceIds` reader LOOKS an id UP instead, so only a value outside the
/// published forms is unreadable to it.
const LOOKUP_DESCRIBES: [&str; 2] = ["DescribeClassicLinkInstances", "DescribeApplicationStatus"];

/// A `(key, value)` request parameter.
fn p(key: &str, value: impl Into<String>) -> (String, String) {
    (key.to_string(), value.into())
}

/// `InstanceId.1 … InstanceId.N`, the wire form of the `InstanceIds` member every operation
/// here declares (`xmlName: InstanceId`).
fn instance_ids(ids: &[&str]) -> Vec<(String, String)> {
    ids.iter()
        .enumerate()
        .map(|(index, id)| p(&format!("InstanceId.{}", index + 1), *id))
        .collect()
}

/// One ec2Query call through the shared harness.
async fn api(
    router: &MultiServiceRouter,
    action: &str,
    params: &[(String, String)],
) -> (u16, String) {
    call(router, REGION, action, params).await
}

/// Pin that `action`, given `ids`, is refused 400 with `expected_code` and a message naming
/// `offending`.
async fn assert_refused(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    ids: &[&str],
    expected_code: &str,
    offending: &str,
) {
    let (status, body) = api(router, action, &instance_ids(ids)).await;
    assert_eq!(
        status, 400,
        "case {case:?} on {action}: expected HTTP 400, got {status}: {body}"
    );
    assert_eq!(
        error_code(&body),
        expected_code,
        "case {case:?} on {action}: expected {expected_code}, got {}: {body}",
        error_code(&body)
    );
    let message = error_message(&body);
    assert!(
        message.contains(offending),
        "case {case:?} on {action}: the refusal must name {offending:?}, got {message:?}"
    );
}

/// RULES 1 and 2: a describe that DECODES a long-form id refuses the id it cannot read before
/// it resolves any id of the request, wherever that id sits in the list.
#[tokio::test]
async fn a_decoding_describe_refuses_the_unreadable_id_before_it_resolves_an_absent_one() {
    let (router, _engine) = emulator();

    // (case, the ids the request names, the id the refusal must name)
    let rows: [(&str, Vec<&str>, &str); 6] = [
        (
            "the undecodable id alone",
            vec![UNDECODABLE_LONG_ID],
            UNDECODABLE_LONG_ID,
        ),
        (
            "the undecodable id before an absent one",
            vec![UNDECODABLE_LONG_ID, ABSENT_SHORT_ID],
            UNDECODABLE_LONG_ID,
        ),
        (
            "the undecodable id after an absent one",
            vec![ABSENT_SHORT_ID, UNDECODABLE_LONG_ID],
            UNDECODABLE_LONG_ID,
        ),
        (
            "the undecodable id last of several absent ones",
            vec![
                ABSENT_SHORT_ID,
                OTHER_ABSENT_SHORT_ID,
                OTHER_UNDECODABLE_LONG_ID,
            ],
            OTHER_UNDECODABLE_LONG_ID,
        ),
        (
            "a value in no id form, after an absent id",
            vec![ABSENT_SHORT_ID, NO_ID_AT_ALL],
            NO_ID_AT_ALL,
        ),
        (
            "the first unreadable id of two",
            vec![OTHER_UNDECODABLE_LONG_ID, NO_ID_AT_ALL],
            OTHER_UNDECODABLE_LONG_ID,
        ),
    ];
    for action in DECODING_DESCRIBES {
        for (case, ids, offending) in &rows {
            assert_refused(
                &router,
                case,
                action,
                ids,
                "InvalidInstanceID.Malformed",
                offending,
            )
            .await;
        }
    }
}

/// RULE 2: the LOOKUP readers take the other reading — a long-form id of the published form
/// that names nothing is absent to them, and only a value outside the forms is unreadable —
/// and RULE 1 holds there too.
#[tokio::test]
async fn a_lookup_describe_refuses_the_unreadable_form_before_it_resolves_an_absent_id() {
    let (router, _engine) = emulator();

    // (case, the ids the request names, the code the refusal carries, the id it must name)
    let rows: [(&str, Vec<&str>, &str, &str); 4] = [
        (
            "a value in no id form, after an absent id",
            vec![ABSENT_SHORT_ID, NO_ID_AT_ALL],
            "InvalidInstanceID.Malformed",
            NO_ID_AT_ALL,
        ),
        (
            "a value in no id form, before an absent id",
            vec![NO_ID_AT_ALL, ABSENT_SHORT_ID],
            "InvalidInstanceID.Malformed",
            NO_ID_AT_ALL,
        ),
        (
            "a long-form id of the published form that names nothing",
            vec![UNDECODABLE_LONG_ID],
            "InvalidInstanceID.NotFound",
            UNDECODABLE_LONG_ID,
        ),
        (
            "a short-form id that names nothing",
            vec![ABSENT_SHORT_ID],
            "InvalidInstanceID.NotFound",
            ABSENT_SHORT_ID,
        ),
    ];
    for action in LOOKUP_DESCRIBES {
        for (case, ids, expected_code, offending) in &rows {
            assert_refused(&router, case, action, ids, expected_code, offending).await;
        }
    }
}

/// RULE 3: reading the whole list first does not swallow the not-found half — a list whose ids
/// are all readable is still refused on the first one that resolves to nothing.
#[tokio::test]
async fn a_list_of_readable_ids_still_earns_the_not_found_refusal() {
    let (router, _engine) = emulator();

    // (case, the ids the request names, the id the refusal must name)
    let rows: [(&str, Vec<&str>, &str); 3] = [
        (
            "one absent short-form id",
            vec![ABSENT_SHORT_ID],
            ABSENT_SHORT_ID,
        ),
        (
            "two absent short-form ids",
            vec![ABSENT_SHORT_ID, OTHER_ABSENT_SHORT_ID],
            ABSENT_SHORT_ID,
        ),
        (
            "the same absent id twice",
            vec![OTHER_ABSENT_SHORT_ID, OTHER_ABSENT_SHORT_ID],
            OTHER_ABSENT_SHORT_ID,
        ),
    ];
    for action in DECODING_DESCRIBES {
        for (case, ids, offending) in &rows {
            assert_refused(
                &router,
                case,
                action,
                ids,
                "InvalidInstanceID.NotFound",
                offending,
            )
            .await;
        }
    }
}

/// RULE 4: `DescribeInstanceStatus` refuses a page size beside an explicit id list with the
/// fixed sentence EC2 words that pair with, naming the parameters as its own refusal spells
/// them — whatever the ids are and whatever page size was asked for.
#[tokio::test]
async fn a_page_size_beside_instance_ids_names_this_operations_own_parameters() {
    let (router, _engine) = emulator();

    /// The whole sentence is fixed text: EC2 names the id selection first and the page size
    /// second, each in this operation's own spelling.
    const EXPECTED: &str =
        "The parameter instanceIdsSet cannot be used with the parameter maxResults";

    // (case, the ids the request names, the page size beside them)
    let rows: [(&str, Vec<&str>, &str); 4] = [
        (
            "one short-form id, the smallest page",
            vec![ABSENT_SHORT_ID],
            "5",
        ),
        (
            "one long-form id, the largest page",
            vec![UNDECODABLE_LONG_ID],
            "1000",
        ),
        (
            "two ids, a page in between",
            vec![ABSENT_SHORT_ID, OTHER_ABSENT_SHORT_ID],
            "50",
        ),
        (
            "ids in both forms, a page below the published minimum",
            vec![ABSENT_SHORT_ID, UNDECODABLE_LONG_ID],
            "1",
        ),
    ];
    for (case, ids, max_results) in rows {
        let mut params = instance_ids(&ids);
        params.push(p("MaxResults", max_results));
        let (status, body) = api(&router, "DescribeInstanceStatus", &params).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status}: {body}"
        );
        assert_eq!(
            error_code(&body),
            "InvalidParameterCombination",
            "case {case:?}: expected InvalidParameterCombination, got {body}"
        );
        assert_eq!(
            error_message(&body),
            EXPECTED,
            "case {case:?}: expected {EXPECTED:?}, got {:?}",
            error_message(&body)
        );
    }
}
