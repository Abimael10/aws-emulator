// SPDX-License-Identifier: CC0-1.0

//! WHICH object the ORIGIN is asked for when a viewer requests a distribution's ROOT.
//!
//! RULE: `DistributionConfig$DefaultRootObject` is "the object that you want CloudFront to
//! request from your origin … when a viewer requests the root URL for your distribution", and
//! "a default root object avoids exposing the contents of your distribution" — so the origin
//! request a ROOT viewer path produces names THAT object, whatever an origin would otherwise
//! answer a request naming no object with, while every viewer path BELOW the root is asked for
//! as the viewer spelled it ("CloudFront doesn't return default root objects in
//! subdirectories") and a distribution naming no root object asks the origin for the viewer's
//! own path.
//!
//! The substitution is on the ORIGIN REQUEST, so what proves it is the ANSWER the viewer is
//! served: the document the origin hands back for the object the configuration named. Each
//! row's expected answer is therefore read here from the origin itself, at the path the rule
//! says it is asked for, and compared with what the edge served — nothing about either
//! document is written down in this suite.
//!
//! The rows differ along what the rule generalises over:
//!
//! * the FORM of the stored value — a path at the origin, the same path spelled with the
//!   leading separator the guide says to leave off ("do not add a `/` before the object
//!   name"), and no root object at all;
//! * the SPELLING of the root viewer path, which differs per addressing ([the edge module's
//!   `resolve`]): `/` host-style, and the empty viewer path a prefixed path-style or path-style
//!   request carries when it names nothing below the distribution;
//! * the viewer paths BELOW the root that are never substituted: a path naming an object, a
//!   directory-style path, and the root object's own path named explicitly.
//!
//! ## The origin these cases point at
//!
//! An edge fetch is a real in-process HTTP request to the origin's own `DomainName`, so the
//! origin here is an endpoint this emulator serves: this service's own control-plane host,
//! whose `GET /2020-05-31/cache-policy` and `GET /2020-05-31/origin-access-control` each
//! answer a document of their own and whose root answers neither. Nothing about the edge is
//! special-cased for it — it is an origin like any other, reached over the same transport an
//! Amazon S3 bucket endpoint would be (the cross-service suites in `core/router/tests/all`
//! drive the same rule over a private bucket).

use std::collections::BTreeSet;

use bytes::Bytes;
use smithy_server_runtime::MultiServiceRouter;

use super::distribution_lifecycle::{
    awaited_deployment, create_distribution, emulator, API_VERSION, HOST,
};

/// The AWS-managed cache policy `Managed-CachingDisabled`, whose TTLs are all zero: every
/// request below reaches the origin, so each case is served the origin's own answer rather
/// than one the edge held for another case.
const CACHING_DISABLED: &str = "4135ea2d-6df8-44a3-9df3-4b5a84be39ad";

/// The origin id the one origin of every distribution below carries, and the one its default
/// behaviour targets.
const ORIGIN_ID: &str = "primary";

/// The emulator's own endpoint, which names NO service: the host a client that cannot resolve
/// a distribution's domain name reaches the edge at, carrying in the path what it cannot carry
/// in the host.
const EMULATOR_ENDPOINT: &str = "127.0.0.1:9297";

/// The reserved first segment of the prefixed path-style addressing of an edge.
const EDGE_PATH_PREFIX: &str = "/_aws/cloudfront";

/// What `X-Cache` reports for an answer the ORIGIN produced for this request, and for one the
/// edge cache held: no answer below may be the second, since each case has to be served what
/// its OWN origin request answered.
const MISS: &str = "Miss from cloudfront";
const HIT: &str = "Hit from cloudfront";

/// What one request was answered with: the status, the `X-Cache` the edge reported and the
/// bytes.
struct Served {
    status: u16,
    x_cache: String,
    body: Vec<u8>,
}

impl Served {
    /// The bytes as text, for an assertion message.
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    /// WHAT the origin answered, in the form two answers to the same origin request can be
    /// compared in: the status, plus the bytes themselves for an answer that IS a document and
    /// the error code alone for one that is a REFUSAL — a refusal envelope carries the id of
    /// the one request it refused, which no second request to the same path shares, so its
    /// bytes are never equal to another's while the answer is the same answer.
    fn identity(&self) -> (u16, String) {
        (
            self.status,
            match element_text(&self.text(), "Code") {
                Some(code) => code,
                None => self.text(),
            },
        )
    }
}

/// The text of the first `name` element of `document`, or `None` when it carries none.
fn element_text(document: &str, name: &str) -> Option<String> {
    let (_, rest) = document.split_once(&format!("<{name}>"))?;
    let (text, _) = rest.split_once(&format!("</{name}>"))?;
    Some(text.to_string())
}

