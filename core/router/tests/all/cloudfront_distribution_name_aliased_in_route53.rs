// SPDX-License-Identifier: CC0-1.0

//! The ROUTE 53 ALIAS a client points at a CloudFront distribution, over the name that
//! distribution published.
//!
//! RULE: **`Distribution$DomainName` is "the domain name that CloudFront assigns when you
//! create a distribution" — in this emulator the address it ANSWERS for that distribution at,
//! a `host:port` a client on this machine can dial — so an alias record set whose
//! `AliasTarget` is `{HostedZoneId: Z2FDTNDATAQYW2, DNSName: <that value>}` names an endpoint
//! of the zone that id names and `ChangeResourceRecordSets` ACCEPTS it, while a target that
//! does NOT lie in that zone is refused `InvalidChangeBatch`; and the emulator ANSWERS on the
//! value it published, so a viewer that states it is served by that distribution's edge.**
//!
//! `AliasTarget$HostedZoneId` fixes the id — "CloudFront distribution: Specify
//! `Z2FDTNDATAQYW2`" — and `AliasTarget$DNSName` the value that goes with it: for a
//! "CloudFront distribution" you "Specify the domain name that CloudFront assigned when you
//! created your distribution". The alias is the documented way to point a customer domain at
//! a distribution, so a published value Route 53 cannot accept as a target makes that whole
//! workflow unreachable — which is the failure this suite pins. The ACCEPTANCE and the
//! control row below are one rule, not an exception: a resource's endpoint in this emulator is
//! published as the address a client dials, so a target that names an ADDRESS names an endpoint
//! of the zone the id fixes and lies within it, while an ordinary NAME is still held to that
//! zone's own domain — which is why `cdn.example.net` under the same id is refused.
//!
//! The rows span what the rule generalises over: the record TYPE an alias to a distribution
//! may carry (`A` and `AAAA`), the OPERATION that minted the distribution
//! (`CreateDistribution` and `CreateDistributionWithTags`, which publish one name apiece
//! under one rule), and the SPELLING of the published name in the target (as published, in
//! upper case, and fully qualified with the root dot — "The trailing dot is optional; Amazon
//! Route 53 assumes that the domain name is fully qualified", and DNS names are
//! case-insensitive). One CONTROL row submits a name that lies in NO zone CloudFront
//! publishes, under the same hosted zone id, and is refused — which is what says the
//! acceptance above is the published name's own form rather than a check that stopped
//! running.
//!
//! Every value asserted comes from the answers themselves: the name from the create, the
//! change id from the change, the target from the listing, and the edge's own report from the
//! viewer's answer.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use smithy_server_runtime::MultiServiceRouter;

use super::query_wire::{element, elements, opt_element};

/// The Region every call of this suite is signed for.
const REGION: &str = "us-east-1";

/// CloudFront's control-plane host and API version, and Route 53's own.
const CLOUDFRONT_HOST: &str = "cloudfront.amazonaws.com";
const CLOUDFRONT_API: &str = "2020-05-31";
const ROUTE53_HOST: &str = "route53.amazonaws.com";
const ROUTE53_API: &str = "2013-04-01";
const ROUTE53_XMLNS: &str = "https://route53.amazonaws.com/doc/2013-04-01/";

/// The hosted zone id `AliasTarget$HostedZoneId` fixes for every CloudFront distribution:
/// "CloudFront distribution: Specify `Z2FDTNDATAQYW2`".
const CLOUDFRONT_ALIAS_ZONE: &str = "Z2FDTNDATAQYW2";

/// The zone the alias record sets below live in: a second-level domain RFC 2606 §3 reserves
/// for documentation, so the suite claims nobody's name.
const ZONE: &str = "sim2real-alias.example.com.";

/// The `ChangeInfo$Status` a change batch reaches once every name server holds it.
const INSYNC: &str = "INSYNC";

/// The status a just-submitted change batch may still be in.
const PENDING: &str = "PENDING";

