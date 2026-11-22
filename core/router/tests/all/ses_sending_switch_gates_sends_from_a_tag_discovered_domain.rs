// SPDX-License-Identifier: CC0-1.0

//! Sending from the domain a client discovered among its OWN hosted zones.
//!
//! The rule under test: **the account's hosted zones, its email identities and its
//! configuration sets are ONE state behind every surface that reports them. So a client
//! that pages `ListHostedZones`, keeps the zones whose `Config/PrivateZone` says they are
//! public and reads each one's `ListTagsForResource` resolves exactly the public zones a
//! selector tag was written on; the apex of such a zone is a sender both SES APIs accept
//! once the verification the clock owes it is due; and a configuration set's sending
//! switch — written on either API — decides whether a send naming that set is served:
//! while the switch is off every v2 send naming the set is refused
//! `SendingPausedException` (HTTP 400) in the sentence that names the set, and while it is
//! on the same sends are served, each message answering a `MessageId` of its own — one per
//! bulk entry.**
//!
//! That is the whole pre-flight a mail client performs before it can send anything: find
//! the account's own DNS zone by tag, take its apex as the sending domain, and then drive
//! the configuration set that gates the send. A client that cannot complete it has no
//! sender to send from, so none of the sending behaviour behind it can be reached at all.
//!
//! Three crates have to agree for the rule to hold — Route 53 enumerates and tags the
//! zones, the classic SES API records the identity and owns the sending switch, and the v2
//! API answers for the identity and serves the sends — so the suite lives here: a
//! crate-local test only ever sees one side of it.
//!
//! The rows differ along what the rule generalises over: the number of labels in a zone's
//! name and the case it was created in, how many tags it carries and whether the selector
//! is among them at the value selected on, whether the zone is public or private, the page
//! size the enumeration is paged at, which API recorded the identity, which API created the
//! configuration set, which API wrote the sending switch, the form of the set's name, and
//! how many entries a bulk send carries. WHICH tag a client selects by is the account's own
//! choice and no part of any API, so the selector here is this suite's.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{blocks_of, elements, form_encode, opt_element};

/// The Region every request here signs for, which is also the Region the SES host names.
const REGION: &str = "us-east-1";

/// The SES endpoint host both APIs are addressed on in [`REGION`].
const SES_HOST: &str = "email.us-east-1.amazonaws.com";

/// The endpoint host a Route 53 request addresses — Route 53 is global.
const ROUTE53_HOST: &str = "route53.amazonaws.com";

/// SES v1's API version, which an `awsQuery` body carries beside its `Action`.
const SES_V1_API_VERSION: &str = "2010-12-01";

/// The `ResourceType` a hosted zone is tagged under (`TagResourceType$hostedzone`).
const HOSTED_ZONE: &str = "hostedzone";

/// The tag a client selects its sending zone by, and the value it selects at. Both halves
/// are the TEST's: a selector tag is something the account writes, so no part of it comes
/// from an API.
const SELECTOR_KEY: &str = "sending-zone";
const SELECTOR_VALUE: &str = "true";

/// The page sizes the enumeration is paged at: one zone per page, two, exactly the number
/// of zones the account holds, and the operation's own documented maximum.
const PAGE_SIZES: &[i32] = &[1, 2, 5, 100];

/// How long a poll waits for a transition the clock owes, and how often it looks.
///
/// The identity's pending -> verified move is owed on the emulator's own accelerated
/// schedule; the deadline is only here so a failure is a failure rather than a hang, and
/// nothing asserts how long the wait took.
const DEADLINE: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(50);

/// The recipients a bulk send addresses: the mailbox simulator, which is what a client
/// sends test traffic to. Each entry addresses a distinct one.
const SIMULATOR: &[&str] = &[
    "success@simulator.amazonses.com",
    "success+second@simulator.amazonses.com",
    "success+third@simulator.amazonses.com",
];

/// The template a bulk send renders, and the variable it substitutes.
const TEMPLATE: &str = "bulk-receipt";
const TEMPLATE_DATA: &str = "{\"name\":\"Ada\"}";

/// The two refusals the ROUTER itself answers, which no row here may provoke.
const ROUTER_REFUSALS: &[&str] = &["AmbiguousService", "UnknownService"];

/// Which of the account's email APIs a call is made on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Surface {
    /// The classic `awsQuery` API.
    V1,
    /// The v2 REST API.
    V2,
}

