// SPDX-License-Identifier: CC0-1.0

//! The refusals the service's DATA endpoint words in its own way, pinned over a
//! SET of inputs rather than over the one request a recording happened to carry.
//!
//! The model hosts this service on three endpoints (`smithy_query(query="operation",
//! …)`, member `endpoint.hostPrefix`) — `api.`, the control plane that holds the
//! asset models and the assets, `monitor.`, the Monitor plane, and `data.`, the
//! time-series plane that reads and writes their values — and the data plane does
//! not word a refusal the way the other two do. The three rules below are the data
//! endpoint's, each driven over every operation of that endpoint it reaches:
//!
//! 1. an `(assetId, propertyId)` pair is resolved as ONE property lookup, so a
//!    pair that names no property — whichever half is the missing one — is the
//!    modelled 404 stating a PROPERTY that was not found and naming both ids,
//!    never a missing asset;
//! 2. a CSV file format a bulk import states must describe a usable row (an
//!    identifier, the data type, the timestamp and the value), and that check is
//!    about the REQUEST, so it is answered ahead of any precondition on the
//!    account's own storage;
//! 3. a constraint-violation report of this endpoint QUOTES the value it rejected
//!    and quotes a member's expression without the pair of anchors the model wraps
//!    it in — where the control plane's report states neither, which the rows here
//!    contrast against;
//! 4. an aggregate answer is bounded by the SAMPLES the store holds for the stream
//!    rather than by the width of the range the request names: a window is
//!    materialised only where a sample falls in it.
//!
//! Every expected string is written out BY HAND as an independent oracle: this
//! suite calls no message helper of the crate. Everything is driven over HTTP
//! through the same `MultiServiceRouter` the emulator boots with, so each
//! assertion is about the wire behaviour a client sees.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::asset_lifecycle::{call, error_code, router};
use crate::hierarchies_etags_and_refusals::{create_asset, create_model, error_message};
use crate::storage_configuration::put as put_storage;
use crate::support::now_seconds;

/// A well-formed `ID` UUID nothing in this suite is stored under.
const UNKNOWN_ID: &str = "cccccccc-dddd-4eee-8fff-aaaaaaaaaaaa";

/// A second one, so a request can be wrong in both halves of the pair at once.
const OTHER_UNKNOWN_ID: &str = "33333333-4444-4555-8666-777777777777";

/// The code this service answers a request that broke one of the model's own
/// rules with ("The request isn't valid …" —
/// `smithy_query(query="operation", operation="CreateBulkImportJob")`).
const INVALID_REQUEST: &str = "InvalidRequestException";

/// The code an absent resource is answered with.
const NOT_FOUND: &str = "ResourceNotFoundException";

/// The `@httpQuery`-bound window the aggregate reader requires, as the ISO-8601
/// `date-time` form a query parameter carries it in.
const WINDOW: &str = "startDate=2026-01-01T00:00:00Z&endDate=2026-01-02T00:00:00Z";

/// The sentence a CSV column list that describes no usable row is refused with,
/// written out by hand from the service's own wording (its grammatical defect
/// included). It carries no variable part.
const CSV_REFUSAL: &str = "Provided CSV file format contains must contain an identifier, \
                           data type, timestamp, and value";

/// The sentence a create that would ingest into time series outside a workspace
/// is refused with while neither storage tier is enabled, written out by hand
/// from the operation's own documentation. It is the refusal the CSV check runs
/// AHEAD of, so the rows below tell the two apart.
const TIER_REFUSAL: &str = "Before you create a bulk import job that ingests data into time \
                            series outside of a workspace, you must enable IoT SiteWise warm \
                            tier or IoT SiteWise cold tier.";

/// The `jobRoleArn` every create below names: an ARN of the form the modelled
/// `ARN` pattern admits.
const JOB_ROLE_ARN: &str = "arn:aws:iam::000000000000:role/SiteWiseBulkImport";

