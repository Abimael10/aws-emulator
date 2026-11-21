// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the FORM of every queue url this service reports, and for what a
//! value supplied where a `QueueUrl` is expected addresses — driven over the wire (`POST /`
//! with `X-Amz-Target: AmazonSQS.<OperationName>`) through the shared harness in
//! [`super::support`].
//!
//! The rule under test: **a queue url this service reports is an address on the endpoint
//! the call REACHED this emulator at — the host the request addressed, and the service's
//! own regional endpoint for a caller that addressed one of AWS's own hosts — followed by
//! the queue's `/<accountId>/<QueueName>` path, which is the only part taken from the
//! store.** A client is required to send the string it was handed straight back, so the
//! host it names has to be a host that serves the queue: a url on an endpoint this emulator
//! does not answer on points every create-then-use flow at production AWS instead of at the
//! queue that was just created.
//!
//! The tables span what the rule generalises over: the REGION the call is made in (four of
//! them), the queue NAME (every character class the member admits, its shortest value, the
//! published length bound and one past it, and a FIFO name), the OPERATION the url is
//! reported by (`CreateQueue`, the repeat of a create, `GetQueueUrl`, `ListQueues` and
//! `ListDeadLetterSourceQueues`), the COUNT of urls in a listing (none, one, several) and
//! the ADDRESS the request arrived on (the service's own endpoint, another region's
//! endpoint, loopback with an ephemeral port, a host name with a port and an IPv6 literal).
//!
//! A listing reports the same form on EVERY page of a `MaxResults`/`NextToken` walk, so the
//! tables for the two paged listings (`ListQueues`, `ListDeadLetterSourceQueues`) span the
//! page SIZE the caller asks for as well — one entry a page, a size that does not divide the
//! count, the count itself, one past it and no size at all — and assert the rule over the
//! whole walk rather than over one answer.
//!
//! The converse rule is tested here too, because it is the same `(account, QueueName)`
//! pair read the other way round: **a value supplied where a `QueueUrl` is expected
//! addresses the queue whose name is its last path segment, in the account the segment
//! before it names — or in the caller's own account when it carries no path at all, so a
//! bare `QueueName` is a queue address — and is refused `QueueDoesNotExist` only when the
//! region the call was made in holds no such queue of this account. A value that carries no
//! such pair is no address of this endpoint at all, and is refused `InvalidAddress`
//! instead.** Those tables span the FORM of the address (the minted url, that url with a
//! trailing slash, a url built against another endpoint, the path alone, the account and
//! name alone, and the bare name; and, for the refusal, an endpoint root, a one-segment
//! path, an empty value and a path whose name is empty), the queue NAME, the OPERATION the
//! address is handed to, and the REGION the call is made in.

use serde_json::{json, Value};
use service_com_amazonaws_sqs_amazonsqs::state::{
    MAX_QUEUE_NAME_LENGTH, QUEUE_DOES_NOT_EXIST_MESSAGE,
};
use smithy_server_runtime::MultiServiceRouter;

use super::support::{
    account, call_in, call_with_headers, create_in, create_ok_in, emulator, error_code,
    error_shape, expected_arn_in, expected_url_in, fifo_attributes, host_in, ok_in, ok_on_host,
    OTHER_ACCOUNT, QUERY_ERROR_HEADER, REGION,
};

/// The regions every table walks: the harness's own and three further ones, so no row can
/// be satisfied by an authority keyed to one region label.
const REGIONS: &[&str] = &[REGION, "us-west-2", "eu-central-1", "ap-southeast-2"];

/// The endpoint prefix Amazon SQS publishes its regional endpoints under, which is the
/// first label of the authority a queue url carries. Spelled out here rather than read from
/// the crate, so a row fails if the service starts rendering urls on some other host.
const ENDPOINT_PREFIX: &str = "sqs";

/// One queue name the url form is asserted over.
struct NameCase {
    /// What the row is about, for the assertion messages.
    case: &'static str,
    /// The `QueueName` the create carries.
    name: String,
    /// The `Attributes` it carries: a FIFO name needs the attribute that makes the queue
    /// a FIFO queue, and nothing else here needs any attribute at all.
    attributes: Value,
    /// Whether a queue is created at all. A name past the published bound is refused, and
    /// a refused create mints no url — the one row where no url may appear.
    accepted: bool,
}

/// The name catalog, from `CreateQueueRequest$QueueName`'s own rules ("A queue name can
/// have up to 80 characters", "Valid values: alphanumeric characters, hyphens (-), and
/// underscores (_)", "A FIFO queue name must end with the .fifo suffix", "Queue URLs and
/// names are case-sensitive"): the shortest name, each character class, mixed case, the
/// published length bound, one character past it, and a FIFO name — none of them the name
/// any recorded call used.
fn name_cases() -> Vec<NameCase> {
    let at_the_bound = format!("url-form-{}", "a".repeat(MAX_QUEUE_NAME_LENGTH - "url-form-".len()));
    let past_the_bound = format!("{at_the_bound}b");
    assert_eq!(
        (at_the_bound.chars().count(), past_the_bound.chars().count()),
        (MAX_QUEUE_NAME_LENGTH, MAX_QUEUE_NAME_LENGTH + 1),
        "case the length rows: expected the bound and one past it, got {} and {}",
        at_the_bound.chars().count(),
        past_the_bound.chars().count()
    );
    vec![
        NameCase {
            case: "the shortest name",
            name: "q".to_string(),
            attributes: json!({}),
            accepted: true,
        },
        NameCase {
            case: "hyphens, underscores, digits and mixed case",
            name: "Url-Form_Queue-07".to_string(),
            attributes: json!({}),
            accepted: true,
        },
        NameCase {
            case: "underscores only",
            name: "url_form_underscored".to_string(),
            attributes: json!({}),
            accepted: true,
        },
        NameCase {
            case: "a FIFO name",
            name: "url-form-ordered.fifo".to_string(),
            attributes: fifo_attributes(&[]),
            accepted: true,
        },
        NameCase {
            case: "the published length bound",
            name: at_the_bound,
            attributes: json!({}),
            accepted: true,
        },
        NameCase {
            case: "one character past the published length bound",
            name: past_the_bound,
            attributes: json!({}),
            accepted: false,
        },
    ]
}

