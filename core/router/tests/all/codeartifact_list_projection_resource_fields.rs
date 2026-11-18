// SPDX-License-Identifier: CC0-1.0

//! What a CodeArtifact LIST read projects for each entry it reports, over the wire.
//!
//! The rule under test: **a list read projects, for every entry, the values stored on that
//! entry's OWN record — so a listing holding many resources reports each resource's own
//! fields and substitutes nothing for any of them.** Two members this suite holds that rule
//! to, in listings that hold several resources at once:
//!
//! * `DomainSummary$encryptionKey`, "The key used to encrypt the domain"
//!   (`smithy_query(query="shape", shape="DomainSummary")`), is the key the domain was
//!   created against, resolved to a key ARN. `CreateDomain$encryptionKey` is the key the
//!   CALLER names and "can be a key ID, a key Amazon Resource Name (ARN), a key alias, or a
//!   key alias ARN" (`smithy_query(query="doc", shape="CreateDomainRequest",
//!   member="encryptionKey")`), so four spellings of one key all record that one key; a
//!   create naming no key records the Amazon Web Services managed key of the account and
//!   Region, which is a key KMS itself holds. Every read of that domain — the create's own
//!   answer, `DescribeDomain`, each page of `ListDomains` — reports the same string.
//! * `PackageGroupSummary$description`, "The description of the package group", is the
//!   description that group's record carries: for a group a CALLER created, the one that
//!   caller sent (and no member at all when it sent none); for the `/*` group the SERVICE
//!   creates with every domain, the fixed text the service assigns it, identical in every
//!   domain and derived from nothing in the request.
//!
//! Why this suite lives here rather than in the service crate: the key a domain is encrypted
//! with is KMS's own resource, so the rule only holds if two crates agree — CodeArtifact
//! resolves the caller's reference, and KMS answers for the key it resolved to.
//!
//! What the crate-local suites already pin, and what is left to this one:
//! `services/service-com-amazonaws-codeartifact-codeartifactcontrolplaneservice/tests/all/domain_resource_fields.rs`
//! pins the key derivation one domain at a time (create, read, delete, each domain released
//! before the next), and `.../root_package_group_description.rs` the root group's description
//! across the projections that report it. Neither reads a listing that holds SEVERAL
//! resources bound to DIFFERENT values at once, which is where a projection that substitutes
//! one record's value for another's shows: this suite holds every key spelling, two different
//! keys and every description in ONE store, addresses each entry by its ARN, and repeats the
//! reading over the pages of a paginated walk. What a SINGLE read of one resource reports
//! (`DescribeDomain`, `DescribePackageGroup`, `DeleteDomain`) is left to those suites, which
//! already pin it.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request below is signed for, and therefore the Region of every ARN the
/// answers carry.
const REGION: &str = "us-west-2";

/// The account id every ARN this emulator mints carries, and therefore the `owner` of every
/// domain created below.
const ACCOUNT: &str = "000000000000";

/// The alias of the Amazon Web Services managed key CodeArtifact encrypts a domain with when
/// the create names no key: `aws/<service-name>` is the form of such a predefined alias,
/// under this service's own name.
const MANAGED_KEY_ALIAS: &str = "alias/aws/codeartifact";

/// The description CodeArtifact itself assigns the implicit root package group of a domain.
/// Fixed text: it carries no part of the request, the account, the domain or the clock.
const ROOT_DESCRIPTION: &str = "Default root package group";

/// The pattern of that implicit root group, which covers the whole domain.
const ROOT_PATTERN: &str = "/*";

/// CodeArtifact and KMS mounted on ONE engine, as `core/router/src/lib.rs` mounts every
/// service for the binary: the key a domain names has to be resolvable in the store the
/// service that owns keys writes.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Deterministic regardless of the ambient setting: these requests carry a placeholder
    // credential, and verifying a signature is the emulator's job, not a service's.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_codeartifact_codeartifactcontrolplaneservice::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(service_com_amazonaws_kms_trentservice::EmulatedService::new(
        engine,
    ));
    router
}

