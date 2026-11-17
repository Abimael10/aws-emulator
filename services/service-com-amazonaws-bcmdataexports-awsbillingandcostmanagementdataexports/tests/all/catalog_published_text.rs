// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the VALUES the data-export table catalog publishes — the column
//! descriptions `GetTable` reports and the valid-value lists `ListTables` reports — over
//! the awsJson1_1 wire.
//!
//! The rule under test: every string and list in the catalog is response DATA a client
//! acts on, so each one is the AWS Data Exports table dictionary's PUBLISHED value for
//! that column or property, carried over as published rather than re-worded, and a value
//! belongs to the SCHEMA that publishes it rather than being generalised across schemas.
//! Concretely:
//!
//! 1. `GetTable` reports, for a column, the dictionary's published `Description` for that
//!    column of that table — not a shortened rewrite of it. Asserted over a TABLE of rows
//!    spanning every table in the catalog and the column families in it (identity, bill,
//!    line item, the aggregate dashboard dimensions, the carbon emissions period bounds,
//!    the recommendations columns, the FOCUS specification columns and the FOCUS 1.2
//!    additions), including columns the issue that prompted this suite did not cite. Each
//!    row's expected text is the one AWS was RECORDED publishing for that column, so the
//!    table is not a restatement of the catalog module.
//!    Also over EVERY column of EVERY table: a column carries its own description, so no
//!    two columns of one schema are described by the same string.
//! 2. A column NAME that two schemas both publish carries each schema's OWN published
//!    text: a later specification that re-words a column it carries forward does not
//!    re-word the earlier schema's copy, and the earlier wording does not leak into the
//!    later schema.
//! 3. `ListTables`'s `ValidValues` is what a client validates a `TableConfigurations`
//!    value against, so it agrees with what the service accepts: every value a property
//!    publishes is accepted, a property that publishes the wildcard (or publishes no list
//!    at all) accepts a value no list could enumerate, and an enumerated property refuses
//!    a value outside its list. Asserted over EVERY table and EVERY property the catalog
//!    publishes, not over the one property that showed the defect.
//! 4. A published value is carried over WHOLE: a description keeps the dictionary's
//!    trailing clauses and its examples, and a `Type` token keeps the parameters the
//!    dictionary publishes with it (the key and value types of a key-value column), so a
//!    client can tell what the column holds and what to cast it to. Asserted per column
//!    over a table spanning every schema and its column families, and as invariants over
//!    EVERY column of EVERY table — no description in the catalog's own "a map column
//!    containing …" voice, no upper-cased storage-engine token.
//!
//! Every literal expectation below cites its source: the AWS Data Exports table
//! dictionary, as recorded on the wire for `GetTable` / `ListTables`. The router, the
//! request signing and the error-code helper come from the `export_lifecycle` suite of
//! this same test target.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};

use service_com_amazonaws_bcmdataexports_awsbillingandcostmanagementdataexports::state::{
    PROPERTY_ANY_VALUE, PROPERTY_INCLUDE_RESOURCES, PROPERTY_INCLUDE_SPLIT_COST_ALLOCATION_DATA,
    PROPERTY_TRUE,
};

/// The table names, the reader of a reported schema and the published sentences the
/// version suite already writes down come from that suite rather than a second copy here.
use super::catalog_schema_versions::{
    schema_of, CARBON_EMISSIONS, COST_AND_USAGE_REPORT, DASHBOARD, FOCUS_1_0, FOCUS_1_0_PREVIEW,
    FOCUS_1_2, PUBLISHED_DESCRIPTIONS as VERSIONED_SENTENCES, RECOMMENDATIONS,
};
use super::export_lifecycle::{error_code, Client};