/// The url the service must report for the queue named `name` to a call that arrived on
/// `authority`, composed here from the parts the rule names rather than taken out of any
/// answer.
fn url_on(authority: &str, name: &str) -> String {
    format!("https://{authority}/{}/{name}", account())
}

/// The url the service must report for the queue named `name` in `region` to a call that
/// arrived on that region's own AWS endpoint host — the address the harness's requests
/// carry.
///
/// Deliberately a second spelling of the form [`expected_url_in`] composes: the two are
/// cross-checked in [`assert_regional_form`], so this suite fails if the harness and the
/// rule ever drift apart.
fn regional_url(region: &str, name: &str) -> String {
    url_on(&format!("{ENDPOINT_PREFIX}.{region}.amazonaws.com"), name)
}

/// Assert that `url` is a queue address on `authority` for `name`, component by component:
/// the scheme, the authority, and the two path segments.
fn assert_url_form(case: &str, authority: &str, name: &str, url: &str) {
    let expected = url_on(authority, name);
    assert_eq!(
        url, expected,
        "case {case} on {authority}: expected {expected:?}, got {url:?}"
    );
    let authority_and_path = url
        .strip_prefix("https://")
        .unwrap_or_else(|| panic!("case {case} on {authority}: expected an https url, got {url:?}"));
    let (reported, path) = authority_and_path
        .split_once('/')
        .unwrap_or_else(|| panic!("case {case} on {authority}: expected a path, got {url:?}"));
    assert_eq!(
        reported, authority,
        "case {case} on {authority}: the authority must be the endpoint the call reached, \
         got {reported:?}"
    );
    assert_eq!(
        path.split('/').collect::<Vec<&str>>(),
        vec![account(), name],
        "case {case} on {authority}: expected /<accountId>/<QueueName>, got {path:?}"
    );
}

/// Assert that `url` is the form a call arriving on `region`'s own AWS endpoint host — the
/// address every request of the shared harness carries — must be answered with.
fn assert_regional_form(case: &str, region: &str, name: &str, url: &str) {
    assert_url_form(case, &format!("{ENDPOINT_PREFIX}.{region}.amazonaws.com"), name, url);
    assert_eq!(
        url,
        regional_url(region, name),
        "case {case} in {region}: expected {:?}, got {url:?}",
        regional_url(region, name)
    );
    // The harness's own derivation of the same form, so the suite and every other suite
    // in this crate cannot drift on it.
    assert_eq!(
        url,
        expected_url_in(region, name),
        "case {case} in {region}: the harness form must be this form"
    );
}

/// The `QueueUrls` of a `ListQueues` reply, as a list — a listing that reports nothing
/// carries no member at all.
fn listed_urls(reply: &Value) -> Vec<String> {
    strings(reply, "QueueUrls")
}

/// The `queueUrls` of a `ListDeadLetterSourceQueues` reply, as a list.
fn source_urls(reply: &Value) -> Vec<String> {
    strings(reply, "queueUrls")
}

