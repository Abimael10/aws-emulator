// SPDX-License-Identifier: CC0-1.0

//! What a cache behaviour's CORS response headers policy does to a viewer request at the EDGE
//! of a private Amazon S3 origin, on the address the distribution PUBLISHES — dialled on a
//! socket, which is what a viewer handed that name does.
//!
//! RULE: a response headers policy's `CorsConfig` only SELECTS the `Access-Control-*` headers
//! the answer carries — it "is used for cross-origin resource sharing (CORS). CloudFront adds
//! these headers to HTTP responses that it sends", and `AccessControlAllowOrigins` is "a list
//! of origins (domain names) that CloudFront can use as the value for the
//! `Access-Control-Allow-Origin` HTTP response header" — so it decides neither the status nor
//! the body: a viewer read is served the origin's object whatever `Origin` it states, and one
//! stating an origin the list does not name is served that same object with none of the
//! `Access-Control-*` headers at all, leaving the page that made it to be denied the answer by
//! its OWN browser. The only viewer request the edge refuses here is one whose METHOD the
//! matched behaviour's `AllowedMethods` does not list, which CloudFront answers with its own
//! page and `X-Cache: Error from cloudfront` — a response headers policy refuses nothing.
//!
//! The rows span what the rule generalises over: the origin a read states (the first the
//! policy names, the second one, stated with a port, four it does not name — an unrelated
//! site, a host the granted one is a prefix of, the granted host over another scheme, the
//! granted host at another port — and no origin at all), the read method (`GET`, `HEAD`), the
//! object asked for (two, so the body is the object the viewer asked for rather than one the
//! edge made up), and, for the refusal half, every method the behaviour's `AllowedMethods`
//! leaves out, stated both from an origin the policy names and by a client stating none.
//!
//! The emulator's OWN guard on that address — a request a BROWSER classified as cross-site is
//! admitted only when the distribution's configuration names its origin, which is this
//! emulator guarding the machine it runs on rather than anything CloudFront does — is the
//! subject of [`crate::edge_response_headers_policy`] and is not restated here: every row
//! below is a client that states no `Sec-Fetch-*`, which is what every non-browser viewer
//! (an SDK, a proxy, a server-side fetch, a test harness) sends.
//!
//! The two services over one engine come from [`crate::edge_signed_s3_origin`], the policy
//! document builder from [`crate::response_headers_policy_lifecycle`], the control-plane wire
//! path from [`crate::distribution_lifecycle`] and the viewer dial, with the reading of the
//! answer it gets back, from [`crate::edge_lambda_url_origin`] — reused rather than copied.

use std::sync::Arc;

use smithy_server_runtime::{EmbeddedEmulator, MultiServiceRouter};

use crate::distribution_lifecycle::{
    awaited_deployment, call, edge_listener_address, is_published_edge_address, API_VERSION,
};
use crate::edge_lambda_url_origin::{Answered, Edge};
use crate::edge_signed_access::CACHING_DISABLED;
use crate::edge_signed_s3_origin::{create_origin_access_control, grant, mounted, origin_domain};
use crate::response_headers_policy_lifecycle::{create_policy, Cors, Policy};

// ---------------------------------------------------------------------------
// What the configuration names, and what it does not
// ---------------------------------------------------------------------------

/// The first origin the policy's `AccessControlAllowOrigins` names.
const GRANTED: &str = "https://portal.example.org";

/// The second one, stated WITH a port: a serialized origin carries one whenever it is not the
/// scheme's default, so the value the list holds and the value a viewer presents are compared
/// as whole origins.
const GRANTED_WITH_PORT: &str = "https://admin.example.org:8443";

/// An unrelated site the policy does not name.
const FOREIGN: &str = "https://elsewhere.example.net";

/// A host the granted one is a PREFIX of: a different origin, so naming the granted one grants
/// this nothing.
const FOREIGN_SUFFIXED: &str = "https://portal.example.org.viewer.example";

/// The granted HOST over another scheme, and the granted host at another PORT: each is its own
/// origin, so neither is the one the list names.
const FOREIGN_SCHEME: &str = "http://portal.example.org";
const FOREIGN_PORT: &str = "https://portal.example.org:9443";

/// The Region the origin bucket is created in, which is the Region the S3 origin's
/// `DomainName` addresses it at ([`origin_domain`]).
const REGION: &str = "us-east-1";

