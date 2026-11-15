// SPDX-License-Identifier: CC0-1.0

//! What the PACKAGE-scoped not-found sentence renders, asserted over the wire.
//!
//! Every case drives real `restJson1` requests through the `MultiServiceRouter` the
//! production emulator serves with, against a repository that exists, so the only thing
//! under test is the refusal a client reads.
//!
//! The rule under test, over a TABLE rather than one example: the sentence
//! `DescribePackage` states for a package the addressed repository does not hold renders
//! the request's FORMAT and the request's package NAME and nothing else — `Package not
//! found in repository. Package = {format=<format>, name=<package>}` — so it is
//! INDEPENDENT of the namespace the request carried, while the lookup it refuses is not: a
//! namespace still addresses a different package. (`DeletePackage` addresses a package by
//! the same triple but states no sentence for one that is absent: its target's absence is
//! what it is for, so it answers success — pinned in `package_delete_cascade`.)
//!
//! Asserted as:
//!
//! 1. Over every `PackageFormat` the model declares, namespaced and not, the sentence is
//!    exactly the format-and-name one, and carries no `namespace=` component.
//! 2. The invariant, per format: the sentence for a coordinate WITH a namespace is
//!    character-for-character the one for the same format and name WITHOUT one.
//! 3. The namespace still decides WHICH package is addressed: with the coordinate held,
//!    the same name under another namespace — and under no namespace — is still refused,
//!    with that same namespace-free sentence, while the held coordinate reads back.

use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use crate::domain_repository_lifecycle::create_repository;
use crate::harness::{call, create_domain, emulator, Answer};

/// The domain every case lives in.
const DOMAIN: &str = "not-found-sentence";

/// The repository every request addresses: it exists, and it holds only what a case seeds.
const REPOSITORY: &str = "Holdings";

/// One coordinate a request addresses.
#[derive(Debug)]
struct Row {
    case: &'static str,
    format: &'static str,
    namespace: Option<&'static str>,
    package: &'static str,
}

/// Every `PackageFormat` the model declares
/// (`smithy_query(query="shape",shape="PackageFormat")`: cargo | generic | maven | npm |
/// nuget | pypi | ruby | swift), each addressed the way its format carries a namespace —
/// the model's `namespace` member documents maven, swift and generic as the formats whose
/// packages have a namespace component, npm as carrying a scope, and Python, NuGet, Ruby
/// and Cargo as carrying none — plus one extra `npm` row so a scoped and an unscoped
/// coordinate of one format both appear.
const ROWS: [Row; 9] = [
    Row {
        case: "cargo",
        format: "cargo",
        namespace: None,
        package: "serde_json",
    },
    Row {
        case: "generic, namespaced",
        format: "generic",
        namespace: Some("acme"),
        package: "release.bundle",
    },
    Row {
        case: "maven, namespaced",
        format: "maven",
        namespace: Some("com.acme.tools"),
        package: "widget-core",
    },
    Row {
        case: "npm, scoped",
        format: "npm",
        namespace: Some("acme"),
        package: "widgets",
    },
    Row {
        case: "npm, unscoped",
        format: "npm",
        namespace: None,
        package: "left-side",
    },
    Row {
        case: "nuget",
        format: "nuget",
        namespace: None,
        package: "Acme.Widgets",
    },
    Row {
        case: "pypi",
        format: "pypi",
        namespace: None,
        package: "acme-widgets",
    },
    Row {
        case: "ruby",
        format: "ruby",
        namespace: None,
        package: "acme_widgets",
    },
    Row {
        case: "swift, namespaced",
        format: "swift",
        namespace: Some("acme"),
        package: "Widgets",
    },
];

impl Row {
    /// The query a package-scoped route carries for this coordinate: the namespace is sent
    /// exactly where the row carries one.
    fn query(&self) -> String {
        self.query_with(self.namespace)
    }

    /// [`Row::query`] with `namespace` substituted for the row's own — how the same format
    /// and name are addressed under another namespace, or under none.
    fn query_with(&self, namespace: Option<&str>) -> String {
        let mut query = format!(
            "domain={DOMAIN}&repository={REPOSITORY}&format={}&package={}",
            self.format, self.package
        );
        if let Some(namespace) = namespace {
            query.push_str(&format!("&namespace={namespace}"));
        }
        query
    }

    /// THE RULE, written out here rather than imported from the crate under test, so a
    /// wrong sentence in the implementation cannot make this suite pass: the request's
    /// format and the request's package name, and no namespace component.
    fn sentence(&self) -> String {
        format!(
            "Package not found in repository. Package = {{format={}, name={}}}",
            self.format, self.package
        )
    }
}

/// The PACKAGE-addressing read, as the wire reaches it: the operation that states this
/// sentence.
const PACKAGE_SCOPED: [(&str, &str); 1] = [("DescribePackage", "GET")];

