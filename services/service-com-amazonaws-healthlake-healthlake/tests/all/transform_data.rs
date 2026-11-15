// SPDX-License-Identifier: CC0-1.0

//! End-to-end behaviour of the data-transformation endpoint's SYNCHRONOUS transform:
//! `POST /transform-data`, which converts the clinical data a request carries into a
//! FHIR R4 Bundle with a stored data transformation profile's mapping.
//!
//! Every call goes through [`MultiServiceRouter::dispatch`] — the same entry point
//! `core/router` serves — as an ordinary signed request, through the shared harness
//! in `fhir_rest_and_validate_source`: by PATH with no `X-Amz-Target`, which is how a
//! client addresses this surface, and once by the RPC target an SDK built from the
//! full model sends instead. The profile each transform applies is created over the
//! awsJson1_0 wire in the same test, so the mapping under test is one the service
//! itself stored.
//!
//! The rules under test, each asserted over a SET of inputs rather than one recorded
//! request:
//!
//! 1. a transform applies the named profile's stored mapping to the submitted input:
//!    each data row of a submitted CSV file becomes one resource per resource mapping
//!    the profile declares over that file's table, carrying the elements its rules
//!    name — for any row count, any column set, any target resource type, and
//!    whatever the mapping and the file spell their names like, including a mapping
//!    this service INFERRED for the profile itself from a sample data file;
//! 2. the version it applies is the one a read that names none resolves to, so a
//!    published version — not a DRAFT written after it — is what a transform uses;
//! 3. a C-CDA transform is the same rule over document sections: each section a rule
//!    names becomes one resource of the type that rule names;
//! 4. the drift report states what the mapping and the submitted input disagree
//!    about, and is answered only to a caller that asked for it;
//! 5. provenance records the profile each produced resource came from;
//! 6. a `ProfileId` no profile is held for is refused with the modelled
//!    `ResourceNotFoundException` at HTTP 404, stating that profile's sentence, and a
//!    request outside a rule the model declares with the modelled
//!    `ValidationException` at HTTP 400 — neither of which produces a Bundle.

use serde_json::{json, Map, Value};
use service_com_amazonaws_healthlake_healthlake::state::{
    profile_not_found_message, MAX_DRIFT_FINDINGS, MAX_TRANSFORM_INPUT_FILES,
};

use crate::fhir_datastore_lifecycle::{error_code, error_message};
use crate::fhir_rest_and_validate_source::HealthLake;

/// The media type a transform request's JSON body travels as.
pub(crate) const JSON: &str = "application/json";

/// The path the surface publishes, from the modelled `@http(uri: "/transform-data")`.
pub(crate) const TRANSFORM_DATA_PATH: &str = "/transform-data";

/// A `ProfileId` the service never minted: 32 characters of the modelled
/// `^[a-f0-9]{32}$` alphabet, so it is refused for being absent rather than for being
/// outside the member's rules.
pub(crate) const UNMINTED_PROFILE_ID: &str = "00000000000000000000000000000000";

/// The bucket the sample data file a profile is inferred from is staged in.
const BUCKET: &str = "transform-sample-data";

/// The mapping config a CSV profile declares, as a caller writes it: one table, and
/// one resource mapping per entry of `resources`, each carrying one rule per
/// (source column, element path) pair.
pub(crate) fn csv_mapping(
    table: Option<&str>,
    primary_key: Option<&str>,
    resources: &[(&str, &[(&str, &str)])],
) -> String {
    let mut yaml = String::from("tables:\n  -");
    match table {
        Some(table) => yaml.push_str(&format!(" tableName: {table}\n")),
        None => yaml.push('\n'),
    }
    if let Some(primary_key) = primary_key {
        yaml.push_str(&format!("    primaryKeyColumn: {primary_key}\n"));
    }
    yaml.push_str("    resourceMappings:\n");
    for (resource_type, fields) in resources {
        yaml.push_str(&format!(
            "      - targetResourceType: {resource_type}\n        fieldMappings:\n"
        ));
        for (column, path) in *fields {
            yaml.push_str(&format!(
                "          - sourceColumn: {column}\n            fhirPath: {path}\n"
            ));
        }
    }
    yaml
}

/// A C-CDA document carrying one titled section per entry of `sections`, each with
/// that entry's narrative text.
fn ccda(sections: &[(&str, &str)]) -> String {
    let body: String = sections
        .iter()
        .map(|(title, narrative)| {
            format!(
                "<component><section><title>{title}</title><text>{narrative}</text>\
                 </section></component>"
            )
        })
        .collect();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
         <ClinicalDocument xmlns=\"urn:hl7-org:v3\">\
         <component><structuredBody>{body}</structuredBody></component>\
         </ClinicalDocument>"
    )
}

