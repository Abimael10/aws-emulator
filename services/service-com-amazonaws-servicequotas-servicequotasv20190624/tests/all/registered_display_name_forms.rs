// SPDX-License-Identifier: CC0-1.0

//! The registered FORM of a service's display name: the parenthesised tail the
//! integrated-service inventory holds on a `ServiceCode` is reproduced as registered.
//!
//! THE RULE, stated over the inventory rather than over one entry: a `ServiceCode`'s
//! `ServiceName` is the display name the integrated-service inventory registers for THAT
//! code, reproduced character for character — the parenthesised tail included, whether it
//! is a bare abbreviation, an abbreviation carrying the product's `AWS`/`Amazon` prefix,
//! the product's short name, or a clause naming the product's former name — and never a
//! form derived from the product's own prefixed name, from the code, or from a sibling
//! code of the same product (`smithy_query(query="shape", shape="ServiceInfo")`:
//! `ServiceInfo{ServiceCode @length 1..63 @pattern ^[a-zA-Z][a-zA-Z0-9-]{1,63}$,
//! ServiceName}` — the model fixes the entry's shape and enumerates no catalog, so the
//! registered strings are recorded behaviour).
//!
//! What this suite owns, and what it does NOT: `inventory_registered_display_names.rs`
//! owns COVERAGE and DISCOVERY (which codes are enumerated, and where in a paged walk),
//! `integrated_service_display_names.rs` owns the SHAPE of a name and that a name is the
//! code's own rather than a sibling's, and `published_row_class_wording.rs` owns that
//! every quota row repeats the name of its code. None of them spans the forms the
//! parenthesised TAIL takes, which is the one a re-wording silently normalises — an
//! abbreviation gaining the product prefix, a short name losing it, a former-name clause
//! dropped — so that is what the table here is: one row per form, each carrying the
//! re-worded string the form invites, so a normalisation in either direction fails.
//!
//! The harness (`client`, `emulator`, `service_inventory`, `default_quotas`, `walk_pages`)
//! is the one `quota_workflow.rs` owns.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::quota_workflow::{
    client, default_quotas, emulator, service_inventory, walk_pages, Client,
};

/// One row of the integrated-service inventory.
struct Registered {
    code: &'static str,
    /// The display name the inventory registers for `code`.
    name: &'static str,
    /// The re-worded form of `name` this row exists to refuse: the same product name with
    /// the parenthesised tail normalised the other way.
    rewritten: &'static str,
    /// What makes this row a different FORM of registered name from the others.
    form: &'static str,
}

/// The rows this suite pins.
///
/// SOURCE: AWS's own integrated-service inventory — the `ServiceInfo{ServiceCode,
/// ServiceName}` pairs `aws service-quotas list-services` reports, which is the table
/// `src/state.rs::SEED_SERVICE_CATALOG` carries, and which the recorded default-quota
/// listing of each of these codes repeats on every one of its rows.
///
/// The rows differ along what the rule generalises over — which FORM the registered tail
/// takes — rather than repeating one case: a bare abbreviation, an abbreviation carrying
/// the product's prefix (both `AWS` and `Amazon`), the product's short name, a clause
/// naming a former product name, and a name with no tail and no prefix at all; and, for
/// two products whose planes come under a pair of codes, BOTH codes, because a sibling's
/// tail is exactly what a re-wording reaches for. Most rows name codes no report about
/// this table cited.
const REGISTERED: &[Registered] = &[
    Registered {
        code: "elasticfilesystem",
        name: "Amazon Elastic File System (EFS)",
        rewritten: "Amazon Elastic File System (Amazon EFS)",
        form: "a bare abbreviation, where the product's own prefixed abbreviation exists",
    },
    Registered {
        code: "acm",
        name: "AWS Certificate Manager (ACM)",
        rewritten: "AWS Certificate Manager (AWS ACM)",
        form: "a bare abbreviation under an AWS-prefixed product name",
    },
    Registered {
        code: "fis",
        name: "AWS Fault Injection Service (AWS FIS)",
        rewritten: "AWS Fault Injection Service (FIS)",
        form: "an abbreviation that DOES carry the AWS prefix",
    },
    Registered {
        code: "s3",
        name: "Amazon Simple Storage Service (Amazon S3)",
        rewritten: "Amazon Simple Storage Service (S3)",
        form: "an abbreviation that DOES carry the Amazon prefix",
    },
    Registered {
        code: "geo",
        name: "Amazon Location Service (Amazon Location)",
        rewritten: "Amazon Location Service (Location)",
        form: "the product's short name in parentheses",
    },
    Registered {
        code: "elasticloadbalancing",
        name: "Elastic Load Balancing (ELB)",
        rewritten: "Amazon Elastic Load Balancing (ELB)",
        form: "an abbreviation under a product name with no prefix",
    },
    Registered {
        code: "sso",
        name: "AWS IAM Identity Center (successor to AWS Single Sign-On)",
        rewritten: "AWS IAM Identity Center",
        form: "a clause naming the product's former name",
    },
    Registered {
        code: "signin",
        name: "AWS IAM Identity Center",
        rewritten: "AWS IAM Identity Center (successor to AWS Single Sign-On)",
        form: "the sibling code of that same product, registered WITHOUT the clause",
    },
    Registered {
        code: "vpc-lattice",
        name: "VPC Lattice",
        rewritten: "Amazon VPC Lattice",
        form: "a product name registered with no prefix and no tail",
    },
    Registered {
        code: "vpc",
        name: "Amazon Virtual Private Cloud (Amazon VPC)",
        rewritten: "Amazon VPC",
        form: "the neighbouring code of that family, registered with prefix and tail",
    },
];