/// The domain and the repository every case addresses.
async fn given_repository(router: &MultiServiceRouter) {
    assert_eq!(
        create_domain(router, DOMAIN, None).await.status,
        200,
        "CreateDomain must succeed"
    );
    let created = create_repository(router, DOMAIN, REPOSITORY, None, &[]).await;
    assert_eq!(
        created.status, 200,
        "CreateRepository must succeed, got {created:?}"
    );
}

/// Address one package-scoped operation at `query`.
async fn package_scoped(router: &MultiServiceRouter, method: &str, query: &str) -> Answer {
    call(router, method, &format!("/v1/package?{query}"), None).await
}

/// Hold the row's coordinate in the repository with no version of it, which is what
/// `PutPackageOriginConfiguration` does.
async fn given_package(router: &MultiServiceRouter, row: &Row) {
    let created = call(
        router,
        "POST",
        &format!("/v1/package?{}", row.query()),
        Some(json!({ "restrictions": { "publish": "ALLOW", "upstream": "ALLOW" } })),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "case {:?}: PutPackageOriginConfiguration must succeed, got {created:?}",
        row.case
    );
}

/// RULE 1: every PACKAGE-scoped refusal renders the format and the name only, whatever the
/// format and whether or not the request carried a namespace.
#[tokio::test]
async fn the_package_scoped_sentence_renders_the_format_and_the_name_only() {
    let router = emulator();
    given_repository(&router).await;

    for (operation, method) in PACKAGE_SCOPED {
        for row in &ROWS {
            let case = format!("{operation}/{}", row.case);
            let answer = package_scoped(&router, method, &row.query()).await;
            assert_eq!(
                (answer.status, answer.code()),
                (404, "ResourceNotFoundException".to_string()),
                "case {case:?}: expected the modelled 404, got {answer:?}"
            );
            assert_eq!(
                answer.message(),
                row.sentence(),
                "case {case:?}: expected the format-and-name sentence, got {:?}",
                answer.message()
            );
            assert!(
                !answer.message().contains("namespace="),
                "case {case:?}: the sentence must carry no namespace component, got {:?}",
                answer.message()
            );
        }
    }
}

/// RULE 2: the sentence does not vary with the namespace — the namespaced coordinate and
/// the namespace-less one of the same format and name are refused with one wording.
#[tokio::test]
async fn the_sentence_is_the_same_with_and_without_a_namespace() {
    let router = emulator();
    given_repository(&router).await;

    for row in &ROWS {
        let case = row.case;
        let with_namespace =
            package_scoped(&router, "GET", &row.query_with(Some("other-namespace"))).await;
        let without = package_scoped(&router, "GET", &row.query_with(None)).await;
        assert_eq!(
            (with_namespace.status, without.status),
            (404, 404),
            "case {case:?}: both coordinates must be refused, got {with_namespace:?} and \
             {without:?}"
        );
        assert_eq!(
            with_namespace.message(),
            without.message(),
            "case {case:?}: the namespaced sentence must equal the namespace-less one, got \
             {:?} and {:?}",
            with_namespace.message(),
            without.message()
        );
        assert_eq!(
            without.message(),
            row.sentence(),
            "case {case:?}: expected the format-and-name sentence, got {:?}",
            without.message()
        );
    }
}

/// RULE 3: the namespace still addresses a package — a held coordinate reads back, while
/// the same name under another namespace (and under none) is refused with the sentence that
/// names neither.
#[tokio::test]
async fn a_namespace_still_decides_which_package_is_addressed() {
    let router = emulator();
    given_repository(&router).await;

    for row in ROWS.iter().filter(|row| row.namespace.is_some()) {
        let case = row.case;
        given_package(&router, row).await;

        let held = package_scoped(&router, "GET", &row.query()).await;
        assert_eq!(
            held.status, 200,
            "case {case:?}: the held coordinate must read back, got {held:?}"
        );
        assert_eq!(
            held.body
                .get("package")
                .and_then(|package| package.get("namespace")),
            Some(&json!(row.namespace.expect("a namespaced row"))),
            "case {case:?}: expected the stored namespace, got {held:?}"
        );

        for other in [Some("not-the-namespace"), None] {
            let answer = package_scoped(&router, "GET", &row.query_with(other)).await;
            assert_eq!(
                (answer.status, answer.code()),
                (404, "ResourceNotFoundException".to_string()),
                "case {case:?}/{other:?}: another namespace addresses another package, got \
                 {answer:?}"
            );
            assert_eq!(
                answer.message(),
                row.sentence(),
                "case {case:?}/{other:?}: expected the format-and-name sentence, got {:?}",
                answer.message()
            );
        }
    }
}