/// `(table, column, the Description the dictionary publishes for it)`.
///
/// SOURCE: the AWS Data Exports table dictionary as RECORDED on the wire for `GetTable` —
/// the text quoted in the issue's recording for the columns it cited, and the text the
/// replay of that recording confirms for the rest. Rows span every table of the catalog.
const PUBLISHED_DESCRIPTIONS: &[(&str, &str, &str)] = &[
    // The carbon emissions table: an account dimension, a period bound, and the shape of
    // its "report" wording.
    (
        "CARBON_EMISSIONS",
        "usage_account_id",
        "The account ID of the account associated with the carbon footprint value. For organizations, this can be either the management account or a member account.",
    ),
    (
        "CARBON_EMISSIONS",
        "usage_period_start",
        "The start date of the period that is covered by this report, in UTC. The format is YYYY-MM-DDTHH:mm:ssZ.",
    ),
    (
        "CARBON_EMISSIONS",
        "usage_period_end",
        "The end date of the period that is covered by this report, in UTC. The format is YYYY-MM-DDTHH:mm:ssZ.",
    ),
    (
        "CARBON_EMISSIONS",
        "payer_account_id",
        "The account ID of the paying account. For an organization in AWS Organizations, this is the account ID of the management account.",
    ),
    // The cost and usage table: the identity, bill, line item, pricing, product and
    // reservation families.
    (
        "COST_AND_USAGE_REPORT",
        "identity_line_item_id",
        "This field is generated for each line item and is unique in a given partition. This does not guarantee that the field will be unique across an entire delivery (that is, all partitions in an update) of the AWS CUR. The line item ID isn't consistent between different Cost and Usage Reports and can't be used to identify the same line item across different reports.",
    ),
    (
        "COST_AND_USAGE_REPORT",
        "identity_time_interval",
        "The time interval that this line item applies to, in the following format: YYYY-MM-DDTHH:mm:ssZ/YYYY-MM-DDTHH:mm:ssZ. The time interval is in UTC and can be either daily or hourly, depending on the granularity of the report.",
    ),
    (
        "COST_AND_USAGE_REPORT",
        "bill_bill_type",
        "The type of bill that this report covers.",
    ),
    (
        "COST_AND_USAGE_REPORT",
        "bill_billing_entity",
        "Helps you identify whether your invoices or transactions are for AWS Marketplace or for purchases of other AWS services.",
    ),
    (
        "COST_AND_USAGE_REPORT",
        "bill_invoice_id",
        "The ID associated with a specific line item. Until the report is final, the InvoiceId is blank.",
    ),
    (
        "COST_AND_USAGE_REPORT",
        "bill_payer_account_id",
        "The account ID of the paying account. For an organization in AWS Organizations, this is the account ID of the management account.",
    ),
    (
        "COST_AND_USAGE_REPORT",
        "line_item_normalization_factor",
        "As long as the instance has shared tenancy, AWS can apply all Regional Linux or Unix Amazon EC2 and Amazon RDS RI discounts to all instance sizes in an instance family and AWS Region. This also applies to RI discounts for member accounts in an organization. All new and existing Amazon EC2 and Amazon RDS size-flexible RIs are sized according to a normalization factor, based on the instance size.",
    ),
    (
        "COST_AND_USAGE_REPORT",
        "line_item_normalized_usage_amount",
        "The amount of usage that you incurred, in normalized units, for size-flexible RIs. The NormalizedUsageAmount is equal to UsageAmount multiplied by NormalizationFactor.",
    ),
    (
        "COST_AND_USAGE_REPORT",
        "line_item_availability_zone",
        "The Availability Zone that hosts this line item.",
    ),
    // The dashboard table: its aggregate dimensions, which the dictionary words as the
    // line item they aggregate.
    (
        "COST_AND_USAGE_DASHBOARD",
        "billing_period",
        "The start date of the billing period that is covered by this report, in UTC. The format is YYYY-MM-DDTHH:mm:ssZ.",
    ),
    (
        "COST_AND_USAGE_DASHBOARD",
        "usage_date",
        "The start date for the line item in UTC, inclusive. The format is YYYY-MM-DDTHH:mm:ssZ.",
    ),
    (
        "COST_AND_USAGE_DASHBOARD",
        "invoice_id",
        "The ID associated with a specific line item. Until the report is final, the InvoiceId is blank.",
    ),
    (
        "COST_AND_USAGE_DASHBOARD",
        "charge_category",
        "The category of charge covered by this line item possible values are Running_Usage and non_usage.",
    ),
    // The recommendations table: the resource it is for, and the cost it reports — which
    // the dictionary says is the CURRENT resource's, not the recommended one's.
    (
        "COST_OPTIMIZATION_RECOMMENDATIONS",
        "resource_arn",
        "The Amazon Resource Name (ARN) of the resource.",
    ),
    (
        "COST_OPTIMIZATION_RECOMMENDATIONS",
        "region",
        "The AWS Region of the resource.",
    ),
    (
        "COST_OPTIMIZATION_RECOMMENDATIONS",
        "estimated_monthly_cost_before_discount",
        "The estimated monthly cost of the current resource before discounts. For Reserved Instances and Savings Plans, it refers to the cost for eligible usage.",
    ),
    (
        "COST_OPTIMIZATION_RECOMMENDATIONS",
        "recommendation_id",
        "The ID for the recommendation.",
    ),
    // The FOCUS 1.0 specification columns, and an AWS `x_` extension.
    (
        "FOCUS_1_0_AWS",
        "AvailabilityZone",
        "A provider-assigned identifier for a physically separated and isolated area within a Region that provides high availability and fault tolerance.",
    ),
    (
        "FOCUS_1_0_AWS",
        "BillingCurrency",
        "Represents the currency that a charge was billed in.",
    ),
    (
        "FOCUS_1_0_AWS",
        "ChargeDescription",
        "Self-contained summary of the charge's purpose and price.",
    ),
    (
        "FOCUS_1_0_AWS",
        "CommitmentDiscountCategory",
        "Indicates whether the commitment-based discount identified in the CommitmentDiscountId column is based on usage quantity or cost (aka \"spend\").",
    ),
    (
        "FOCUS_1_0_AWS",
        "BilledCost",
        "A charge serving as the basis for invoicing, inclusive of all reduced rates and discounts while excluding the amortization of upfront charges (one-time or recurring).",
    ),
    (
        "FOCUS_1_0_AWS",
        "ProviderName",
        "The name of the entity that made the resources or services available for purchase.",
    ),
    // The preview table publishes the same FOCUS 1.0 text: it is the same specification.
    (
        "FOCUS_1_0_AWS_PREVIEW",
        "BillingCurrency",
        "Represents the currency that a charge was billed in.",
    ),
    // The FOCUS 1.2 columns, including the ones 1.2 re-words and the ones it adds.
    (
        "FOCUS_1_2_AWS",
        "ChargeCategory",
        "Represents the highest-level classification of a charge based on the nature of how it is billed.",
    ),
    (
        "FOCUS_1_2_AWS",
        "ChargeClass",
        "Indicates whether the row represents a correction to a previously invoiced billing period.",
    ),
    (
        "FOCUS_1_2_AWS",
        "CapacityReservationStatus",
        "Indicates whether the charge represents either the consumption of a capacity reservation or when a capacity reservation is unused.",
    ),
];

/// `(column, the FOCUS 1.0 text, the FOCUS 1.2 text)` for a column BOTH FOCUS schemas
/// publish and the 1.2 specification re-words (AWS Data Exports table dictionary).
const REVISED_BETWEEN_SCHEMAS: &[(&str, &str, &str)] = &[
    (
        "ChargeCategory",
        "Indicates whether the row represents an upfront or recurring fee, cost of usage that already occurred, an after-the-fact adjustment (e.g., credits), or taxes.",
        "Represents the highest-level classification of a charge based on the nature of how it is billed.",
    ),
    (
        "ChargeClass",
        "Indicates whether the row represents a regular charge or a correction to one or more previous charges, its primary use is for differentiating refunds from normal usage.",
        "Indicates whether the row represents a correction to a previously invoiced billing period.",
    ),
];