/// One zone of the account: how it is created, the tags written on it, and whether a
/// client selecting PUBLIC zones by the selector tag resolves it.
struct ZoneRow {
    case: &'static str,
    /// The name the create request spells, which need not be the canonical form.
    name: &'static str,
    /// The VPC a PRIVATE zone is created for; `None` for a public zone.
    vpc: Option<(&'static str, &'static str)>,
    /// The tags `ChangeTagsForResource` writes on it.
    tags: &'static [(&'static str, &'static str)],
    /// Whether the tag-driven discovery must resolve this zone.
    selected: bool,
}

/// The zones the account holds, spanning the forms the rule generalises over.
fn zone_rows() -> Vec<ZoneRow> {
    vec![
        ZoneRow {
            case: "a two-label public zone carrying the selector alone",
            name: "first-sender.example.com",
            vpc: None,
            tags: &[(SELECTOR_KEY, SELECTOR_VALUE)],
            selected: true,
        },
        ZoneRow {
            case: "a four-label public zone created in mixed case, selector among other tags",
            name: "Mail.Second-Sender.Example.Net",
            vpc: None,
            tags: &[
                ("Env", "prod"),
                (SELECTOR_KEY, SELECTOR_VALUE),
                ("owner:team", "dns"),
            ],
            selected: true,
        },
        ZoneRow {
            case: "a public zone carrying the selector key at another value",
            name: "third-sender.example.org",
            vpc: None,
            tags: &[(SELECTOR_KEY, "false"), ("Env", "staging")],
            selected: false,
        },
        ZoneRow {
            case: "a private zone carrying the selector",
            name: "internal.fourth-sender.example",
            vpc: Some(("vpc-0123456789abcdef0", REGION)),
            tags: &[(SELECTOR_KEY, SELECTOR_VALUE), ("Scope", "vpc")],
            selected: false,
        },
        ZoneRow {
            case: "a public zone nobody tagged",
            name: "untagged.fifth-sender.example.com",
            vpc: None,
            tags: &[],
            selected: false,
        },
    ]
}

/// The services this suite exercises, over ONE engine exactly as `core/router/src/lib.rs`
/// mounts them: Route 53 beside the whole family that shares the `ses` signing name, so
/// the claim that resolves each request to one service is exercised too.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // These rows exercise routing and handler behaviour, not SigV4.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_route53_awsdnsv20130401::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_sesv2_simpleemailservice_v2::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_ses_simpleemailservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_pinpointemail_amazonpinpointemailservice::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_mailmanager_mailmanagersvc::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name` — the signal
/// the router resolves a routing key from. The signature itself is never checked.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host;x-amz-date, Signature=deadbeef"
    )
}

/// One answered request: its status, its body as text and as JSON, and the error shape the
/// restJson1 wire names in a header.
struct Answer {
    status: u16,
    text: String,
    json: Value,
    error_type: Option<String>,
}