/// The objects the origin bucket holds: the key, and the bytes stored at it. Two, so a row
/// shows the answer carrying the object the viewer ASKED for.
const OBJECTS: [(&str, &str); 2] = [
    ("app/config.json", "{\"feature\":\"cors\",\"retries\":3}"),
    ("styles/site.css", "body{margin:0}"),
];

/// The bytes stored at `key`, read from the one declaration of the staged objects.
fn stored(key: &str) -> &'static str {
    OBJECTS
        .iter()
        .find(|(staged, _body)| *staged == key)
        .map(|(_key, body)| *body)
        .unwrap_or_else(|| panic!("{key:?} is not one of the staged objects"))
}

/// The prefix of every header a CORS configuration decides, which is the whole of what it
/// decides: a row the policy does not grant must carry no header under it.
const CORS_PREFIX: &str = "access-control-";

/// `X-Cache` for an answer CloudFront produced ITSELF rather than one an origin did.
const ERROR: &str = "Error from cloudfront";

/// The sentence CloudFront's generated page carries for a viewer method the matched cache
/// behaviour does not allow, as CloudFront words that refusal.
const METHOD_NOT_ALLOWED_DETAIL: &str =
    "This distribution is not configured to allow the HTTP request method that was used for \
     this request. The distribution supports only cachable requests.";

/// Every method the staged behaviour's `AllowedMethods` leaves out — the rest of the model's
/// `Method` enum beside the `GET`/`HEAD`/`OPTIONS` it lists
/// (`smithy_query(query="shape", shape="Method")`).
const METHODS_NOT_ALLOWED: [&str; 4] = ["POST", "PUT", "PATCH", "DELETE"];

// ---------------------------------------------------------------------------
// Staging the distribution
// ---------------------------------------------------------------------------

/// A policy whose CORS group names `granted` and carries every member a `CorsConfig` can, so
/// an answer the group decorates is told from one it does not by more than a single header.
fn cors_policy(name: &str, granted: &[&str]) -> Policy {
    Policy {
        case: "a cors policy naming its origins",
        name: name.to_string(),
        cors: Some(Cors {
            origins: granted.iter().map(|origin| origin.to_string()).collect(),
            headers: vec!["content-type".to_string()],
            methods: vec!["GET".to_string(), "HEAD".to_string()],
            credentials: true,
            expose: Some(vec!["x-amz-cf-id".to_string()]),
            max_age: Some(600),
            origin_override: true,
        }),
        ..Policy::default()
    }
}

/// One `DistributionConfig`: the private bucket as an S3 origin behind the origin access
/// control `control`, and a default behaviour that allows only the read methods, caches
/// nothing and attaches the response headers policy `policy`.
fn config(reference: &str, bucket: &str, control: &str, policy: &str) -> String {
    format!(
        "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CallerReference>{reference}</CallerReference>\
         <Origins><Quantity>1</Quantity><Items><Origin>\
         <Id>origin-bucket</Id><DomainName>{}</DomainName>\
         <S3OriginConfig><OriginAccessIdentity></OriginAccessIdentity></S3OriginConfig>\
         <OriginAccessControlId>{control}</OriginAccessControlId>\
         </Origin></Items></Origins>\
         <DefaultCacheBehavior><TargetOriginId>origin-bucket</TargetOriginId>\
         <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>\
         <AllowedMethods><Quantity>3</Quantity>\
         <Items><Method>GET</Method><Method>HEAD</Method><Method>OPTIONS</Method></Items>\
         <CachedMethods><Quantity>2</Quantity>\
         <Items><Method>GET</Method><Method>HEAD</Method></Items></CachedMethods>\
         </AllowedMethods>\
         <CachePolicyId>{CACHING_DISABLED}</CachePolicyId>\
         <ResponseHeadersPolicyId>{policy}</ResponseHeadersPolicyId>\
         </DefaultCacheBehavior>\
         <Comment>{reference}</Comment>\
         <Enabled>true</Enabled>\
         </DistributionConfig>",
        origin_domain(bucket)
    )
}

/// What one run staged: the object store the origin lives in, and the address the deployed
/// distribution published.
///
/// The object-store client holds the in-process callback into the plane every setup call went
/// through, which is what keeps that plane — and the listener the create opened on it — alive
/// for the whole run.
struct Staged {
    s3: ::aws_sdk_s3::Client,
    /// Where a viewer reaches the staged distribution's edge: the name it published and the
    /// address that name resolves to for a client on this machine.
    domain: Edge,
}