/// A column name that more than one table publishes, with the tables that publish it:
/// each one reports its own schema's text for it, so a description is not generalised
/// from one schema onto another.
const SHARED_COLUMN_NAMES: &[(&str, &[&str])] = &[
    (
        "region",
        &["COST_AND_USAGE_DASHBOARD", "COST_OPTIMIZATION_RECOMMENDATIONS"],
    ),
    (
        "last_refresh_timestamp",
        &["CARBON_EMISSIONS", "COST_OPTIMIZATION_RECOMMENDATIONS"],
    ),
    (
        "tags",
        &["COST_AND_USAGE_REPORT", "COST_OPTIMIZATION_RECOMMENDATIONS"],
    ),
];

/// A value no published list could enumerate, for a free-form property: used to check
/// that a property advertising the wildcard really does accept any value.
const UNENUMERABLE_VALUES: &[&str] = &[
    "{}",
    "{\"dimensions\":{\"key\":\"REGION\",\"values\":[\"us-east-1\",\"eu-west-1\"]}}",
    "arn:aws:billing::123456789012:billingview/primary",
];

/// A value no enumerated property publishes, used to check that it is refused.
const VALUE_OUTSIDE_EVERY_LIST: &str = "NOT_A_PUBLISHED_VALUE";

/// `GetTable` for `table`, requiring HTTP 200.
async fn get_table(client: &Client, table: &str) -> Value {
    client.ok("GetTable", &json!({ "TableName": table })).await
}

/// The reported schema as `name -> Description`.
async fn descriptions_of(client: &Client, table: &str) -> BTreeMap<String, String> {
    let body = get_table(client, table).await;
    body.get("Schema")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {table:?}: expected a Schema list, got {body}"))
        .iter()
        .map(|column| {
            let name = column
                .get("Name")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("case {table:?}: a column without a Name: {column}"))
                .to_string();
            let description = column
                .get("Description")
                .and_then(Value::as_str)
                .unwrap_or_else(|| {
                    panic!("case {table:?}: column {name} without a Description: {column}")
                })
                .to_string();
            (name, description)
        })
        .collect()
}

/// One table property as `ListTables` reports it.
#[derive(Debug, Clone)]
struct ListedProperty {
    name: String,
    valid_values: Vec<String>,
    default_value: Option<String>,
}

/// Every table `ListTables` advertises, as `name -> its reported properties`.
async fn listed_properties(client: &Client) -> BTreeMap<String, Vec<ListedProperty>> {
    client
        .ok("ListTables", &json!({}))
        .await
        .get("Tables")
        .and_then(Value::as_array)
        .expect("ListTables reports Tables")
        .iter()
        .map(|table| {
            let name = table
                .get("TableName")
                .and_then(Value::as_str)
                .expect("a listed table carries TableName")
                .to_string();
            let properties = table
                .get("TableProperties")
                .and_then(Value::as_array)
                .unwrap_or_else(|| panic!("case {name:?}: TableProperties must be a list"))
                .iter()
                .map(|property| ListedProperty {
                    name: property
                        .get("Name")
                        .and_then(Value::as_str)
                        .expect("a listed property carries Name")
                        .to_string(),
                    valid_values: property
                        .get("ValidValues")
                        .and_then(Value::as_array)
                        .unwrap_or_else(|| {
                            panic!("case {name:?}: ValidValues must be a list, got {property}")
                        })
                        .iter()
                        .map(|value| {
                            value
                                .as_str()
                                .expect("a valid value is a string")
                                .to_string()
                        })
                        .collect(),
                    default_value: property
                        .get("DefaultValue")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                })
                .collect();
            (name, properties)
        })
        .collect()
}

#[tokio::test]
async fn every_column_reports_the_description_its_dictionary_publishes() {
    let client = Client::new();

    let mut schemas: BTreeMap<&str, BTreeMap<String, String>> = BTreeMap::new();
    let mut tables_covered: BTreeSet<&str> = BTreeSet::new();
    for (table, column, published) in PUBLISHED_DESCRIPTIONS {
        if !schemas.contains_key(table) {
            schemas.insert(table, descriptions_of(&client, table).await);
        }
        tables_covered.insert(table);
        let case = format!("{table}.{column}");
        let reported = schemas[table]
            .get(*column)
            .unwrap_or_else(|| panic!("case {case:?}: the table must publish the column"));
        assert_eq!(
            reported, published,
            "case {case:?}: expected the dictionary's published description {published:?}, got {reported:?}"
        );
    }

    // The rows span the whole catalog, so a table slipping out of the catalog cannot pass
    // this unnoticed.
    let listed: BTreeSet<String> = listed_properties(&client).await.into_keys().collect();
    for table in &listed {
        assert!(
            tables_covered.contains(table.as_str()),
            "case {table:?}: every published table must be covered by a row of PUBLISHED_DESCRIPTIONS, got {tables_covered:?}"
        );
    }

    // And over every column of every table: each one carries ITS OWN description, so no
    // column of a schema is described by another column's string or by a generic one.
    for table in &listed {
        let described = descriptions_of(&client, table).await;
        let mut by_text: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for (name, description) in &described {
            by_text
                .entry(description.as_str())
                .or_default()
                .push(name.as_str());
        }
        for (description, columns) in &by_text {
            assert_eq!(
                columns.len(),
                1,
                "case {table:?}: the columns {columns:?} share one description {description:?}, so at least one is not described by the dictionary's own text for it"
            );
        }
    }
}

