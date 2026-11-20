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
//! (`(shape="Branch",member="displayName")`), so the member is the DOMAIN the app's sites
//! are published under and a client composes a site's address by prefixing the branch's
//! label to it. This service GENERATES that domain from the one thing that names the app —
//! its own id — as a label in the zone this emulator may announce a name of its own in, so
//! the LABEL follows the app, and the zone leads back to this process rather than to one AWS
//! serves.
//!
//! So the value is a domain name and nothing else — no scheme, no path, no port — composed
//! ONCE when the app is created and reported unchanged by every later read, whichever
//! endpoint that read is addressed at. A client that resolves nothing but this emulator's own
//! endpoint cannot dial a DNS name at all, so the same site is ALSO served under the path
//! that names the app and the branch.
//!
//! The table below spans the class that rule generalises over: every spelling of the
//! emulator's own endpoint a client is configured with (the IPv4 loopback address, the
//! loopback name, a bracketed IPv6 literal, each on its own port, and one naming no port),
//! and the service's own published host beside them — none of which moves the app's own
//! label. Each row asserts BOTH halves — the domain reported, and that a `GET` of the
//! address built from it is answered by the site that branch deployed — because the two
//! together are what a client told the domain can do.
//! The suite's last two tests assert the FORM of that domain over a table of app names, and
//! that every app of one emulator announces a domain of its own.
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
}

