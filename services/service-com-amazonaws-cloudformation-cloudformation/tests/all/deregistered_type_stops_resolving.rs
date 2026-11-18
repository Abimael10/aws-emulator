// SPDX-License-Identifier: CC0-1.0

//! What the registry's READERS say about a private extension once its registration has been
//! deregistered, driven through the registry's own writers over the awsQuery wire.
//!
//! The rule under test: **a private extension is addressable in this account and Region's
//! registry only while it holds a version in ACTIVE USE, so once `DeregisterType` has taken
//! its last live version out of use every reader of the registry stops resolving the
//! extension — `DescribeType` naming it with no version (by `Type` and `TypeName`, or by the
//! extension's own ARN) answers the modelled `TypeNotFoundException` (404) and keeps
//! answering it however often it is asked, `ListTypes` reports it in the `DEPRECATED` listing
//! instead of the `LIVE` one a request naming no status gets, and `ListTypeVersions` reports
//! every one of its versions as `DEPRECATED` and none as `LIVE` — while a request naming one
//! of those versions still reads that version back, which is the read "to view the
//! deprecation status of an extension or extension version".** A second `DeregisterType` of
//! the same extension is then the same 404: the deregistration is durable, not a status a
//! later call can undo.
//!
//! The registration is the one a client really makes: the handler package is staged in Amazon
//! S3 with a client's own `PutObject`, `RegisterType` reads the schema out of it, and the
//! request is monitored to `COMPLETE` through `DescribeTypeRegistration` before anything is
//! deregistered — so what is retired here is a registration this registry performed rather
//! than a record seeded into the store. The staging-and-registration harness is
//! `extension_registrations.rs`', the suite that owns it.
//!
//! The rows differ along what the rule generalises over: the extension KINDS the model admits
//! (`RESOURCE`, `MODULE`, `HOOK`) and how many versions the registration holds (one, two and
//! three) — a record holding more than one version in active use can only be deregistered
//! after its non-default versions have gone individually ("to deregister an entire extension,
//! you must individually deregister all active versions of that extension"), so the rows with
//! several versions reach the same retirement by that longer path. The recorded trace this
//! suite was written for is one row of it: a MODULE registered from a package in Amazon S3,
//! deregistered, and then polled for through `DescribeType`.

