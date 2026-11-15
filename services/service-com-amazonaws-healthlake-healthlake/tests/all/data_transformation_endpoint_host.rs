// SPDX-License-Identifier: CC0-1.0

//! Which HOST a client reached this emulator on, and what that does NOT change.
//!
//! The emulator serves every surface of a service at the ONE address it is reached
//! on, and a surface that carries no `X-Amz-Target` is selected by HTTP method and
//! path alone (`EmulatedService::override_operation`, whose readers in
//! `src/service/non_rpc.rs` take the request path and nothing else). So the host a
//! client was pointed at — the endpoint host an SDK resolves, the
//! `datatransformation.`-prefixed host a client splices to address the
//! data-transformation endpoint `TransformData` and `/validate-source` are bound to,
//! the literal address and port an emulator is served on, either form of it, or no
//! `Host` header at all — decides neither WHETHER a surface answers nor WHAT it
//! answers.
//!
//! The rules under test, each over the SET of hosts above rather than one recorded
//! request:
//!
//! 1. `POST /transform-data` applies the named profile's stored mapping and answers
//!    HTTP 200 with `TransformedData` carrying the FHIR Bundle, whichever host the
//!    request was addressed to — including a profile created over the awsJson1_0 wire
//!    on a DIFFERENT host, since one address serves both;
//! 2. a `ProfileId` no profile is held for is refused with the modelled
//!    `ResourceNotFoundException` at HTTP 404 on every one of those hosts, so the
//!    modelled refusal is reachable wherever the operation is addressed;
//! 3. the sibling non-RPC surface of the same endpoint, `POST /validate-source`,
//!    answers on every one of them too — the rule is the seam's, not this one
//!    operation's.

use serde_json::{json, Value};
use service_com_amazonaws_healthlake_healthlake::state::profile_not_found_message;

use crate::fhir_datastore_lifecycle::{error_code, error_message};
use crate::fhir_rest_and_validate_source::{HealthLake, ENDPOINT_HOST};
use crate::transform_data::{
    create_profile, csv_mapping, csv_request, resources, JSON, TRANSFORM_DATA_PATH,
    UNMINTED_PROFILE_ID,
};