/// The string list `member` of a reply, empty when the member is absent.
fn strings(reply: &Value, member: &str) -> Vec<String> {
    reply
        .get(member)
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Every operation that carries a queue url reports the regional endpoint form, for every
/// region and every shape of name — and the url it reports addresses the queue it names.
#[tokio::test]
async fn every_reported_queue_url_is_the_regional_endpoint_form() {
    let router = emulator();
    for region in REGIONS {
        for NameCase {
            case,
            name,
            attributes,
            accepted,
        } in name_cases()
        {
            let (status, created) = create_in(&router, region, &name, &attributes).await;
            if !accepted {
                assert_eq!(
                    status, 400,
                    "case {case} in {region}: a name past the bound must be refused, \
                     got {status} with {created}"
                );
                assert!(
                    created.get("QueueUrl").is_none(),
                    "case {case} in {region}: a refused create must mint no url, got {created}"
                );
                continue;
            }
            assert_eq!(
                status, 200,
                "case {case} in {region}: expected the queue to be created, \
                 got {status} with {created}"
            );
            let url = created["QueueUrl"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case} in {region}: no QueueUrl in {created}"))
                .to_string();
            assert_regional_form(case, region, &name, &url);

            // The repeat of a create that asks for nothing different is answered with the
            // url the queue already carries.
            let again = ok_in(
                &router,
                region,
                "CreateQueue",
                match attributes.as_object().is_some_and(|map| !map.is_empty()) {
                    true => json!({"QueueName": &name, "Attributes": attributes}),
                    false => json!({"QueueName": &name}),
                },
            )
            .await;
            assert_eq!(
                again["QueueUrl"].as_str(),
                Some(url.as_str()),
                "case {case} in {region}: a repeated create must report the same url, \
                 got {again}"
            );

            // The name lookup reports the same string.
            let resolved = ok_in(&router, region, "GetQueueUrl", json!({"QueueName": &name})).await;
            assert_eq!(
                resolved["QueueUrl"].as_str(),
                Some(url.as_str()),
                "case {case} in {region}: GetQueueUrl must report the same url, got {resolved}"
            );

            // So does the listing of exactly this queue.
            let listed = ok_in(
                &router,
                region,
                "ListQueues",
                json!({"QueueNamePrefix": &name}),
            )
            .await;
            assert_eq!(
                listed_urls(&listed),
                vec![url.clone()],
                "case {case} in {region}: ListQueues must report the same url, got {listed}"
            );

            // And the url addresses the queue it names: the ARN read back through it is
            // that queue's.
            let reported = ok_in(
                &router,
                region,
                "GetQueueAttributes",
                json!({"QueueUrl": &url, "AttributeNames": ["QueueArn"]}),
            )
            .await;
            assert_eq!(
                reported["Attributes"]["QueueArn"].as_str(),
                Some(expected_arn_in(region, &name).as_str()),
                "case {case} in {region}: the url must address the queue it names, \
                 got {reported}"
            );
        }
    }
}

/// The url reported for a queue names the endpoint the call REACHED this emulator at: a
/// create, a name lookup and a listing made through an address are each answered a url on
/// THAT address, a url reported on one address still addresses its queue through another,
/// and a caller that addressed one of AWS's own hosts is answered the service's own
/// regional endpoint.
#[tokio::test]
async fn a_queue_url_names_the_endpoint_the_call_reached() {
    // The addresses span what a request can arrive on: the service's own regional
    // endpoint and the endpoint of a DIFFERENT region — both hosts of AWS's own, which a
    // caller that resolves them is answered the service's regional endpoint for — then
    // loopback with an ephemeral port, a host name with the emulator's own port, and an
    // IPv6 literal, which are addresses only this emulator answers on.
    let addresses: Vec<String> = vec![
        host_in(REGION),
        host_in("eu-west-1"),
        "127.0.0.1:40085".to_string(),
        "localhost:9297".to_string(),
        "[::1]:9297".to_string(),
    ];

    let router = emulator();
    let shared = "url-form-shared";
    let shared_url = create_ok_in(&router, REGION, shared, &json!({})).await;
    assert_regional_form("the shared queue", REGION, shared, &shared_url);

    for (index, address) in addresses.iter().enumerate() {
        // Which authority this address must be answered with: its own, unless it is a
        // host AWS publishes an endpoint at — a caller that resolves those is told the
        // service's own regional endpoint for the region the call was made in.
        let expected_authority = match address.ends_with(".amazonaws.com") {
            true => format!("{ENDPOINT_PREFIX}.{REGION}.amazonaws.com"),
            false => address.clone(),
        };

        // 1. The stored queue is reported at this endpoint.
        let resolved = ok_on_host(
            &router,
            address,
            REGION,
            "GetQueueUrl",
            json!({"QueueName": shared}),
        )
        .await;
        let looked_up = resolved["QueueUrl"]
            .as_str()
            .unwrap_or_else(|| panic!("case GetQueueUrl on {address}: no QueueUrl in {resolved}"))
            .to_string();
        assert_url_form("GetQueueUrl", &expected_authority, shared, &looked_up);

        // 2. A create made through this address mints a url on this endpoint.
        let name = format!("url-form-address-{index}");
        let created = ok_on_host(
            &router,
            address,
            REGION,
            "CreateQueue",
            json!({"QueueName": &name}),
        )
        .await;
        let minted = created["QueueUrl"]
            .as_str()
            .unwrap_or_else(|| panic!("case CreateQueue on {address}: no QueueUrl in {created}"))
            .to_string();
        assert_url_form("CreateQueue", &expected_authority, &name, &minted);
        // The repeat of a create that asks for nothing different reports the same url.
        let again = ok_on_host(
            &router,
            address,
            REGION,
            "CreateQueue",
            json!({"QueueName": &name}),
        )
        .await;
        assert_eq!(
            again["QueueUrl"].as_str(),
            Some(minted.as_str()),
            "case the repeat of a create on {address}: expected {minted:?}, got {again}"
        );

        // 3. The listing read through this address reports every url on this endpoint.
        let listed = ok_on_host(&router, address, REGION, "ListQueues", json!({})).await;
        let urls = listed_urls(&listed);
        assert!(
            urls.contains(&minted) && urls.contains(&url_on(&expected_authority, shared)),
            "case ListQueues on {address}: expected the listing to hold {minted:?} and the \
             shared queue's url on this endpoint, got {urls:?}"
        );
        let prefix = format!("https://{expected_authority}/");
        for url in &urls {
            assert!(
                url.starts_with(&prefix),
                "case ListQueues on {address}: every url must be on the endpoint the call \
                 reached, got {url:?}"
            );
        }

        // 4. And a url reported on ANOTHER endpoint still addresses its queue through
        //    this one: the queue is resolved by its path, so no caller is cut off from a
        //    url it already holds.
        let reported = ok_on_host(
            &router,
            address,
            REGION,
            "GetQueueAttributes",
            json!({"QueueUrl": &shared_url, "AttributeNames": ["QueueArn"]}),
        )
        .await;
        assert_eq!(
            reported["Attributes"]["QueueArn"].as_str(),
            Some(expected_arn_in(REGION, shared).as_str()),
            "case GetQueueAttributes on {address}: a url of another endpoint must still \
             address its queue, got {reported}"
        );

        // 5. The url this endpoint minted is itself a working address here: the string a
        //    client is handed is the one it sends back.
        let through_minted = ok_on_host(
            &router,
            address,
            REGION,
            "GetQueueAttributes",
            json!({"QueueUrl": &minted, "AttributeNames": ["QueueArn"]}),
        )
        .await;
        assert_eq!(
            through_minted["Attributes"]["QueueArn"].as_str(),
            Some(expected_arn_in(REGION, &name).as_str()),
            "case GetQueueAttributes through the url minted on {address}: expected that \
             queue's ARN, got {through_minted}"
        );
        let sent = ok_on_host(
            &router,
            address,
            REGION,
            "SendMessage",
            json!({"QueueUrl": &minted, "MessageBody": "through the minted url"}),
        )
        .await;
        assert!(
            sent.get("MessageId").is_some(),
            "case SendMessage through the url minted on {address}: expected a message id, \
             got {sent}"
        );
    }
}

/// No url this service reports names a host it does not answer on: a caller that reached
/// this emulator at an address of its own is never handed a production AWS endpoint, which
/// is the url that sends a create-then-use flow out of the sandbox.
#[tokio::test]
async fn a_url_reported_to_a_caller_on_its_own_address_names_that_address() {
    let router = emulator();
    let address = "127.0.0.1:41099";
    let name = "url-form-no-public-host";
    let created = ok_on_host(
        &router,
        address,
        REGION,
        "CreateQueue",
        json!({"QueueName": name}),
    )
    .await;
    let minted = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case the create: no QueueUrl in {created}"))
        .to_string();
    assert!(
        !minted.contains("amazonaws.com"),
        "case the create on {address}: the url must name no AWS endpoint, got {minted:?}"
    );
    assert_url_form("the create", address, name, &minted);
}

/// The dead-letter source listing reports each source queue's own url, on the endpoint the
/// listing was read through — for a dead-letter queue with no source, one, and several.
#[tokio::test]
async fn the_dead_letter_source_listing_reports_the_urls_the_creates_minted() {
    // The counts span what the listing generalises over: a dead-letter queue nothing
    // redrives to, one source, and several.
    for sources in [0_usize, 1, 3] {
        let router = emulator();
        let dead_letter = format!("url-form-dlq-{sources}");
        let dead_letter_url = create_ok_in(&router, REGION, &dead_letter, &json!({})).await;
        let target_arn = expected_arn_in(REGION, &dead_letter);

        let mut names: Vec<String> = Vec::with_capacity(sources);
        for index in 0..sources {
            let name = format!("url-form-source-{sources}-{index}");
            let policy = json!({"deadLetterTargetArn": &target_arn, "maxReceiveCount": "3"});
            let url = create_ok_in(
                &router,
                REGION,
                &name,
                &json!({"RedrivePolicy": policy.to_string()}),
            )
            .await;
            assert_regional_form("a source queue", REGION, &name, &url);
            names.push(name);
        }
        names.sort();

        // Read through the regional endpoint and through a loopback address: each listing
        // reports the same source queues, each at the endpoint THAT call reached.
        for address in [host_in(REGION), "127.0.0.1:40225".to_string()] {
            let authority = match address.ends_with(".amazonaws.com") {
                true => format!("{ENDPOINT_PREFIX}.{REGION}.amazonaws.com"),
                false => address.clone(),
            };
            let mut expected: Vec<String> = names
                .iter()
                .map(|name| url_on(&authority, name))
                .collect();
            expected.sort();
            let reply = ok_on_host(
                &router,
                &address,
                REGION,
                "ListDeadLetterSourceQueues",
                json!({"QueueUrl": &dead_letter_url}),
            )
            .await;
            let mut reported = source_urls(&reply);
            reported.sort();
            assert_eq!(
                reported, expected,
                "case {sources} sources listed on {address}: expected {expected:?}, \
                 got {reported:?}"
            );
        }
    }
}

/// A queue url reported by this service resolves back to its queue: a name whose url is
/// taken from one operation and handed to another addresses the same queue, and no other.
#[tokio::test]
async fn a_reported_url_round_trips_through_the_operations_that_take_one() {
    let router = emulator();
    let name = "url-form-round-trip";
    let created = create_ok_in(&router, REGION, name, &json!({})).await;
    let resolved = ok_in(&router, REGION, "GetQueueUrl", json!({"QueueName": name})).await;
    let from_lookup = resolved["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case the lookup: no QueueUrl in {resolved}"))
        .to_string();
    assert_eq!(
        from_lookup, created,
        "case the lookup: expected {created:?}, got {from_lookup:?}"
    );

    // Every url-addressed operation this suite needs to prove the url is a working
    // address, each one driven with the url the LOOKUP reported.
    let sent = ok_in(
        &router,
        REGION,
        "SendMessage",
        json!({"QueueUrl": &from_lookup, "MessageBody": "through a reported url"}),
    )
    .await;
    assert!(
        sent.get("MessageId").is_some(),
        "case SendMessage through a reported url: expected a message id, got {sent}"
    );
    let tagged = call_in(
        &router,
        REGION,
        "TagQueue",
        &json!({"QueueUrl": &from_lookup, "Tags": {"suite": "url-form"}}),
    )
    .await;
    assert_eq!(
        tagged.0, 200,
        "case TagQueue through a reported url: expected HTTP 200, got {tagged:?}"
    );
    let tags = ok_in(
        &router,
        REGION,
        "ListQueueTags",
        json!({"QueueUrl": &from_lookup}),
    )
    .await;
    assert_eq!(
        tags["Tags"]["suite"].as_str(),
        Some("url-form"),
        "case ListQueueTags through a reported url: expected the tag just set, got {tags}"
    );
}

