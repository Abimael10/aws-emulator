// SPDX-License-Identifier: CC0-1.0

//! The count an alias listing reports beside its page, driven over the wire through the
//! shared [`super::harness`].
//!
//! The rule under test: a `ListAliases` success reports, beside the page of aliases it
//! carries, how many aliases that page holds — `AliasCount` — so the member is present on
//! every answer and always describes the page in the same response: `0` for an endpoint
//! holding no aliases, the page length for a page the `Limit` shortened, and the number of
//! that key's aliases for a list narrowed by `KeyId`.
//!
//! Every assertion is about that rule over a TABLE, whose rows differ in how many aliases
//! the endpoint holds, whether the request narrows the list to one key and by which
//! identifier form, the page size asked for, how far through the pages the request is, and
//! whether the alias was created by a caller or is a predefined Amazon Web Services one
//! that a read associated with an Amazon Web Services managed key.

use serde_json::{json, Value};

use super::harness::{
    alias_arn, call, create_alias, create_key, ok, ok_in, refused, router, text, REGION,
};

/// The member a listing reports the size of its page in.
const ALIAS_COUNT: &str = "AliasCount";

/// The count `answer` reports, as a number — a listing that reports none at all fails the
/// case, which is the defect this suite is about.
fn counted(case: &str, answer: &Value) -> u64 {
    answer[ALIAS_COUNT]
        .as_u64()
        .unwrap_or_else(|| panic!("case {case}: expected a numeric {ALIAS_COUNT}, got {answer}"))
}

/// How many aliases the page in `answer` actually carries.
fn page_length(case: &str, answer: &Value) -> u64 {
    let aliases = answer["Aliases"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case}: expected an Aliases list, got {answer}"));
    u64::try_from(aliases.len()).expect("a page fits in u64")
}

/// Assert the count `answer` reports is the length of the page it carries, and that it is
/// `expected`.
fn assert_counted(case: &str, answer: &Value, expected: u64) {
    let actual = counted(case, answer);
    let length = page_length(case, answer);
    assert_eq!(
        actual, length,
        "case {case}: expected {ALIAS_COUNT} to be the length of the page ({length}), got \
         {actual} in {answer}"
    );
    assert_eq!(
        actual, expected,
        "case {case}: expected {ALIAS_COUNT} {expected}, got {actual} in {answer}"
    );
}

/// A listing reports the count of the page it returns, whatever narrows the list and
/// whatever page size was asked for.
#[tokio::test]
async fn a_listing_reports_the_count_of_the_page_it_returns() {
    let router = router();
    let first = create_key(&router, &json!({"Description": "first"})).await;
    let second = create_key(&router, &json!({"Description": "second"})).await;
    let first_id = text("setup", &first, "KeyId");
    let first_arn = text("setup", &first, "Arn");
    let second_id = text("setup", &second, "KeyId");

    // Three names for the first key, two for the second: five aliases in this Region.
    for alias in ["alias/count-one", "alias/count-two", "alias/count-three"] {
        create_alias(&router, alias, alias, &first_id).await;
    }
    for alias in ["alias/count-four", "alias/count-five"] {
        create_alias(&router, alias, alias, &second_id).await;
    }

    // (case, the request, the count the answer must report)
    let rows: Vec<(&str, Value, u64)> = vec![
        ("every alias of the Region", json!({}), 5),
        ("narrowed by key id", json!({"KeyId": first_id.clone()}), 3),
        ("narrowed by key ARN", json!({"KeyId": first_arn.clone()}), 3),
        ("narrowed to the other key", json!({"KeyId": second_id.clone()}), 2),
        ("a page of one", json!({"Limit": 1}), 1),
        ("a page of two", json!({"Limit": 2}), 2),
        ("a page the whole list fits in", json!({"Limit": 5}), 5),
        (
            "a page at the published bound",
            json!({"Limit": 100}),
            5,
        ),
        (
            "a narrowed list, shortened by a limit",
            json!({"KeyId": first_id.clone(), "Limit": 2}),
            2,
        ),
    ];

    for (case, request, expected) in rows {
        let answer = ok(&router, "ListAliases", &request).await;
        assert_counted(case, &answer, expected);
    }

    // A narrowing is by the KEY: "Specify the key ID or key ARN of the KMS key"
    // (`smithy_query(query="doc",shape="ListAliasesRequest",member="KeyId")`), so an
    // alias of that key is not an identifier this listing narrows by — it is refused for
    // its kind, and reports no count at all.
    let (status, code, message) =
        refused(&router, REGION, "ListAliases", &json!({"KeyId": "alias/count-one"})).await;
    assert_eq!(
        (status, code.as_str(), message.as_str()),
        (400, "InvalidArnException", "Key Aliases are not supported for this operation."),
        "case narrowed by an alias of that key: expected 400 InvalidArnException with the \
         fixed sentence, got {status} {code} ({message:?})"
    );
}

