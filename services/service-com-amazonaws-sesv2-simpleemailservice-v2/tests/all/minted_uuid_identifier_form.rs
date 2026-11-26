// SPDX-License-Identifier: CC0-1.0

//! The FORM of every identifier this service MINTS as a UUID, end to end over HTTP.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` with this
//! service registered — so routing, deserialization, the handler and serialization are
//! all exercised, and the id asserted on is the one the response carried.
//!
//! The rule these cases pin: **an identifier this service hands back as a UUID is a
//! well-formed UUID** — the text form the `uuid` crate parses and renders, carrying a
//! UUID's version and variant, and not merely thirty-two hexadecimal digits in dashed
//! 8-4-4-4-12 groups. A client that validates or round-trips such an id through a UUID
//! type rejects a value whose groups are the right length but whose version or variant
//! bits say it is no UUID, and these ids travel on into the ARNs and the paths that
//! address the resource.
//!
//! Two identifiers of this API are minted that way, and neither carries a modelled
//! pattern to take the form from (`smithy_query(query="shape",shape="JobId")` reports a
//! `length: min 1` string): an export job's `JobId` and an import job's `JobId` — the two
//! `CreateExportJob` and `CreateImportJob` answer with. Each is checked by the same
//! crate that renders one (`service_common::uuid_from_random_bytes`), so what counts as
//! well formed is that crate's rule rather than a spelling re-derived here.
//!
//! A multi-region endpoint's `EndpointId` is NOT one of them: SES hands that id back as a
//! pair of dot-separated alphanumeric labels, and
//! `tests/all/multi_region_endpoint_identifier_form.rs` pins that form.
//!
//! Beside the form, the two guarantees a client depends on: the id is UNIQUE across the
//! resources of its kind — rows mint several in one breath, which is where ids built from
//! the clock alone collide — and STABLE, so the read of the resource reports exactly the
//! id its create answered with.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_sesv2_simpleemailservice_v2::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;
use uuid::{Uuid, Variant, Version};

/// The Region the cases execute in: the one their `Authorization` credential scope
/// names.
const HOME: &str = "us-east-1";

/// How many resources each row mints, back to back: enough that two ids minted inside
/// one clock tick would have to collide.
const MINTS_PER_ROW: usize = 4;

/// The start of the date range an export job's data source covers, as whole epoch
/// seconds.
const RANGE_START_SECS: i64 = 1_761_580_800;

/// The end of that range: a day later.
const RANGE_END_SECS: i64 = RANGE_START_SECS + 24 * 60 * 60;

/// One answered request: the status, the error shape when there was one, and the parsed
/// body.
struct Reply {
    status: u16,
    error_type: Option<String>,
    body: Value,
}

impl Reply {
    /// The modelled error code the response names, or an empty string on success.
    ///
    /// restJson1 resolves the error shape from `x-amzn-errortype`; the JSON body's
    /// `__type` carries the same name, so either is read.
    fn error_code(&self) -> String {
        let raw = self
            .error_type
            .as_deref()
            .or_else(|| self.body.get("__type").and_then(Value::as_str))
            .unwrap_or_default();
        let name = raw.split('#').next_back().unwrap_or(raw);
        name.split(':').next().unwrap_or(name).to_string()
    }

    /// A string member of the body, or `""` when absent.
    fn text(&self, member: &str) -> String {
        self.body
            .get(member)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }
}

/// The emulated service behind its router.
struct Ses {
    router: MultiServiceRouter,
}

impl Ses {
    fn new() -> Self {
        let engine = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // These cases exercise handler behaviour, not SigV4.
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(engine));
        Self { router }
    }

    /// Send one request, signed for [`HOME`].
    async fn call(&self, method: &str, path: &str, body: Option<Value>) -> Reply {
        let payload = match &body {
            Some(value) => bytes::Bytes::from(serde_json::to_vec(value).expect("serialize body")),
            None => bytes::Bytes::new(),
        };
        let host = format!("email.{HOME}.amazonaws.com");
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://{host}{path}"))
            .header("host", host)
            .header("content-type", "application/json")
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIAEMULATOR0000000/20260101/{HOME}/ses/aws4_request, SignedHeaders=host, Signature=00"
                ),
            )
            .body(payload)
            .expect("build request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let error_type = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let bytes = response.into_body();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        Reply {
            status,
            error_type,
            body,
        }
    }
}