/// A SigV4 `Authorization` whose credential scope names `service` in [`REGION`], which is
/// what routes the request and what each handler reads its Region from. The signature itself
/// is never checked: the router above runs with request authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000"
    )
}

/// The status and parsed body of one response.
fn answered(response: &http::Response<Bytes>) -> (u16, Value) {
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One CodeArtifact control-plane call over its `restJson1` wire: the method and URI the
/// operation's own `@http` binding declares.
async fn codeartifact(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let payload = match &body {
        Some(document) => Bytes::from(document.to_string()),
        None => Bytes::new(),
    };
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("codeartifact.{REGION}.amazonaws.com"))
        .header("authorization", authorization("codeartifact"))
        .header("content-type", "application/json")
        .body(payload)
        .expect("a well-formed request");
    answered(&router.dispatch(request).await)
}

/// One KMS call over its `awsJson1_1` wire: `POST /` with `X-Amz-Target:
/// TrentService.<Operation>`.
async fn kms(router: &MultiServiceRouter, operation: &str, body: Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("kms.{REGION}.amazonaws.com"))
        .header("authorization", authorization("kms"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("TrentService.{operation}"))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    answered(&router.dispatch(request).await)
}

/// `POST /v1/domain?domain=` — `encryptionKey` travels in the body, and is sent only when the
/// case names a key.
async fn create_domain(
    router: &MultiServiceRouter,
    domain: &str,
    encryption_key: Option<&str>,
) -> (u16, Value) {
    let body = match encryption_key {
        Some(key) => json!({ "encryptionKey": key }),
        None => json!({}),
    };
    codeartifact(
        router,
        "POST",
        &format!("/v1/domain?domain={domain}"),
        Some(body),
    )
    .await
}

/// `POST /v1/package-group?domain=` — the pattern and the metadata travel in the body.
async fn create_package_group(
    router: &MultiServiceRouter,
    domain: &str,
    pattern: &str,
    description: Option<&str>,
) -> (u16, Value) {
    let mut body = json!({ "packageGroup": pattern });
    if let Some(description) = description {
        body["description"] = json!(description);
    }
    codeartifact(
        router,
        "POST",
        &format!("/v1/package-group?domain={domain}"),
        Some(body),
    )
    .await
}

/// The ARN a domain of `name` carries: the whole addressing a listing's entry is found by
/// below, and the way the recorded client addresses one.
fn domain_arn(name: &str) -> String {
    format!("arn:aws:codeartifact:{REGION}:{ACCOUNT}:domain/{name}")
}

/// The ARN of the implicit root package group of `domain`. Its pattern `/*` is carried in the
/// ARN's resource path with the wildcard percent-encoded, which is how the entry is addressed
/// in a listing.
fn root_group_arn(domain: &str) -> String {
    format!("arn:aws:codeartifact:{REGION}:{ACCOUNT}:package-group/{domain}/%2a")
}

/// The entry of `entries` whose `member` is `value`.
fn entry_by<'a>(entries: &'a [Value], member: &str, value: &str) -> Option<&'a Value> {
    entries
        .iter()
        .find(|entry| entry.get(member).and_then(Value::as_str) == Some(value))
}

/// The `description` of a package-group object, or `None` when the member is absent — which
/// is what a group whose creator sent none must report.
fn description(group: &Value) -> Option<&str> {
    group.get("description").and_then(Value::as_str)
}

/// How many pages a walk reads before the suite calls the listing non-terminating: far more
/// than any case below creates.
const MAX_PAGES: u32 = 64;

