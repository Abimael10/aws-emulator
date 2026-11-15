// SPDX-License-Identifier: CC0-1.0

//! What `DeletePackage` answers for a package the addressed repository does not hold,
//! asserted end-to-end over the wire.
//!
//! Every case drives real `restJson1` requests through the `MultiServiceRouter` the
//! production emulator serves with (the shared `harness` module), so what is pinned here is
//! the status and the body a CodeArtifact client reads — not a handler's return value.
//!
//! The rules under test, each asserted over a TABLE of coordinates rather than one example:
//!
//! 1. The absence of the package a delete NAMES is the call's GOAL, not a precondition: a
//!    coordinate the addressed repository holds no package under answers HTTP 200 carrying
//!    no `deletedPackage` (`DeletePackageResult$deletedPackage` is optional —
//!    `smithy_query(query="operation",operation="DeletePackage")`), with no error shape, and
//!    changes nothing. That covers a coordinate never held, a repeat of a delete that
//!    already committed, and a held name addressed under another format or namespace — which
//!    addresses a different package.
//! 2. The SCOPE a delete is addressed to is a different matter: a domain this caller does not
//!    hold, and a repository the domain does not hold, are still the modelled
//!    `ResourceNotFoundException` (404) naming the kind that could not be resolved.
//! 3. The repeat is the END of the sequence a client sees: the first delete answers the
//!    `PackageSummary` of the record it removed, the package-addressing read then refuses
//!    with the package-scoped sentence, the version-addressing read with the
//!    coordinate-and-repository one, and a second identical delete succeeds carrying nothing.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::domain_repository_lifecycle::create_repository;
use crate::harness::{call, create_domain, emulator, Answer, ACCOUNT};
// The two requests that SEED a coordinate are the ones sibling suites already build:
// `PublishPackageVersion` carries its asset as the HTTP payload and its digest as a header,
// and `PutPackageOriginConfiguration` holds a coordinate with no version at all.
use crate::package_version_publish::{publish, Publish};
use crate::update_operations_persist::put_origin_configuration;

/// The domain every case lives in.
const DOMAIN: &str = "delete-repeat";

/// The repository every case addresses.
const REPOSITORY: &str = "Releases";

/// A well-formed 12-digit account that is NOT the one this emulator serves (`AccountId` is
/// modelled `len 12..12, pattern ^[0-9]{12}$`), used to address a domain this account does
/// not hold. Asserted distinct from [`ACCOUNT`] where it is used.
const FOREIGN_ACCOUNT: &str = "111122223333";

/// How a coordinate comes to be held, which is also what decides how many versions the
/// delete has to take with it.
///
/// `PublishPackageVersion` serves the `generic` format only ("Only generic packages can be
/// published using this API" — `smithy_query(query="operation",operation="PublishPackageVersion")`),
/// so every other format is held through `PutPackageOriginConfiguration`, which creates the
/// package record carrying no version at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Seeded {
    /// `PublishPackageVersion`, one call per version.
    GenericPublish,
    /// `PutPackageOriginConfiguration`: the package, and no version of it.
    OriginConfigurationOnly,
}

/// One coordinate a case addresses.
#[derive(Debug, Clone)]
struct Row {
    case: &'static str,
    format: &'static str,
    namespace: Option<String>,
    package: String,
    versions: &'static [&'static str],
    seeded: Seeded,
}

impl Row {
    /// The query members every package-scoped route carries for this coordinate, against
    /// `repository`.
    fn query(&self, repository: &str) -> String {
        self.query_in(repository, self.namespace.as_deref())
    }

    /// [`Row::query`] with `namespace` substituted for the row's own — how the same format
    /// and name are addressed under another namespace, or under none.
    fn query_in(&self, repository: &str, namespace: Option<&str>) -> String {
        let mut query = format!(
            "domain={DOMAIN}&repository={repository}&format={}&package={}",
            self.format, self.package
        );
        if let Some(namespace) = namespace {
            query.push_str(&format!("&namespace={namespace}"));
        }
        query
    }

    /// The sentence `DescribePackage` states once this coordinate is gone: the addressed
    /// format and package name only. Written out here rather than imported, so a wrong
    /// sentence in the implementation cannot make this suite pass.
    fn package_sentence(&self) -> String {
        format!(
            "Package not found in repository. Package = {{format={}, name={}}}",
            self.format, self.package
        )
    }

