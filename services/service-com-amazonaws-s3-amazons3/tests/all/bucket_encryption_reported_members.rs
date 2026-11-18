// SPDX-License-Identifier: CC0-1.0

//! Which MEMBERS a general purpose bucket's default encryption is reported with.
//!
//! RULE: `GetBucketEncryption` reports, for every rule, the settings IN FORCE on the
//! bucket — a setting with a documented default is materialised at that default, and the
//! bucket-level blocked set is reported whether a configuration named it or not.
//!
//! The two documented defaults are the algorithm ("by default, all buckets have a default
//! encryption configuration that uses server-side encryption with Amazon S3 managed keys
//! (SSE-S3)", `GetBucketEncryption`'s own documentation) and the S3 Bucket Key ("general
//! purpose buckets - by default, S3 Bucket Key is not enabled",
//! `smithy_query(query="doc",shape="ServerSideEncryptionRule",member="BucketKeyEnabled")`),
//! so both are reported whatever a rule named. `BlockedEncryptionTypes` is "a bucket-level
//! setting for Amazon S3 general purpose buckets used to prevent the upload of new objects
//! encrypted with the specified server-side encryption type", and "currently, this parameter
//! only supports blocking or unblocking server-side encryption with customer-provided keys
//! (SSE-C)"
//! (`smithy_query(query="doc",shape="ServerSideEncryptionRule",member="BlockedEncryptionTypes")`)
//! — a setting of the BUCKET rather than of one put, so a rule that named no set is in force
//! with the set the bucket carries anyway (the customer-key type) and a rule that named one
//! is in force with that.
//!
//! Why it matters on the wire rather than only in a parsed view: a compliance reader asks
//! this operation which encryption types the bucket blocks, and a client that round-trips the
//! rules it is handed (Get → Put) must be handed the set in force.
//!
//! The rows differ along what the rule generalises over: whether the bucket was configured
//! at all, WHICH algorithm a rule names (every `ServerSideEncryption` value the model
//! publishes, including the three no recorded call sends), whether a KMS key accompanies
//! it, whether the Bucket Key is on, off or unnamed, whether a rule names a block and
//! WHICH `EncryptionType` values it blocks (each published value and both together), how
//! MANY rules the container carries and which of them block, and whether the bucket's
//! blocked set was established and then replaced or reset.
//!
//! The last two suites assert WHERE the blocked set sits in the reported document rather
//! than only that it appears: a client reads it at
//! `ServerSideEncryptionConfiguration/Rule/BlockedEncryptionTypes/EncryptionType`, since
//! `BlockedEncryptionTypes$EncryptionType` is `xmlFlattened`
//! (`smithy_query(query="shape",shape="BlockedEncryptionTypes")`), so its values are the
//! container's own children with no list element between them — and they read every row in
//! both address forms, path-style and virtual-hosted-style, because where a request was
//! addressed is not part of what a bucket reports — and on a bucket hardened elsewhere
//! (versioning enabled, every published public-access switch on), because what this read
//! reports is the bucket's encryption state alone.

use quick_xml::events::Event;
use quick_xml::Reader;
use service_com_amazonaws_s3_amazons3::state::{DEFAULT_BUCKET_KEY_ENABLED, DEFAULT_SSE_ALGORITHM};
use smithy_server_runtime::MultiServiceRouter;

use super::bucket_encryption::{
    as_reported, get_encryption, put_encryption, reported_rules, Rule, ENCRYPTION_TYPE_VALUES,
    SERVER_SIDE_ENCRYPTION_VALUES,
};
use super::bucket_lifecycle::{create_bucket, router, send, send_bytes_to_host, Answer};
use super::bucket_public_access_block::{put_public_access_block, SETTINGS};
use super::object_and_tagging_roundtrip::text;
use super::object_versioning::set_versioning;

/// The element a reported rule carries its blocked set in — the container whose presence
/// this suite is about (`BlockedEncryptionTypes`, the member's own name: the model puts no
/// `xmlName` on it).
const BLOCKED_ELEMENT: &str = "BlockedEncryptionTypes";

