// SPDX-License-Identifier: CC0-1.0

//! IAM's decision in front of the collection read that reports the account's whole custom
//! domain inventory: `ListDomainNames` — `GET /v1/domainnames`, output member
//! `domainNameConfigs`, input `maxResults` (`@range` 0..25) and `nextToken`, modelled
//! `AccessDeniedException` at 403 (`smithy_query(query="operation",
//! operation="ListDomainNames")`).
//!
//! The rule under test: a collection read names no single resource, so the identity-based
//! decision taken in FRONT of this service judges it against every resource of this service
//! in the account and region the request addresses, BEFORE the listing assembles anything; a
//! caller whose own identity-based documents do not allow that action there is refused with
//! the modelled `AccessDeniedException` (403) and IAM's refusal sentence — the explicit-deny
//! clause where a document denies the action, the default-deny clause where nothing allows
//! it — and the refusal carries nothing of what the account holds: no page, no domain name,
//! no certificate ARN, no AppSync-side hostname and no hosted-zone id.
//!
//! The tables span what the rule generalises over: the request FORM the listing is asked in
//! (the bare collection, a page size at the modelled maximum, the smallest page, and a
//! resume carrying the cursor a served page handed back), WHAT the caller's documents say
//! (nothing at all, an allow of another action of this service, an allow of this action on
//! one domain's own ARN rather than on the account's resources, an explicit deny beside a
//! wildcard allow), the KIND of principal (an IAM user, an assumed-role session, the
//! account's own credential), what the account HOLDS (several domains of different modelled
//! forms, none), and — for one principal whose document is rewritten under it — the three
//! states a document can be in (denying, allowing, absent).
//!
//! The principals are made with IAM's and STS's own operations over the same engine, so the
//! documents this service reads are the ones those services wrote; the harness is the
//! identity-authorization suite's.

use serde_json::json;
use service_com_amazonaws_appsync_awsdeepdishcontrolplaneservice::service::authorization::{
    ACTION_PREFIX, ANY_RESOURCE,
};
use service_com_amazonaws_appsync_awsdeepdishcontrolplaneservice::state;

use crate::identity_authorization::{
    allow, allow_all_but, arn, default_deny_message, explicit_deny_message, form_encode, Emulator,
    Reply, ACCESS_DENIED, ACCESS_DENIED_STATUS, ACCOUNT_KEY,
};

/// The collection this listing reads, and the one `CreateDomainName` writes into. Source:
/// the `http` binding of both (`smithy_query(query="operation",
/// operation="ListDomainNames,CreateDomainName")`).
const DOMAIN_NAMES_PATH: &str = "/v1/domainnames";

/// The operation under test, which is also the short name of the IAM action a policy states
/// about it (`appsync:ListDomainNames`).
const OPERATION: &str = "ListDomainNames";

/// Another action of this service, used to show the decision is about the action the request
/// names: a read of ONE custom domain rather than of the collection.
const OTHER_OPERATION: &str = "GetDomainName";

/// The member the output carries its page in. Source: `ListDomainNamesResponse`.
const LISTED_MEMBER: &str = "domainNameConfigs";

/// The largest page the input admits: `maxResults` is `@range` 0..25.
const MAX_RESULTS_MAX: i32 = 25;

/// The domains the inventory holds, one per form the modelled `DomainName` pattern
/// (`^(\*[\w\d-]*\.)?([\w\d-]+\.)+[\w\d-]+$`) admits, so neither the listing nor the refusal
/// can be keyed to one kind of domain. Source: the `domainName` member of
/// `CreateDomainNameRequest`.
const SEEDED_DOMAINS: &[&str] = &[
    "listed-gate.example",
    "api.listed-gate.example.com",
    "*.listed-gate.example.com",
];