/// The queue names every address-form table is walked over: a plain name, one mixing the
/// character classes `CreateQueueRequest$QueueName` admits (alphanumeric, hyphen,
/// underscore, and case, which is significant), a FIFO name, and the published 80-character
/// bound — so no row can pass through a name of one shape alone.
fn address_form_names() -> Vec<(&'static str, String, Value)> {
    let at_the_bound = format!(
        "address-form-{}",
        "a".repeat(MAX_QUEUE_NAME_LENGTH - "address-form-".len())
    );
    assert_eq!(
        at_the_bound.chars().count(),
        MAX_QUEUE_NAME_LENGTH,
        "case the length row: expected the published bound, got {}",
        at_the_bound.chars().count()
    );
    vec![
        ("a plain name", "address-form-plain".to_string(), json!({})),
        (
            "hyphens, underscores, digits and mixed case",
            "Address-Form_07".to_string(),
            json!({}),
        ),
        (
            "a FIFO name",
            "address-form-ordered.fifo".to_string(),
            fifo_attributes(&[]),
        ),
        ("the published length bound", at_the_bound, json!({})),
    ]
}

/// Every form of address that carries the queue's name as its last segment and this
/// account — or no account segment at all — before it, as a function of the name.
fn resolving_address_forms(name: &str) -> Vec<(&'static str, String)> {
    let minted = expected_url_in(REGION, name);
    vec![
        ("the url the create minted", minted.clone()),
        ("that url with a trailing slash", format!("{minted}/")),
        (
            "a url built against another endpoint this emulator answers on",
            format!("http://127.0.0.1:40193/{}/{name}", account()),
        ),
        (
            "a url built against the legacy global endpoint",
            format!("https://queue.amazonaws.com/{}/{name}", account()),
        ),
        ("the path alone", format!("/{}/{name}", account())),
        (
            "the account and the name with no endpoint before them",
            format!("{}/{name}", account()),
        ),
        ("the bare QueueName", name.to_string()),
    ]
}