/// `GET path` at `host`, carrying no credential of any kind.
///
/// One request path for both halves of every case: a VIEWER request when `host` is an address
/// of a distribution's edge, and a DIRECT read of the origin when it is the origin's own host —
/// so the expected answer is what that origin really answers rather than a document written
/// here.
async fn get(router: &MultiServiceRouter, host: &str, path: &str) -> Served {
    let request = ::http::Request::builder()
        .method("GET")
        .uri(path)
        .header("host", host)
        .body(Bytes::new())
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let x_cache = response
        .headers()
        .get("x-cache")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    Served {
        status,
        x_cache,
        body: response.into_body().to_vec(),
    }
}

/// The `DistributionConfig` of one row: the control-plane origin above, a default behaviour
/// that caches nothing, and `root_object` as its `DefaultRootObject` where the row names one.
fn config(reference: &str, root_object: Option<&str>) -> String {
    let root = root_object
        .map(|object| format!("<DefaultRootObject>{object}</DefaultRootObject>"))
        .unwrap_or_default();
    format!(
        "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CallerReference>{reference}</CallerReference>{root}\
         <Origins><Quantity>1</Quantity><Items><Origin>\
         <Id>{ORIGIN_ID}</Id><DomainName>{HOST}</DomainName>\
         <CustomOriginConfig><HTTPPort>80</HTTPPort><HTTPSPort>443</HTTPSPort>\
         <OriginProtocolPolicy>https-only</OriginProtocolPolicy></CustomOriginConfig>\
         </Origin></Items></Origins>\
         <DefaultCacheBehavior><TargetOriginId>{ORIGIN_ID}</TargetOriginId>\
         <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>\
         <CachePolicyId>{CACHING_DISABLED}</CachePolicyId>\
         </DefaultCacheBehavior>\
         <Comment>which object the root of a distribution asks the origin for</Comment>\
         <Enabled>true</Enabled>\
         </DistributionConfig>"
    )
}

/// One root-object configuration.
struct RootObjectCase {
    case: &'static str,
    /// The `DefaultRootObject` the row's distribution carries, exactly as its configuration
    /// spells it, or `None` for a distribution naming none.
    root_object: Option<&'static str>,
    /// The path the ORIGIN must be asked for when a viewer asks for this distribution's root:
    /// the object the row named, or the viewer's own path where it named none.
    origin_path: &'static str,
    /// Whether that path is one the origin answers a document of its own at, which is what
    /// lets the row also state that the answer is NOT the one the origin's root gives.
    serves_a_document: bool,
}

/// The paths each row names are the ones the model binds the origin's own list operations to
/// (`smithy_query query=operation operation=ListCachePolicies,ListOriginAccessControls,ListDistributions`:
/// `GET /2020-05-31/cache-policy`, `GET /2020-05-31/origin-access-control`, `GET
/// /2020-05-31/distribution`), each answering a document of its own; the origin's own root is
/// bound to no operation at all, which is exactly the shape a bucket root has.
const ROOT_OBJECT_CASES: &[RootObjectCase] = &[
    RootObjectCase {
        case: "a root object naming a path at the origin",
        root_object: Some("2020-05-31/cache-policy"),
        origin_path: "/2020-05-31/cache-policy",
        serves_a_document: true,
    },
    RootObjectCase {
        case: "a root object spelled with the leading separator the guide says to leave off",
        root_object: Some("/2020-05-31/origin-access-control"),
        origin_path: "/2020-05-31/origin-access-control",
        serves_a_document: true,
    },
    RootObjectCase {
        case: "a distribution naming no root object",
        root_object: None,
        origin_path: "/",
        serves_a_document: false,
    },
];

/// Create every row's distribution and let the clock deploy it: the row, its id and the domain
/// name CloudFront assigned it.
async fn staged(router: &MultiServiceRouter) -> Vec<(&'static RootObjectCase, String, String)> {
    let mut staged = Vec::new();
    for (index, row) in ROOT_OBJECT_CASES.iter().enumerate() {
        let created = create_distribution(
            router,
            &config(&format!("edge-root-object-{index}"), row.root_object),
        )
        .await;
        assert_eq!(
            created.status, 201,
            "case {:?}: CreateDistribution must succeed, got {} with {}",
            row.case, created.status, created.raw
        );
        let id = created
            .root()
            .find_text("Id")
            .unwrap_or_default()
            .to_string();
        let domain = created
            .root()
            .find_text("DomainName")
            .unwrap_or_default()
            .to_string();
        assert!(
            !id.is_empty() && !domain.is_empty(),
            "case {:?}: the created distribution reports no Id or DomainName: {}",
            row.case,
            created.raw
        );
        assert_eq!(
            created.root().text_at("DistributionConfig/DefaultRootObject"),
            Some(row.root_object.unwrap_or_default()),
            "case {:?}: the created distribution must carry the root object it was given, got {}",
            row.case,
            created.raw
        );
        staged.push((row, id, domain));
    }
    for (row, id, _domain) in &staged {
        awaited_deployment(router, row.case, id).await;
    }
    staged
}