/// The two forms `CertificateArn`'s pattern admits — an ACM certificate and an IAM server
/// certificate — rotated across the seeded domains. Source: the `certificateArn` member of
/// `CreateDomainNameRequest`.
const CERTIFICATE_PREFIXES: &[&str] = &[
    "arn:aws:acm:us-east-1:000000000000:certificate/listed-gate-",
    "arn:aws:iam::000000000000:server-certificate/listed_gate_",
];

/// What a refused caller's documents say, built from one domain the account holds.
struct Refused {
    case: &'static str,
    document: fn(&str) -> Option<String>,
    /// Whether the principal is an assumed-role session rather than an IAM user — which is
    /// also what decides the ARN form the refusal blames it by.
    assumed_role: bool,
    /// Whether a document DENIES the action, which is the clause the refusal names.
    explicit_deny: bool,
}

/// No document at all: IAM's default deny.
fn nothing(_domain: &str) -> Option<String> {
    None
}

/// An allow of ANOTHER action of this service on everything.
fn another_action(_domain: &str) -> Option<String> {
    Some(allow(
        &format!("{ACTION_PREFIX}:{OTHER_OPERATION}"),
        ANY_RESOURCE,
    ))
}

/// An allow of THIS action, but on one domain's own ARN rather than on the resources a
/// collection read is judged against.
fn this_action_on_one_domain(domain: &str) -> Option<String> {
    Some(allow(
        &format!("{ACTION_PREFIX}:{OPERATION}"),
        &arn(&state::domain_name_resource(domain)),
    ))
}

/// A wildcard allow over this service with an explicit deny of this action beside it.
fn an_explicit_deny(_domain: &str) -> Option<String> {
    Some(allow_all_but(&format!("{ACTION_PREFIX}:{OPERATION}")))
}

const REFUSED: &[Refused] = &[
    Refused {
        case: "an IAM user whose documents allow nothing",
        document: nothing,
        assumed_role: false,
        explicit_deny: false,
    },
    Refused {
        case: "an assumed-role session whose role allows nothing",
        document: nothing,
        assumed_role: true,
        explicit_deny: false,
    },
    Refused {
        case: "an allow of another action of this service",
        document: another_action,
        assumed_role: false,
        explicit_deny: false,
    },
    Refused {
        case: "an allow of this action on one domain rather than on the account's resources",
        document: this_action_on_one_domain,
        assumed_role: false,
        explicit_deny: false,
    },
    Refused {
        case: "an IAM user with an explicit deny beside a wildcard allow",
        document: an_explicit_deny,
        assumed_role: false,
        explicit_deny: true,
    },
    Refused {
        case: "an assumed-role session with an explicit deny beside a wildcard allow",
        document: an_explicit_deny,
        assumed_role: true,
        explicit_deny: true,
    },
];

/// A wildcard allow over every action of this service, which is what serves the listing.
fn wildcard_allow() -> String {
    allow(&format!("{ACTION_PREFIX}:*"), ANY_RESOURCE)
}

/// The sentence the refusal carries: the principal that signed, the action it asked for, the
/// resources a collection read of this service is judged against, and which of IAM's rules
/// withheld the grant.
fn expected_message(principal: &str, explicit_deny: bool) -> String {
    let resource = arn(ANY_RESOURCE);
    if explicit_deny {
        explicit_deny_message(principal, OPERATION, &resource)
    } else {
        default_deny_message(principal, OPERATION, &resource)
    }
}

/// What the account holds while the tables run, and every string of it a refusal must not
/// disclose.
struct Inventory {
    /// The domain names, sorted, as a served listing reports them.
    domains: Vec<String>,
    disclosures: Vec<(String, String)>,
}