/// Every address form that names a live queue of this account resolves to THAT queue: the
/// attributes read through it are the queue's own, and a change made through one form is
/// seen through every other.
#[tokio::test]
async fn every_address_form_of_a_live_queue_resolves_to_that_queue() {
    let router = emulator();
    for (case, name, attributes) in address_form_names() {
        let minted = create_ok_in(&router, REGION, &name, &attributes).await;
        let expected_arn = expected_arn_in(REGION, &name);

        for (form, address) in resolving_address_forms(&name) {
            let reported = ok_in(
                &router,
                REGION,
                "GetQueueAttributes",
                json!({"QueueUrl": &address, "AttributeNames": ["QueueArn"]}),
            )
            .await;
            assert_eq!(
                reported["Attributes"]["QueueArn"].as_str(),
                Some(expected_arn.as_str()),
                "case {case} addressed by {form} ({address:?}): expected {expected_arn:?}, \
                 got {reported}"
            );

            // A reporting operation that carries no url of its own resolves the same queue
            // through the same address.
            let tags = ok_in(
                &router,
                REGION,
                "ListQueueTags",
                json!({"QueueUrl": &address}),
            )
            .await;
            assert!(
                tags.get("Tags")
                    .and_then(|tags| tags.get("form"))
                    .is_none(),
                "case {case} addressed by {form} ({address:?}): the queue carries no tag from \
                 an earlier row, got {tags}"
            );

            // And a WRITE through this address lands on the same queue: the tag it sets is
            // the one the minted url reports back, and the next form sees the queue
            // untagged again.
            let (status, tagged) = call_in(
                &router,
                REGION,
                "TagQueue",
                &json!({"QueueUrl": &address, "Tags": {"form": form}}),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case} tagged by {form} ({address:?}): expected HTTP 200, \
                 got {status} with {tagged}"
            );
            let through_minted = ok_in(
                &router,
                REGION,
                "ListQueueTags",
                json!({"QueueUrl": &minted}),
            )
            .await;
            assert_eq!(
                through_minted["Tags"]["form"].as_str(),
                Some(form),
                "case {case} tagged by {form} ({address:?}): the minted url must report the \
                 tag that address set, got {through_minted}"
            );
            let (status, untagged) = call_in(
                &router,
                REGION,
                "UntagQueue",
                &json!({"QueueUrl": &address, "TagKeys": ["form"]}),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case} untagged by {form} ({address:?}): expected HTTP 200, \
                 got {status} with {untagged}"
            );
        }
    }
}

/// An ADDRESS that names no queue of this account in the region the call was made in is
/// refused `QueueDoesNotExist` with AWS's own sentence — whichever operation it was handed
/// to, and whatever makes it name nothing.
#[tokio::test]
async fn an_address_naming_no_queue_of_this_account_is_refused() {
    let router = emulator();
    let held = "address-form-held";
    let _ = create_ok_in(&router, REGION, held, &json!({})).await;
    assert_ne!(
        OTHER_ACCOUNT,
        account(),
        "case another account: the rows below need an account that is not this emulator's"
    );
    // One character past the published name bound: no queue of any account carries a name
    // that long, so neither spelling of it addresses one.
    let past_the_bound = "b".repeat(MAX_QUEUE_NAME_LENGTH + 1);

    // Every row IS a queue address — a `/<accountId>/<QueueName>` pair, or a bare queue
    // name — and names no queue this region holds. A value carrying no such pair is the
    // other fault, which [`a_value_that_is_no_address_of_this_endpoint_is_refused`] owns.
    let rows: Vec<(&str, String)> = vec![
        ("a bare name no queue carries", "address-form-absent".to_string()),
        (
            "a url of a name no queue carries",
            expected_url_in(REGION, "address-form-absent"),
        ),
        (
            "a url of a name no queue carries, on another endpoint this emulator answers on",
            format!("https://127.0.0.1:40777/{}/address-form-absent", account()),
        ),
        (
            "another account's queue of a name this account holds",
            format!("https://{}/{OTHER_ACCOUNT}/{held}", host_in(REGION)),
        ),
        (
            "the bare account and a name this account holds, for another account",
            format!("{OTHER_ACCOUNT}/{held}"),
        ),
        (
            "a last segment longer than a queue name can be",
            format!("{}/{past_the_bound}", account()),
        ),
        (
            "a bare value longer than a queue name can be",
            past_the_bound.clone(),
        ),
    ];

    for (case, address) in &rows {
        for operation in [
            "GetQueueAttributes",
            "ListQueueTags",
            "SetQueueAttributes",
            "SendMessage",
        ] {
            let request = match operation {
                "GetQueueAttributes" => {
                    json!({"QueueUrl": address, "AttributeNames": ["QueueArn"]})
                }
                "SetQueueAttributes" => {
                    json!({"QueueUrl": address, "Attributes": {"VisibilityTimeout": "10"}})
                }
                "SendMessage" => json!({"QueueUrl": address, "MessageBody": "hello"}),
                _ => json!({"QueueUrl": address}),
            };
            let (status, parsed) = call_in(&router, REGION, operation, &request).await;
            assert_eq!(
                (status, error_code(&parsed).as_str()),
                (400, "QueueDoesNotExist"),
                "case {operation} on {case} ({address:?}): expected a 400 QueueDoesNotExist, \
                 got {status} with {parsed}"
            );
            assert_eq!(
                parsed.get("message").and_then(Value::as_str),
                Some(QUEUE_DOES_NOT_EXIST_MESSAGE),
                "case {operation} on {case} ({address:?}): expected AWS's fixed sentence, \
                 got {parsed}"
            );
        }
    }

    // The queue this account does hold still resolves through the same forms, so every
    // refusal above is about the queue the address names and not about the form it is in.
    for (form, address) in resolving_address_forms(held) {
        let reported = ok_in(
            &router,
            REGION,
            "GetQueueAttributes",
            json!({"QueueUrl": &address, "AttributeNames": ["QueueArn"]}),
        )
        .await;
        assert_eq!(
            reported["Attributes"]["QueueArn"].as_str(),
            Some(expected_arn_in(REGION, held).as_str()),
            "case the queue this account holds addressed by {form} ({address:?}): \
             expected its own ARN, got {reported}"
        );
    }
}

