// SPDX-License-Identifier: CC0-1.0

//! What a CloudFront public key upload and a response headers policy create ADMIT, and what a
//! published response headers policy reports — end to end over HTTP.
//!
//! Every assertion goes through the router, so each case exercises routing, restXml
//! (de)serialization, the handler, the persisted state and serialization — the same path a
//! client takes. The rules under test:
//!
//! * a configuration member CloudFront bounds is judged BEFORE anything is minted, and the
//!   refusal names the member as CloudFront names it: `PublicKeyConfig$Name` is bounded in
//!   length (`state::MAX_PUBLIC_KEY_NAME`) and built from the characters CloudFront admits in a
//!   name (`state::is_public_key_name`);
//! * `PublicKeyConfig$EncodedKey` carries the RSA public key of a signer's key pair, of the one
//!   width such a pair has (`state::PUBLIC_KEY_RSA_MODULUS_BITS`), so a readable RSA key of any
//!   other width is refused with the sentence CloudFront states for material it cannot take;
//! * a response headers policy adds at most the published quota of response headers of its own
//!   (`state::MAX_CUSTOM_HEADERS_IN_RESPONSE_HEADERS_POLICY`) and may not take off the headers
//!   CloudFront keeps (`state::HEADERS_A_RESPONSE_HEADERS_POLICY_CANNOT_REMOVE`);
//! * a refused create mints NOTHING, so the name it carried is still free: the next create
//!   under that name is served, with the policy, the `ETag` and the `Location` the operation
//!   binds;
//! * a policy AWS published reports the values AWS publishes for it — the fixed
//!   `LastModifiedTime` every published policy of every family carries, and the
//!   `SecurityHeadersConfig` of the published security headers.
//!
//! The rows differ along what each rule generalises over: the LENGTH of a name (inside the
//! bound, on it, one past it and far past it), the characters it is built from, the WIDTH of
//! the submitted key (the admitted width, one octet either side of it, and the widths a client
//! is most likely to submit instead), the COUNT of headers a policy adds (none, the wrapper
//! with none, one, the quota, one past it and far past it), WHICH kept header a policy asks to
//! remove and in which letter case, and every published policy of all three families.
//!
//! The harnesses (the router, the request helpers, the configuration writers) are the
//! lifecycle suites', reused rather than copied.