/// One identifier this service mints as a UUID: the create that mints it, the member it
/// is answered in, and the read that must report it back.
struct MintedIdCase {
    case: &'static str,
    /// The route the create is sent to.
    create_path: &'static str,
    /// The create request's body for the `index`-th resource of the row.
    body: fn(usize) -> Value,
    /// The output member carrying the minted id, in the create and in the read alike.
    member: &'static str,
    /// The route that reads the `index`-th resource back, addressed the way the resource
    /// is addressed: by its own name, or by the minted id itself.
    read_path: fn(usize, &str) -> String,
}

/// The identifiers of this API that are minted as UUIDs — one row each for the
/// export-job plane and the import-job plane, so the rule is asserted at every minter
/// rather than at the one plane.
const MINTED_ID_CASES: &[MintedIdCase] = &[
    MintedIdCase {
        case: "CreateExportJob answers a JobId",
        create_path: "/v2/email/export-jobs",
        body: |_index| {
            json!({
                "ExportDataSource": {
                    "MessageInsightsDataSource": {
                        "StartDate": RANGE_START_SECS,
                        "EndDate": RANGE_END_SECS,
                    }
                },
                "ExportDestination": {"DataFormat": "CSV"},
            })
        },
        member: "JobId",
        read_path: |_index, id| format!("/v2/email/export-jobs/{id}"),
    },
    MintedIdCase {
        case: "CreateImportJob answers a JobId",
        create_path: "/v2/email/import-jobs",
        body: |index| {
            json!({
                "ImportDestination": {
                    "SuppressionListDestination": {"SuppressionListImportAction": "PUT"},
                },
                "ImportDataSource": {
                    "S3Url": format!("s3://ses-imports/minted-{index}.csv"),
                    "DataFormat": "CSV",
                },
            })
        },
        member: "JobId",
        read_path: |_index, id| format!("/v2/email/import-jobs/{id}"),
    },
];

/// Assert `id` is a well-formed UUID, as the crate that renders one reads it.
///
/// `Uuid::parse_str` accepts the simple, braced and URN spellings as well, so the parsed
/// value's own hyphenated rendering is compared back against the id: that pins the text
/// form a client receives, not only that the bytes could be read.
fn assert_uuid_form(case: &str, id: &str) {
    let parsed = Uuid::parse_str(id)
        .unwrap_or_else(|error| panic!("case {case:?}: expected a UUID, got {id:?} ({error})"));
    assert_eq!(
        parsed.hyphenated().to_string(),
        id,
        "case {case:?}: a minted id is spelled as the hyphenated, lower-case UUID text form, \
         got {id:?}"
    );
    assert_eq!(
        parsed.get_version(),
        Some(Version::Random),
        "case {case:?}: expected the version of a UUID minted from opaque bytes, got {id:?}"
    );
    assert_eq!(
        parsed.get_variant(),
        Variant::RFC4122,
        "case {case:?}: expected a UUID's variant, got {id:?}"
    );
}

#[tokio::test]
async fn every_minted_identifier_is_a_well_formed_uuid() {
    for row in MINTED_ID_CASES {
        let case = row.case;
        let ses = Ses::new();
        let mut minted: Vec<String> = Vec::new();

        for index in 0..MINTS_PER_ROW {
            let created = ses
                .call("POST", row.create_path, Some((row.body)(index)))
                .await;
            assert_eq!(
                created.status,
                200,
                "case {case:?}: resource {index} must be created, got {} {} {:?}",
                created.status,
                created.error_code(),
                created.body
            );

            let id = created.text(row.member);
            assert_uuid_form(&format!("{case}, resource {index}"), &id);
            assert!(
                !minted.contains(&id),
                "case {case:?}: resource {index} repeated the id {id:?}"
            );

            // And the id is the resource's: the read reports exactly what the create
            // answered, so a client can address the resource by it.
            let read = ses.call("GET", &(row.read_path)(index, &id), None).await;
            assert_eq!(
                read.status,
                200,
                "case {case:?}: resource {index} must read back, got {} {} {:?}",
                read.status,
                read.error_code(),
                read.body
            );
            assert_eq!(
                read.text(row.member),
                id,
                "case {case:?}: the read of resource {index} must report the minted id, got {:?}",
                read.body
            );

            minted.push(id);
        }

        assert_eq!(
            minted.len(),
            MINTS_PER_ROW,
            "case {case:?}: expected {MINTS_PER_ROW} minted ids, got {minted:?}"
        );
    }
}