/// An address carrying no endpoint of its own resolves in the region the CALL was made in:
/// the same bare name names each region's own queue, and nothing at all in a region that
/// holds none.
#[tokio::test]
async fn an_address_resolves_in_the_region_the_call_was_made_in() {
    let router = emulator();
    let name = "address-form-regional";
    let home = REGIONS[0];
    let elsewhere = REGIONS[1];
    assert_ne!(
        home, elsewhere,
        "case the two regions: the rows below need two different regions"
    );
    let _ = create_ok_in(&router, home, name, &json!({})).await;

    for (form, address) in resolving_address_forms(name) {
        // The region that holds the queue resolves the address to its own queue.
        let reported = ok_in(
            &router,
            home,
            "GetQueueAttributes",
            json!({"QueueUrl": &address, "AttributeNames": ["QueueArn"]}),
        )
        .await;
        assert_eq!(
            reported["Attributes"]["QueueArn"].as_str(),
            Some(expected_arn_in(home, name).as_str()),
            "case {form} ({address:?}) in {home}: expected the queue of that region, \
             got {reported}"
        );

        // The region that holds none has nothing of that name to report.
        let (status, parsed) = call_in(
            &router,
            elsewhere,
            "GetQueueAttributes",
            &json!({"QueueUrl": &address, "AttributeNames": ["QueueArn"]}),
        )
        .await;
        assert_eq!(
            (status, error_code(&parsed).as_str()),
            (400, "QueueDoesNotExist"),
            "case {form} ({address:?}) in {elsewhere}: a region holding no queue of that \
             name must refuse it, got {status} with {parsed}"
        );
    }

    // Once that other region holds a queue of the same name, the same address resolves
    // there as well — to THAT region's queue, which carries its own ARN.
    let _ = create_ok_in(&router, elsewhere, name, &json!({})).await;
    for (form, address) in resolving_address_forms(name) {
        let reported = ok_in(
            &router,
            elsewhere,
            "GetQueueAttributes",
            json!({"QueueUrl": &address, "AttributeNames": ["QueueArn"]}),
        )
        .await;
        assert_eq!(
            reported["Attributes"]["QueueArn"].as_str(),
            Some(expected_arn_in(elsewhere, name).as_str()),
            "case {form} ({address:?}) in {elsewhere}: expected that region's own queue, \
             got {reported}"
        );
    }
}

/// Every form of value that is NO queue address of this endpoint, with the ADDRESS the
/// refusal of it must name: each carries no `/<accountId>/<QueueName>` pair and is not a
/// bare queue name either, so there is no queue for it to name whatever this store holds.
///
/// The rows span how a value can fail to be an address: an endpoint root (with and without
/// its trailing slash), a path naming only one segment (the segment ahead of the name is
/// then not an account id), a path whose account segment is not an account number, a path
/// whose name is empty, a url on another scheme and endpoint, the root path, and an empty
/// value. The address each is refused with is the ENDPOINT the value points at — RECORDED:
/// a `QueueUrl` whose path names no queue is refused naming the url's endpoint, the path it
/// carried dropped — and the value itself where it names no endpoint at all, which for the
/// empty value leaves the sentence's fixed text with nothing between its two halves.
fn addresses_no_endpoint_serves(held: &str) -> Vec<(&'static str, String, String)> {
    let endpoint = host_in(REGION);
    let root = format!("https://{endpoint}/");
    vec![
        (
            "an endpoint root with a trailing slash",
            root.clone(),
            root.clone(),
        ),
        (
            "an endpoint root with no path at all",
            format!("https://{endpoint}"),
            root.clone(),
        ),
        (
            "a url carrying a path prefix but no account segment",
            format!("https://{endpoint}/{held}"),
            root.clone(),
        ),
        (
            "a url whose account segment is not an account number",
            format!("https://{endpoint}/not-an-account/{held}"),
            root.clone(),
        ),
        (
            "a url whose account segment is one digit short of an account number",
            format!("https://{endpoint}/{}/{held}", "1".repeat(11)),
            root.clone(),
        ),
        (
            "a url whose queue-name segment is empty",
            format!("https://{endpoint}/{}/", account()),
            root,
        ),
        (
            "a url on another scheme and endpoint with a one-segment path",
            format!("http://127.0.0.1:40193/{held}"),
            "http://127.0.0.1:40193/".to_string(),
        ),
        (
            "an address that is a single slash",
            "/".to_string(),
            "/".to_string(),
        ),
        ("an empty address", String::new(), String::new()),
    ]
}