/// Every `domains` entry of `ListDomains` walked at `page_size` per page, so each entry is
/// read out of a PAGE rather than one complete answer. `maxResults` and `nextToken` carry no
/// `httpQuery` binding on this operation, so they travel in the BODY.
async fn walk_domains(router: &MultiServiceRouter, page_size: u32) -> Vec<Value> {
    let mut collected: Vec<Value> = Vec::new();
    let mut token: Option<String> = None;
    for page in 0..MAX_PAGES {
        let mut body = json!({ "maxResults": page_size });
        if let Some(held) = &token {
            body["nextToken"] = json!(held);
        }
        let (status, answer) = codeartifact(router, "POST", "/v1/domains", Some(body)).await;
        assert_eq!(
            status, 200,
            "the paged walk: page {page} of ListDomains expected 200, got {status} with {answer}"
        );
        collected.extend(
            answer
                .get("domains")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
        );
        match answer.get("nextToken").and_then(Value::as_str) {
            Some(next) => token = Some(next.to_string()),
            None => return collected,
        }
    }
    panic!("the paged walk of ListDomains did not terminate, collected {collected:?}")
}

/// Every `packageGroups` entry of `ListPackageGroups` for `domain`, walked at `page_size` per
/// page. Every input member of this operation is `httpQuery`-bound, so the page size and the
/// token travel in the query string.
async fn walk_package_groups(
    router: &MultiServiceRouter,
    domain: &str,
    page_size: u32,
) -> Vec<Value> {
    let mut collected: Vec<Value> = Vec::new();
    let mut token: Option<String> = None;
    for page in 0..MAX_PAGES {
        let mut uri = format!("/v1/package-groups?domain={domain}&max-results={page_size}");
        if let Some(held) = &token {
            uri.push_str(&format!("&next-token={held}"));
        }
        let (status, answer) = codeartifact(router, "POST", &uri, None).await;
        assert_eq!(
            status, 200,
            "the paged walk: page {page} of {uri} expected 200, got {status} with {answer}"
        );
        collected.extend(
            answer
                .get("packageGroups")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
        );
        match answer.get("nextToken").and_then(Value::as_str) {
            Some(next) => {
                // Spliced into the query string below, so the walk states the assumption it
                // makes about the cursor rather than silently corrupting one that needed
                // escaping: RFC 3986's unreserved set carries literally in a query value.
                assert!(
                    next.chars()
                        .all(|held| held.is_ascii_alphanumeric() || "-._~".contains(held)),
                    "the paged walk: page {page} handed back a cursor needing escaping: \
                     {next:?}"
                );
                token = Some(next.to_string());
            }
            None => return collected,
        }
    }
    panic!("the paged walk of {domain}'s package groups did not terminate: {collected:?}")
}

// ---------------------------------------------------------------------------
// The key every listed domain is encrypted with
// ---------------------------------------------------------------------------

/// What a row expects the domain to be listed with.
#[derive(Debug)]
enum Expected {
    /// The Amazon Web Services managed key of the account and Region: the one key every
    /// create that names none resolves to.
    ManagedKey,
    /// The one key ARN this reference names, whichever spelling it used.
    Key(String),
}

/// One domain of the listing: how its create named the key, and the key it must be listed
/// with.
#[derive(Debug)]
struct KeyCase {
    label: &'static str,
    domain: &'static str,
    /// The `encryptionKey` the create sends, `None` for a create that names none.
    reference: Option<String>,
    expected: Expected,
}

