// SPDX-License-Identifier: CC0-1.0

//! The DOMAIN an app's hosting is announced under: `App$defaultDomain`, and whether the
//! emulator answers at the address a client composes from it.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a restJson1
//! request signed for the `amplify` signing name, then a plain unsigned `GET` of the site
//! — so what is asserted is what a client that was told where its site is sees.
//!
//! The rule under test: `App$defaultDomain` is "the default domain for the Amplify app"
//! (`smithy_query(query="doc",shape="App",member="defaultDomain")`) and
//! `Branch$displayName` "is used as the default domain prefix"
//! (`(shape="Branch",member="displayName")`), so the member is a name a client RESOLVES and
//! then DIALS — it composes its site's address by prefixing the branch's label to it. A name
//! a service hands a client therefore has to lead back to THIS emulator: the address this
//! emulator answers on, which is the endpoint the create reached, and AWS's own hosting zone
//! only for a caller that resolves AWS's own hosts on an emulator that is served at no
//! address of its own — the one case where there is no address of this process to name.
//!
//! So the value is an address and nothing else — no scheme and no path — resolved ONCE when
//! the app is created and reported unchanged by every later read, whichever endpoint that
//! read is addressed at. A client that resolves nothing but one local endpoint cannot dial a
//! branch label under it, so the same site is ALSO served under the path that names the app
//! and the branch.
//!
//! The table below spans the class that rule generalises over: every spelling of the
//! emulator's own endpoint a client is configured with (the IPv4 loopback address, the
//! loopback name, a bracketed IPv6 literal, each on its own port, and one naming no port),
//! and the service's own published host beside them. Each row asserts BOTH halves — the
//! address reported, and that a `GET` of the address built from it is answered by the site
//! that branch deployed — because the two together are what a client told the domain can do.
//!
//! The harness (`Emulator`) and the deployment driver (`Hosted`) are the ones
//! `app_branch_lifecycle` and `hosted_branch_site` already drive.

use serde_json::{json, Value};

use crate::app_branch_lifecycle::{announced_domain, list_member, string_member, Emulator, REGION};
use crate::hosted_branch_site::{Hosted, SUCCEED};

/// The root document the rows deploy, so a `GET` of the site has something to answer with.
const ROOT_DOCUMENT: &str = "index.html";

/// The path prefix an app announced on this emulator's own endpoint carries: the reserved
/// data-plane marker and the label naming the hosting plane. Written here independently of
/// the service, so the suite pins the address and not the code.
const LOCAL_MARKER: &str = "/_aws/amplify";

/// One row: the endpoint a client was pointed at, and what an app created through it
/// announces — the class the rule generalises over.
struct Row {
    /// What the row is, named in every assertion.
    case: &'static str,
    /// The `Host` the control-plane calls address.
    endpoint: &'static str,
    /// Whether a branch label can be written in front of the announced address, which is
    /// what a client composing a site's HOST does. A bracketed IPv6 authority is no such
    /// address — a label ahead of `[::1]:38199` is not a host a client can write — so that
    /// row asserts the PATH addressing alone, which is the addressing a client holding one
    /// local endpoint uses anyway.
    host_style: bool,
}

/// The rows: every addressing a client of this emulator uses, and the service's own
/// published host beside them.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the IPv4 loopback address with a port",
            endpoint: "127.0.0.1:44285",
            host_style: true,
        },
        Row {
            case: "the loopback name with another port",
            endpoint: "localhost:9297",
            host_style: true,
        },
        Row {
            case: "a bracketed IPv6 loopback",
            endpoint: "[::1]:38199",
            host_style: false,
        },
        Row {
            case: "the loopback address naming no port",
            endpoint: "127.0.0.1",
            host_style: true,
        },
        Row {
            case: "this service's own published host",
            endpoint: "amplify.us-east-1.amazonaws.com",
            host_style: true,
        },
    ]
}