/// A value supplied for a `QueueUrl` that is no address of this endpoint is refused as the
/// ADDRESS fault it is — HTTP 404 `InvalidAddress`, reading "The address <endpoint> is not
/// valid for this endpoint." and reported in the legacy query header as
/// `InvalidAddress;Sender` — and never as a queue that does not exist.
///
/// The two faults are different conditions, and the model declares an error for each
/// (`InvalidAddress`, httpError 404; `QueueDoesNotExist`, httpError 400): a client keying
/// retries or create-on-absence logic on `QueueDoesNotExist` must not see it for a url it
/// built wrong or pointed at the wrong endpoint.
///
/// The table spans the form of the value (every row of
/// [`addresses_no_endpoint_serves`]) and the operation it is handed to.
#[tokio::test]
async fn a_value_that_is_no_address_of_this_endpoint_is_refused() {
    let router = emulator();
    let held = "address-fault-held";
    let held_url = create_ok_in(&router, REGION, held, &json!({})).await;

    for (case, address, named) in addresses_no_endpoint_serves(held) {
        for operation in [
            "GetQueueAttributes",
            "ListQueueTags",
            "SetQueueAttributes",
            "SendMessage",
            "ReceiveMessage",
            "PurgeQueue",
            "DeleteQueue",
            "TagQueue",
        ] {
            let request = match operation {
                "GetQueueAttributes" => {
                    json!({"QueueUrl": &address, "AttributeNames": ["All"]})
                }
                "SetQueueAttributes" => {
                    json!({"QueueUrl": &address, "Attributes": {"VisibilityTimeout": "10"}})
                }
                "SendMessage" => json!({"QueueUrl": &address, "MessageBody": "hello"}),
                "TagQueue" => json!({"QueueUrl": &address, "Tags": {"form": "address"}}),
                _ => json!({"QueueUrl": &address}),
            };
            let (status, headers, parsed) = call_with_headers(&router, operation, &request).await;
            let expected_message = format!("The address {named} is not valid for this endpoint.");
            assert_eq!(
                (
                    status,
                    error_code(&parsed).as_str(),
                    parsed.get("message").and_then(Value::as_str),
                ),
                (404, "InvalidAddress", Some(expected_message.as_str())),
                "case {operation} on {case} ({address:?}): expected a 404 InvalidAddress \
                 reading {expected_message:?}, got {status} with {parsed}"
            );
            assert_eq!(
                error_shape(&parsed),
                INVALID_ADDRESS_SHAPE,
                "case {operation} on {case} ({address:?}): expected the modelled shape, \
                 got {parsed}"
            );
            assert_eq!(
                headers
                    .get(QUERY_ERROR_HEADER)
                    .and_then(|value| value.to_str().ok()),
                Some(INVALID_ADDRESS_QUERY_REPORT),
                "case {operation} on {case} ({address:?}): expected \
                 {QUERY_ERROR_HEADER} {INVALID_ADDRESS_QUERY_REPORT:?}, got {parsed}"
            );
        }
    }

    // The queue this account does hold still resolves, so every refusal above is about the
    // value supplied and not about the operation.
    let reported = ok_in(
        &router,
        REGION,
        "GetQueueAttributes",
        json!({"QueueUrl": &held_url, "AttributeNames": ["QueueArn"]}),
    )
    .await;
    assert_eq!(
        reported["Attributes"]["QueueArn"].as_str(),
        Some(expected_arn_in(REGION, held).as_str()),
        "case the queue this account holds: expected its own ARN, got {reported}"
    );
}

/// The shape id a refusal of a value that is no address of this endpoint names: the model's
/// own `com.amazonaws.sqs#InvalidAddress` (`smithy_query(query="operation",
/// operation="GetQueueAttributes",
/// model_path="services/service-com-amazonaws-sqs-amazonsqs/smithy-model/com-amazonaws-sqs-amazonsqs.json")`
/// lists it with `httpError` 404).
const INVALID_ADDRESS_SHAPE: &str = "com.amazonaws.sqs#InvalidAddress";

/// The legacy query report that refusal travels under: the model's `awsQueryError.code`
/// for that shape (`InvalidAddress`), beside the fault side a refusal the caller caused is
/// reported under.
const INVALID_ADDRESS_QUERY_REPORT: &str = "InvalidAddress;Sender";

/// The addresses a PAGED listing is read through: the service's own regional endpoint host
/// — a host of AWS's own, which a caller that resolves it is answered the service's
/// regional endpoint for — then loopback with an ephemeral port, a host name with the
/// emulator's own port, and an IPv6 literal, which are addresses only this emulator
/// answers on.
fn paged_addresses() -> Vec<String> {
    vec![
        host_in(REGION),
        "127.0.0.1:40613".to_string(),
        "localhost:9297".to_string(),
        "[::1]:9297".to_string(),
    ]
}

/// Which authority a call arriving on `address` must be answered with: its own, unless the
/// address is a host AWS publishes an endpoint at — a caller that resolves those is told the
/// service's own regional endpoint for the region the call was made in.
fn answered_authority(address: &str) -> String {
    match address.ends_with(".amazonaws.com") {
        true => format!("{ENDPOINT_PREFIX}.{REGION}.amazonaws.com"),
        false => address.to_string(),
    }
}

/// How many pages a walk in this suite may take before it is a walk that does not end:
/// every table here lists a handful of queues at a page size of at least one entry, so a
/// walk longer than this is a cursor that stopped advancing rather than a longer listing.
const PAGE_WALK_BOUND: usize = 16;

/// Walk EVERY page of `operation` — the request built from `body`, asking for `page_size`
/// entries a page when it names one — through the `NextToken` each reply carries, with
/// every page read through `address`, and answer the pages in walk order as each page's
/// `member` list.
///
/// A client that pages does exactly this, which is why the walk rather than one answer is
/// what the rule is asserted over.
async fn walked_pages(
    router: &MultiServiceRouter,
    address: &str,
    operation: &str,
    member: &str,
    body: &Value,
    page_size: Option<i64>,
) -> Vec<Vec<String>> {
    let mut pages: Vec<Vec<String>> = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let mut request = body.clone();
        if let Some(size) = page_size {
            request["MaxResults"] = json!(size);
        }
        if let Some(token) = &cursor {
            request["NextToken"] = json!(token);
        }
        let reply = ok_on_host(router, address, REGION, operation, request).await;
        pages.push(strings(&reply, member));
        assert!(
            pages.len() <= PAGE_WALK_BOUND,
            "case {operation} on {address} at MaxResults {page_size:?}: the walk must end, \
             got {} pages: {pages:?}",
            pages.len()
        );
        cursor = reply
            .get("NextToken")
            .and_then(Value::as_str)
            .map(ToString::to_string);
        if cursor.is_none() {
            return pages;
        }
    }
}

/// Assert that every url of every page of `pages` is a queue address on `authority`, and
/// that the whole walk reported `names`' queues in that order, once each.
fn assert_walked_urls(case: &str, authority: &str, names: &[String], pages: &[Vec<String>]) {
    let expected: Vec<String> = names.iter().map(|name| url_on(authority, name)).collect();
    let walked: Vec<String> = pages.concat();
    assert_eq!(
        walked, expected,
        "case {case}: expected the walk to report {expected:?}, got {walked:?}"
    );
    // Component by component as well, so a row says WHICH part of an url diverged.
    for (name, url) in names.iter().zip(walked.iter()) {
        assert_url_form(case, authority, name, url);
    }
}