#[tokio::test]
async fn every_listed_domain_reports_the_key_it_was_created_against() {
    let router = router();

    // Two customer keys, created through KMS itself, and an alias for the first: the spellings
    // `encryptionKey` accepts are several ways of naming ONE of them.
    let mut keys = Vec::new();
    for index in 0..2_u32 {
        let (status, created) = kms(
            &router,
            "CreateKey",
            json!({ "Description": format!("codeartifact domain key {index}") }),
        )
        .await;
        assert_eq!(
            status, 200,
            "setup: CreateKey {index} expected 200, got {status} with {created}"
        );
        let id = created["KeyMetadata"]["KeyId"]
            .as_str()
            .unwrap_or_else(|| panic!("setup: CreateKey {index} reports no KeyId: {created}"))
            .to_string();
        let arn = created["KeyMetadata"]["Arn"]
            .as_str()
            .unwrap_or_else(|| panic!("setup: CreateKey {index} reports no Arn: {created}"))
            .to_string();
        keys.push((id, arn));
    }
    let (first_id, first_arn) = keys[0].clone();
    let (_, second_arn) = keys[1].clone();

    let alias = "alias/codeartifact-listed-domains";
    let (status, aliased) = kms(
        &router,
        "CreateAlias",
        json!({ "AliasName": alias, "TargetKeyId": first_id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateAlias expected 200, got {status} with {aliased}"
    );
    let alias_arn = format!("arn:aws:kms:{REGION}:{ACCOUNT}:{alias}");

    // A reference KMS holds no key for: a key of the caller's own, in an account or key store
    // this emulator does not keep, which is recorded as the caller wrote it. That fallback is
    // this service's own documented policy (`state::domain_encryption`), not a modelled rule:
    // the row is here because it puts another VALUE in the listing, and whether AWS refuses
    // such a reference is not what it asserts.
    let foreign = format!("arn:aws:kms:{REGION}:{ACCOUNT}:key/not-a-key-this-store-holds");

    // The rows differ along the FORM of the reference the create named — none, a bare key id,
    // the key's own ARN, an alias name, an alias ARN, a reference naming no key here — and
    // along WHICH key it names, so one listing holds two different keys, two spellings of one
    // key, and two domains sharing the managed key.
    let cases = vec![
        KeyCase {
            label: "no key named: the managed key of the account and Region",
            domain: "listproj-managed-one",
            reference: None,
            expected: Expected::ManagedKey,
        },
        KeyCase {
            label: "a second create naming no key: the SAME managed key",
            domain: "listproj-managed-two",
            reference: None,
            expected: Expected::ManagedKey,
        },
        KeyCase {
            label: "a bare key id",
            domain: "listproj-by-id",
            reference: Some(first_id.clone()),
            expected: Expected::Key(first_arn.clone()),
        },
        KeyCase {
            label: "an alias name for that same key",
            domain: "listproj-by-alias",
            reference: Some(alias.to_string()),
            expected: Expected::Key(first_arn.clone()),
        },
        KeyCase {
            label: "an alias ARN for that same key",
            domain: "listproj-by-alias-arn",
            reference: Some(alias_arn.clone()),
            expected: Expected::Key(first_arn.clone()),
        },
        KeyCase {
            label: "another key, named by its own ARN",
            domain: "listproj-by-arn",
            reference: Some(second_arn.clone()),
            expected: Expected::Key(second_arn.clone()),
        },
        KeyCase {
            label: "a reference this store holds no key for",
            domain: "listproj-foreign",
            reference: Some(foreign.clone()),
            expected: Expected::Key(foreign.clone()),
        },
    ];

    // The managed key is whatever KMS associated with the service's predefined alias the
    // first time it was asked for; every row that names no key must agree on it.
    let mut managed: Option<String> = None;
    // What each domain's own create answered, which every later read of it must repeat.
    let mut created_with: Vec<(&str, &str, String)> = Vec::new();

    for case in &cases {
        let label = case.label;
        let (status, created) = create_domain(&router, case.domain, case.reference.as_deref()).await;
        assert_eq!(
            status, 200,
            "case {label:?}: CreateDomain expected 200, got {status} with {created}"
        );
        let reported = created["domain"]["encryptionKey"]
            .as_str()
            .unwrap_or_default()
            .to_string();

        match &case.expected {
            Expected::Key(key) => assert_eq!(
                &reported, key,
                "case {label:?}: expected the key the reference names, got {reported:?}"
            ),
            Expected::ManagedKey => {
                // A key KMS ACTUALLY HOLDS, and the one the service's predefined alias names
                // — asked of the service that owns keys, not asserted against a literal.
                let (status, described) =
                    kms(&router, "DescribeKey", json!({ "KeyId": reported })).await;
                assert_eq!(
                    (status, described["KeyMetadata"]["Arn"].as_str()),
                    (200, Some(reported.as_str())),
                    "case {label:?}: KMS must hold the key the domain reports, got {described}"
                );
                let (status, by_alias) =
                    kms(&router, "DescribeKey", json!({ "KeyId": MANAGED_KEY_ALIAS })).await;
                assert_eq!(
                    (status, by_alias["KeyMetadata"]["Arn"].as_str()),
                    (200, Some(reported.as_str())),
                    "case {label:?}: the predefined alias must name that key, got {by_alias}"
                );
                match &managed {
                    None => managed = Some(reported.clone()),
                    Some(first) => assert_eq!(
                        &reported, first,
                        "case {label:?}: one account and Region has ONE managed key, got \
                         {reported:?} after {first:?}"
                    ),
                }
            }
        }

        created_with.push((label, case.domain, reported));
    }

    // ONE listing, holding every domain above at once: each entry addressed by its ARN
    // reports the key ITS domain was created against.
    let (status, listed) = codeartifact(&router, "POST", "/v1/domains", Some(json!({}))).await;
    assert_eq!(
        status, 200,
        "ListDomains expected 200, got {status} with {listed}"
    );
    let entries = listed["domains"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| panic!("ListDomains reports no domains: {listed}"));
    assert_eq!(
        entries.len(),
        cases.len(),
        "ListDomains must report every domain created above, got {listed}"
    );

    for (label, domain, reported) in &created_with {
        let arn = domain_arn(domain);
        let entry = entry_by(&entries, "arn", &arn)
            .unwrap_or_else(|| panic!("case {label:?}: no entry with arn {arn} in {listed}"));
        assert_eq!(
            entry.get("encryptionKey").and_then(Value::as_str),
            Some(reported.as_str()),
            "case {label:?}: the listed entry expected the key the create bound \
             ({reported:?}), got {entry}"
        );
    }

    // Two domains created against DIFFERENT keys are listed with different keys, and two
    // created against the SAME key with the same one: neither entry's value can have come
    // from a neighbour's record. Read out of the LISTING, which is what is under test.
    let listed_key = |domain: &str| -> String {
        let arn = domain_arn(domain);
        let entry = entry_by(&entries, "arn", &arn)
            .unwrap_or_else(|| panic!("no entry with arn {arn} in {listed}"));
        entry
            .get("encryptionKey")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("the entry at {arn} reports no encryptionKey: {entry}"))
            .to_string()
    };
    assert_ne!(
        listed_key("listproj-by-id"),
        listed_key("listproj-by-arn"),
        "two domains created against different keys must be listed with different keys"
    );
    assert_eq!(
        listed_key("listproj-by-id"),
        listed_key("listproj-by-alias"),
        "two spellings of one key must be listed with that one key"
    );

    // Every page of a paginated walk projects the same per-entry value: one domain per page,
    // so no entry rides along with another's record.
    let walked = walk_domains(&router, 1).await;
    assert_eq!(
        walked.len(),
        cases.len(),
        "the paged walk must report every domain, got {walked:?}"
    );
    for (label, domain, reported) in &created_with {
        let arn = domain_arn(domain);
        let entry = entry_by(&walked, "arn", &arn)
            .unwrap_or_else(|| panic!("case {label:?}: no entry with arn {arn} in {walked:?}"));
        assert_eq!(
            entry.get("encryptionKey").and_then(Value::as_str),
            Some(reported.as_str()),
            "case {label:?}: the paged entry expected {reported:?}, got {entry}"
        );
    }
}

// ---------------------------------------------------------------------------
// The description every listed package group carries
// ---------------------------------------------------------------------------

/// One group a caller creates, with the description it is created with (`None` is "no
/// description member sent").
#[derive(Debug)]
struct GroupCase {
    pattern: &'static str,
    description: Option<&'static str>,
}

/// The groups a caller creates in the populated domain. They differ along whether a
/// description was sent at all, its length (the modelled `Description` extremes, length
/// 0..1000), and — deliberately — one group carrying the SAME text the service assigns the
/// root group, so the root entry's value cannot be one borrowed from a neighbour.
const GROUP_CASES: &[GroupCase] = &[
    GroupCase {
        pattern: "/npm/*",
        description: Some("every npm package"),
    },
    GroupCase {
        pattern: "/npm/@scope/*",
        description: None,
    },
    GroupCase {
        pattern: "/maven/com.example/*",
        description: Some(""),
    },
    GroupCase {
        pattern: "/pypi/requests$",
        description: Some(ROOT_DESCRIPTION),
    },
];

#[tokio::test]
async fn every_listed_package_group_reports_its_own_description() {
    let router = router();

    // Two domains: one holding several caller-created groups beside the implicit root group,
    // one holding only the root group. The root entry of each must carry the service's text,
    // so it is the record's own value and not something a caller in that domain supplied.
    let populated = "listproj-groups";
    let fresh = "listproj-fresh";
    for domain in [populated, fresh] {
        let (status, created) = create_domain(&router, domain, None).await;
        assert_eq!(
            status, 200,
            "setup: CreateDomain {domain} expected 200, got {status} with {created}"
        );
    }
    for case in GROUP_CASES {
        let (status, created) =
            create_package_group(&router, populated, case.pattern, case.description).await;
        assert_eq!(
            status, 200,
            "setup: CreatePackageGroup {} expected 200, got {status} with {created}",
            case.pattern
        );
    }

    for domain in [populated, fresh] {
        let label = format!("domain {domain}");
        let (status, listed) = codeartifact(
            &router,
            "POST",
            &format!("/v1/package-groups?domain={domain}"),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {label:?}: ListPackageGroups expected 200, got {status} with {listed}"
        );
        let entries = listed["packageGroups"]
            .as_array()
            .cloned()
            .unwrap_or_else(|| panic!("case {label:?}: no packageGroups in {listed}"));

        // The root entry as the recorded client addresses it: by the ARN carrying its
        // percent-encoded pattern.
        let arn = root_group_arn(domain);
        let root = entry_by(&entries, "arn", &arn)
            .unwrap_or_else(|| panic!("case {label:?}: no entry with arn {arn} in {listed}"));
        assert_eq!(
            description(root),
            Some(ROOT_DESCRIPTION),
            "case {label:?}: the root entry expected the service's description \
             {ROOT_DESCRIPTION:?}, got {root}"
        );
        assert_eq!(
            entry_by(&entries, "pattern", ROOT_PATTERN),
            Some(root),
            "case {label:?}: the entry at {arn} must be the root group's own entry, got {listed}"
        );

        // Every caller-created group reports what ITS creator sent, and nothing at all when
        // the creator sent none.
        if domain == populated {
            for case in GROUP_CASES {
                let entry = entry_by(&entries, "pattern", case.pattern).unwrap_or_else(|| {
                    panic!("case {label:?}: no entry for {} in {listed}", case.pattern)
                });
                assert_eq!(
                    description(entry),
                    case.description,
                    "case {label:?}: {} expected the description its creator sent, got {entry}",
                    case.pattern
                );
            }
        }
    }

    // A page of a paginated listing projects the same values: one entry per page, so the root
    // group is read out of a page of its own.
    let walked = walk_package_groups(&router, populated, 1).await;
    assert_eq!(
        walked.len(),
        GROUP_CASES.len() + 1,
        "the paged walk must report the root group and every created group, got {walked:?}"
    );
    let arn = root_group_arn(populated);
    let root = entry_by(&walked, "arn", &arn)
        .unwrap_or_else(|| panic!("no entry with arn {arn} in the paged walk {walked:?}"));
    assert_eq!(
        description(root),
        Some(ROOT_DESCRIPTION),
        "the paged root entry expected {ROOT_DESCRIPTION:?}, got {root}"
    );
    for case in GROUP_CASES {
        let entry = entry_by(&walked, "pattern", case.pattern)
            .unwrap_or_else(|| panic!("no entry for {} in {walked:?}", case.pattern));
        assert_eq!(
            description(entry),
            case.description,
            "the paged entry for {} expected the description its creator sent, got {entry}",
            case.pattern
        );
    }
}
