// SPDX-License-Identifier: CC0-1.0

//! The `Description` a published default-quota row carries, over the wire.
//!
//! The rule under test is about the TEXT of a row rather than about one quota: a published
//! row carries the `Description` the service publishes for the quota its own `QuotaCode`
//! names, whatever class that quota belongs to — a row AWS publishes as a resource COUNT
//! carries the sentence published for that count exactly as a metered rate carries its
//! category's, and the member is absent only where no source available here states that
//! quota's own text. The text is reference data keyed on the quota's code, so it reaches a
//! caller identically through the default listing, through the default single read,
//! through the applied read and in every Region.
//!
//! The table spans what the rule generalises over: a count against a metered rate, a
//! one-sentence text against a two-sentence one, a count of the whole account's resources
//! against one counted inside a parent resource, an adjustable quota against a fixed one,
//! values from 15 to 1000, and three service codes that word their sentences in three
//! voices (two closing with a full stop, one terse with none). Two of its rows are quotas
//! this suite's issue never cited.
//!
//! The harness (`client`, `emulator`, `default_quotas`) is the one `quota_workflow.rs`
//! owns.

use serde_json::{json, Value};

use crate::quota_workflow::{client, default_quotas, emulator};

/// The modelled ceiling on the text: `ServiceQuota`'s `Description` carries `@length
/// 0..350` (`smithy_query(query="shape", shape="ServiceQuota")`).
const DESCRIPTION_MAX: usize = 350;

/// One published row and the text its service publishes for it.
///
/// SOURCE: AWS's published Service Quotas data for each service code, as
/// `src/state/quota_rows/cognito_idp.rs`, `src/state/quota_rows/dax.rs` and
/// `src/state/quota_rows/docdb.rs` carry it, and the recorded default-quota listing of
/// each code, which is what states the text each of these quotas is published with (those
/// modules name, row by row, the few facts they take from a service's own documentation
/// instead).
struct Published {
    /// The `ServiceCode` that publishes the quota.
    service_code: &'static str,
    /// AWS's own `QuotaCode` for it, which is what the text travels with.
    quota_code: &'static str,
    /// `QuotaName`, as AWS publishes it.
    quota_name: &'static str,
    /// The `Description` AWS publishes for THAT quota.
    description: &'static str,
    /// Whether AWS publishes the quota as a METERED RATE — a window and an `AWS/Usage`
    /// call count beside the text. A count carries neither and carries its text all the
    /// same, which is what this suite is about.
    metered: bool,
}

/// The rows asserted here.
const ROWS: &[Published] = &[
    // Amazon Cognito User Pools' two resource COUNTS: the pools of the account, and the
    // resource servers inside one pool. Neither is a rate, and each carries the sentence
    // the service publishes for it — the second of them in two sentences, the first in
    // one.
    Published {
        service_code: "cognito-idp",
        quota_code: "L-66E6DF30",
        quota_name: "User pools per account",
        description: "The maximum number of user pools that you can create in this account per \
                      region.",
        metered: false,
    },
    Published {
        service_code: "cognito-idp",
        quota_code: "L-7CDAF993",
        quota_name: "Resource servers per user pool",
        description: "The maximum number of resource servers per user pool. A resource server is \
                      a server for access-protected resources.",
        metered: false,
    },
    // The same service's metered CATEGORY RATES, for contrast: a fixed rate served per
    // user pool, a fixed rate served for the whole account, and one AWS lets a rate limit
    // be provisioned within (a quota this suite's issue never cited).
    Published {
        service_code: "cognito-idp",
        quota_code: "L-60A0B411",
        quota_name: "Rate of UserPoolUpdate requests",
        description: "The maximum combined call rate (requests per second) for all API operations \
                      in the UserPoolUpdate category. All operations within a category share the \
                      same quota. For the list of included operations, see \
                      https://docs.aws.amazon.com/cognito/latest/developerguide/\
                      limits.html#category_operations.",
        metered: true,
    },
    Published {
        service_code: "cognito-idp",
        quota_code: "L-12C4D74A",
        quota_name: "Rate of UserPoolClientUpdate requests per account",
        description: "The maximum combined call rate (requests per second) for all API operations \
                      in the UserPoolClientUpdate category. All operations within a category \
                      share the same quota. For the list of included operations, see \
                      https://docs.aws.amazon.com/cognito/latest/developerguide/\
                      limits.html#category_operations.",
        metered: true,
    },
    Published {
        service_code: "cognito-idp",
        quota_code: "L-D6BD5178",
        quota_name: "Rate of UserRead requests",
        description: "The maximum combined call rate (requests per second) you can request for \
                      all API operations in the UserRead category. Provision your rate limit \
                      within this quota from the Amazon Cognito console or APIs. For the list of \
                      included operations, see https://docs.aws.amazon.com/cognito/latest/\
                      developerguide/limits.html#category_operations.",
        metered: true,
    },
    // Two other services' counts, in their own voices: one closing with a full stop and
    // naming the Region it counts in, one terse with no closing stop. Neither was cited
    // by this suite's issue, and the rule holds over them just the same.
    Published {
        service_code: "dax",
        quota_code: "L-315AFD08",
        quota_name: "Parameter groups",
        description: "The maximum number of parameter groups in a single AWS region.",
        metered: false,
    },
    Published {
        service_code: "docdb",
        quota_code: "L-02DEA053",
        quota_name: "Subnet groups",
        description: "The maximum number of DB subnet groups",
        metered: false,
    },
];