/// Create a bucket and assert it was created, so a later assertion about what it reports is
/// about a bucket that exists.
async fn bucket(router: &MultiServiceRouter, name: &str, case: &str) {
    let created = create_bucket(router, name, None, None).await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: CreateBucket({name:?}) must answer 200, got {} with body {:?}",
        created.status,
        text(&created)
    );
}

/// `DELETE /<bucket>?encryption` — `DeleteBucketEncryption`.
async fn delete_encryption(router: &MultiServiceRouter, bucket: &str) -> Answer {
    send(router, "DELETE", &format!("/{bucket}?encryption"), &[], "").await
}

/// Read the bucket's configuration back and assert, on the wire and in the parsed view,
/// that it reports `expected` — including which blocked set each rule carries.
async fn assert_reports(
    router: &MultiServiceRouter,
    bucket_name: &str,
    case: &str,
    expected: &[Rule],
) {
    let read = get_encryption(router, bucket_name).await;
    assert_eq!(
        read.status, 200,
        "case {case:?}: GetBucketEncryption({bucket_name:?}) must answer 200, got {} with \
         body {:?}",
        read.status,
        text(&read)
    );
    let reported = reported_rules(&read);
    assert_eq!(
        reported, expected,
        "case {case:?}: {bucket_name:?} must report {expected:?}, got {reported:?} (body {:?})",
        text(&read)
    );

    // The container's presence, asserted on the bytes as well: every rule in force carries
    // the blocked set the bucket is on, so the element appears once per rule.
    let blocking_rules = expected
        .iter()
        .filter(|rule| rule.blocked_encryption_types.is_some())
        .count();
    let occurrences = text(&read).matches(&format!("<{BLOCKED_ELEMENT}>")).count();
    assert_eq!(
        occurrences, blocking_rules,
        "case {case:?}: {bucket_name:?} must carry {blocking_rules} <{BLOCKED_ELEMENT}> \
         element(s), got {occurrences} (body {:?})",
        text(&read)
    );
}

/// One row: the case, the bucket it configures, and the configuration it puts — `None` for
/// a bucket left exactly as `CreateBucket` made it.
struct Row {
    case: &'static str,
    bucket: &'static str,
    configured: Option<Vec<Rule>>,
}

/// The rule every bucket reports while no `PutBucketEncryption` has configured one: the
/// documented SSE-S3 algorithm with the Bucket Key off, and the bucket-level blocked set.
fn base_default() -> Rule {
    Rule::by_default(DEFAULT_SSE_ALGORITHM)
        .with_bucket_key(DEFAULT_BUCKET_KEY_ENABLED)
        .blocking(&[ENCRYPTION_TYPE_VALUES[1]])
}

/// The rules each row must report: the configured ones with `BucketKeyEnabled` materialised,
/// the bucket-level blocked set in force where a rule named none, and every other member
/// exactly as configured — or the base default.
fn expected_of(row: &Row) -> Vec<Rule> {
    match &row.configured {
        None => vec![base_default()],
        Some(rules) => rules
            .iter()
            .map(|rule| Rule {
                bucket_key_enabled: Some(rule.bucket_key_enabled.unwrap_or(false)),
                blocked_encryption_types: rule
                    .blocked_encryption_types
                    .clone()
                    .or_else(|| Some(vec![ENCRYPTION_TYPE_VALUES[1].to_string()])),
                ..rule.clone()
            })
            .collect(),
    }
}

/// A KMS key in the ARN form `KMSMasterKeyID` documents ("you can specify the key ID, key
/// alias, or the Amazon Resource Name (ARN) of the KMS key").
const KEY_ARN: &str = "arn:aws:kms:us-west-2:111122223333:key/1234abcd-12ab-34cd-56ef-1234567890ab";

