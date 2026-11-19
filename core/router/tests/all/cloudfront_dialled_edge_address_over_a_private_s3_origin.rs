// SPDX-License-Identifier: CC0-1.0

//! A CloudFront distribution over a PRIVATE Amazon S3 origin is reached by DIALLING the
//! `DomainName` the create published — on a real socket, with no credential of any kind —
//! across the two services on one engine.
//!
//! RULE: an address a service publishes for a client to dial is an address this emulator
//! itself answers on, and what answers there is the data plane of the resource that published
//! it. `Distribution$DomainName` is "the domain name that corresponds to the distribution …
//! such as `d111111abcdef8.cloudfront.net`", and the only way a viewer uses a distribution is
//! to issue HTTP(S) to that name — so the value a create reports has to be a `host:port` a
//! client on the machine this emulator runs on can open a connection to, and the viewer
//! request that arrives there is answered by that distribution's edge: the origin fetch is
//! made on the distribution's behalf under the origin access control its origin names, the
//! root URL asks the origin for the object `DefaultRootObject` names and no path below it
//! does, and whatever the origin answers — the object's bytes or its own refusal — is what
//! the viewer is served.
//!
//! Only a cross-service suite can state this: the published address and the edge are
//! CloudFront's, the object and the document that locks it are Amazon S3's, and the subject
//! is that a client holding nothing but the published name is served the one and the direct
//! reader of the other is not. The sibling in-process suites (`cloudfront_oac_s3_origin_edge`,
//! `cloudfront_s3_origin_edge_authorization`, `cloudfront_oac_locked_bucket_blocks_direct_reads`)
//! address the edge by setting a `Host` header on a dispatched request, which says nothing
//! about whether the published name is an address at all; the CloudFront crate's own
//! `published_edge_address` suite dials one, over an origin that needs no credential.
//!
//! The rows differ along what the rule generalises over: the key's form (a bare name, a
//! nested prefix), the viewer path against the distribution's root object (the root itself, a
//! path below it, a directory-style path), whether the bucket holds the key at all, and what
//! the bucket's document grants (this distribution's ARN, another distribution's ARN, no
//! policy at all) — so the one dialled address is shown serving the object, serving the root
//! object, and surfacing the origin's own refusal.
//!
//! Every expected value is computed here from what the row declares. The distribution's id,
//! ARN and published address are CloudFront's to mint: what is asserted about the address is
//! its FORM (a `host:port` a client can dial), that `GetDistribution` reports the same one,
//! and what it answers.

use std::collections::BTreeMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use smithy_server_runtime::MultiServiceRouter;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::cloudfront_oac_s3_origin_edge::{
    call, create_bucket, create_origin_access_control, emulator, origin_domain, policy,
    put_bucket_policy, put_object, regional_endpoint, view, API_VERSION, CLOUDFRONT_HOST,
    CLOUDFRONT_PRINCIPAL,
};

/// The account every emulated ARN carries, which is the account the ARN below names.
const ACCOUNT_ID: &str = "000000000000";

/// A distribution that is not the one each row creates: the ARN a row scoping its grant
/// elsewhere binds it to.
fn another_distribution_arn() -> String {
    format!("arn:aws:cloudfront::{ACCOUNT_ID}:distribution/E2NOTTHISONE")
}

/// The AWS-managed cache policy `Managed-CachingDisabled`, whose TTLs are all zero: every
/// viewer request under it reaches the origin, so each row is served the origin's own answer
/// rather than one held at the edge.
const CACHING_DISABLED: &str = "4135ea2d-6df8-44a3-9df3-4b5a84be39ad";

/// The origin id each row's one origin carries, and the one its behaviour targets.
const ORIGIN_ID: &str = "origin-bucket";

/// The code and the message Amazon S3 refuses a read nothing granted with (Amazon S3 "Error
/// responses"). Both are entirely fixed text: such a read names no principal, so there is
/// nothing of the request in either.
const ACCESS_DENIED: &str = "AccessDenied";
const ACCESS_DENIED_MESSAGE: &str = "Access Denied";

