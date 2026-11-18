// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the WIRE FORM of every date-time member AppFabric answers with,
//! driven over the wire (restJson1) through the same router the emulator boots.
//!
//! RULE: a timestamp member whose model declares `@timestampFormat("date-time")` is carried
//! on the JSON wire as an RFC 3339 / ISO 8601 date-time STRING ending in `Z`, never as the
//! epoch-seconds number the `restJson1` protocol defaults to. Every timestamp member of this
//! model targets `com.amazonaws.appfabric#DateTime`, which carries that trait, so the rule
//! holds for every one of them — in a single record, in a list item, in a `@required` member
//! and in an optional one.
//!
//! The tests below assert that form over a TABLE spanning the shapes the trait reaches and
//! the operations that report them, and the invariants the instants keep across reads: a
//! record's creation instant is the same one every later read reports, and its update
//! instant never goes backwards.
//!
//! The harness (the router, `Answer`, `encode`, `arn_of`, `uuid_of`) is the one
//! `app_bundle_ingestion_lifecycle` already builds.

use aws_smithy_types::date_time::Format;
use aws_smithy_types::DateTime;
use serde_json::{json, Value};

use crate::app_bundle_ingestion_lifecycle::{arn_of, encode, uuid_of, Answer, Emulator};

/// Every member name this model targets at `com.amazonaws.appfabric#DateTime`, the one
/// timestamp shape it declares and the one carrying `@timestampFormat("date-time")`:
/// `AppAuthorization$createdAt`/`$updatedAt`, `AppAuthorizationSummary$updatedAt`,
/// `Ingestion$createdAt`/`$updatedAt` and `IngestionDestination$createdAt`/`$updatedAt`
/// (`smithy_query(query="shape",
/// shape="DateTime,AppAuthorization,AppAuthorizationSummary,Ingestion,IngestionDestination")`).
/// Two names cover all seven members, so a body is searched for these wherever they appear.
const DATE_TIME_MEMBERS: &[&str] = &["createdAt", "updatedAt"];

/// Every date-time member a response body carries, as `(path, value)` pairs in the order the
/// walk finds them — so a nested record and a list item are checked exactly as a top-level
/// member is.
fn date_time_members(value: &Value, path: &str, found: &mut Vec<(String, Value)>) {
    match value {
        Value::Object(members) => {
            for (name, member) in members {
                let child = format!("{path}.{name}");
                if DATE_TIME_MEMBERS.contains(&name.as_str()) {
                    found.push((child.clone(), member.clone()));
                } else {
                    date_time_members(member, &child, found);
                }
            }
        }
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                date_time_members(item, &format!("{path}[{index}]"), found);
            }
        }
        _ => {}
    }
}

/// The instant one date-time member reports, refusing anything that is not the modelled
/// form: a JSON string an RFC 3339 date-time parser reads, in UTC (`Z`).
fn instant_of(case: &str, path: &str, value: &Value) -> f64 {
    let text = value.as_str().unwrap_or_else(|| {
        panic!("case {case:?}: expected {path} to be a date-time string, got {value}")
    });
    assert!(
        text.ends_with('Z'),
        "case {case:?}: expected {path} in UTC (a trailing Z), got {text:?}"
    );
    assert!(
        text.contains('T'),
        "case {case:?}: expected {path} as an RFC 3339 date-time, got {text:?}"
    );
    let parsed = DateTime::from_str(text, Format::DateTime).unwrap_or_else(|error| {
        panic!("case {case:?}: expected {path} to parse as a date-time, got {text:?}: {error}")
    });
    parsed.as_secs_f64()
}

/// Every date-time member of `answer`, as `(path, instant)`, after asserting that the answer
/// carries exactly `expected` of them and that each is in the modelled form.
fn instants(case: &str, answer: &Answer, expected: usize) -> Vec<(String, f64)> {
    let mut found = Vec::new();
    date_time_members(&answer.body, "$", &mut found);
    assert_eq!(
        found.len(),
        expected,
        "case {case:?}: expected {expected} date-time member(s), found {:?} in body {}",
        found.iter().map(|(path, _)| path.clone()).collect::<Vec<_>>(),
        answer.body
    );
    found
        .iter()
        .map(|(path, value)| (path.clone(), instant_of(case, path, value)))
        .collect()
}