/// Every published row reports the text its service publishes for that quota, on every
/// plane that resolves the quota's code and in every Region.
#[tokio::test]
async fn a_published_row_reports_the_text_its_service_publishes_for_that_quota() {
    let engine = emulator();

    for region in ["us-west-2", "ap-northeast-1"] {
        for row in ROWS {
            let caller = client(&engine, region);
            let case = format!("{region} {}/{}", row.service_code, row.quota_code);
            let listed = default_quotas(&caller, row.service_code).await;
            let served = listed
                .iter()
                .find(|quota| quota["QuotaCode"] == json!(row.quota_code))
                .unwrap_or_else(|| {
                    panic!("case {case}: the listing must publish this quota, got {listed:?}")
                });

            assert_eq!(
                (served["QuotaName"].clone(), served["Description"].clone()),
                (json!(row.quota_name), json!(row.description)),
                "case {case}: expected the published name and the text published for it, got \
                 {served}"
            );
            let text = served["Description"].as_str().unwrap_or_default();
            assert!(
                !text.is_empty() && text.chars().count() <= DESCRIPTION_MAX,
                "case {case}: a published text is 1..={DESCRIPTION_MAX} characters, got {} in \
                 {served}",
                text.chars().count()
            );

            // A count carries no window and no call count — and its text all the same;
            // a metered rate carries both beside the same text.
            let rate_facts = !served.get("Period").is_none_or(Value::is_null)
                && !served.get("UsageMetric").is_none_or(Value::is_null);
            assert_eq!(
                rate_facts, row.metered,
                "case {case}: a window and a call count are published for a metered rate and \
                 for nothing else, got {served}"
            );

            // The text travels with the code: both single reads resolve the same text for
            // the same pair.
            let pair = json!({
                "ServiceCode": row.service_code,
                "QuotaCode": row.quota_code,
            });
            for operation in ["GetAWSDefaultServiceQuota", "GetServiceQuota"] {
                let read = caller.ok(operation, pair.clone()).await["Quota"].clone();
                assert_eq!(
                    read["Description"],
                    json!(row.description),
                    "case {case}: {operation} must resolve the text published for this quota, \
                     got {read}"
                );
            }
        }
    }
}

/// Amazon Cognito User Pools publishes a text for EVERY quota of its catalog, counts
/// included: a row of that service code with no `Description` would be one whose
/// published sentence a caller cannot render.
#[tokio::test]
async fn every_user_pool_quota_is_published_with_its_text() {
    let engine = emulator();
    let caller = client(&engine, "eu-west-1");

    let listed = default_quotas(&caller, "cognito-idp").await;
    assert!(
        listed.len() >= ROWS.iter().filter(|row| row.service_code == "cognito-idp").count(),
        "case cognito-idp: the catalog must publish the cited rows, got {listed:?}"
    );
    for quota in &listed {
        let case = format!("cognito-idp/{}", quota["QuotaCode"].as_str().unwrap_or(""));
        let text = quota["Description"].as_str().unwrap_or_default();
        assert!(
            !text.is_empty() && text.chars().count() <= DESCRIPTION_MAX,
            "case {case}: every published row carries the quota's own text, within \
             1..={DESCRIPTION_MAX} characters, got {quota}"
        );
    }
}