impl Answer {
    /// The modelled error code the answer names, or the empty string on success.
    ///
    /// restJson1 resolves the error shape from `x-amzn-errortype`, with the body's `__type`
    /// carrying the same name; awsQuery writes it as `Error/Code`.
    fn error_code(&self) -> String {
        let raw = self
            .error_type
            .as_deref()
            .map(str::to_string)
            .or_else(|| {
                self.json
                    .get("__type")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .or_else(|| opt_element(&self.text, "Code"))
            .unwrap_or_default();
        let name = raw.split('#').next_back().unwrap_or(&raw).to_string();
        name.split(':').next().unwrap_or(&name).to_string()
    }

    /// The sentence the refusal carried, on either wire.
    fn message(&self) -> String {
        self.json
            .get("message")
            .or_else(|| self.json.get("Message"))
            .and_then(Value::as_str)
            .map(str::to_string)
            // The awsQuery wire escapes the angle brackets SES delimits a named value
            // with, so the sentence is read back decoded.
            .or_else(|| elements(&self.text, "Message").into_iter().next())
            .unwrap_or_default()
    }

    /// The `MessageId` a send answered with, or the empty string when it carried none.
    fn message_id(&self) -> String {
        self.json
            .get("MessageId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }
}

/// Send one request, refusing at the door the two answers the ROUTER itself produces: a
/// request that reached nobody, or one several services claimed.
async fn send(
    router: &MultiServiceRouter,
    case: &str,
    leg: &str,
    request: http::Request<Bytes>,
) -> Answer {
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let error_type = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let text = String::from_utf8_lossy(response.body()).to_string();
    for refusal in ROUTER_REFUSALS {
        assert!(
            !text.contains(refusal)
                && !error_type.as_deref().unwrap_or_default().contains(refusal),
            "case {case:?}: {leg} must be served by exactly one service, got HTTP {status}: {text}"
        );
    }
    let json = serde_json::from_str(&text).unwrap_or(Value::Null);
    Answer {
        status,
        text,
        json,
        error_type,
    }
}

/// Call one Route 53 operation over its restXml wire, requiring HTTP `expected`.
async fn route53(
    router: &MultiServiceRouter,
    case: &str,
    method: &str,
    uri: &str,
    body: &str,
    expected: u16,
) -> String {
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{ROUTE53_HOST}{uri}"))
        .header(http::header::HOST, ROUTE53_HOST)
        .header(http::header::AUTHORIZATION, authorization("route53"))
        .header(http::header::CONTENT_TYPE, "application/xml")
        .body(Bytes::from(body.to_owned()))
        .expect("a well-formed request");
    let answered = send(router, case, &format!("Route 53 {method} {uri}"), request).await;
    assert_eq!(
        answered.status, expected,
        "case {case:?}: Route 53 {method} {uri} expected HTTP {expected}, got {} with body {}",
        answered.status, answered.text
    );
    answered.text
}

/// One classic-API call: `awsQuery` under the `ses` credential scope, `POST /` with the
/// operation in the `Action` form field.
async fn v1(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    fields: &[(&str, String)],
) -> Answer {
    let mut body = format!("Action={action}&Version={SES_V1_API_VERSION}");
    for (name, value) in fields {
        body.push('&');
        body.push_str(name);
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{SES_HOST}/"))
        .header(http::header::HOST, SES_HOST)
        .header(http::header::AUTHORIZATION, authorization("ses"))
        .header(
            http::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .body(Bytes::from(body))
        .expect("a well-formed request");
    send(router, case, &format!("SES v1 {action}"), request).await
}

/// [`v1`] where the call must be served.
async fn v1_ok(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    fields: &[(&str, String)],
) -> String {
    let answered = v1(router, case, action, fields).await;
    assert_eq!(
        answered.status, 200,
        "case {case:?}: SES v1 {action} expected HTTP 200, got {} with body {}",
        answered.status, answered.text
    );
    answered.text
}

/// One v2-API call: `restJson1` under the `ses` credential scope, in the `/v2/email/…`
/// path space.
async fn v2(
    router: &MultiServiceRouter,
    case: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Answer {
    let payload = match &body {
        Some(value) => Bytes::from(value.to_string()),
        None => Bytes::new(),
    };
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{SES_HOST}{path}"))
        .header(http::header::HOST, SES_HOST)
        .header(http::header::AUTHORIZATION, authorization("ses"))
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(payload)
        .expect("a well-formed request");
    send(router, case, &format!("SESv2 {method} {path}"), request).await
}

/// [`v2`] where the call must be served.
async fn v2_ok(
    router: &MultiServiceRouter,
    case: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Value {
    let answered = v2(router, case, method, path, body).await;
    assert_eq!(
        answered.status, 200,
        "case {case:?}: SESv2 {method} {path} expected HTTP 200, got {} with body {}",
        answered.status, answered.text
    );
    answered.json
}

/// One enumerated hosted zone, as a client reads it off `ListHostedZones`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ListedZone {
    /// The zone id with the `/hostedzone/` prefix stripped: the segment after the last
    /// `/`, which is the form every `ResourceId` member takes.
    id: String,
    /// The zone's name as the service canonicalised it.
    name: String,
    /// `Config/PrivateZone`, which the enumeration always reports.
    private: bool,
}

/// Page `ListHostedZones` to the end at `page_size`, answering every zone it enumerated.
///
/// This IS the client-side procedure under test: follow `NextMarker` while `IsTruncated`
/// is true, exactly as an SDK paginator does.
async fn all_zones(router: &MultiServiceRouter, case: &str, page_size: i32) -> Vec<ListedZone> {
    let mut seen: Vec<ListedZone> = Vec::new();
    let mut marker: Option<String> = None;
    loop {
        let mut query = format!("?maxitems={page_size}");
        if let Some(marker) = &marker {
            query.push_str(&format!("&marker={marker}"));
        }
        let page = route53(
            router,
            case,
            "GET",
            &format!("/2013-04-01/hostedzone{query}"),
            "",
            200,
        )
        .await;
        for zone in blocks_of(&page, "HostedZone") {
            let id = elements(zone, "Id").into_iter().next().unwrap_or_else(|| {
                panic!("case {case:?}: an enumerated zone must carry an Id, got {zone}")
            });
            let private = elements(zone, "PrivateZone")
                .into_iter()
                .next()
                .unwrap_or_else(|| {
                    panic!(
                        "case {case:?}: an enumerated zone must report Config/PrivateZone, got \
                         {zone}"
                    )
                });
            seen.push(ListedZone {
                id: id.rsplit('/').next().unwrap_or_default().to_string(),
                name: elements(zone, "Name").into_iter().next().unwrap_or_default(),
                private: private == "true",
            });
        }
        let truncated = opt_element(&page, "IsTruncated").unwrap_or_default() == "true";
        let next = opt_element(&page, "NextMarker");
        assert_eq!(
            truncated,
            next.is_some(),
            "case {case:?}: NextMarker is present exactly when IsTruncated is true, got \
             IsTruncated {truncated} and NextMarker {next:?}"
        );
        if !truncated {
            return seen;
        }
        marker = next;
        assert!(
            seen.len() < 1_000,
            "case {case:?}: the ListHostedZones pagination did not terminate, saw {seen:?}"
        );
    }
}

/// The tag map `ListTagsForResource` reports for one hosted zone.
async fn zone_tags(router: &MultiServiceRouter, case: &str, id: &str) -> BTreeMap<String, String> {
    let read = route53(
        router,
        case,
        "GET",
        &format!("/2013-04-01/tags/{HOSTED_ZONE}/{id}"),
        "",
        200,
    )
    .await;
    assert_eq!(
        opt_element(&read, "ResourceId").as_deref(),
        Some(id),
        "case {case:?}: the tag set must name the zone it was read for, got {read}"
    );
    blocks_of(&read, "Tag")
        .into_iter()
        .filter_map(|tag| {
            let key = elements(tag, "Key").into_iter().next()?;
            let value = elements(tag, "Value").into_iter().next()?;
            Some((key, value))
        })
        .collect()
}

/// Create one row's zone and write its tags, answering its bare id.
async fn create_and_tag(router: &MultiServiceRouter, row: &ZoneRow, reference: &str) -> String {
    let vpc = match row.vpc {
        Some((id, region)) => {
            format!("<VPC><VPCId>{id}</VPCId><VPCRegion>{region}</VPCRegion></VPC>")
        }
        None => String::new(),
    };
    let created = route53(
        router,
        row.case,
        "POST",
        "/2013-04-01/hostedzone",
        &format!(
            "<CreateHostedZoneRequest xmlns=\"https://route53.amazonaws.com/doc/2013-04-01/\">\
             <Name>{}</Name>{vpc}<CallerReference>{reference}</CallerReference>\
             </CreateHostedZoneRequest>",
            row.name
        ),
        201,
    )
    .await;
    let id = opt_element(&created, "Id").unwrap_or_else(|| {
        panic!(
            "case {:?}: the create must answer HostedZone/Id, got {created}",
            row.case
        )
    });
    let id = id.rsplit('/').next().unwrap_or_default().to_string();

    if !row.tags.is_empty() {
        let tags: String = row
            .tags
            .iter()
            .map(|(key, value)| format!("<Tag><Key>{key}</Key><Value>{value}</Value></Tag>"))
            .collect();
        route53(
            router,
            row.case,
            "POST",
            &format!("/2013-04-01/tags/{HOSTED_ZONE}/{id}"),
            &format!(
                "<ChangeTagsForResourceRequest \
                 xmlns=\"https://route53.amazonaws.com/doc/2013-04-01/\">\
                 <AddTags>{tags}</AddTags></ChangeTagsForResourceRequest>"
            ),
            200,
        )
        .await;
    }
    id
}

/// One sending domain a client resolved for itself.
#[derive(Debug, Clone)]
struct Discovered {
    /// The zone's apex with the trailing dot stripped, which is the identity's name.
    apex: String,
    /// The bare id of the zone it was resolved from.
    zone_id: String,
}

/// The sending domains a tag-driven client resolves: page the zones, keep the public ones,
/// read each one's tags and select the zones the selector tag was written on.
///
/// Both reads are REPORTING calls, so the walk is made at every page size and the
/// selection may not depend on how the pages fell.
async fn discover(router: &MultiServiceRouter, case: &str) -> Vec<Discovered> {
    let mut resolved: Option<Vec<Discovered>> = None;
    for &page_size in PAGE_SIZES {
        let case = format!("{case} (paged {page_size} at a time)");
        let mut selected: Vec<Discovered> = Vec::new();
        for zone in all_zones(router, &case, page_size).await {
            if zone.private {
                continue;
            }
            let tags = zone_tags(router, &case, &zone.id).await;
            if tags.get(SELECTOR_KEY).map(String::as_str) == Some(SELECTOR_VALUE) {
                selected.push(Discovered {
                    apex: zone.name.trim_end_matches('.').to_string(),
                    zone_id: zone.id.clone(),
                });
            }
        }
        selected.sort_by(|left, right| left.apex.cmp(&right.apex));
        match &resolved {
            None => resolved = Some(selected),
            Some(first) => assert_eq!(
                first
                    .iter()
                    .map(|found| (found.apex.clone(), found.zone_id.clone()))
                    .collect::<Vec<_>>(),
                selected
                    .iter()
                    .map(|found| (found.apex.clone(), found.zone_id.clone()))
                    .collect::<Vec<_>>(),
                "case {case:?}: the page size cannot change which zones the selector resolves"
            ),
        }
    }
    resolved.unwrap_or_default()
}

/// Record the identity for `apex` on `surface`, and wait for the verification the clock
/// owes it: the v2 plane must then report it a DOMAIN identity, verified for sending, with
/// its Easy DKIM set up — which is what makes the apex a sender.
async fn verified_sender(router: &MultiServiceRouter, apex: &str, surface: Surface) {
    let case = format!("the apex {apex} recorded on {surface:?}");
    match surface {
        Surface::V2 => {
            let created = v2_ok(
                router,
                &case,
                "POST",
                "/v2/email/identities",
                Some(json!({ "EmailIdentity": apex })),
            )
            .await;
            assert_eq!(
                created["IdentityType"],
                json!("DOMAIN"),
                "case {case:?}: a zone apex is a DOMAIN identity, got {created}"
            );
        }
        Surface::V1 => {
            let recorded = v1_ok(
                router,
                &case,
                "VerifyDomainIdentity",
                &[("Domain", apex.to_string())],
            )
            .await;
            assert!(
                opt_element(&recorded, "VerificationToken").is_some_and(|token| !token.is_empty()),
                "case {case:?}: VerifyDomainIdentity must answer the TXT token to publish, got \
                 {recorded}"
            );
            // Easy DKIM for the domain, so the identity carries the same setup a v2 create
            // starts it with.
            let dkim = v1_ok(
                router,
                &case,
                "VerifyDomainDkim",
                &[("Domain", apex.to_string())],
            )
            .await;
            assert!(
                !elements(&dkim, "member").is_empty(),
                "case {case:?}: VerifyDomainDkim must answer the account's DKIM tokens, got {dkim}"
            );
        }
    }

    let deadline = Instant::now() + DEADLINE;
    let read = loop {
        let read = v2_ok(
            router,
            &case,
            "GET",
            &format!("/v2/email/identities/{apex}"),
            None,
        )
        .await;
        if read["VerifiedForSendingStatus"] == json!(true) {
            break read;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the identity must reach VerifiedForSendingStatus true, last read \
             {read}"
        );
        tokio::time::sleep(POLL).await;
    };
    assert_eq!(
        read["IdentityType"],
        json!("DOMAIN"),
        "case {case:?}: the apex is reported as a DOMAIN identity, got {read}"
    );
    assert_eq!(
        read["DkimAttributes"]["Status"],
        json!("SUCCESS"),
        "case {case:?}: a verified domain identity reports its DKIM set up, got {read}"
    );
}

/// The account this suite sends from: the zones created and tagged, the selector-tagged
/// public ones discovered, and an identity recorded for each discovered apex — one on each
/// API, so neither recording surface is the only one the rule is asserted through.
async fn account_with_discovered_senders(router: &MultiServiceRouter) -> Vec<Discovered> {
    let rows = zone_rows();
    for (index, row) in rows.iter().enumerate() {
        create_and_tag(router, row, &format!("sending-zone-ref-{index}")).await;
    }

    let discovered = discover(router, "the account's sending zones").await;
    let mut expected: Vec<String> = rows
        .iter()
        .filter(|row| row.selected)
        .map(|row| row.name.trim_end_matches('.').to_ascii_lowercase())
        .collect();
    expected.sort();
    let mut found: Vec<String> = discovered
        .iter()
        .map(|zone| zone.apex.to_ascii_lowercase())
        .collect();
    found.sort();
    assert_eq!(
        found, expected,
        "the discovery must resolve exactly the PUBLIC zones the selector was written on at \
         its selected value, got {discovered:?}"
    );

    for (index, zone) in discovered.iter().enumerate() {
        let surface = if index % 2 == 0 {
            Surface::V2
        } else {
            Surface::V1
        };
        verified_sender(router, &zone.apex, surface).await;
    }
    discovered
}

/// The sentence a send through a paused configuration set is refused with: fixed text
/// around the one variable part, the set the request named.
fn paused_message(set: &str) -> String {
    format!("Sending is paused for configuration set {set}")
}

/// Write the sending switch of `set` on `surface`.
async fn switch_sending(
    router: &MultiServiceRouter,
    case: &str,
    surface: Surface,
    set: &str,
    enabled: bool,
) {
    match surface {
        Surface::V1 => {
            v1_ok(
                router,
                case,
                "UpdateConfigurationSetSendingEnabled",
                &[
                    ("ConfigurationSetName", set.to_string()),
                    ("Enabled", enabled.to_string()),
                ],
            )
            .await;
        }
        Surface::V2 => {
            v2_ok(
                router,
                case,
                "PUT",
                &format!("/v2/email/configuration-sets/{set}/sending"),
                Some(json!({ "SendingEnabled": enabled })),
            )
            .await;
        }
    }
    // Whichever API wrote it, the stored set reports it: the switch is the ACCOUNT's set's.
    let read = v2_ok(
        router,
        case,
        "GET",
        &format!("/v2/email/configuration-sets/{set}"),
        None,
    )
    .await;
    assert_eq!(
        read["SendingOptions"]["SendingEnabled"]
            .as_bool()
            .unwrap_or(false),
        enabled,
        "case {case:?}: the set must report the switch that was written on it, got {read}"
    );
}

/// `SendEmail` on the v2 API, from `apex` and naming `set` when the leg supplies one.
async fn send_email(
    router: &MultiServiceRouter,
    case: &str,
    apex: &str,
    set: Option<&str>,
) -> Answer {
    let mut body = json!({
        "FromEmailAddress": format!("noreply@{apex}"),
        "Destination": { "ToAddresses": [SIMULATOR[0]] },
        "Content": {"Simple": {
            "Subject": {"Data": "a receipt"},
            "Body": {"Text": {"Data": "thank you"}},
        }},
    });
    if let Some(set) = set {
        body["ConfigurationSetName"] = json!(set);
    }
    v2(
        router,
        case,
        "POST",
        "/v2/email/outbound-emails",
        Some(body),
    )
    .await
}

/// `SendBulkEmail` on the v2 API: one entry per simulator recipient, rendering the one
/// template this suite created.
async fn send_bulk_email(
    router: &MultiServiceRouter,
    case: &str,
    apex: &str,
    set: &str,
    entries: usize,
) -> Answer {
    let bulk_entries: Vec<Value> = SIMULATOR[..entries]
        .iter()
        .map(|recipient| json!({"Destination": {"ToAddresses": [recipient]}}))
        .collect();
    v2(
        router,
        case,
        "POST",
        "/v2/email/outbound-bulk-emails",
        Some(json!({
            "FromEmailAddress": format!("noreply@{apex}"),
            "ConfigurationSetName": set,
            "DefaultContent": {"Template": {
                "TemplateName": TEMPLATE,
                "TemplateData": TEMPLATE_DATA,
            }},
            "BulkEmailEntries": bulk_entries,
        })),
    )
    .await
}

/// One sending row: which API created the configuration set, which API writes its sending
/// switch, the form of its name, and how many entries its bulk send carries.
struct SendRow {
    case: &'static str,
    created_on: Surface,
    switched_on: Surface,
    set: &'static str,
    entries: usize,
}

/// The rows, spanning both creating surfaces, both switching surfaces, the name forms a
/// `ConfigurationSetName` takes and the bulk sizes.
fn send_rows() -> Vec<SendRow> {
    vec![
        SendRow {
            case: "a set created and switched on the classic API, one bulk entry",
            created_on: Surface::V1,
            switched_on: Surface::V1,
            set: "classic-set-1",
            entries: 1,
        },
        SendRow {
            case: "a set created on the classic API and switched on the v2 API, two entries",
            created_on: Surface::V1,
            switched_on: Surface::V2,
            set: "Classic_Set-2",
            entries: 2,
        },
        SendRow {
            case: "a set created on the v2 API and switched on the classic API, three entries",
            created_on: Surface::V2,
            switched_on: Surface::V1,
            set: "v2-set-3",
            entries: 3,
        },
        SendRow {
            case: "a one-character set created and switched on the v2 API, two entries",
            created_on: Surface::V2,
            switched_on: Surface::V2,
            set: "s",
            entries: 2,
        },
    ]
}

/// Record `row`'s configuration set on the API the row names.
async fn create_set(router: &MultiServiceRouter, row: &SendRow) {
    match row.created_on {
        Surface::V1 => {
            v1_ok(
                router,
                row.case,
                "CreateConfigurationSet",
                &[("ConfigurationSet.Name", row.set.to_string())],
            )
            .await;
        }
        Surface::V2 => {
            v2_ok(
                router,
                row.case,
                "POST",
                "/v2/email/configuration-sets",
                Some(json!({ "ConfigurationSetName": row.set })),
            )
            .await;
        }
    }
}

/// Every `MessageId` a served bulk send answered, asserting one result per entry, each
/// accepted and carrying an id of its own.
fn bulk_message_ids(answered: &Answer, case: &str, entries: usize) -> Vec<String> {
    let results = answered.json["BulkEmailEntryResults"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        results.len(),
        entries,
        "case {case:?}: a bulk send answers one result per entry, expected {entries}, got {}",
        answered.text
    );
    results
        .iter()
        .map(|result| {
            assert_eq!(
                result["Status"].as_str(),
                Some("SUCCESS"),
                "case {case:?}: every entry of this send is accepted, got {result}"
            );
            let id = result["MessageId"].as_str().unwrap_or_default().to_string();
            assert!(
                !id.is_empty(),
                "case {case:?}: an accepted entry answers a MessageId of its own, got {result}"
            );
            id
        })
        .collect()
}

#[tokio::test]
async fn a_paused_configuration_set_blocks_the_sends_that_name_it_and_restoring_it_serves_them() {
    let router = build_router();
    let discovered = account_with_discovered_senders(&router).await;
    assert!(
        !discovered.is_empty(),
        "the account must hold at least one tag-discovered sending domain"
    );
    // The one template every bulk send in this suite renders.
    v2_ok(
        &router,
        "the template a bulk send renders",
        "POST",
        "/v2/email/templates",
        Some(json!({
            "TemplateName": TEMPLATE,
            "TemplateContent": {
                "Subject": "Your receipt, {{name}}",
                "Text": "Thank you {{name}}",
            },
        })),
    )
    .await;

    let rows = send_rows();
    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        // Each row sends from one of the discovered domains, cycling through them so both
        // recording surfaces carry a send.
        let apex = &discovered[index % discovered.len()].apex;
        create_set(&router, row).await;

        // Every MessageId this row minted: no send may replay another's.
        let mut minted: BTreeSet<String> = BTreeSet::new();

        // The set a create leaves with sending ON serves the sends that name it.
        let served = send_email(&router, case, apex, Some(row.set)).await;
        assert_eq!(
            served.status, 200,
            "case {case:?}: a set whose sending is on serves the send naming it, got {} with \
             body {}",
            served.status, served.text
        );
        assert!(
            minted.insert(served.message_id()),
            "case {case:?}: every accepted message answers a MessageId of its own, got {}",
            served.text
        );

        // Switched OFF: every send naming the set is refused, in the sentence naming it.
        switch_sending(&router, case, row.switched_on, row.set, false).await;
        let refused = send_email(&router, case, apex, Some(row.set)).await;
        assert_eq!(
            (refused.status, refused.error_code().as_str()),
            (400, "SendingPausedException"),
            "case {case:?}: a send naming a paused set is refused SendingPausedException, got \
             {} with body {}",
            refused.status,
            refused.text
        );
        assert_eq!(
            refused.message(),
            paused_message(row.set),
            "case {case:?}: the refusal names the set whose sending is paused, got {}",
            refused.text
        );
        assert!(
            refused.message_id().is_empty(),
            "case {case:?}: a refused send mints no MessageId, got {}",
            refused.text
        );

        let refused_bulk = send_bulk_email(&router, case, apex, row.set, row.entries).await;
        assert_eq!(
            (refused_bulk.status, refused_bulk.error_code().as_str()),
            (400, "SendingPausedException"),
            "case {case:?}: a bulk send naming a paused set is refused the same way, got {} with \
             body {}",
            refused_bulk.status,
            refused_bulk.text
        );
        assert_eq!(
            refused_bulk.message(),
            paused_message(row.set),
            "case {case:?}: the bulk refusal names the set whose sending is paused, got {}",
            refused_bulk.text
        );

        // The pause gates the sends that NAME the set, and no others.
        let unnamed = send_email(&router, case, apex, None).await;
        assert_eq!(
            unnamed.status, 200,
            "case {case:?}: a send naming no configuration set is not gated by a paused one, \
             got {} with body {}",
            unnamed.status, unnamed.text
        );
        assert!(
            minted.insert(unnamed.message_id()),
            "case {case:?}: every accepted message answers a MessageId of its own, got {}",
            unnamed.text
        );

        // Switched back ON: the same sends are served again.
        switch_sending(&router, case, row.switched_on, row.set, true).await;
        let restored = send_email(&router, case, apex, Some(row.set)).await;
        assert_eq!(
            restored.status, 200,
            "case {case:?}: restoring the switch serves the send naming the set again, got {} \
             with body {}",
            restored.status, restored.text
        );
        assert!(
            minted.insert(restored.message_id()),
            "case {case:?}: the restored send mints a MessageId of its own rather than replaying \
             an earlier one, got {}",
            restored.text
        );

        let bulk = send_bulk_email(&router, case, apex, row.set, row.entries).await;
        assert_eq!(
            bulk.status, 200,
            "case {case:?}: the restored set serves the bulk send naming it, got {} with body {}",
            bulk.status, bulk.text
        );
        for id in bulk_message_ids(&bulk, case, row.entries) {
            assert!(
                minted.insert(id.clone()),
                "case {case:?}: each bulk entry answers a MessageId of its own, and {id} was \
                 already answered for another message"
            );
        }
    }
}

#[tokio::test]
async fn a_tracking_domain_is_admitted_exactly_under_a_discovered_verified_apex() {
    // The other write a client makes against the domain it discovered: a configuration
    // set's custom open-and-click redirect domain, which SES admits only under a domain
    // this account has verified — so the discovered apex covers it and a domain of a zone
    // the account never verified does not.
    let router = build_router();
    let discovered = account_with_discovered_senders(&router).await;

    for (index, zone) in discovered.iter().enumerate() {
        let case = format!("the tracking domain of the apex {}", zone.apex);
        let set = format!("tracking-set-{index}");
        v1_ok(
            &router,
            &case,
            "CreateConfigurationSet",
            &[("ConfigurationSet.Name", set.clone())],
        )
        .await;

        // A subdomain of the discovered apex: what a client points its tracking CNAME at.
        let tracking = format!("click.{}", zone.apex);
        v1_ok(
            &router,
            &case,
            "CreateConfigurationSetTrackingOptions",
            &[
                ("ConfigurationSetName", set.clone()),
                ("TrackingOptions.CustomRedirectDomain", tracking.clone()),
            ],
        )
        .await;
        let read = v2_ok(
            &router,
            &case,
            "GET",
            &format!("/v2/email/configuration-sets/{set}"),
            None,
        )
        .await;
        assert_eq!(
            read["TrackingOptions"]["CustomRedirectDomain"].as_str(),
            Some(tracking.as_str()),
            "case {case:?}: the set must report the tracking domain written on it, got {read}"
        );

        // And the same write for a domain no identity of this account covers is refused as
        // invalid tracking options, in the sentence naming that domain.
        let unverified = "click.never-verified.example.test";
        let other = format!("{set}-unverified");
        v1_ok(
            &router,
            &case,
            "CreateConfigurationSet",
            &[("ConfigurationSet.Name", other.clone())],
        )
        .await;
        let refused = v1(
            &router,
            &case,
            "CreateConfigurationSetTrackingOptions",
            &[
                ("ConfigurationSetName", other.clone()),
                (
                    "TrackingOptions.CustomRedirectDomain",
                    unverified.to_string(),
                ),
            ],
        )
        .await;
        assert_eq!(
            (refused.status, refused.error_code().as_str()),
            (400, "InvalidTrackingOptions"),
            "case {case:?}: a tracking domain no verified identity covers is refused \
             InvalidTrackingOptions, got {} with body {}",
            refused.status,
            refused.text
        );
        assert_eq!(
            refused.message(),
            format!("Domain <{unverified}> is not verified under this account."),
            "case {case:?}: the refusal names the domain it rejected, got {}",
            refused.text
        );
    }
}