/// One date-time member of an answer, by the member name it ends with.
fn member(case: &str, reported: &[(String, f64)], name: &str) -> f64 {
    reported
        .iter()
        .find(|(path, _)| path.ends_with(name))
        .map(|(_, instant)| *instant)
        .unwrap_or_else(|| panic!("case {case:?}: expected a {name} among {reported:?}"))
}

/// An app bundle in the default Region, as its ARN.
async fn app_bundle(emulator: &Emulator) -> String {
    let created = emulator.create_app_bundle(json!({})).await;
    assert_eq!(
        created.status, 201,
        "the set-up app bundle must be created, got HTTP {} with body {}",
        created.status, created.body
    );
    arn_of(&created.body, "appBundle")
}

/// The credential body one `authType` takes — the pairing the model documents on
/// `credential` ("Specify credentials that match the authorization type for your request").
fn credential_for(auth_type: &str) -> Value {
    if auth_type == "apiKey" {
        json!({"apiKeyCredential": {"apiKey": "placeholder-api-key"}})
    } else {
        json!({"oauth2Credential": {
            "clientId": "placeholder-client-id",
            "clientSecret": "placeholder-client-secret",
        }})
    }
}

/// `CreateAppAuthorization`.
async fn create_authorization(
    emulator: &Emulator,
    bundle: &str,
    app: &str,
    auth_type: &str,
    tenant: &str,
) -> Answer {
    emulator
        .call(
            "POST",
            &format!("/appbundles/{}/appauthorizations", encode(bundle)),
            Some(json!({
                "app": app,
                "authType": auth_type,
                "credential": credential_for(auth_type),
                "tenant": {
                    "tenantIdentifier": tenant,
                    "tenantDisplayName": format!("{tenant} display"),
                },
            })),
        )
        .await
}

/// `GetAppAuthorization`.
async fn get_authorization(emulator: &Emulator, bundle: &str, authorization: &str) -> Answer {
    emulator
        .call(
            "GET",
            &format!(
                "/appbundles/{}/appauthorizations/{}",
                encode(bundle),
                encode(authorization)
            ),
            None,
        )
        .await
}

/// `UpdateAppAuthorization`, rotating the credential of the stored `authType`.
async fn update_authorization(
    emulator: &Emulator,
    bundle: &str,
    authorization: &str,
    auth_type: &str,
) -> Answer {
    emulator
        .call(
            "PATCH",
            &format!(
                "/appbundles/{}/appauthorizations/{}",
                encode(bundle),
                encode(authorization)
            ),
            Some(json!({"credential": credential_for(auth_type)})),
        )
        .await
}

/// `ListAppAuthorizations`.
async fn list_authorizations(emulator: &Emulator, bundle: &str) -> Answer {
    emulator
        .call(
            "GET",
            &format!("/appbundles/{}/appauthorizations", encode(bundle)),
            None,
        )
        .await
}

/// The `appAuthorizationArn` of a success body.
fn authorization_arn(answer: &Answer) -> String {
    answer
        .body
        .get("appAuthorization")
        .and_then(|authorization| authorization.get("appAuthorizationArn"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!(
                "expected appAuthorization.appAuthorizationArn in {}",
                answer.body
            )
        })
        .to_string()
}