/// Create a profile holding `mapping` (file path to content) and return its
/// `ProfileId`, after reading the mapping back through
/// `GetDataTransformationProfile` — so the content a transform applies is content the
/// service really stored.
pub(crate) async fn create_profile(
    service: &HealthLake,
    name: &str,
    source_format: &str,
    mapping: &[(&str, &str)],
) -> String {
    let stored: Map<String, Value> = mapping
        .iter()
        .map(|(path, content)| ((*path).to_string(), Value::from(*content)))
        .collect();
    let (status, created) = service
        .rpc(
            "CreateDataTransformationProfile",
            &json!({
                "ProfileName": name,
                "SourceFormat": source_format,
                "Source": { "ProfileMapping": { "ProfileMapping": Value::Object(stored.clone()) } },
            }),
        )
        .await;
    assert!(
        (200..300).contains(&status),
        "CreateDataTransformationProfile({name}) must succeed, got {status}: {created}"
    );
    let profile_id = created["ProfileId"]
        .as_str()
        .unwrap_or_else(|| panic!("the create reports a ProfileId, got {created}"))
        .to_string();

    let (status, read) = service
        .rpc(
            "GetDataTransformationProfile",
            &json!({ "ProfileId": profile_id }),
        )
        .await;
    assert_eq!(
        status, 200,
        "GetDataTransformationProfile({name}) must answer 200, got {status}: {read}"
    );
    assert_eq!(
        read["ProfileMapping"],
        Value::Object(stored),
        "the profile a transform applies must hold the mapping the create stored, got {read}"
    );
    profile_id
}

/// Dispatch one transform by PATH, as a client with no `X-Amz-Target` addresses it.
async fn transform(service: &HealthLake, request: &Value) -> (u16, Value) {
    service
        .rest(
            "POST",
            TRANSFORM_DATA_PATH,
            JSON,
            serde_json::to_vec(request).expect("the request body serializes"),
        )
        .await
}

/// One transform request over a single CSV file.
pub(crate) fn csv_request(
    profile_id: &str,
    file: &str,
    content: &str,
    flags: &[(&str, bool)],
) -> Value {
    let files: Map<String, Value> =
        Map::from_iter([(file.to_string(), Value::from(content))]);
    let mut request = json!({
        "ProfileId": profile_id,
        "InputData": { "CsvInput": Value::Object(files) },
    });
    for (member, value) in flags {
        request[*member] = Value::from(*value);
    }
    request
}