/// Create the custom domains the tables are written about, with the account's own
/// credential, and collect what a refusal must withhold.
async fn inventory(emulator: &Emulator) -> Inventory {
    let mut disclosures: Vec<(String, String)> = Vec::new();
    for (index, domain) in SEEDED_DOMAINS.iter().enumerate() {
        let mut body = json!({
            "domainName": domain,
            "certificateArn": format!(
                "{}{index:04}",
                CERTIFICATE_PREFIXES[index % CERTIFICATE_PREFIXES.len()]
            ),
        });
        // A description and tags on some rows, so the projection the listing carries — and
        // therefore what a refusal withholds — is not one shape of record only.
        if index.is_multiple_of(2) {
            body["description"] = json!(format!("the domain at {index}"));
            body["tags"] = json!({"stage": "beta"});
        }
        let created = emulator
            .call(ACCOUNT_KEY, "POST", DOMAIN_NAMES_PATH, Some(&body))
            .await;
        assert_eq!(
            created.status, 200,
            "case \"the inventory\" / {domain:?}: expected CreateDomainName 200, got {} {:?} \
             with {:?}",
            created.status, created.error_code, created.body
        );
        let config = &created.body["domainNameConfig"];
        for member in [
            "domainName",
            "certificateArn",
            "appsyncDomainName",
            "hostedZoneId",
            "domainNameArn",
            "description",
        ] {
            // A member the create answered nothing for is nothing to withhold.
            if let Some(value) = config[member].as_str().filter(|value| !value.is_empty()) {
                disclosures.push((format!("{domain}'s {member}"), value.to_string()));
            }
        }
    }
    let mut domains: Vec<String> = SEEDED_DOMAINS.iter().map(|&name| name.to_string()).collect();
    domains.sort();
    assert!(
        !disclosures.is_empty(),
        "case \"the inventory\": expected the creates to answer the stored records"
    );
    Inventory {
        domains,
        disclosures,
    }
}

/// One listing request, with the `maxResults` and `nextToken` the caller names.
fn listing_path(max_results: Option<i32>, next_token: Option<&str>) -> String {
    let mut query: Vec<String> = Vec::new();
    if let Some(max_results) = max_results {
        query.push(format!("maxResults={max_results}"));
    }
    if let Some(next_token) = next_token {
        // Percent-encoded, so a cursor carrying a character reserved in a query string
        // reaches the service as the token the previous page handed back.
        query.push(format!("nextToken={}", form_encode(next_token)));
    }
    if query.is_empty() {
        DOMAIN_NAMES_PATH.to_string()
    } else {
        format!("{DOMAIN_NAMES_PATH}?{}", query.join("&"))
    }
}

/// Page the whole account as the caller behind `access_key_id`, asserting every page is a
/// served 200, and answer the domain names reported, sorted.
async fn served_domains(emulator: &Emulator, case: &str, access_key_id: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let mut token: Option<String> = None;
    // One page per entry at the smallest page size, plus the page an empty listing answers.
    for _ in 0..=SEEDED_DOMAINS.len() {
        let path = listing_path(Some(1), token.as_deref());
        let reply = emulator.call(access_key_id, "GET", &path, None).await;
        assert_eq!(
            reply.status, 200,
            "case {case} at {path:?}: expected the listing served, got {} {:?} with {:?}",
            reply.status, reply.error_code, reply.body
        );
        let page = reply.body[LISTED_MEMBER]
            .as_array()
            .unwrap_or_else(|| {
                panic!("case {case} at {path:?}: expected a page, got {:?}", reply.body)
            })
            .clone();
        names.extend(
            page.iter()
                .filter_map(|config| config["domainName"].as_str())
                .map(str::to_string),
        );
        match reply.body["nextToken"].as_str() {
            None => {
                names.sort();
                return names;
            }
            Some(cursor) => token = Some(cursor.to_string()),
        }
    }
    panic!("case {case}: the walk never reached a last page, got {names:?}");
}

