// SPDX-License-Identifier: CC0-1.0

//! `ListAppVersionResources` is a read over a RESOLUTION of an app version, end to
//! end over HTTP.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` with a
//! restJson1 request — and asserts the response BODY, not just the status.
//!
//! The rules under test:
//!
//! * a resolution-scoped listing of an app version no resolution has been started
//!   for has nothing to report, so it is refused with the modelled
//!   `ConflictException` (HTTP 409) naming the operation that starts one — and the
//!   refusal starts nothing itself, so the version is still unresolved afterwards;
//! * a resolution belongs to ONE app version: resolving one version does not make
//!   another listable;
//! * a listing reports exactly the mapping the resolution it is against captured —
//!   an entry written into the version after that resolution is not in it, an entry
//!   removed after it still is, and the mapping a later edit produced is reported
//!   only by a later resolution.

use serde_json::json;
use service_com_amazonaws_resiliencehub_awsresiliencehub::state::DRAFT_APP_VERSION;
use smithy_server_runtime::MultiServiceRouter;

// The wire harness every suite in this crate shares: the one-service router, the
// signed restJson1 POST and the reply it answers with.
use super::application_lifecycle::{emulator, post, Reply};

/// The sentence this service states when a resolution-scoped read addresses an app
/// version nothing has resolved — the whole body of that refusal, which carries no
/// value from the request or from state.
const NEVER_RESOLVED: &str = "Resources were never resolved. Use the \
                              ResolveAppVersionResources API to resolve resources \
                              first.";

/// An application to compose and list, and its ARN.
async fn create_an_app(router: &MultiServiceRouter, name: &str) -> String {
    post(router, "/create-app", &json!({"name": name}))
        .await
        .ok(name)["app"]["appArn"]
        .as_str()
        .expect("a minted appArn")
        .to_string()
}

/// Add one physical resource, named `identifier`, to `app_arn`'s draft version.
async fn add_a_resource(router: &MultiServiceRouter, app_arn: &str, identifier: &str) {
    post(
        router,
        "/create-app-version-resource",
        &json!({
            "appArn": app_arn,
            "logicalResourceId": {"identifier": identifier},
            "physicalResourceId": identifier,
            "resourceType": "AWS::SQS::Queue",
            "appComponents": ["messaging-tier"],
        }),
    )
    .await
    .ok(identifier);
}

/// Remove the resource `identifier` from `app_arn`'s draft version.
async fn remove_a_resource(router: &MultiServiceRouter, app_arn: &str, identifier: &str) {
    post(
        router,
        "/delete-app-version-resource",
        &json!({
            "appArn": app_arn,
            "logicalResourceId": {"identifier": identifier},
        }),
    )
    .await
    .ok(identifier);
}

/// Publish `app_arn`'s draft and answer the `appVersion` the publish minted.
async fn publish(router: &MultiServiceRouter, app_arn: &str) -> String {
    post(router, "/publish-app-version", &json!({"appArn": app_arn}))
        .await
        .ok("publish")["appVersion"]
        .as_str()
        .expect("a published appVersion")
        .to_string()
}

/// Start a resolution of `app_version` of `app_arn` and answer its `resolutionId`.
async fn resolve(router: &MultiServiceRouter, app_arn: &str, app_version: &str) -> String {
    post(
        router,
        "/resolve-app-version-resources",
        &json!({"appArn": app_arn, "appVersion": app_version}),
    )
    .await
    .ok("resolve")["resolutionId"]
        .as_str()
        .expect("a minted resolutionId")
        .to_string()
}

/// `ListAppVersionResources` for one version of one application, naming
/// `resolution_id` when the case addresses one and leaving the member out
/// otherwise (which reads the version's CURRENT resolution).
async fn list_resources(
    router: &MultiServiceRouter,
    app_arn: &str,
    app_version: &str,
    resolution_id: Option<&str>,
) -> Reply {
    let mut body = json!({"appArn": app_arn, "appVersion": app_version});
    if let Some(resolution_id) = resolution_id {
        body["resolutionId"] = json!(resolution_id);
    }
    post(router, "/list-app-version-resources", &body).await
}

/// Read the resolution status of `app_version` of `app_arn`, which is what says
/// whether that version has a resolution at all.
async fn resolution_status(
    router: &MultiServiceRouter,
    app_arn: &str,
    app_version: &str,
) -> Reply {
    post(
        router,
        "/describe-app-version-resources-resolution-status",
        &json!({"appArn": app_arn, "appVersion": app_version}),
    )
    .await
}

/// The `logicalResourceId.identifier` of every resource a successful listing
/// reports, sorted, so a case compares WHAT was reported without pinning the order
/// the service pages in.
fn listed_identifiers(reply: &Reply, case: &str) -> Vec<String> {
    let mut identifiers: Vec<String> = reply.ok(case)["physicalResources"]
        .as_array()
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: physicalResources is @required, got {:?}",
                reply.body
            )
        })
        .iter()
        .map(|resource| {
            resource["logicalResourceId"]["identifier"]
                .as_str()
                .expect("logicalResourceId.identifier is @required")
                .to_string()
        })
        .collect();
    identifiers.sort();
    identifiers
}

