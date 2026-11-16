// SPDX-License-Identifier: CC0-1.0

//! A domain's DATA PORTAL address, over the whole class of endpoints this emulator can be
//! served at.
//!
//! RULE: every response that reports a domain's data portal URL (`CreateDomain`'s and
//! `GetDomain`'s `portalUrl`, and each `ListDomains` summary's) names the endpoint THIS
//! emulator is served at, with the domain's own id in the path — never a host AWS publishes
//! and this emulator does not answer on — and a fetch of that address is served here, from the
//! stored domain.
//!
//! The rows span what the rule generalises over: the FORM of the address the emulator answers
//! on (a loopback literal with a port, a name with another port, a bracketed IPv6 literal, and
//! the base of an emulator served on no port at all) and the domains whose portal is published
//! (several per emulator, one of them named with characters a document has to escape).

use super::harness::Emulator;
use serde_json::{json, Value};

/// The endpoint bases this emulator can report as its own: the forms its data port is
/// published at, plus the base of an emulator embedded in another process
/// (`smithy_server_runtime::UNSERVED_URL_BASE`). One row each, because the published portal
/// address has to follow whichever one this emulator is reached at.
const ENDPOINT_BASES: [&str; 4] = [
    "https://127.0.0.1:9297",
    "https://localhost:39355",
    "https://[::1]:8443",
    smithy_server_runtime::UNSERVED_URL_BASE,
];

/// The DNS zones AWS publishes its own endpoints in, and the portal host it serves a domain's
/// data portal on: a published address must name none of them, because this emulator answers
/// at none of them.
const FOREIGN_HOST_MARKERS: [&str; 3] = [".on.aws", ".amazonaws.com", ".api.aws"];

/// The domains each emulator in this suite publishes a portal for. The third name carries the
/// characters a markup document has to escape, which the portal page is checked against.
fn domain_bodies() -> Vec<(&'static str, Value)> {
    vec![
        ("a plain name", json!({ "name": "Sales" })),
        (
            "a described domain",
            json!({ "name": "analytics-engineering", "description": "the analytics catalog" }),
        ),
        (
            "a name carrying markup characters",
            json!({ "name": "Sales & <Marketing>" }),
        ),
    ]
}

/// The address a domain's portal is published at, spelled out here rather than taken from the
/// service: the reserved namespace segment every emulator-served plane is addressed under, the
/// segment naming this service's portal plane, and the domain's own id.
fn published_portal_url(base: &str, domain_id: &str) -> String {
    format!("{base}/_aws/datazone-portal/{domain_id}")
}

/// Create the suite's domains on `dz` and answer `(case, id, name, portalUrl)` for each,
/// asserting as it goes that the create reported the address `base` serves.
async fn created_domains(dz: &Emulator, base: &str) -> Vec<(&'static str, String, String, String)> {
    let mut created = Vec::new();
    for (case, body) in domain_bodies() {
        let answer = dz.create_domain(body.clone()).await;
        assert_eq!(
            answer.status, 201,
            "case {base:?}/{case:?}: expected 201 from CreateDomain, got {} {:?}",
            answer.status, answer.body
        );
        let id = answer.s("id");
        let name = body
            .get("name")
            .and_then(Value::as_str)
            .expect("the request names the domain")
            .to_string();
        let expected = published_portal_url(base, &id);
        assert_eq!(
            answer.s("portalUrl"),
            expected,
            "case {base:?}/{case:?}: expected CreateDomain to report the portal on this \
             emulator's own endpoint ({expected:?}), got {:?}",
            answer.s("portalUrl")
        );
        created.push((case, id, name, expected));
    }
    created
}

/// RULE: `CreateDomain`, `GetDomain` and every `ListDomains` summary report one and the same
/// portal address for a domain, on the endpoint this emulator is served at, with no host of
/// AWS's own in it.
#[tokio::test]
async fn every_reported_portal_url_names_this_emulators_own_endpoint() {
    for base in ENDPOINT_BASES {
        let dz = Emulator::served_at(base);
        let created = created_domains(&dz, base).await;

        for (case, id, _name, expected) in &created {
            for marker in FOREIGN_HOST_MARKERS {
                assert!(
                    !expected.contains(marker),
                    "case {base:?}/{case:?}: a published portal URL must name no AWS host, \
                     got {expected:?}"
                );
            }
            let got = dz.call("GET", &format!("/v2/domains/{id}"), None).await;
            assert_eq!(
                got.status, 200,
                "case {base:?}/{case:?}: expected 200 from GetDomain, got {} {:?}",
                got.status, got.body
            );
            assert_eq!(
                got.s("portalUrl"),
                *expected,
                "case {base:?}/{case:?}: expected GetDomain to report the same portal URL \
                 ({expected:?}), got {:?}",
                got.s("portalUrl")
            );
        }

        let listed = dz.call("GET", "/v2/domains", None).await;
        assert_eq!(
            listed.status, 200,
            "case {base:?}: expected 200 from ListDomains, got {} {:?}",
            listed.status, listed.body
        );
        let items = listed
            .body
            .get("items")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            items.len(),
            created.len(),
            "case {base:?}: expected every created domain to be listed, got {:?}",
            listed.body
        );
        for (case, id, _name, expected) in &created {
            let summary = items
                .iter()
                .find(|item| item.get("id").and_then(Value::as_str) == Some(id.as_str()))
                .unwrap_or_else(|| {
                    panic!("case {base:?}/{case:?}: {id} is missing from {items:?}")
                });
            let reported = summary.get("portalUrl").and_then(Value::as_str);
            assert_eq!(
                reported,
                Some(expected.as_str()),
                "case {base:?}/{case:?}: expected the listed summary to carry the same portal \
                 URL ({expected:?}), got {reported:?}"
            );
        }
    }
}