/// The sentence the data endpoint reports an unresolvable `(assetId, propertyId)`
/// pair with: the pair is one property lookup, so both ids are named and the
/// property is what was not found.
fn property_absence(asset_id: &str, property_id: &str) -> String {
    format!("Property not found (Property Id = {property_id}, Asset Id = {asset_id})")
}

/// One clause of a constraint report as the DATA endpoint states it: the value it
/// rejected, the member and the requirement.
fn data_clause(value: &str, member: &str, requirement: &str) -> String {
    format!("Value '{value}' at '{member}' failed to satisfy constraint: {requirement}")
}

/// The requirement an expression states, over the expression as the endpoint
/// quotes it.
fn must_match(expression: &str) -> String {
    format!("Member must satisfy regular expression pattern: {expression}")
}

/// The whole report: the envelope counts the clauses and joins them with `; `.
fn report(clauses: &[String]) -> String {
    match clauses {
        [one] => format!("1 validation error detected: {one}"),
        many => format!(
            "{} validation errors detected: {}",
            many.len(),
            many.join("; ")
        ),
    }
}

/// An asset of a model carrying one measurement, and the id of that materialised
/// property: a pair whose halves both resolve, so a row can be wrong in exactly
/// one of them.
async fn fixture(router: &MultiServiceRouter, name: &str) -> (String, String) {
    let model_id = create_model(
        router,
        json!({
            "assetModelName": format!("{name} model"),
            "assetModelProperties": [
                {"name": "Temperature", "dataType": "DOUBLE", "type": {"measurement": {}}}
            ]
        }),
    )
    .await;
    let asset_id = create_asset(
        router,
        json!({ "assetName": name, "assetModelId": model_id }),
    )
    .await;
    let (status, described) = call(router, "GET", &format!("/assets/{asset_id}"), None).await;
    assert_eq!(status, 200, "DescribeAsset {name}: got {status} {described}");
    let property_id = described["assetProperties"][0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("no materialised property in {described}"))
        .to_string();
    (asset_id, property_id)
}

/// Every single-read of the data endpoint that addresses a property by the pair,
/// as the URI each takes for one.
fn pair_reads(asset_id: &str, property_id: &str) -> Vec<(&'static str, String)> {
    vec![
        (
            "GetAssetPropertyValue",
            format!("/properties/latest?assetId={asset_id}&propertyId={property_id}"),
        ),
        (
            "GetAssetPropertyValueHistory",
            format!("/properties/history?assetId={asset_id}&propertyId={property_id}"),
        ),
        (
            "GetAssetPropertyAggregates",
            format!(
                "/properties/aggregates?assetId={asset_id}&propertyId={property_id}\
                 &aggregateTypes=COUNT&resolution=1m&{WINDOW}"
            ),
        ),
        (
            "GetInterpolatedAssetPropertyValues",
            format!(
                "/properties/interpolated?assetId={asset_id}&propertyId={property_id}\
                 &startTimeInSeconds=1000&endTimeInSeconds=1100&intervalInSeconds=10\
                 &quality=GOOD&type=LINEAR_INTERPOLATION"
            ),
        ),
    ]
}

