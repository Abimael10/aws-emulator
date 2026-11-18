// SPDX-License-Identifier: CC0-1.0

//! AWS CodeArtifact's published default-quota catalog.

use super::*;

/// One CodeArtifact API request-rate quota: the requests per second the service accepts
/// for one operation.
///
/// CodeArtifact publishes a row of this class per throttled operation under the name
/// `<Api> requests per second`, and every member but the operation's own name and the value
/// is the same across the class — the window is one second and the `Description` is the
/// sentence of the class with the operation named in it — so one constructor fills them and
/// two rows of the class cannot drift apart.
///
/// SOURCE: the recorded default-quota listing of `codeartifact` for the name form, the
/// one-second window and the sentence, which states the CALLS that can be made to the
/// operation rather than describing the account.
fn requests_per_second(code: &str, api_name: &str, per_second: f64) -> QuotaRow {
    quota(format!("{api_name} requests per second"), per_second)
        .code(code)
        .period(1, "SECOND")
        .describe(&format!(
            "The maximum number of calls that can be made to {api_name} per second."
        ))
}

/// AWS CodeArtifact's published default-quota catalog.
///
/// SOURCE: AWS's published Service Quotas data for the `codeartifact` service code (what
/// `aws service-quotas list-aws-default-service-quotas --service-code codeartifact`
/// reports, which is the "Quotas in CodeArtifact" page of the CodeArtifact User Guide), and
/// the recorded default-quota listing of that code for which `QuotaCode` names which quota,
/// for the `<Api> requests per second` wording of its rate rows and for each row's
/// adjustability. Every row carries the `L-` code AWS publishes the quota under.
pub(in crate::state) fn codeartifact_quotas() -> Vec<QuotaRow> {
    vec![
        quota("Repositories per domain", 1000.0)
            .code("L-0B362111")
            .describe("The maximum number of repositories that can be created in a domain."),
        quota("Upstream repositories per repository", 10.0)
            .code("L-3072382D")
            .describe(
                "The maximum number of direct upstream repositories of a repository.",
            ),
        quota("Asset size", 5.0)
            .code("L-308A4050")
            .unit("Gigabytes")
            .describe("The maximum size of an asset."),
        // Two rate rows the recorded listing publishes as NOT raisable: the token and
        // asset-download rates are properties of the service rather than ceilings an
        // account can have lifted.
        requests_per_second("L-37894301", "GetAuthorizationToken", 40.0).fixed(),
        requests_per_second("L-3E27C79F", "PublishPackageVersion", 10.0),
        requests_per_second("L-5B2BD2E1", "GetPackageVersionAsset", 100.0).fixed(),
        quota("Upstream repositories searched", 25.0)
            .code("L-5D176989")
            .fixed()
            .describe(
                "The maximum number of upstream repositories searched when resolving a \
                 package.",
            ),
        requests_per_second("L-6010CAF9", "ListPackages", 200.0),
        requests_per_second("L-6C12FB34", "DescribePackageVersion", 100.0),
        requests_per_second("L-86608C96", "ListRepositories", 100.0),
        requests_per_second("L-A649E766", "ListPackageVersionAssets", 100.0),
        requests_per_second("L-AA0DC56D", "DeletePackageVersions", 10.0),
        // The ANONYMOUS request rate: a per-second ceiling on requests that carry no
        // authentication token, counted per source IP address rather than per account, and
        // published as one a client cannot have raised.
        quota("Requests without authentication token per IP address", 600.0)
            .code("L-BD902DFC")
            .fixed()
            .period(1, "SECOND")
            .describe(
                "The maximum number of requests per second without an authentication token \
                 from a single IP address.",
            ),
        requests_per_second("L-CBBCDF5C", "ListPackageVersions", 200.0),
        quota("Assets per package version", 100.0)
            .code("L-D42B1EF2")
            .fixed()
            .describe("The maximum number of assets of a package version."),
        quota("Domains per AWS account", 10.0)
            .code("L-DD7208D3")
            .describe("The maximum number of domains that can be created per AWS account."),
        quota("Versions per package", 1000.0)
            .code("L-F39CF68A")
            .describe("The maximum number of versions of a package."),
    ]
}
