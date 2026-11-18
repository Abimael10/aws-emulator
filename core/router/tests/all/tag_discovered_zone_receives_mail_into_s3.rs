// SPDX-License-Identifier: CC0-1.0

//! Mail for the domain a client FINDS by tag among its own DNS zones, received into Amazon S3.
//!
//! The rule under test: **a hosted zone the account created is enumerated by `ListHostedZones`
//! across every `Marker`/`NextMarker` page, reporting the `Config.PrivateZone` flag it was
//! created with, and `ListTagsForResource('hostedzone', <the zone id after the last '/'>)`
//! reports exactly the tags the last `ChangeTagsForResource` batch left on it — so a client
//! that pages the zones, keeps the PUBLIC ones and reads each one's tags resolves the zone it
//! selects by tag; the apex of that zone is an SESv2 email identity verified for sending once
//! the verification the clock owes is due; and a message Amazon SES then accepts for a
//! recipient of that apex is RECEIVED by the account, so every enabled rule of the ACTIVE
//! receipt rule set whose `Recipients` match carries out its `S3Action` — ONE object at
//! `<ObjectKeyPrefix><MessageId>` of the bucket the action names, carrying the message's own
//! RFC 5322 bytes unaltered behind the header fields Amazon SES stamped on it, and sealed in
//! the Amazon S3 encryption client's envelope when the action names a KMS key.**
//!
//! That is the whole path an inbound-mail deployment stands on: find the account's own zone by
//! tag (`route53 list-hosted-zones` plus `list-tags-for-resource`), treat its apex as the
//! verified domain, and let the active receipt rule set drop what arrives for it into a bucket.
//! Five crates have to agree for it to hold — Route 53 enumerates and projects tags, SESv2
//! answers for the identity of the apex, Amazon SES receives and acts, Amazon S3 holds the
//! object and KMS seals it — so the suite lives here: a crate-local test only ever sees one
//! side of it.
//!
//! The rows differ along what the rule generalises over: the page size the zones are
//! enumerated at, whether a zone is public or private and whether it carries the selector tag,
//! the form of the rule's recipient condition (the domain, one address, none at all), the
//! `ObjectKeyPrefix` the object is placed under (none, one segment, nested), whether the rule
//! scans, whether the action names a KMS key, and the message itself (a plain message, a
//! multipart message carrying an attachment, and one carrying each published scan test
//! signature). The zone apex, the prefix and the message an issue happens to mention are one
//! row's values.
//!
//! A message is received AFTER it is accepted, so every assertion about an object waits for
//! the CONDITION — the object appearing under the prefix — rather than for a span of time.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{blocks_of, elements, form_encode, opt_element};

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// The API version Amazon SES's query API carries on every request.
const SES_VERSION: &str = "2010-12-01";

/// The `ResourceType` a hosted zone is tagged under (`TagResourceType$hostedzone`).
const HOSTED_ZONE: &str = "hostedzone";

/// The tag a deployment selects its receiving zone by. The rule is about ANY tag, so the other
/// zones here spell their keys differently and one of them carries this key too.
const SELECTOR_KEY: &str = "inbound-mail-baseline";
const SELECTOR_VALUE: &str = "true";

/// The page sizes the enumeration is paged at: one zone per page, two, exactly the number of
/// zones the account holds, and the operation's own documented maximum — "Amazon Route 53
/// returns a maximum of 100 items in each response" (`ListHostedZones`).
const PAGE_SIZES: &[i32] = &[1, 2, 3, 100];

/// How long a wait for a condition goes on for, and how often it looks again.
///
/// Generously longer than the in-process work takes: what an assertion is about is the state
/// reached, never how quickly it was reached, so a loaded host makes these waits more patient
/// and never less.
const DEADLINE: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(20);

/// The status Amazon S3 answers a stored `PutBucketPolicy` with: the write has no body to
/// return, so S3 reports `204 No Content`.
const BUCKET_POLICY_STORED: u16 = 204;

/// The two refusals the ROUTER itself answers, which no call here may provoke.
const ROUTER_REFUSALS: &[&str] = &["AmbiguousService", "UnknownService"];

/// The published Generic Test for Unsolicited Bulk Email — the one pattern a spam filter is
/// expected to report on so that reporting can be tested.
const SPAM_TEST_PATTERN: &str =
    "XJS*C4JDBQADN1.NSBN3*2IDNEN*GTUBE-STANDARD-ANTI-UBE-TEST-EMAIL*C.34X";

/// The published anti-malware test file — the one content an anti-virus scanner is expected to
/// report on so that reporting can be tested.
const VIRUS_TEST_SIGNATURE: &str =
    r"X5O!P%@AP[4\PZX54(P^)7CC)7}$EICAR-STANDARD-ANTIVIRUS-TEST-FILE!$H+H*";

/// The verdict of a scan that found nothing, and of one that found what it scans for
/// (`ReceiptAction`'s scan verdicts, as Amazon SES stamps them on a scanned message).
const VERDICT_PASS: &str = "PASS";
const VERDICT_FAIL: &str = "FAIL";

/// `Received-SPF` (RFC 7208 §9.1) and `Authentication-Results` (RFC 7601 §2), the two fields a
/// scanning rule has the authentication results reported in, and the two Amazon SES reports
/// each scan's verdict in.
const RECEIVED_SPF_HEADER: &str = "received-spf";
const AUTHENTICATION_RESULTS_HEADER: &str = "authentication-results";
const SPAM_VERDICT_HEADER: &str = "x-ses-spam-verdict";
const VIRUS_VERDICT_HEADER: &str = "x-ses-virus-verdict";