/// The `resolutionId` a successful listing reports.
fn reported_resolution(reply: &Reply, case: &str) -> String {
    reply.ok(case)["resolutionId"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: resolutionId is @required, got {:?}",
                reply.body
            )
        })
        .to_string()
}

/// One row of the version table: an app version to list the resources of, how it
/// was composed, and how the listing addresses the resolution.
struct Version {
    label: &'static str,
    /// The `logicalResourceId.identifier` of each resource written into the draft
    /// before the version is taken.
    composed_of: &'static [&'static str],
    /// Whether the draft is PUBLISHED and the published version listed instead of
    /// the draft.
    publish: bool,
    /// Whether the listing names the `resolutionId` rather than leaving the member
    /// out.
    names_the_resolution: bool,
}

/// The table: one app version per row, each against its own application.
///
/// The rows differ along what the rule generalises over — how much the version
/// holds (nothing, one resource, several), which version it is (the draft, or a
/// frozen published one), whether the request addresses the resolution by id or
/// leaves it to the version's current one, and the form of the identifiers written
/// into the version (a plain name, one carrying a hyphen, one carrying an
/// underscore, a single character, and one that is a prefix of another).
fn version_table() -> Vec<Version> {
    vec![
        Version {
            label: "a draft nobody wrote anything into",
            composed_of: &[],
            publish: false,
            names_the_resolution: false,
        },
        Version {
            label: "a draft holding one resource, resolution named",
            composed_of: &["orders"],
            publish: false,
            names_the_resolution: true,
        },
        Version {
            label: "a draft holding several resources",
            composed_of: &["orders", "orders-archive", "orders_audit_log", "x"],
            publish: false,
            names_the_resolution: false,
        },
        Version {
            label: "a published version holding several resources, resolution named",
            composed_of: &["payments", "payments-eu"],
            publish: true,
            names_the_resolution: true,
        },
        Version {
            label: "a published version holding one resource",
            composed_of: &["only_queue"],
            publish: true,
            names_the_resolution: false,
        },
    ]
}

/// Compose one row's application and answer its ARN together with the `appVersion`
/// the row addresses.
async fn compose(router: &MultiServiceRouter, index: usize, row: &Version) -> (String, String) {
    let app_arn = create_an_app(router, &format!("resolution-scoped-{index}")).await;
    for identifier in row.composed_of {
        add_a_resource(router, &app_arn, identifier).await;
    }
    let app_version = if row.publish {
        publish(router, &app_arn).await
    } else {
        DRAFT_APP_VERSION.to_string()
    };
    (app_arn, app_version)
}

/// A composed app version nothing has resolved has no mapping to report: the
/// listing is refused with the modelled 409 and that one sentence, the refusal
/// starts no resolution of its own, and the SAME listing succeeds once a resolve
/// has produced one.
#[tokio::test]
async fn listing_a_version_nothing_resolved_is_refused_with_the_modeled_conflict() {
    let router = emulator();

    for (index, row) in version_table().into_iter().enumerate() {
        let case = row.label;
        let (app_arn, app_version) = compose(&router, index, &row).await;

        // Refused — twice, because a refusal may not change what the next caller
        // gets either.
        for read in ["the first listing", "the second listing"] {
            let refused = list_resources(&router, &app_arn, &app_version, None).await;
            refused.refused(&format!("{case} ({read})"), 409, "ConflictException");
            assert_eq!(
                refused.body["message"].as_str(),
                Some(NEVER_RESOLVED),
                "case {case:?} ({read}): expected the refusal to state \
                 {NEVER_RESOLVED:?}, got {:?}",
                refused.body
            );
            // The refusal is about the ABSENCE of a resolution, so it names no
            // resource of its own.
            assert!(
                refused.body["resourceId"].is_null() && refused.body["resourceType"].is_null(),
                "case {case:?} ({read}): the refusal must name no resource, got {:?}",
                refused.body
            );
        }

        // The refused listing started nothing: the version still has no resolution
        // for the status reader to report.
        resolution_status(&router, &app_arn, &app_version)
            .await
            .refused(
                &format!("{case}: the status read after the refusals"),
                404,
                "ResourceNotFoundException",
            );

        // A resolution is OF one app version, so resolving ANOTHER version of the
        // same application leaves this one unresolved. The other version is the
        // draft for a published row, and a publish of the draft otherwise — except
        // for a row whose draft maps nothing, which has nothing to publish.
        let elsewhere = if row.publish {
            Some(DRAFT_APP_VERSION.to_string())
        } else if row.composed_of.is_empty() {
            None
        } else {
            Some(publish(&router, &app_arn).await)
        };
        if let Some(elsewhere) = elsewhere {
            let resolved_elsewhere = resolve(&router, &app_arn, &elsewhere).await;
            list_resources(&router, &app_arn, &app_version, None)
                .await
                .refused(
                    &format!("{case}: after resolving {elsewhere}"),
                    409,
                    "ConflictException",
                );
            // ... and that other version's resolution does not address this one.
            list_resources(&router, &app_arn, &app_version, Some(&resolved_elsewhere))
                .await
                .refused(
                    &format!("{case}: against {elsewhere}'s resolution"),
                    404,
                    "ResourceNotFoundException",
                );
        }

        // Resolving the addressed version is what clears the refusal, and the
        // listing then reports what that version holds.
        let resolution_id = resolve(&router, &app_arn, &app_version).await;
        let addressed = row.names_the_resolution.then_some(resolution_id.as_str());
        let listed = list_resources(&router, &app_arn, &app_version, addressed).await;
        let expected: Vec<String> = {
            let mut composed: Vec<String> =
                row.composed_of.iter().map(|id| (*id).to_string()).collect();
            composed.sort();
            composed
        };
        assert_eq!(
            listed_identifiers(&listed, case),
            expected,
            "case {case:?}: the resolved listing must report the resources the \
             version holds"
        );
        assert_eq!(
            reported_resolution(&listed, case),
            resolution_id,
            "case {case:?}: the listing must report the resolution it is against"
        );
    }
}

