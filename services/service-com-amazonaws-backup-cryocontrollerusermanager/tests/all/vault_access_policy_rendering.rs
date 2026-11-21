// SPDX-License-Identifier: CC0-1.0

//! `PutBackupVaultAccessPolicy` / `GetBackupVaultAccessPolicy`: the LAYOUT an
//! attached vault policy is reported in.
//!
//! The rule under test: a service that stores a resource policy does not echo
//! the caller's bytes back — it reports its own rendering of the document, so
//! the whitespace between the document's JSON tokens is the store's and a value
//! list holding exactly one value is reported as that value alone, while
//! everything that is the caller's own data (member order, the statement list,
//! each number's spelling, each string literal's escapes) survives untouched.
//! The rendering is therefore a FIXED POINT: a client that puts back the
//! document it read sees the same bytes again.
//!
//! The expected layouts below are written out by hand rather than computed, so
//! they pin the wire form a client parses: each member of a JSON object on its
//! own line indented two spaces per enclosing object, `" : "` between a key and
//! its value, arrays inline (`[ `, `, `, ` ]`), an empty object and an empty
//! array written `{ }` and `[ ]`, and a one-value `Action`/`Resource`/principal
//! list written without its brackets.
//!
//! That layout is AWS's own, not this emulator's choice: a vault-policy round
//! trip against the real service answers the policy read with the document
//! re-indented that way, with a statement's one-action `Action` list reported as
//! the bare action string while the one-statement `Statement` list keeps its
//! brackets, which is the "a string or an array of strings" form the IAM JSON
//! policy reference gives those elements and the array of statements does not.
//! `service_common::iam_policy::canonical_document` is where the tree states it
//! once for every service that reports a stored policy.
//!
//! The rows differ along what the rendering generalises over: the whitespace
//! the caller laid the document out with, the arity of a value list (none, one,
//! two), the element type of a list (strings against statement objects), the
//! nesting depth, the member order and the JSON types a condition carries.
//!
//! Every case drives real HTTP through the router; the harness is the shared one
//! in `vault_and_backup_job.rs`, and the route bindings are the ones the sibling
//! `vault_access_policy` suite states.

use crate::vault_access_policy::{get_policy, principal_arn, put_policy, vault_named};
use crate::vault_and_backup_job::{error_code, router, string_at};

/// The lines of a document, joined as the rendering writes them: one line
/// break between them and none after the last.
fn lines(rows: &[&str]) -> String {
    rows.join("\n")
}

/// One document, submitted as a client that minifies its JSON sends it.
const COMPACT: &str = "{\"Version\":\"2012-10-17\",\"Statement\":[{\"Sid\":\"One\",\
                       \"Effect\":\"Allow\",\"Principal\":\"*\",\
                       \"Action\":[\"backup:DescribeBackupVault\",\
                       \"backup:ListRecoveryPointsByBackupVault\"]}]}";

/// The SAME document, laid out by a caller whose printer indents with tabs.
fn tab_indented() -> String {
    lines(&[
        "{",
        "\t\"Version\": \"2012-10-17\",",
        "\t\"Statement\": [",
        "\t\t{",
        "\t\t\t\"Sid\": \"One\",",
        "\t\t\t\"Effect\": \"Allow\",",
        "\t\t\t\"Principal\": \"*\",",
        "\t\t\t\"Action\": [",
        "\t\t\t\t\"backup:DescribeBackupVault\",",
        "\t\t\t\t\"backup:ListRecoveryPointsByBackupVault\"",
        "\t\t\t]",
        "\t\t}",
        "\t]",
        "}",
    ])
}

/// The one layout that document is reported in, whichever of the two above was
/// submitted.
fn rendered_one() -> String {
    lines(&[
        "{",
        "  \"Version\" : \"2012-10-17\",",
        "  \"Statement\" : [ {",
        "    \"Sid\" : \"One\",",
        "    \"Effect\" : \"Allow\",",
        "    \"Principal\" : \"*\",",
        "    \"Action\" : [ \"backup:DescribeBackupVault\", \
         \"backup:ListRecoveryPointsByBackupVault\" ]",
        "  } ]",
        "}",
    ])
}