/// Rule 1 over the SINGLE reads: the rows vary along which half of the pair fails
/// to resolve — neither, the asset alone, the property alone, and a property that
/// exists on ANOTHER asset — and the expected message is built from the row's own
/// ids, so no row can pass by matching a hard-coded one.
#[tokio::test]
async fn a_pair_that_names_no_property_is_one_property_lookup_failure() {
    let router = router();
    let (asset_id, property_id) = fixture(&router, "Turbine1").await;
    let (other_asset, other_property) = fixture(&router, "Turbine2").await;

    let pairs: Vec<(&str, String, String)> = vec![
        (
            "both halves absent",
            UNKNOWN_ID.to_string(),
            OTHER_UNKNOWN_ID.to_string(),
        ),
        (
            "an absent asset carrying a property id that exists elsewhere",
            UNKNOWN_ID.to_string(),
            property_id.clone(),
        ),
        (
            "an asset that exists and a property id nothing minted",
            asset_id.clone(),
            UNKNOWN_ID.to_string(),
        ),
        (
            "an asset that exists and another asset's property",
            asset_id.clone(),
            other_property.clone(),
        ),
        (
            "the other asset and this asset's property",
            other_asset.clone(),
            property_id.clone(),
        ),
    ];

    for (variation, asset, property) in &pairs {
        for (operation, uri) in pair_reads(asset, property) {
            let case = format!("{operation}, {variation}");
            let (status, answered) = call(&router, "GET", &uri, None).await;
            assert_eq!(
                status, 404,
                "case {case:?}: a pair naming no property is a 404, got {status} {answered}"
            );
            assert_eq!(
                error_code(&answered),
                NOT_FOUND,
                "case {case:?}: expected {NOT_FOUND}, got {answered}"
            );
            let expected = property_absence(asset, property);
            assert_eq!(
                error_message(&answered),
                expected,
                "case {case:?}: expected {expected:?}, got {:?}",
                error_message(&answered)
            );
        }
    }

    // The pair whose halves both resolve is ANSWERED by each of those reads, so
    // the rows above are about resolution and not about the operations.
    for (operation, uri) in pair_reads(&asset_id, &property_id) {
        let (status, answered) = call(&router, "GET", &uri, None).await;
        assert_eq!(
            status, 200,
            "{operation}: a pair that resolves must be answered, got {status} {answered}"
        );
    }
}

/// Rule 1 over the BATCH operations of the same endpoint, whose refusal is
/// reported per entry: the three batch reads and the batch write word the absence
/// exactly as the single reads do.
#[tokio::test]
async fn every_batch_entry_words_an_unresolvable_pair_the_same_way() {
    let router = router();
    let (asset_id, property_id) = fixture(&router, "Turbine1").await;

    // (case, the assetId, the propertyId) — the same variations the single reads
    // are driven over.
    let pairs: Vec<(&str, String, String)> = vec![
        (
            "both halves absent",
            UNKNOWN_ID.to_string(),
            OTHER_UNKNOWN_ID.to_string(),
        ),
        (
            "an asset that exists and a property id nothing minted",
            asset_id.clone(),
            UNKNOWN_ID.to_string(),
        ),
    ];

    for (variation, asset, property) in &pairs {
        // Each operation's entry for that pair, under the URI the model binds, and
        // where in the answer the refusal of one entry is reported: the batch
        // WRITE nests a refusal per timestamp inside its entry, the batch reads
        // report one per entry.
        let entries: Vec<(&str, &str, Value, bool)> = vec![
            (
                "BatchGetAssetPropertyValue",
                "/properties/batch/latest",
                json!({ "entryId": "e1", "assetId": asset, "propertyId": property }),
                false,
            ),
            (
                "BatchGetAssetPropertyValueHistory",
                "/properties/batch/history",
                json!({ "entryId": "e1", "assetId": asset, "propertyId": property }),
                false,
            ),
            (
                "BatchGetAssetPropertyAggregates",
                "/properties/batch/aggregates",
                json!({
                    "entryId": "e1",
                    "assetId": asset,
                    "propertyId": property,
                    "aggregateTypes": ["COUNT"],
                    "resolution": "1m",
                    "startDate": 1_767_225_600_i64,
                    "endDate": 1_767_312_000_i64,
                }),
                false,
            ),
            (
                "BatchPutAssetPropertyValue",
                "/properties",
                json!({
                    "entryId": "e1",
                    "assetId": asset,
                    "propertyId": property,
                    "propertyValues": [{
                        "value": { "doubleValue": 72.5 },
                        "timestamp": { "timeInSeconds": now_seconds(), "offsetInNanos": 0 },
                    }],
                }),
                true,
            ),
        ];
        for (operation, uri, entry, nested) in entries {
            let case = format!("{operation}, {variation}");
            let (status, answered) =
                call(&router, "POST", uri, Some(json!({ "entries": [entry] }))).await;
            assert_eq!(
                status, 200,
                "case {case:?}: a per-entry failure is reported in the body, \
                 got {status} {answered}"
            );
            let reported = answered["errorEntries"]
                .as_array()
                .and_then(|entries| entries.first().cloned())
                .unwrap_or_else(|| panic!("case {case:?}: no error entry in {answered}"));
            let reported = if nested {
                reported["errors"]
                    .as_array()
                    .and_then(|errors| errors.first().cloned())
                    .unwrap_or_else(|| panic!("case {case:?}: no error in {reported}"))
            } else {
                reported
            };
            assert_eq!(
                reported["errorCode"].as_str().unwrap_or(""),
                NOT_FOUND,
                "case {case:?}: expected {NOT_FOUND}, got {reported}"
            );
            let expected = property_absence(asset, property);
            assert_eq!(
                reported["errorMessage"].as_str().unwrap_or(""),
                expected,
                "case {case:?}: expected {expected:?}, got {reported}"
            );
        }
    }

    // The pair whose halves both resolve is accepted by the write, so the rows
    // above are about resolution: nothing refuses a well-formed entry.
    let (status, answered) = call(
        &router,
        "POST",
        "/properties",
        Some(json!({ "entries": [{
            "entryId": "accepted",
            "assetId": asset_id,
            "propertyId": property_id,
            "propertyValues": [{
                "value": { "doubleValue": 72.5 },
                "timestamp": { "timeInSeconds": now_seconds(), "offsetInNanos": 0 },
            }],
        }]})),
    )
    .await;
    assert_eq!(status, 200, "the accepted write: got {status} {answered}");
    assert_eq!(
        answered["errorEntries"].as_array().map(Vec::len),
        Some(0),
        "a pair that resolves must be accepted, got {answered}"
    );
}