#[tokio::test]
async fn a_column_two_schemas_publish_carries_each_schemas_own_text() {
    let client = Client::new();

    let focus_1_0 = descriptions_of(&client, "FOCUS_1_0_AWS").await;
    let focus_1_0_preview = descriptions_of(&client, "FOCUS_1_0_AWS_PREVIEW").await;
    let focus_1_2 = descriptions_of(&client, "FOCUS_1_2_AWS").await;

    for (column, text_1_0, text_1_2) in REVISED_BETWEEN_SCHEMAS {
        for (schema, schema_name, expected) in [
            (&focus_1_0, "FOCUS_1_0_AWS", text_1_0),
            (&focus_1_0_preview, "FOCUS_1_0_AWS_PREVIEW", text_1_0),
            (&focus_1_2, "FOCUS_1_2_AWS", text_1_2),
        ] {
            let case = format!("{schema_name}.{column}");
            let reported = schema
                .get(*column)
                .unwrap_or_else(|| panic!("case {case:?}: the schema must publish the column"));
            assert_eq!(
                reported, expected,
                "case {case:?}: expected this schema's own published text {expected:?}, got {reported:?}"
            );
        }
        assert_ne!(
            text_1_0, text_1_2,
            "case {column:?}: the two specifications word this column differently, which is what this rule is about"
        );
    }

    // The same holds for a column NAME two unrelated tables publish: each reports its own
    // schema's text, which the dictionary words for that table's rows.
    for (column, tables) in SHARED_COLUMN_NAMES {
        let mut texts: Vec<(String, String)> = Vec::new();
        for table in *tables {
            let case = format!("{table}.{column}");
            let reported = descriptions_of(&client, table)
                .await
                .get(*column)
                .unwrap_or_else(|| panic!("case {case:?}: the table must publish the column"))
                .clone();
            assert!(
                !reported.is_empty(),
                "case {case:?}: a published column carries the dictionary's description"
            );
            texts.push(((*table).to_string(), reported));
        }
        assert_eq!(
            texts.len(),
            tables.len(),
            "case {column:?}: every table that publishes the column must report it, got {texts:?}"
        );
    }
}

#[tokio::test]
async fn a_property_accepts_exactly_what_its_valid_values_advertise() {
    let client = Client::new();

    let listed = listed_properties(&client).await;
    assert!(
        !listed.is_empty(),
        "case \"catalog\": the listing must advertise tables"
    );

    let mut enumerated = 0usize;
    let mut free_form = 0usize;
    for (table, properties) in &listed {
        for property in properties {
            let name = property.name.as_str();
            let case = format!("{table}/{name}");
            let wildcard = property
                .valid_values
                .iter()
                .any(|value| value == PROPERTY_ANY_VALUE);
            let unconstrained = wildcard || property.valid_values.is_empty();

            // A property that publishes a default publishes it as one of the values it
            // accepts, so a client that echoes the default back cannot be refused.
            if let Some(default) = &property.default_value {
                let body = client
                    .ok(
                        "GetTable",
                        &json!({ "TableName": table, "TableProperties": { name: default } }),
                    )
                    .await;
                assert_eq!(
                    body.get("TableProperties")
                        .and_then(|reported| reported.get(name))
                        .and_then(Value::as_str),
                    Some(default.as_str()),
                    "case {case:?}: the published default must be accepted and reported in force, got {body}"
                );
            }

            if unconstrained {
                free_form += 1;
                // A value no list could enumerate is accepted and reported in force.
                for value in UNENUMERABLE_VALUES {
                    let case = format!("{case}={value}");
                    let (status, body) = client
                        .call(
                            "GetTable",
                            &json!({ "TableName": table, "TableProperties": { name: value } }),
                        )
                        .await;
                    assert_eq!(
                        status, 200,
                        "case {case:?}: a property advertising {:?} accepts any value, got {status} with {body}",
                        property.valid_values
                    );
                    assert_eq!(
                        body.get("TableProperties")
                            .and_then(|reported| reported.get(name))
                            .and_then(Value::as_str),
                        Some(*value),
                        "case {case:?}: the reported properties must carry the requested value, got {body}"
                    );
                }
            } else {
                enumerated += 1;
                // Every value the list publishes is accepted …
                for value in &property.valid_values {
                    let case = format!("{case}={value}");
                    let body = client
                        .ok(
                            "GetTable",
                            &json!({ "TableName": table, "TableProperties": { name: value } }),
                        )
                        .await;
                    assert_eq!(
                        body.get("TableProperties")
                            .and_then(|reported| reported.get(name))
                            .and_then(Value::as_str),
                        Some(value.as_str()),
                        "case {case:?}: a published value must be accepted, got {body}"
                    );
                }
                // … and a value outside it is refused as a validation failure.
                let (status, body) = client
                    .call(
                        "GetTable",
                        &json!({
                            "TableName": table,
                            "TableProperties": { name: VALUE_OUTSIDE_EVERY_LIST },
                        }),
                    )
                    .await;
                assert_eq!(
                    (status, error_code(&body).as_str()),
                    (400, "ValidationException"),
                    "case {case:?}: a value outside the published list must be refused, got {body}"
                );
            }
        }
    }

    assert!(
        enumerated > 0 && free_form > 0,
        "case \"catalog\": the catalog publishes both enumerated ({enumerated}) and free-form ({free_form}) properties, so both branches above are exercised"
    );
}

// ---------------------------------------------------------------------------
// Rule 4: a published value is carried over WHOLE — the sentence with its trailing
// clauses and examples, and the `Type` token with the parameters the dictionary publishes
// with it.
// ---------------------------------------------------------------------------