/// The (submitted, reported) pairs this suite asserts, one per form the
/// rendering generalises over.
fn cases() -> Vec<(&'static str, String, String)> {
    let principal = principal_arn("reader");
    vec![
        (
            "a compact document",
            COMPACT.to_string(),
            rendered_one(),
        ),
        (
            "the same document indented with tabs",
            tab_indented(),
            rendered_one(),
        ),
        (
            "the document as the read already reported it",
            rendered_one(),
            rendered_one(),
        ),
        (
            "one-value lists, an empty list, and the statement list that is not a value list",
            format!(
                "{{\"Statement\":[{{\"Effect\":\"Allow\",\
                 \"Principal\":{{\"AWS\":[\"{principal}\"]}},\
                 \"Action\":[\"backup:DescribeBackupVault\"],\"Resource\":[]}}]}}"
            ),
            lines(&[
                "{",
                "  \"Statement\" : [ {",
                "    \"Effect\" : \"Allow\",",
                "    \"Principal\" : {",
                format!("      \"AWS\" : \"{principal}\"").as_str(),
                "    },",
                "    \"Action\" : \"backup:DescribeBackupVault\",",
                "    \"Resource\" : [ ]",
                "  } ]",
                "}",
            ]),
        ),
        (
            "a condition, whose values keep the JSON types the caller wrote",
            "{\"Version\":\"2012-10-17\",\"Statement\":{\"Effect\":\"Deny\",\
             \"Principal\":\"*\",\"Action\":\"backup:DeleteRecoveryPoint\",\
             \"Condition\":{\"NumericLessThan\":{\"backup:RetentionDays\":35},\
             \"Bool\":{\"aws:SecureTransport\":false},\
             \"StringEquals\":{\"aws:PrincipalOrgID\":[\"o-example\"]}}}}"
                .to_string(),
            lines(&[
                "{",
                "  \"Version\" : \"2012-10-17\",",
                "  \"Statement\" : {",
                "    \"Effect\" : \"Deny\",",
                "    \"Principal\" : \"*\",",
                "    \"Action\" : \"backup:DeleteRecoveryPoint\",",
                "    \"Condition\" : {",
                "      \"NumericLessThan\" : {",
                "        \"backup:RetentionDays\" : 35",
                "      },",
                "      \"Bool\" : {",
                "        \"aws:SecureTransport\" : false",
                "      },",
                "      \"StringEquals\" : {",
                "        \"aws:PrincipalOrgID\" : \"o-example\"",
                "      }",
                "    }",
                "  }",
                "}",
            ]),
        ),
        (
            "the member order and the number spelling the caller wrote",
            "{\"Statement\":[{\"Action\":\"backup:DescribeBackupVault\",\
             \"Principal\":{\"AWS\":\"*\"},\"Effect\":\"Allow\",\
             \"Condition\":{\"NumericLessThanEquals\":{\"backup:RetentionDays\":35.50}}}],\
             \"Version\":\"2012-10-17\"}"
                .to_string(),
            lines(&[
                "{",
                "  \"Statement\" : [ {",
                "    \"Action\" : \"backup:DescribeBackupVault\",",
                "    \"Principal\" : {",
                "      \"AWS\" : \"*\"",
                "    },",
                "    \"Effect\" : \"Allow\",",
                "    \"Condition\" : {",
                "      \"NumericLessThanEquals\" : {",
                "        \"backup:RetentionDays\" : 35.50",
                "      }",
                "    }",
                "  } ],",
                "  \"Version\" : \"2012-10-17\"",
                "}",
            ]),
        ),
        (
            "a string literal's own escapes, which are not decoded",
            "{\"Version\":\"2012-10-17\",\"Statement\":[{\"Sid\":\"caf\\u00e9\",\
             \"Effect\":\"Allow\",\"Principal\":{\"AWS\":\"*\"},\
             \"Action\":\"backup:DescribeBackupVault\",\"Resource\":\"*\"}]}"
                .to_string(),
            lines(&[
                "{",
                "  \"Version\" : \"2012-10-17\",",
                "  \"Statement\" : [ {",
                "    \"Sid\" : \"caf\\u00e9\",",
                "    \"Effect\" : \"Allow\",",
                "    \"Principal\" : {",
                "      \"AWS\" : \"*\"",
                "    },",
                "    \"Action\" : \"backup:DescribeBackupVault\",",
                "    \"Resource\" : \"*\"",
                "  } ]",
                "}",
            ]),
        ),
        (
            "an empty condition block",
            "{\"Statement\":{\"Effect\":\"Allow\",\"Principal\":\"*\",\
             \"Action\":\"backup:DescribeBackupVault\",\"Condition\":{}}}"
                .to_string(),
            lines(&[
                "{",
                "  \"Statement\" : {",
                "    \"Effect\" : \"Allow\",",
                "    \"Principal\" : \"*\",",
                "    \"Action\" : \"backup:DescribeBackupVault\",",
                "    \"Condition\" : { }",
                "  }",
                "}",
            ]),
        ),
    ]
}