/// `CreateBulkImportJob` with one file and the `jobConfiguration` given.
async fn create_job(
    router: &MultiServiceRouter,
    job_name: &str,
    file_format: Option<Value>,
    job_configuration: Option<Value>,
) -> (u16, Value) {
    let mut file = json!({ "bucket": "sitewise-import", "key": "turbines/2026/01.csv" });
    if let Some(format) = file_format {
        file["fileFormat"] = format;
    }
    let mut body = json!({
        "jobName": job_name,
        "jobRoleArn": JOB_ROLE_ARN,
        "files": [file],
        "errorReportLocation": { "bucket": "sitewise-errors", "prefix": "jobs/" },
    });
    if let Some(configuration) = job_configuration {
        body["jobConfiguration"] = configuration;
    }
    call(router, "POST", "/jobs", Some(body)).await
}

/// A `jobName` for `case`: the modelled `BulkImportJobName` pattern admits
/// letters, numbers, spaces and `._:/-`, so the case's own punctuation is folded
/// to a separator the pattern accepts and the name stays unique per row.
fn job_name(case: &str) -> String {
    let spelled: String = case
        .chars()
        .map(|character| {
            if character.is_alphanumeric() || character == ' ' {
                character
            } else {
                '-'
            }
        })
        .collect();
    format!("job {spelled}")
}

/// Column lists that describe NO usable row, each missing one part of a sample.
///
/// The catalog they are drawn from is the modelled `ColumnName` value set
/// (`smithy_query(query="shape", shape="ColumnName")` — ALIAS, ASSET_ID,
/// PROPERTY_ID, DATA_TYPE, TIMESTAMP_SECONDS, TIMESTAMP_NANO_OFFSET, QUALITY,
/// VALUE), written out here as the oracle; the rows differ along WHICH part the
/// list leaves out, including the half-stated asset-property identifier and the
/// two columns that only refine a sample.
fn unusable_column_lists() -> Vec<(&'static str, Value)> {
    vec![
        ("no column at all", json!([])),
        (
            "half of the asset-property identifier",
            json!(["ASSET_ID", "DATA_TYPE", "TIMESTAMP_SECONDS", "VALUE"]),
        ),
        (
            "no identifier",
            json!(["DATA_TYPE", "TIMESTAMP_SECONDS", "VALUE"]),
        ),
        (
            "no data type",
            json!(["ALIAS", "TIMESTAMP_SECONDS", "VALUE"]),
        ),
        ("no timestamp", json!(["ALIAS", "DATA_TYPE", "VALUE"])),
        (
            "an offset standing in for the timestamp",
            json!(["ALIAS", "DATA_TYPE", "TIMESTAMP_NANO_OFFSET", "VALUE"]),
        ),
        (
            "no value",
            json!(["ALIAS", "DATA_TYPE", "TIMESTAMP_SECONDS", "QUALITY"]),
        ),
    ]
}