/// `ServiceCode -> ServiceName`, as `ListServices` reports the whole inventory.
async fn inventory(client: &Client) -> BTreeMap<String, String> {
    service_inventory(client)
        .await
        .iter()
        .filter_map(|service| {
            Some((
                service["ServiceCode"].as_str()?.to_string(),
                service["ServiceName"].as_str()?.to_string(),
            ))
        })
        .collect()
}

/// The applied quotas of one service code.
async fn applied_quotas(client: &Client, service_code: &str) -> Vec<Value> {
    walk_pages(
        client,
        "ListServiceQuotas",
        &json!({"ServiceCode": service_code}),
        "Quotas",
        None,
    )
    .await
}

/// Each registered tail is reported as registered — never normalised into, or out of, the
/// product's own prefixed form — by `ListServices` and by every quota read of that code.
#[tokio::test]
async fn a_registered_tail_is_reported_as_registered_by_every_read() {
    let engine = emulator();
    // The inventory is the same in every Region, so the two endpoints make this about the
    // registered string rather than one Region's view of it. The quota reads run against
    // the first of them; the one table backs both Regions.
    let regions = ["us-west-2", "eu-west-1"];
    let mut exercised_through_quota_rows: Vec<&str> = Vec::new();

    for (at, region) in regions.iter().enumerate() {
        let client = client(&engine, region);
        let listed = inventory(&client).await;

        for row in REGISTERED {
            let case = format!("{region} {} ({})", row.code, row.form);
            let reported = listed.get(row.code).map(String::as_str);
            assert_eq!(
                reported,
                Some(row.name),
                "case {case}: expected the registered display name, not the re-worded form \
                 {:?}, got {reported:?}",
                row.rewritten
            );

            if at > 0 {
                continue;
            }

            // A listed code the catalog publishes no quota row for is still listed (the
            // inventory is the whole integrated-service list, not the subset with rows),
            // so the quota-read half of the rule is checked wherever there ARE rows.
            let defaults = default_quotas(&client, row.code).await;
            if defaults.is_empty() {
                continue;
            }
            exercised_through_quota_rows.push(row.code);
            for quota in &defaults {
                assert_eq!(
                    quota["ServiceName"],
                    json!(row.name),
                    "case {case}: every default-quota row carries the registered name, got \
                     {quota}"
                );
            }

            // The single read of one row, and the APPLIED listing, report the same string:
            // the name comes out of one table rather than being re-formed per operation.
            let code = defaults[0]["QuotaCode"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case}: a default quota without a code"));
            let single = client
                .ok(
                    "GetAWSDefaultServiceQuota",
                    json!({"ServiceCode": row.code, "QuotaCode": code}),
                )
                .await;
            assert_eq!(
                single["Quota"]["ServiceName"],
                json!(row.name),
                "case {case}/{code}: the single read carries the registered name, got {single}"
            );

            for quota in applied_quotas(&client, row.code).await {
                assert_eq!(
                    quota["ServiceName"],
                    json!(row.name),
                    "case {case}: every applied row carries the registered name, got {quota}"
                );
            }
        }
    }

    // The quota-read half of the rule has to have been exercised on most of the table,
    // not on one lucky row.
    assert!(
        exercised_through_quota_rows.len() * 2 > REGISTERED.len(),
        "case the whole table: expected the quota reads of most registered codes to be \
         exercised, got {exercised_through_quota_rows:?} of {} rows",
        REGISTERED.len()
    );
}