/// The `Status` CloudFront reports a distribution in once its configuration has reached the
/// edge locations, which is the state a viewer request is served in.
const DEPLOYED: &str = "Deployed";

/// How long a clock-owed transition (the distribution's propagation, the change batch's) is
/// waited for, and how often it is looked at again. GENEROUS: what is waited for is the
/// CONDITION each reports, so this bound only decides when something that never settles is
/// reported as a failure.
const SETTLES_WITHIN: Duration = Duration::from_secs(90);
const LOOK_AGAIN: Duration = Duration::from_millis(25);

/// The AWS-managed cache policy `Managed-CachingDisabled`, whose TTLs are all zero: the one
/// viewer request below reaches the plane that answers it rather than a cache.
const CACHING_DISABLED: &str = "4135ea2d-6df8-44a3-9df3-4b5a84be39ad";

/// The origin the distributions below name: a host RFC 2606 §3 reserves for documentation,
/// which no service of this emulator answers at. The viewer assertion is about WHICH plane
/// answered the request, never about what an origin served, so an origin that answers nothing
/// is the cheapest one that makes the edge the only thing that could have answered.
const ORIGIN_HOST: &str = "origin.example.com";

/// The emulator with the two services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them. Request authentication is off: a setup call carries
/// a credential scope (which is what routes it and names the Region) without a computed
/// signature, and the VIEWER carries no credential at all, as a client of a distribution
/// sends none.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudfront_cloudfront2020_05_31::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_route53_awsdnsv20130401::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
/// request. The signature itself is never checked here.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/\
         aws4_request, SignedHeaders=host, Signature=0"
    )
}

/// One call over a restXml wire, addressed to `host` and signed for `service`.
async fn call(
    router: &MultiServiceRouter,
    service: &str,
    host: &str,
    method: &str,
    uri: &str,
    body: &str,
) -> (u16, String) {
    let request = ::http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", host)
        .header("content-type", "application/xml")
        .header("authorization", authorization(service))
        .body(Bytes::from(body.to_owned()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One CloudFront control-plane call.
async fn cloudfront(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: &str,
) -> (u16, String) {
    call(router, "cloudfront", CLOUDFRONT_HOST, method, uri, body).await
}

/// One Route 53 call.
async fn route53(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: &str,
) -> (u16, String) {
    call(router, "route53", ROUTE53_HOST, method, uri, body).await
}

/// The `DistributionConfig` every row submits: one origin — a documentation host nothing here
/// answers at — and one default cache behaviour caching nothing.
fn config_xml(reference: &str) -> String {
    format!(
        "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{CLOUDFRONT_API}/\">\
         <CallerReference>{reference}</CallerReference>\
         <Origins><Quantity>1</Quantity><Items><Origin>\
         <Id>only</Id><DomainName>{ORIGIN_HOST}</DomainName>\
         <CustomOriginConfig><HTTPPort>80</HTTPPort><HTTPSPort>443</HTTPSPort>\
         <OriginProtocolPolicy>https-only</OriginProtocolPolicy></CustomOriginConfig>\
         </Origin></Items></Origins>\
         <DefaultCacheBehavior><TargetOriginId>only</TargetOriginId>\
         <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>\
         <CachePolicyId>{CACHING_DISABLED}</CachePolicyId>\
         </DefaultCacheBehavior>\
         <Comment>a distribution aliased from route 53</Comment>\
         <Enabled>true</Enabled>\
         </DistributionConfig>"
    )
}

/// WHICH operation minted a distribution, which is what the rows cross the alias over: each
/// publishes one name apiece and the rule is about that name, not about the call that made
/// it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mint {
    /// `POST /2020-05-31/distribution`.
    Create,
    /// `POST /2020-05-31/distribution?WithTags`, whose payload wraps the same configuration.
    CreateWithTags,
}

impl Mint {
    /// The operation's own name, which every assertion names its case by.
    fn case(self) -> &'static str {
        match self {
            Self::Create => "CreateDistribution",
            Self::CreateWithTags => "CreateDistributionWithTags",
        }
    }
}

/// Mint one distribution through `how` and answer `(Id, DomainName)`, with the published name
/// held to the one CloudFront assigns.
async fn minted_distribution(
    router: &MultiServiceRouter,
    how: Mint,
    reference: &str,
) -> (String, String) {
    let case = how.case();
    let (uri, body) = match how {
        Mint::Create => (
            format!("/{CLOUDFRONT_API}/distribution"),
            config_xml(reference),
        ),
        Mint::CreateWithTags => (
            format!("/{CLOUDFRONT_API}/distribution?WithTags"),
            format!(
                "<DistributionConfigWithTags \
                 xmlns=\"http://cloudfront.amazonaws.com/doc/{CLOUDFRONT_API}/\">{}\
                 <Tags><Items><Tag><Key>owner</Key><Value>alias-suite</Value></Tag></Items>\
                 </Tags></DistributionConfigWithTags>",
                config_xml(reference)
            ),
        ),
    };
    let (status, created) = cloudfront(router, "POST", &uri, &body).await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected 201 from {case}, got {status} with {created}"
    );
    let id = element(&created, "Id");
    let domain = element(&created, "DomainName");
    assert!(
        !id.is_empty(),
        "case {case:?}: the created distribution reports no Id: {created}"
    );
    assert!(
        !domain.contains('/') && domain.parse::<::std::net::SocketAddr>().is_ok(),
        "case {case:?}: expected an address this emulator answers on (host:port) as the \
         published DomainName, got {domain:?}"
    );
    (id, domain)
}