/// Column lists that DO describe a usable row: the two identifier forms, at the
/// smallest list each admits and with every refining column added.
fn usable_column_lists() -> Vec<(&'static str, Value)> {
    vec![
        (
            "the alias form",
            json!(["ALIAS", "DATA_TYPE", "TIMESTAMP_SECONDS", "VALUE"]),
        ),
        (
            "the asset-property form",
            json!([
                "ASSET_ID",
                "PROPERTY_ID",
                "DATA_TYPE",
                "TIMESTAMP_SECONDS",
                "VALUE"
            ]),
        ),
        (
            "every column the model declares",
            json!([
                "ALIAS",
                "ASSET_ID",
                "PROPERTY_ID",
                "DATA_TYPE",
                "TIMESTAMP_SECONDS",
                "TIMESTAMP_NANO_OFFSET",
                "QUALITY",
                "VALUE"
            ]),
        ),
    ]
}

/// Rule 2: a CSV column list describing no usable row is the modelled 400 in the
/// service's own words, wherever the request states that format — on the job's
/// configuration or on one of its files — and the check is answered AHEAD of the
/// storage-tier precondition, which the same requests would otherwise draw.
#[tokio::test]
async fn a_csv_column_list_that_describes_no_row_is_refused_before_the_tier_precondition() {
    let router = router();

    // Neither tier is enabled here, so every create below would be refused for
    // the tier if its column list were not the thing refusing it first.
    for (variation, columns) in unusable_column_lists() {
        for (where_stated, file_format, job_configuration) in [
            (
                "on the job configuration",
                None,
                Some(json!({ "fileFormat": { "csv": { "columnNames": columns } } })),
            ),
            (
                "on the file",
                Some(json!({ "csv": { "columnNames": columns } })),
                None,
            ),
        ] {
            let case = format!("{variation}, {where_stated}");
            let (status, answered) =
                create_job(&router, &job_name(&case), file_format, job_configuration).await;
            assert_eq!(
                status, 400,
                "case {case:?}: an unusable column list is a 400, got {status} {answered}"
            );
            assert_eq!(
                error_code(&answered),
                INVALID_REQUEST,
                "case {case:?}: expected {INVALID_REQUEST}, got {answered}"
            );
            assert_eq!(
                error_message(&answered),
                CSV_REFUSAL,
                "case {case:?}: expected {CSV_REFUSAL:?}, got {:?}",
                error_message(&answered)
            );
        }
    }

    // A list that DOES describe a row gets past the column check and is refused
    // for the precondition it really breaks, which is what shows the order.
    for (variation, columns) in usable_column_lists() {
        let (status, answered) = create_job(
            &router,
            &job_name(variation),
            None,
            Some(json!({ "fileFormat": { "csv": { "columnNames": columns } } })),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {variation:?}: no tier is enabled, so this is a 400, got {status} {answered}"
        );
        assert_eq!(
            error_message(&answered),
            TIER_REFUSAL,
            "case {variation:?}: expected the tier precondition, got {:?}",
            error_message(&answered)
        );
    }

    // And with the warm tier enabled, a usable list is ACCEPTED: the column check
    // refuses lists, not CSV imports.
    let (status, stored) = put_storage(
        &router,
        // The `StorageType` and `WarmTierState` values the model declares
        // (`smithy_query(query="shape", shape="StorageType")`,
        // `shape="WarmTierState"`), written out here as the oracle.
        json!({ "storageType": "SITEWISE_DEFAULT_STORAGE", "warmTier": "ENABLED" }),
    )
    .await;
    assert_eq!(status, 200, "PutStorageConfiguration: got {status} {stored}");
    for (variation, columns) in usable_column_lists() {
        let (status, answered) = create_job(
            &router,
            &job_name(&format!("accepted {variation}")),
            None,
            Some(json!({ "fileFormat": { "csv": { "columnNames": columns } } })),
        )
        .await;
        assert_eq!(
            status, 202,
            "case {variation:?}: a usable column list must be accepted, got {status} {answered}"
        );
    }

    // A refused create started no job: the listing holds only the accepted ones.
    let (status, listed) = call(&router, "GET", "/jobs", None).await;
    assert_eq!(status, 200, "ListBulkImportJobs: got {status} {listed}");
    assert_eq!(
        listed["jobSummaries"].as_array().map(Vec::len),
        Some(usable_column_lists().len()),
        "only the accepted creates started a job, got {listed}"
    );
}