use service_com_amazonaws_cloudformation_cloudformation::state::registry::{
    deprecated_status, registration_status, registry_type, type_arn, type_version_arn,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::extension_registrations::{
    concluded_registration, create_package_bucket, describe_type, package_of, register_ok,
    schema_of, stage, Style, EVERY_HANDLER,
};
use crate::harness::{call, member, refusal, router_with_engine, xml_blocks, xml_text, REGION};

/// The error code the registry answers a request naming an extension it does not hold
/// (`TypeNotFoundException`'s `awsQueryError`).
const NOT_FOUND_CODE: &str = "TypeNotFoundException";

/// The sentence that refusal carries, with the extension the request identified quoted in it.
fn cannot_be_found(type_name: &str) -> String {
    format!("The type '{type_name}' cannot be found.")
}

/// The version id of the `index`-th version of a record, in the zero-padded form the registry
/// gives its version ids.
fn version_id(index: usize) -> String {
    format!("{:08}", index + 1)
}

/// One row: a registration of this kind, holding this many versions, which is then retired.
struct Retired {
    case: &'static str,
    type_kind: &'static str,
    type_name: &'static str,
    /// How many versions the registration holds by the time it is deregistered. The FIRST
    /// one registered is the record's default version, so every other one has to be
    /// deregistered individually before the extension itself can be.
    versions: usize,
}

/// Every row, spanning the kinds the model admits and the version counts that tell a record
/// holding one version from one holding several.
const ROWS: &[Retired] = &[
    Retired {
        case: "a resource type holding one version",
        type_kind: registry_type::RESOURCE,
        type_name: "Retired::Sample::Resource",
        versions: 1,
    },
    Retired {
        case: "a module holding one version",
        type_kind: registry_type::MODULE,
        type_name: "Retired::Sample::Thing::MODULE",
        versions: 1,
    },
    Retired {
        case: "a Hook holding one version",
        type_kind: registry_type::HOOK,
        type_name: "Retired::Sample::Hook",
        versions: 1,
    },
    Retired {
        case: "a resource type holding two versions",
        type_kind: registry_type::RESOURCE,
        type_name: "Retired::Multi::Resource",
        versions: 2,
    },
    Retired {
        case: "a module holding three versions",
        type_kind: registry_type::MODULE,
        type_name: "Retired::Multi::Thing::MODULE",
        versions: 3,
    },
    Retired {
        case: "a Hook holding two versions",
        type_kind: registry_type::HOOK,
        type_name: "Retired::Multi::Hook",
        versions: 2,
    },
];

/// Register the `round`-th version of every row that holds one, and wait for each of those
/// registrations to conclude `COMPLETE`.
///
/// One round per version, so the suite waits on the conclusions of a whole round together
/// rather than once per registration — and it waits on the CONDITION (the request's reported
/// progress), never on a duration: the conclusion is due at an instant and nothing in the
/// service sleeps for it.
async fn register_round(router: &MultiServiceRouter, round: usize) {
    let mut pending: Vec<(&str, String)> = Vec::new();
    for row in ROWS {
        if row.versions <= round {
            continue;
        }
        let case = row.case;
        let key = format!("{}-{}.zip", row.type_name.replace("::", "-"), round);
        let schema = schema_of(row.type_name, EVERY_HANDLER);
        stage(router, &key, &package_of(Some(&schema))).await;
        let token = register_ok(
            case,
            router,
            Some(row.type_kind),
            row.type_name,
            &Style::S3Uri.url(&key),
            &[],
        )
        .await;
        pending.push((case, token));
    }
    for (case, token) in pending {
        let monitored = concluded_registration(case, router, &token).await;
        assert_eq!(
            monitored.progress,
            registration_status::COMPLETE,
            "case {case:?}: expected the registration to complete, got {:?} ({:?})",
            monitored.progress,
            monitored.description
        );
    }
}

/// One `DescribeType` read by the extension's own ARN, the other way a request identifies an
/// extension; the reads by kind and name are the sibling suite's `describe_type`.
async fn describe_type_by_arn(router: &MultiServiceRouter, arn: &str) -> (u16, String) {
    call(router, "DescribeType", &[member("Arn", arn)]).await
}

/// Hold one read to the registry's modelled 404 for `type_name`: the status, the code and the
/// sentence a client branches on.
fn assert_not_found(case: &str, type_name: &str, status: u16, body: &str) {
    assert_eq!(
        status, 404,
        "case {case:?}: expected the read of a deregistered extension to be refused as not \
         found, got {status} {body}"
    );
    let (code, message) = refusal(body);
    assert_eq!(
        code, NOT_FOUND_CODE,
        "case {case:?}: expected the modelled not-found error, got {body}"
    );
    assert_eq!(
        message,
        cannot_be_found(type_name),
        "case {case:?}: expected {:?}, got {message:?}",
        cannot_be_found(type_name)
    );
}

/// The type names `ListTypes` reports for one kind at one deprecation status — `None` for a
/// request naming no status, which "the default is LIVE" decides.
async fn listed_types(
    case: &str,
    router: &MultiServiceRouter,
    type_kind: &str,
    status_filter: Option<&str>,
) -> Vec<String> {
    let mut members = vec![member("Type", type_kind)];
    if let Some(status_filter) = status_filter {
        members.push(member("DeprecatedStatus", status_filter));
    }
    let (status, body) = call(router, "ListTypes", &members).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the type listing to succeed, got {status} {body}"
    );
    // The summaries are read INSIDE their own block, so an element of another list the
    // envelope carries cannot be swept up with them.
    match xml_blocks(&body, "TypeSummaries").into_iter().next() {
        Some(block) => xml_blocks(&block, "member")
            .into_iter()
            .map(|summary| xml_text(&summary, "TypeName").unwrap_or_default())
            .collect(),
        None => Vec::new(),
    }
}

/// The version ids `ListTypeVersions` reports for one extension at one deprecation status.
async fn listed_versions(
    case: &str,
    router: &MultiServiceRouter,
    type_kind: &str,
    type_name: &str,
    status_filter: Option<&str>,
) -> Vec<String> {
    let mut members = vec![member("Type", type_kind), member("TypeName", type_name)];
    if let Some(status_filter) = status_filter {
        members.push(member("DeprecatedStatus", status_filter));
    }
    let (status, body) = call(router, "ListTypeVersions", &members).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the version listing to succeed, got {status} {body}"
    );
    match xml_blocks(&body, "TypeVersionSummaries").into_iter().next() {
        Some(block) => xml_blocks(&block, "member")
            .into_iter()
            .map(|summary| xml_text(&summary, "VersionId").unwrap_or_default())
            .collect(),
        None => Vec::new(),
    }
}

/// One `DeregisterType` call, by kind and name and optionally at one version.
async fn deregister(
    router: &MultiServiceRouter,
    type_kind: &str,
    type_name: &str,
    version: Option<&str>,
) -> (u16, String) {
    let mut members = vec![member("Type", type_kind), member("TypeName", type_name)];
    if let Some(version) = version {
        members.push(member("VersionId", version));
    }
    call(router, "DeregisterType", &members).await
}

