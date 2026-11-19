// SPDX-License-Identifier: CC0-1.0

//! `GetPackageVersionReadme`: the README of a package version, end to end over the wire.
//!
//! The rule under test, over a TABLE rather than one example:
//!
//! 1. The readme a version reports is the text its PUBLISH carried, decoded as the UTF-8 it
//!    arrived as — so a multi-byte readme (CJK, Cyrillic, accented Latin, emoji) round-trips
//!    unchanged. The rows span where that text travels: inline in the npm publish document,
//!    inside the published tarball when the document declares none, and the long description
//!    a PyPI upload sends.
//! 2. The answer reports the coordinate of the version it read, with the same
//!    `versionRevision` `DescribePackageVersion` reports for it.
//! 3. A coordinate the repository does not hold is refused `ResourceNotFoundException`
//!    (404), naming what was missing.
//! 4. The readme is a DOCUMENT of the version, not an optional label on it: a version whose
//!    publish carried none holds no readme to read, so the read is refused
//!    `ResourceNotFoundException` (404) with the service's fixed sentence rather than
//!    answered `200` with the member omitted — whatever format the version was published
//!    in, and whether or not it sits under a namespace.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::domain_repository_lifecycle::create_repository;
use crate::harness::{call, create_domain, emulator, Answer};
use crate::npm_registry_endpoint::{endpoint_host, registry, token_for};
use crate::pypi_data_plane::{
    basic_credential, data_plane, sha256_hex, upload_form, upload_media_type,
};

