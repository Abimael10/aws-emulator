// SPDX-License-Identifier: CC0-1.0

//! What the `resolutionId` a resource resolution is addressed by IS, end to end
//! over HTTP.
//!
//! The rule under test: the identifier `ResolveAppVersionResources` hands back is a
//! HANDLE the service mints for that one resolution — server-generated and opaque
//! (it carries nothing the request supplied, and nothing another resolution's
//! handle shares), unique across resolutions and across the resource ids this
//! service mints, inside the `@length` the model gives the member it travels in
//! (`String255`, 1..255, `smithy_query query="operation"
//! operation="ResolveAppVersionResources,DescribeAppVersionResourcesResolutionStatus,\
//! ListAppVersionResources"`), minted only by the call that STARTS a resolution,
//! and the value every resolution-scoped read of that app version accepts back —
//! and only of that app version.
//!
//! The rows of the table below differ along what the rule generalises over: what
//! the resolved version holds (nothing, one resource, several), which kind of
//! version it is (the draft, a published one), how often the same version is
//! resolved, and which application the version belongs to — including a second
//! application composed identically to the first, so a handle derived from the
//! request would repeat.
//!
//! The harness (`emulator`, `post`, `Reply`) is the one `application_lifecycle`
//! drives every request of this crate's suites through: the real wire path,
//! `MultiServiceRouter::dispatch` with a restJson1 request.

use std::collections::BTreeSet;

use serde_json::json;
use service_com_amazonaws_resiliencehub_awsresiliencehub::state::DRAFT_APP_VERSION;
use smithy_server_runtime::MultiServiceRouter;

use super::application_lifecycle::{create_a_policy, emulator, post, ACCOUNT, REGION};

/// The modelled `@length` of `String255`, the shape every `resolutionId` member
/// targets (`smithy_query query="operation"
/// operation="ResolveAppVersionResources"`: `resolutionId` targets `String255`,
/// length 1..255).
const RESOLUTION_ID_LENGTH: std::ops::RangeInclusive<usize> = 1..=255;

/// The modelled error a read scoped to a resolution the addressed app version does
/// not have is refused with: `ResourceNotFoundException`, `httpError` 404
/// (`smithy_query query="operation"
/// operation="DescribeAppVersionResourcesResolutionStatus"`).
const NOT_FOUND: (u16, &str) = (404, "ResourceNotFoundException");

/// One resolution handle the service minted, with the request it was minted for.
#[derive(Debug)]
struct Handle {
    case: String,
    app_arn: String,
    app_version: String,
    /// The `resolutionId` `ResolveAppVersionResources` answered.
    text: String,
}

/// Create an application and answer its `appArn`.
async fn create_an_app(router: &MultiServiceRouter, name: &str) -> String {
    let reply = post(router, "/create-app", &json!({"name": name})).await;
    reply.ok(name)["app"]["appArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {name:?}: a minted appArn, got {:?}", reply.body))
        .to_string()
}

/// Add one physical resource to `app_arn`'s draft version.
async fn add_a_resource(router: &MultiServiceRouter, app_arn: &str, identifier: &str) {
    let reply = post(
        router,
        "/create-app-version-resource",
        &json!({
            "appArn": app_arn,
            "resourceName": identifier,
            "logicalResourceId": {"identifier": identifier},
            "physicalResourceId": identifier,
            "resourceType": "AWS::SQS::Queue",
            "appComponents": ["queue-tier"],
        }),
    )
    .await;
    reply.ok(identifier);
}

/// Publish `app_arn`'s draft and answer the `appVersion` the publish minted.
async fn publish(router: &MultiServiceRouter, app_arn: &str) -> String {
    let reply = post(router, "/publish-app-version", &json!({"appArn": app_arn})).await;
    reply.ok("publish")["appVersion"]
        .as_str()
        .unwrap_or_else(|| panic!("a published appVersion, got {:?}", reply.body))
        .to_string()
}