/// Rule 3: the data endpoint's constraint report quotes the value it rejected and
/// the member's expression WITHOUT the anchors the model wraps it in, while the
/// control plane's quotes neither.
///
/// The rows span the members and bindings this endpoint carries a
/// pattern-constrained value in (`resolution` and `nextToken` of
/// `GetAssetPropertyAggregates`, `smithy_query(query="operation",
/// operation="GetAssetPropertyAggregates")`), a value breaking its expression
/// alone and one breaking its expression and a `@length` bound at once, and one
/// control-plane row as the contrast.
#[tokio::test]
async fn the_data_endpoints_constraint_report_quotes_the_value_and_the_bare_expression() {
    let router = router();

    // The expressions the model declares, written out by hand as the oracle, with
    // the pair of anchors this endpoint drops already removed.
    let resolution = "1m|15m|1h|1d";
    let next_token = "[A-Za-z0-9+/=]+";

    // (case, the URI, the report the endpoint states)
    let rows: Vec<(&str, String, String)> = vec![
        (
            "a resolution outside its expression",
            format!(
                "/properties/aggregates?assetId={UNKNOWN_ID}&propertyId={OTHER_UNKNOWN_ID}\
                 &aggregateTypes=COUNT&resolution=2d&{WINDOW}"
            ),
            report(&[data_clause("2d", "resolution", &must_match(resolution))]),
        ),
        (
            "a second resolution outside it",
            format!(
                "/properties/aggregates?assetId={UNKNOWN_ID}&propertyId={OTHER_UNKNOWN_ID}\
                 &aggregateTypes=COUNT&resolution=7d&{WINDOW}"
            ),
            report(&[data_clause("7d", "resolution", &must_match(resolution))]),
        ),
        (
            "a resolution outside its expression and over its length",
            format!(
                "/properties/aggregates?assetId={UNKNOWN_ID}&propertyId={OTHER_UNKNOWN_ID}\
                 &aggregateTypes=COUNT&resolution=hour&{WINDOW}"
            ),
            report(&[
                data_clause("hour", "resolution", &must_match(resolution)),
                data_clause(
                    "hour",
                    "resolution",
                    "Member must have length less than or equal to 3",
                ),
            ]),
        ),
        (
            "a nextToken outside its expression",
            format!(
                "/properties/aggregates?assetId={UNKNOWN_ID}&propertyId={OTHER_UNKNOWN_ID}\
                 &aggregateTypes=COUNT&resolution=1m&nextToken=nottoken!&{WINDOW}"
            ),
            report(&[data_clause(
                "nottoken!",
                "nextToken",
                &must_match(next_token),
            )]),
        ),
    ];

    for (case, uri, expected) in &rows {
        let (status, answered) = call(&router, "GET", uri, None).await;
        assert_eq!(
            status, 400,
            "case {case:?}: a broken constraint is a 400, got {status} {answered}"
        );
        assert_eq!(
            error_code(&answered),
            INVALID_REQUEST,
            "case {case:?}: expected {INVALID_REQUEST}, got {answered}"
        );
        assert_eq!(
            error_message(&answered),
            expected,
            "case {case:?}: expected {expected:?}, got {:?}",
            error_message(&answered)
        );
    }

    // The CONTROL plane states the same class of refusal without the value and
    // with the expression as the model declares it, anchors and all
    // (`smithy_query(query="shape", shape="AssetModelVersionFilter")`).
    let (status, answered) = call(
        &router,
        "GET",
        &format!("/asset-models/{UNKNOWN_ID}?assetModelVersion=active"),
        None,
    )
    .await;
    assert_eq!(status, 400, "the control plane: got {status} {answered}");
    let expected = report(&[format!(
        "Value at 'assetModelVersion' failed to satisfy constraint: {}",
        must_match("^(LATEST|ACTIVE)$")
    )]);
    assert_eq!(
        error_message(&answered),
        expected,
        "the control plane: expected {expected:?}, got {:?}",
        error_message(&answered)
    );
}

