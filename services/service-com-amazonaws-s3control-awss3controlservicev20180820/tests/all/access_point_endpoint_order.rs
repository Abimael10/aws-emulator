// SPDX-License-Identifier: CC0-1.0

//! WHICH addresses an access point read publishes its data plane at, and in
//! WHICH order — driven end to end over HTTP.
//!
//! The rules under test:
//!
//! * a map member of a response body is written entry by entry in the order the
//!   map holds, so the endpoint map is reported in ONE fixed order — the plain
//!   Regional endpoint, then FIPS, then FIPS over dual-stack, then dual-stack —
//!   and every read of one unchanged access point answers the same bytes;
//! * every entry names the `host[:port]` THIS emulator serves its data plane at,
//!   never a host in a zone AWS serves its own endpoints in and never a name of
//!   this emulator's own invention under it, so a client that takes an entry out
//!   of the map and sends its object requests there reaches the access point it
//!   read the map from;
//! * a refused read publishes no endpoint at all.
//!
//! Each rule is driven over a TABLE whose rows differ along what it generalises
//! over: the address this emulator is served at (a loopback socket, an IPv6
//! socket, a name an operator serves it under, and no port at all), the Region
//! the request is signed for, what the access point fronts and whether its
//! create restricted it to a VPC, how many times the same access point is read,
//! and why a read is refused.

use smithy_server_runtime::EndpointUrl;

use crate::access_point_lifecycle::{
    call_in_region, Emulator, Reply, ACCOUNT, OTHER_ACCOUNT, REGION, XMLNS,
};
use crate::object_store::ObjectStore;

/// The keys `GetAccessPoint$Endpoints` reports, in the order they are reported:
/// the plain Regional access point endpoint first, then FIPS, then FIPS over
/// dual-stack, then dual-stack. Source: the recorded `Endpoints` map of
/// `GetAccessPoint` (`smithy_query(query="operation",
/// operation="GetAccessPoint")` names the member).
const REPORTED_KEYS: [&str; 4] = ["ipv4", "fips", "fips_dualstack", "dualstack"];

/// The entries a read must report for an emulator served at `authority`: every
/// variant, in order, at the one address this emulator answers on.
fn expected_entries(authority: &str) -> Vec<(String, String)> {
    REPORTED_KEYS
        .into_iter()
        .map(|key| (key.to_string(), authority.to_string()))
        .collect()
}

/// The `<Endpoints>` entries of a reply as `(key, value)` pairs IN DOCUMENT
/// ORDER: the restXml wrapped-map layout is
/// `<Endpoints><entry><key>..</key><value>..</value></entry>…</Endpoints>`, so
/// this reads each `<entry>` where it sits rather than pairing keys and values
/// up by position across the whole body.
fn endpoint_entries(reply: &Reply) -> Vec<(String, String)> {
    let Some(start) = reply.body.find("<Endpoints>") else {
        return Vec::new();
    };
    let after_open = &reply.body[start + "<Endpoints>".len()..];
    let end = after_open
        .find("</Endpoints>")
        .unwrap_or_else(|| panic!("the emulator answered <Endpoints> unclosed: {}", reply.body));
    let mut entries = Vec::new();
    let mut rest = &after_open[..end];
    while let Some(entry_start) = rest.find("<entry>") {
        let entry = &rest[entry_start + "<entry>".len()..];
        let entry_end = entry
            .find("</entry>")
            .unwrap_or_else(|| panic!("the emulator answered <entry> unclosed: {}", reply.body));
        let (one, after) = entry.split_at(entry_end);
        let read = |element: &str| -> String {
            let open = format!("<{element}>");
            let close = format!("</{element}>");
            let from = one
                .find(&open)
                .unwrap_or_else(|| panic!("an <entry> carrying no <{element}>: {}", reply.body))
                + open.len();
            let to = one[from..].find(&close).unwrap_or_else(|| {
                panic!("an <entry> whose <{element}> is unclosed: {}", reply.body)
            });
            one[from..from + to].to_string()
        };
        entries.push((read("key"), read("value")));
        rest = &after["</entry>".len()..];
    }
    entries
}

/// One access point this suite creates: what it fronts, and whether its create
/// restricted it to a VPC — the members that change the rest of the body the
/// endpoint map travels in.
struct Created {
    case: &'static str,
    name: &'static str,
    bucket: &'static str,
    bucket_account_id: Option<&'static str>,
    vpc_id: Option<&'static str>,
}

impl Created {
    const fn new(case: &'static str, name: &'static str, bucket: &'static str) -> Self {
        Self {
            case,
            name,
            bucket,
            bucket_account_id: None,
            vpc_id: None,
        }
    }

    fn body(&self) -> String {
        let mut xml = format!("<CreateAccessPointRequest xmlns=\"{XMLNS}\">");
        xml.push_str(&format!("<Bucket>{}</Bucket>", self.bucket));
        if let Some(account) = self.bucket_account_id {
            xml.push_str(&format!("<BucketAccountId>{account}</BucketAccountId>"));
        }
        if let Some(vpc) = self.vpc_id {
            xml.push_str(&format!(
                "<VpcConfiguration><VpcId>{vpc}</VpcId></VpcConfiguration>"
            ));
        }
        xml.push_str("</CreateAccessPointRequest>");
        xml
    }
}