/// The FHIR resources the answered Bundle carries, asserted to be a Bundle at all.
pub(crate) fn resources(case: &str, payload: &Value) -> Vec<Value> {
    let transformed = payload["TransformedData"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the answer carries TransformedData, got {payload}"));
    let bundle: Value = serde_json::from_str(transformed)
        .unwrap_or_else(|_| panic!("case {case:?}: TransformedData is JSON, got {transformed:?}"));
    assert_eq!(
        bundle["resourceType"], "Bundle",
        "case {case:?}: TransformedData must be a FHIR Bundle, got {bundle}"
    );
    assert_eq!(
        bundle["type"], "collection",
        "case {case:?}: a transform's Bundle is a collection, got {bundle}"
    );
    bundle["entry"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: a Bundle carries entries, got {bundle}"))
        .iter()
        .map(|entry| entry["resource"].clone())
        .collect()
}

/// RULE: each data row of a submitted file becomes one resource per resource mapping
/// the profile declares over that file's table, carrying the elements its rules name.
#[tokio::test]
async fn a_transform_applies_the_profiles_mapping_to_every_data_row() {
    let service = HealthLake::new();

    /// One row: the case, the mapping config, the submitted file name and content,
    /// and the resources the transform must produce.
    type Row = (&'static str, String, &'static str, &'static str, Vec<Value>);
    let patient_name: &[(&str, &str)] = &[
        ("LAST_NM", "name[0].family"),
        ("FIRST_NM", "name[0].given[0]"),
    ];
    let rows: Vec<Row> = vec![
        (
            "two data rows of a named table",
            csv_mapping(Some("patients"), Some("PAT_ID"), &[("Patient", patient_name)]),
            "patients",
            "PAT_ID,LAST_NM,FIRST_NM\nP001,Smith,John\nP002,Jones,Amy",
            vec![
                json!({
                    "resourceType": "Patient",
                    "id": "P001",
                    "name": [{ "family": "Smith", "given": ["John"] }],
                }),
                json!({
                    "resourceType": "Patient",
                    "id": "P002",
                    "name": [{ "family": "Jones", "given": ["Amy"] }],
                }),
            ],
        ),
        (
            "one data row",
            csv_mapping(Some("patients"), Some("PAT_ID"), &[("Patient", patient_name)]),
            "patients.csv",
            "PAT_ID,LAST_NM,FIRST_NM\nP009,Nakamura,Rei",
            vec![json!({
                "resourceType": "Patient",
                "id": "P009",
                "name": [{ "family": "Nakamura", "given": ["Rei"] }],
            })],
        ),
        (
            "a header row with no data rows",
            csv_mapping(Some("patients"), Some("PAT_ID"), &[("Patient", patient_name)]),
            "patients",
            "PAT_ID,LAST_NM,FIRST_NM\n",
            Vec::new(),
        ),
        (
            "a row carrying an empty cell, and a ragged row",
            csv_mapping(Some("patients"), Some("PAT_ID"), &[("Patient", patient_name)]),
            "patients",
            "PAT_ID,LAST_NM,FIRST_NM\nP001,Smith,\nP002,Jones",
            vec![
                json!({ "resourceType": "Patient", "id": "P001", "name": [{ "family": "Smith" }] }),
                json!({ "resourceType": "Patient", "id": "P002", "name": [{ "family": "Jones" }] }),
            ],
        ),
        (
            "two resource mappings over one table",
            csv_mapping(
                Some("vitals"),
                Some("ENC_ID"),
                &[
                    ("Patient", &[("LAST_NM", "name[0].family")]),
                    ("Observation", &[("SYSTOLIC", "valueQuantity.value")]),
                ],
            ),
            "vitals",
            "ENC_ID,LAST_NM,SYSTOLIC\nE1,Smith,120",
            vec![
                json!({ "resourceType": "Patient", "id": "E1", "name": [{ "family": "Smith" }] }),
                json!({
                    "resourceType": "Observation",
                    "id": "E1",
                    "valueQuantity": { "value": "120" },
                }),
            ],
        ),
        (
            "another target resource type and a deeper element path",
            csv_mapping(
                Some("staff"),
                None,
                &[(
                    "Practitioner",
                    &[
                        ("LAST_NM", "name[0].family"),
                        ("NPI", "identifier[0].value"),
                        ("SYSTEM", "identifier[0].system"),
                    ],
                )],
            ),
            "staff",
            "LAST_NM,NPI,SYSTEM\nOkafor,1234567893,http://hl7.org/fhir/sid/us-npi",
            vec![json!({
                "resourceType": "Practitioner",
                "name": [{ "family": "Okafor" }],
                "identifier": [{
                    "value": "1234567893",
                    "system": "http://hl7.org/fhir/sid/us-npi",
                }],
            })],
        ),
        (
            "a mapping and a file that spell the names differently",
            csv_mapping(
                Some("Patient_Records"),
                Some("pat id"),
                &[("Patient", &[("last name", "name[0].family")])],
            ),
            "patientrecords",
            "PatId,LastName\nP001,Smith",
            vec![
                json!({ "resourceType": "Patient", "id": "P001", "name": [{ "family": "Smith" }] }),
            ],
        ),
        (
            "a mapping naming no table, which reads whatever file is submitted",
            csv_mapping(None, None, &[("Patient", &[("LAST_NM", "name[0].family")])]),
            "anything-at-all.csv",
            "LAST_NM\nSmith",
            vec![json!({ "resourceType": "Patient", "name": [{ "family": "Smith" }] })],
        ),
        (
            "a file no rule of the mapping reads",
            csv_mapping(Some("patients"), None, &[("Patient", patient_name)]),
            "encounters",
            "ENC_ID\nE1",
            Vec::new(),
        ),
    ];

    for (case, mapping, file, content, expected) in rows {
        let profile_id = create_profile(
            &service,
            &format!("transform {case}"),
            "CSV",
            &[("mapping-config.yaml", &mapping)],
        )
        .await;
        // Provenance is asserted on its own below; with it off the produced
        // resources are exactly what the mapping wrote.
        let (status, payload) = transform(
            &service,
            &csv_request(&profile_id, file, content, &[("ProvenanceEnabled", false)]),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the transform must answer 200, got {status}: {payload}"
        );
        assert!(
            payload.get("DriftReport").is_none(),
            "case {case:?}: a caller that did not ask for drift detection is sent no \
             DriftReport, got {payload}"
        );
        let produced = resources(case, &payload);
        assert_eq!(
            produced, expected,
            "case {case:?}: expected the resources {expected:?}, got {produced:?}"
        );
    }
}

/// RULE: the version a transform applies is the one a read that names none resolves
/// to — the latest published version, and the DRAFT while nothing has been published.
#[tokio::test]
async fn a_transform_applies_the_version_a_read_resolves() {
    let service = HealthLake::new();
    let draft = csv_mapping(
        Some("patients"),
        None,
        &[("Patient", &[("LAST_NM", "name[0].family")])],
    );
    let profile_id = create_profile(
        &service,
        "transform versioning",
        "CSV",
        &[("mapping-config.yaml", &draft)],
    )
    .await;
    let input = "LAST_NM\nSmith";

    // The DRAFT, which nothing has been published from yet.
    let (status, payload) = transform(
        &service,
        &csv_request(&profile_id, "patients", input, &[("ProvenanceEnabled", false)]),
    )
    .await;
    assert_eq!(status, 200, "the DRAFT transform must answer 200: {payload}");
    assert_eq!(
        resources("the DRAFT", &payload),
        vec![json!({ "resourceType": "Patient", "name": [{ "family": "Smith" }] })],
        "the transform must apply the DRAFT while nothing is published, got {payload}"
    );

    // A DRAFT that writes another element, published as version 1.
    let published = csv_mapping(
        Some("patients"),
        None,
        &[("Patient", &[("LAST_NM", "name[0].text")])],
    );
    let (status, updated) = service
        .rpc(
            "UpdateDataTransformationProfile",
            &json!({
                "ProfileId": profile_id,
                "ProfileMapping": { "mapping-config.yaml": published },
            }),
        )
        .await;
    assert_eq!(status, 200, "the update must answer 200: {updated}");
    let (status, release) = service
        .rpc(
            "PublishDataTransformationProfile",
            &json!({ "ProfileId": profile_id, "SourceFormat": "CSV" }),
        )
        .await;
    assert_eq!(status, 200, "the publish must answer 200: {release}");

    // A DRAFT written AFTER the publish, which a read that names no version does not
    // resolve — so a transform does not apply it either.
    let later_draft = csv_mapping(
        Some("patients"),
        None,
        &[("Patient", &[("LAST_NM", "name[0].prefix[0]")])],
    );
    let (status, updated) = service
        .rpc(
            "UpdateDataTransformationProfile",
            &json!({
                "ProfileId": profile_id,
                "ProfileMapping": { "mapping-config.yaml": later_draft },
            }),
        )
        .await;
    assert_eq!(status, 200, "the second update must answer 200: {updated}");

    let (status, payload) = transform(
        &service,
        &csv_request(&profile_id, "patients", input, &[("ProvenanceEnabled", false)]),
    )
    .await;
    assert_eq!(
        status, 200,
        "the published transform must answer 200: {payload}"
    );
    assert_eq!(
        resources("the published version", &payload),
        vec![json!({ "resourceType": "Patient", "name": [{ "text": "Smith" }] })],
        "the transform must apply the latest PUBLISHED version, got {payload}"
    );
}

/// RULE: a C-CDA transform is the same rule over document sections — each section a
/// rule names becomes one resource of the type that rule names, carrying that
/// section's narrative.
#[tokio::test]
async fn a_ccda_transform_maps_each_section_a_rule_names() {
    let service = HealthLake::new();
    let mapping: Vec<(&str, String)> = vec![
        (
            "sections/vital-signs.json",
            json!({ "sourceSection": "Vital Signs", "targetResourceType": "Observation" })
                .to_string(),
        ),
        (
            "sections/problems.json",
            json!({ "sourceSection": "Problems", "targetResourceType": "Condition" }).to_string(),
        ),
    ];
    let declared: Vec<(&str, &str)> = mapping
        .iter()
        .map(|(path, content)| (*path, content.as_str()))
        .collect();
    let profile_id =
        create_profile(&service, "ccda transform", "CCDA", &declared).await;

    let document = ccda(&[
        ("Vital Signs", "Blood pressure 120/80"),
        ("Problems", "Essential hypertension"),
        ("Social History", "Never smoked"),
    ]);
    let (status, payload) = transform(
        &service,
        &json!({
            "ProfileId": profile_id,
            "InputData": { "CcdaInput": document },
            "DriftDetectionEnabled": true,
            "ProvenanceEnabled": false,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the C-CDA transform must answer 200, got {status}: {payload}"
    );
    let produced = resources("a C-CDA document", &payload);
    let expected = vec![
        json!({
            "resourceType": "Observation",
            "text": {
                "status": "additional",
                "div": "<div xmlns=\"http://www.w3.org/1999/xhtml\">Blood pressure 120/80</div>",
            },
        }),
        json!({
            "resourceType": "Condition",
            "text": {
                "status": "additional",
                "div": "<div xmlns=\"http://www.w3.org/1999/xhtml\">Essential hypertension\
                        </div>",
            },
        }),
    ];
    assert_eq!(
        produced, expected,
        "expected one resource per named section {expected:?}, got {produced:?}"
    );

    // The section no rule names is reported as drift rather than transformed.
    let report = drift(&payload);
    assert_eq!(
        report["DriftDetected"], Value::Bool(true),
        "a document carrying a section no rule names has drifted, got {report}"
    );
    assert!(
        report["Findings"]
            .as_array()
            .is_some_and(|findings| findings.contains(&json!({
                "Type": "UNMAPPED_SOURCE_SECTION",
                "SourceSection": "Social History",
            }))),
        "the drift report must name the unmapped section, got {report}"
    );
}

/// The drift report the answer carries, parsed.
fn drift(payload: &Value) -> Value {
    let report = payload["DriftReport"]
        .as_str()
        .unwrap_or_else(|| panic!("a caller that asked for drift detection is sent a DriftReport, got {payload}"));
    serde_json::from_str(report).unwrap_or_else(|_| panic!("the drift report is JSON, got {report:?}"))
}

/// RULE: the drift report states what the mapping and the submitted input disagree
/// about, and is answered only to a caller that asked for it.
#[tokio::test]
async fn drift_detection_reports_what_the_mapping_and_the_input_disagree_about() {
    let service = HealthLake::new();
    let mapping = csv_mapping(
        Some("patients"),
        None,
        &[(
            "Patient",
            &[
                ("LAST_NM", "name[0].family"),
                ("MRN", "identifier[0].value"),
            ],
        )],
    );
    let profile_id = create_profile(
        &service,
        "transform drift",
        "CSV",
        &[("mapping-config.yaml", &mapping)],
    )
    .await;

    /// One row: the case, the submitted file name and content, whether the input has
    /// drifted, and the findings the report must name.
    type Row = (&'static str, &'static str, &'static str, bool, Vec<Value>);
    let rows: Vec<Row> = vec![
        (
            "an input carrying exactly the mapped columns",
            "patients",
            "LAST_NM,MRN\nSmith,M1",
            false,
            Vec::new(),
        ),
        (
            "an input missing a mapped column and carrying an unmapped one",
            "patients",
            "LAST_NM,SSN\nSmith,123",
            true,
            vec![
                json!({
                    "Type": "MISSING_SOURCE_COLUMN",
                    "FileName": "patients",
                    "SourceColumn": "MRN",
                }),
                json!({
                    "Type": "UNMAPPED_SOURCE_COLUMN",
                    "FileName": "patients",
                    "SourceColumn": "SSN",
                }),
            ],
        ),
        (
            "a file no rule of the mapping reads",
            "encounters",
            "ENC_ID\nE1",
            true,
            vec![json!({ "Type": "UNMAPPED_FILE", "FileName": "encounters" })],
        ),
    ];

    for (case, file, content, drifted, expected) in rows {
        let (status, payload) = transform(
            &service,
            &csv_request(
                &profile_id,
                file,
                content,
                &[("DriftDetectionEnabled", true), ("ProvenanceEnabled", false)],
            ),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the transform must answer 200, got {status}: {payload}"
        );
        let report = drift(&payload);
        assert_eq!(
            report["DriftDetected"],
            Value::Bool(drifted),
            "case {case:?}: expected DriftDetected == {drifted}, got {report}"
        );
        let findings = report["Findings"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: the report carries Findings, got {report}"))
            .clone();
        assert_eq!(
            findings, expected,
            "case {case:?}: expected the findings {expected:?}, got {findings:?}"
        );
    }
}

/// RULE: provenance records the profile each produced resource came from, and is on
/// unless the request turns it off (the member's `@default` is `true`).
#[tokio::test]
async fn provenance_records_the_profile_each_resource_came_from() {
    let service = HealthLake::new();
    let mapping = csv_mapping(
        Some("patients"),
        None,
        &[("Patient", &[("LAST_NM", "name[0].family")])],
    );
    let profile_id = create_profile(
        &service,
        "transform provenance",
        "CSV",
        &[("mapping-config.yaml", &mapping)],
    )
    .await;
    let input = "LAST_NM\nSmith\nJones";

    /// One row: the case, the `ProvenanceEnabled` flags the request carries, and
    /// whether the produced resources record where they came from.
    type Row = (&'static str, Vec<(&'static str, bool)>, bool);
    let rows: Vec<Row> = vec![
        ("the member left out, which defaults to true", Vec::new(), true),
        ("the member sent true", vec![("ProvenanceEnabled", true)], true),
        ("the member sent false", vec![("ProvenanceEnabled", false)], false),
    ];

    for (case, flags, recorded) in rows {
        let (status, payload) =
            transform(&service, &csv_request(&profile_id, "patients", input, &flags)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the transform must answer 200, got {status}: {payload}"
        );
        let produced = resources(case, &payload);
        assert_eq!(
            produced.len(),
            2,
            "case {case:?}: both data rows must be transformed, got {produced:?}"
        );
        for resource in &produced {
            let source = resource["meta"]["source"].as_str();
            if recorded {
                let source = source.unwrap_or_else(|| {
                    panic!("case {case:?}: a resource records its source, got {resource}")
                });
                // The ARN the service minted for the profile that produced it: an
                // ARN naming that profile, not a value this test wrote.
                assert!(
                    source.starts_with("arn:")
                        && source.ends_with(&format!("dataTransformationProfile/{profile_id}")),
                    "case {case:?}: expected the source to be the profile's own ARN, got \
                     {source:?}"
                );
            } else {
                assert_eq!(
                    source, None,
                    "case {case:?}: a transform without provenance records no source, got \
                     {resource}"
                );
            }
        }
    }
}

/// RULE: a `ProfileId` no profile is held for is refused with the modelled
/// `ResourceNotFoundException` at HTTP 404, stating that profile's sentence.
#[tokio::test]
async fn a_profile_the_service_does_not_hold_is_refused_with_the_modelled_not_found() {
    let service = HealthLake::new();
    let mapping = csv_mapping(
        Some("patients"),
        None,
        &[("Patient", &[("LAST_NM", "name[0].family")])],
    );
    let deleted = create_profile(
        &service,
        "transform deleted profile",
        "CSV",
        &[("mapping-config.yaml", &mapping)],
    )
    .await;
    let (status, payload) = service
        .rpc(
            "DeleteDataTransformationProfile",
            &json!({ "ProfileId": deleted }),
        )
        .await;
    assert_eq!(status, 200, "the delete must answer 200, got {payload}");

    // (case, the ProfileId the request names)
    let rows: Vec<(&str, String)> = vec![
        ("an id the service never minted", UNMINTED_PROFILE_ID.to_string()),
        ("the id of a deleted profile", deleted),
    ];

    for (case, profile_id) in rows {
        let (status, payload) = transform(
            &service,
            &csv_request(&profile_id, "patients", "LAST_NM\nSmith", &[]),
        )
        .await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected the modelled 404, got {status}: {payload}"
        );
        assert_eq!(
            error_code(&payload),
            "ResourceNotFoundException",
            "case {case:?}: expected the modelled error shape, got {payload}"
        );
        assert_eq!(
            error_message(&payload),
            profile_not_found_message(&profile_id),
            "case {case:?}: expected the profile-not-found sentence, got {payload}"
        );
        assert!(
            payload.get("TransformedData").is_none(),
            "case {case:?}: a refusal carries no Bundle, got {payload}"
        );
    }
}

/// RULE: a request outside a rule the model declares — or input the profile does not
/// convert — is refused with the modelled `ValidationException` at HTTP 400, and
/// produces no Bundle.
#[tokio::test]
async fn a_request_outside_the_model_is_refused_with_the_modelled_validation_exception() {
    let service = HealthLake::new();
    let mapping = csv_mapping(
        Some("patients"),
        None,
        &[("Patient", &[("LAST_NM", "name[0].family")])],
    );
    let profile_id = create_profile(
        &service,
        "transform refusals",
        "CSV",
        &[("mapping-config.yaml", &mapping)],
    )
    .await;
    let unmapped = create_profile(
        &service,
        "transform with no mapping",
        "CSV",
        &[("notes.txt", "a profile note, in prose")],
    )
    .await;

    // (case, the request body)
    let rows: Vec<(&str, Value)> = vec![
        (
            "no ProfileId",
            json!({ "InputData": { "CsvInput": { "patients": "LAST_NM\nSmith" } } }),
        ),
        (
            "a ProfileId outside the member's length",
            json!({
                "ProfileId": "abc",
                "InputData": { "CsvInput": { "patients": "LAST_NM\nSmith" } },
            }),
        ),
        (
            "a ProfileId outside the member's pattern",
            json!({
                "ProfileId": "A".repeat(32),
                "InputData": { "CsvInput": { "patients": "LAST_NM\nSmith" } },
            }),
        ),
        ("no InputData", json!({ "ProfileId": profile_id })),
        (
            "an InputData carrying neither member of the union",
            json!({ "ProfileId": profile_id, "InputData": {} }),
        ),
        (
            "an InputData carrying both members of the union",
            json!({
                "ProfileId": profile_id,
                "InputData": { "CcdaInput": "<ClinicalDocument/>", "CsvInput": {} },
            }),
        ),
        (
            "a file name outside the modelled pattern",
            json!({
                "ProfileId": profile_id,
                "InputData": { "CsvInput": { "pat ients": "LAST_NM\nSmith" } },
            }),
        ),
        (
            "a flag that is not a boolean",
            json!({
                "ProfileId": profile_id,
                "InputData": { "CsvInput": { "patients": "LAST_NM\nSmith" } },
                "DriftDetectionEnabled": "yes",
            }),
        ),
        (
            "input the profile's source format does not travel in",
            json!({
                "ProfileId": profile_id,
                "InputData": { "CcdaInput": "<ClinicalDocument/>" },
            }),
        ),
        (
            "a submitted file carrying no header row",
            json!({
                "ProfileId": profile_id,
                "InputData": { "CsvInput": { "patients": "" } },
            }),
        ),
        (
            "a profile whose content declares no mapping",
            json!({
                "ProfileId": unmapped,
                "InputData": { "CsvInput": { "patients": "LAST_NM\nSmith" } },
            }),
        ),
        (
            "more files than one transform reads",
            json!({
                "ProfileId": profile_id,
                "InputData": { "CsvInput": Value::Object(
                    (0..=MAX_TRANSFORM_INPUT_FILES)
                        .map(|index| (format!("f{index}"), Value::from("LAST_NM\nSmith")))
                        .collect::<Map<String, Value>>()
                ) },
            }),
        ),
    ];

    for (case, request) in rows {
        let (status, payload) = transform(&service, &request).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the modelled 400, got {status}: {payload}"
        );
        assert_eq!(
            error_code(&payload),
            "ValidationException",
            "case {case:?}: expected the modelled error shape, got {payload}"
        );
        assert!(
            !error_message(&payload).is_empty(),
            "case {case:?}: a refusal states why, got {payload}"
        );
        assert!(
            payload.get("TransformedData").is_none(),
            "case {case:?}: a refusal carries no Bundle, got {payload}"
        );
    }
}

/// RULE: the drift report is bounded in what it records — a submitted input that
/// diverges in more ways than the report holds is reported up to that ceiling and
/// says it was cut off, rather than answering a report sized by the input.
#[tokio::test]
async fn a_drift_report_is_bounded_in_what_it_records() {
    let service = HealthLake::new();
    let mapping = csv_mapping(
        Some("wide"),
        None,
        &[("Patient", &[("V", "name[0].family")])],
    );
    let profile_id = create_profile(
        &service,
        "transform drift ceiling",
        "CSV",
        &[("mapping-config.yaml", &mapping)],
    )
    .await;
    // One column every rule reads, and far more that no rule does.
    let unmapped: Vec<String> = (0..MAX_DRIFT_FINDINGS + 50)
        .map(|index| format!("c{index}"))
        .collect();
    let content = format!("V,{}\nx\n", unmapped.join(","));

    let (status, payload) = transform(
        &service,
        &csv_request(
            &profile_id,
            "wide",
            &content,
            &[("DriftDetectionEnabled", true), ("ProvenanceEnabled", false)],
        ),
    )
    .await;
    assert_eq!(
        status, 200,
        "the transform must answer 200, got {status}: {payload}"
    );
    let report = drift(&payload);
    assert_eq!(
        report["Findings"].as_array().map(Vec::len),
        Some(MAX_DRIFT_FINDINGS),
        "the report records at most its ceiling of findings, got {report}"
    );
    assert_eq!(
        report["FindingsTruncated"],
        Value::Bool(true),
        "a report that was cut off must say so, got {report}"
    );
    assert_eq!(
        report["DriftDetected"],
        Value::Bool(true),
        "an input that diverged has drifted, got {report}"
    );
}

/// RULE: the operation answers wherever it is addressed — by path, and by the RPC
/// target an SDK built from the full model sends.
#[tokio::test]
async fn the_transform_answers_the_rpc_target_too() {
    let service = HealthLake::new();
    let mapping = csv_mapping(
        Some("patients"),
        None,
        &[("Patient", &[("LAST_NM", "name[0].family")])],
    );
    let profile_id = create_profile(
        &service,
        "transform over the rpc wire",
        "CSV",
        &[("mapping-config.yaml", &mapping)],
    )
    .await;
    let request = csv_request(
        &profile_id,
        "patients",
        "LAST_NM\nSmith",
        &[("ProvenanceEnabled", false)],
    );

    let (path_status, by_path) = transform(&service, &request).await;
    let (rpc_status, by_target) = service.rpc("TransformData", &request).await;
    assert_eq!(
        (path_status, rpc_status),
        (200, 200),
        "both wires must answer 200, got {path_status} ({by_path}) and {rpc_status} ({by_target})"
    );
    assert_eq!(
        resources("by target", &by_target),
        resources("by path", &by_path),
        "the two wires must answer the same Bundle, got {by_target} and {by_path}"
    );
}

/// RULE: the mapping a transform applies is whatever the profile holds, including the
/// rules this service INFERRED for it from a sample data file — where a rule names the
/// resource it is a path into (`Patient.gender`), the value lands in that ELEMENT of
/// the resource and not in a member named after the type.
#[tokio::test]
async fn a_transform_applies_the_mapping_inferred_from_a_sample_data_file() {
    let service = HealthLake::with_object_store();
    let created_bucket = service.s3().create_bucket().bucket(BUCKET).send().await;
    assert!(
        created_bucket.is_ok(),
        "Amazon S3 must create the sample bucket, got {created_bucket:?}"
    );
    // A header row of two `Patient` elements, one more under the resource's own name,
    // and one column that is no element of it — which the inference maps onto an
    // `Observation` instead.
    let sample = "patient_id,gender,birth_date,systolic_bp\nP001,female,1980-01-02,120\n";
    let staged = service
        .s3()
        .put_object()
        .bucket(BUCKET)
        .key("vitals.csv")
        .body(aws_sdk_s3::primitives::ByteStream::from(
            sample.as_bytes().to_vec(),
        ))
        .send()
        .await;
    assert!(
        staged.is_ok(),
        "Amazon S3 must hold the sample file, got {staged:?}"
    );

    let (status, created) = service
        .rpc(
            "CreateDataTransformationProfile",
            &json!({
                "ProfileName": "transform from sample data",
                "SourceFormat": "CSV",
                "Source": { "SampleData": { "S3Uri": format!("s3://{BUCKET}/vitals.csv") } },
            }),
        )
        .await;
    assert!(
        (200..300).contains(&status),
        "the create must infer a profile from the sample file, got {status}: {created}"
    );
    let profile_id = created["ProfileId"]
        .as_str()
        .unwrap_or_else(|| panic!("the create reports a ProfileId, got {created}"))
        .to_string();

    let (status, payload) = transform(
        &service,
        &csv_request(
            &profile_id,
            "vitals",
            "patient_id,gender,birth_date,systolic_bp\nP002,male,1975-07-08,135\n",
            &[("ProvenanceEnabled", false)],
        ),
    )
    .await;
    assert_eq!(
        status, 200,
        "the inferred mapping must transform, got {status}: {payload}"
    );
    let produced = resources("an inferred mapping", &payload);
    let expected = vec![
        json!({
            "resourceType": "Patient",
            "id": "P002",
            "gender": "male",
            "birthDate": "1975-07-08",
        }),
        json!({ "resourceType": "Observation", "valueQuantity": "135" }),
    ];
    assert_eq!(
        produced, expected,
        "expected the elements the inferred rules name {expected:?}, got {produced:?}"
    );
}

/// RULE: the bytes a transform writes into its Bundle are bounded, so the product of
/// how many element rules a mapping declares and how long the submitted values are
/// cannot grow one answer without limit — a request past the ceiling is refused with
/// the modelled `ValidationException` and answers no Bundle.
#[tokio::test]
async fn a_transform_that_would_write_past_the_output_ceiling_is_refused() {
    let service = HealthLake::new();
    // One column, read by many rules, each writing a different element: neither the
    // row count nor the resource count moves, so only the bytes written can bound it.
    let fields: Vec<(String, String)> = (0..1_000)
        .map(|index| ("V".to_string(), format!("p{index}")))
        .collect();
    let borrowed: Vec<(&str, &str)> = fields
        .iter()
        .map(|(column, path)| (column.as_str(), path.as_str()))
        .collect();
    let mapping = csv_mapping(Some("wide"), None, &[("Patient", &borrowed)]);
    let profile_id = create_profile(
        &service,
        "transform output ceiling",
        "CSV",
        &[("mapping-config.yaml", &mapping)],
    )
    .await;

    // (case, the one value the row carries, whether the transform is served)
    let rows: [(&str, usize, bool); 2] = [
        ("a value the rules can all copy", 8, true),
        ("a value the rules would amplify past the ceiling", 10_000, false),
    ];

    for (case, value_len, served) in rows {
        let content = format!("V\n{}\n", "x".repeat(value_len));
        let (status, payload) = transform(
            &service,
            &csv_request(&profile_id, "wide", &content, &[("ProvenanceEnabled", false)]),
        )
        .await;
        if served {
            assert_eq!(
                status, 200,
                "case {case:?}: a transform inside the ceiling is served, got {status}: {payload}"
            );
            assert_eq!(
                resources(case, &payload).len(),
                1,
                "case {case:?}: one row read by one resource mapping is one resource, got \
                 {payload}"
            );
        } else {
            assert_eq!(
                status, 400,
                "case {case:?}: expected the modelled 400, got {status}: {payload}"
            );
            assert_eq!(
                error_code(&payload),
                "ValidationException",
                "case {case:?}: expected the modelled error shape, got {payload}"
            );
            assert!(
                payload.get("TransformedData").is_none(),
                "case {case:?}: a refusal carries no Bundle, got {payload}"
            );
        }
    }
}