/// Every spelling of a distribution's ROOT, across the three addressings of an edge.
fn root_viewer_requests(id: &str, domain: &str) -> Vec<(String, String, String)> {
    vec![
        (
            "host-style, the domain name CloudFront assigned".to_string(),
            domain.to_string(),
            "/".to_string(),
        ),
        (
            "the reserved data-plane prefix, naming nothing below the distribution".to_string(),
            EMULATOR_ENDPOINT.to_string(),
            format!("{EDGE_PATH_PREFIX}/{id}"),
        ),
        (
            "the reserved data-plane prefix, at the distribution's root".to_string(),
            EMULATOR_ENDPOINT.to_string(),
            format!("{EDGE_PATH_PREFIX}/{id}/"),
        ),
        (
            "path-style, the distribution id as the only segment".to_string(),
            EMULATOR_ENDPOINT.to_string(),
            format!("/{id}"),
        ),
        (
            "path-style, at the distribution's root".to_string(),
            EMULATOR_ENDPOINT.to_string(),
            format!("/{id}/"),
        ),
    ]
}

/// A viewer asking for the ROOT of a distribution is served the object its `DefaultRootObject`
/// names — the bytes the ORIGIN answers at that object's path, and never what the origin
/// answers a request naming no object with.
#[tokio::test]
async fn the_distribution_root_is_served_the_object_its_default_root_object_names() {
    let router = emulator();
    let staged = staged(&router).await;

    // What the origin itself answers at each path a row names, read directly: the expected
    // bytes of every case below, and of the root the rows that name no object fall back to.
    let at_the_origin_root = get(&router, HOST, "/").await;

    for (row, id, domain) in &staged {
        let expected = get(&router, HOST, row.origin_path).await;
        for (addressing, host, path) in root_viewer_requests(id, domain) {
            let case = format!("{} / {addressing}", row.case);
            let served = get(&router, &host, &path).await;
            assert_eq!(
                served.status,
                expected.status,
                "case {case:?}: expected the origin's own status {} for {:?}, got {} with {:?}",
                expected.status,
                row.origin_path,
                served.status,
                served.text()
            );
            assert_ne!(
                served.x_cache, HIT,
                "case {case:?}: expected the answer of this case's OWN origin request, got one \
                 the edge cache held: {:?}",
                served.text()
            );
            assert_eq!(
                served.identity(),
                expected.identity(),
                "case {case:?}: expected what the origin answers at {:?}, got {:?}",
                row.origin_path,
                served.text()
            );
            if row.serves_a_document {
                assert_eq!(
                    served.x_cache, MISS,
                    "case {case:?}: expected X-Cache {MISS:?} for an answer the origin \
                     produced, got {:?}",
                    served.x_cache
                );
                // What a default root object is FOR: the viewer is never served what the
                // origin answers a request naming no object with.
                assert_ne!(
                    served.identity(),
                    at_the_origin_root.identity(),
                    "case {case:?}: the root object was never substituted — the viewer was \
                     served what the origin answers at its own root: {:?}",
                    served.text()
                );
            }
        }
    }
}

/// One viewer path below the root: a path naming an object the origin answers a document at, a
/// directory-style path it answers nothing at, and the path the first row's root object names,
/// asked for by the viewer itself.
const BELOW_THE_ROOT: &[(&str, &str)] = &[
    ("an object below the root", "/2020-05-31/distribution"),
    ("a directory-style path", "/2020-05-31/"),
    ("the root object's own path", "/2020-05-31/cache-policy"),
];

/// No viewer path BELOW a distribution's root is substituted: each one is asked of the origin
/// as the viewer spelled it, whatever the distribution's `DefaultRootObject` names — so the
/// same path is served the same bytes under every row's configuration.
#[tokio::test]
async fn no_viewer_path_below_the_root_is_substituted() {
    let router = emulator();
    let staged = staged(&router).await;

    for (path_case, path) in BELOW_THE_ROOT {
        let expected = get(&router, HOST, path).await;
        let mut answers: BTreeSet<(u16, String)> = BTreeSet::new();
        for (row, _id, domain) in &staged {
            let case = format!("{} / {path_case}", row.case);
            let served = get(&router, domain, path).await;
            assert_eq!(
                served.identity(),
                expected.identity(),
                "case {case:?}: expected what the origin answers at {path:?}, got {} with {:?}",
                served.status,
                served.text()
            );
            answers.insert(served.identity());
            // A path below the root is never answered with the object the root names, unless
            // the viewer asked for that object itself.
            if let Some(root_object) = row.root_object {
                let named = format!("/{}", root_object.trim_start_matches('/'));
                if named != *path {
                    let root_document = get(&router, HOST, &named).await;
                    assert_ne!(
                        served.identity(),
                        root_document.identity(),
                        "case {case:?}: the path below the root was answered with the root \
                         object at {named:?}: {:?}",
                        served.text()
                    );
                }
            }
        }
        assert_eq!(
            answers.len(),
            1,
            "case {path_case:?}: every configuration must ask the origin for the viewer's own \
             path, so {path:?} is answered the same way under all of them, got {} distinct \
             answers: {answers:?}",
            answers.len()
        );
    }
}