/// Stage the whole recorded shape: a private bucket holding [`OBJECTS`], an origin access
/// control signing every origin request, a CORS policy naming `granted`, a distribution over
/// that bucket carrying both, the bucket's grant to CloudFront scoped to that distribution,
/// and the wait for the clock to deploy it.
async fn staged(reference: &str, bucket: &str, granted: &[&str]) -> Staged {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    // One plane with authentication off: each setup call carries a credential scope — which is
    // what routes it and names the Region — without a computed signature, and the listener the
    // create opens hands the viewer requests it accepts back to this same emulator.
    let setup: MultiServiceRouter = mounted(&engine, false);
    let s3 = ::service_common::s3_client::s3_client(
        &EmbeddedEmulator::from_router(setup.clone()),
        REGION,
    );
    s3.create_bucket()
        .bucket(bucket)
        .send()
        .await
        .unwrap_or_else(|error| panic!("creating {bucket} must succeed, got {error:?}"));
    for (key, body) in OBJECTS {
        s3.put_object()
            .bucket(bucket)
            .key(key)
            .body(::aws_sdk_s3::primitives::ByteStream::from(
                body.as_bytes().to_vec(),
            ))
            .send()
            .await
            .unwrap_or_else(|error| panic!("putting {bucket}/{key} must succeed, got {error:?}"));
    }

    let created = create_policy(&setup, &cors_policy(reference, granted).xml()).await;
    assert_eq!(
        created.status, 201,
        "creating the response headers policy answered {}: {}",
        created.status, created.raw
    );
    let policy = created
        .root()
        .find_text("Id")
        .expect("the created response headers policy reports an id")
        .to_string();
    let control = create_origin_access_control(&setup, reference, reference).await;
    let created = call(
        &setup,
        "POST",
        &format!("/{API_VERSION}/distribution"),
        Some(&config(reference, bucket, &control, &policy)),
        None,
    )
    .await;
    assert_eq!(
        created.status, 201,
        "creating the distribution answered {}: {}",
        created.status, created.raw
    );
    let read = |member: &str| {
        created
            .root()
            .find_text(member)
            .unwrap_or_else(|| {
                panic!(
                    "the created distribution reports no {member}: {}",
                    created.raw
                )
            })
            .to_string()
    };
    let (id, domain, arn) = (read("Id"), read("DomainName"), read("ARN"));
    // The grant is written AFTER the distribution exists, because it is scoped to that
    // distribution's own ARN — the order a client has to follow.
    s3.put_bucket_policy()
        .bucket(bucket)
        .policy(grant(bucket, &arn))
        .send()
        .await
        .unwrap_or_else(|error| panic!("the policy of {bucket} must be stored, got {error:?}"));
    assert!(
        is_published_edge_address(&domain),
        "expected an address this emulator answers on (host:port), \
         got {domain:?}"
    );
    // A configuration that has not reached the edge locations serves no viewer, and the CLOCK
    // owes that propagation: the condition is polled for, never a span waited out.
    awaited_deployment(&setup, reference, &id).await;
    let address = edge_listener_address(&engine, &id);
    Staged {
        s3,
        domain: Edge {
            domain,
            address,
        },
    }
}

// ---------------------------------------------------------------------------
// Sending one viewer request
// ---------------------------------------------------------------------------

/// Dial the address the distribution published and send one viewer request of `method` for
/// `key`, carrying `headers` — a client holding nothing but that name, with no AWS credential
/// and no `Sec-Fetch-*` of any kind.
///
/// The dial and the reading of the answer are [`crate::edge_lambda_url_origin`]'s
/// [`Answered`], which every suite that reads an answer off a published address uses; this
/// adds only the path a key is asked for.
async fn viewed(
    case: &str,
    domain: &Edge,
    method: &str,
    key: &str,
    headers: &[(&str, &str)],
) -> Answered {
    Answered::of_stating(case, domain, method, &format!("/{key}"), headers).await
}