/// Start a resolution of `app_version` of `app_arn` and answer the handle it was
/// given.
async fn resolve(
    router: &MultiServiceRouter,
    case: &str,
    app_arn: &str,
    app_version: &str,
) -> Handle {
    let reply = post(
        router,
        "/resolve-app-version-resources",
        &json!({"appArn": app_arn, "appVersion": app_version}),
    )
    .await;
    let body = reply.ok(case);
    Handle {
        case: case.to_string(),
        app_arn: app_arn.to_string(),
        app_version: app_version.to_string(),
        text: body["resolutionId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: resolutionId is @required, got {body:?}"))
            .to_string(),
    }
}

/// Read a resolution's status, naming `resolution_id` when the case addresses one
/// and leaving the member out otherwise (which reads the version's CURRENT
/// resolution).
async fn describe(
    router: &MultiServiceRouter,
    app_arn: &str,
    app_version: &str,
    resolution_id: Option<&str>,
) -> super::application_lifecycle::Reply {
    let mut body = json!({"appArn": app_arn, "appVersion": app_version});
    if let Some(resolution_id) = resolution_id {
        body["resolutionId"] = json!(resolution_id);
    }
    post(
        router,
        "/describe-app-version-resources-resolution-status",
        &body,
    )
    .await
}

/// List an app version's resources against the resolution `resolution_id`.
async fn list_against(
    router: &MultiServiceRouter,
    app_arn: &str,
    app_version: &str,
    resolution_id: &str,
) -> super::application_lifecycle::Reply {
    post(
        router,
        "/list-app-version-resources",
        &json!({
            "appArn": app_arn,
            "appVersion": app_version,
            "resolutionId": resolution_id,
        }),
    )
    .await
}

/// The id segment of an ARN this service minted: everything after
/// `arn:aws:resiliencehub:<region>:<account>:<kind>/`.
fn arn_id_segment(case: &str, arn: &str, kind: &str) -> String {
    let prefix = format!("arn:aws:resiliencehub:{REGION}:{ACCOUNT}:{kind}/");
    arn.strip_prefix(&prefix)
        .unwrap_or_else(|| panic!("case {case:?}: expected an ARN starting {prefix:?}, got {arn:?}"))
        .to_string()
}

/// What two applications, the resources they hold and the resolutions started for
/// them produced: every minted handle, in minting order, plus the id segments of
/// the ARNs the same run minted.
struct Minted {
    handles: Vec<Handle>,
    /// The id segment of each `appArn` and of the `policyArn` — the OTHER
    /// identifiers this service mints, which no handle may be.
    resource_ids: Vec<String>,
    /// Everything the requests above supplied: no handle may carry any of it.
    request_values: Vec<String>,
}

/// Drive the table: two applications composed the same way, the draft of each
/// resolved (empty, then holding one resource, then holding three), the draft of
/// the first resolved twice, and a published version of each resolved too.
async fn minted(router: &MultiServiceRouter) -> Minted {
    let mut handles = Vec::new();
    let mut resource_ids = Vec::new();
    let mut request_values = Vec::new();

    let policy_arn = create_a_policy(router, "handle-identity-policy").await;
    resource_ids.push(arn_id_segment(
        "a minted policyArn",
        &policy_arn,
        "resiliency-policy",
    ));

    for app in ["handle-identity-a", "handle-identity-b"] {
        let app_arn = create_an_app(router, app).await;
        resource_ids.push(arn_id_segment("a minted appArn", &app_arn, "app"));
        request_values.push(app.to_string());
        request_values.push(app_arn.clone());

        // An app version holding NOTHING still resolves, so the handle cannot be
        // the identity of what the version holds.
        handles.push(
            resolve(
                router,
                &format!("{app}: the empty draft"),
                &app_arn,
                DRAFT_APP_VERSION,
            )
            .await,
        );

        for (count, resource) in [1_usize, 3].into_iter().zip(["orders", "payments"]) {
            for index in 0..count {
                let identifier = format!("{app}-{resource}-{index}");
                add_a_resource(router, &app_arn, &identifier).await;
                request_values.push(identifier);
            }
            handles.push(
                resolve(
                    router,
                    &format!("{app}: the draft holding {resource}"),
                    &app_arn,
                    DRAFT_APP_VERSION,
                )
                .await,
            );
        }

        // Resolving the SAME, unchanged version again is another resolution, so it
        // gets another handle.
        handles.push(
            resolve(
                router,
                &format!("{app}: the unchanged draft, resolved again"),
                &app_arn,
                DRAFT_APP_VERSION,
            )
            .await,
        );

        let published = publish(router, &app_arn).await;
        handles.push(
            resolve(
                router,
                &format!("{app}: published version {published}"),
                &app_arn,
                &published,
            )
            .await,
        );
    }

    Minted {
        handles,
        resource_ids,
        request_values,
    }
}