/// One submitted CSV data row: the primary key column and the two name columns the
/// profile's mapping reads.
type DataRow = (&'static str, &'static str, &'static str);

/// Every host form a client may have reached this emulator's HealthLake endpoint on,
/// with the data rows the transform addressed to it submits.
///
/// `None` is a client that sent no `Host` header, which is the floor of the same rule:
/// nothing about the host is read, so its absence cannot be read either.
fn hosts() -> Vec<(&'static str, Option<&'static str>, Vec<DataRow>)> {
    vec![
        (
            "the endpoint host an SDK resolves",
            Some(ENDPOINT_HOST),
            vec![("P001", "Smith", "John"), ("P002", "Jones", "Amy")],
        ),
        (
            "the data-transformation endpoint host",
            Some("datatransformation.healthlake.us-east-1.amazonaws.com"),
            vec![("P010", "Nakamura", "Rei")],
        ),
        (
            "the data-transformation host of another region",
            Some("datatransformation.healthlake.eu-west-1.amazonaws.com"),
            vec![
                ("P020", "Okafor", "Ada"),
                ("P021", "Silva", "Luis"),
                ("P022", "Haddad", "Nour"),
            ],
        ),
        (
            "a dual-stack endpoint host",
            Some("healthlake.us-east-1.api.aws"),
            vec![("P030", "Brown", "Max")],
        ),
        (
            "the literal address and port an emulator is reached on",
            Some("127.0.0.1:9297"),
            vec![("P040", "Dubois", "Elise")],
        ),
        (
            "that literal address with the data-transformation prefix",
            Some("datatransformation.127.0.0.1:9297"),
            vec![("P050", "Kowalski", "Ewa"), ("P051", "Rossi", "Gia")],
        ),
        (
            // What this case pins is the ROUTING seam's indifference to the host — the
            // floor of the rule, since a header that is not read cannot be read when it
            // is absent either. It is not a claim about the wire: a real HTTP/1.1 request
            // carrying no `Host` is refused by the server before any routing happens.
            "a request carrying no Host header at all",
            None,
            vec![("P060", "Ali", "Sara")],
        ),
    ]
}

/// The mapping every profile below holds: one table, read into one `Patient` per data
/// row carrying the two mapped name elements.
fn patient_mapping() -> String {
    csv_mapping(
        Some("patients"),
        Some("PAT_ID"),
        &[(
            "Patient",
            &[
                ("LAST_NM", "name[0].family"),
                ("FIRST_NM", "name[0].given[0]"),
            ],
        )],
    )
}

/// `rows` as the CSV file a client submits: the header the mapping names, then one
/// line per row.
fn csv(rows: &[DataRow]) -> String {
    let mut content = String::from("PAT_ID,LAST_NM,FIRST_NM");
    for (id, family, given) in rows {
        content.push_str(&format!("\n{id},{family},{given}"));
    }
    content
}

/// The `Patient` each data row maps to, which is what the answered Bundle must carry.
fn mapped(rows: &[DataRow]) -> Vec<Value> {
    rows.iter()
        .map(|(id, family, given)| {
            json!({
                "resourceType": "Patient",
                "id": id,
                "name": [{ "family": family, "given": [given] }],
            })
        })
        .collect()
}

/// Dispatch one transform from a client that reached the emulator on `host`.
async fn transform_from_host(
    service: &HealthLake,
    host: Option<&str>,
    request: &Value,
) -> (u16, Value) {
    service
        .rest_from_host(
            host,
            "POST",
            TRANSFORM_DATA_PATH,
            JSON,
            serde_json::to_vec(request).expect("the request body serializes"),
        )
        .await
}

/// RULE: a transform applies the named profile's mapping and answers the Bundle
/// whichever host the request was addressed to.
#[tokio::test]
async fn a_transform_answers_whichever_host_the_endpoint_was_reached_on() {
    let service = HealthLake::new();
    // Created over the awsJson1_0 wire on the endpoint host: the profile a transform
    // addressed to any other host then resolves is this one, because the emulator is
    // ONE address.
    let profile_id = create_profile(
        &service,
        "transform over every endpoint host",
        "CSV",
        &[("mapping-config.yaml", &patient_mapping())],
    )
    .await;

    for (case, host, rows) in hosts() {
        let (status, payload) = transform_from_host(
            &service,
            host,
            // Provenance is asserted by `tests/all/transform_data.rs`; with it off the
            // produced resources are exactly what the mapping wrote.
            &csv_request(
                &profile_id,
                "patients",
                &csv(&rows),
                &[("ProvenanceEnabled", false)],
            ),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the transform must answer 200, got {status}: {payload}"
        );
        let produced = resources(case, &payload);
        let expected = mapped(&rows);
        assert_eq!(
            produced, expected,
            "case {case:?}: expected the resources {expected:?}, got {produced:?}"
        );
    }
}

/// RULE: the modelled `ResourceNotFoundException` at HTTP 404 is what a `ProfileId` no
/// profile is held for is refused with, on every host the operation can be addressed
/// on.
#[tokio::test]
async fn an_unknown_profile_is_refused_the_modelled_not_found_on_every_host() {
    let service = HealthLake::new();
    let deleted = create_profile(
        &service,
        "transform host refusals",
        "CSV",
        &[("mapping-config.yaml", &patient_mapping())],
    )
    .await;
    let (status, payload) = service
        .rpc(
            "DeleteDataTransformationProfile",
            &json!({ "ProfileId": deleted }),
        )
        .await;
    assert_eq!(status, 200, "the delete must answer 200, got {payload}");

    for (host_case, host, rows) in hosts() {
        // (what the request names, the ProfileId it names)
        let named: [(&str, &str); 2] = [
            ("an id the service never minted", UNMINTED_PROFILE_ID),
            ("the id of a deleted profile", &deleted),
        ];
        for (named_case, profile_id) in named {
            let case = format!("{named_case} on {host_case}");
            let (status, payload) = transform_from_host(
                &service,
                host,
                &csv_request(profile_id, "patients", &csv(&rows), &[]),
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
                profile_not_found_message(profile_id),
                "case {case:?}: expected the profile-not-found sentence, got {payload}"
            );
            assert!(
                payload.get("TransformedData").is_none(),
                "case {case:?}: a refusal carries no Bundle, got {payload}"
            );
        }
    }
}

/// RULE: the host is unread by the whole non-RPC seam, so the sibling surface of the
/// same endpoint — `POST /validate-source` — answers on every one of those hosts too.
#[tokio::test]
async fn the_sibling_non_rpc_surface_answers_on_every_host_too() {
    let service = HealthLake::new();
    // A document whose outcome is settled by its CONTENT (it is not XML at all), so
    // what varies between the cases below is only the host.
    const DOCUMENT: &str = "not xml at all";

    for (case, host, _rows) in hosts() {
        let (status, report) = service
            .rest_from_host(
                host,
                "POST",
                "/validate-source?sourceFormat=CCDA",
                "application/octet-stream",
                DOCUMENT.as_bytes().to_vec(),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: a content-level outcome is reported at HTTP 200, got {status}: \
             {report}"
        );
        assert_eq!(
            report["SourceFormat"].as_str(),
            Some("CCDA"),
            "case {case:?}: the report names the validated source format, got {report}"
        );
        assert_eq!(
            report["Valid"].as_bool(),
            Some(false),
            "case {case:?}: a document that is not a C-CDA at all is reported invalid, \
             got {report}"
        );
    }
}