/// What the edge reports as the outcome of an answer the ORIGIN produced for this request.
const MISS: &str = "Miss from cloudfront";

/// The `Status` CloudFront reports a distribution in once its configuration has reached the
/// edge locations, which is the state a viewer request is served in.
const DEPLOYED: &str = "Deployed";

/// How long the suite waits for the clock to deploy a distribution, and how often it looks
/// while waiting. The wait is on the CONDITION CloudFront reports, with a deadline far past
/// the propagation any configuration is owed, so this bound only decides when a distribution
/// that never settles is reported as a failure.
const POLL_EVERY: Duration = Duration::from_millis(50);
const SETTLES_WITHIN: Duration = Duration::from_secs(90);

/// How long the viewer's own socket is given to connect, and how long for the whole answer to
/// arrive. Generous for the same reason: what a row asserts is WHAT the published address
/// answers, never how quickly the host it runs on answers it.
const DIAL_WITHIN: Duration = Duration::from_secs(30);
const ANSWER_WITHIN: Duration = Duration::from_secs(60);

// ---------------------------------------------------------------------------
// The distribution each row creates
// ---------------------------------------------------------------------------

/// `cloudfront:CreateDistribution` over the S3 origin `bucket` with `control` attached, the
/// caching-disabled policy on its default behaviour and `root_object` as its
/// `DefaultRootObject` when the row names one — answering the distribution's
/// `(id, DomainName, ARN)`.
async fn create_distribution(
    router: &MultiServiceRouter,
    case: &str,
    caller_reference: &str,
    bucket: &str,
    control: &str,
    root_object: Option<&str>,
) -> (String, String, String) {
    let root = root_object
        .map(|object| format!("<DefaultRootObject>{object}</DefaultRootObject>"))
        .unwrap_or_default();
    let body = format!(
        "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CallerReference>{caller_reference}</CallerReference>{root}\
         <Origins><Quantity>1</Quantity><Items><Origin>\
         <Id>{ORIGIN_ID}</Id><DomainName>{}</DomainName>\
         <S3OriginConfig><OriginAccessIdentity></OriginAccessIdentity></S3OriginConfig>\
         <OriginAccessControlId>{control}</OriginAccessControlId>\
         </Origin></Items></Origins>\
         <DefaultCacheBehavior><TargetOriginId>{ORIGIN_ID}</TargetOriginId>\
         <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>\
         <CachePolicyId>{CACHING_DISABLED}</CachePolicyId>\
         </DefaultCacheBehavior>\
         <Comment>a private s3 origin reached at the published address</Comment>\
         <Enabled>true</Enabled>\
         </DistributionConfig>",
        origin_domain(bucket)
    );
    let created = call(
        router,
        "cloudfront",
        CLOUDFRONT_HOST,
        "POST",
        &format!("/{API_VERSION}/distribution"),
        &[("content-type", "application/xml")],
        Bytes::from(body),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "case {case:?}: CreateDistribution must succeed, got {} with {:?}",
        created.status,
        created.text()
    );
    let (id, domain, arn) = (
        created.element("Id"),
        created.element("DomainName"),
        created.element("ARN"),
    );
    for (member, value) in [("Id", &id), ("DomainName", &domain), ("ARN", &arn)] {
        assert!(
            !value.is_empty(),
            "case {case:?}: the created distribution reports no {member}: {:?}",
            created.text()
        );
    }
    (id, domain, arn)
}

