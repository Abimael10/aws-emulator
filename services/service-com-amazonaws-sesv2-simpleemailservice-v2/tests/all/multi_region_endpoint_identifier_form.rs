// SPDX-License-Identifier: CC0-1.0

//! The TEXT FORM a multi-region endpoint's `EndpointId` is handed back in, end to end
//! over HTTP.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` with this
//! service registered — so routing, deserialization, the handler and serialization are
//! all exercised, and the identifier asserted on is the one the response carried.
//!
//! The rule these cases pin: **a multi-region endpoint's `EndpointId` is a PAIR OF
//! DOT-SEPARATED ALPHANUMERIC LABELS** — `<label>.<label>`, lowercase letters and digits,
//! exactly one dot, each label within what a DNS label admits — and not a dashed
//! hexadecimal UUID, a bare run of hexadecimal characters or a prefixed token. The model
//! fixes no form (`smithy_query(query="shape",shape="EndpointId")` reports a plain string
//! with no `@pattern` and no `@length`, documented only as "The ID of the multi-region
//! endpoint (global-endpoint)"), so the form is the one the service hands back, which is
//! what a client that validates the id — or that places it where the endpoint it names is
//! addressed — reads.
//!
//! Every read path reports the id off the stored record, so the same form is asserted
//! where a client meets it: on the create, on `GetMultiRegionEndpoint` and on
//! `ListMultiRegionEndpoints`.
//!
//! The identity those ids carry — minted per endpoint, never caller input, stable for the
//! endpoint's life — is pinned by `tests/all/multi_region_endpoint_identifier.rs`; the
//! cases here are about the form that identity is written in.
//!
//! The table below spans what the rule generalises over: the endpoint name's length and
//! character set, the secondary region the route details name, how many tags the create
//! carries, and the Region the call executes in.

use std::collections::BTreeSet;
use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_sesv2_simpleemailservice_v2::state::MAX_TAGS_PER_RESOURCE;
use service_com_amazonaws_sesv2_simpleemailservice_v2::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The longest a single DNS label may be, which is the bound each label of the id stays
/// inside so the whole value remains usable as a host label.
const MAX_LABEL_LENGTH: usize = 63;

/// One answered request: the status, the error shape when there was one, and the parsed
/// body.
struct Reply {
    status: u16,
    error_type: Option<String>,
    body: Value,
}

impl Reply {
    /// The modelled error code the response names, or an empty string on success.
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