/// Every handle is opaque — it carries nothing the request supplied — is inside
/// the modelled length, and is none of the resource ids the same run minted.
#[tokio::test]
async fn every_resolution_handle_is_opaque_and_within_the_modelled_length() {
    let router = emulator();
    let Minted {
        handles,
        resource_ids,
        request_values,
    } = minted(&router).await;

    assert!(
        handles.len() >= 8,
        "the table must span both applications, all three version contents, a \
         repeated resolve and a published version, got {handles:?}"
    );

    for handle in &handles {
        let Handle { case, text, .. } = handle;
        assert!(
            RESOLUTION_ID_LENGTH.contains(&text.chars().count()),
            "case {case:?}: a handle must satisfy String255's modelled length \
             {RESOLUTION_ID_LENGTH:?}, got {} characters ({text:?})",
            text.chars().count()
        );
        for supplied in &request_values {
            assert!(
                !text.contains(supplied.as_str()),
                "case {case:?}: a handle must carry nothing the request supplied, \
                 yet {text:?} carries {supplied:?}"
            );
        }
        for region_or_account in [REGION, ACCOUNT] {
            assert!(
                !text.contains(region_or_account),
                "case {case:?}: a handle must carry no part of the caller's scope, \
                 yet {text:?} carries {region_or_account:?}"
            );
        }
        assert_ne!(
            text, &handle.app_version,
            "case {case:?}: a handle must not be the app version it resolves"
        );
        assert!(
            !resource_ids.contains(text),
            "case {case:?}: a handle must not be one of the resource ids this run \
             minted ({resource_ids:?}), got {text:?}"
        );
    }
}

/// No two resolutions share a handle — not two resolutions of one version, not
/// resolutions of two versions, and not resolutions of two identically composed
/// applications.
#[tokio::test]
async fn every_resolution_gets_its_own_handle() {
    let router = emulator();
    let Minted { handles, .. } = minted(&router).await;

    let distinct: BTreeSet<&str> = handles.iter().map(|handle| handle.text.as_str()).collect();
    assert_eq!(
        distinct.len(),
        handles.len(),
        "every started resolution must get its own handle, got {:?}",
        handles
            .iter()
            .map(|handle| (handle.case.clone(), handle.text.clone()))
            .collect::<Vec<(String, String)>>()
    );
}

/// A handle ADDRESSES its resolution: every resolution-scoped read of the app
/// version it was minted for accepts it back and reports it, as often as it is
/// asked.
#[tokio::test]
async fn a_handle_addresses_its_resolution_on_every_read_that_takes_one() {
    let router = emulator();
    let Minted { handles, .. } = minted(&router).await;

    for Handle {
        case,
        app_arn,
        app_version,
        text,
    } in &handles
    {
        // Twice, because a read may not change what the next reader sees.
        for read in ["the first read", "the second read"] {
            let described = describe(&router, app_arn, app_version, Some(text)).await;
            assert_eq!(
                described.ok(case)["resolutionId"],
                json!(text),
                "case {case:?} ({read}): the status read must answer the handle it \
                 was given, got {:?}",
                described.body
            );
        }
        let listed = list_against(&router, app_arn, app_version, text).await;
        assert_eq!(
            listed.ok(case)["resolutionId"],
            json!(text),
            "case {case:?}: the resource listing against a handle must answer that \
             handle, got {:?}",
            listed.body
        );
    }
}