/// `(table, column, the Description the dictionary publishes for it)`, for the shapes of
/// published value rule 4 is about: a sentence with the dictionary's trailing clause, a
/// sentence with its example, a two-paragraph entry and a plain one.
///
/// SOURCE: the AWS Data Exports table dictionary as RECORDED on the wire for `GetTable`.
/// Rows span every table of the catalog and the column families of each, and include
/// columns the issue that prompted them did not cite (the carbon emissions period bound,
/// the cost and usage start date, the FOCUS billing-period bound). A sentence the version
/// suite already writes down is asserted from ITS table ([`VERSIONED_SENTENCES`]) rather
/// than repeated here.
const PUBLISHED_SENTENCES: &[(&str, &str, &str)] = &[
    // The carbon emissions table: the time the value behind the row was generated, the
    // model that generated it, a period bound the issue did not cite, and the emissions
    // measures. (The Region code and the location are asserted from the version suite's
    // table.)
    (
        CARBON_EMISSIONS,
        "last_refresh_timestamp",
        "The time when the carbon footprint value was last generated, in UTC. The format is YYYY-MM-DDTHH:mm:ssZ.",
    ),
    (
        CARBON_EMISSIONS,
        "model_version",
        "The version of the model that was used to generate the carbon footprint value.",
    ),
    (
        CARBON_EMISSIONS,
        "usage_period_start",
        "The start date of the period that is covered by this report, in UTC. The format is YYYY-MM-DDTHH:mm:ssZ.",
    ),
    (
        CARBON_EMISSIONS,
        "total_lbm_emissions_value",
        "The total estimated location-based method (LBM) emissions value associated with the account, including Scope 1, Scope 2, and Scope 3.",
    ),
    (
        CARBON_EMISSIONS,
        "total_lbm_emissions_unit",
        "The unit of measurement for the total estimated location-based method (LBM) emissions value. The unit we currently support is metric tons of carbon dioxide-equivalent (MTCO2e).",
    ),
    (
        CARBON_EMISSIONS,
        "total_scope_3_mbm_emissions_value",
        "The total estimated Scope 3 market-based method (MBM) emissions value associated with the account.",
    ),
    // The cost and usage table: the paying account and the using account, each with the
    // dictionary's own clause on what the column is for …
    (
        COST_AND_USAGE_REPORT,
        "bill_payer_account_name",
        "The account name of the paying account. For an organization in AWS Organizations, this is the name of the management account.",
    ),
    (
        COST_AND_USAGE_REPORT,
        "line_item_usage_account_id",
        "The account ID of the account that used this line item. For organizations, this can be either the management account or a member account. You can use this field to track costs or usage by account.",
    ),
    (
        COST_AND_USAGE_REPORT,
        "line_item_usage_account_name",
        "The name of the account that used this line item. For organizations, this can be either the management account or a member account. You can use this field to track costs or usage by account.",
    ),
    // … the line item's own bounds, the start one a row the issue did not cite (the
    // unblended rate's two paragraphs are asserted from the version suite's table) …
    (
        COST_AND_USAGE_REPORT,
        "line_item_usage_start_date",
        "The start date and time for the line item in UTC, inclusive. The format is YYYY-MM-DDTHH:mm:ssZ.",
    ),
    (
        COST_AND_USAGE_REPORT,
        "line_item_usage_end_date",
        "The end date and time for the line item in UTC, exclusive. The format is YYYY-MM-DDTHH:mm:ssZ.",
    ),
    // … the service abbreviation, the public On-Demand pair with the clause that says which
    // rate is used, and the key-value columns as the pairs they carry.
    (
        COST_AND_USAGE_REPORT,
        "product_servicecode",
        "This identifies the specific AWS service to the customer as a unique short abbreviation.",
    ),
    (
        COST_AND_USAGE_REPORT,
        "pricing_public_on_demand_cost",
        "The total cost for the line item based on public On-Demand Instance rates. If you have SKUs with multiple On-Demand public costs, the maximum rate is used.",
    ),
    (
        COST_AND_USAGE_REPORT,
        "pricing_public_on_demand_rate",
        "The public On-Demand Instance rate in this billing period for the specific line item of usage. If you have SKUs with multiple On-Demand public rates, the maximum rate is used.",
    ),
    (
        COST_AND_USAGE_REPORT,
        "cost_category",
        "Key-value pairs of the cost categories and their values for a given line item.",
    ),
    (
        COST_AND_USAGE_REPORT,
        "discount",
        "Key-value pairs for any discounts you are receiving.",
    ),
    (
        COST_AND_USAGE_REPORT,
        "product",
        "Key-value pairs of the product attributes and their values for a given line item.",
    ),
    (
        COST_AND_USAGE_REPORT,
        "resource_tags",
        "Key-value pairs of the AWS generated and user-defined tags and their values for a given line item.",
    ),
    (
        COST_AND_USAGE_REPORT,
        "tags",
        "Key-value pairs of the user-defined tags and their values for a given line item.",
    ),
    // … the Region a resource resides in, published with the dictionary's whole
    // Region/Availability-Zone explanation, and the reservation family in the
    // dictionary's own abbreviation for a Reserved Instance.
    (
        COST_AND_USAGE_REPORT,
        "product_region_code",
        "A Region is a physical location around the world where data centers are clustered. AWS calls each group of logical data centers an Availability Zone (AZ). Each AWS Region consists of multiple, isolated, and physically separate AZs within a geographic area. The Region code attribute specifies where the AWS service is available. For example, us-east-1.",
    ),
    (
        COST_AND_USAGE_REPORT,
        "reservation_unused_quantity",
        "The number of RI hours that you didn't use during this billing period.",
    ),
    (
        COST_AND_USAGE_REPORT,
        "reservation_net_effective_cost",
        "The after-discount effective cost for the RI.",
    ),
    (
        COST_AND_USAGE_REPORT,
        "savings_plan_used_commitment",
        "The total dollar amount of the Savings Plan commitment used (SavingsPlanRate multiplied by usage).",
    ),
    // The dashboard view: its own account dimensions, the service abbreviation, the place
    // the usage happened, and the service name with the clause that says what to use the
    // column for.
    (
        DASHBOARD,
        "payer_account_name",
        "The account name of the paying account. For an organization in AWS Organizations, this is the name of the management account.",
    ),
    (
        DASHBOARD,
        "linked_account_id",
        "The account ID of the account that used this line item. For organizations, this can be either the management account or a member account. You can use this field to track costs or usage by account.",
    ),
    (
        DASHBOARD,
        "linked_account_name",
        "The account name of the account that used this line item. For organizations, this can be either the management account or a member account. You can use this field to track costs or usage by account.",
    ),
    (
        DASHBOARD,
        "service",
        "This identifies the specific AWS service to the customer as a unique short abbreviation.",
    ),
    (
        DASHBOARD,
        "region",
        "The geographical area that hosts your AWS services.",
    ),
    (
        DASHBOARD,
        "availability_zone",
        "The Availability Zone that hosts this line item.",
    ),
    (
        DASHBOARD,
        "product_name",
        "The full name of the AWS service. Use this column to filter AWS usage by AWS service.",
    ),
    // A measure the view RENAMES is published in the view's own column names.
    (
        DASHBOARD,
        "unblended_cost",
        "The unblended rate multiplied by the usage quantity.",
    ),
    // The recommendations table: the resource the recommendation is for and the one it
    // recommends, the action it asks for, the savings percentages — which word the discount
    // clause before the lookback clause — the lookback period, the refresh time and the
    // resource's tags.
    (
        RECOMMENDATIONS,
        "current_resource_type",
        "The type of resource.",
    ),
    (
        RECOMMENDATIONS,
        "recommended_resource_type",
        "The resource type of the recommendation.",
    ),
    (
        RECOMMENDATIONS,
        "current_resource_summary",
        "Describes the current resource.",
    ),
    (
        RECOMMENDATIONS,
        "recommended_resource_summary",
        "Describes the recommended resource.",
    ),
    (
        RECOMMENDATIONS,
        "current_resource_details",
        "The details for the resource.",
    ),
    (
        RECOMMENDATIONS,
        "recommended_resource_details",
        "The details about the recommended resource.",
    ),
    (
        RECOMMENDATIONS,
        "action_type",
        "The type of action you can take by adopting the recommendation.",
    ),
    (
        RECOMMENDATIONS,
        "estimated_savings_percentage_before_discount",
        "The estimated savings percentage before discounts, relative to the total cost over the cost calculation lookback period.",
    ),
    (
        RECOMMENDATIONS,
        "estimated_savings_percentage_after_discount",
        "The estimated savings percentage after discounts, relative to the total cost over the cost calculation lookback period.",
    ),
    (
        RECOMMENDATIONS,
        "recommendation_lookback_period_in_days",
        "The lookback period in days, used to calculate the savings.",
    ),
    (
        RECOMMENDATIONS,
        "last_refresh_timestamp",
        "The time when the recommendation was last generated, in UTC. The format is YYYY-MM-DDTHH:mm:ssZ.",
    ),
    (
        RECOMMENDATIONS,
        "tags",
        "A list of tags associated with the resource for which the recommendation exists.",
    ),
    // The FOCUS 1.0 specification's columns and the AWS extensions published beside them,
    // which carry the cost and usage dictionary's text for the line-item column they
    // expose — examples included.
    (
        FOCUS_1_0,
        "ChargePeriodStart",
        "The inclusive start date and time within a charge period.",
    ),
    (
        FOCUS_1_0,
        "BillingPeriodStart",
        "The inclusive start date and time of a billing period.",
    ),
    (
        FOCUS_1_0,
        "ListCost",
        "Cost calculated by multiplying List Unit Price and the corresponding Pricing Quantity.",
    ),
    (
        FOCUS_1_0,
        "ResourceType",
        "The kind of resource the charge applies to.",
    ),
    (
        FOCUS_1_0,
        "ServiceName",
        "An offering that can be purchased from a provider (e.g., cloud virtual machine, SaaS database, professional services from a systems integrator).",
    ),
    (
        FOCUS_1_0,
        "Tags",
        "The set of tags assigned to tag sources that account for potential provider-defined or user-defined tag evaluations.",
    ),
    (
        FOCUS_1_0,
        "x_CostCategories",
        "Key-value pairs of the cost categories and their values for a given line item.",
    ),
    (
        FOCUS_1_0,
        "x_Discounts",
        "Key-value pairs for any discounts you are receiving.",
    ),
    (
        FOCUS_1_0,
        "x_Operation",
        "The specific AWS operation covered by this line item. This describes the specific usage of the line item. For example, a value of RunInstances indicates the operation of an Amazon EC2 instance.",
    ),
    (
        FOCUS_1_0,
        "x_ServiceCode",
        "This identifies the specific AWS service to the customer as a unique short abbreviation.",
    ),
    (
        FOCUS_1_0,
        "x_UsageType",
        "The usage details of the line item. For example, USW2-BoxUsage:m2.2xlarge describes an M2 High Memory Double Extra Large instance in the US West (Oregon) Region.",
    ),
    // The preview table publishes the same specification, so the same text.
    (
        FOCUS_1_0_PREVIEW,
        "x_UsageType",
        "The usage details of the line item. For example, USW2-BoxUsage:m2.2xlarge describes an M2 High Memory Double Extra Large instance in the US West (Oregon) Region.",
    ),
    // The FOCUS 1.2 specification's own text: the metered-SKU family, the invoice
    // identifier, the pricing-currency prices and the SKU columns. (The commitment-discount
    // quantity and unit are asserted from the version suite's table.)
    (
        FOCUS_1_2,
        "ConsumedQuantity",
        "The volume of a metered SKU associated with a resource or service used, based on the Consumed Unit.",
    ),
    (
        FOCUS_1_2,
        "ConsumedUnit",
        "The provider-specified measurement unit indicating how a provider measures usage of a metered SKU associated with a resource or service.",
    ),
    (
        FOCUS_1_2,
        "InvoiceId",
        "The provider-assigned identifier for an invoice encapsulating some or all charges in the corresponding billing period for a given billing account.",
    ),
    (
        FOCUS_1_2,
        "PricingCurrency",
        "Represents the currency that a charge was priced in.",
    ),
    (
        FOCUS_1_2,
        "PricingCurrencyEffectiveCost",
        "The effective cost of a charge, denominated in the pricing currency.",
    ),
    (
        FOCUS_1_2,
        "SkuId",
        "The identifier assigned to a SKU by the provider.",
    ),
    (
        FOCUS_1_2,
        "SkuPriceId",
        "The identifier assigned to a SKU Price by the provider.",
    ),
    (
        FOCUS_1_2,
        "SkuPriceDetails",
        "Key-value pairs of the provider-specified details that define the SKU Price used to calculate the charge.",
    ),
    (
        FOCUS_1_2,
        "SubAccountType",
        "The provider-assigned name that identifies the type of a sub account.",
    ),
];