/// A document a vault accepts is reported in the store's own layout, and
/// putting back what was read reports the very same bytes.
#[tokio::test]
async fn an_attached_policy_is_reported_in_the_layout_a_stored_policy_is_reported_in() {
    let router = router();
    for (index, (case, submitted, expected)) in cases().into_iter().enumerate() {
        let vault = format!("render-vault-{index}");
        vault_named(&router, &vault).await;

        let (status, attached) = put_policy(&router, &vault, &submitted).await;
        assert_eq!(
            (status, error_code(&attached).as_str()),
            (200, "<none>"),
            "case {case:?}: expected PutBackupVaultAccessPolicy 200, got {status} {attached:?}"
        );

        let (status, read) = get_policy(&router, &vault).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected GetBackupVaultAccessPolicy 200, got {status} {read:?}"
        );
        let reported = string_at(&read, "Policy");
        assert_eq!(
            reported, expected,
            "case {case:?}: the read must report the stored layout, got {reported:?}"
        );

        // The layout is a fixed point: a client that puts back the document it
        // read sees the same bytes again.
        let (status, again) = put_policy(&router, &vault, &reported).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the reported document must itself be acceptable, got {status} \
             {again:?}"
        );
        let (status, read) = get_policy(&router, &vault).await;
        let settled = string_at(&read, "Policy");
        assert_eq!(
            (status, settled.as_str()),
            (200, expected.as_str()),
            "case {case:?}: re-attaching what was read must change nothing, got {status} \
             {settled:?}"
        );
    }
}

/// The layout the read reports is the STORE's, so two vaults handed the same
/// document in different layouts report byte-identical policies.
#[tokio::test]
async fn the_reported_layout_does_not_depend_on_how_the_caller_laid_the_document_out() {
    let router = router();
    let layouts = [
        ("minified", COMPACT.to_string()),
        ("indented with tabs", tab_indented()),
        ("already rendered", rendered_one()),
    ];

    let mut reported: Vec<(&str, String)> = Vec::new();
    for (index, (case, submitted)) in layouts.iter().enumerate() {
        let vault = format!("layout-vault-{index}");
        vault_named(&router, &vault).await;
        let (status, attached) = put_policy(&router, &vault, submitted).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected PutBackupVaultAccessPolicy 200, got {status} {attached:?}"
        );
        let (status, read) = get_policy(&router, &vault).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected GetBackupVaultAccessPolicy 200, got {status} {read:?}"
        );
        reported.push((*case, string_at(&read, "Policy")));
    }

    let (first_case, first) = &reported[0];
    for (case, policy) in &reported[1..] {
        assert_eq!(
            policy, first,
            "case {case:?}: the same document must be reported as it is for {first_case:?}, got \
             {policy:?}"
        );
    }
}