/// Rule 4: the aggregate read's answer is bounded by the SAMPLES the store holds for
/// the stream, not by the width of the range the request names — a window is
/// materialised only where a sample of the filtered quality falls in it. The rows
/// span the shortest and the longest modelled `Resolution` over a range covering
/// every instant the ingest window admits, so the widest span a caller can ask for is
/// answered out of the three samples the stream holds.
#[tokio::test]
async fn an_aggregate_answer_is_bounded_by_the_stored_samples_not_by_the_range() {
    let router = router();
    let (asset_id, property_id) = fixture(&router, "Turbine1").await;

    // Three samples, each in its own minute, so the one-minute grid has exactly three
    // windows to report however wide the queried range is.
    let newest = now_seconds();
    let written = [newest - 120, newest - 60, newest];
    for (at, second) in written.iter().enumerate() {
        let (status, answered) = call(
            &router,
            "POST",
            "/properties",
            Some(json!({ "entries": [{
                "entryId": format!("e{at}"),
                "assetId": asset_id,
                "propertyId": property_id,
                "propertyValues": [{
                    "value": { "doubleValue": 70.0 + at as f64 },
                    "timestamp": { "timeInSeconds": second, "offsetInNanos": 0 },
                }],
            }]})),
        )
        .await;
        assert_eq!(status, 200, "ingest {at}: got {status} {answered}");
        assert_eq!(
            answered["errorEntries"].as_array().map(Vec::len),
            Some(0),
            "ingest {at}: every entry must be accepted, got {answered}"
        );
    }

    // A range covering every instant a sample may carry, at each end of the modelled
    // resolution table (`smithy_query(query="shape", shape="Resolution")` — 1m, 15m,
    // 1h, 1d), asked for with the documented maximum page size.
    for resolution in ["1m", "15m", "1h", "1d"] {
        let (status, answered) = call(
            &router,
            "GET",
            &format!(
                "/properties/aggregates?assetId={asset_id}&propertyId={property_id}\
                 &aggregateTypes=COUNT&resolution={resolution}&maxResults=2500\
                 &startDate=1970-01-01T00:00:00Z&endDate=2100-01-01T00:00:00Z"
            ),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {resolution:?}: the widest range is answered, got {status} {answered}"
        );
        let reported = answered["aggregatedValues"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert!(
            !reported.is_empty() && reported.len() <= written.len(),
            "case {resolution:?}: expected at most one window per stored sample \
             ({} of them), got {} — an answer sized by the range rather than by the \
             data: {answered}",
            written.len(),
            reported.len()
        );
        // And every window reported really summarises stored data: the counts add up
        // to the samples written, so nothing was invented to fill the span.
        let counted: u64 = reported
            .iter()
            .map(|value| value["value"]["count"].as_f64().unwrap_or_default() as u64)
            .sum();
        assert_eq!(
            counted,
            written.len() as u64,
            "case {resolution:?}: expected the windows to count the samples written, \
             got {counted} in {answered}"
        );
    }
}