/// The `app` record `GetApp` reports, addressed at `endpoint`.
async fn read_app(emulator: &Emulator, endpoint: &str, app_id: &str) -> Value {
    let reply = emulator
        .call_at(endpoint, "GET", &format!("/apps/{app_id}"), None)
        .await;
    assert_eq!(
        reply.status, 200,
        "app {app_id:?}: expected HTTP 200 from GetApp, got {reply:?}"
    );
    reply.member("app")
}

/// Rule: an app's default domain is an address that leads back to this emulator — the
/// endpoint the create reached — with AWS's own hosting zone standing only where this
/// emulator has no address of its own to name; and the site of a branch of that app is
/// served at the address a client composes from it, host-style with the branch's label ahead
/// of that address, and under the path that names the app and the branch for a client that
/// resolves only this emulator's own endpoint.
#[tokio::test]
async fn an_app_announces_an_address_that_leads_back_to_this_emulator() {
    for row in rows() {
        let case = row.case;
        let emulator = Emulator::new();
        let created = emulator
            .call_at(
                row.endpoint,
                "POST",
                "/apps",
                Some(&json!({ "name": format!("hosting-{case}") })),
            )
            .await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: expected HTTP 200 from CreateApp, got {created:?}"
        );
        let app = created.member("app");
        let app_id = string_member(&app, "appId");
        let reported = string_member(&app, "defaultDomain");

        let expected = announced_domain(row.endpoint, &app_id);
        assert_eq!(
            reported, expected,
            "case {case:?}: expected the announced address {expected:?}, got {reported:?}"
        );
        // The announced value is an ADDRESS and nothing else: a name a branch label is
        // prefixed to, carrying no scheme and no path.
        assert!(
            !reported.contains('/') && !reported.contains(' '),
            "case {case:?}: an address carries no scheme and no path, got {reported:?}"
        );

        // The same value is reported by every reader, and by one addressed somewhere
        // else: it is the app's record, resolved once by the create, not a function of the
        // reading request.
        let read_back = read_app(&emulator, row.endpoint, &app_id).await;
        assert_eq!(
            string_member(&read_back, "defaultDomain"),
            reported,
            "case {case:?}: GetApp must report the app's domain, got {read_back:?}"
        );
        let elsewhere = read_app(&emulator, &format!("amplify.{REGION}.amazonaws.com"), &app_id)
            .await;
        assert_eq!(
            string_member(&elsewhere, "defaultDomain"),
            reported,
            "case {case:?}: a read addressed elsewhere must report the same domain, got \
             {elsewhere:?}"
        );
        let listed = emulator.list_apps("").await;
        let first = list_member(&listed.body, "apps")
            .iter()
            .find(|listed| listed.get("appId").and_then(Value::as_str) == Some(app_id.as_str()))
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: ListApps must report the created app"));
        assert_eq!(
            string_member(&first, "defaultDomain"),
            reported,
            "case {case:?}: ListApps must report the app's domain, got {first:?}"
        );

        // The address a client composes from the two members is the address the hosting
        // edge serves: a branch of that app, deployed to, answers there.
        let display_name = "main";
        let hosted = Hosted::adopt(
            emulator,
            app_id.clone(),
            &json!({ "branchName": display_name }),
        )
        .await;
        let body = format!("<html>{case}</html>").into_bytes();
        let status = hosted
            .deploy(case, &[(ROOT_DOCUMENT, body.as_slice())])
            .await;
        assert_eq!(
            status, SUCCEED,
            "case {case:?}: the deployment must succeed, got {status}"
        );
        // BOTH addressings answer, whatever endpoint the app was created at: the host a
        // client composes from the two members, and the path a client that resolves only
        // this emulator's own endpoint reaches the same site under.
        let mut addressings = vec![(
            "the path this plane serves the site under",
            row.endpoint.to_string(),
            format!("{LOCAL_MARKER}/{app_id}/{display_name}/"),
            row.endpoint.to_string(),
            format!("{LOCAL_MARKER}/{app_id}/another-branch/"),
        )];
        if row.host_style {
            addressings.push((
                "the host composed from the two members",
                format!("{display_name}.{reported}"),
                "/".to_string(),
                format!("another-branch.{reported}"),
                "/".to_string(),
            ));
        }
        for (addressing, visited_host, visited_path, other_host, other_path) in addressings {
            let visited = hosted
                .visit_host("GET", &visited_host, &visited_path)
                .await;
            assert_eq!(
                visited.status, 200,
                "case {case:?} via {addressing:?}: a GET of {visited_host:?}{visited_path:?} \
                 must be answered, got {visited:?}"
            );
            assert_eq!(
                visited.body, body,
                "case {case:?} via {addressing:?}: expected the deployed document, got {:?}",
                String::from_utf8_lossy(&visited.body)
            );

            // And nothing else is served there: a branch label this app has no branch
            // under is not served at all, so the edge answers for the branch the address
            // names rather than for the app. What such a request earns instead is the
            // framework's, and depends on the endpoint it was addressed at, so what is
            // pinned here is that the site was not served.
            let missed = hosted.visit_host("GET", &other_host, &other_path).await;
            assert_ne!(
                missed.status, 200,
                "case {case:?} via {addressing:?}: a branch this app does not have must not \
                 be served, got {missed:?}"
            );
            assert_ne!(
                missed.body, body,
                "case {case:?} via {addressing:?}: a branch this app does not have must not \
                 answer this site's document"
            );
        }
    }
}