/// A gzip-framed tar carrying one file at `path`, which is what an npm publish attaches.
fn tarball(path: &str, content: &[u8]) -> Vec<u8> {
    use std::io::Write as _;

    let mut builder = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_size(content.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    builder
        .append_data(&mut header, path, content)
        .expect("append the entry");
    let packed = builder.into_inner().expect("finish the archive");
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(&packed).expect("compress the archive");
    encoder.finish().expect("finish the compression")
}

/// The npm publish document `npm publish` sends for one version: the manifest, with the
/// `readme` member when the publisher declared one, and the tarball attached base64-encoded.
fn publish_document(package: &str, version: &str, readme: Option<&str>, tarball: &[u8]) -> Vec<u8> {
    use base64::Engine as _;

    let mut manifest = json!({
        "name": package,
        "version": version,
        "description": "a package with documentation",
    });
    if let Some(readme) = readme {
        manifest["readme"] = Value::String(readme.to_string());
    }
    serde_json::to_vec(&json!({
        "_id": package,
        "name": package,
        "dist-tags": { "latest": version },
        "versions": { version: manifest },
        "_attachments": {
            format!("{package}-{version}.tgz"): {
                "content_type": "application/octet-stream",
                "data": base64::engine::general_purpose::STANDARD.encode(tarball),
                "length": tarball.len(),
            }
        },
    }))
    .expect("serialize the publish document")
}

/// The `namespace` query component a coordinate carries, empty for a package whose format
/// has no namespace component or that was published without one. Percent-encoded with the
/// suite-wide helper, so a namespace carrying reserved characters (a Maven groupId, a scope
/// written with a separator) survives the server's decode.
fn namespace_component(namespace: Option<&str>) -> String {
    namespace.map_or_else(String::new, |namespace| {
        format!("&namespace={}", crate::resource_tags::encode_query(namespace))
    })
}

/// `GetPackageVersionReadme` over the control plane, for a coordinate with or without a
/// namespace.
async fn get_readme(
    router: &MultiServiceRouter,
    domain: &str,
    repository: &str,
    format: &str,
    namespace: Option<&str>,
    package: &str,
    version: &str,
) -> Answer {
    call(
        router,
        "GET",
        &format!(
            "/v1/package/version/readme?domain={domain}&repository={repository}&format={format}\
             &package={package}&version={version}{}",
            namespace_component(namespace)
        ),
        None,
    )
    .await
}

/// `DescribePackageVersion` over the control plane, for the revision the readme answer must
/// agree with.
async fn describe_version(
    router: &MultiServiceRouter,
    domain: &str,
    repository: &str,
    format: &str,
    namespace: Option<&str>,
    package: &str,
    version: &str,
) -> Answer {
    call(
        router,
        "GET",
        &format!(
            "/v1/package/version?domain={domain}&repository={repository}&format={format}\
             &package={package}&version={version}{}",
            namespace_component(namespace)
        ),
        None,
    )
    .await
}

/// A domain with one repository, and a token for it.
async fn given_repository(
    router: &MultiServiceRouter,
    domain: &str,
    repository: &str,
) -> String {
    assert_eq!(
        create_domain(router, domain, None).await.status,
        200,
        "CreateDomain {domain:?} must succeed"
    );
    assert_eq!(
        create_repository(router, domain, repository, None, &[]).await.status,
        200,
        "CreateRepository {repository:?} must succeed"
    );
    token_for(router, domain).await
}

/// The `readme` of an answer, or `None` when it carries none.
fn readme_of(answer: &Answer) -> Option<String> {
    answer
        .body
        .get("readme")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// One row of the npm readme table: the case, the package, the readme the document declares
/// inline, the readme packed in the tarball, and the text the API must report.
type NpmRow = (
    &'static str,
    &'static str,
    Option<&'static str>,
    Option<&'static str>,
    &'static str,
);

/// RULES 1 and 2: the readme an npm publish carried is reported byte for byte after UTF-8
/// decoding, whichever way the publish carried it, beside the version's own coordinate and
/// revision.
#[tokio::test]
async fn the_readme_an_npm_publish_carried_is_reported_unchanged() {
    let router = emulator();
    let domain = "ca-readme";
    let repository = "documented";
    let token = given_repository(&router, domain, repository).await;
    let host = endpoint_host(domain);
    let version = "1.0.0";

    // The rows span what the rule generalises over: where the text travelled, and every
    // multi-byte script a README carries.
    let rows: [NpmRow; 5] = [
        (
            "CJK declared inline",
            "readme-cjk",
            Some("# 概要\nこのパッケージは日本語の README を持つ。"),
            None,
            "# 概要\nこのパッケージは日本語の README を持つ。",
        ),
        (
            "Cyrillic declared inline",
            "readme-cyrillic",
            Some("# Обзор\nПривет, мир!"),
            None,
            "# Обзор\nПривет, мир!",
        ),
        (
            "accented Latin and emoji declared inline",
            "readme-emoji",
            Some("# Café ☕\nShip it 🚀 — naïve façade"),
            None,
            "# Café ☕\nShip it 🚀 — naïve façade",
        ),
        (
            "declared only inside the tarball",
            "readme-packed",
            None,
            Some("# Packed\nünïcode from package/README.md"),
            "# Packed\nünïcode from package/README.md",
        ),
        (
            "declared both ways, where the document is the publisher's own declaration",
            "readme-both",
            Some("# Inline wins"),
            Some("# Packed loses"),
            "# Inline wins",
        ),
    ];

    for (case, package, inline, packed, expected) in rows {
        let archive = match packed {
            Some(text) => tarball("package/README.md", text.as_bytes()),
            None => tarball("package/index.js", b"module.exports = 1;"),
        };
        let published = registry(
            &router,
            "PUT",
            &host,
            &format!("/npm/{repository}/{package}"),
            Some(&token),
            Some(publish_document(package, version, inline, &archive)),
        )
        .await;
        assert_eq!(
            published.status, 200,
            "case {case:?}: the npm publish must succeed, got {published:?}"
        );

        let answer =
            get_readme(&router, domain, repository, "npm", None, package, version).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected HTTP 200, got {answer:?}"
        );
        assert_eq!(
            readme_of(&answer).as_deref(),
            Some(expected),
            "case {case:?}: expected the readme {expected:?}, got {:?}",
            readme_of(&answer)
        );
        // RULE 2: the coordinate and the revision of the version that was read.
        for (member, wanted) in [
            ("format", "npm"),
            ("package", package),
            ("version", version),
        ] {
            assert_eq!(
                answer.body.get(member).and_then(Value::as_str),
                Some(wanted),
                "case {case:?}: expected {member} {wanted:?}, got {answer:?}"
            );
        }
        let described =
            describe_version(&router, domain, repository, "npm", None, package, version).await;
        assert_eq!(
            answer.body.get("versionRevision").and_then(Value::as_str),
            described
                .body
                .pointer("/packageVersion/revision")
                .and_then(Value::as_str),
            "case {case:?}: expected the revision DescribePackageVersion reports, got \
             {answer:?}"
        );
    }
}

/// One row of the namespaced table: the case, the scope the publish wrote (`None` for an
/// unscoped publish of the same name), and the readme that publish declared inline.
type ScopedRow = (&'static str, Option<&'static str>, &'static str);

/// RULES 1 and 2 where the version sits under a NAMESPACE, which for npm is its scope: a
/// package is its whole `(format, namespace, name)` coordinate, so one name published under
/// several scopes keeps each publish's own readme, reported beside the namespace it was
/// stored under. Reading it again answers the same thing, because reporting a readme moves
/// nothing.
#[tokio::test]
async fn a_namespaced_version_reports_its_own_readme_and_namespace() {
    let router = emulator();
    let domain = "ca-readme-scoped";
    let repository = "documented";
    let token = given_repository(&router, domain, repository).await;
    let host = endpoint_host(domain);
    // ONE name, published under scopes and bare: each row's readme must come back only for
    // the coordinate that carried it.
    let package = "widget";
    let version = "2.1.0";
    let rows: [ScopedRow; 3] = [
        ("under one scope", Some("acme"), "# acme widget\n説明 ☕"),
        ("under another scope", Some("globex"), "# globex widget\nОбзор"),
        ("with no scope at all", None, "# bare widget\nplain"),
    ];

    for (case, namespace, readme) in rows {
        let display_name = match namespace {
            Some(scope) => format!("@{scope}/{package}"),
            None => package.to_string(),
        };
        // npm addresses a scoped package in ONE path segment, its separator encoded.
        let path = match namespace {
            Some(scope) => format!("/npm/{repository}/@{scope}%2f{package}"),
            None => format!("/npm/{repository}/{package}"),
        };
        let archive = tarball("package/index.js", b"module.exports = 1;");
        let published = registry(
            &router,
            "PUT",
            &host,
            &path,
            Some(&token),
            Some(publish_document(
                &display_name,
                version,
                Some(readme),
                &archive,
            )),
        )
        .await;
        assert_eq!(
            published.status, 200,
            "case {case:?}: the npm publish of {display_name:?} must succeed, got {published:?}"
        );
    }

    for (case, namespace, readme) in rows {
        let answer =
            get_readme(&router, domain, repository, "npm", namespace, package, version).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected HTTP 200, got {answer:?}"
        );
        assert_eq!(
            readme_of(&answer).as_deref(),
            Some(readme),
            "case {case:?}: expected the readme {readme:?}, got {:?}",
            readme_of(&answer)
        );
        // RULE 2: the namespace the version is stored under, reported for a scoped version
        // and absent for one published without a scope.
        assert_eq!(
            answer.body.get("namespace").and_then(Value::as_str),
            namespace,
            "case {case:?}: expected the namespace {namespace:?}, got {answer:?}"
        );
        assert_eq!(
            answer.body.get("package").and_then(Value::as_str),
            Some(package),
            "case {case:?}: expected the package {package:?}, got {answer:?}"
        );

        // RULE 2, for a namespaced coordinate too: the version it read, at the revision
        // `DescribePackageVersion` reports for the same coordinate.
        assert_eq!(
            answer.body.get("version").and_then(Value::as_str),
            Some(version),
            "case {case:?}: expected the version {version:?}, got {answer:?}"
        );
        let described =
            describe_version(&router, domain, repository, "npm", namespace, package, version)
                .await;
        assert_eq!(
            answer.body.get("versionRevision").and_then(Value::as_str),
            described
                .body
                .pointer("/packageVersion/revision")
                .and_then(Value::as_str),
            "case {case:?}: expected the revision DescribePackageVersion reports, got {answer:?}"
        );

        // Reporting only: a second read answers exactly what the first one did.
        let again =
            get_readme(&router, domain, repository, "npm", namespace, package, version).await;
        assert_eq!(
            again.body, answer.body,
            "case {case:?}: expected a second read to answer {:?}, got {:?}",
            answer.body, again.body
        );
    }
}

/// RULE 1, for the other format whose publish carries documentation: the long description a
/// PyPI upload sends is the readme the API reports.
#[tokio::test]
async fn the_long_description_a_pypi_upload_sent_is_its_readme() {
    let router = emulator();
    let domain = "ca-readme-pypi";
    let repository = "documented";
    let token = given_repository(&router, domain, repository).await;
    let host = endpoint_host(domain);

    // (case, the project as the build spells it, the normalised name, the description)
    let rows: [(&str, &str, &str, &str); 2] = [
        (
            "a multi-byte long description",
            "sim_doc",
            "sim-doc",
            "# Описание\n説明 — café ☕",
        ),
        (
            "a plain long description",
            "Sim.Doc.Two",
            "sim-doc-two",
            "# Overview\nplain ASCII",
        ),
    ];
    for (case, declared, normalized, description) in rows {
        let content = format!("wheel of {declared}").into_bytes();
        let uploaded = data_plane(
            &router,
            "POST",
            &host,
            &format!("/pypi/{repository}/"),
            &[
                ("authorization", basic_credential(&token)),
                ("content-type", upload_media_type()),
            ],
            upload_form(
                declared,
                "1.0.0",
                &format!("{declared}-1.0.0-py3-none-any.whl"),
                &content,
                Some(description),
                Some(&sha256_hex(&content)),
            ),
        )
        .await;
        assert_eq!(
            uploaded.status, 201,
            "case {case:?}: the upload must succeed, got {uploaded:?}"
        );
        let answer =
            get_readme(&router, domain, repository, "pypi", None, normalized, "1.0.0").await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected HTTP 200, got {answer:?}"
        );
        assert_eq!(
            readme_of(&answer).as_deref(),
            Some(description),
            "case {case:?}: expected the readme {description:?}, got {:?}",
            readme_of(&answer)
        );
    }
}