    /// The sentence the VERSION-addressing `ListPackageVersions` states for the same
    /// absence: the format, the name, the namespace where the request carried one, and the
    /// repository it looked in. Written out here for the same reason.
    fn versions_sentence(&self, repository: &str) -> String {
        match self.namespace.as_deref() {
            Some(namespace) => format!(
                "The {} package '{}' with namespace '{namespace}' does not exist in repository \
                 '{repository}'.",
                self.format, self.package
            ),
            None => format!(
                "The {} package '{}' does not exist in repository '{repository}'.",
                self.format, self.package
            ),
        }
    }
}

/// The modelled extremes of `PackageName` (len 1..255) and `PackageNamespace` (len 1..255),
/// so a row addresses a coordinate at each bound as well as an ordinary one.
fn longest_name() -> String {
    "p".repeat(255)
}

/// The coordinates every rule is asserted over: the formats whose packages carry a
/// namespace component and the ones whose do not (the `namespace` member documents maven,
/// swift and generic as requiring one, npm as carrying a scope, and Python, NuGet, Ruby and
/// Cargo as carrying none), seeded with 0, 1 and 2 versions, and addressed by names at both
/// modelled length bounds.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "generic, namespaced, two versions",
            format: "generic",
            namespace: Some("ops".to_string()),
            package: "toolchain".to_string(),
            versions: &["1.0.0", "2.0.0"],
            seeded: Seeded::GenericPublish,
        },
        Row {
            case: "generic, 255-character namespace, one version",
            format: "generic",
            namespace: Some("n".repeat(255)),
            package: "bundle".to_string(),
            versions: &["0.1.0"],
            seeded: Seeded::GenericPublish,
        },
        Row {
            case: "generic, one-character namespace and name, one version",
            format: "generic",
            namespace: Some("n".to_string()),
            package: "p".to_string(),
            versions: &["1.0.0"],
            seeded: Seeded::GenericPublish,
        },
        Row {
            case: "maven, namespaced, no versions",
            format: "maven",
            namespace: Some("com.acme.tools".to_string()),
            package: "widget-core".to_string(),
            versions: &[],
            seeded: Seeded::OriginConfigurationOnly,
        },
        Row {
            case: "npm, scoped, no versions",
            format: "npm",
            namespace: Some("acme".to_string()),
            package: "widgets".to_string(),
            versions: &[],
            seeded: Seeded::OriginConfigurationOnly,
        },
        Row {
            case: "pypi, no namespace, no versions",
            format: "pypi",
            namespace: None,
            package: "acme-widgets".to_string(),
            versions: &[],
            seeded: Seeded::OriginConfigurationOnly,
        },
        Row {
            case: "nuget, no namespace, 255-character name",
            format: "nuget",
            namespace: None,
            package: longest_name(),
            versions: &[],
            seeded: Seeded::OriginConfigurationOnly,
        },
    ]
}

// ---------------------------------------------------------------------------
// The requests this suite sends, each the modelled route of one operation.
// ---------------------------------------------------------------------------

/// `DeletePackage`: every member is `httpQuery`-bound, so the request carries no body.
async fn delete_package(router: &MultiServiceRouter, query: &str) -> Answer {
    call(router, "DELETE", &format!("/v1/package?{query}"), None).await
}

/// `DescribePackage` for one coordinate.
async fn describe_package(router: &MultiServiceRouter, query: &str) -> Answer {
    call(router, "GET", &format!("/v1/package?{query}"), None).await
}

/// `ListPackageVersions` for one coordinate.
async fn list_versions(router: &MultiServiceRouter, query: &str) -> Answer {
    call(
        router,
        "POST",
        &format!("/v1/package/versions?{query}"),
        Some(json!({})),
    )
    .await
}

/// The request that SEEDS one row's coordinate: the `Publish` both sibling builders take.
///
/// `PublishPackageVersion` sends its asset as the HTTP payload with the declared digest in
/// the `x-amz-content-sha256` header ([`publish`]), and `PutPackageOriginConfiguration` holds
/// a coordinate carrying no version at all ([`put_origin_configuration`]) — so this suite
/// restates neither request. `version` is the member the origin-configuration route does not
/// carry, and therefore ignores.
fn seed_request(row: &Row, repository: &str, version: &str) -> Publish {
    let content = format!("{} {version} payload", row.package).into_bytes();
    let mut request = Publish::new(&row.package, version, "bundle.bin", content);
    request.domain = DOMAIN.to_string();
    request.repository = repository.to_string();
    request.format = row.format.to_string();
    request.namespace = row.namespace.clone();
    request
}