use service_com_amazonaws_cloudfront_cloudfront2020_05_31::state::{
    managed_policy_etag, HEADERS_A_RESPONSE_HEADERS_POLICY_CANNOT_REMOVE,
    INVALID_ENCODED_KEY_MESSAGE, MANAGED_CACHE_POLICIES, MANAGED_ORIGIN_REQUEST_POLICIES,
    MANAGED_RESPONSE_HEADERS_POLICIES, MAX_CUSTOM_HEADERS_IN_RESPONSE_HEADERS_POLICY,
    MAX_PUBLIC_KEY_NAME, PUBLIC_KEY_RSA_MODULUS_BITS,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{call, emulator, API_VERSION};
use crate::public_key_lifecycle::{create_key, get_key, pem, pem_of_modulus_octets, Key};
use crate::response_headers_policy_lifecycle::{create_policy, get_policy, Policy};
use crate::response_headers_policy_update::update_policy;

/// The sentence CloudFront refuses a public key name longer than the bound with: fixed text
/// naming the parameter the name arrived in. Spelled out here rather than read off the crate's
/// own helper, so the WORDING is what these rows are held to.
const NAME_TOO_BIG: &str = "The parameter PublicKeyName is too big.";

/// The sentence CloudFront refuses a public key name built from characters it does not admit
/// with, spelled out here for the same reason — a different naming of the same member from
/// [`NAME_TOO_BIG`]'s, which is why both are pinned.
const NAME_ILLEGAL_CHARACTERS: &str = "The parameter PublicKey:Name contains illegal characters.";

/// The sentence CloudFront refuses a policy adding more response headers than the published
/// quota with, spelled out here for the same reason.
const TOO_MANY_CUSTOM_HEADERS: &str =
    "Your request contains more headers in the Response Headers Policy than are allowed.";

/// The instant AWS publishes as the `LastModifiedTime` of a policy it owns: the start of the
/// Unix epoch, the same in every account and on every read.
const PUBLISHED_LAST_MODIFIED: &str = "1970-01-01T00:00:00Z";

/// The ids `ListPublicKeys` reports, which is what says whether a refused upload stored a key.
async fn listed_key_ids(router: &MultiServiceRouter) -> Vec<String> {
    let listed = call(
        router,
        "GET",
        &format!("/{API_VERSION}/public-key"),
        None,
        None,
    )
    .await;
    assert_eq!(
        listed.status, 200,
        "expected HTTP 200 from ListPublicKeys, got {} with {}",
        listed.status, listed.raw
    );
    listed
        .root()
        .children_named("Items")
        .first()
        .map(|items| {
            items
                .children_named("PublicKeySummary")
                .iter()
                .filter_map(|summary| summary.text_at("Id").map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// One row of the public key NAME table: the name submitted, and the sentence the upload is
/// refused with — or `None` when the name is one CloudFront admits.
fn name_rows() -> Vec<(String, String, Option<&'static str>)> {
    vec![
        ("the shortest name".to_string(), "k".to_string(), None),
        (
            "every character class the alphabet admits".to_string(),
            "Signer_42-key".to_string(),
            None,
        ),
        (
            "a name exactly on the published bound".to_string(),
            "n".repeat(MAX_PUBLIC_KEY_NAME),
            None,
        ),
        (
            "a name one character past the bound".to_string(),
            "n".repeat(MAX_PUBLIC_KEY_NAME + 1),
            Some(NAME_TOO_BIG),
        ),
        (
            "a name far past the bound".to_string(),
            "n".repeat(MAX_PUBLIC_KEY_NAME * 4),
            Some(NAME_TOO_BIG),
        ),
        (
            "a name carrying a space".to_string(),
            "signer key".to_string(),
            Some(NAME_ILLEGAL_CHARACTERS),
        ),
        (
            "a name carrying a dot".to_string(),
            "signer.key".to_string(),
            Some(NAME_ILLEGAL_CHARACTERS),
        ),
        (
            "a name carrying a path separator".to_string(),
            "signer/key".to_string(),
            Some(NAME_ILLEGAL_CHARACTERS),
        ),
        (
            "a name carrying an XML metacharacter".to_string(),
            "signer<key>".to_string(),
            Some(NAME_ILLEGAL_CHARACTERS),
        ),
        (
            "a name carrying a character outside ASCII".to_string(),
            "signeré".to_string(),
            Some(NAME_ILLEGAL_CHARACTERS),
        ),
    ]
}

/// The name a public key is identified by is bounded in length and built from the characters
/// CloudFront admits in one: a name outside either rule is refused with the sentence CloudFront
/// states for that rule and stores no key, while a name inside both is stored and read back.
#[tokio::test]
async fn a_public_key_name_outside_the_rules_is_refused() {
    let router = emulator();

    for (index, (case, name, refusal)) in name_rows().into_iter().enumerate() {
        let before = listed_key_ids(&router).await;
        let key = Key {
            case: case.clone(),
            caller_reference: format!("name-rule-reference-{index}"),
            name: name.clone(),
            encoded_key: pem(index, 'N'),
            comment: Some("the public key name suite".to_string()),
        };
        let answer = create_key(&router, &key.xml()).await;

        let Some(sentence) = refusal else {
            assert_eq!(
                answer.status, 201,
                "case {case}: expected HTTP 201, got {} with {}",
                answer.status, answer.raw
            );
            let id = answer
                .root()
                .find_text("Id")
                .unwrap_or_else(|| panic!("case {case}: the stored key reports an id"))
                .to_string();
            let read = get_key(&router, &id).await;
            assert_eq!(
                read.root().find_text("Name"),
                Some(name.as_str()),
                "case {case}: GetPublicKey reported {:?} in {}",
                read.root().find_text("Name"),
                read.raw
            );
            continue;
        };

        assert_eq!(
            answer.status, 400,
            "case {case}: expected HTTP 400, got {} with {}",
            answer.status, answer.raw
        );
        assert_eq!(
            answer.error_code(),
            "InvalidArgument",
            "case {case}: expected InvalidArgument, got {} in {}",
            answer.error_code(),
            answer.raw
        );
        assert_eq!(
            answer.root().find_text("Message"),
            Some(sentence),
            "case {case}: expected the sentence {sentence:?}, got {:?}",
            answer.root().find_text("Message")
        );
        let after = listed_key_ids(&router).await;
        assert_eq!(after, before, "case {case}: the refused upload stored a key");
    }
}

/// One row of the key WIDTH table: how many octets wide the submitted modulus is, and whether
/// CloudFront admits a key of that width.
fn width_rows() -> Vec<(String, usize, bool)> {
    let admitted = PUBLIC_KEY_RSA_MODULUS_BITS / 8;
    vec![
        ("the admitted width".to_string(), admitted, true),
        (
            "one octet narrower than the admitted width".to_string(),
            admitted - 1,
            false,
        ),
        (
            "one octet wider than the admitted width".to_string(),
            admitted + 1,
            false,
        ),
        // The widths a client is most likely to submit instead: the halves and doubles of a
        // signer's key pair that other services take.
        ("a 512-bit modulus".to_string(), 64, false),
        ("a 1024-bit modulus".to_string(), 128, false),
        ("a 3072-bit modulus".to_string(), 384, false),
        ("a 4096-bit modulus".to_string(), 512, false),
    ]
}

/// The material an upload carries is the RSA public key of a signer's key pair, of the one
/// width such a pair has: a readable RSA key of any other width is refused with the sentence
/// CloudFront states for material it cannot take and stores nothing, while one of the admitted
/// width is stored and read back.
#[tokio::test]
async fn only_a_key_of_the_admitted_width_is_admitted() {
    let router = emulator();

    for (index, (case, octets, admitted)) in width_rows().into_iter().enumerate() {
        let before = listed_key_ids(&router).await;
        let material = pem_of_modulus_octets(octets, index, 'W');
        let key = Key {
            case: case.clone(),
            caller_reference: format!("width-reference-{index}"),
            name: format!("width-key-{index}"),
            encoded_key: material.clone(),
            comment: None,
        };
        let answer = create_key(&router, &key.xml()).await;

        if !admitted {
            assert_eq!(
                answer.status, 400,
                "case {case}: expected HTTP 400, got {} with {}",
                answer.status, answer.raw
            );
            assert_eq!(
                answer.error_code(),
                "InvalidArgument",
                "case {case}: expected InvalidArgument, got {} in {}",
                answer.error_code(),
                answer.raw
            );
            assert_eq!(
                answer.root().find_text("Message"),
                Some(INVALID_ENCODED_KEY_MESSAGE),
                "case {case}: expected the sentence {INVALID_ENCODED_KEY_MESSAGE:?}, got {:?}",
                answer.root().find_text("Message")
            );
            let after = listed_key_ids(&router).await;
            assert_eq!(
                after, before,
                "case {case}: the refused upload stored a key"
            );
            continue;
        }

        assert_eq!(
            answer.status, 201,
            "case {case}: expected HTTP 201, got {} with {}",
            answer.status, answer.raw
        );
        let id = answer
            .root()
            .find_text("Id")
            .unwrap_or_else(|| panic!("case {case}: the stored key reports an id"))
            .to_string();
        let read = get_key(&router, &id).await;
        assert_eq!(
            read.root().find_text("EncodedKey"),
            Some(material.as_str()),
            "case {case}: GetPublicKey reported {:?} in {}",
            read.root().find_text("EncodedKey"),
            read.raw
        );
    }
}

/// `CustomHeadersConfig.Items` of `count` headers, each distinct.
fn custom_headers(count: usize) -> Vec<(String, String, bool)> {
    (0..count)
        .map(|index| {
            (
                format!("x-added-{index}"),
                format!("value {index}"),
                index % 2 == 0,
            )
        })
        .collect()
}

/// One row of the custom-header COUNT table: how many headers of its own the policy adds, and
/// whether CloudFront admits a policy adding that many.
fn custom_header_rows() -> Vec<(String, Option<usize>, bool)> {
    let quota = MAX_CUSTOM_HEADERS_IN_RESPONSE_HEADERS_POLICY;
    vec![
        ("no custom headers wrapper at all".to_string(), None, true),
        ("the wrapper carrying no headers".to_string(), Some(0), true),
        ("one header".to_string(), Some(1), true),
        ("headers up to the published quota".to_string(), Some(quota), true),
        (
            "one header past the published quota".to_string(),
            Some(quota + 1),
            false,
        ),
        (
            "far past the published quota".to_string(),
            Some(quota * 3),
            false,
        ),
    ]
}

/// The `(Header, Value)` pairs a read reports under `CustomHeadersConfig`, in page order.
fn stored_custom_headers(read: &crate::distribution_lifecycle::Answer) -> Vec<(String, String)> {
    read.root()
        .at("ResponseHeadersPolicyConfig/CustomHeadersConfig/Items")
        .map(|items| {
            items
                .children_named("ResponseHeadersPolicyCustomHeader")
                .iter()
                .map(|entry| {
                    (
                        entry.text_at("Header").unwrap_or_default().to_string(),
                        entry.text_at("Value").unwrap_or_default().to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// A policy adding more response headers of its own than the published quota is refused with
/// the error the model binds for that quota, and mints nothing — so the NAME it carried is
/// still free, and a create under that same name with a list inside the quota is served with
/// the policy, the `ETag` and the `Location` the operation binds.
#[tokio::test]
async fn a_policy_past_the_custom_header_quota_is_refused_and_leaves_its_name_free() {
    let router = emulator();

    for (index, (case, count, admitted)) in custom_header_rows().into_iter().enumerate() {
        let name = format!("custom-header-quota-{index}");
        let policy = Policy {
            case: "the custom header quota suite",
            name: name.clone(),
            comment: Some(format!("a policy adding {count:?} headers")),
            custom_headers: count.map(custom_headers),
            ..Policy::default()
        };
        let answer = create_policy(&router, &policy.xml()).await;

        if admitted {
            assert_eq!(
                answer.status, 201,
                "case {case}: expected HTTP 201, got {} with {}",
                answer.status, answer.raw
            );
            // A row inside the quota is STORED, down to the list it submitted: the boundary row
            // (exactly the quota) is what makes this more than an admission check.
            let id = answer
                .root()
                .find_text("Id")
                .unwrap_or_else(|| panic!("case {case}: the stored policy reports an id"))
                .to_string();
            let read = get_policy(&router, &id).await;
            let reported = stored_custom_headers(&read);
            let expected: Vec<(String, String)> = count
                .map(custom_headers)
                .unwrap_or_default()
                .into_iter()
                .map(|(header, value, _)| (header, value))
                .collect();
            assert_eq!(
                reported, expected,
                "case {case}: expected the stored custom headers {expected:?}, got {reported:?} \
                 in {}",
                read.raw
            );
            continue;
        }

        assert_eq!(
            answer.status, 400,
            "case {case}: expected HTTP 400, got {} with {}",
            answer.status, answer.raw
        );
        assert_eq!(
            answer.error_code(),
            "TooManyCustomHeadersInResponseHeadersPolicy",
            "case {case}: expected TooManyCustomHeadersInResponseHeadersPolicy, got {} in {}",
            answer.error_code(),
            answer.raw
        );
        assert_eq!(
            answer.root().find_text("Message"),
            Some(TOO_MANY_CUSTOM_HEADERS),
            "case {case}: expected the sentence {TOO_MANY_CUSTOM_HEADERS:?}, got {:?}",
            answer.root().find_text("Message")
        );
        assert!(
            answer.root().descendant("ResponseHeadersPolicy").is_none(),
            "case {case}: the refusal carries a policy: {}",
            answer.raw
        );

        // The refusal minted nothing, so the name it carried is free: the create that follows
        // under that very name is served, and carries everything the operation binds.
        let repaired = Policy {
            custom_headers: Some(custom_headers(1)),
            ..policy.clone()
        };
        let served = create_policy(&router, &repaired.xml()).await;
        assert_eq!(
            served.status, 201,
            "case {case}: expected the name to be free, got {} with {}",
            served.status, served.raw
        );
        let id = served
            .root()
            .find_text("Id")
            .unwrap_or_else(|| panic!("case {case}: the stored policy reports an id"))
            .to_string();
        assert!(
            !served.header("etag").is_empty(),
            "case {case}: the create reports no ETag: {}",
            served.raw
        );
        assert!(
            served.header("location").ends_with(&id),
            "case {case}: expected a Location naming {id}, got {:?}",
            served.header("location")
        );
        let read = get_policy(&router, &id).await;
        assert_eq!(
            read.root().find_text("Name"),
            Some(name.as_str()),
            "case {case}: GetResponseHeadersPolicy reported {:?} in {}",
            read.root().find_text("Name"),
            read.raw
        );
    }
}

/// A policy may take headers off a response, but not the ones CloudFront keeps: a
/// configuration naming one of those is refused with the sentence CloudFront states for the
/// list it arrived in — whatever letter case the header is spelled in, since a header name is
/// case-insensitive — and mints nothing, so the name it carried is still free.
#[tokio::test]
async fn a_policy_removing_a_header_cloudfront_keeps_is_refused() {
    let router = emulator();

    // Every header of the published set, each spelled in a different letter case: the
    // canonical one, all lower case and all upper case, cycled across the set so the rule is
    // asserted over every entry AND over every spelling.
    let spellings: [fn(&str) -> String; 3] = [
        |header| header.to_string(),
        |header| header.to_lowercase(),
        |header| header.to_uppercase(),
    ];
    for (index, kept) in HEADERS_A_RESPONSE_HEADERS_POLICY_CANNOT_REMOVE
        .iter()
        .enumerate()
    {
        let submitted = spellings[index % spellings.len()](kept);
        let case = format!("removing {submitted}");
        let name = format!("unremovable-header-{index}");
        let policy = Policy {
            case: "the unremovable header suite",
            name: name.clone(),
            // One header the policy may remove sits ahead of the kept one, so what is refused
            // is the kept header rather than the list carrying any header at all.
            remove_headers: Some(vec!["x-removable".to_string(), submitted.clone()]),
            ..Policy::default()
        };
        let answer = create_policy(&router, &policy.xml()).await;
        assert_eq!(
            answer.status, 400,
            "case {case}: expected HTTP 400, got {} with {}",
            answer.status, answer.raw
        );
        assert_eq!(
            answer.error_code(),
            "InvalidArgument",
            "case {case}: expected InvalidArgument, got {} in {}",
            answer.error_code(),
            answer.raw
        );
        let expected = format!("The parameter RemoveHeaders contains {submitted} that is not allowed.");
        assert_eq!(
            answer.root().find_text("Message"),
            Some(expected.as_str()),
            "case {case}: expected the sentence {expected:?}, got {:?}",
            answer.root().find_text("Message")
        );

        // Nothing was minted, so the name is free for the create that drops the kept header.
        let repaired = Policy {
            remove_headers: Some(vec!["x-removable".to_string()]),
            ..policy.clone()
        };
        let served = create_policy(&router, &repaired.xml()).await;
        assert_eq!(
            served.status, 201,
            "case {case}: expected the name to be free, got {} with {}",
            served.status, served.raw
        );
    }
}

/// The SAME two rules hold on the payload an update states, and a refused update leaves the
/// stored policy exactly as it was: the configuration it was serving and the version token a
/// later `If-Match` is checked against.
#[tokio::test]
async fn a_refused_update_changes_neither_the_configuration_nor_the_version() {
    let router = emulator();

    // What every row starts from: a policy CloudFront admits, carrying one header of its own
    // and one header it may remove, so a refused update has something to have changed.
    let rows: Vec<(String, Policy)> = vec![
        (
            "an update past the custom header quota".to_string(),
            Policy {
                custom_headers: Some(custom_headers(
                    MAX_CUSTOM_HEADERS_IN_RESPONSE_HEADERS_POLICY + 1,
                )),
                ..Policy::default()
            },
        ),
        (
            "an update removing a header CloudFront keeps".to_string(),
            Policy {
                remove_headers: Some(vec![
                    "x-removable".to_string(),
                    HEADERS_A_RESPONSE_HEADERS_POLICY_CANNOT_REMOVE[0].to_string(),
                ]),
                ..Policy::default()
            },
        ),
    ];

    for (index, (case, change)) in rows.into_iter().enumerate() {
        let name = format!("refused-update-{index}");
        let stored = Policy {
            case: "the refused update suite",
            name: name.clone(),
            comment: Some("the configuration the update must not disturb".to_string()),
            custom_headers: Some(custom_headers(1)),
            remove_headers: Some(vec!["x-removable".to_string()]),
            ..Policy::default()
        };
        let created = create_policy(&router, &stored.xml()).await;
        assert_eq!(
            created.status, 201,
            "case {case}: expected HTTP 201 from the create, got {} with {}",
            created.status, created.raw
        );
        let id = created
            .root()
            .find_text("Id")
            .unwrap_or_else(|| panic!("case {case}: the stored policy reports an id"))
            .to_string();
        let version = created.header("etag").to_string();
        let before = get_policy(&router, &id).await;

        // The update states the offending configuration under the version the caller read, so
        // nothing but the rule under test can refuse it.
        let attempted = Policy {
            name: name.clone(),
            ..change
        };
        let refused = update_policy(&router, &id, &attempted.xml(), Some(&version)).await;
        assert_eq!(
            refused.status, 400,
            "case {case}: expected HTTP 400 from the update, got {} with {}",
            refused.status, refused.raw
        );

        // The record is exactly what it was, down to the version token a later If-Match is
        // checked against.
        let after = get_policy(&router, &id).await;
        assert_eq!(
            after.header("etag"),
            version,
            "case {case}: expected the version token {version:?} to be untouched, got {:?}",
            after.header("etag")
        );
        assert_eq!(
            stored_custom_headers(&after),
            stored_custom_headers(&before),
            "case {case}: the refused update changed the stored custom headers: {}",
            after.raw
        );
        assert_eq!(
            after.root().find_text("Comment"),
            before.root().find_text("Comment"),
            "case {case}: the refused update changed the stored comment: {}",
            after.raw
        );
    }
}

/// A policy removing only headers CloudFront lets a policy remove is served and reports that
/// list back — the other side of the rule above.
#[tokio::test]
async fn a_policy_removing_headers_cloudfront_allows_is_served() {
    let router = emulator();

    for (index, headers) in [
        vec!["Server".to_string()],
        vec!["X-Powered-By".to_string(), "x-internal".to_string()],
        // A header whose name only STARTS like one of the kept ones: the rule is about the
        // header, not about the text it begins with.
        vec!["Content-Encoding-Hint".to_string()],
    ]
    .into_iter()
    .enumerate()
    {
        let case = format!("removing {headers:?}");
        let policy = Policy {
            case: "the removable header suite",
            name: format!("removable-header-{index}"),
            remove_headers: Some(headers.clone()),
            ..Policy::default()
        };
        let answer = create_policy(&router, &policy.xml()).await;
        assert_eq!(
            answer.status, 201,
            "case {case}: expected HTTP 201, got {} with {}",
            answer.status, answer.raw
        );
        let id = answer
            .root()
            .find_text("Id")
            .unwrap_or_else(|| panic!("case {case}: the stored policy reports an id"))
            .to_string();
        let read = get_policy(&router, &id).await;
        let reported: Vec<String> = read
            .root()
            .at("ResponseHeadersPolicyConfig/RemoveHeadersConfig/Items")
            .map(|items| {
                items
                    .children_named("ResponseHeadersPolicyRemoveHeader")
                    .iter()
                    .filter_map(|entry| entry.text_at("Header").map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(
            reported, headers,
            "case {case}: expected the stored list {headers:?}, got {reported:?} in {}",
            read.raw
        );
    }
}

/// Every policy AWS published reports the `LastModifiedTime` AWS publishes for it: a fixed
/// instant rather than a clock value, the same for every published policy of every family and
/// on every read.
#[tokio::test]
async fn every_published_policy_reports_the_published_last_modified_time() {
    let router = emulator();

    let families = [
        (
            "response-headers-policy",
            MANAGED_RESPONSE_HEADERS_POLICIES
                .iter()
                .map(|managed| managed.id)
                .collect::<Vec<_>>(),
        ),
        (
            "cache-policy",
            MANAGED_CACHE_POLICIES
                .iter()
                .map(|managed| managed.id)
                .collect(),
        ),
        (
            "origin-request-policy",
            MANAGED_ORIGIN_REQUEST_POLICIES
                .iter()
                .map(|managed| managed.id)
                .collect(),
        ),
    ];

    for (path, ids) in families {
        for id in ids {
            let case = format!("{path}/{id}");
            let read = call(
                &router,
                "GET",
                &format!("/{API_VERSION}/{path}/{id}"),
                None,
                None,
            )
            .await;
            assert_eq!(
                read.status, 200,
                "case {case}: expected HTTP 200, got {} with {}",
                read.status, read.raw
            );
            assert_eq!(
                read.root().find_text("LastModifiedTime"),
                Some(PUBLISHED_LAST_MODIFIED),
                "case {case}: expected LastModifiedTime {PUBLISHED_LAST_MODIFIED:?}, got {:?}",
                read.root().find_text("LastModifiedTime")
            );
            // The version token of a published policy is derived rather than drawn, so the
            // same read reports the same one in any process.
            assert_eq!(
                read.header("etag"),
                managed_policy_etag(id),
                "case {case}: expected the derived version token, got {:?}",
                read.header("etag")
            );
        }
    }
}

/// A published policy that adds the security headers reports the directives AWS publishes for
/// them, down to whether CloudFront overrides what the origin already set: the published set
/// overrides the origin's `X-Content-Type-Options`.
#[tokio::test]
async fn a_published_security_headers_policy_overrides_the_origin_content_type_options() {
    let router = emulator();

    for managed in MANAGED_RESPONSE_HEADERS_POLICIES {
        if !managed.security_headers {
            continue;
        }
        let case = managed.name;
        let read = get_policy(&router, managed.id).await;
        assert_eq!(
            read.status, 200,
            "case {case}: expected HTTP 200, got {} with {}",
            read.status, read.raw
        );
        let reported = read
            .root()
            .text_at("ResponseHeadersPolicyConfig/SecurityHeadersConfig/ContentTypeOptions/Override");
        assert_eq!(
            reported,
            Some("true"),
            "case {case}: expected ContentTypeOptions.Override true, got {reported:?} in {}",
            read.raw
        );
    }
}

/// The published policies are read the same way by the CONFIGURATION reader of the family, so
/// a client reading either report sees one published policy.
#[tokio::test]
async fn the_configuration_reader_reports_the_same_published_policy() {
    let router = emulator();

    for managed in MANAGED_RESPONSE_HEADERS_POLICIES {
        let case = managed.name;
        let config = call(
            &router,
            "GET",
            &format!("/{API_VERSION}/response-headers-policy/{}/config", managed.id),
            None,
            None,
        )
        .await;
        assert_eq!(
            config.status, 200,
            "case {case}: expected HTTP 200, got {} with {}",
            config.status, config.raw
        );
        let read = get_policy(&router, managed.id).await;
        let expected = read
            .root()
            .at("ResponseHeadersPolicyConfig")
            .map(|node| node.text_at("Name").unwrap_or_default().to_string());
        assert_eq!(
            config.root().text_at("Name").map(str::to_string),
            expected,
            "case {case}: the two readers disagree about the published policy: {} and {}",
            config.raw,
            read.raw
        );
        if managed.security_headers {
            let reported = config.root().text_at("SecurityHeadersConfig/ContentTypeOptions/Override");
            assert_eq!(
                reported,
                Some("true"),
                "case {case}: expected ContentTypeOptions.Override true from the configuration \
                 reader, got {reported:?} in {}",
                config.raw
            );
        }
    }
}

/// A sanity control over the harness: the `Answer` of a create carries the headers the
/// operation binds, so the assertions above read real header values rather than empty ones.
#[tokio::test]
async fn a_served_policy_create_carries_its_etag_and_location() {
    let router = emulator();

    let policy = Policy {
        case: "the header control",
        name: "admission-header-control".to_string(),
        ..Policy::default()
    };
    let answer = create_policy(&router, &policy.xml()).await;
    assert_eq!(
        answer.status, 201,
        "case the header control: expected HTTP 201, got {} with {}",
        answer.status, answer.raw
    );
    let id = answer
        .root()
        .find_text("Id")
        .expect("the stored policy reports an id")
        .to_string();
    assert!(
        !answer.header("etag").is_empty(),
        "case the header control: the create reports no ETag: {}",
        answer.raw
    );
    assert!(
        answer.header("location").ends_with(&id),
        "case the header control: expected a Location naming {id}, got {:?}",
        answer.header("location")
    );
}