/// RULE 3: a coordinate the repository does not hold is refused `404`, naming what was
/// missing.
#[tokio::test]
async fn a_coordinate_the_repository_does_not_hold_is_refused() {
    let router = emulator();
    let domain = "ca-readme-refusals";
    let repository = "documented";
    let token = given_repository(&router, domain, repository).await;
    let host = endpoint_host(domain);
    let package = "readme-present";
    let archive = tarball("package/README.md", b"# present");
    assert_eq!(
        registry(
            &router,
            "PUT",
            &host,
            &format!("/npm/{repository}/{package}"),
            Some(&token),
            Some(publish_document(package, "1.0.0", None, &archive)),
        )
        .await
        .status,
        200,
        "the npm publish must succeed"
    );

    // (case, the domain, the repository, the package, the version, the sentence fragment)
    let rows: [(&str, &str, &str, &str, &str, &str); 4] = [
        (
            "a domain this service does not hold",
            "ca-no-such-domain",
            repository,
            package,
            "1.0.0",
            "Domain not found",
        ),
        (
            "a repository the domain does not hold",
            domain,
            "no-such-repository",
            package,
            "1.0.0",
            "Repository not found",
        ),
        (
            "a package the repository does not hold",
            domain,
            repository,
            "no-such-package",
            "1.0.0",
            "does not exist in repository",
        ),
        (
            "a version the package does not hold",
            domain,
            repository,
            package,
            "9.9.9",
            "Package version not found in repo",
        ),
    ];
    for (case, in_domain, in_repository, for_package, for_version, fragment) in rows {
        let answer = get_readme(
            &router,
            in_domain,
            in_repository,
            "npm",
            None,
            for_package,
            for_version,
        )
        .await;
        assert_eq!(
            answer.status, 404,
            "case {case:?}: expected HTTP 404, got {answer:?}"
        );
        assert_eq!(
            answer.code(),
            "ResourceNotFoundException",
            "case {case:?}: expected ResourceNotFoundException, got {answer:?}"
        );
        assert!(
            answer.message().contains(fragment),
            "case {case:?}: expected a sentence carrying {fragment:?}, got {:?}",
            answer.message()
        );
    }
}

