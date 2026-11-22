// SPDX-License-Identifier: CC0-1.0

//! The INTERNAL MASTER USER's own reach on the address the control plane published, over a
//! SET of engine releases: not which users the database holds or what a role is worth —
//! the parent suite pins those — but that a domain created with fine-grained access control
//! and an internal user database ANSWERS the master's HTTP Basic credential once it reports
//! itself up, and that what it answers is computed from the domain's own record.
//!
//! RULE: once such a domain reports itself created with nothing in flight, the address it
//! published answers `GET /` presented with the configured master user's Basic credential
//! `200`, with a `version` block naming the distribution and the release the domain's own
//! `EngineVersion` names; the same request presenting no credential is refused `401` with
//! the `WWW-Authenticate` challenge that says how to present one and names nothing of the
//! cluster; and `GET /_plugins/_security/api/account` as that master is answered `200`
//! reporting it as a user of the internal database holding the all-permitting role.
//!
//! The rows differ along what that rule generalises over — the engine release the domain
//! runs, and the length of the master's own name and password within the two modelled
//! bounds — so no value of one recorded run is part of what is asserted. WHICH credentials
//! the cluster refuses is the parent suite's own table and is not copied here; each row
//! asserts only that moving the release leaves that boundary where the parent pinned it.
//!
//! The create body is the parent suite's ([`super::create_body`]), with only the release and
//! the master's credentials moved; the address is read where a client reads it
//! ([`crate::published_domain_address::published_endpoint`]), the wait is the one a client
//! performs ([`crate::domain_lifecycle::settled_domain`]), and the Basic-authenticated
//! request goes through the parent's own helper ([`super::rest`]) — no second copy of any of
//! them.

use super::*;

use crate::domain_lifecycle::settled_domain;
use crate::published_domain_address::{published_endpoint, split_address};

/// One domain this case stands up: the release it runs, the master user it is created with,
/// and what its banner must then report.
struct Row {
    case: &'static str,
    domain: &'static str,
    engine_version: &'static str,
    master: &'static str,
    password: &'static str,
    /// `version.distribution` the banner must carry.
    distribution: &'static str,
    /// `version.number` it must carry.
    number: &'static str,
}

/// The releases below are entries of the catalogue this service publishes — the
/// `VersionString` values `ListVersions` answers, each in the `<EngineType>_<major>.<minor>`
/// form `CreateDomain$EngineVersion` takes (`smithy_query(query="shape",
/// shape="VersionString")`, `smithy_query(query="operation", operation="ListVersions")`).
/// Four of them rather than one: both engines, two OpenSearch majors and two minors of the
/// same major, so the banner's distribution and number follow the domain's record and not a
/// fixed string. Only the first is the release the report that prompted this suite cited.
///
/// The master names and passwords span the two modelled bounds —
/// `MasterUserOptions$MasterUserName` is `@length 1..64` and `MasterUserPassword` is
/// `@length 8..128` (`smithy_query(query="shape", shape="MasterUserOptions")`) — with a
/// name at each end of its range and a password at the shortest its range admits.
const ROWS: &[Row] = &[
    Row {
        case: "the OpenSearch 2.11 line",
        domain: "fgac-master-2-11",
        engine_version: "OpenSearch_2.11",
        master: "os-master",
        password: "Master-pass-1!",
        distribution: "opensearch",
        number: "2.11.0",
    },
    Row {
        case: "an older OpenSearch line, with the shortest master name and password the \
               model admits",
        domain: "fgac-master-1-3",
        engine_version: "OpenSearch_1.3",
        master: "m",
        password: "Pass-123",
        distribution: "opensearch",
        number: "1.3.0",
    },
    Row {
        case: "another minor of the same OpenSearch major, with the longest master name the \
               model admits",
        domain: "fgac-master-2-19",
        engine_version: "OpenSearch_2.19",
        master: "master-user-0123456789012345678901234567890123456789012345678901",
        password: "Master-pass-0123456789012345678901234567890123456789",
        distribution: "opensearch",
        number: "2.19.0",
    },
    Row {
        case: "the Elasticsearch line",
        domain: "fgac-master-es-7-10",
        engine_version: "Elasticsearch_7.10",
        master: "legacy-master",
        password: "Legacy-pass-7!",
        distribution: "elasticsearch",
        number: "7.10.0",
    },
];

/// The security plugin's own all-permitting predefined role, which the control plane binds a
/// domain's master user to.
const ALL_ACCESS: &str = "all_access";

/// The parent suite's create body with only this row's release and master credentials moved,
/// so every prerequisite fine-grained access control needs is the one both suites state once.
fn create_body_for(row: &Row) -> Value {
    let mut body = create_body(row.domain);
    body["EngineVersion"] = json!(row.engine_version);
    body["AdvancedSecurityOptions"]["MasterUserOptions"] = json!({
        "MasterUserName": row.master,
        "MasterUserPassword": row.password,
    });
    body
}