/// The user-metadata entries of the Amazon S3 encryption client's envelope, as a `GetObject`
/// answers them: the wrapped data key, the initialisation vector, the two algorithms, the
/// authentication tag's length, the material description and the plaintext's length.
const WRAPPED_KEY_METADATA: &str = "x-amz-meta-x-amz-key-v2";
const IV_METADATA: &str = "x-amz-meta-x-amz-iv";
const CONTENT_ALGORITHM_METADATA: &str = "x-amz-meta-x-amz-cek-alg";
const WRAP_ALGORITHM_METADATA: &str = "x-amz-meta-x-amz-wrap-alg";
const TAG_LENGTH_METADATA: &str = "x-amz-meta-x-amz-tag-len";
const MATERIAL_DESCRIPTION_METADATA: &str = "x-amz-meta-x-amz-matdesc";
const PLAINTEXT_LENGTH_METADATA: &str = "x-amz-meta-x-amz-unencrypted-content-length";

/// How many bytes AES-GCM's authentication tag adds to the ciphertext, which is what the
/// envelope's declared tag length says ([`TAG_LENGTH_METADATA`] is in BITS).
const TAG_BYTES: usize = 16;

/// The account this emulator serves, which owns every zone, identity, bucket and key here.
fn account() -> String {
    engine::EMULATOR_ACCOUNT_ID.to_string()
}

/// One answered request, reduced to what the next step reads off it.
struct Answer {
    status: u16,
    /// The response headers, names lowercased.
    headers: Vec<(String, String)>,
    body: Bytes,
}

impl Answer {
    /// The body as text, for an XML or JSON answer.
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// The body rendered for an assertion message, without dumping a whole message into it.
    fn summary(&self) -> String {
        let shown = String::from_utf8_lossy(&self.body[..self.body.len().min(160)]);
        format!(
            "{} byte(s) {shown:?}{}",
            self.body.len(),
            if self.body.len() > 160 { "…" } else { "" }
        )
    }

    /// The value of one response header, or `None` when the answer carries none.
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(held, _)| held == name)
            .map(|(_, value)| value.as_str())
    }
}

/// Route 53, SESv2, Amazon SES, Amazon S3 and AWS KMS mounted on ONE engine, exactly as
/// `core/router/src/lib.rs` mounts them, with request authentication off so every call carries
/// a credential scope (which is what names the Region and the service) without a signature.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_route53_awsdnsv20130401::EmulatedService::new(Arc::clone(
                &engine,
            )),
        );
        router.register_service(
            service_com_amazonaws_sesv2_simpleemailservice_v2::EmulatedService::new(Arc::clone(
                &engine,
            )),
        );
        router.register_service(
            service_com_amazonaws_ses_simpleemailservice::EmulatedService::new(Arc::clone(
                &engine,
            )),
        );
        router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
            Arc::clone(&engine),
        ));
        router.register_service(service_com_amazonaws_kms_trentservice::EmulatedService::new(
            engine,
        ));
        Self { router }
    }

    /// One call to `path` on `signing_name`'s endpoint, refusing at the door the two answers
    /// the ROUTER itself produces: a request that reached nobody, or one several services
    /// claimed.
    #[allow(clippy::too_many_arguments)]
    async fn call(
        &self,
        case: &str,
        leg: &str,
        method: &str,
        signing_name: &str,
        path: &str,
        content_type: Option<&str>,
        target: Option<&str>,
        body: Vec<u8>,
    ) -> Answer {
        let host = format!("{signing_name}.{REGION}.amazonaws.com");
        let mut request = ::http::Request::builder()
            .method(method)
            .uri(format!("https://{host}{path}"))
            .header("host", host)
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 \
                     Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
                     SignedHeaders=host, Signature=not-verified"
                ),
            );
        if let Some(content_type) = content_type {
            request = request.header("content-type", content_type);
        }
        if let Some(target) = target {
            request = request.header("x-amz-target", target);
        }
        let request = request
            .body(Bytes::from(body))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let headers: Vec<(String, String)> = response
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_ascii_lowercase(),
                    String::from_utf8_lossy(value.as_bytes()).into_owned(),
                )
            })
            .collect();
        let answered = Answer {
            status,
            headers,
            body: response.into_body(),
        };
        let error_type = answered.header("x-amzn-errortype").unwrap_or_default().to_string();
        let text = answered.text();
        for refusal in ROUTER_REFUSALS {
            assert!(
                !text.contains(refusal) && !error_type.contains(refusal),
                "case {case:?}: {leg} must be served by exactly one service, got HTTP {status}: \
                 {}",
                answered.summary()
            );
        }
        answered
    }

    /// One Route 53 `restXml` call, requiring HTTP `expected`, as its XML body.
    async fn route53(
        &self,
        case: &str,
        method: &str,
        path: &str,
        body: &str,
        expected: u16,
    ) -> String {
        let answered = self
            .call(
                case,
                &format!("Route 53 {method} {path}"),
                method,
                "route53",
                path,
                Some("application/xml"),
                None,
                body.as_bytes().to_vec(),
            )
            .await;
        assert_eq!(
            answered.status,
            expected,
            "case {case:?}: Route 53 {method} {path} expected HTTP {expected}, got {} ({})",
            answered.status,
            answered.summary()
        );
        answered.text()
    }

    /// One SESv2 `restJson1` call, requiring HTTP `expected`, as its decoded body.
    async fn sesv2(
        &self,
        case: &str,
        method: &str,
        path: &str,
        body: Option<Value>,
        expected: u16,
    ) -> Value {
        let payload = match &body {
            Some(value) => serde_json::to_vec(value).expect("the request body serializes"),
            None => Vec::new(),
        };
        let answered = self
            .call(
                case,
                &format!("SESv2 {method} {path}"),
                method,
                "ses",
                path,
                Some("application/json"),
                None,
                payload,
            )
            .await;
        assert_eq!(
            answered.status,
            expected,
            "case {case:?}: SESv2 {method} {path} expected HTTP {expected}, got {} ({})",
            answered.status,
            answered.summary()
        );
        serde_json::from_slice(&answered.body).unwrap_or(Value::Null)
    }

    /// One Amazon SES `awsQuery` call that must be served, as its XML body.
    async fn ses(&self, case: &str, action: &str, params: &[(String, String)]) -> String {
        let mut body = format!("Action={action}&Version={SES_VERSION}");
        for (name, value) in params {
            body.push('&');
            body.push_str(&form_encode(name));
            body.push('=');
            body.push_str(&form_encode(value));
        }
        let answered = self
            .call(
                case,
                &format!("Amazon SES {action}"),
                "POST",
                "ses",
                "/",
                Some("application/x-www-form-urlencoded"),
                None,
                body.into_bytes(),
            )
            .await;
        assert_eq!(
            answered.status,
            200,
            "case {case:?}: Amazon SES {action} must be served, got {} ({})",
            answered.status,
            answered.summary()
        );
        answered.text()
    }

    /// One Amazon S3 call in [`REGION`], path style.
    async fn s3(&self, case: &str, method: &str, path: &str, body: Vec<u8>) -> Answer {
        self.call(
            case,
            &format!("Amazon S3 {method} {path}"),
            method,
            "s3",
            path,
            None,
            None,
            body,
        )
        .await
    }

    /// One AWS KMS `awsJson1_1` call that must be served, as its decoded body.
    async fn kms(&self, case: &str, operation: &str, body: &Value) -> Value {
        let answered = self
            .call(
                case,
                &format!("AWS KMS {operation}"),
                "POST",
                "kms",
                "/",
                Some("application/x-amz-json-1.1"),
                Some(&format!("TrentService.{operation}")),
                serde_json::to_vec(body).expect("the request body serializes"),
            )
            .await;
        assert_eq!(
            answered.status,
            200,
            "case {case:?}: AWS KMS {operation} must be served, got {} ({})",
            answered.status,
            answered.summary()
        );
        serde_json::from_slice(&answered.body).unwrap_or(Value::Null)
    }
}