/// Hold one row's coordinate in `repository`, however that row comes to exist.
async fn given_package(router: &MultiServiceRouter, row: &Row, repository: &str) {
    let case = row.case;
    match row.seeded {
        Seeded::GenericPublish => {
            for version in row.versions {
                let published = publish(router, &seed_request(row, repository, version)).await;
                assert_eq!(
                    published.status, 200,
                    "case {case:?}: PublishPackageVersion {version} must succeed, got \
                     {published:?}"
                );
            }
        }
        Seeded::OriginConfigurationOnly => {
            let created = put_origin_configuration(
                router,
                &seed_request(row, repository, "0.0.0"),
                "ALLOW",
                "ALLOW",
            )
            .await;
            assert_eq!(
                created.status, 200,
                "case {case:?}: PutPackageOriginConfiguration must succeed, got {created:?}"
            );
        }
    }
}

/// The domain and the repository every case addresses, with every row seeded into it.
async fn given_repository(router: &MultiServiceRouter) -> Vec<Row> {
    assert_eq!(
        create_domain(router, DOMAIN, None).await.status,
        200,
        "setup: CreateDomain must succeed"
    );
    let created = create_repository(router, DOMAIN, REPOSITORY, None, &[]).await;
    assert_eq!(
        created.status, 200,
        "setup: CreateRepository must succeed, got {created:?}"
    );
    let rows = rows();
    for row in &rows {
        given_package(router, row, REPOSITORY).await;
    }
    rows
}

/// The `PackageSummary` a successful delete must report, derived from the LAST read before
/// the call: the same projection `DescribePackage` answers with, whose `name` member a
/// summary spells `package` (`smithy_query(query="shape",shape="PackageSummary")`).
fn summary_of(described: &Value, case: &str) -> Value {
    let mut summary = described
        .as_object()
        .unwrap_or_else(|| panic!("case {case:?}: expected a package description, got {described:?}"))
        .clone();
    let name = summary
        .remove("name")
        .unwrap_or_else(|| panic!("case {case:?}: expected a package name, got {described:?}"));
    summary.insert("package".to_string(), name);
    Value::Object(summary)
}

/// RULE 1 and RULE 3: the whole sequence a client sees — the delete that removes the record
/// reports it, the reads then refuse with their own sentences, and the SECOND identical
/// delete succeeds carrying nothing.
#[tokio::test]
async fn a_repeated_package_delete_succeeds_and_reports_nothing() {
    let router = emulator();
    let rows = given_repository(&router).await;

    for row in &rows {
        let case = row.case;
        let query = row.query(REPOSITORY);

        // The last read before the delete: what the delete reports has to be this record.
        let before = describe_package(&router, &query).await;
        assert_eq!(
            before.status, 200,
            "case {case:?}: the seeded coordinate must read back, got {before:?}"
        );

        let first = delete_package(&router, &query).await;
        assert_eq!(
            (first.status, first.body["deletedPackage"].clone()),
            (200, summary_of(&before.body["package"], case)),
            "case {case:?}: the first delete must report the record it removed, got {first:?}"
        );

        let read = describe_package(&router, &query).await;
        assert_eq!(
            (read.status, read.code(), read.message()),
            (
                404,
                "ResourceNotFoundException".to_string(),
                row.package_sentence()
            ),
            "case {case:?}: the removed package must be gone from DescribePackage, got {read:?}"
        );
        let listed = list_versions(&router, &query).await;
        assert_eq!(
            (listed.status, listed.code(), listed.message()),
            (
                404,
                "ResourceNotFoundException".to_string(),
                row.versions_sentence(REPOSITORY)
            ),
            "case {case:?}: the removed package must be gone from ListPackageVersions, got \
             {listed:?}"
        );

        let again = delete_package(&router, &query).await;
        assert_eq!(
            (again.status, again.body.clone()),
            (200, json!({})),
            "case {case:?}: the second delete must succeed carrying no deletedPackage, got \
             {again:?}"
        );
        assert_eq!(
            again.code(),
            String::new(),
            "case {case:?}: a successful delete carries no error shape, got {again:?}"
        );

        // ...and the repeat changed nothing: the coordinate is still absent, by the same
        // sentence as before it.
        let still_gone = describe_package(&router, &query).await;
        assert_eq!(
            (still_gone.status, still_gone.message()),
            (404, row.package_sentence()),
            "case {case:?}: the repeat must leave the coordinate absent, got {still_gone:?}"
        );
    }
}