/// Look again at `GetDistribution` until CloudFront first reports `id` [`DEPLOYED`] — a
/// configuration that has not reached the edge locations serves no viewer — answering the
/// `DomainName` that read reports, or failing the case once [`SETTLES_WITHIN`] has passed.
async fn awaited_deployment(router: &MultiServiceRouter, case: &str, id: &str) -> String {
    let giving_up_at = Instant::now() + SETTLES_WITHIN;
    loop {
        let read = call(
            router,
            "cloudfront",
            CLOUDFRONT_HOST,
            "GET",
            &format!("/{API_VERSION}/distribution/{id}"),
            &[],
            Bytes::new(),
        )
        .await;
        if read.element("Status") == DEPLOYED {
            return read.element("DomainName");
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: CloudFront never reported the distribution {id} {DEPLOYED}, last \
             read {:?}",
            read.text()
        );
        ::tokio::time::sleep(POLL_EVERY).await;
    }
}

// ---------------------------------------------------------------------------
// The viewer's own connection
// ---------------------------------------------------------------------------

/// What a viewer's own connection answered: the status line's code, the headers under their
/// lowercased names, and the body the answer framed.
struct Served {
    status: u16,
    headers: BTreeMap<String, String>,
    body: String,
    raw: String,
}

impl Served {
    /// A response header's value, by its lowercased name.
    fn header(&self, name: &str) -> String {
        self.headers.get(name).cloned().unwrap_or_default()
    }

    /// The text of `<name>…</name>` in the body, or the empty string — the members of the
    /// origin's own `<Error>` document.
    fn element(&self, name: &str) -> String {
        self.body
            .split_once(&format!("<{name}>"))
            .and_then(|(_before, rest)| rest.split_once(&format!("</{name}>")))
            .map_or(String::new(), |(text, _after)| text.to_string())
    }
}

/// Whether `domain` is an address a client on the machine this emulator runs on can DIAL: a
/// `host:port`, which is exactly the set of values a client reaches without resolving a name.
/// A name under a zone nothing here serves resolves to nothing, so a distribution reporting
/// one would publish an address no viewer could open a connection to.
fn is_dialable(domain: &str) -> bool {
    domain.parse::<SocketAddr>().is_ok()
}

/// Dial `authority` in the clear and send one HTTP/1.1 `GET`, answering what came back.
///
/// This is the VIEWER: a client holding nothing but the address the create published, with no
/// AWS credential of any kind, speaking the protocol that address answers rather than going
/// through the router — which is the one thing an in-process dispatch addressed by `Host`
/// cannot show.
///
/// The dial names the LOOPBACK interface explicitly and takes only the PORT from the
/// published authority — the caller has already held that authority to being one of this
/// emulator's own listeners — so nothing here opens a connection anywhere else.
async fn dial(case: &str, authority: &str, path: &str) -> Served {
    let port = authority
        .parse::<SocketAddr>()
        .unwrap_or_else(|error| {
            panic!("case {case:?}: the published address {authority:?} is no address: {error}")
        })
        .port();
    let mut stream = ::tokio::time::timeout(
        DIAL_WITHIN,
        ::tokio::net::TcpStream::connect((Ipv4Addr::LOCALHOST, port)),
    )
    .await
    .unwrap_or_else(|_| {
        panic!("case {case:?}: the published address {authority:?} did not accept a connection")
    })
    .unwrap_or_else(|error| {
        panic!("case {case:?}: the published address {authority:?} refused a connection: {error}")
    });
    let request = format!("GET {path} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .await
        .unwrap_or_else(|error| {
            panic!("case {case:?}: the request to {authority:?} was not sent: {error}")
        });
    let mut answer = Vec::new();
    let read = ::tokio::time::timeout(ANSWER_WITHIN, stream.read_to_end(&mut answer))
        .await
        .unwrap_or_else(|_| {
            panic!("case {case:?}: the published address {authority:?} did not answer in time")
        });
    read.unwrap_or_else(|error| {
        panic!("case {case:?}: the answer from {authority:?} was not read: {error}")
    });
    parsed(case, &String::from_utf8_lossy(&answer))
}

/// One HTTP/1.1 answer, taken apart: the status line's code, the headers under their
/// lowercased names, and the body.
///
/// The body is taken by the `Content-Length` the answer states, so what is compared is the
/// entity the edge framed rather than everything that arrived on the connection; an answer
/// stating no length is read to the end of the stream, which is what the dial's
/// `Connection: close` asks for.
fn parsed(case: &str, raw: &str) -> Served {
    let (head, body) = raw
        .split_once("\r\n\r\n")
        .unwrap_or_else(|| panic!("case {case:?}: the answer carries no end of headers: {raw:?}"));
    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or_default();
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .unwrap_or_else(|| {
            panic!("case {case:?}: the answer carries no status code: {status_line:?}")
        });
    let headers: BTreeMap<String, String> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_string()))
        .collect();
    let framed = headers
        .get("content-length")
        .and_then(|length| length.parse::<usize>().ok())
        .and_then(|length| body.get(..length))
        .unwrap_or(body);
    Served {
        status,
        headers,
        body: framed.to_string(),
        raw: raw.to_string(),
    }
}

