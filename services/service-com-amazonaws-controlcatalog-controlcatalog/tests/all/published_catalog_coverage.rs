// SPDX-License-Identifier: CC0-1.0

//! What the catalog has to hold for a client to read a control it discovered elsewhere.
//!
//! The rule under test: the Control Catalog is an Amazon Web Services-published,
//! account- and Region-independent read-only dataset (its ARNs carry neither account nor
//! Region), so a control the catalog publishes is served under the identity the catalog
//! publishes for it — one record per published control, reachable BOTH by its published
//! ARN through `GetControl` and in every filtered `ListControls` page that selects it,
//! with the page sequence walking the WHOLE filtered set whatever page size drives it.
//! `GetControl` "[r]eturns details about a specific control" and `ListControls`
//! "[r]eturns a paginated list of all available controls in the Control Catalog library"
//! (`smithy_query(query="operation", operation="GetControl,ListControls",
//! model_path="services/service-com-amazonaws-controlcatalog-controlcatalog/smithy-model/com-amazonaws-controlcatalog-controlcatalog.json")`).
//! The refusal the other side of the rule — an ARN the catalog publishes nothing for —
//! is pinned by `all/refusal_wording.rs`, and the member-for-member projection of a
//! served control by `all/control_catalog.rs`; this suite is about what the catalog must
//! HOLD and how far a filtered walk reaches.
//!
//! Each test is a TABLE rather than one control picked by hand: the implementation table
//! below spans both implementing surfaces the catalog publishes controls over and the
//! services they govern, the walk test takes EVERY control the service serves, and the
//! page sizes span one control per page, a partial page and the modeled maximum of
//! `MaxListControlsResults` (`@range 1..100`).

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::control_catalog::{by_arn, drain, Emulator};

/// The AWS Config implementation type, as the catalog spells it.
const CONFIG_RULE: &str = "AWS::Config::ConfigRule";

/// The Security Hub implementation type, as the catalog spells it.
const SECURITY_HUB_CONTROL: &str = "AWS::SecurityHub::SecurityControl";

/// Implementations the catalog publishes a control over: `(ImplementationType,
/// ImplementationIdentifier)`.
///
/// Source: the implementing surfaces' own published identifiers — an AWS Config managed
/// rule identifier and a Security Hub control id — each of which the catalog publishes a
/// control for. The rows span both surfaces and the services they govern (certificate
/// authorities, user pools, file systems, tables, data warehouses, functions, load
/// balancers, security groups, identities and keys), and they are deliberately NOT the
/// implementations any one recorded page named.
const PUBLISHED_IMPLEMENTATIONS: &[(&str, &str)] = &[
    (CONFIG_RULE, "ACMPCA_CERTIFICATE_AUTHORITY_TAGGED"),
    (CONFIG_RULE, "COGNITO_USER_POOL_ADVANCED_SECURITY_ENABLED"),
    (CONFIG_RULE, "EFS_ENCRYPTED_CHECK"),
    (CONFIG_RULE, "DYNAMODB_TABLE_ENCRYPTED_KMS"),
    (CONFIG_RULE, "SNS_ENCRYPTED_KMS"),
    (CONFIG_RULE, "OPENSEARCH_ENCRYPTED_AT_REST"),
    (CONFIG_RULE, "REDSHIFT_CLUSTER_CONFIGURATION_CHECK"),
    (CONFIG_RULE, "REDSHIFT_REQUIRE_TLS_SSL"),
    (CONFIG_RULE, "ELBV2_ACM_CERTIFICATE_REQUIRED"),
    (CONFIG_RULE, "S3_BUCKET_LOGGING_ENABLED"),
    (CONFIG_RULE, "API_GW_EXECUTION_LOGGING_ENABLED"),
    (CONFIG_RULE, "LAMBDA_FUNCTION_PUBLIC_ACCESS_PROHIBITED"),
    (CONFIG_RULE, "EBS_SNAPSHOT_PUBLIC_RESTORABLE_CHECK"),
    (CONFIG_RULE, "IAM_USER_UNUSED_CREDENTIALS_CHECK"),
    (CONFIG_RULE, "RESTRICTED_INCOMING_TRAFFIC"),
    (CONFIG_RULE, "CMK_BACKING_KEY_ROTATION_ENABLED"),
    (CONFIG_RULE, "ROOT_ACCOUNT_HARDWARE_MFA_ENABLED"),
    (SECURITY_HUB_CONTROL, "S3.1"),
    (SECURITY_HUB_CONTROL, "EC2.2"),
    (SECURITY_HUB_CONTROL, "RDS.2"),
    (SECURITY_HUB_CONTROL, "CloudTrail.4"),
    (SECURITY_HUB_CONTROL, "KMS.4"),
    (SECURITY_HUB_CONTROL, "GuardDuty.1"),
];

/// Page sizes the walks below are driven with: the service's own default, one control per
/// page, a partial page, and the modeled maximum of `MaxListControlsResults`
/// (`@range 1..100`).
const PAGE_SIZES: [Option<u32>; 4] = [None, Some(1), Some(7), Some(100)];

/// Every control the catalog publishes, keyed by its ARN.
async fn catalog(emulator: &Emulator) -> BTreeMap<String, Value> {
    let controls = drain(emulator, "/list-controls", "Controls", None, "{}").await;
    assert!(
        !controls.is_empty(),
        "case catalog: ListControls must publish the catalog's controls"
    );
    controls
        .into_iter()
        .map(|control| {
            let arn = control["Arn"]
                .as_str()
                .unwrap_or_else(|| panic!("case {control:?}: a control carries its ARN"))
                .to_string();
            (arn, control)
        })
        .collect()
}