/// The CURRENT resolution of an app version is the last handle minted for it —
/// which is what a status read that names no handle answers.
#[tokio::test]
async fn the_current_resolution_is_the_last_handle_minted_for_that_version() {
    let router = emulator();
    let Minted { handles, .. } = minted(&router).await;

    let mut expected: Vec<(&str, &str, &str)> = Vec::new();
    for handle in &handles {
        let scope = (handle.app_arn.as_str(), handle.app_version.as_str());
        match expected
            .iter_mut()
            .find(|(app_arn, app_version, _)| (*app_arn, *app_version) == scope)
        {
            Some(entry) => entry.2 = handle.text.as_str(),
            None => expected.push((scope.0, scope.1, handle.text.as_str())),
        }
    }

    for (app_arn, app_version, last) in expected {
        let case = format!("the current resolution of {app_version} of {app_arn}");
        let described = describe(&router, app_arn, app_version, None).await;
        assert_eq!(
            described.ok(&case)["resolutionId"],
            json!(last),
            "case {case:?}: a read naming no handle must answer the last handle \
             minted for that version, got {:?}",
            described.body
        );
    }
}

/// A handle is scoped to the app version it was minted for, and nothing else this
/// service mints is a handle: another version's handle, another application's
/// handle, an application's or a policy's own id and a handle nobody minted are
/// each the modelled 404 — on the status read and on the listing alike.
#[tokio::test]
async fn only_a_handle_of_the_addressed_app_version_resolves() {
    let router = emulator();
    let Minted {
        handles,
        resource_ids,
        ..
    } = minted(&router).await;

    let first = &handles[0];
    let elsewhere = handles
        .iter()
        .find(|handle| handle.app_arn != first.app_arn)
        .expect("the table covers two applications");
    let published = handles
        .iter()
        .find(|handle| handle.app_version != DRAFT_APP_VERSION)
        .expect("the table covers a published version");

    let mut refused: Vec<(String, String, String, String)> = vec![
        (
            "another application's handle".to_string(),
            first.app_arn.clone(),
            first.app_version.clone(),
            elsewhere.text.clone(),
        ),
        (
            "another version's handle".to_string(),
            first.app_arn.clone(),
            published.app_version.clone(),
            first.text.clone(),
        ),
        (
            "a handle nobody minted".to_string(),
            first.app_arn.clone(),
            first.app_version.clone(),
            "00000000-0000-4000-8000-000000000000".to_string(),
        ),
    ];
    for resource_id in &resource_ids {
        refused.push((
            format!("the resource id {resource_id:?}"),
            first.app_arn.clone(),
            first.app_version.clone(),
            resource_id.clone(),
        ));
    }

    for (case, app_arn, app_version, resolution_id) in refused {
        describe(&router, &app_arn, &app_version, Some(&resolution_id))
            .await
            .refused(&format!("the status read of {case}"), NOT_FOUND.0, NOT_FOUND.1);
        list_against(&router, &app_arn, &app_version, &resolution_id)
            .await
            .refused(&format!("the listing against {case}"), NOT_FOUND.0, NOT_FOUND.1);
    }

    // The handle itself still resolves, so the refusals above are about SCOPE, not
    // about a resolution the service forgot.
    let described = describe(&router, &first.app_arn, &first.app_version, Some(&first.text)).await;
    assert_eq!(
        described.ok(&first.case)["resolutionId"],
        json!(first.text),
        "case {:?}: the handle must still address its own resolution, got {:?}",
        first.case,
        described.body
    );
}