/// The access points every row below is driven over: they differ in what they
/// front, in whether the create named the bucket's owning account, and in
/// whether they are VPC-restricted — not merely in their names.
fn created() -> Vec<Created> {
    vec![
        Created::new("an internet-facing access point", "reports-ap", "reports"),
        Created {
            bucket_account_id: Some(OTHER_ACCOUNT),
            ..Created::new("a cross-account bucket", "partner-ap", "partner")
        },
        Created {
            vpc_id: Some("vpc-0a1b2c3d"),
            ..Created::new("a VPC-restricted access point", "private-ap", "finance")
        },
    ]
}

/// Create `case` in `region` on `emulator`, and answer the reply of a read of
/// it.
async fn create_then_read(emulator: &Emulator, region: &str, case: &Created) -> Reply {
    emulator.provision_bucket_in(case.case, case.bucket, region);
    let created = call_in_region(
        &emulator.router,
        region,
        "PUT",
        &format!("/v20180820/accesspoint/{}", case.name),
        ACCOUNT,
        &case.body(),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "case {:?} in {region}: expected the create to succeed, got {} with body {}",
        case.case, created.status, created.body
    );
    read(emulator, region, case.name).await
}

async fn read(emulator: &Emulator, region: &str, name: &str) -> Reply {
    call_in_region(
        &emulator.router,
        region,
        "GET",
        &format!("/v20180820/accesspoint/{name}"),
        ACCOUNT,
        "",
    )
    .await
}

/// One emulator served at `base`, the way the data port publishes the URL it
/// bound.
fn served_at(base: &str) -> Emulator {
    let mut emulator = Emulator::start();
    emulator
        .router
        .set_url_base(EndpointUrl::parse(base).unwrap_or_else(|| {
            panic!("{base:?} is a usable endpoint URL");
        }));
    emulator
}

/// One row of the publishing table: where this emulator is served, the Region
/// the request is signed for, and the `host[:port]` every entry has to name.
type Published = (&'static str, &'static str, &'static str, &'static str);

/// The addresses this emulator can be served at, each with the authority a
/// published entry carries — the base's own, because the data plane is served
/// there and nowhere else. The last row is the base the core reports while the
/// emulator is served on no port at all
/// (`smithy_server_runtime::UNSERVED_URL_BASE`), whose host nothing resolves,
/// which is the truth about an emulator serving no port.
const PUBLISHED: [Published; 4] = [
    ("a loopback socket", "https://127.0.0.1:9297", REGION, "127.0.0.1:9297"),
    (
        "an IPv6 loopback socket",
        "https://[::1]:42899",
        "us-east-1",
        "[::1]:42899",
    ),
    (
        "a name an operator serves it under",
        "https://emulator.test:8443",
        "eu-central-1",
        "emulator.test:8443",
    ),
    (
        "served on no port",
        ::smithy_server_runtime::UNSERVED_URL_BASE,
        "ap-southeast-2",
        "emulator.invalid",
    ),
];

#[tokio::test]
async fn the_endpoint_map_is_published_in_one_order_at_the_address_this_emulator_serves() {
    for (served, base, region, authority) in PUBLISHED {
        let emulator = served_at(base);
        for case in created() {
            let reply = create_then_read(&emulator, region, &case).await;
            assert_eq!(
                reply.status, 200,
                "case {:?} served at {served:?}: expected the read to answer 200, got {} with \
                 body {}",
                case.case, reply.status, reply.body
            );
            let entries = endpoint_entries(&reply);
            assert_eq!(
                entries,
                expected_entries(authority),
                "case {:?} served at {served:?} in {region}: expected every access point \
                 endpoint variant, in its reported order, at {authority:?}, got {entries:?} in \
                 body {}",
                case.case,
                reply.body
            );
            // A host in a zone AWS serves its own endpoints in resolves off this
            // machine, so no published entry may carry one.
            for (key, host) in &entries {
                assert!(
                    !host.contains("amazonaws.com"),
                    "case {:?} served at {served:?}: expected entry {key} to publish no host of \
                     AWS's own, got {host:?}",
                    case.case
                );
            }
        }
    }
}

#[tokio::test]
async fn every_read_of_one_unchanged_access_point_answers_the_same_document() {
    // One row per read count: a client that reads the same access point again,
    // and again, must be handed the same bytes every time — which is what an
    // order-preserving map behind a map member is for.
    for reads in [2usize, 3, 5] {
        let emulator = served_at("https://127.0.0.1:9297");
        for case in created() {
            let first = create_then_read(&emulator, REGION, &case).await;
            for attempt in 2..=reads {
                let again = read(&emulator, REGION, case.name).await;
                assert_eq!(
                    (again.status, again.body.as_str()),
                    (first.status, first.body.as_str()),
                    "case {:?}: expected read {attempt} of {reads} to answer the same document \
                     as the first, got {} with body {}",
                    case.case,
                    again.status,
                    again.body
                );
            }
        }
    }
}