/// The cursor a served first page of one entry hands back, which is the resume form the
/// tables below ask the listing in.
async fn cursor(emulator: &Emulator, access_key_id: &str) -> String {
    let path = listing_path(Some(1), None);
    let reply = emulator.call(access_key_id, "GET", &path, None).await;
    assert_eq!(
        reply.status, 200,
        "case \"the cursor\": expected the listing served, got {} {:?} with {:?}",
        reply.status, reply.error_code, reply.body
    );
    reply.body["nextToken"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "case \"the cursor\": expected a nextToken while entries remain, got {:?}",
                reply.body
            )
        })
        .to_string()
}

/// Assert `reply` is the modelled refusal of the listing for `principal`, carrying nothing
/// of what the account holds.
fn assert_refused(
    case: &str,
    principal: &str,
    explicit_deny: bool,
    reply: &Reply,
    disclosures: &[(String, String)],
) {
    assert_eq!(
        (reply.status, reply.error_code.as_str()),
        (ACCESS_DENIED_STATUS, ACCESS_DENIED),
        "case {case}: expected {ACCESS_DENIED_STATUS} {ACCESS_DENIED}, got {} {:?} with {:?}",
        reply.status,
        reply.error_code,
        reply.body
    );
    assert_eq!(
        reply.message(),
        expected_message(principal, explicit_deny),
        "case {case}: expected IAM's refusal sentence about this action and the resources the \
         listing is judged against, got {:?}",
        reply.message()
    );
    assert!(
        reply.body.get(LISTED_MEMBER).is_none() && reply.body.get("nextToken").is_none(),
        "case {case}: expected the refusal to carry no page and no cursor, got {:?}",
        reply.body
    );
    let answered = reply.body.to_string();
    for (what, value) in disclosures {
        assert!(
            !answered.contains(value.as_str()),
            "case {case}: expected the refusal to disclose nothing of what the account holds, \
             got {what} in {answered}"
        );
    }
}

/// RULE: the custom domain inventory is served only to a caller whose identity-based
/// documents allow `appsync:ListDomainNames` on the resources a collection read is judged
/// against; every other caller is refused with the modelled 403 `AccessDeniedException` and
/// IAM's refusal sentence — the explicit-deny clause where a document denies the action,
/// the default-deny clause where nothing allows it — carrying nothing of what the account
/// holds.
#[tokio::test]
async fn an_unauthorized_caller_is_refused_the_domain_name_inventory() {
    let emulator = Emulator::new();
    let held = inventory(&emulator).await;
    let permitted = emulator
        .user("domain_inventory_reader", Some(&wildcard_allow()))
        .await;

    // The positive control: a caller the documents allow IS served the account's whole
    // inventory, so a refusal below is the gate's answer and not an empty account.
    let served = served_domains(
        &emulator,
        "a caller the documents allow",
        &permitted.access_key_id,
    )
    .await;
    assert_eq!(
        served, held.domains,
        "case \"a caller the documents allow\": expected every stored domain, got {served:?}"
    );

    // The request forms the listing is asked in: the bare collection, the modelled maximum
    // page, the smallest page, and a resume carrying the cursor a served page handed back.
    let resume = cursor(&emulator, &permitted.access_key_id).await;
    let forms: Vec<String> = vec![
        listing_path(None, None),
        listing_path(Some(MAX_RESULTS_MAX), None),
        listing_path(Some(1), None),
        listing_path(None, Some(&resume)),
        listing_path(Some(1), Some(&resume)),
    ];

    for (index, row) in REFUSED.iter().enumerate() {
        let label = format!("domain_listing_{index}");
        let document = (row.document)(SEEDED_DOMAINS[0]);
        let principal = if row.assumed_role {
            emulator.assumed_role(&label, document.as_deref()).await
        } else {
            emulator.user(&label, document.as_deref()).await
        };
        for path in &forms {
            let case = format!("{OPERATION} at {path:?} for {}", row.case);
            let reply = emulator
                .call(&principal.access_key_id, "GET", path, None)
                .await;
            assert_refused(
                &case,
                &principal.arn,
                row.explicit_deny,
                &reply,
                &held.disclosures,
            );
        }
    }

    // The refusals read nothing and wrote nothing: the permitted caller is served the same
    // inventory afterwards.
    let again = served_domains(&emulator, "after the refusals", &permitted.access_key_id).await;
    assert_eq!(
        again, held.domains,
        "case \"after the refusals\": expected the inventory unchanged, got {again:?}"
    );
}