/// The rows: every addressing a client of this emulator uses, and the service's own
/// published host beside them.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the IPv4 loopback address with a port",
            endpoint: "127.0.0.1:44285",
        },
        Row {
            case: "the loopback name with another port",
            endpoint: "localhost:9297",
        },
        Row {
            case: "a bracketed IPv6 loopback",
            endpoint: "[::1]:38199",
        },
        Row {
            case: "the loopback address naming no port",
            endpoint: "127.0.0.1",
        },
        Row {
            case: "this service's own published host",
            endpoint: "amplify.us-east-1.amazonaws.com",
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

/// Rule: an app's default domain is the DNS name generated from its own id in a zone that
/// leads back to this emulator, and the site of
/// a branch of that app is served at the address a client composes from it, host-style with
/// the branch's label ahead of that domain, and under the path that names the app and the
/// branch for a client that resolves only this emulator's own endpoint.
#[tokio::test]
async fn an_app_announces_the_domain_generated_from_its_own_id() {
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

        let expected = announced_domain(&app_id, row.endpoint);
        assert_eq!(
            reported, expected,
            "case {case:?}: expected the announced domain {expected:?}, got {reported:?}"
        );
        // The announced value is a DOMAIN and nothing else: a name a branch label is
        // prefixed to, carrying no scheme and no path.
        assert!(
            !reported.contains('/') && !reported.contains(' '),
            "case {case:?}: a domain carries no scheme and no path, got {reported:?}"
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
        let addressings = vec![
            (
                "the path this plane serves the site under",
                row.endpoint.to_string(),
                format!("{LOCAL_MARKER}/{app_id}/{display_name}/"),
                row.endpoint.to_string(),
                format!("{LOCAL_MARKER}/{app_id}/another-branch/"),
            ),
            (
                "the host composed from the two members",
                format!("{display_name}.{reported}"),
                "/".to_string(),
                format!("another-branch.{reported}"),
                "/".to_string(),
            ),
        ];
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

/// Rule: the announced domain follows the APP and nothing a request carried, so two apps of
/// one emulator announce two domains — and each app's site is also addressed on its own by the
/// PATH the plane serves its sites under, which names the app.
///
/// The rows differ along what the rule generalises over — how many apps one emulator holds
/// and the names they were created with (a plain name, one carrying digits and hyphens, one
/// repeated so two apps differ in nothing a request stated) — because the announced domain
/// follows neither the app's name nor anything else a request carried.
#[tokio::test]
async fn every_app_announces_its_own_domain_whichever_endpoint_created_it() {
    let names: &[&str] = &[
        "hosting-plain",
        "hosting-2nd-app-9",
        "hosting-plain",
        "hosting-plain",
    ];
    // The two kinds of caller: one configured with this emulator's own endpoint, and one
    // resolving AWS's own hosts. Neither moves what an app announces.
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
                announced_domain(&app_id, endpoint),
                "case {case}: expected the domain this app announces, got {reported:?}"
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

/// Rule: the announced domain is a DOMAIN NAME and nothing else — no scheme, no path, no
/// credential, no port — generated from the app's own id, which is the form a client prefixes
/// a branch's label to.
///
/// The rows differ along the one thing a request can vary that the member might have
/// followed — the app's `name` (a plain one, one carrying digits, hyphens and mixed case,
/// the shortest and the longest the member allows) — crossed with the endpoint the create
/// was addressed at, which the domain does not follow either.
#[tokio::test]
async fn an_apps_default_domain_is_the_name_generated_from_its_id() {
    // "The name of the Amplify app" is `length 1..255`
    // (`smithy_query(query="shape",shape="CreateAppRequest")`), so the rows below are the
    // ends of that range beside two ordinary names.
    let names: Vec<(&str, String)> = vec![
        ("a plain name", "hosting-plain".to_string()),
        (
            "a name carrying digits, hyphens and mixed case",
            "Hosting-2nd-App-9".to_string(),
        ),
        ("the shortest name a request may send", "a".to_string()),
        ("the longest name a request may send", "n".repeat(255)),
    ];
    for (name_case, name) in names {
        for row in rows() {
            let case = format!("{name_case} at {}", row.endpoint);
            let emulator = Emulator::new();
            let created = emulator
                .call_at(row.endpoint, "POST", "/apps", Some(&json!({ "name": name })))
                .await;
            assert_eq!(
                created.status, 200,
                "case {case:?}: expected HTTP 200 from CreateApp, got {created:?}"
            );
            let app = created.member("app");
            let app_id = string_member(&app, "appId");
            let reported = string_member(&app, "defaultDomain");

            // The DNS name this service generates for this app: its own id as a label in the
            // zone this emulator may announce in, whatever else the request carried.
            let expected = announced_domain(&app_id, row.endpoint);
            assert_eq!(
                reported, expected,
                "case {case:?}: expected the generated domain {expected:?}, got {reported:?}"
            );
            // A DOMAIN NAME and nothing else: no scheme, no path, no credential and no port.
            for carried in ['/', ' ', '@', '?', ':'] {
                assert!(
                    !reported.contains(carried),
                    "case {case:?}: a domain carries no {carried:?}, got {reported:?}"
                );
            }
            // ROUND TRIP: the same address is what a later read of the app reports.
            let read_back = read_app(&emulator, row.endpoint, &app_id).await;
            assert_eq!(
                string_member(&read_back, "defaultDomain"),
                reported,
                "case {case:?}: GetApp must report the app's own domain, got {read_back:?}"
            );
        }
    }
}

/// Rule, read the other way: the domain names the APP, so every app of one emulator
/// announces a domain of its own — and each app's site is also addressed by the PATH this
/// plane serves its sites under, which names the app.
#[tokio::test]
async fn every_app_of_one_emulator_announces_a_domain_of_its_own() {
    let emulator = Emulator::new();
    let endpoint = "127.0.0.1:44285";
    let mut announced: Vec<(String, String)> = Vec::new();
    for index in 0..4 {
        let case = format!("app {index}");
        let created = emulator
            .call_at(
                endpoint,
                "POST",
                "/apps",
                Some(&json!({ "name": "hosting-sibling" })),
            )
            .await;
        assert_eq!(
            created.status, 200,
            "case {case}: expected HTTP 200 from CreateApp, got {created:?}"
        );
        let app = created.member("app");
        announced.push((
            string_member(&app, "appId"),
            string_member(&app, "defaultDomain"),
        ));
    }
    for (app_id, domain) in &announced {
        let expected = announced_domain(app_id, endpoint);
        assert_eq!(
            domain, &expected,
            "case app {app_id}: every app announces the domain generated from its own id, \
             got {announced:?}"
        );
        // No two apps of one emulator announce one domain, and each app keeps a site
        // address of its own, because the path names the app too.
        assert!(
            announced
                .iter()
                .filter(|(other_id, _)| other_id != app_id)
                .all(|(other_id, other_domain)| other_domain != domain
                    && !format!("{LOCAL_MARKER}/{app_id}/")
                        .starts_with(&format!("{LOCAL_MARKER}/{other_id}/"))),
            "case app {app_id}: one domain names one app, got {announced:?}"
        );
    }
}