/// One request to the ADDRESS the control plane published, dialled as a client dials it: the
/// authority of the published value is the host, and whatever it carries after that is the
/// prefix of the request path ([`split_address`]).
async fn at_published(
    emulator: &Emulator,
    address: &str,
    credential: Option<(&str, &str)>,
    path: &str,
) -> Answer {
    let (authority, prefix) = split_address(address);
    rest(
        emulator,
        authority,
        credential,
        "GET",
        &format!("{prefix}{path}"),
        None,
    )
    .await
}

#[tokio::test]
async fn the_internal_master_is_served_the_banner_of_the_release_its_domain_runs() {
    let emulator = Emulator::new();
    // Every domain is created first and waited for afterwards, so the provisioning spans of
    // the rows overlap instead of running end to end.
    for row in ROWS {
        let created = emulator.create(create_body_for(row)).await;
        assert_eq!(
            created.status, 200,
            "case {:?}: expected CreateDomain to answer 200, got {} with {:?}",
            row.case, created.status, created.body
        );
    }

    for row in ROWS {
        let settled = settled_domain(&emulator, row.domain).await;
        assert_eq!(
            settled["Processing"].as_bool(),
            Some(false),
            "case {:?}: expected the domain to report nothing in flight, got {settled:?}",
            row.case
        );
        let address = published_endpoint(&emulator, row.domain, row.case).await;

        // The master's own credential: served, and what it is served is this domain's
        // record.
        let banner =
            at_published(&emulator, &address, Some((row.master, row.password)), "/").await;
        assert_eq!(
            banner.status, 200,
            "case {:?}: expected the master's Basic credential to be served 200, got {} with \
             {:?}",
            row.case, banner.status, banner.body
        );
        assert_eq!(
            banner.body["version"]["distribution"].as_str(),
            Some(row.distribution),
            "case {:?}: expected version.distribution {:?}, got {:?}",
            row.case,
            row.distribution,
            banner.body
        );
        assert_eq!(
            banner.body["version"]["number"].as_str(),
            Some(row.number),
            "case {:?}: expected version.number {:?}, got {:?}",
            row.case,
            row.number,
            banner.body
        );
        assert_eq!(
            banner.body["name"].as_str(),
            Some(row.domain),
            "case {:?}: expected the banner to name the domain it is, got {:?}",
            row.case,
            banner.body
        );

        // And the same request presenting NO credential: refused, told how to present one,
        // and saying nothing about the cluster behind it. The challenge header's name is the
        // parent suite's one statement of it ([`super::CHALLENGE`]), so the two suites
        // cannot disagree on it. Which credentials the cluster refuses is the parent's own
        // table and is not copied here; what this row adds is that switching the release
        // leaves the boundary where it was.
        let refused = at_published(&emulator, &address, None, "/").await;
        assert_eq!(
            refused.status, 401,
            "case {:?}: expected a request presenting no credential to be refused 401, got {} \
             with {:?}",
            row.case, refused.status, refused.body
        );
        let challenge = refused.headers.get(CHALLENGE).cloned();
        assert!(
            challenge
                .as_deref()
                .is_some_and(|challenge| challenge.starts_with("Basic realm=")),
            "case {:?}: expected a Basic realm challenge on the anonymous refusal, got \
             {challenge:?}",
            row.case
        );
        let said = refused.body.to_string();
        assert!(
            !said.contains("cluster_name") && !said.contains(row.domain),
            "case {:?}: expected the refusal to name nothing of the cluster, got {said:?}",
            row.case
        );

        // And the plugin's account route tells that master who it is.
        let account = at_published(
            &emulator,
            &address,
            Some((row.master, row.password)),
            "/_plugins/_security/api/account",
        )
        .await;
        assert_eq!(
            account.status, 200,
            "case {:?}: expected the account route to answer the master 200, got {} with {:?}",
            row.case, account.status, account.body
        );
        assert_eq!(
            account.body["user_name"].as_str(),
            Some(row.master),
            "case {:?}: expected the account route to name the configured master, got {:?}",
            row.case,
            account.body
        );
        assert_eq!(
            account.body["is_internal_user"].as_bool(),
            Some(true),
            "case {:?}: expected the master to be reported as a user of the internal \
             database, got {:?}",
            row.case,
            account.body
        );
        let roles: Vec<&str> = account.body["roles"]
            .as_array()
            .map(|roles| roles.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        assert!(
            roles.contains(&ALL_ACCESS),
            "case {:?}: expected {ALL_ACCESS:?} among the master's roles, got {roles:?}",
            row.case
        );
        assert!(
            !account.body.to_string().contains(row.password),
            "case {:?}: expected the account route to report no credential, got {:?}",
            row.case,
            account.body
        );
    }
}