/// RULE: the published portal address is served BY this emulator, from the stored domain: a
/// fetch of it answers that domain's portal page, and a portal address of a domain this
/// emulator does not hold answers that there is none.
#[tokio::test]
async fn a_published_portal_url_is_served_by_this_emulator() {
    for base in ENDPOINT_BASES {
        let dz = Emulator::served_at(base);
        for (case, id, name, url) in created_domains(&dz, base).await {
            let page = dz.fetch("GET", &url).await;
            assert_eq!(
                page.status, 200,
                "case {base:?}/{case:?}: expected the published portal URL {url:?} to be served \
                 here, got {} {:?}",
                page.status, page.body
            );
            assert!(
                page.content_type.starts_with("text/html"),
                "case {base:?}/{case:?}: expected a portal page, got content type {:?}",
                page.content_type
            );
            assert!(
                page.body.contains(&id),
                "case {base:?}/{case:?}: expected the page to name the domain {id:?}, got {:?}",
                page.body
            );
            // Every stored string reaches the document escaped, so a name carrying markup
            // characters cannot open or close an element of the page it is written into.
            let escaped = name
                .replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;");
            assert!(
                page.body.contains(&escaped),
                "case {base:?}/{case:?}: expected the page to name the domain {escaped:?}, got \
                 {:?}",
                page.body
            );
            if escaped != name {
                assert!(
                    !page.body.contains(&name),
                    "case {base:?}/{case:?}: expected {name:?} to be escaped in the page, got \
                     {:?}",
                    page.body
                );
            }

            // What a client asks when it wants only to know the portal is there.
            let head = dz.fetch("HEAD", &url).await;
            assert_eq!(
                head.status, 200,
                "case {base:?}/{case:?}: expected a HEAD of {url:?} to answer 200, got {}",
                head.status
            );
            assert!(
                head.body.is_empty(),
                "case {base:?}/{case:?}: expected a HEAD to carry no body, got {:?}",
                head.body
            );

            // A portal address of this shape that names no domain of this emulator: the page
            // says there is none, rather than reporting a portal nothing stands behind.
            let absent = published_portal_url(base, &format!("{id}-no-such-domain"));
            let missing = dz.fetch("GET", &absent).await;
            assert_eq!(
                missing.status, 404,
                "case {base:?}/{case:?}: expected no portal at {absent:?}, got {} {:?}",
                missing.status, missing.body
            );
            assert!(
                missing.content_type.starts_with("text/html"),
                "case {base:?}/{case:?}: expected the portal route itself to answer for \
                 {absent:?}, got content type {:?}",
                missing.content_type
            );
        }
    }
}

/// RULE: the page is rendered from the domain's own record, so it reports the status that
/// record currently carries and stops being served once the domain is gone.
#[tokio::test]
async fn the_portal_page_follows_the_stored_domain() {
    let base = ENDPOINT_BASES[0];
    let dz = Emulator::served_at(base);
    let created = dz
        .create_domain(json!({ "name": "Sales" }))
        .await;
    assert_eq!(
        created.status, 201,
        "creating the domain: expected 201, got {} {:?}",
        created.status, created.body
    );
    let id = created.s("id");
    let url = published_portal_url(base, &id);

    // The clock owes this domain its transition to AVAILABLE. Wait for the CONDITION, not for
    // a span: the page reports whatever the record says at the instant it is fetched.
    let settled = poll_until(&dz, &url, "AVAILABLE").await;
    assert!(
        settled,
        "expected the portal page at {url:?} to report the domain's settled status"
    );

    let deleted = dz.call("DELETE", &format!("/v2/domains/{id}"), None).await;
    assert_eq!(
        deleted.status, 202,
        "deleting the domain: expected 202, got {} {:?}",
        deleted.status, deleted.body
    );
    // Once the deletion the call started has completed, the domain is gone — and so is its
    // portal, because the page is the stored record and nothing else.
    let gone = poll_until_status(&dz, &url, 404).await;
    assert!(
        gone,
        "expected the portal at {url:?} to stop being served once the domain is deleted"
    );
}

/// How long a poll of a clock-owed transition keeps looking: generous, so a loaded or slow
/// host still reaches the condition, and the test never asserts on how long it took.
const POLL_DEADLINE: std::time::Duration = std::time::Duration::from_secs(60);

/// How long one poll waits before looking again.
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(20);

/// Whether the portal page at `url` comes to contain `wanted` before the deadline.
async fn poll_until(dz: &Emulator, url: &str, wanted: &str) -> bool {
    let deadline = std::time::Instant::now() + POLL_DEADLINE;
    loop {
        if dz.fetch("GET", url).await.body.contains(wanted) {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Whether a fetch of `url` comes to answer `wanted` before the deadline.
async fn poll_until_status(dz: &Emulator, url: &str, wanted: u16) -> bool {
    let deadline = std::time::Instant::now() + POLL_DEADLINE;
    loop {
        if dz.fetch("GET", url).await.status == wanted {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}