/// The names of every header a CORS configuration decides that `answer` carries, which is
/// what a policy that does not grant this viewer's origin must leave off entirely.
fn cors_headers(answer: &Answered) -> Vec<String> {
    answer
        .stated_headers()
        .into_iter()
        .map(|(name, _value)| name)
        .filter(|name| name.starts_with(CORS_PREFIX))
        .collect()
}

/// One viewer READ: the method it uses, the `Origin` it states (if any) and the object it asks
/// for — and the value the answer's `Access-Control-Allow-Origin` must carry, `None` where the
/// answer must carry no `Access-Control-*` header at all.
struct Read {
    case: &'static str,
    method: &'static str,
    origin: Option<&'static str>,
    key: &'static str,
    allow_origin: Option<&'static str>,
}

// ---------------------------------------------------------------------------
// The rule: the group decorates the answer and decides nothing else
// ---------------------------------------------------------------------------

/// Every read is served the origin's object, whatever `Origin` it states; the CORS group is
/// the only difference between the answers.
#[tokio::test]
async fn a_cors_policy_only_decorates_the_answer_it_never_refuses_the_read() {
    let staged = staged(
        "cors-not-enforced",
        "cors-not-enforced-origin",
        &[GRANTED, GRANTED_WITH_PORT],
    )
    .await;

    let rows: [Read; 9] = [
        Read {
            case: "a GET from the first origin the policy names",
            method: "GET",
            origin: Some(GRANTED),
            key: OBJECTS[0].0,
            allow_origin: Some(GRANTED),
        },
        Read {
            case: "a GET from the second origin the policy names, stated with a port",
            method: "GET",
            origin: Some(GRANTED_WITH_PORT),
            key: OBJECTS[1].0,
            allow_origin: Some(GRANTED_WITH_PORT),
        },
        Read {
            case: "a GET from an unrelated site the policy does not name",
            method: "GET",
            origin: Some(FOREIGN),
            key: OBJECTS[0].0,
            allow_origin: None,
        },
        Read {
            case: "a GET from a host the granted origin is a prefix of",
            method: "GET",
            origin: Some(FOREIGN_SUFFIXED),
            key: OBJECTS[0].0,
            allow_origin: None,
        },
        Read {
            case: "a GET from the granted host over another scheme",
            method: "GET",
            origin: Some(FOREIGN_SCHEME),
            key: OBJECTS[1].0,
            allow_origin: None,
        },
        Read {
            case: "a GET from the granted host at another port",
            method: "GET",
            origin: Some(FOREIGN_PORT),
            key: OBJECTS[0].0,
            allow_origin: None,
        },
        Read {
            case: "a GET stating no origin at all, which is no CORS request",
            method: "GET",
            origin: None,
            key: OBJECTS[1].0,
            allow_origin: None,
        },
        Read {
            case: "a HEAD from an origin the policy names",
            method: "HEAD",
            origin: Some(GRANTED),
            key: OBJECTS[0].0,
            allow_origin: Some(GRANTED),
        },
        Read {
            case: "a HEAD from an origin the policy does not name",
            method: "HEAD",
            origin: Some(FOREIGN),
            key: OBJECTS[0].0,
            allow_origin: None,
        },
    ];

    for row in rows {
        let headers: Vec<(&str, &str)> = row
            .origin
            .map(|origin| vec![("origin", origin)])
            .unwrap_or_default();
        let answer = viewed(row.case, &staged.domain, row.method, row.key, &headers).await;
        let case = row.case;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected the object to be served, got {} (headers {:?}, body {:?})",
            answer.status,
            answer.stated_headers(),
            answer.body
        );
        // The entity is the ORIGIN's object, byte for byte — the policy wrote no part of it. A
        // `HEAD` carries no entity to compare, so its row stands on the headers alone.
        if row.method == "GET" {
            assert_eq!(
                answer.body,
                stored(row.key),
                "case {case:?}: expected the stored object's bytes, got {:?}",
                answer.body
            );
        }
        match row.allow_origin {
            Some(granted) => {
                assert_eq!(
                    answer.stated("access-control-allow-origin").as_deref(),
                    Some(granted),
                    "case {case:?}: expected the viewer's own origin as the allowed one, got \
                     {:?} (every header {:?})",
                    answer.stated("access-control-allow-origin"),
                    answer.stated_headers()
                );
                // The rest of the group the configuration states, so a row that is granted is
                // granted the whole group rather than one header of it.
                for (name, value) in [
                    ("access-control-allow-credentials", "true"),
                    ("access-control-expose-headers", "x-amz-cf-id"),
                ] {
                    assert_eq!(
                        answer.stated(name).as_deref(),
                        Some(value),
                        "case {case:?}: expected {name:?} to be {value:?}, got {:?} (every \
                         header {:?})",
                        answer.stated(name),
                        answer.stated_headers()
                    );
                }
            }
            None => assert!(
                cors_headers(&answer).is_empty(),
                "case {case:?}: expected an answer carrying no CORS header at all, got {:?} \
                 (every header {:?})",
                cors_headers(&answer),
                answer.stated_headers()
            ),
        }
    }
}