/// Look again at `GetDistribution` until CloudFront first reports `id` [`DEPLOYED`] — a
/// configuration that has not reached the edge locations serves no viewer — and hold the name
/// that read reports to being the one the create published.
async fn awaited_deployment(router: &MultiServiceRouter, case: &str, id: &str, domain: &str) {
    let giving_up_at = ::std::time::Instant::now() + SETTLES_WITHIN;
    loop {
        let (status, read) = cloudfront(
            router,
            "GET",
            &format!("/{CLOUDFRONT_API}/distribution/{id}"),
            "",
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from GetDistribution, got {status} with {read}"
        );
        if element(&read, "Status") == DEPLOYED {
            assert_eq!(
                opt_element(&read, "DomainName").as_deref(),
                Some(domain),
                "case {case:?}: expected the record to report the published name {domain:?}, \
                 got {read}"
            );
            return;
        }
        assert!(
            ::std::time::Instant::now() < giving_up_at,
            "case {case:?}: the distribution {id} never reported {DEPLOYED}, last read {read}"
        );
        ::tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Submit one `CREATE` of an alias record set at `name`, of `rr_type`, targeting `target`
/// inside the hosted zone `target_zone`.
async fn aliased(
    router: &MultiServiceRouter,
    zone_id: &str,
    name: &str,
    rr_type: &str,
    target_zone: &str,
    target: &str,
) -> (u16, String) {
    route53(
        router,
        "POST",
        &format!("/{ROUTE53_API}/hostedzone/{zone_id}/rrset"),
        &format!(
            "<ChangeResourceRecordSetsRequest xmlns=\"{ROUTE53_XMLNS}\"><ChangeBatch>\
             <Changes><Change><Action>CREATE</Action><ResourceRecordSet>\
             <Name>{name}</Name><Type>{rr_type}</Type><AliasTarget>\
             <HostedZoneId>{target_zone}</HostedZoneId><DNSName>{target}</DNSName>\
             <EvaluateTargetHealth>false</EvaluateTargetHealth></AliasTarget>\
             </ResourceRecordSet></Change></Changes></ChangeBatch>\
             </ChangeResourceRecordSetsRequest>"
        ),
    )
    .await
}

/// Look again at `GetChange` until the change `id` reports [`INSYNC`], which is the state
/// every name server of the zone holds the batch in.
async fn awaited_change(router: &MultiServiceRouter, case: &str, submitted: &str) {
    let change_id = element(submitted, "Id")
        .trim_start_matches("/change/")
        .to_string();
    assert!(
        !change_id.is_empty(),
        "case {case:?}: the accepted change carries no ChangeInfo id: {submitted}"
    );
    let mut status = element(submitted, "Status");
    assert!(
        status == PENDING || status == INSYNC,
        "case {case:?}: expected the change to be {PENDING} or {INSYNC}, got {status:?}"
    );
    let giving_up_at = ::std::time::Instant::now() + SETTLES_WITHIN;
    while status != INSYNC {
        ::tokio::time::sleep(LOOK_AGAIN).await;
        let (code, read) = route53(
            router,
            "GET",
            &format!("/{ROUTE53_API}/change/{change_id}"),
            "",
        )
        .await;
        assert_eq!(
            code, 200,
            "case {case:?}: expected 200 from GetChange, got {code} with {read}"
        );
        status = element(&read, "Status");
        assert!(
            ::std::time::Instant::now() < giving_up_at,
            "case {case:?}: the change {change_id} never reached {INSYNC}, last {status:?}"
        );
    }
}

/// Every `AliasTarget/DNSName` the zone's record sets carry, as the service reports them.
async fn alias_targets(router: &MultiServiceRouter, case: &str, zone_id: &str) -> Vec<String> {
    let (status, listed) = route53(
        router,
        "GET",
        &format!("/{ROUTE53_API}/hostedzone/{zone_id}/rrset"),
        "",
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from ListResourceRecordSets, got {status} with {listed}"
    );
    elements(&listed, "DNSName")
}

/// One row: how the distribution was minted, the record type the alias carries, and how the
/// published name is SPELLED in the target.
struct Row {
    case: &'static str,
    how: Mint,
    rr_type: &'static str,
    /// The published name as this row submits it.
    spell: fn(&str) -> String,
}

/// The published name as published, which is what a client that read it sends.
fn as_published(domain: &str) -> String {
    domain.to_string()
}

/// The same name in upper case: a DNS name is case-insensitive, so it names the same
/// distribution.
fn upper_cased(domain: &str) -> String {
    domain.to_ascii_uppercase()
}

/// The same name fully qualified: "The trailing dot is optional; Amazon Route 53 assumes that
/// the domain name is fully qualified" (`AliasTarget$DNSName`).
fn fully_qualified(domain: &str) -> String {
    format!("{domain}.")
}

/// The rows, crossing the minting operation, the record type and the spelling of the target.
const ROWS: &[Row] = &[
    Row {
        case: "an A alias to the name a create published",
        how: Mint::Create,
        rr_type: "A",
        spell: as_published,
    },
    Row {
        case: "an AAAA alias to that same name",
        how: Mint::Create,
        rr_type: "AAAA",
        spell: as_published,
    },
    Row {
        case: "an A alias to the name a tagged create published",
        how: Mint::CreateWithTags,
        rr_type: "A",
        spell: as_published,
    },
    Row {
        case: "an AAAA alias spelling that name in upper case",
        how: Mint::CreateWithTags,
        rr_type: "AAAA",
        spell: upper_cased,
    },
    Row {
        case: "an A alias spelling the name fully qualified",
        how: Mint::Create,
        rr_type: "A",
        spell: fully_qualified,
    },
];

/// The whole rule: every row's alias to a published name is accepted and settles, the listing
/// carries the target, the emulator answers on that name — and a target that lies in no
/// CloudFront zone is still refused.
#[tokio::test]
async fn an_alias_record_targets_the_name_a_distribution_published() {
    let router = build_router();

    // The zone the alias record sets live in, which is the account's own.
    let (status, zone) = route53(
        &router,
        "POST",
        &format!("/{ROUTE53_API}/hostedzone"),
        &format!(
            "<CreateHostedZoneRequest xmlns=\"{ROUTE53_XMLNS}\"><Name>{ZONE}</Name>\
             <CallerReference>cloudfront-alias-suite</CallerReference>\
             </CreateHostedZoneRequest>"
        ),
    )
    .await;
    assert_eq!(
        status, 201,
        "setup: expected 201 from CreateHostedZone, got {status} with {zone}"
    );
    let zone_id = element(&zone, "Id")
        .trim_start_matches("/hostedzone/")
        .to_string();

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let (id, domain) = minted_distribution(
            &router,
            row.how,
            &format!("cloudfront-alias-{index}"),
        )
        .await;
        awaited_deployment(&router, case, &id, &domain).await;

        // The alias the documented workflow writes: the customer's own name in the account's
        // zone, pointed at the distribution through the id CloudFront publishes for every one
        // of them.
        let name = format!("edge{index}.{ZONE}");
        let target = (row.spell)(&domain);
        let (status, changed) = aliased(
            &router,
            &zone_id,
            &name,
            row.rr_type,
            CLOUDFRONT_ALIAS_ZONE,
            &target,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from ChangeResourceRecordSets for the alias target \
             {target:?}, got {status} with {changed}"
        );
        awaited_change(&router, case, &changed).await;

        // The record set the zone holds carries the target as Route 53 reports one: the name
        // the owning service published, folded to lower case and fully qualified.
        let reported = format!("{}.", domain.to_ascii_lowercase());
        let targets = alias_targets(&router, case, &zone_id).await;
        assert!(
            targets.contains(&reported),
            "case {case:?}: expected the alias target {reported:?} among the zone's record \
             sets, got {targets:?}"
        );

        // The complementary half of the rule: the emulator ANSWERS on the name it published.
        // A viewer that states it is served by THAT distribution's edge, which is what the
        // edge's own report on the answer says — the front door answers no `X-Cache` and no
        // `X-Amz-Cf-Id` for a host it can place nowhere.
        let viewer = ::http::Request::builder()
            .method("GET")
            .uri("/index.html")
            .header("host", &domain)
            .body(Bytes::new())
            .expect("a well-formed viewer request");
        let answered = router.dispatch(viewer).await;
        let header = |name: &str| {
            answered
                .headers()
                .get(name)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_string()
        };
        let x_cache = header("x-cache");
        assert!(
            x_cache.ends_with("from cloudfront"),
            "case {case:?}: expected the edge to answer a viewer addressing {domain:?}, got \
             status {} with X-Cache {x_cache:?}",
            answered.status().as_u16()
        );
        assert!(
            !header("x-amz-cf-id").is_empty(),
            "case {case:?}: expected the edge's own X-Amz-Cf-Id on the answer for {domain:?}, \
             got the headers {:?}",
            answered.headers()
        );
    }

    // The CONTROL: a target that lies in NO zone CloudFront publishes, under the very id the
    // rows above are accepted for. The containment check is still running, so what the rows
    // show is the published name's own form rather than an accepted-everything rule.
    let elsewhere = "cdn.example.net";
    let (status, refused) = aliased(
        &router,
        &zone_id,
        &format!("control.{ZONE}"),
        "A",
        CLOUDFRONT_ALIAS_ZONE,
        elsewhere,
    )
    .await;
    assert_eq!(
        status, 400,
        "case \"a target outside CloudFront's zone\": expected 400, got {status} with {refused}"
    );
    assert_eq!(
        opt_element(&refused, "Code").as_deref(),
        Some("InvalidChangeBatch"),
        "case \"a target outside CloudFront's zone\": expected InvalidChangeBatch, got {refused}"
    );
    let expected = format!(
        "[Tried to create an alias that targets {elsewhere}., type A in zone \
         {CLOUDFRONT_ALIAS_ZONE}, but the alias target name does not lie within the target \
         zone]"
    );
    assert!(
        refused.contains(&expected),
        "case \"a target outside CloudFront's zone\": expected the message {expected:?}, got \
         {refused}"
    );

    // And nothing of that refusal was written: the zone still holds exactly the alias targets
    // the accepted rows put there.
    let targets = alias_targets(&router, "the refused control", &zone_id).await;
    assert!(
        !targets.iter().any(|target| target.contains(elsewhere)),
        "case \"a target outside CloudFront's zone\": expected the refused alias to be absent \
         from the zone, got {targets:?}"
    );
}