/// RULE 1: a coordinate the repository holds no package under is answered the same way
/// whether it was never held or is held under another format or namespace — and the package
/// that IS held survives every one of those calls, versions included.
#[tokio::test]
async fn deleting_a_coordinate_the_repository_does_not_hold_removes_nothing() {
    let router = emulator();
    let rows = given_repository(&router).await;

    // Each absent coordinate, as a (case, query) pair: a name never held at either modelled
    // length bound, a held name under another format, a held name under another namespace,
    // and a held namespaced name with its namespace omitted.
    let mut absent: Vec<(String, String)> = Vec::new();
    for row in &rows {
        let never_held = Row {
            case: "never held",
            package: format!("absent-{}", row.format),
            ..row.clone()
        };
        absent.push((
            format!("{}: a coordinate never held", row.case),
            never_held.query(REPOSITORY),
        ));
        let other_format = Row {
            format: if row.format == "nuget" {
                "ruby"
            } else {
                "nuget"
            },
            ..row.clone()
        };
        absent.push((
            format!("{}: the held name under another format", row.case),
            other_format.query(REPOSITORY),
        ));
        absent.push((
            format!("{}: the held name under another namespace", row.case),
            row.query_in(REPOSITORY, Some("not-the-namespace")),
        ));
        if row.namespace.is_some() {
            absent.push((
                format!("{}: the held name with its namespace omitted", row.case),
                row.query_in(REPOSITORY, None),
            ));
        }
    }
    // A one-character and a 255-character name, neither ever held, so the bounds of
    // `PackageName` are covered by a row of their own.
    for name in ["x", longest_name().as_str()] {
        absent.push((
            format!("a {}-character name never held", name.len()),
            format!(
                "domain={DOMAIN}&repository={REPOSITORY}&format=cargo&package={name}"
            ),
        ));
    }

    for (case, query) in &absent {
        let answer = delete_package(&router, query).await;
        assert_eq!(
            (answer.status, answer.body.clone()),
            (200, json!({})),
            "case {case:?}: deleting what the repository does not hold must succeed with no \
             removed record, got {answer:?}"
        );
        assert_eq!(
            answer.code(),
            String::new(),
            "case {case:?}: a successful delete carries no error shape, got {answer:?}"
        );
    }

    // None of those calls removed anything: every seeded coordinate still reads back, with
    // every version it was published with.
    for row in &rows {
        let case = row.case;
        let query = row.query(REPOSITORY);
        let read = describe_package(&router, &query).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: a delete of another coordinate must remove nothing, got {read:?}"
        );
        let listed = list_versions(&router, &query).await;
        let held: Vec<&str> = listed.body["versions"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: expected a versions list, got {listed:?}"))
            .iter()
            .filter_map(|version| version["version"].as_str())
            .collect();
        for version in row.versions {
            assert!(
                held.contains(version),
                "case {case:?}: version {version} must survive, got {held:?}"
            );
        }
        assert_eq!(
            held.len(),
            row.versions.len(),
            "case {case:?}: expected exactly {} versions, got {held:?}",
            row.versions.len()
        );
    }
}

/// RULE 2: the scope a delete is addressed to still has to resolve — only the package's own
/// absence is success.
#[tokio::test]
async fn the_scope_a_package_delete_addresses_must_still_resolve() {
    let router = emulator();
    let rows = given_repository(&router).await;
    let row = rows.first().expect("at least one row");
    assert_ne!(
        FOREIGN_ACCOUNT, ACCOUNT,
        "the wrong-owner case needs an account the emulator does not present"
    );

    let scopes = [
        (
            "a domain that is not held",
            format!(
                "domain=no-such-domain&repository={REPOSITORY}&format={}&package={}",
                row.format, row.package
            ),
            "domain",
        ),
        (
            "a domain pinned to another account",
            format!("{}&domain-owner={FOREIGN_ACCOUNT}", row.query(REPOSITORY)),
            "domain",
        ),
        (
            "a repository that is not held",
            row.query("NoSuchRepo"),
            "repository",
        ),
    ];

    for (case, query, resource_type) in &scopes {
        let refused = delete_package(&router, query).await;
        assert_eq!(
            (refused.status, refused.code()),
            (404, "ResourceNotFoundException".to_string()),
            "case {case:?}: an unresolvable scope must be not found, got {refused:?}"
        );
        assert_eq!(
            refused.body["resourceType"].as_str(),
            Some(*resource_type),
            "case {case:?}: the refusal must name the kind it could not resolve, got {refused:?}"
        );
    }

    // The scope refusals removed nothing.
    let intact = describe_package(&router, &row.query(REPOSITORY)).await;
    assert_eq!(
        intact.status, 200,
        "the held coordinate must survive every scope refusal, got {intact:?}"
    );
}