/// One form field of an Amazon SES query call.
fn field(name: impl Into<String>, value: impl Into<String>) -> (String, String) {
    (name.into(), value.into())
}

/// One hosted zone of the account: how it is created, and the tags it is left carrying.
struct ZoneRow {
    /// What this zone is, for the assertion messages.
    case: &'static str,
    /// The name the zone is created with.
    name: &'static str,
    /// The VPC a PRIVATE zone is created for; `None` for a public zone.
    vpc: Option<&'static str>,
    /// The tags one `ChangeTagsForResource` batch writes on it.
    tags: &'static [(&'static str, &'static str)],
}

/// The zones the account holds: the public one carrying the selector tag — whose apex is the
/// domain mail is received for — beside a PRIVATE zone tagged the same way and a public zone
/// nobody tagged, neither of which a tag-driven discovery may resolve to.
const ZONE_ROWS: &[ZoneRow] = &[
    ZoneRow {
        case: "a public zone nobody tagged",
        name: "untagged.first-zone.example.org",
        vpc: None,
        tags: &[],
    },
    ZoneRow {
        case: "the public zone carrying the selector tag",
        name: "Receiving-Apex.Example.COM",
        vpc: None,
        tags: &[
            (SELECTOR_KEY, SELECTOR_VALUE),
            ("Owner", "the mail deployment"),
        ],
    },
    ZoneRow {
        case: "a private zone tagged the same way",
        name: "internal.third-zone.example",
        vpc: Some("vpc-0123456789abcdef0"),
        tags: &[(SELECTOR_KEY, SELECTOR_VALUE), ("Scope", "vpc")],
    },
];

/// The apex of [`ZONE_ROWS`]'s selected zone, as a DNS name compares: the zone is created in
/// mixed case, and what a client resolves is the name the service canonicalised.
fn selected_apex() -> String {
    ZONE_ROWS
        .iter()
        .find(|row| row.tags.iter().any(|(key, _)| *key == SELECTOR_KEY) && row.vpc.is_none())
        .map(|row| row.name.trim_end_matches('.').to_ascii_lowercase())
        .expect("one public zone of the account carries the selector tag")
}

/// One enumerated hosted zone, as a client reads it off `ListHostedZones`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ListedZone {
    /// The zone id with the `/hostedzone/` prefix stripped — what a client keeps "after the
    /// last '/'", which is the form every `ResourceId` member takes.
    id: String,
    /// The zone's name, as the service canonicalised it.
    name: String,
    /// `Config.PrivateZone`, which the enumeration always reports.
    private: bool,
}