/// The request body that narrows `ListControls` to one implementation identifier.
fn identifier_filter(identifier: &str) -> String {
    format!(r#"{{"Filter":{{"Implementations":{{"Identifiers":["{identifier}"]}}}}}}"#)
}

/// The request body that narrows `ListControls` to one implementation type.
fn type_filter(implementation_type: &str) -> String {
    format!(r#"{{"Filter":{{"Implementations":{{"Types":["{implementation_type}"]}}}}}}"#)
}

/// Every implementation the catalog records a published control for is served as one: the
/// `Identifiers` filter selects it at every page size, and each control it selects reads
/// back by its own published ARN.
#[tokio::test]
async fn every_published_implementation_is_served_as_a_catalog_control() {
    let emulator = Emulator::new();
    let published = catalog(&emulator).await;

    for (implementation_type, identifier) in PUBLISHED_IMPLEMENTATIONS {
        let body = identifier_filter(identifier);
        let mut per_page_size: Vec<(Option<u32>, Vec<Value>)> = Vec::new();

        for page_size in PAGE_SIZES {
            let case = format!("{identifier} maxResults={page_size:?}");
            let selected = drain(&emulator, "/list-controls", "Controls", page_size, &body).await;
            assert!(
                !selected.is_empty(),
                "case {case}: the catalog must publish a control over this implementation"
            );

            for control in &selected {
                let arn = control["Arn"]
                    .as_str()
                    .unwrap_or_else(|| panic!("case {case}: a control carries its ARN"));
                assert_eq!(
                    (
                        control["Implementation"]["Type"].as_str(),
                        control["Implementation"]["Identifier"].as_str(),
                    ),
                    (Some(*implementation_type), Some(*identifier)),
                    "case {case}: expected the implementing surface \
                     {implementation_type}/{identifier}, got {:?}",
                    control["Implementation"]
                );
                assert!(
                    published.contains_key(arn),
                    "case {case}: the unfiltered listing must publish {arn} too"
                );

                // A client that harvested the ARN reads the control back by it.
                let read_back = emulator
                    .ok("/get-control", &format!(r#"{{"ControlArn":"{arn}"}}"#))
                    .await;
                assert_eq!(
                    read_back["Arn"].as_str(),
                    Some(arn),
                    "case {case}: GetControl must resolve the published ARN, got {read_back:?}"
                );
                assert_eq!(
                    read_back["Name"].as_str(),
                    control["Name"].as_str(),
                    "case {case}: GetControl and ListControls must report one name, got \
                     {read_back:?}"
                );
            }
            per_page_size.push((page_size, by_arn(selected)));
        }

        // The page size decides how the filtered set is walked, never how much of it the
        // walk reaches.
        let (first_size, first) = &per_page_size[0];
        for (page_size, selected) in &per_page_size[1..] {
            assert_eq!(
                selected, first,
                "case {identifier} maxResults={page_size:?}: expected the same controls \
                 the walk at maxResults={first_size:?} reached, got {selected:?}"
            );
        }
    }
}

/// Every filtered walk reaches the whole filtered set: for each implementation type the
/// catalog serves, the walk returns exactly the controls of that type the unfiltered
/// catalog holds, at every page size — no page is short and no walk stops early.
#[tokio::test]
async fn a_filtered_walk_reaches_every_control_the_filter_selects() {
    let emulator = Emulator::new();
    let published = catalog(&emulator).await;

    let mut types: BTreeSet<String> = BTreeSet::new();
    for control in published.values() {
        if let Some(implementation_type) = control["Implementation"]["Type"].as_str() {
            types.insert(implementation_type.to_string());
        }
    }
    assert!(
        types.len() >= 2,
        "case catalog: the catalog must publish more than one implementation type for a \
         type filter to narrow anything, got {types:?}"
    );

    for implementation_type in &types {
        let expected = by_arn(
            published
                .values()
                .filter(|control| {
                    control["Implementation"]["Type"].as_str() == Some(implementation_type.as_str())
                })
                .cloned()
                .collect(),
        );
        let body = type_filter(implementation_type);
        for page_size in PAGE_SIZES {
            let selected =
                by_arn(drain(&emulator, "/list-controls", "Controls", page_size, &body).await);
            assert_eq!(
                selected.len(),
                expected.len(),
                "case type {implementation_type} maxResults={page_size:?}: expected the \
                 {} control(s) the catalog publishes over this surface, got {}",
                expected.len(),
                selected.len()
            );
            assert_eq!(
                selected, expected,
                "case type {implementation_type} maxResults={page_size:?}: expected the \
                 catalog's own entries for this surface"
            );
        }
    }

    // The walk is not asserted vacuously over a handful of entries: the catalog's
    // Config-implemented surface alone publishes a control for every implementation the
    // table above records for it, and more besides.
    let config_rules = PUBLISHED_IMPLEMENTATIONS
        .iter()
        .filter(|(implementation_type, _)| *implementation_type == CONFIG_RULE)
        .count();
    let served = published
        .values()
        .filter(|control| control["Implementation"]["Type"].as_str() == Some(CONFIG_RULE))
        .count();
    assert!(
        served >= config_rules,
        "case type {CONFIG_RULE}: the catalog must publish at least the {config_rules} \
         recorded Config-implemented control(s), got {served}"
    );
}