#[tokio::test]
async fn a_refused_read_publishes_no_endpoint_at_all() {
    let emulator = served_at("https://127.0.0.1:9297");
    let present = created().remove(0);
    let served = create_then_read(&emulator, REGION, &present).await;
    assert_eq!(
        served.status, 200,
        "the access point the refusal rows read around is served, got {} with body {}",
        served.status, served.body
    );

    // The rows differ in WHY the read is refused: an account the caller holds no
    // credentials for (authorized ahead of the resource, so it is refused even
    // though this access point exists), and a name no access point in the
    // account carries, of two spellings.
    let cases: [(&str, &str, &str, u16, &str, &str); 3] = [
        (
            "another account's control plane",
            OTHER_ACCOUNT,
            present.name,
            403,
            "AccessDenied",
            "Access Denied",
        ),
        (
            "a name no access point carries",
            ACCOUNT,
            "no-such-ap",
            404,
            "NoSuchAccessPoint",
            "The specified accesspoint does not exist",
        ),
        (
            "another absent name again",
            ACCOUNT,
            "reports-ap-2",
            404,
            "NoSuchAccessPoint",
            "The specified accesspoint does not exist",
        ),
    ];
    for (case, account, name, status, code, message) in cases {
        let reply = call_in_region(
            &emulator.router,
            REGION,
            "GET",
            &format!("/v20180820/accesspoint/{name}"),
            account,
            "",
        )
        .await;
        assert_eq!(
            (reply.status, reply.code().as_str()),
            (status, code),
            "case {case:?}: expected {status} {code}, got {} with body {}",
            reply.status,
            reply.body
        );
        assert_eq!(
            reply.text("Message").as_deref(),
            Some(message),
            "case {case:?}: expected the fixed refusal message, got body {}",
            reply.body
        );
        let entries = endpoint_entries(&reply);
        assert!(
            entries.is_empty(),
            "case {case:?}: expected a refusal to publish no endpoint, got {entries:?} in body {}",
            reply.body
        );
    }
}

/// The object a published endpoint is dialled for.
const REACHED_KEY: &str = "finance/q1.csv";
const REACHED_BODY: &[u8] = b"revenue,units\n42,7\n";

#[tokio::test]
async fn a_published_endpoint_is_an_address_this_emulator_serves_the_access_point_at() {
    // Both services on one engine: the access point is created through S3
    // Control and USED in the Amazon S3 data plane, which is what makes a
    // published address either reachable or a dead route.
    let mut store = ObjectStore::start();
    let authority = "127.0.0.1:9297";
    store.router.set_url_base(
        EndpointUrl::parse(&format!("https://{authority}")).expect("a usable endpoint URL"),
    );

    // One row per Region the access point lives in; inside each, one row per
    // entry of the map — whichever of the four a client picked, the address it
    // sends to has to reach the access point.
    for region in [REGION, "us-east-1"] {
        let name = format!("reach-ap-{region}");
        let bucket = format!("reports-{region}");
        store.create_bucket("the data path", region, &bucket).await;
        store
            .put_object("the data path", region, &bucket, REACHED_KEY, REACHED_BODY)
            .await;
        let created = store
            .s3control(
                region,
                "PUT",
                &format!("/v20180820/accesspoint/{name}"),
                &format!(
                    "<CreateAccessPointRequest xmlns=\"{XMLNS}\"><Bucket>{bucket}</Bucket>\
                     </CreateAccessPointRequest>"
                ),
            )
            .await;
        assert_eq!(
            created.status, 200,
            "in {region}: expected the create to succeed, got {} with body {}",
            created.status, created.body
        );
        let reply = store
            .s3control(region, "GET", &format!("/v20180820/accesspoint/{name}"), "")
            .await;
        let entries = endpoint_entries(&reply);
        assert_eq!(
            entries,
            expected_entries(authority),
            "in {region}: expected every variant at the served address, got {entries:?} in body \
             {}",
            reply.body
        );
        let alias = reply
            .text("Alias")
            .unwrap_or_else(|| panic!("in {region}: the read reports the access point's alias"));
        for (key, host) in entries {
            // What a client sends to that address: the access point addressed by
            // its alias, which "you can use instead of an access point ARN for
            // any access point data plane operation".
            let request = ::http::Request::builder()
                .method("GET")
                .uri(format!("https://{host}/{alias}/{REACHED_KEY}"))
                .header("host", &host)
                .header(
                    "authorization",
                    format!(
                        "AWS4-HMAC-SHA256 \
                         Credential=AKIDEXAMPLE/20260101/{region}/s3/aws4_request, \
                         SignedHeaders=host, Signature=0123456789abcdef"
                    ),
                )
                .body(::bytes::Bytes::new())
                .expect("the request builds");
            let response = store.router.dispatch(request).await;
            let status = response.status().as_u16();
            let body = response.into_body().to_vec();
            assert_eq!(
                (status, body.as_slice()),
                (200, REACHED_BODY),
                "entry {key} in {region}: expected the published address {host:?} to reach the \
                 object through the access point, got {status} with body {:?}",
                String::from_utf8_lossy(&body)
            );
        }
    }
}