/// Create the zone `row` describes, write its tags, and answer its bare id.
async fn given_zone(emulator: &Emulator, row: &ZoneRow) -> String {
    let vpc = match row.vpc {
        Some(vpc) => format!("<VPC><VPCId>{vpc}</VPCId><VPCRegion>{REGION}</VPCRegion></VPC>"),
        None => String::new(),
    };
    let created = emulator
        .route53(
            row.case,
            "POST",
            "/2013-04-01/hostedzone",
            &format!(
                "<CreateHostedZoneRequest xmlns=\"https://route53.amazonaws.com/doc/2013-04-01/\">\
                 <Name>{}</Name>{vpc}<CallerReference>{}</CallerReference>\
                 <HostedZoneConfig><Comment>created by {}</Comment></HostedZoneConfig>\
                 </CreateHostedZoneRequest>",
                row.name, row.case, row.case
            ),
            201,
        )
        .await;
    let id = elements(&created, "Id")
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {:?}: the create must answer HostedZone.Id: {created}", row.case));
    let id = id.rsplit('/').next().unwrap_or_default().to_string();
    if !row.tags.is_empty() {
        let tags: String = row
            .tags
            .iter()
            .map(|(key, value)| format!("<Tag><Key>{key}</Key><Value>{value}</Value></Tag>"))
            .collect();
        emulator
            .route53(
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

/// Page `ListHostedZones` to the end at `page_size`, answering every zone it enumerated.
///
/// This IS the client-side procedure under test: follow `NextMarker` while `IsTruncated` is
/// true, exactly as an SDK paginator does.
async fn all_zones(emulator: &Emulator, case: &str, page_size: i32) -> Vec<ListedZone> {
    let mut seen: Vec<ListedZone> = Vec::new();
    let mut marker: Option<String> = None;
    loop {
        let mut query = format!("?maxitems={page_size}");
        if let Some(marker) = &marker {
            query.push_str(&format!("&marker={marker}"));
        }
        let page = emulator
            .route53(case, "GET", &format!("/2013-04-01/hostedzone{query}"), "", 200)
            .await;
        for zone in blocks_of(&page, "HostedZone") {
            let id = elements(zone, "Id").into_iter().next().unwrap_or_else(|| {
                panic!("case {case:?}: an enumerated zone must carry an Id: {zone}")
            });
            let private = elements(zone, "PrivateZone")
                .into_iter()
                .next()
                .unwrap_or_else(|| {
                    panic!(
                        "case {case:?}: an enumerated zone must report Config.PrivateZone: {zone}"
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
async fn zone_tags(emulator: &Emulator, case: &str, id: &str) -> BTreeMap<String, String> {
    let read = emulator
        .route53(
            case,
            "GET",
            &format!("/2013-04-01/tags/{HOSTED_ZONE}/{id}"),
            "",
            200,
        )
        .await;
    assert_eq!(
        elements(&read, "ResourceId").into_iter().next().as_deref(),
        Some(id),
        "case {case:?}: the tag set must name the zone it was read for, got {read}"
    );
    blocks_of(&read, "Tag")
        .into_iter()
        .filter_map(|tag| {
            let key = elements(tag, "Key").into_iter().next()?;
            let value = elements(tag, "Value").into_iter().next().unwrap_or_default();
            Some((key, value))
        })
        .collect()
}

/// The discovery under test: page the account's zones at `page_size`, keep the PUBLIC ones,
/// read each one's tags, and answer the apex of the one carrying the selector tag.
async fn discovered_receiving_domain(emulator: &Emulator, case: &str, page_size: i32) -> String {
    let mut selected: Vec<ListedZone> = Vec::new();
    for zone in all_zones(emulator, case, page_size).await {
        if zone.private {
            continue;
        }
        let tags = zone_tags(emulator, case, &zone.id).await;
        if tags.get(SELECTOR_KEY).map(String::as_str) == Some(SELECTOR_VALUE) {
            selected.push(zone);
        }
    }
    assert_eq!(
        selected.len(),
        1,
        "case {case:?}: exactly one PUBLIC zone of the account carries \
         {SELECTOR_KEY}={SELECTOR_VALUE} (the private zone carrying it is not one), got \
         {selected:?}"
    );
    selected[0].name.trim_end_matches('.').to_ascii_lowercase()
}

/// The apex of the discovered zone as an SESv2 email identity verified for sending: the
/// identity is created for it, and the verification the clock owes it is waited for.
async fn given_verified_identity(emulator: &Emulator, case: &str, apex: &str) -> Value {
    let created = emulator
        .sesv2(
            case,
            "POST",
            "/v2/email/identities",
            Some(json!({ "EmailIdentity": apex })),
            200,
        )
        .await;
    assert_eq!(
        created["IdentityType"],
        json!("DOMAIN"),
        "case {case:?}: the apex of a hosted zone is a DOMAIN identity, got {created}"
    );
    let deadline = Instant::now() + DEADLINE;
    loop {
        let read = emulator
            .sesv2(
                case,
                "GET",
                &format!("/v2/email/identities/{apex}"),
                None,
                200,
            )
            .await;
        if read["VerifiedForSendingStatus"] == json!(true) {
            return read;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the identity for {apex} must reach VerifiedForSendingStatus true, \
             last read {read}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// A bucket Amazon SES may write a received message into: created through Amazon S3's own
/// `CreateBucket`, carrying the inbound-mail bucket policy the Amazon SES Developer Guide
/// writes — the service principal granted `s3:PutObject`, scoped to this account.
async fn given_bucket(emulator: &Emulator, case: &str, bucket: &str) {
    let created = emulator.s3(case, "PUT", &format!("/{bucket}"), Vec::new()).await;
    assert_eq!(
        created.status,
        200,
        "case {case:?}: the CreateBucket of {bucket} must succeed, got {} ({})",
        created.status,
        created.summary()
    );
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "AllowSESPuts",
            "Effect": "Allow",
            "Principal": { "Service": "ses.amazonaws.com" },
            "Action": "s3:PutObject",
            "Resource": format!("arn:aws:s3:::{bucket}/*"),
            "Condition": { "StringEquals": { "AWS:SourceAccount": account() } },
        }],
    });
    let granted = emulator
        .s3(
            case,
            "PUT",
            &format!("/{bucket}?policy"),
            serde_json::to_vec(&policy).expect("the bucket policy serializes"),
        )
        .await;
    assert_eq!(
        granted.status,
        BUCKET_POLICY_STORED,
        "case {case:?}: the PutBucketPolicy of {bucket} must be stored, got {} ({})",
        granted.status,
        granted.summary()
    );
}

/// A customer managed key an `S3Action` may name: the account root keeps `kms:*` — "the key
/// policy must allow the calling principal to make a subsequent PutKeyPolicy request" — and the
/// Amazon SES service principal is granted the encrypt operations `S3Action$KmsKeyArn` asks
/// for. The ARN is the one KMS itself reports for the key.
async fn given_key(emulator: &Emulator, case: &str) -> String {
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [
            {
                "Sid": "Enable IAM User Permissions",
                "Effect": "Allow",
                "Principal": { "AWS": format!("arn:aws:iam::{}:root", account()) },
                "Action": "kms:*",
                "Resource": "*",
            },
            {
                "Sid": "AllowSESEncryption",
                "Effect": "Allow",
                "Principal": { "Service": "ses.amazonaws.com" },
                "Action": ["kms:Encrypt", "kms:GenerateDataKey*"],
                "Resource": "*",
            },
        ],
    });
    let created = emulator
        .kms(
            case,
            "CreateKey",
            &json!({ "Policy": policy.to_string(), "Description": "inbound mail" }),
        )
        .await;
    created["KeyMetadata"]["Arn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the created key must report its ARN: {created}"))
        .to_string()
}

/// The rule set `rule_set` holding one rule `rule`, made ACTIVE: the rule matches
/// `recipients` (none at all "matches all recipients on all verified domains"), scans when
/// `scan`, and saves what it receives under `prefix` of `bucket`, encrypted under
/// `kms_key_arn` when the row names one.
#[allow(clippy::too_many_arguments)]
async fn given_active_rule(
    emulator: &Emulator,
    case: &str,
    rule_set: &str,
    rule: &str,
    recipients: &[String],
    prefix: &str,
    scan: bool,
    bucket: &str,
    kms_key_arn: Option<&str>,
) {
    emulator
        .ses(case, "CreateReceiptRuleSet", &[field("RuleSetName", rule_set)])
        .await;
    let mut params = vec![
        field("RuleSetName", rule_set),
        field("Rule.Name", rule),
        field("Rule.Enabled", "true"),
        field("Rule.ScanEnabled", if scan { "true" } else { "false" }),
        field("Rule.Actions.member.1.S3Action.BucketName", bucket),
    ];
    // `S3Action$ObjectKeyPrefix` is optional ("the key prefix of the Amazon S3 bucket"), and a
    // rule stating none saves at the bucket's root — which is not the same request as one
    // carrying an empty prefix.
    if !prefix.is_empty() {
        params.push(field(
            "Rule.Actions.member.1.S3Action.ObjectKeyPrefix",
            prefix,
        ));
    }
    if let Some(kms_key_arn) = kms_key_arn {
        params.push(field("Rule.Actions.member.1.S3Action.KmsKeyArn", kms_key_arn));
    }
    for (index, recipient) in recipients.iter().enumerate() {
        params.push(field(
            format!("Rule.Recipients.member.{}", index + 1),
            recipient.clone(),
        ));
    }
    emulator.ses(case, "CreateReceiptRule", &params).await;
    emulator
        .ses(
            case,
            "SetActiveReceiptRuleSet",
            &[field("RuleSetName", rule_set)],
        )
        .await;
    // The set a received message is evaluated against, read back from the service itself: a row
    // asserting on an object proves nothing about a rule the account does not hold.
    let active = emulator
        .ses(case, "DescribeActiveReceiptRuleSet", &[])
        .await;
    assert!(
        active.contains(&format!("<Name>{rule}</Name>")),
        "case {case:?}: the active rule set must hold the rule {rule}, got {active}"
    );
}

/// `SendRawEmail` carrying `raw` for `recipients`, answering the `MessageId` it was accepted
/// under — the id the object a matching `S3Action` is keyed by.
///
/// `RawMessage$Data` is a blob, so the awsQuery wire carries it base64-encoded.
async fn send_raw(
    emulator: &Emulator,
    case: &str,
    source: &str,
    recipients: &[String],
    raw: &[u8],
) -> String {
    let mut params = vec![
        field("Source", source),
        field("RawMessage.Data", ::aws_smithy_types::base64::encode(raw)),
    ];
    for (index, recipient) in recipients.iter().enumerate() {
        params.push(field(
            format!("Destinations.member.{}", index + 1),
            recipient.clone(),
        ));
    }
    let answered = emulator.ses(case, "SendRawEmail", &params).await;
    elements(&answered, "MessageId")
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: the send must answer a MessageId: {answered}"))
}

/// The keys of `bucket` under `prefix`, in the order Amazon S3 lists them.
async fn keys(emulator: &Emulator, case: &str, bucket: &str, prefix: &str) -> Vec<String> {
    let listed = emulator
        .s3(
            case,
            "GET",
            &format!("/{bucket}?list-type=2&prefix={prefix}"),
            Vec::new(),
        )
        .await;
    assert_eq!(
        listed.status,
        200,
        "case {case:?}: listing {bucket}/{prefix} must succeed, got {} ({})",
        listed.status,
        listed.summary()
    );
    elements(&listed.text(), "Key")
}

/// The ONE key under `prefix` of `bucket`, waited for until it is there.
///
/// The wait is for the CONDITION — an object under that prefix — looked for again until
/// [`DEADLINE`] passes, because a message is received after it is accepted and nothing a sender
/// is told says when.
async fn settled_key(emulator: &Emulator, case: &str, bucket: &str, prefix: &str) -> String {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let found = keys(emulator, case, bucket, prefix).await;
        if let Some(key) = found.first() {
            assert_eq!(
                found.len(),
                1,
                "case {case:?}: exactly one object may be saved under {bucket}/{prefix}, got \
                 {found:?}"
            );
            return key.clone();
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: no object appeared under {bucket}/{prefix}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// The object `key` of `bucket`, read back through Amazon S3's own `GetObject`.
async fn stored_object(emulator: &Emulator, case: &str, bucket: &str, key: &str) -> Answer {
    let read = emulator
        .s3(case, "GET", &format!("/{bucket}/{key}"), Vec::new())
        .await;
    assert_eq!(
        read.status,
        200,
        "case {case:?}: reading {bucket}/{key} must succeed, got {} ({})",
        read.status,
        read.summary()
    );
    read
}

/// The header fields Amazon SES stamped on the received message, as a map keyed by the
/// lowercased field name.
///
/// Read off the object itself: the message's own bytes have to be its TAIL — an `S3Action`
/// saves "the received message", whose own bytes no action alters — so whatever precedes them
/// is what Amazon SES added, and every line of it has to be an RFC 5322 header field rather
/// than a rewriting of the message.
fn stamped_fields(case: &str, stored: &[u8], raw: &[u8]) -> BTreeMap<String, String> {
    assert!(
        stored.len() >= raw.len() && stored[stored.len() - raw.len()..] == *raw,
        "case {case:?}: the saved object must carry the message's own {} bytes unaltered, got \
         {} bytes {:?}",
        raw.len(),
        stored.len(),
        String::from_utf8_lossy(&stored[..stored.len().min(400)])
    );
    let added = String::from_utf8_lossy(&stored[..stored.len() - raw.len()]).into_owned();
    let mut fields: BTreeMap<String, String> = BTreeMap::new();
    let mut last: Option<String> = None;
    for line in added.split("\r\n").filter(|line| !line.is_empty()) {
        if line.starts_with(' ') || line.starts_with('\t') {
            let folded = last
                .clone()
                .unwrap_or_else(|| panic!("case {case:?}: a folded line opens no field: {added:?}"));
            let held = fields.entry(folded).or_default();
            held.push(' ');
            held.push_str(line.trim());
            continue;
        }
        let (name, value) = line.split_once(':').unwrap_or_else(|| {
            panic!(
                "case {case:?}: what Amazon SES added must be header fields, got the line \
                 {line:?}"
            )
        });
        let name = name.to_ascii_lowercase();
        fields.insert(name.clone(), value.trim().to_string());
        last = Some(name);
    }
    fields
}

/// A plain RFC 5322 message from `from` to `to`, carrying `body` — which is where a row puts a
/// scan test signature when it has one.
fn plain_message(from: &str, to: &str, subject: &str, body: &str) -> Vec<u8> {
    format!(
        "From: {from}\r\nTo: {to}\r\nSubject: {subject}\r\nMIME-Version: 1.0\r\n\
         Content-Type: text/plain; charset=UTF-8\r\n\r\n{body}\r\n"
    )
    .into_bytes()
}

/// A multipart message from `from` to `to` carrying a text part and a base64 attachment — the
/// MIME structure, the part media types, the `Content-Disposition` filename and the transfer
/// encoding a client submitted, which the object a rule saves has to carry unaltered.
fn multipart_message(from: &str, to: &str) -> Vec<u8> {
    let boundary = "a-boundary-the-sender-chose";
    format!(
        "From: {from}\r\nTo: {to}\r\nSubject: an attachment\r\nMIME-Version: 1.0\r\n\
         Content-Type: multipart/mixed; boundary=\"{boundary}\"\r\n\r\n\
         --{boundary}\r\nContent-Type: text/plain; charset=UTF-8\r\n\r\n\
         the covering note\r\n\
         --{boundary}\r\nContent-Type: application/octet-stream; name=\"payload.bin\"\r\n\
         Content-Transfer-Encoding: base64\r\n\
         Content-Disposition: attachment; filename=\"payload.bin\"\r\n\r\n\
         {}\r\n\
         --{boundary}--\r\n",
        ::aws_smithy_types::base64::encode([0_u8, 1, 2, 250, 251, 252, 253, 254, 255].as_slice())
    )
    .into_bytes()
}

/// One delivery row: what the account's active rule does, what is sent, and what the object it
/// saves has to be.
struct DeliveryRow {
    /// What this row is called in every assertion it makes.
    case: &'static str,
    /// The rule set and the rule the row creates and activates.
    rule_set: &'static str,
    rule: &'static str,
    /// The bucket the row's action saves into — one per row, so a row listing a bucket under
    /// the EMPTY prefix sees its own object and no other row's.
    bucket: &'static str,
    /// `ReceiptRule$Recipients`, empty for a rule stating no condition.
    recipients: Vec<String>,
    /// `S3Action$ObjectKeyPrefix`, which the object is placed under.
    prefix: &'static str,
    /// `ReceiptRule$ScanEnabled`.
    scan: bool,
    /// Whether the action names the account's KMS key.
    encrypt: bool,
    /// The envelope recipients the message is sent to.
    destinations: Vec<String>,
    /// The message as the sender submits it.
    raw: Vec<u8>,
    /// The verdicts a scanning rule has to report for this message.
    spam: &'static str,
    virus: &'static str,
}

/// The rows, written against the apex the discovery resolved — so the suite sends to the
/// domain it FOUND rather than to one it named.
fn delivery_rows(apex: &str) -> Vec<DeliveryRow> {
    let sender = format!("postmaster@{apex}");
    let recipient = format!("inbox@{apex}");
    let other = format!("second@{apex}");
    vec![
        DeliveryRow {
            case: "a clean plain message, a domain condition, a nested prefix, scanned",
            rule_set: "nested-prefix",
            rule: "save-under-a-nested-prefix",
            bucket: "inbound-mail-nested-prefix",
            recipients: vec![apex.to_string()],
            prefix: "inbound/mail/",
            scan: true,
            encrypt: false,
            destinations: vec![recipient.clone()],
            raw: plain_message(&sender, &recipient, "a clean message", "nothing to report"),
            spam: VERDICT_PASS,
            virus: VERDICT_PASS,
        },
        DeliveryRow {
            case: "a multipart message with an attachment, one address, no prefix, unscanned",
            rule_set: "byte-exact",
            rule: "save-at-the-bucket-root",
            bucket: "inbound-mail-at-the-root",
            recipients: vec![other.clone()],
            prefix: "",
            scan: false,
            encrypt: false,
            destinations: vec![other.clone()],
            raw: multipart_message(&sender, &other),
            spam: VERDICT_PASS,
            virus: VERDICT_PASS,
        },
        DeliveryRow {
            case: "a message carrying the published spam test pattern, no condition, scanned",
            rule_set: "spam-scanned",
            rule: "save-every-recipient",
            bucket: "inbound-mail-spam-scanned",
            recipients: Vec::new(),
            prefix: "scanned/",
            scan: true,
            encrypt: false,
            destinations: vec![recipient.clone()],
            raw: plain_message(
                &sender,
                &recipient,
                "a bulk-mail test",
                &format!("the pattern follows\r\n{SPAM_TEST_PATTERN}"),
            ),
            spam: VERDICT_FAIL,
            virus: VERDICT_PASS,
        },
        DeliveryRow {
            case: "a message carrying the published virus test signature, scanned",
            rule_set: "virus-scanned",
            rule: "save-and-report",
            bucket: "inbound-mail-virus-scanned",
            recipients: vec![apex.to_string()],
            prefix: "scanned/viruses/",
            scan: true,
            encrypt: false,
            destinations: vec![recipient.clone()],
            raw: plain_message(
                &sender,
                &recipient,
                "an anti-virus test",
                &format!("the signature follows\r\n{VIRUS_TEST_SIGNATURE}"),
            ),
            spam: VERDICT_PASS,
            virus: VERDICT_FAIL,
        },
        DeliveryRow {
            case: "a message saved under a KMS key, two envelope recipients, scanned",
            rule_set: "encrypted",
            rule: "seal-and-save",
            bucket: "inbound-mail-sealed",
            recipients: vec![apex.to_string()],
            prefix: "sealed/",
            scan: true,
            encrypt: true,
            destinations: vec![recipient.clone(), other.clone()],
            raw: plain_message(&sender, &recipient, "a sealed message", "nothing to report"),
            spam: VERDICT_PASS,
            virus: VERDICT_PASS,
        },
    ]
}

/// The account as a deployment leaves it: its zones, and the apex of the tagged public one
/// verified for sending.
async fn given_account(emulator: &Emulator, case: &str) -> String {
    for row in ZONE_ROWS {
        given_zone(emulator, row).await;
    }
    let apex = discovered_receiving_domain(emulator, case, 1).await;
    given_verified_identity(emulator, case, &apex).await;
    apex
}

/// A zone the account created is enumerated as it was created, its tags are projected back,
/// and the apex of the one a selector tag picks out is a domain this account is verified to
/// send from — whatever page size the enumeration is paged at.
#[tokio::test]
async fn the_tagged_public_zones_apex_is_a_domain_verified_for_sending() {
    let emulator = Emulator::new();
    let case = "the account's own zones";
    let mut ids = Vec::new();
    for row in ZONE_ROWS {
        ids.push(given_zone(&emulator, row).await);
    }

    for &page_size in PAGE_SIZES {
        let case = format!("paged {page_size} zones at a time");
        let listed = all_zones(&emulator, &case, page_size).await;
        assert_eq!(
            listed.len(),
            ZONE_ROWS.len(),
            "case {case:?}: the account holds {} zones and the enumeration carried {}: {listed:?}",
            ZONE_ROWS.len(),
            listed.len()
        );
        let mut unique: Vec<&String> = listed.iter().map(|zone| &zone.id).collect();
        unique.sort();
        unique.dedup();
        assert_eq!(
            unique.len(),
            listed.len(),
            "case {case:?}: no zone may be enumerated twice, got {listed:?}"
        );
        for (row, id) in ZONE_ROWS.iter().zip(&ids) {
            let found = listed
                .iter()
                .find(|zone| &zone.id == id)
                .unwrap_or_else(|| {
                    panic!(
                        "case {case:?}: the zone {id} created for {:?} must be enumerated, got \
                         {listed:?}",
                        row.case
                    )
                });
            assert_eq!(
                found.private,
                row.vpc.is_some(),
                "case {case:?}: zone {id} ({:?}) must report PrivateZone {}, got {found:?}",
                row.case,
                row.vpc.is_some()
            );
            assert_eq!(
                found.name.trim_end_matches('.').to_ascii_lowercase(),
                row.name.trim_end_matches('.').to_ascii_lowercase(),
                "case {case:?}: zone {id} ({:?}) must be enumerated under the name it was \
                 created with, got {found:?}",
                row.case
            );
            let reported = zone_tags(&emulator, &case, id).await;
            let written: BTreeMap<String, String> = row
                .tags
                .iter()
                .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
                .collect();
            assert_eq!(
                reported, written,
                "case {case:?}: zone {id} ({:?}) must report the tags written on it, got \
                 {reported:?}",
                row.case
            );
        }
        let apex = discovered_receiving_domain(&emulator, &case, page_size).await;
        assert_eq!(
            apex,
            selected_apex(),
            "case {case:?}: the selector must resolve to the apex of the tagged PUBLIC zone, \
             got {apex:?}"
        );
    }

    // And that apex is a domain this account may send from: the identity reports itself
    // verified for sending, with the Easy DKIM setup whose tokens it handed out.
    let read = given_verified_identity(&emulator, case, &selected_apex()).await;
    assert_eq!(
        read["DkimAttributes"]["Status"],
        json!("SUCCESS"),
        "case {case:?}: a verified domain identity reports its DKIM set up, got {read}"
    );
}

/// Mail Amazon SES accepts for a recipient of the discovered domain is RECEIVED by the account:
/// the active rule's `S3Action` saves it as one object under the action's prefix, carrying the
/// message's own bytes behind the fields a scan stamped, sealed when the action names a key.
#[tokio::test]
async fn mail_for_the_discovered_domain_is_received_into_s3_by_the_active_rule() {
    let emulator = Emulator::new();
    let apex = given_account(&emulator, "the receiving account").await;
    let key_arn = given_key(&emulator, "the receiving account").await;
    let sender = format!("postmaster@{apex}");

    for row in delivery_rows(&apex) {
        let case = row.case;
        given_bucket(&emulator, case, row.bucket).await;
        given_active_rule(
            &emulator,
            case,
            row.rule_set,
            row.rule,
            &row.recipients,
            row.prefix,
            row.scan,
            row.bucket,
            row.encrypt.then_some(key_arn.as_str()),
        )
        .await;
        let message_id = send_raw(&emulator, case, &sender, &row.destinations, &row.raw).await;
        let key = settled_key(&emulator, case, row.bucket, row.prefix).await;
        assert_eq!(
            key,
            format!("{}{message_id}", row.prefix),
            "case {case:?}: the saved object is keyed by the action's prefix and the id the send \
             was accepted under, got {key:?}"
        );
        let stored = stored_object(&emulator, case, row.bucket, &key).await;
        let body = stored.body.to_vec();

        if row.encrypt {
            // "Your mail is encrypted by Amazon SES using the Amazon S3 encryption client
            // before the mail is submitted to Amazon S3 for storage. It is not encrypted using
            // Amazon S3 server-side encryption" (`S3Action$KmsKeyArn`): the body is ciphertext
            // and the user metadata is the envelope a reader opens it from.
            for entry in [
                WRAPPED_KEY_METADATA,
                IV_METADATA,
                CONTENT_ALGORITHM_METADATA,
                WRAP_ALGORITHM_METADATA,
                TAG_LENGTH_METADATA,
                MATERIAL_DESCRIPTION_METADATA,
                PLAINTEXT_LENGTH_METADATA,
            ] {
                assert!(
                    stored.header(entry).is_some_and(|value| !value.is_empty()),
                    "case {case:?}: the saved object must carry the envelope entry {entry}, got \
                     {:?}",
                    stored.headers
                );
            }
            let material: Value = serde_json::from_str(
                stored.header(MATERIAL_DESCRIPTION_METADATA).unwrap_or_default(),
            )
            .unwrap_or_else(|error| {
                panic!("case {case:?}: the material description must be a document: {error}")
            });
            assert_eq!(
                material["kms_cmk_id"].as_str(),
                Some(key_arn.as_str()),
                "case {case:?}: the material must name the key the action carried, got {material}"
            );
            assert_eq!(
                material["aws:ses:rule-name"].as_str(),
                Some(row.rule),
                "case {case:?}: the material must name the rule that saved it, got {material}"
            );
            assert_eq!(
                material["aws:ses:message-id"].as_str(),
                Some(message_id.as_str()),
                "case {case:?}: the material must name the message it sealed, got {material}"
            );
            // The `Subject` the sender wrote: text that is IN the message, so a body carrying it
            // is the message in the clear rather than the ciphertext the action asked for.
            let written = String::from_utf8_lossy(&row.raw).into_owned();
            let subject = written
                .lines()
                .find(|line| line.starts_with("Subject: "))
                .unwrap_or_else(|| panic!("case {case:?}: the row's message carries a Subject"));
            assert!(
                !String::from_utf8_lossy(&body).contains(subject),
                "case {case:?}: the saved body must be ciphertext, got {:?}",
                String::from_utf8_lossy(&body)
            );
            let declared: usize = stored
                .header(PLAINTEXT_LENGTH_METADATA)
                .unwrap_or_default()
                .parse()
                .unwrap_or_else(|error| {
                    panic!("case {case:?}: the plaintext length must be a number: {error}")
                });
            assert_eq!(
                body.len(),
                declared + TAG_BYTES,
                "case {case:?}: the ciphertext is the sealed message plus its authentication \
                 tag, got {} bytes for a declared {declared}",
                body.len()
            );
            assert!(
                declared > row.raw.len(),
                "case {case:?}: a scanning rule stamps its results on the message it seals, so \
                 the sealed message is longer than the {} bytes sent, got {declared}",
                row.raw.len()
            );
            // The envelope really opens: KMS's own `Decrypt` unwraps the data key under the
            // material description the object carries, and answers a key of the length the
            // content algorithm takes.
            let context: BTreeMap<String, String> = material
                .as_object()
                .unwrap_or_else(|| panic!("case {case:?}: the material is an object: {material}"))
                .iter()
                .map(|(name, value)| {
                    (name.clone(), value.as_str().unwrap_or_default().to_string())
                })
                .collect();
            let unwrapped = emulator
                .kms(
                    case,
                    "Decrypt",
                    &json!({
                        "CiphertextBlob": stored.header(WRAPPED_KEY_METADATA).unwrap_or_default(),
                        "EncryptionContext": context,
                    }),
                )
                .await;
            let data_key = ::aws_smithy_types::base64::decode(
                unwrapped["Plaintext"].as_str().unwrap_or_default(),
            )
            .unwrap_or_else(|error| {
                panic!("case {case:?}: the unwrapped data key is base64: {error}")
            });
            assert_eq!(
                data_key.len(),
                32,
                "case {case:?}: the sealed data key is the AES-256 key the declared content \
                 algorithm takes, got {} bytes",
                data_key.len()
            );
            continue;
        }

        // An action naming no key saves the message itself: its own bytes, behind the header
        // fields Amazon SES added to it.
        let fields = stamped_fields(case, &body, &row.raw);
        if row.scan {
            let results = fields.get(AUTHENTICATION_RESULTS_HEADER).unwrap_or_else(|| {
                panic!("case {case:?}: a scanning rule must stamp its results, got {fields:?}")
            });
            assert!(
                results.contains("spf=pass"),
                "case {case:?}: the results must report the SPF check, got {results:?}"
            );
            assert!(
                results.contains("dkim=pass"),
                "case {case:?}: a message Amazon SES signed for this identity must report its \
                 DKIM check, got {results:?}"
            );
            assert!(
                fields
                    .get(RECEIVED_SPF_HEADER)
                    .is_some_and(|spf| spf.starts_with("pass")),
                "case {case:?}: the scan must stamp the Received-SPF of the relayed message, got \
                 {fields:?}"
            );
            assert_eq!(
                fields.get(SPAM_VERDICT_HEADER).map(String::as_str),
                Some(row.spam),
                "case {case:?}: the spam verdict must be the one this message's content earns, \
                 got {fields:?}"
            );
            assert_eq!(
                fields.get(VIRUS_VERDICT_HEADER).map(String::as_str),
                Some(row.virus),
                "case {case:?}: the virus verdict must be the one this message's content earns, \
                 got {fields:?}"
            );
        } else {
            for header in [
                RECEIVED_SPF_HEADER,
                AUTHENTICATION_RESULTS_HEADER,
                SPAM_VERDICT_HEADER,
                VIRUS_VERDICT_HEADER,
            ] {
                assert!(
                    !fields.contains_key(header),
                    "case {case:?}: a rule that does not scan has the message saved as it \
                     arrived, so no {header} is stamped, got {fields:?}"
                );
            }
        }
    }
}