/// RULE: both date-time members of an app authorization (`createdAt` and `updatedAt`, each
/// `@required` and each targeting the `date-time` shape) are RFC 3339 strings in every
/// operation that reports the record — the create, the read back and the update — and the
/// creation instant a read reports is the one the create call reported.
///
/// The table spans both `AuthType` values, applications the issue did not cite, and both
/// identifier forms a read accepts.
#[tokio::test]
async fn an_app_authorization_reports_its_date_time_members_as_rfc3339_strings() {
    // (case, app, authType, tenant)
    let rows: &[(&str, &str, &str, &str)] = &[
        ("an apiKey authorization", "ZENDESK", "apiKey", "tenant-z"),
        ("an oauth2 authorization", "SLACK", "oauth2", "tenant-s"),
        ("another application", "ASANA", "apiKey", "tenant-a"),
    ];

    for (case, app, auth_type, tenant) in rows {
        // A fresh emulator per row: a Region holds one app bundle.
        let emulator = Emulator::new();
        let bundle = app_bundle(&emulator).await;

        let created = create_authorization(&emulator, &bundle, app, auth_type, tenant).await;
        assert_eq!(
            created.status, 201,
            "case {case:?}: expected HTTP 201, got {} with body {}",
            created.status, created.body
        );
        let reported = instants(case, &created, 2);
        let created_at = member(case, &reported, "createdAt");
        let created_updated_at = member(case, &reported, "updatedAt");
        assert!(
            created_updated_at >= created_at,
            "case {case:?}: expected updatedAt not before createdAt, got {reported:?}"
        );

        let arn = authorization_arn(&created);
        for identifier in [arn.clone(), uuid_of(&arn)] {
            let read = get_authorization(&emulator, &bundle, &identifier).await;
            assert_eq!(
                read.status, 200,
                "case {case:?} read by {identifier:?}: expected HTTP 200, got {} with body {}",
                read.status, read.body
            );
            let read_back = instants(case, &read, 2);
            assert_eq!(
                member(case, &read_back, "createdAt"),
                created_at,
                "case {case:?} read by {identifier:?}: expected the creation instant the create \
                 call reported, got {}",
                read.body
            );
        }

        let updated = update_authorization(&emulator, &bundle, &arn, auth_type).await;
        assert_eq!(
            updated.status, 200,
            "case {case:?}: expected the update to answer HTTP 200, got {} with body {}",
            updated.status, updated.body
        );
        let after = instants(case, &updated, 2);
        assert_eq!(
            member(case, &after, "createdAt"),
            created_at,
            "case {case:?}: an update must leave the creation instant alone, got {}",
            updated.body
        );
        assert!(
            member(case, &after, "updatedAt") >= created_updated_at,
            "case {case:?}: expected the update instant not to go backwards, got {after:?}"
        );
    }
}

/// RULE: each entry of a listing carries the same date-time form a single record does —
/// `AppAuthorizationSummary$updatedAt` is `@required` and targets the same `date-time` shape
/// — and reports the instant the record itself reports.
///
/// The table spans the number of entries the list carries, which is what "one date-time
/// string per entry" has to hold across.
#[tokio::test]
async fn every_app_authorization_summary_reports_its_update_instant_as_an_rfc3339_string() {
    for count in [1_usize, 2, 4] {
        let case = format!("{count} authorization(s)");
        let emulator = Emulator::new();
        let bundle = app_bundle(&emulator).await;

        let mut by_arn: Vec<(String, f64)> = Vec::new();
        for index in 0..count {
            let created = create_authorization(
                &emulator,
                &bundle,
                "SLACK",
                "apiKey",
                &format!("tenant-{index}"),
            )
            .await;
            assert_eq!(
                created.status, 201,
                "case {case:?}, number {index}: expected HTTP 201, got {} with body {}",
                created.status, created.body
            );
            let reported = instants(&case, &created, 2);
            by_arn.push((
                authorization_arn(&created),
                member(&case, &reported, "updatedAt"),
            ));
        }

        let listed = list_authorizations(&emulator, &bundle).await;
        assert_eq!(
            listed.status, 200,
            "case {case:?}: expected HTTP 200, got {} with body {}",
            listed.status, listed.body
        );
        // One `updatedAt` per entry, each in the modelled form.
        let reported = instants(&case, &listed, count);
        assert_eq!(
            reported.len(),
            count,
            "case {case:?}: expected one date-time member per entry, got {reported:?}"
        );

        let summaries = listed
            .body
            .get("appAuthorizationSummaryList")
            .and_then(Value::as_array)
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected an appAuthorizationSummaryList in {}",
                    listed.body
                )
            });
        for (arn, instant) in &by_arn {
            let summary = summaries
                .iter()
                .find(|summary| {
                    summary.get("appAuthorizationArn").and_then(Value::as_str) == Some(arn.as_str())
                })
                .unwrap_or_else(|| {
                    panic!("case {case:?}: expected a summary for {arn:?} in {}", listed.body)
                });
            let path = format!("$.appAuthorizationSummaryList[{arn}].updatedAt");
            let member_value = summary.get("updatedAt").unwrap_or_else(|| {
                panic!("case {case:?}: expected an updatedAt in {summary}")
            });
            assert_eq!(
                instant_of(&case, &path, member_value),
                *instant,
                "case {case:?}: expected the summary of {arn:?} to report the record's update \
                 instant, got {summary}"
            );
        }
    }
}

/// `CreateIngestion` for an application and tenant the bundle is authorized for.
async fn create_ingestion(emulator: &Emulator, bundle: &str, app: &str, tenant: &str) -> Answer {
    emulator
        .create_ingestion(
            bundle,
            json!({"app": app, "tenantId": tenant, "ingestionType": "auditLog"}),
        )
        .await
}