// ---------------------------------------------------------------------------
// The table
// ---------------------------------------------------------------------------

/// What the bucket's document grants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Grant {
    /// `s3:GetObject` to the CloudFront service principal, bound to THIS distribution's ARN.
    ThisDistribution,
    /// The same grant, bound to another distribution's ARN.
    AnotherDistribution,
    /// No bucket policy at all.
    NoPolicy,
}

/// What a row expects the viewer dialling the published address to be served.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expected {
    /// The origin object with these exact bytes, under HTTP 200.
    Object(&'static str),
    /// The origin's own refusal: HTTP 403 with its `AccessDenied` document and none of the
    /// bucket's bytes.
    OriginRefusal,
}

/// One row: a private bucket holding the objects it names, a distribution over it under an
/// origin access control, the document the bucket carries, the path the viewer dials, and what
/// the dialled address then answers.
struct Row {
    case: &'static str,
    /// This row's own bucket, so no row's grant or object can serve another's read.
    bucket: &'static str,
    /// The `(key, bytes)` pairs this row's bucket holds.
    objects: &'static [(&'static str, &'static str)],
    /// The `DefaultRootObject` the distribution names, or `None` for one naming none.
    root_object: Option<&'static str>,
    grant: Grant,
    /// The path the viewer asks the dialled address for.
    request_path: &'static str,
    expected: Expected,
}

const ROWS: &[Row] = &[
    Row {
        case: "a bare key under a grant scoped to this distribution",
        bucket: "dialled-edge-bare-key",
        objects: &[("asset.txt", "the private object the edge serves")],
        root_object: None,
        grant: Grant::ThisDistribution,
        request_path: "/asset.txt",
        expected: Expected::Object("the private object the edge serves"),
    },
    Row {
        case: "a key under a nested prefix",
        bucket: "dialled-edge-nested-key",
        objects: &[("static/app/main.js", "export const served = true;\n")],
        root_object: None,
        grant: Grant::ThisDistribution,
        request_path: "/static/app/main.js",
        expected: Expected::Object("export const served = true;\n"),
    },
    Row {
        case: "the root URL of a distribution naming a default root object",
        bucket: "dialled-edge-root-object",
        objects: &[("index.html", "<p>the root object</p>")],
        root_object: Some("index.html"),
        grant: Grant::ThisDistribution,
        request_path: "/",
        expected: Expected::Object("<p>the root object</p>"),
    },
    Row {
        case: "a path below the root of a distribution naming a default root object",
        bucket: "dialled-edge-below-the-root",
        objects: &[
            ("index.html", "<p>the root object</p>"),
            ("page1.html", "<p>page one</p>"),
        ],
        root_object: Some("index.html"),
        grant: Grant::ThisDistribution,
        request_path: "/page1.html",
        expected: Expected::Object("<p>page one</p>"),
    },
    Row {
        // "CloudFront doesn't return default root objects in subdirectories": the origin is
        // asked for the directory path itself, which the bucket holds no object at, and the
        // principal holds no `s3:ListBucket` — so the answer is the bucket's refusal.
        case: "a directory-style path under a default root object",
        bucket: "dialled-edge-directory-path",
        objects: &[
            ("index.html", "<p>the root object</p>"),
            ("tenant/index.html", "<p>the tenant page</p>"),
        ],
        root_object: Some("index.html"),
        grant: Grant::ThisDistribution,
        request_path: "/tenant/",
        expected: Expected::OriginRefusal,
    },
    Row {
        case: "a key the bucket does not hold, with no listing granted",
        bucket: "dialled-edge-absent-key",
        objects: &[("held.txt", "a different object")],
        root_object: None,
        grant: Grant::ThisDistribution,
        request_path: "/missing.txt",
        expected: Expected::OriginRefusal,
    },
    Row {
        case: "a bucket carrying no policy at all",
        bucket: "dialled-edge-no-policy",
        objects: &[("asset.txt", "nothing granted")],
        root_object: None,
        grant: Grant::NoPolicy,
        request_path: "/asset.txt",
        expected: Expected::OriginRefusal,
    },
    Row {
        case: "a grant scoped to another distribution's ARN",
        bucket: "dialled-edge-other-distribution",
        objects: &[("asset.txt", "not for this distribution")],
        root_object: None,
        grant: Grant::AnotherDistribution,
        request_path: "/asset.txt",
        expected: Expected::OriginRefusal,
    },
];