/// The emissions the carbon dictionary publishes a measure for, with the words its text
/// names that measure by (AWS Data Exports table dictionary): a value column names the
/// method or scope it totals, and its unit column names the same one.
const EMISSIONS_MEASURE_WORDS: &[(&str, &[&str])] = &[
    ("total_lbm_emissions", &["location-based method (LBM)"]),
    ("total_mbm_emissions", &["market-based method (MBM)"]),
    ("total_scope_1_emissions", &["Scope 1"]),
    (
        "total_scope_2_lbm_emissions",
        &["Scope 2", "location-based method (LBM)"],
    ),
    (
        "total_scope_2_mbm_emissions",
        &["Scope 2", "market-based method (MBM)"],
    ),
    (
        "total_scope_3_lbm_emissions",
        &["Scope 3", "location-based method (LBM)"],
    ),
    (
        "total_scope_3_mbm_emissions",
        &["Scope 3", "market-based method (MBM)"],
    ),
];

/// The unit the carbon dictionary says it supports, named by every unit column it
/// publishes (AWS Data Exports table dictionary).
const SUPPORTED_EMISSIONS_UNIT: &str = "metric tons of carbon dioxide-equivalent (MTCO2e)";

/// Wording that belongs to a description OF the catalog rather than to the dictionary's
/// published text: a sentence about how the column is STORED ("a map column containing …")
/// in place of what the column holds.
const SELF_DESCRIBING_WORDING: &[&str] = &["A map column containing", "a map column containing"];

