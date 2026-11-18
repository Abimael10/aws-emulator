// SPDX-License-Identifier: CC0-1.0

//! What a `RegisterType` schema handler package has to CARRY for the registration to register
//! a version, end to end over the awsQuery wire.
//!
//! The rule every case here holds the service to: **a registration is accepted and then
//! validates the extension project package it retrieved — the schema handler package is "the
//! extension project package that contains the necessary files for the extension you want to
//! register" (`RegisterTypeInput$SchemaHandlerPackage`) and registering "includes determining
//! which handlers, if any, have been specified for the extension" and "making the extension
//! available for use in your account" (`RegisterType`) — so for an extension kind whose
//! extensions are RUN by handlers, a resource type or a Hook, a package carrying the schema
//! and no handler artifact concludes the request `FAILED`, with the reason described and no
//! version and no record written; a MODULE, which has no handlers at all, needs no artifact
//! and registers from the same package.**
//!
//! The table's rows differ along what the rule generalises over: the extension KIND the
//! request names (`RESOURCE`, `HOOK`, `MODULE`, and the request that names none, which
//! registers a resource type) crossed with what the package CARRIES (the project package
//! `cfn submit` builds, which has the built handler artifact beside the schema and the
//! project's documents; and the schema document alone).
//!
//! `extension_registrations.rs` is the table for what a registration WRITES, and the suite
//! this one borrows its package, staging and monitoring helpers from, so both halves of the
//! rule are driven through one package builder.

use crate::extension_registrations::{
    concluded_registration, create_package_bucket, describe_registration, describe_type,
    package_of, package_of_the_schema_alone, register_ok, schema_of, stage, Style,
    EVERY_HANDLER, HANDLER_ARTIFACT,
};
use crate::harness::{refusal, router_with_engine};
use service_com_amazonaws_cloudformation_cloudformation::state::registry::{
    registration_status, registry_type,
};

/// The error code the registry answers a request naming an extension it does not hold
/// (`TypeNotFoundException`'s `awsQueryError`), which is what "nothing was registered" reads
/// as.
const NOT_FOUND_CODE: &str = "TypeNotFoundException";

/// What the package a row stages carries.
#[derive(Clone, Copy, Debug)]
enum Package {
    /// The project package a client builds with `cfn submit`: the schema document, the
    /// project's own documents, and the built handler artifact.
    Project,
    /// The schema document and nothing else — no handler code for the handlers it declares.
    SchemaAlone,
}

/// One row of the package-contents table.
struct Row {
    case: &'static str,
    /// The kind the request names, or `None` for a request that names none at all.
    type_kind: Option<&'static str>,
    /// The kind the registry reports, which is the resource type a request naming no kind
    /// registers.
    reported_kind: &'static str,
    type_name: &'static str,
    package: Package,
    /// The status the request must conclude in.
    conclusion: &'static str,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a resource type from the project package",
            type_kind: Some(registry_type::RESOURCE),
            reported_kind: registry_type::RESOURCE,
            type_name: "My::Packaged::Resource",
            package: Package::Project,
            conclusion: registration_status::COMPLETE,
        },
        Row {
            case: "a resource type from the schema document alone",
            type_kind: Some(registry_type::RESOURCE),
            reported_kind: registry_type::RESOURCE,
            type_name: "My::Schemaonly::Resource",
            package: Package::SchemaAlone,
            conclusion: registration_status::FAILED,
        },
        Row {
            case: "a request naming no kind, from the schema document alone",
            type_kind: None,
            reported_kind: registry_type::RESOURCE,
            type_name: "My::Schemaonly::Default",
            package: Package::SchemaAlone,
            conclusion: registration_status::FAILED,
        },
        Row {
            case: "a Hook from the project package",
            type_kind: Some(registry_type::HOOK),
            reported_kind: registry_type::HOOK,
            type_name: "My::Packaged::Hook",
            package: Package::Project,
            conclusion: registration_status::COMPLETE,
        },
        Row {
            case: "a Hook from the schema document alone",
            type_kind: Some(registry_type::HOOK),
            reported_kind: registry_type::HOOK,
            type_name: "My::Schemaonly::Hook",
            package: Package::SchemaAlone,
            conclusion: registration_status::FAILED,
        },
        Row {
            case: "a module from the schema document alone, which has no handlers to run",
            type_kind: Some(registry_type::MODULE),
            reported_kind: registry_type::MODULE,
            type_name: "My::Schemaonly::Thing::MODULE",
            package: Package::SchemaAlone,
            conclusion: registration_status::COMPLETE,
        },
        Row {
            case: "a module from the project package",
            type_kind: Some(registry_type::MODULE),
            reported_kind: registry_type::MODULE,
            type_name: "My::Packaged::Thing::MODULE",
            package: Package::Project,
            conclusion: registration_status::COMPLETE,
        },
    ]
}