#[tokio::test]
async fn a_deregistered_registration_resolves_in_none_of_the_registrys_readers() {
    let (router, _engine) = router_with_engine();
    create_package_bucket(&router).await;

    // Every row's registration, a round of versions at a time: the registry holds each row's
    // extension at the version count its row states before anything is deregistered.
    let rounds = ROWS
        .iter()
        .map(|row| row.versions)
        .max()
        .expect("the table has rows");
    for round in 0..rounds {
        register_round(&router, round).await;
    }

    for row in ROWS {
        let case = row.case;
        let every_version: Vec<String> = (0..row.versions).map(version_id).collect();
        let extension_arn = type_arn(REGION, row.type_kind, row.type_name);

        // Before the deregistration: the registration is what every reader resolves.
        let (status, body) = describe_type(&router, row.type_kind, row.type_name, None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the registration to be readable before it is \
             deregistered, got {status} {body}"
        );
        // A read naming no version resolves the record's DEFAULT version, which is the first
        // one registered, so the ARN it reports is that version's.
        let default_version_arn = type_version_arn(&extension_arn, &version_id(0));
        assert_eq!(
            xml_text(&body, "Arn").as_deref(),
            Some(default_version_arn.as_str()),
            "case {case:?}: expected the read to report the default version's ARN, got {body}"
        );
        let live = listed_versions(
            case,
            &router,
            row.type_kind,
            row.type_name,
            Some(deprecated_status::LIVE),
        )
        .await;
        assert_eq!(
            live, every_version,
            "case {case:?}: expected every registered version to be in active use before the \
             deregistration, got {live:?}"
        );
        let listed = listed_types(case, &router, row.type_kind, None).await;
        assert!(
            listed.iter().any(|name| name == row.type_name),
            "case {case:?}: expected {} in the LIVE listing before the deregistration, got \
             {listed:?}",
            row.type_name
        );

        // The deregistration: every version but the record's default one goes individually,
        // and the extension itself is what takes that last one out of active use.
        for version in every_version.iter().skip(1) {
            let (status, body) =
                deregister(&router, row.type_kind, row.type_name, Some(version)).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected version {version} to be deregistered, got {status} \
                 {body}"
            );
        }
        let (status, body) = deregister(&router, row.type_kind, row.type_name, None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the extension to be deregistered, got {status} {body}"
        );

        // After it: no reader resolves the extension any more — in neither of the two ways a
        // request identifies one.
        for form in ["by kind and name", "by its own ARN"] {
            // Asked twice: the deregistration is durable, so the second read is the same
            // refusal rather than a record that came back.
            for attempt in ["the first read", "a second read"] {
                let case = format!("{case}, read {form} ({attempt})");
                let (status, body) = if form == "by its own ARN" {
                    describe_type_by_arn(&router, &extension_arn).await
                } else {
                    describe_type(&router, row.type_kind, row.type_name, None).await
                };
                assert_not_found(&case, row.type_name, status, &body);
            }
        }

        let listed = listed_types(case, &router, row.type_kind, None).await;
        assert!(
            !listed.iter().any(|name| name == row.type_name),
            "case {case:?}: expected {} to leave the LIVE listing, got {listed:?}",
            row.type_name
        );
        let deprecated = listed_types(
            case,
            &router,
            row.type_kind,
            Some(deprecated_status::DEPRECATED),
        )
        .await;
        assert!(
            deprecated.iter().any(|name| name == row.type_name),
            "case {case:?}: expected {} in the DEPRECATED listing, got {deprecated:?}",
            row.type_name
        );

        let live = listed_versions(
            case,
            &router,
            row.type_kind,
            row.type_name,
            Some(deprecated_status::LIVE),
        )
        .await;
        assert!(
            live.is_empty(),
            "case {case:?}: expected no version of {} to be left in active use, got {live:?}",
            row.type_name
        );
        let deprecated = listed_versions(
            case,
            &router,
            row.type_kind,
            row.type_name,
            Some(deprecated_status::DEPRECATED),
        )
        .await;
        assert_eq!(
            deprecated, every_version,
            "case {case:?}: expected every version of {} to be reported deprecated, got \
             {deprecated:?}",
            row.type_name
        );

        // The VERSION records survive their extension: "to view the deprecation status of an
        // extension or extension version, use DescribeType".
        for version in &every_version {
            let case = format!("{case}, version {version} read by its id");
            let (status, body) =
                describe_type(&router, row.type_kind, row.type_name, Some(version)).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected the deregistered version to be readable by its id, \
                 got {status} {body}"
            );
            let reported = xml_text(&body, "DeprecatedStatus");
            assert_eq!(
                reported.as_deref(),
                Some(deprecated_status::DEPRECATED),
                "case {case:?}: expected the version to report itself deprecated, got \
                 {reported:?} in {body}"
            );
        }

        // And the deregistration cannot be made twice: there is no live registration of that
        // name left for a second call to name.
        let (status, body) = deregister(&router, row.type_kind, row.type_name, None).await;
        assert_not_found(
            &format!("{case}, deregistered a second time"),
            row.type_name,
            status,
            &body,
        );
    }
}