fn rows() -> Vec<Row> {
    // The two published `EncryptionType` values, taken from the model's own list rather
    // than spelled here: `NONE` unblocks and `SSE-C` blocks the customer-key type.
    let [unblocked, customer_key] = ENCRYPTION_TYPE_VALUES;
    // The algorithm values, likewise from the published list: index 0 is SSE-S3 and the
    // last two are the SSE-KMS pair a `KMSMasterKeyID` may accompany.
    let sse_s3 = SERVER_SIDE_ENCRYPTION_VALUES[0];
    let kms = SERVER_SIDE_ENCRYPTION_VALUES[3];
    let dual_layer_kms = SERVER_SIDE_ENCRYPTION_VALUES[4];

    vec![
        Row {
            case: "a bucket no put has configured",
            bucket: "members-unconfigured",
            configured: None,
        },
        Row {
            case: "an SSE-S3 default, named explicitly and nothing else",
            bucket: "members-sse-s3",
            configured: Some(vec![Rule::by_default(sse_s3)]),
        },
        Row {
            case: "an SSE-KMS default naming no key, as a toolkit staging bucket declares",
            bucket: "members-kms-no-key",
            configured: Some(vec![Rule::by_default(kms)]),
        },
        Row {
            case: "an SSE-KMS default naming a key ARN, with the Bucket Key on",
            bucket: "members-kms-keyed",
            configured: Some(vec![Rule::by_default(kms)
                .with_key(KEY_ARN)
                .with_bucket_key(true)]),
        },
        Row {
            case: "a dual-layer SSE-KMS default with the Bucket Key off",
            bucket: "members-dsse-kms",
            configured: Some(vec![Rule::by_default(dual_layer_kms).with_bucket_key(false)]),
        },
        Row {
            case: "a rule naming only the Bucket Key",
            bucket: "members-bucket-key-only",
            configured: Some(vec![Rule::bucket_key_only(true)]),
        },
        Row {
            case: "a rule that blocks the customer-key type",
            bucket: "members-blocking-sse-c",
            configured: Some(vec![Rule::by_default(sse_s3).blocking(&[customer_key])]),
        },
        Row {
            case: "a rule that names the unblocking value",
            bucket: "members-blocking-none",
            configured: Some(vec![Rule::by_default(kms).blocking(&[unblocked])]),
        },
        Row {
            case: "a rule that names every published encryption type",
            bucket: "members-blocking-both",
            configured: Some(vec![
                Rule::by_default(sse_s3).blocking(&ENCRYPTION_TYPE_VALUES)
            ]),
        },
        Row {
            case: "two rules, only the second of which blocks",
            bucket: "members-two-rules",
            configured: Some(vec![
                Rule::by_default(kms).with_key(KEY_ARN),
                Rule::bucket_key_only(true).blocking(&[customer_key]),
            ]),
        },
    ]
}

#[tokio::test]
async fn a_bucket_reports_the_encryption_members_its_configuration_established() {
    let router = router();
    for row in rows() {
        bucket(&router, row.bucket, row.case).await;
        if let Some(rules) = &row.configured {
            let put = put_encryption(&router, row.bucket, rules).await;
            assert_eq!(
                put.status, 200,
                "case {:?}: PutBucketEncryption must answer 200, got {} with body {:?}",
                row.case,
                put.status,
                text(&put)
            );
        }
        assert_reports(&router, row.bucket, row.case, &expected_of(&row)).await;

        // Reading reports, it does not configure: the second read answers exactly what the
        // first did, so no member appeared because the bucket was read.
        assert_reports(&router, row.bucket, row.case, &expected_of(&row)).await;
    }
}