/// Rule: the announced address follows the ENDPOINT, not the request that created the app,
/// so every app created through one endpoint is announced that one address — and each app's
/// site is still addressed on its own, because the app is named in the PATH the plane serves
/// its sites under. A caller that resolves AWS's own hosts, which has no address of this
/// emulator to be told, is announced its app's own id under AWS's hosting zone instead, so
/// those names differ per app.
///
/// The rows differ along what the rule generalises over — how many apps one emulator holds
/// and the names they were created with (a plain name, one carrying digits and hyphens, one
/// repeated so two apps differ in nothing a request stated) — because the announced address
/// follows neither the app's name nor anything else a request carried.
#[tokio::test]
async fn every_app_created_through_one_endpoint_announces_that_endpoints_address() {
    let names: &[&str] = &[
        "hosting-plain",
        "hosting-2nd-app-9",
        "hosting-plain",
        "hosting-plain",
    ];
    // The two kinds of caller: one configured with this emulator's own endpoint, and one
    // resolving AWS's own hosts on an emulator served at no address of its own.
    for endpoint in ["127.0.0.1:44285", "amplify.us-east-1.amazonaws.com"] {
        let emulator = Emulator::new();
        let mut announced: Vec<(String, String)> = Vec::new();
        for (index, name) in names.iter().enumerate() {
            let case = format!("app {index} named {name:?} at {endpoint}");
            let created = emulator
                .call_at(endpoint, "POST", "/apps", Some(&json!({ "name": name })))
                .await;
            assert_eq!(
                created.status, 200,
                "case {case}: expected HTTP 200 from CreateApp, got {created:?}"
            );
            let app = created.member("app");
            let app_id = string_member(&app, "appId");
            let reported = string_member(&app, "defaultDomain");
            assert_eq!(
                reported,
                announced_domain(endpoint, &app_id),
                "case {case}: expected the address this endpoint announces, got {reported:?}"
            );
            announced.push((app_id, reported));
        }

        // Every one of them is still reported, unchanged, by a later read: the address is
        // the app's own record rather than anything about the reading request. And each
        // app's site keeps an address of its own, because the PATH names the app — whatever
        // the announced domain is.
        for (app_id, reported) in &announced {
            let read_back = read_app(&emulator, &format!("amplify.{REGION}.amazonaws.com"), app_id)
                .await;
            assert_eq!(
                &string_member(&read_back, "defaultDomain"),
                reported,
                "case app {app_id}: a later read must report the same address, got {read_back:?}"
            );
            assert!(
                announced
                    .iter()
                    .filter(|(other_id, _)| other_id != app_id)
                    .all(|(other_id, _)| !format!("{LOCAL_MARKER}/{app_id}/")
                        .starts_with(&format!("{LOCAL_MARKER}/{other_id}/"))),
                "case app {app_id}: the path this plane serves its sites under names one app \
                 only, got {announced:?}"
            );
        }
    }
}