// ---------------------------------------------------------------------------
// The refusal that IS the edge's: the behaviour's own allowed methods
// ---------------------------------------------------------------------------

/// The one viewer request the edge refuses at that address: a method the matched behaviour's
/// `AllowedMethods` does not list, in CloudFront's own words — and the object it refused to
/// touch is still there afterwards, through the edge and through the object store itself.
#[tokio::test]
async fn only_a_method_the_behaviour_disallows_is_refused_and_it_changes_nothing() {
    let staged = staged(
        "cors-method-refusal",
        "cors-method-refusal-origin",
        &[GRANTED, GRANTED_WITH_PORT],
    )
    .await;
    let key = OBJECTS[0].0;

    // Both halves of what states an origin: a client whose `Origin` the distribution names
    // (which is what the address admits a state-changing request from) and one that states
    // none at all. Neither is what decides the refusal — the METHOD is.
    for origin in [Some(GRANTED), None] {
        for method in METHODS_NOT_ALLOWED {
            let case = format!("{method} stating the origin {origin:?}");
            let headers: Vec<(&str, &str)> = origin
                .map(|origin| vec![("origin", origin)])
                .unwrap_or_default();
            let answer = viewed(&case, &staged.domain, method, key, &headers).await;
            assert_eq!(
                answer.status, 403,
                "case {case:?}: expected CloudFront's own refusal of the method, got {} \
                 (headers {:?}, body {:?})",
                answer.status,
                answer.stated_headers(),
                answer.body
            );
            assert_eq!(
                answer.stated("x-cache").as_deref(),
                Some(ERROR),
                "case {case:?}: expected X-Cache {ERROR:?}, got {:?} (every header {:?})",
                answer.stated("x-cache"),
                answer.stated_headers()
            );
            assert!(
                answer.body.contains(METHOD_NOT_ALLOWED_DETAIL),
                "case {case:?}: expected CloudFront's own page naming the method, got {:?}",
                answer.body
            );
            assert!(
                !answer.body.contains(stored(key)),
                "case {case:?}: a refusal CloudFront produced must carry none of the object's \
                 bytes, got {:?}",
                answer.body
            );
        }
    }

    // Nothing the refusals touched: the object is still served through the edge, still with
    // the group for an origin the policy names, and the object store still holds its bytes.
    let answer = viewed(
        "a GET after the refused methods",
        &staged.domain,
        "GET",
        key,
        &[("origin", GRANTED)],
    )
    .await;
    assert_eq!(
        (answer.status, answer.body.as_str()),
        (200, stored(key)),
        "case a GET after the refused methods: expected the object to still be served, got {} \
         with {:?}",
        answer.status,
        answer.body
    );
    assert_eq!(
        answer.stated("access-control-allow-origin").as_deref(),
        Some(GRANTED),
        "case a GET after the refused methods: expected the group to still be written, got \
         {:?} (every header {:?})",
        answer.stated("access-control-allow-origin"),
        answer.stated_headers()
    );
    for (key, body) in OBJECTS {
        let read = staged
            .s3
            .get_object()
            .bucket("cors-method-refusal-origin")
            .key(key)
            .send()
            .await
            .unwrap_or_else(|error| {
                panic!("case {key:?}: reading the object back must succeed, got {error:?}")
            });
        let bytes = read
            .body
            .collect()
            .await
            .unwrap_or_else(|error| {
                panic!("case {key:?}: the object's bytes must be read, got {error:?}")
            })
            .to_vec();
        assert_eq!(
            String::from_utf8_lossy(&bytes),
            body,
            "case {key:?}: expected the stored object to be untouched, got {:?}",
            String::from_utf8_lossy(&bytes)
        );
    }
}