/// Every page of a PAGED listing reports its urls on the endpoint the call reached: a page
/// handed to a `MaxResults`/`NextToken` walk is rendered exactly as an unpaged answer is, so
/// a client that accumulates pages never collects an address it cannot reach.
///
/// The table spans the page SIZE the caller asks for (one entry a page, so every boundary is
/// a page; a size that does not divide the count; the count itself; one past it; and no size
/// at all, which is the service's own page size), and the ADDRESS the call arrives on.
#[tokio::test]
async fn every_page_of_a_queue_listing_reports_urls_on_the_endpoint_the_call_reached() {
    const PREFIX: &str = "paged-url-form-";
    let router = emulator();
    // Name-ordered, which is the order a listing of this service walks in.
    let names: Vec<String> = ["alpha", "bravo", "charlie", "delta", "echo"]
        .iter()
        .map(|suffix| format!("{PREFIX}{suffix}"))
        .collect();
    for name in &names {
        create_ok_in(&router, REGION, name, &json!({})).await;
    }
    // One queue outside the prefix, so each page is a slice of the FILTERED listing.
    create_ok_in(&router, REGION, "paged-other-queue", &json!({})).await;

    for address in paged_addresses() {
        let authority = answered_authority(&address);
        for page_size in [Some(1), Some(2), Some(5), Some(6), None] {
            let pages = walked_pages(
                &router,
                &address,
                "ListQueues",
                "QueueUrls",
                &json!({"QueueNamePrefix": PREFIX}),
                page_size,
            )
            .await;
            let case = format!("ListQueues on {address} at MaxResults {page_size:?}");
            if let Some(size) = page_size {
                for (index, page) in pages.iter().enumerate() {
                    assert!(
                        i64::try_from(page.len()).is_ok_and(|held| held <= size),
                        "case {case}: page {index} must hold at most {size} urls, got {page:?}"
                    );
                }
            }
            // A page size of one entry is a walk of as many pages as there are queues, so
            // the rows that page really do page.
            if page_size == Some(1) {
                assert!(
                    pages.len() >= names.len(),
                    "case {case}: expected a page per queue, got {} pages: {pages:?}",
                    pages.len()
                );
            }
            assert_walked_urls(&case, &authority, &names, &pages);
        }
    }
}

/// The same rule on the dead-letter source listing, which pages the same way: every source
/// queue url of every page is an address on the endpoint the walk was read through, which is
/// what redrive tooling feeds back into the queue-addressed operations.
///
/// The table spans the page SIZE (one entry a page, a size that does not divide the count,
/// the count itself, and no size at all) and the ADDRESS the call arrives on.
#[tokio::test]
async fn every_page_of_a_dead_letter_source_listing_reports_urls_on_the_reached_endpoint() {
    let router = emulator();
    let dead_letter = "paged-dlq";
    let dead_letter_url = create_ok_in(&router, REGION, dead_letter, &json!({})).await;
    let target_arn = expected_arn_in(REGION, dead_letter);
    let names: Vec<String> = ["one", "three", "two"]
        .iter()
        .map(|suffix| format!("paged-dlq-source-{suffix}"))
        .collect();
    for name in &names {
        let policy = json!({"deadLetterTargetArn": &target_arn, "maxReceiveCount": "3"});
        create_ok_in(
            &router,
            REGION,
            name,
            &json!({"RedrivePolicy": policy.to_string()}),
        )
        .await;
    }
    // The listing is name-ordered, and these names are already in that order.
    let mut ordered = names.clone();
    ordered.sort();
    assert_eq!(
        ordered, names,
        "case the source names: expected the listing's own order, got {names:?}"
    );

    for address in paged_addresses() {
        let authority = answered_authority(&address);
        for page_size in [Some(1), Some(2), Some(3), None] {
            let pages = walked_pages(
                &router,
                &address,
                "ListDeadLetterSourceQueues",
                "queueUrls",
                &json!({"QueueUrl": &dead_letter_url}),
                page_size,
            )
            .await;
            let case =
                format!("ListDeadLetterSourceQueues on {address} at MaxResults {page_size:?}");
            if page_size == Some(1) {
                assert!(
                    pages.len() >= names.len(),
                    "case {case}: expected a page per source queue, got {} pages: {pages:?}",
                    pages.len()
                );
            }
            assert_walked_urls(&case, &authority, &names, &pages);
        }
    }
}

/// A url taken off a PAGE is a working handle on the endpoint that reported it: the string a
/// paging client is handed addresses the queue it names when it is sent straight back, which
/// is the whole reason the page's authority has to be an endpoint this emulator answers on.
#[tokio::test]
async fn a_url_taken_off_a_page_addresses_its_queue_through_the_endpoint_that_reported_it() {
    const PREFIX: &str = "paged-handle-";
    let router = emulator();
    let names: Vec<String> = ["alpha", "bravo", "charlie"]
        .iter()
        .map(|suffix| format!("{PREFIX}{suffix}"))
        .collect();
    for name in &names {
        create_ok_in(&router, REGION, name, &json!({})).await;
    }

    for address in paged_addresses() {
        let authority = answered_authority(&address);
        // One queue a page, so every url below came off a page of its own — the later
        // pages of the walk included.
        let pages = walked_pages(
            &router,
            &address,
            "ListQueues",
            "QueueUrls",
            &json!({"QueueNamePrefix": PREFIX}),
            Some(1),
        )
        .await;
        let case = format!("the paged walk on {address}");
        assert_walked_urls(&case, &authority, &names, &pages);

        for (name, url) in names.iter().zip(pages.concat().iter()) {
            let reported = ok_on_host(
                &router,
                &address,
                REGION,
                "GetQueueAttributes",
                json!({"QueueUrl": url, "AttributeNames": ["QueueArn"]}),
            )
            .await;
            assert_eq!(
                reported["Attributes"]["QueueArn"].as_str(),
                Some(expected_arn_in(REGION, name).as_str()),
                "case the url {url:?} off a page on {address}: expected the ARN of {name}, \
                 got {reported}"
            );
        }
    }
}