/// Every page of a walk reports its OWN length, including the short last page — so the
/// count follows the page rather than the size of the list behind it.
#[tokio::test]
async fn each_page_of_a_walk_reports_its_own_length() {
    let router = router();
    let metadata = create_key(&router, &json!({"Description": "paged"})).await;
    let key_id = text("setup", &metadata, "KeyId");
    let total: u64 = 5;
    for index in 1..=total {
        let alias = format!("alias/paged-{index}");
        create_alias(&router, &alias, &alias, &key_id).await;
    }

    // The page sizes span a walk of one page, several even pages, and pages that leave a
    // short last one.
    for limit in [1u64, 2, 3, 5] {
        let mut marker: Option<String> = None;
        let mut remaining = total;
        let mut page_number = 1;
        loop {
            let case = format!("limit {limit}, page {page_number}");
            let mut request = json!({"Limit": limit});
            if let Some(token) = &marker {
                request["Marker"] = json!(token);
            }
            let answer = ok(&router, "ListAliases", &request).await;
            assert_counted(&case, &answer, remaining.min(limit));
            remaining -= counted(&case, &answer);
            if answer["Truncated"] == json!(true) {
                marker = Some(text(&case, &answer, "NextMarker"));
                page_number += 1;
            } else {
                assert_eq!(
                    remaining, 0,
                    "case {case}: the pages must have reported every alias, {remaining} left \
                     over"
                );
                break;
            }
        }
    }
}

/// An endpoint holding no aliases reports an empty page and a count of zero — a count is
/// reported for an empty list too, not only for one with entries.
#[tokio::test]
async fn an_endpoint_holding_no_aliases_reports_a_count_of_zero() {
    let router = router();

    // The rows span an endpoint nothing has ever been created at, and a narrowing that
    // selects none of the aliases a Region does hold.
    let metadata = create_key(&router, &json!({"Description": "unaliased"})).await;
    let unaliased_id = text("setup", &metadata, "KeyId");
    create_alias(&router, "setup", "alias/held", &{
        let other = create_key(&router, &json!({"Description": "aliased"})).await;
        text("setup", &other, "KeyId")
    })
    .await;

    for (case, region, request) in [
        ("an endpoint with no aliases at all", "eu-west-1", json!({})),
        (
            "a key of this Region holding no alias",
            REGION,
            json!({"KeyId": unaliased_id.clone()}),
        ),
    ] {
        let answer = ok_in(&router, region, "ListAliases", &request).await;
        assert_counted(case, &answer, 0);
        assert_eq!(
            answer["Truncated"],
            json!(false),
            "case {case}: an empty list is not truncated, got {answer}"
        );
    }
}

/// A predefined Amazon Web Services alias, once a read has associated it with an Amazon
/// Web Services managed key, is listed under that key as one entry — count included. This
/// is the chain a client follows from a service's published key back to the alias that
/// names it.
///
/// The rows are three of the `aws/<service-name>` names the KMS model documents the form
/// of (`smithy_query(query="doc",shape="ListAliases")`: "their names have the format
/// aws/, such as aws/dynamodb"), each resolved through a different identifier form.
#[tokio::test]
async fn a_predefined_aws_alias_is_listed_under_its_managed_key_with_its_count() {
    let router = router();

    for (case, alias) in [
        ("the Backup managed key", "alias/aws/backup"),
        ("the DynamoDB managed key", "alias/aws/dynamodb"),
        ("the S3 managed key", "alias/aws/s3"),
    ] {
        // Asking for the predefined alias is what gives it a key, which is what a service
        // publishing its default key does.
        let metadata = ok(&router, "DescribeKey", &json!({"KeyId": alias})).await["KeyMetadata"]
            .clone();
        let key_id = text(case, &metadata, "KeyId");
        let key_arn = text(case, &metadata, "Arn");
        assert_eq!(
            metadata["KeyManager"],
            json!("AWS"),
            "case {case}: a predefined alias names an Amazon Web Services managed key, got \
             {metadata}"
        );

        for (form, identifier) in [
            ("the key ARN the service published", key_arn.clone()),
            ("the key id", key_id.clone()),
        ] {
            let case = format!("{case} read by {form}");
            let (status, answer) =
                call(&router, REGION, "ListAliases", &json!({"KeyId": identifier})).await;
            assert_eq!(
                status, 200,
                "case {case}: expected HTTP 200, got {status} with {answer}"
            );
            assert_counted(&case, &answer, 1);
            let entry = answer["Aliases"][0].clone();
            assert_eq!(
                (
                    entry["AliasName"].clone(),
                    entry["AliasArn"].clone(),
                    entry["TargetKeyId"].clone(),
                    answer["Truncated"].clone(),
                ),
                (
                    json!(alias),
                    json!(alias_arn(REGION, alias)),
                    json!(key_id),
                    json!(false),
                ),
                "case {case}: expected the one predefined alias of that key, got {answer}"
            );
        }

        // The chain runs from the KEY: the predefined alias itself, bare or as its ARN, is
        // not an identifier this listing takes — "Specify the key ID or key ARN of the KMS
        // key" (`smithy_query(query="doc",shape="ListAliasesRequest",member="KeyId")`) —
        // so a client holding only the alias has to resolve it to the key first.
        for (form, identifier) in [
            ("the alias itself", alias.to_string()),
            ("the alias ARN", alias_arn(REGION, alias)),
        ] {
            let case = format!("{case} read by {form}");
            let (status, code, message) =
                refused(&router, REGION, "ListAliases", &json!({"KeyId": identifier})).await;
            assert_eq!(
                (status, code.as_str(), message.as_str()),
                (400, "InvalidArnException", "Key Aliases are not supported for this operation."),
                "case {case}: expected 400 InvalidArnException with the fixed sentence, got \
                 {status} {code} ({message:?})"
            );
        }
    }
}