/// How the dictionary publishes a key-value column: as the PAIRS the column carries.
const KEY_VALUE_WORDING: &str = "Key-value pairs";

/// `(table, column, the words the dictionary publishes that column's tags with)` for the
/// key-value columns whose published text words the TAGS the column assigns rather than
/// the pairs it carries: the FOCUS specification's tag column and the recommendations
/// dictionary's (AWS Data Exports table dictionary). Every other key-value column of the
/// catalog is published as the pairs it carries.
const TAG_WORDED_COLUMNS: &[(&str, &str, &str)] = &[
    (FOCUS_1_0, "Tags", "tags assigned to"),
    (FOCUS_1_0_PREVIEW, "Tags", "tags assigned to"),
    (FOCUS_1_2, "Tags", "tags assigned to"),
    (RECOMMENDATIONS, "tags", "tags associated with"),
];

/// The `Type` token a schema publishes for a key-value column whose key and value types it
/// names (AWS Data Exports table dictionary, recommendations table).
const PARAMETERISED_MAP_TOKEN: &str = "Map<String, String>";

/// The `Type` token the recommendations dictionary counts a whole number of days with.
const WHOLE_NUMBER_TOKEN: &str = "Integer";

/// The name part that marks a cost and usage column as the NET of what it measures.
const NET_COLUMN_MARKER: &str = "_net_";

/// How the cost and usage dictionary publishes a net column: as the AFTER-DISCOUNT amount
/// of what it nets ("The actual after-discount cost that you're paying for the line item",
/// "The after-discount cost of the recurring usage fee" — AWS Data Exports table
/// dictionary).
const AFTER_DISCOUNT_WORDING: &str = "after-discount";

/// The re-wording a net column is NOT published with: a trailing clause about discounts
/// having been applied, in place of the dictionary's after-discount form.
const DISCOUNTS_APPLIED_WORDING: &str = "with discounts applied";

/// Every table `ListTables` advertises.
async fn published_table_names(client: &Client) -> Vec<String> {
    listed_properties(client).await.into_keys().collect()
}

/// RULE: `GetTable` reports, for a column, the dictionary's published sentence for that
/// column of that table — whole, with its trailing clauses and its examples.
#[tokio::test]
async fn every_published_column_carries_the_dictionarys_own_sentence() {
    let client = Client::new();

    let mut schemas: BTreeMap<&str, BTreeMap<String, String>> = BTreeMap::new();
    let mut covered: BTreeSet<&str> = BTreeSet::new();
    for (table, column, published) in PUBLISHED_SENTENCES.iter().chain(VERSIONED_SENTENCES) {
        if !schemas.contains_key(table) {
            schemas.insert(table, descriptions_of(&client, table).await);
        }
        covered.insert(table);
        let case = format!("{table}.{column}");
        let reported = schemas[table]
            .get(*column)
            .unwrap_or_else(|| panic!("case {case:?}: the table must publish the column"));
        assert_eq!(
            reported, published,
            "case {case:?}: expected the dictionary's published sentence {published:?}, got {reported:?}"
        );
    }

    // The rows span the whole catalog, so no published table is left without a sentence of
    // its own under test.
    for table in published_table_names(&client).await {
        assert!(
            covered.contains(table.as_str()),
            "case {table:?}: every published table must be covered by a row, got {covered:?}"
        );
    }
}

/// RULE, over EVERY column of EVERY table rather than a list written here: a published
/// description states what the column HOLDS, in the dictionary's words, never how the
/// catalog stores it — so a key-value column is published as the pairs it carries (or as
/// the tags it assigns).
#[tokio::test]
async fn no_column_is_described_by_how_the_catalog_stores_it() {
    let client = Client::new();
    let mut key_value_columns = 0usize;

    for table in published_table_names(&client).await {
        for (name, column) in schema_of(&client, &table).await {
            let case = format!("{table}.{name}");
            for wording in SELF_DESCRIBING_WORDING {
                assert!(
                    !column.description.contains(wording),
                    "case {case:?}: expected the dictionary's text for the column, got the catalog's own {wording:?} wording: {:?}",
                    column.description
                );
            }
            if column.data_type.starts_with("Map") {
                key_value_columns += 1;
                let expected = TAG_WORDED_COLUMNS
                    .iter()
                    .find(|(tagged_table, tagged, _)| *tagged_table == table && *tagged == name)
                    .map_or(KEY_VALUE_WORDING, |(_, _, wording)| *wording);
                assert!(
                    column.description.contains(expected),
                    "case {case:?}: a key-value column is published naming {expected:?}, got {:?}",
                    column.description
                );
            }
        }
    }

    assert!(
        key_value_columns > 0,
        "case \"catalog\": the catalog publishes key-value columns, so the branch above runs"
    );
}

