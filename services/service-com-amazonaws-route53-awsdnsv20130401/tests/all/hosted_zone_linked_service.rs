// SPDX-License-Identifier: CC0-1.0

//! `HostedZone$LinkedService`: which hosted zones report another service as their creator.
//!
//! RULE: a hosted zone another emulated service created for one of its own resources — a
//! Cloud Map private DNS namespace is the standing case — reports that service on every
//! hosted-zone read: `LinkedService.ServicePrincipal` is the creating service's principal and
//! `LinkedService.Description` the resource it created the zone for. A zone a client created
//! for itself reports no linked service at all, and nothing a request carries can claim one:
//! the marker is recorded by the creating service on the zone whose id THIS service minted
//! (`state::managed_zone`), so the rows below differ along which zone is marked and whether
//! the id named is one this service holds.
//!
//! Every request goes through the real router, as `hosted_zone_records` does, so what is
//! asserted is the restXml a Route 53 client reads.

use std::sync::Arc;

use bytes::Bytes;
use service_com_amazonaws_route53_awsdnsv20130401::state::managed_zone;
use smithy_server_runtime::MultiServiceRouter;

use crate::hosted_zone_records::ENDPOINT;

/// The service principal Cloud Map creates a private DNS namespace's zone as.
const CLOUD_MAP_PRINCIPAL: &str = "servicediscovery.amazonaws.com";

/// The namespace a marked zone was created for, which the linked service describes.
const NAMESPACE_ARN: &str = "arn:aws:servicediscovery:us-west-2:000000000000:namespace/ns-linked";

#[tokio::test]
async fn a_zone_reports_the_service_that_created_it_and_no_other() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.register_service(
        service_com_amazonaws_route53_awsdnsv20130401::EmulatedService::new(Arc::clone(&engine)),
    );

    // Two zones, created through this service's own API, so each id is one Route 53 minted.
    let mut zone_ids = Vec::new();
    for (index, name) in ["linked.example.com", "client-owned.example.com"]
        .into_iter()
        .enumerate()
    {
        let body = format!(
            "<CreateHostedZoneRequest xmlns=\"https://route53.amazonaws.com/doc/2013-04-01/\">\
             <Name>{name}</Name><CallerReference>linked-{index}</CallerReference>\
             </CreateHostedZoneRequest>"
        );
        let (status, created) = call(&router, "POST", "/2013-04-01/hostedzone", &body).await;
        assert_eq!(
            status, 201,
            "case {name:?}: CreateHostedZone must answer 201, got {status} with {created}"
        );
        assert!(
            !created.contains("<LinkedService>"),
            "case {name:?}: a zone this create just minted is managed by nobody, got {created}"
        );
        zone_ids.push(
            between(&created, "<Id>/hostedzone/", "</Id>")
                .unwrap_or_else(|| panic!("case {name:?}: expected the zone's Id, got {created}"))
                .to_string(),
        );
    }

    // The FIRST zone is marked as Cloud Map's, the second is left to its client, and an id
    // no zone is stored under is marked at all: `false`, and nothing is written.
    assert!(
        managed_zone::link_zone_to_service(
            &*engine,
            &zone_ids[0],
            CLOUD_MAP_PRINCIPAL,
            NAMESPACE_ARN
        ),
        "the zone Cloud Map created must take its linked service"
    );
    assert!(
        !managed_zone::link_zone_to_service(
            &*engine,
            "Z00000000000000",
            CLOUD_MAP_PRINCIPAL,
            NAMESPACE_ARN
        ),
        "an id this service holds no zone under must take no linked service"
    );

    // (case, the zone read, whether it reports a linked service)
    for (case, zone_id, linked) in [
        ("the zone Cloud Map created", &zone_ids[0], true),
        ("the zone its client created", &zone_ids[1], false),
    ] {
        let (status, read) = call(
            &router,
            "GET",
            &format!("/2013-04-01/hostedzone/{zone_id}"),
            "",
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetHostedZone must answer 200, got {status} with {read}"
        );
        if linked {
            for element in [
                format!("<ServicePrincipal>{CLOUD_MAP_PRINCIPAL}</ServicePrincipal>"),
                format!("<Description>{NAMESPACE_ARN}</Description>"),
            ] {
                assert!(
                    read.contains(&element),
                    "case {case:?}: the read must report {element}, got {read}"
                );
            }
        } else {
            assert!(
                !read.contains("<LinkedService>"),
                "case {case:?}: the read must report no linked service, got {read}"
            );
        }

        // The listing projects every zone the same way, so it agrees with the read.
        let (status, listed) = call(&router, "GET", "/2013-04-01/hostedzone", "").await;
        assert_eq!(
            status, 200,
            "case {case:?}: ListHostedZones must answer 200, got {status} with {listed}"
        );
        assert_eq!(
            listed.matches("<LinkedService>").count(),
            1,
            "case {case:?}: exactly the one marked zone reports a linked service, got {listed}"
        );
    }
}

/// Send one request and answer `(status, body)`.
async fn call(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: &str,
) -> (u16, String) {
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header(http::header::HOST, ENDPOINT)
        .header(http::header::CONTENT_TYPE, "application/xml")
        .body(Bytes::from(body.to_owned()))
        .expect("the test request must build");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// The text between `open` and the next `close` in `body`, if both are there.
fn between<'b>(body: &'b str, open: &str, close: &str) -> Option<&'b str> {
    let (_, rest) = body.split_once(open)?;
    let (found, _) = rest.split_once(close)?;
    Some(found)
}