#[tokio::test]
async fn a_blocked_set_a_put_named_is_replaced_by_the_one_in_force() {
    let router = router();
    let [unblocked, customer_key] = ENCRYPTION_TYPE_VALUES;
    let name = "members-block-withdrawn";
    let case = "a blocked set named, then named otherwise and reset";
    bucket(&router, name, case).await;

    // Established: the put names the block, so the bucket reports it.
    let blocking = vec![Rule::by_default(DEFAULT_SSE_ALGORITHM).blocking(&[customer_key])];
    let put = put_encryption(&router, name, &blocking).await;
    assert_eq!(
        put.status, 200,
        "case {case:?}: the blocking put must answer 200, got {} with body {:?}",
        put.status,
        text(&put)
    );
    assert_reports(
        &router,
        name,
        "the blocking put is reported",
        &[Rule::by_default(DEFAULT_SSE_ALGORITHM)
            .with_bucket_key(DEFAULT_BUCKET_KEY_ENABLED)
            .blocking(&[customer_key])],
    )
    .await;

    // A put is a REPLACEMENT of the rules, and a rule that names NO block is in force with
    // the set the bucket carries anyway — the bucket-level setting is not withdrawn by a put
    // that says nothing about it, while a put that names the unblocking value IS the bucket's
    // set.
    let replacement = vec![Rule::by_default(SERVER_SIDE_ENCRYPTION_VALUES[3])];
    let replaced = put_encryption(&router, name, &replacement).await;
    assert_eq!(
        replaced.status, 200,
        "case {case:?}: the replacing put must answer 200, got {} with body {:?}",
        replaced.status,
        text(&replaced)
    );
    assert_reports(
        &router,
        name,
        "a replacement that names no block",
        &[Rule::by_default(SERVER_SIDE_ENCRYPTION_VALUES[3])
            .with_bucket_key(DEFAULT_BUCKET_KEY_ENABLED)
            .blocking(&[customer_key])],
    )
    .await;

    let unblocking = vec![Rule::by_default(SERVER_SIDE_ENCRYPTION_VALUES[3]).blocking(&[unblocked])];
    let unblocked_put = put_encryption(&router, name, &unblocking).await;
    assert_eq!(
        unblocked_put.status, 200,
        "case {case:?}: the unblocking put must answer 200, got {} with body {:?}",
        unblocked_put.status,
        text(&unblocked_put)
    );
    assert_reports(
        &router,
        name,
        "a put that names the unblocking value",
        &[Rule::by_default(SERVER_SIDE_ENCRYPTION_VALUES[3])
            .with_bucket_key(DEFAULT_BUCKET_KEY_ENABLED)
            .blocking(&[unblocked])],
    )
    .await;

    // And a reset puts the bucket back where `CreateBucket` left it.
    let reset = delete_encryption(&router, name).await;
    assert_eq!(
        reset.status, 204,
        "case {case:?}: DeleteBucketEncryption must answer the modelled 204, got {} with \
         body {:?}",
        reset.status,
        text(&reset)
    );
    assert_reports(&router, name, "the bucket after a reset", &[base_default()]).await;
}

/// The element the whole report sits in, the element one rule is reported as, and the
/// element one blocked type is reported as.
///
/// `ServerSideEncryptionConfiguration$Rules` is `xmlFlattened` with `xmlName` `Rule`, and
/// `BlockedEncryptionTypes$EncryptionType` likewise with `xmlName` `EncryptionType`
/// (`smithy_query(query="shape",shape="ServerSideEncryptionConfiguration"` and
/// `…shape="BlockedEncryptionTypes"`) — so each list's values are DIRECT children of their
/// container, with no intermediate list element between them.
const CONFIGURATION_ELEMENT: &str = "ServerSideEncryptionConfiguration";
const RULE_ELEMENT: &str = "Rule";
const ENCRYPTION_TYPE_ELEMENT: &str = "EncryptionType";

/// One element of a reported document: its name, the text it carries itself, and its DIRECT
/// children in document order.
///
/// A parsed view keyed by element name alone says only that a value appears SOMEWHERE; a
/// client reads the blocked set at a path, so what this suite asserts is where each element
/// sits.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Element {
    name: String,
    text: String,
    children: Vec<Element>,
}

impl Element {
    /// The direct children named `name`, in document order.
    fn children_named(&self, name: &str) -> Vec<&Element> {
        self.children
            .iter()
            .filter(|child| child.name == name)
            .collect()
    }

    /// The names of every direct child, in document order.
    fn child_names(&self) -> Vec<&str> {
        self.children
            .iter()
            .map(|child| child.name.as_str())
            .collect()
    }
}