/// `CreateIngestionDestination` delivering an audit log to an S3 bucket.
async fn create_destination(
    emulator: &Emulator,
    bundle: &str,
    ingestion: &str,
    bucket: &str,
) -> Answer {
    emulator
        .call(
            "POST",
            &format!(
                "/appbundles/{}/ingestions/{}/ingestiondestinations",
                encode(bundle),
                encode(ingestion)
            ),
            Some(json!({
                "processingConfiguration": {"auditLog": {"schema": "ocsf", "format": "json"}},
                "destinationConfiguration": {
                    "auditLog": {"destination": {"s3Bucket": {"bucketName": bucket}}},
                },
            })),
        )
        .await
}

/// RULE: the same date-time form holds for every other shape the trait reaches, whether the
/// member is `@required` (an ingestion's `createdAt`/`updatedAt`) or optional (an ingestion
/// destination's), and holds on the read-back path as well as on the write that minted the
/// record.
///
/// The table spans the two shapes, both of the chain's write operations and their reads, and
/// applications the issue did not cite.
#[tokio::test]
async fn the_ingestion_chain_reports_its_date_time_members_as_rfc3339_strings() {
    // (case, app, tenant, bucket)
    let rows: &[(&str, &str, &str, &str)] = &[
        ("a SLACK ingestion", "SLACK", "tenant-s", "audit-bucket-one"),
        ("a GOOGLE ingestion", "GOOGLE", "tenant-g", "audit-bucket-two"),
    ];

    for (case, app, tenant, bucket) in rows {
        let emulator = Emulator::new();
        let bundle = app_bundle(&emulator).await;

        let created = create_ingestion(&emulator, &bundle, app, tenant).await;
        assert_eq!(
            created.status, 201,
            "case {case:?}: expected HTTP 201 creating the ingestion, got {} with body {}",
            created.status, created.body
        );
        let reported = instants(case, &created, 2);
        let ingestion_created_at = member(case, &reported, "createdAt");
        let ingestion = arn_of(&created.body, "ingestion");

        let read = emulator.get_ingestion(&bundle, &ingestion).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: expected HTTP 200 reading the ingestion, got {} with body {}",
            read.status, read.body
        );
        assert_eq!(
            member(case, &instants(case, &read, 2), "createdAt"),
            ingestion_created_at,
            "case {case:?}: expected the creation instant the create call reported, got {}",
            read.body
        );

        let destination = create_destination(&emulator, &bundle, &ingestion, bucket).await;
        assert_eq!(
            destination.status, 201,
            "case {case:?}: expected HTTP 201 creating the destination, got {} with body {}",
            destination.status, destination.body
        );
        let reported = instants(case, &destination, 2);
        let destination_created_at = member(case, &reported, "createdAt");
        let destination_arn = destination
            .body
            .get("ingestionDestination")
            .and_then(|record| record.get("arn"))
            .and_then(Value::as_str)
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected ingestionDestination.arn in {}",
                    destination.body
                )
            })
            .to_string();

        let destination_uri = format!(
            "/appbundles/{}/ingestions/{}/ingestiondestinations/{}",
            encode(&bundle),
            encode(&ingestion),
            encode(&destination_arn)
        );
        let read = emulator.call("GET", &destination_uri, None).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: expected HTTP 200 reading the destination, got {} with body {}",
            read.status, read.body
        );
        assert_eq!(
            member(case, &instants(case, &read, 2), "createdAt"),
            destination_created_at,
            "case {case:?}: expected the creation instant the create call reported, got {}",
            read.body
        );

        let updated = emulator
            .call(
                "PATCH",
                &destination_uri,
                Some(json!({"destinationConfiguration": {
                    "auditLog": {"destination": {"firehoseStream": {"streamName": "audit-stream"}}},
                }})),
            )
            .await;
        assert_eq!(
            updated.status, 200,
            "case {case:?}: expected HTTP 200 updating the destination, got {} with body {}",
            updated.status, updated.body
        );
        let after = instants(case, &updated, 2);
        assert_eq!(
            member(case, &after, "createdAt"),
            destination_created_at,
            "case {case:?}: an update must leave the creation instant alone, got {}",
            updated.body
        );
        assert!(
            member(case, &after, "updatedAt") >= destination_created_at,
            "case {case:?}: expected the update instant not to precede the creation one, got \
             {after:?}"
        );
    }
}