/// RULE: the decision is taken before the listing reads anything, so what the account holds
/// does not change it — a caller whose documents allow nothing is refused the same way over
/// an account holding no custom domain at all, where an ungated read would answer an empty
/// page, and the account's own credential is served that empty page.
#[tokio::test]
async fn the_refusal_does_not_depend_on_what_the_account_holds() {
    let emulator = Emulator::new();
    let ungranted = emulator.user("empty_domain_account_probe", None).await;

    for path in [
        listing_path(None, None),
        listing_path(Some(MAX_RESULTS_MAX), None),
        listing_path(Some(0), None),
    ] {
        let case = format!("{OPERATION} at {path:?} over an account holding no domain");
        let reply = emulator
            .call(&ungranted.access_key_id, "GET", &path, None)
            .await;
        assert_refused(&case, &ungranted.arn, false, &reply, &[]);

        let served = emulator.call(ACCOUNT_KEY, "GET", &path, None).await;
        assert_eq!(
            (
                served.status,
                served.body[LISTED_MEMBER].as_array().map(Vec::len)
            ),
            (200, Some(0)),
            "case {case}: expected the account's own credential to be served an empty page, \
             got {} {:?} with {:?}",
            served.status,
            served.error_code,
            served.body
        );
    }
}

/// RULE: the decision follows the documents the IAM store holds for the principal at the
/// time of the request, so the SAME signed listing flips as those documents are rewritten
/// under it: denied while an explicit deny matches, served while a grant covers it, and
/// refused by IAM's default deny once no document is attached at all.
#[tokio::test]
async fn what_the_principals_documents_say_decides_the_same_listing() {
    let emulator = Emulator::new();
    let held = inventory(&emulator).await;
    let user_name = "domain_listing_rewritten";
    let principal = emulator
        .user(
            user_name,
            Some(&allow_all_but(&format!("{ACTION_PREFIX}:{OPERATION}"))),
        )
        .await;
    let path = listing_path(None, None);

    let denied = emulator
        .call(&principal.access_key_id, "GET", &path, None)
        .await;
    assert_refused(
        "an explicit deny beside a wildcard allow",
        &principal.arn,
        true,
        &denied,
        &held.disclosures,
    );

    emulator.put_user_policy(user_name, &wildcard_allow()).await;
    let served = served_domains(
        &emulator,
        "the deny lifted for a wildcard allow",
        &principal.access_key_id,
    )
    .await;
    assert_eq!(
        served, held.domains,
        "case \"the deny lifted for a wildcard allow\": expected every stored domain, got \
         {served:?}"
    );

    // A grant of exactly this action on exactly the resources a collection read is judged
    // against serves it too, so the grant above is not a wildcard-only path.
    emulator
        .put_user_policy(
            user_name,
            &allow(&format!("{ACTION_PREFIX}:{OPERATION}"), &arn(ANY_RESOURCE)),
        )
        .await;
    let scoped = served_domains(
        &emulator,
        "a grant of this action on the account's resources",
        &principal.access_key_id,
    )
    .await;
    assert_eq!(
        scoped, held.domains,
        "case \"a grant of this action on the account's resources\": expected every stored \
         domain, got {scoped:?}"
    );

    emulator.delete_user_policy(user_name).await;
    let default_denied = emulator
        .call(&principal.access_key_id, "GET", &path, None)
        .await;
    assert_refused(
        "the document removed again",
        &principal.arn,
        false,
        &default_denied,
        &held.disclosures,
    );
}