/// A listing reports the mapping its resolution CAPTURED: an entry written into
/// the version after that resolution is not in it, an entry removed after it is
/// still in it, and the edited mapping is reported only by a resolution started
/// after the edit — while the earlier resolution keeps answering its own mapping.
#[tokio::test]
async fn a_listing_reports_only_the_mapping_its_resolution_captured() {
    let router = emulator();

    for (index, row) in version_table().into_iter().enumerate() {
        let case = row.label;
        let (app_arn, app_version) = compose(&router, index, &row).await;

        let first_resolution = resolve(&router, &app_arn, &app_version).await;
        let mut captured: Vec<String> =
            row.composed_of.iter().map(|id| (*id).to_string()).collect();
        captured.sort();

        // The draft's edit buffer changes AFTER that resolution: one resource
        // written in, and one of the row's own taken out when it has one.
        let written = format!("written-after-{index}");
        add_a_resource(&router, &app_arn, &written).await;
        let removed = row.composed_of.first().copied();
        if let Some(removed) = removed {
            remove_a_resource(&router, &app_arn, removed).await;
        }

        // The first resolution still reports exactly what it mapped — addressed by
        // its id, and as the version's current resolution.
        for addressed in [Some(first_resolution.as_str()), None] {
            let listed = list_resources(&router, &app_arn, &app_version, addressed).await;
            let selector = addressed.map_or("no resolutionId", |_| "its resolutionId");
            assert_eq!(
                listed_identifiers(&listed, case),
                captured,
                "case {case:?} (addressed by {selector}): a listing must report the \
                 mapping its resolution captured, not the edits made since"
            );
            assert_eq!(
                reported_resolution(&listed, case),
                first_resolution,
                "case {case:?} (addressed by {selector}): the listing must report the \
                 resolution it answered from"
            );
        }

        // A second resolution maps the version as it stands now: the draft as
        // edited, or the frozen snapshot a published version keeps whatever the
        // draft does.
        let second_resolution = resolve(&router, &app_arn, &app_version).await;
        assert_ne!(
            second_resolution, first_resolution,
            "case {case:?}: every resolve starts a resolution of its own"
        );
        let now_mapped: Vec<String> = if row.publish {
            captured.clone()
        } else {
            let mut after: Vec<String> = captured
                .iter()
                .filter(|identifier| Some(identifier.as_str()) != removed)
                .cloned()
                .collect();
            after.push(written.clone());
            after.sort();
            after
        };
        let listed = list_resources(&router, &app_arn, &app_version, None).await;
        assert_eq!(
            listed_identifiers(&listed, case),
            now_mapped,
            "case {case:?}: the newest resolution must map the version as it stands"
        );
        assert_eq!(
            reported_resolution(&listed, case),
            second_resolution,
            "case {case:?}: a listing naming no resolution reports the version's \
             current one"
        );

        // And the superseded resolution keeps its own mapping: a client holding an
        // older handle reads what that resolution mapped, not what the newest one
        // did.
        let earlier = list_resources(
            &router,
            &app_arn,
            &app_version,
            Some(first_resolution.as_str()),
        )
        .await;
        assert_eq!(
            listed_identifiers(&earlier, case),
            captured,
            "case {case:?}: a superseded resolution must keep answering the mapping \
             it captured"
        );
        assert_eq!(
            reported_resolution(&earlier, case),
            first_resolution,
            "case {case:?}: a listing against an older handle must answer that handle"
        );
    }
}