#[tokio::test]
async fn a_registration_needs_the_handler_artifact_exactly_where_handlers_are_run() {
    let (router, _engine) = router_with_engine();
    create_package_bucket(&router).await;

    // Every row's package is staged and its registration started first — the conclusion is
    // owed at an instant, so the rows run side by side rather than one span each — and every
    // request is ACCEPTED whatever its package carries: the package was retrieved, and what
    // it carries is the registration's own business.
    let mut tokens: Vec<String> = Vec::new();
    for row in rows() {
        let schema = schema_of(row.type_name, EVERY_HANDLER);
        let package = match row.package {
            Package::Project => package_of(Some(&schema)),
            Package::SchemaAlone => package_of_the_schema_alone(&schema),
        };
        let key = format!("{}.zip", row.type_name.replace("::", "-"));
        stage(&router, &key, &package).await;
        let token = register_ok(
            row.case,
            &router,
            row.type_kind,
            row.type_name,
            &Style::S3Uri.url(&key),
            &[],
        )
        .await;
        let monitored = describe_registration(row.case, &router, &token).await;
        assert_eq!(
            monitored.progress,
            registration_status::IN_PROGRESS,
            "case {:?}: a registration whose conclusion is not due is still running, got {:?}",
            row.case,
            monitored.progress
        );
        tokens.push(token);
    }

    for (row, token) in rows().iter().zip(tokens.iter()) {
        let monitored = concluded_registration(row.case, &router, token).await;
        assert_eq!(
            monitored.progress, row.conclusion,
            "case {:?}: expected the request to conclude {:?}, got {:?} ({:?})",
            row.case, row.conclusion, monitored.progress, monitored.description
        );
        let (status, body) = describe_type(&router, row.reported_kind, row.type_name, None).await;
        if row.conclusion == registration_status::FAILED {
            // "The description of the extension registration request": a request that failed
            // is described by WHY it could not register the version, which here is the
            // artifact the package did not carry.
            let description = monitored.description.clone().unwrap_or_default();
            let names_the_cause = format!(
                "handler artifact is missing from the schema handler package for {}",
                row.type_name
            );
            assert!(
                description.contains(&names_the_cause),
                "case {:?}: expected the description to name {names_the_cause:?}, got \
                 {description:?}",
                row.case
            );
            assert_eq!(
                (
                    monitored.type_arn.as_deref(),
                    monitored.type_version_arn.as_deref()
                ),
                (None, None),
                "case {:?}: a registration other than COMPLETE reports neither ARN, got {:?} \
                 and {:?}",
                row.case,
                monitored.type_arn,
                monitored.type_version_arn
            );
            // Nothing was registered: the extension is not in this Region's registry at all.
            assert_eq!(
                status, 404,
                "case {:?}: expected no record for a failed registration, got {status} {body}",
                row.case
            );
            let (code, _) = refusal(&body);
            assert_eq!(
                code, NOT_FOUND_CODE,
                "case {:?}: expected {NOT_FOUND_CODE}, got {code:?} in {body}",
                row.case
            );
        } else {
            // The version the registration added, read back where a client reads it.
            assert!(
                monitored.type_version_arn.is_some(),
                "case {:?}: expected the registered version's ARN, got {:?}",
                row.case,
                monitored.type_version_arn
            );
            assert_eq!(
                status, 200,
                "case {:?}: expected the registered extension to be readable, got {status} \
                 {body}",
                row.case
            );
        }
    }
}

#[tokio::test]
async fn the_handler_artifact_is_what_the_package_carries_beside_its_own_documents() {
    let (router, _engine) = router_with_engine();
    create_package_bucket(&router).await;

    // The same resource type, registered twice: once from a package carrying the schema and
    // the project's documents and nothing else, and once from that package with the built
    // handler artifact added. What differs between the two registrations is the artifact
    // alone, so it is what the conclusion follows from.
    let type_name = "My::Artifact::Resource";
    let schema = schema_of(type_name, EVERY_HANDLER);
    let documents_only = package_of_the_schema_alone(&schema);
    let project = package_of(Some(&schema));
    assert!(
        project.len() > documents_only.len(),
        "the project package must carry {HANDLER_ARTIFACT} beside the documents, got {} bytes \
         against {}",
        project.len(),
        documents_only.len()
    );
    stage(&router, "artifactless.zip", &documents_only).await;
    stage(&router, "with-artifact.zip", &project).await;

    for (case, key, conclusion) in [
        (
            "the package without the handler artifact",
            "artifactless.zip",
            registration_status::FAILED,
        ),
        (
            "the same package with the handler artifact",
            "with-artifact.zip",
            registration_status::COMPLETE,
        ),
    ] {
        let token = register_ok(
            case,
            &router,
            Some(registry_type::RESOURCE),
            type_name,
            &Style::S3Uri.url(key),
            &[],
        )
        .await;
        let monitored = concluded_registration(case, &router, &token).await;
        assert_eq!(
            monitored.progress, conclusion,
            "case {case:?}: expected the request to conclude {conclusion:?}, got {:?} ({:?})",
            monitored.progress, monitored.description
        );
    }

    // The second registration is the one that wrote a version, so the record the registry
    // holds for the extension came from the package that carried the artifact.
    let (status, body) = describe_type(&router, registry_type::RESOURCE, type_name, None).await;
    assert_eq!(
        status, 200,
        "expected the extension the artifact-carrying package registered to be readable, got \
         {status} {body}"
    );
}