/// RULE: a `Type` token is the dictionary's own, carried over with the parameters it
/// publishes — which for a key-value column of the recommendations schema NAME the key and
/// the value type, and for a count of days is the whole-number token rather than the one
/// that schema measures money with.
#[tokio::test]
async fn a_type_token_carries_everything_the_dictionary_publishes_with_it() {
    let client = Client::new();

    let recommendations = schema_of(&client, RECOMMENDATIONS).await;
    let tags = &recommendations["tags"];
    assert_eq!(
        tags.data_type, PARAMETERISED_MAP_TOKEN,
        "case \"{RECOMMENDATIONS}.tags\": expected the parameterised map token {PARAMETERISED_MAP_TOKEN:?}, got {:?}",
        tags.data_type
    );
    // A client builds a cast out of the token, so both parameters have to be in it.
    let parameters = tags
        .data_type
        .strip_prefix("Map<")
        .and_then(|rest| rest.strip_suffix('>'))
        .unwrap_or_else(|| {
            panic!(
                "case \"{RECOMMENDATIONS}.tags\": expected a parameterised map, got {:?}",
                tags.data_type
            )
        });
    assert_eq!(
        parameters.split(", ").count(),
        2,
        "case \"{RECOMMENDATIONS}.tags\": expected the key and the value type, got {parameters:?}"
    );

    let lookback = &recommendations["recommendation_lookback_period_in_days"];
    assert_eq!(
        lookback.data_type, WHOLE_NUMBER_TOKEN,
        "case \"{RECOMMENDATIONS}.recommendation_lookback_period_in_days\": expected the whole-number token {WHOLE_NUMBER_TOKEN:?}, got {:?}",
        lookback.data_type
    );
    assert_ne!(
        lookback.data_type, recommendations["estimated_monthly_cost_before_discount"].data_type,
        "case \"{RECOMMENDATIONS}\": a count of days and a cost are not the same kind of data"
    );

    // And over every column of every table: a token is published in the dictionary's
    // mixed-case words, never in an upper-cased storage-engine spelling.
    for table in published_table_names(&client).await {
        for (name, column) in schema_of(&client, &table).await {
            let case = format!("{table}.{name}");
            assert_ne!(
                column.data_type,
                column.data_type.to_ascii_uppercase(),
                "case {case:?}: expected the dictionary's token, got an upper-cased spelling {:?}",
                column.data_type
            );
        }
    }
}

/// RULE, applied to the family the dictionary publishes as value-and-unit pairs: each
/// measure's value column names the method or scope it totals, and its unit column names
/// the same one AND the unit of measurement the dictionary says it supports.
#[tokio::test]
async fn an_emissions_measure_names_its_method_and_its_supported_unit() {
    let client = Client::new();
    let schema = descriptions_of(&client, CARBON_EMISSIONS).await;

    for (measure, words) in EMISSIONS_MEASURE_WORDS {
        for suffix in ["_value", "_unit"] {
            let name = format!("{measure}{suffix}");
            let case = format!("{CARBON_EMISSIONS}.{name}");
            let description = schema
                .get(&name)
                .unwrap_or_else(|| panic!("case {case:?}: the table must publish the column"));
            for word in *words {
                assert!(
                    description.contains(word),
                    "case {case:?}: expected the dictionary's own {word:?} in the text, got {description:?}"
                );
            }
            let names_the_unit = description.contains(SUPPORTED_EMISSIONS_UNIT);
            assert_eq!(
                names_the_unit,
                suffix == "_unit",
                "case {case:?}: only a unit column names the supported unit, got {description:?}"
            );
        }
    }
}

/// RULE, applied to the family the dictionary publishes as the NET of another measure —
/// over EVERY such column of the cost and usage schema, at every configuration of the
/// column-selecting properties, rather than over the ones a report cited: a net column is
/// published as the AFTER-DISCOUNT amount of what it nets, in the dictionary's own form,
/// never as the gross sentence with a discount clause appended.
#[tokio::test]
async fn a_net_column_is_published_as_the_after_discount_amount() {
    let client = Client::new();
    let mut net_columns = 0usize;

    for properties in [
        None,
        Some(json!({ PROPERTY_INCLUDE_SPLIT_COST_ALLOCATION_DATA: PROPERTY_TRUE })),
        Some(json!({ PROPERTY_INCLUDE_RESOURCES: PROPERTY_TRUE })),
    ] {
        let request = match &properties {
            None => json!({ "TableName": COST_AND_USAGE_REPORT }),
            Some(properties) => {
                json!({ "TableName": COST_AND_USAGE_REPORT, "TableProperties": properties })
            }
        };
        let body = client.ok("GetTable", &request).await;
        let columns = body
            .get("Schema")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("case {properties:?}: expected a Schema list, got {body}"));
        for column in columns {
            let name = column
                .get("Name")
                .and_then(Value::as_str)
                .expect("a column carries a Name");
            if !name.contains(NET_COLUMN_MARKER) {
                continue;
            }
            net_columns += 1;
            let description = column
                .get("Description")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("case {name:?}: a column carries a Description"));
            let case = format!("{COST_AND_USAGE_REPORT}.{name}");
            assert!(
                description.contains(AFTER_DISCOUNT_WORDING),
                "case {case:?}: a net column is published as the {AFTER_DISCOUNT_WORDING} amount of what it nets, got {description:?}"
            );
            assert!(
                !description.contains(DISCOUNTS_APPLIED_WORDING),
                "case {case:?}: expected the dictionary's after-discount form, got the {DISCOUNTS_APPLIED_WORDING:?} re-wording: {description:?}"
            );
        }
    }

    assert!(
        net_columns > 10,
        "case \"net columns\": the schema publishes a net column per measure it nets, got {net_columns} across the configurations"
    );
}