/// The sentence CodeArtifact states for a version that carries no readme, spelled out here
/// rather than read from the implementation, so the wording itself is under test.
///
/// Entirely fixed text: it names neither the version nor the repository, so there is no
/// variable part for a row to vary.
const NO_README_MESSAGE: &str = "The readme file of this package version is not found";

/// How one row of the no-readme table published its version: the three publish routes this
/// emulator serves, which is where a version without a readme can come from at all.
enum PublishedWithoutReadme {
    /// `PublishPackageVersion`, the control-plane publish of a `generic` version: it carries
    /// an asset and never a readme.
    Generic,
    /// An npm publish document declaring no `readme`, whose tarball holds no `README`
    /// either, optionally under a scope.
    Npm { scope: Option<&'static str> },
    /// A PyPI upload sending no long description.
    PyPi,
}

/// RULE 4: a version whose publish carried no readme has no readme to read, so the read is
/// refused `ResourceNotFoundException` (404) with the service's fixed sentence — never a
/// `200` whose `readme` member is simply missing.
///
/// The rows span what the rule generalises over: the FORMAT the version was published in
/// (`generic`, `npm`, `pypi`), the publish ROUTE the version came in through (the
/// control-plane publish, an npm publish document, a PyPI upload), and whether the version
/// sits under a namespace. Each row's version is read back through
/// `DescribePackageVersion` first, so the 404 is about the readme and not about the version.
#[tokio::test]
async fn a_version_that_carries_no_readme_is_refused() {
    use crate::package_version_publish::{publish, Publish};

    let router = emulator();
    let domain = "ca-readme-none";
    let repository = "documented";
    let token = given_repository(&router, domain, repository).await;
    let host = endpoint_host(domain);
    let version = "1.0.0";

    // The one place the service states this sentence says the same thing, so a re-spelling
    // on either side is caught rather than passing silently.
    assert_eq!(
        service_com_amazonaws_codeartifact_codeartifactcontrolplaneservice::state::PACKAGE_VERSION_README_NOT_FOUND_MESSAGE,
        NO_README_MESSAGE,
        "the sentence the service states must be the one CodeArtifact states"
    );

    // (case, how it was published, the format it is addressed in, its namespace, its name)
    let rows: [(&str, PublishedWithoutReadme, &str, Option<&str>, &str); 4] = [
        (
            "a generic version published through the control plane",
            PublishedWithoutReadme::Generic,
            "generic",
            Some("team-core"),
            "generic-undocumented",
        ),
        (
            "an npm publish declaring no readme and packing none",
            PublishedWithoutReadme::Npm { scope: None },
            "npm",
            None,
            "npm-undocumented",
        ),
        (
            "the same npm publish under a scope",
            PublishedWithoutReadme::Npm {
                scope: Some("acme"),
            },
            "npm",
            Some("acme"),
            "npm-scoped-undocumented",
        ),
        (
            "a PyPI upload sending no long description",
            PublishedWithoutReadme::PyPi,
            "pypi",
            None,
            "pypi-undocumented",
        ),
    ];

    for (case, route, format, namespace, package) in rows {
        match route {
            PublishedWithoutReadme::Generic => {
                let mut request =
                    Publish::new(package, version, "payload.bin", b"generic bytes".to_vec());
                request.domain = domain.to_string();
                request.repository = repository.to_string();
                request.namespace = namespace.map(str::to_string);
                let published = publish(&router, &request).await;
                assert_eq!(
                    published.status, 200,
                    "case {case:?}: the generic publish must succeed, got {published:?}"
                );
            }
            PublishedWithoutReadme::Npm { scope } => {
                let (display_name, path) = match scope {
                    Some(scope) => (
                        format!("@{scope}/{package}"),
                        format!("/npm/{repository}/@{scope}%2f{package}"),
                    ),
                    None => (
                        package.to_string(),
                        format!("/npm/{repository}/{package}"),
                    ),
                };
                // No `readme` in the document and no `README` in the tarball: the publish
                // carried none either way.
                let archive = tarball("package/index.js", b"module.exports = 1;");
                let published = registry(
                    &router,
                    "PUT",
                    &host,
                    &path,
                    Some(&token),
                    Some(publish_document(&display_name, version, None, &archive)),
                )
                .await;
                assert_eq!(
                    published.status, 200,
                    "case {case:?}: the npm publish must succeed, got {published:?}"
                );
            }
            PublishedWithoutReadme::PyPi => {
                let content = format!("wheel of {package}").into_bytes();
                let uploaded = data_plane(
                    &router,
                    "POST",
                    &host,
                    &format!("/pypi/{repository}/"),
                    &[
                        ("authorization", basic_credential(&token)),
                        ("content-type", upload_media_type()),
                    ],
                    upload_form(
                        package,
                        version,
                        &format!("{package}-{version}-py3-none-any.whl"),
                        &content,
                        None,
                        Some(&sha256_hex(&content)),
                    ),
                )
                .await;
                assert_eq!(
                    uploaded.status, 201,
                    "case {case:?}: the PyPI upload must succeed, got {uploaded:?}"
                );
            }
        }

        // The version itself exists, so the refusal below cannot be about the version.
        let described =
            describe_version(&router, domain, repository, format, namespace, package, version)
                .await;
        assert_eq!(
            described.status, 200,
            "case {case:?}: the published version must be readable, got {described:?}"
        );

        let answer =
            get_readme(&router, domain, repository, format, namespace, package, version).await;
        assert_eq!(
            answer.status, 404,
            "case {case:?}: expected HTTP 404 for a version with no readme, got {answer:?}"
        );
        assert_eq!(
            answer.code(),
            "ResourceNotFoundException",
            "case {case:?}: expected ResourceNotFoundException, got {answer:?}"
        );
        assert_eq!(
            answer.message(),
            NO_README_MESSAGE,
            "case {case:?}: expected the sentence {NO_README_MESSAGE:?}, got {:?}",
            answer.message()
        );
        assert!(
            readme_of(&answer).is_none(),
            "case {case:?}: a refusal carries no readme, got {answer:?}"
        );

        // Reporting only: the refusal is stable, so a second read answers exactly the same.
        let again =
            get_readme(&router, domain, repository, format, namespace, package, version).await;
        assert_eq!(
            (again.status, again.code(), again.message()),
            (answer.status, answer.code(), answer.message()),
            "case {case:?}: expected a second read to answer {answer:?}, got {again:?}"
        );
    }
}