/// The document a response body carries, as a tree.
///
/// Parsed with `quick-xml`, the way the suite this one shares its harness with reads a
/// response, rather than by slicing the body.
fn document(answer: &Answer) -> Element {
    let body = String::from_utf8_lossy(&answer.body).into_owned();
    let mut reader = Reader::from_str(&body);
    let mut open: Vec<Element> = Vec::new();
    let mut root: Option<Element> = None;
    loop {
        match reader
            .read_event()
            .unwrap_or_else(|error| panic!("the response body must be well-formed XML: {error}"))
        {
            Event::Start(start) => open.push(Element {
                name: String::from_utf8_lossy(start.name().as_ref()).into_owned(),
                text: String::new(),
                children: Vec::new(),
            }),
            // A SELF-CLOSING element is one that opens and closes at once: it belongs in the
            // tree as a childless element rather than being dropped, so an assertion about
            // where an element sits reports the document as it really arrived.
            Event::Empty(empty) => {
                let childless = Element {
                    name: String::from_utf8_lossy(empty.name().as_ref()).into_owned(),
                    text: String::new(),
                    children: Vec::new(),
                };
                match open.last_mut() {
                    Some(parent) => parent.children.push(childless),
                    None => {
                        assert!(
                            root.is_none(),
                            "the body must carry exactly one root element, got a second {:?} \
                             in {body:?}",
                            childless.name
                        );
                        root = Some(childless);
                    }
                }
            }
            Event::Text(found) => {
                let value = found
                    .xml10_content()
                    .unwrap_or_else(|error| panic!("the response body must decode: {error}"))
                    .into_owned();
                if let Some(innermost) = open.last_mut() {
                    innermost.text.push_str(&value);
                }
            }
            // A CDATA section carries the element's text verbatim, so it is text too.
            Event::CData(found) => {
                let value = String::from_utf8_lossy(found.as_ref()).into_owned();
                if let Some(innermost) = open.last_mut() {
                    innermost.text.push_str(&value);
                }
            }
            Event::End(_) => {
                let closed = open
                    .pop()
                    .unwrap_or_else(|| panic!("every end tag must close an element: {body:?}"));
                match open.last_mut() {
                    Some(parent) => parent.children.push(closed),
                    None => {
                        // One document, one root: a second top-level element would otherwise
                        // replace the first and leave the path assertions looking at half a
                        // body.
                        assert!(
                            root.is_none(),
                            "the body must carry exactly one root element, got a second \
                             {:?} in {body:?}",
                            closed.name
                        );
                        root = Some(closed);
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    root.unwrap_or_else(|| panic!("the body must carry one root element, got {body:?}"))
}

/// `GET /?encryption` addressed VIRTUAL-HOSTED-STYLE: the bucket is the first label of the
/// host and the path names it nowhere.
async fn get_encryption_from_host(router: &MultiServiceRouter, bucket: &str) -> Answer {
    send_bytes_to_host(
        router,
        "GET",
        &format!("{bucket}.s3.us-east-1.amazonaws.com"),
        "/?encryption",
        &[("content-type", "application/xml")],
        b"",
    )
    .await
}

/// Assert, on the bytes, that the reported document carries one `Rule` per rule in force and
/// that each of them carries its blocked set as a `BlockedEncryptionTypes` child whose
/// `EncryptionType` values are its own direct children.
fn assert_blocked_set_structure(case: &str, form: &str, read: &Answer, expected: &[Rule]) {
    let reported = document(read);
    assert_eq!(
        reported.name, CONFIGURATION_ELEMENT,
        "case {case:?} ({form}): the report's root must be <{CONFIGURATION_ELEMENT}>, got \
         {:?} in body {:?}",
        reported.name,
        text(read)
    );
    assert_eq!(
        reported.child_names(),
        vec![RULE_ELEMENT; expected.len()],
        "case {case:?} ({form}): the report must carry {} <{RULE_ELEMENT}> child element(s) \
         and nothing else, got {:?} in body {:?}",
        expected.len(),
        reported.child_names(),
        text(read)
    );
    let rules = reported.children_named(RULE_ELEMENT);

    for (position, (rule, expectation)) in rules.iter().zip(expected).enumerate() {
        let blocked = rule.children_named(BLOCKED_ELEMENT);
        let expected_set = expectation
            .blocked_encryption_types
            .clone()
            .unwrap_or_else(|| {
                panic!("case {case:?}: every rule in force carries a blocked set to report")
            });
        assert_eq!(
            blocked.len(),
            1,
            "case {case:?} ({form}): rule {position} must carry exactly one \
             <{BLOCKED_ELEMENT}> child, got {} in body {:?}",
            blocked.len(),
            text(read)
        );

        // Flattened: the values are the container's OWN children, so a client reads them at
        // `…/Rule/BlockedEncryptionTypes/EncryptionType` with no list element in between.
        let container = blocked[0];
        assert_eq!(
            container.child_names(),
            vec![ENCRYPTION_TYPE_ELEMENT; expected_set.len()],
            "case {case:?} ({form}): rule {position}'s <{BLOCKED_ELEMENT}> must carry \
             {} <{ENCRYPTION_TYPE_ELEMENT}> child element(s) and nothing else, got {:?} in \
             body {:?}",
            expected_set.len(),
            container.child_names(),
            text(read)
        );
        // Trimmed: what is asserted is the VALUE each element carries, not whether the
        // serialiser laid the document out with whitespace between its elements.
        let values: Vec<&str> = container
            .children
            .iter()
            .map(|value| value.text.trim())
            .collect();
        assert_eq!(
            values, expected_set,
            "case {case:?} ({form}): rule {position} must block {expected_set:?}, got \
             {values:?} in body {:?}",
            text(read)
        );
    }
}

/// RULE: the blocked set in force on the bucket is reported INSIDE each rule, as a
/// `BlockedEncryptionTypes` element whose `EncryptionType` values are its direct children —
/// whatever the bucket's configuration state, and whichever address form asked for it.
///
/// The rows are the table [`rows`] already spans (configured or not, which algorithm, which
/// Bucket Key setting, which blocked set, how many rules), read in both address forms: where
/// a request was addressed is not part of what a bucket reports.
#[tokio::test]
async fn every_reported_rule_carries_its_blocked_set_as_its_own_flattened_values() {
    let router = router();
    for row in rows() {
        bucket(&router, row.bucket, row.case).await;
        if let Some(rules) = &row.configured {
            let put = put_encryption(&router, row.bucket, rules).await;
            assert_eq!(
                put.status, 200,
                "case {:?}: PutBucketEncryption must answer 200, got {} with body {:?}",
                row.case,
                put.status,
                text(&put)
            );
        }
        let expected = expected_of(&row);

        let path_style = get_encryption(&router, row.bucket).await;
        let host_style = get_encryption_from_host(&router, row.bucket).await;
        for (form, read) in [("path-style", &path_style), ("host-style", &host_style)] {
            assert_eq!(
                read.status, 200,
                "case {:?} ({form}): GetBucketEncryption must answer 200, got {} with body {:?}",
                row.case,
                read.status,
                text(read)
            );
            assert_blocked_set_structure(row.case, form, read, &expected);
        }
        assert_eq!(
            document(&path_style),
            document(&host_style),
            "case {:?}: both address forms must report the same document, got {:?} and {:?}",
            row.case,
            text(&path_style),
            text(&host_style)
        );
    }
}

/// RULE, on the two configuration states no put leaves behind: a bucket no put has
/// configured and a bucket whose configuration was DELETED both report the blocked set the
/// bucket carries anyway, in the same place.
#[tokio::test]
async fn a_bucket_with_no_configuration_of_its_own_still_reports_its_blocked_set_there() {
    let router = router();
    let name = "members-structure-after-reset";
    let case = "a bucket configured and then reset";
    bucket(&router, name, case).await;

    // Never configured.
    let fresh = get_encryption(&router, name).await;
    assert_eq!(
        fresh.status, 200,
        "case {case:?}: the first read must answer 200, got {} with body {:?}",
        fresh.status,
        text(&fresh)
    );
    assert_blocked_set_structure(case, "never configured", &fresh, &[base_default()]);

    // Configured with a rule that names a block of its own, then reset.
    let configured = vec![Rule::by_default(SERVER_SIDE_ENCRYPTION_VALUES[0])
        .blocking(&[ENCRYPTION_TYPE_VALUES[0]])];
    let put = put_encryption(&router, name, &configured).await;
    assert_eq!(
        put.status, 200,
        "case {case:?}: PutBucketEncryption must answer 200, got {} with body {:?}",
        put.status,
        text(&put)
    );
    let reset = delete_encryption(&router, name).await;
    assert_eq!(
        reset.status, 204,
        "case {case:?}: DeleteBucketEncryption must answer the modelled 204, got {} with \
         body {:?}",
        reset.status,
        text(&reset)
    );

    let after = get_encryption(&router, name).await;
    assert_eq!(
        after.status, 200,
        "case {case:?}: the read after the reset must answer 200, got {} with body {:?}",
        after.status,
        text(&after)
    );
    assert_blocked_set_structure(case, "after a reset", &after, &[base_default()]);
    let parsed = reported_rules(&after);
    assert_eq!(
        parsed,
        vec![base_default()],
        "case {case:?}: the bucket after a reset must report {:?}, got {parsed:?} (body {:?})",
        vec![base_default()],
        text(&after)
    );
}

/// RULE, over the bucket's OTHER configuration: what the encryption read reports is the
/// encryption state alone, so a bucket hardened elsewhere — versioning enabled and every
/// published public-access switch on — reports the same blocked set in the same place as one
/// left as `CreateBucket` made it.
#[tokio::test]
async fn a_bucket_hardened_elsewhere_reports_the_same_blocked_set() {
    let router = router();
    let name = "members-hardened";
    let case = "a versioned bucket with every public-access switch on";
    bucket(&router, name, case).await;

    set_versioning(&router, name, "Enabled").await;
    let switches: Vec<(&str, bool)> = SETTINGS.iter().map(|setting| (*setting, true)).collect();
    let blocked_public = put_public_access_block(&router, name, &switches, &[]).await;
    assert_eq!(
        blocked_public.status, 200,
        "case {case:?}: PutPublicAccessBlock must answer 200, got {} with body {:?}",
        blocked_public.status,
        text(&blocked_public)
    );

    // Unconfigured encryption on a hardened bucket, then an explicit rule on it: the blocked
    // set is reported in both states, as it is on a bucket nothing else was done to.
    let unconfigured = get_encryption(&router, name).await;
    assert_eq!(
        unconfigured.status, 200,
        "case {case:?}: GetBucketEncryption must answer 200, got {} with body {:?}",
        unconfigured.status,
        text(&unconfigured)
    );
    assert_blocked_set_structure(
        case,
        "hardened and unconfigured",
        &unconfigured,
        &[base_default()],
    );

    let configured = vec![Rule::by_default(DEFAULT_SSE_ALGORITHM)];
    let put = put_encryption(&router, name, &configured).await;
    assert_eq!(
        put.status, 200,
        "case {case:?}: PutBucketEncryption must answer 200, got {} with body {:?}",
        put.status,
        text(&put)
    );
    let read = get_encryption(&router, name).await;
    assert_eq!(
        read.status, 200,
        "case {case:?}: the read after the put must answer 200, got {} with body {:?}",
        read.status,
        text(&read)
    );
    // What the put is owed back, through the reader the encryption suite already owns:
    // the Bucket Key materialised at its documented off state and the bucket-level blocked
    // set the rule named nothing about.
    let expected = as_reported(&configured);
    assert_blocked_set_structure(case, "hardened and configured", &read, &expected);
    let parsed = reported_rules(&read);
    assert_eq!(
        parsed, expected,
        "case {case:?}: the hardened bucket must report {expected:?}, got {parsed:?} (body \
         {:?})",
        text(&read)
    );
}