/// What one row left behind for the dialling phase.
struct Prepared {
    case: &'static str,
    bucket: &'static str,
    objects: &'static [(&'static str, &'static str)],
    request_path: &'static str,
    expected: Expected,
    id: String,
    domain: String,
}

#[tokio::test]
async fn the_published_domain_name_dialled_on_a_socket_serves_a_private_s3_origin() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    // The setup plane: authentication off, so each call carries a credential scope (which is
    // what routes it and what names the Region) without a computed signature. Every create
    // goes through it, so the listener each distribution's edge is served on hands the
    // requests it accepts back to this same emulator. The direct reads below go through the
    // authenticating plane over the SAME engine.
    let setup = emulator(&engine, false);
    let readers = emulator(&engine, true);

    let mut prepared: Vec<Prepared> = Vec::new();
    for (index, row) in ROWS.iter().enumerate() {
        create_bucket(&setup, row.case, row.bucket).await;
        for (key, body) in row.objects {
            put_object(&setup, row.case, row.bucket, key, body, &[]).await;
        }
        let control = create_origin_access_control(
            &setup,
            row.case,
            &format!("dialled-edge-{index}"),
            "always",
        )
        .await;
        let (id, domain, arn) = create_distribution(
            &setup,
            row.case,
            &format!("dialled-edge-reference-{index}"),
            row.bucket,
            &control,
            row.root_object,
        )
        .await;
        // The published name is an address a client can open a connection to, or there is
        // nothing for a viewer to dial at all.
        assert!(
            is_dialable(&domain),
            "case {:?}: expected a host:port a client on this machine can dial as the \
             published DomainName, got {domain:?}",
            row.case
        );
        // The document is written AFTER the distribution exists, because a row that scopes it
        // binds it to that distribution's own ARN — the order a client has to follow.
        let scope = match row.grant {
            Grant::ThisDistribution => Some(arn.clone()),
            Grant::AnotherDistribution => Some(another_distribution_arn()),
            Grant::NoPolicy => None,
        };
        if let Some(source_arn) = scope {
            put_bucket_policy(
                &setup,
                row.case,
                row.bucket,
                &policy(
                    CLOUDFRONT_PRINCIPAL,
                    "s3:GetObject",
                    &format!("arn:aws:s3:::{}/*", row.bucket),
                    Some(&format!(
                        "{{\"StringEquals\":{{\"AWS:SourceArn\":\"{source_arn}\"}}}}"
                    )),
                ),
            )
            .await;
        }
        prepared.push(Prepared {
            case: row.case,
            bucket: row.bucket,
            objects: row.objects,
            request_path: row.request_path,
            expected: row.expected,
            id,
            domain,
        });
    }

    for row in &prepared {
        let case = row.case;
        // The address CloudFront reports on a READ is the same one the create published: the
        // name is the distribution's, not the create response's.
        let read_back = awaited_deployment(&setup, case, &row.id).await;
        assert_eq!(
            read_back, row.domain,
            "case {case:?}: expected GetDistribution to report the published address \
             {:?}, got {read_back:?}",
            row.domain
        );

        let answer = dial(case, &row.domain, row.request_path).await;
        // Whatever the answer is, it is the edge's: CloudFront identifies the request it
        // served, which is what says the dialled address is a distribution's data plane and
        // not some other listener that happened to accept the connection.
        assert!(
            !answer.header("x-amz-cf-id").is_empty(),
            "case {case:?}: expected the edge's own X-Amz-Cf-Id on the answer, got {:?}",
            answer.raw
        );
        match row.expected {
            Expected::Object(bytes) => {
                assert_eq!(
                    answer.status, 200,
                    "case {case:?}: expected the origin's object at the published address, got \
                     {} with {:?}",
                    answer.status, answer.body
                );
                assert_eq!(
                    answer.body, bytes,
                    "case {case:?}: expected the ORIGIN's own bytes, got {:?}",
                    answer.body
                );
                assert_eq!(
                    answer.header("x-cache"),
                    MISS,
                    "case {case:?}: expected the answer to be the origin's, got X-Cache {:?}",
                    answer.header("x-cache")
                );
            }
            Expected::OriginRefusal => {
                assert_eq!(
                    answer.status, 403,
                    "case {case:?}: expected the origin's refusal at the published address, \
                     got {} with {:?}",
                    answer.status, answer.body
                );
                assert_eq!(
                    answer.element("Code"),
                    ACCESS_DENIED,
                    "case {case:?}: expected the origin's own {ACCESS_DENIED} document, got {:?}",
                    answer.body
                );
                for (key, bytes) in row.objects {
                    assert!(
                        !answer.body.contains(bytes),
                        "case {case:?}: a refused request must carry none of {key:?}'s bytes, \
                         got {:?}",
                        answer.body
                    );
                }
            }
        }

        // The other half of what the published address is for: every object in the bucket is
        // refused to a reader that dials Amazon S3 itself with no credential, at both
        // addressings of the REST endpoint — so the distribution's own address is the only
        // way in.
        for (key, bytes) in row.objects {
            for (addressing, host, path) in [
                (
                    "path-style",
                    regional_endpoint(),
                    format!("/{}/{key}", row.bucket),
                ),
                (
                    "virtual-hosted-style",
                    origin_domain(row.bucket),
                    format!("/{key}"),
                ),
            ] {
                let direct = view(&readers, &host, &path).await;
                assert_eq!(
                    direct.status, 403,
                    "case {case:?} ({addressing}, {key:?}): expected HTTP 403 from a \
                     credential-free direct read, got {} with {:?}",
                    direct.status,
                    direct.text()
                );
                assert_eq!(
                    direct.element("Code"),
                    ACCESS_DENIED,
                    "case {case:?} ({addressing}, {key:?}): expected {ACCESS_DENIED}, got {:?}",
                    direct.text()
                );
                assert_eq!(
                    direct.element("Message"),
                    ACCESS_DENIED_MESSAGE,
                    "case {case:?} ({addressing}, {key:?}): expected \
                     {ACCESS_DENIED_MESSAGE:?}, got {:?}",
                    direct.element("Message")
                );
                assert!(
                    !direct.text().contains(bytes),
                    "case {case:?} ({addressing}, {key:?}): a refused read must not carry the \
                     object's bytes, got {:?}",
                    direct.text()
                );
            }
        }
    }
}