    /// Send one request, signed for `region` — the credential scope is what tells the
    /// handler which AWS-Region the operation executes in.
    async fn call(&self, region: &str, method: &str, path: &str, body: Option<Value>) -> Reply {
        let payload = match &body {
            Some(value) => bytes::Bytes::from(serde_json::to_vec(value).expect("serialize body")),
            None => bytes::Bytes::new(),
        };
        let host = format!("email.{region}.amazonaws.com");
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://{host}{path}"))
            .header("host", host)
            .header("content-type", "application/json")
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIAEMULATOR0000000/20260101/{region}/ses/aws4_request, SignedHeaders=host, Signature=00"
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

/// `count` tags, as a create request carries them.
fn tag_list(count: usize) -> Vec<Value> {
    (0..count)
        .map(|index| json!({"Key": format!("key{index}"), "Value": format!("value{index}")}))
        .collect()
}

/// The `EndpointId` a listing reports for the endpoint named `name`, or `None` when the
/// page carries no entry for it.
fn listed_id(reply: &Reply, name: &str) -> Option<String> {
    reply
        .body
        .get("MultiRegionEndpoints")?
        .as_array()?
        .iter()
        .find(|row| row.get("EndpointName").and_then(Value::as_str) == Some(name))
        .and_then(|row| row.get("EndpointId").and_then(Value::as_str))
        .map(str::to_string)
}

/// Assert `id` is written in the form SES hands a multi-region endpoint's id back in: two
/// dot-separated labels of lowercase alphanumeric text.
///
/// `where_seen` names the response the id was read from, so a failure says which of the
/// create, the read and the listing diverged.
fn assert_label_pair_form(case: &str, where_seen: &str, id: &str) {
    let labels: Vec<&str> = id.split('.').collect();
    assert_eq!(
        labels.len(),
        2,
        "case {case:?} ({where_seen}): the EndpointId is two dot-separated labels, got {id:?}"
    );
    for (position, label) in labels.iter().enumerate() {
        assert!(
            !label.is_empty(),
            "case {case:?} ({where_seen}): label {position} of the EndpointId is empty, got {id:?}"
        );
        assert!(
            label.len() <= MAX_LABEL_LENGTH,
            "case {case:?} ({where_seen}): label {position} of the EndpointId is longer than the \
             {MAX_LABEL_LENGTH} characters a DNS label admits, got {id:?}"
        );
        assert!(
            label
                .chars()
                .all(|character| character.is_ascii_lowercase() || character.is_ascii_digit()),
            "case {case:?} ({where_seen}): label {position} of the EndpointId carries characters \
             outside the lowercase alphanumerics, got {id:?}"
        );
    }
    // The forms this one is NOT: the dashed hexadecimal UUID the store writes its job ids
    // in, and a prefixed token.
    assert!(
        !id.contains('-') && !id.contains('_'),
        "case {case:?} ({where_seen}): the EndpointId carries no dash and no underscore, \
         got {id:?}"
    );
}

/// One created endpoint: the request that creates it, spanning what the rule generalises
/// over.
struct Case {
    case: &'static str,
    /// `EndpointName`: within the modelled `@length 1..64` and `@pattern ^[\w\-_]+$`.
    name: &'static str,
    /// The Region the create executes in, which becomes the endpoint's primary region.
    primary: &'static str,
    /// The Region the one route configuration names.
    secondary: &'static str,
    /// How many tags the create carries.
    tags: usize,
}

/// The rows span the name's length and character set (the shortest and the longest name
/// the model admits, word characters, digits, hyphens and underscores), the secondary
/// region, the tag count (none, one, and the documented maximum of
/// [`MAX_TAGS_PER_RESOURCE`]) and the Region the call executes in.
fn cases() -> Vec<Case> {
    vec![
        Case {
            case: "the shortest name the model admits, no tags",
            name: "e",
            primary: "us-east-1",
            secondary: "us-west-2",
            tags: 0,
        },
        Case {
            case: "the longest name the model admits, one tag",
            // 64 characters: `EndpointName`'s @length maximum.
            name: "endpoint_name_of_the_full_sixty_four_characters_the_model_admits",
            primary: "us-east-1",
            secondary: "eu-west-1",
            tags: 1,
        },
        Case {
            case: "hyphens and underscores, as many tags as a resource may carry",
            name: "global-endpoint_ID-2",
            primary: "eu-west-1",
            secondary: "us-east-1",
            tags: MAX_TAGS_PER_RESOURCE,
        },
        Case {
            case: "digits only, created in a third Region",
            name: "20261125",
            primary: "ap-south-1",
            secondary: "eu-central-1",
            tags: 2,
        },
    ]
}

#[tokio::test]
async fn every_minted_endpoint_identifier_is_a_pair_of_dot_separated_labels() {
    let ses = Ses::new();
    // Every id this suite has seen: the form carries an identity, so no two endpoints may
    // be handed one value.
    let mut minted: BTreeSet<String> = BTreeSet::new();

    for row in cases() {
        let case = row.case;
        let created = ses
            .call(
                row.primary,
                "POST",
                "/v2/email/multi-region-endpoints",
                Some(json!({
                    "EndpointName": row.name,
                    "Details": {"RoutesDetails": [{"Region": row.secondary}]},
                    "Tags": tag_list(row.tags),
                })),
            )
            .await;
        assert_eq!(
            created.status,
            200,
            "case {case:?}: the create must succeed, got {} {} {:?}",
            created.status,
            created.error_code(),
            created.body
        );

        let id = created.text("EndpointId");
        assert_label_pair_form(case, "the create response", &id);
        assert!(
            minted.insert(id.clone()),
            "case {case:?}: the EndpointId {id:?} was already minted for another endpoint"
        );

        // Every read reports the id off the record, so the form a client meets on the
        // read and in the listing is the same one.
        let read = ses
            .call(
                row.primary,
                "GET",
                &format!("/v2/email/multi-region-endpoints/{}", row.name),
                None,
            )
            .await;
        assert_eq!(
            read.status,
            200,
            "case {case:?}: the endpoint must read back, got {} {} {:?}",
            read.status,
            read.error_code(),
            read.body
        );
        let read_id = read.text("EndpointId");
        assert_label_pair_form(case, "the GetMultiRegionEndpoint response", &read_id);
        assert_eq!(
            read_id, id,
            "case {case:?}: the read must report the id the create minted, got {:?}",
            read.body
        );

        let listing = ses
            .call(row.primary, "GET", "/v2/email/multi-region-endpoints", None)
            .await;
        assert_eq!(
            listing.status,
            200,
            "case {case:?}: the listing must succeed, got {} {} {:?}",
            listing.status,
            listing.error_code(),
            listing.body
        );
        let listed = listed_id(&listing, row.name).unwrap_or_default();
        assert_label_pair_form(case, "the ListMultiRegionEndpoints response", &listed);
        assert_eq!(
            listed, id,
            "case {case:?}: the listing must report the id the create minted, got {:?}",
            listing.body
        );
    }

    assert_eq!(
        minted.len(),
        cases().len(),
        "every created endpoint must hold an id of its own, got {minted:?}"
    );
}

#[tokio::test]
async fn endpoint_identifiers_minted_in_one_breath_differ_in_both_labels() {
    let ses = Ses::new();
    // Several endpoints created back to back, which is where an id built from the clock
    // alone repeats itself, and where a form whose second label were a constant would
    // show it.
    let mut first_labels: BTreeSet<String> = BTreeSet::new();
    let mut second_labels: BTreeSet<String> = BTreeSet::new();
    let endpoints = 6;

    for index in 0..endpoints {
        let case = format!("endpoint {index} of a run of {endpoints}");
        let created = ses
            .call(
                "us-east-1",
                "POST",
                "/v2/email/multi-region-endpoints",
                Some(json!({
                    "EndpointName": format!("endpoint_{index}"),
                    "Details": {"RoutesDetails": [{"Region": "us-west-2"}]},
                })),
            )
            .await;
        assert_eq!(
            created.status,
            200,
            "case {case:?}: the create must succeed, got {} {} {:?}",
            created.status,
            created.error_code(),
            created.body
        );
        let id = created.text("EndpointId");
        assert_label_pair_form(&case, "the create response", &id);
        let (first, second) = id
            .split_once('.')
            .unwrap_or_else(|| panic!("case {case:?}: expected two labels, got {id:?}"));
        first_labels.insert(first.to_string());
        second_labels.insert(second.to_string());
    }

    assert_eq!(
        first_labels.len(),
        endpoints,
        "each endpoint's id must carry a first label of its own, got {first_labels:?}"
    );
    assert_eq!(
        second_labels.len(),
        endpoints,
        "each endpoint's id must carry a second label of its own — a constant second label \
         would name every endpoint alike, got {second_labels:?}"
    );
}
