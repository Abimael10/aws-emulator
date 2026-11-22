// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of the thing-principal relation read from the PRINCIPAL's end:
//! `ListPrincipalThings`.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a restJson1
//! request signed for the `iot` signing name, with the principal in the `x-amzn-principal`
//! header the model binds it to — so what is asserted is what an AWS SDK client would see
//! and not an internal function's return value.
//!
//! The rule under test: a listing addressed to a principal reports, out of the ONE
//! thing-principal relation the account's state keeps, exactly the things that principal is
//! attached to — in one total order, one page of at most `maxResults` rows at a time, with
//! `nextToken` present exactly while the listing is truncated — and the listing resolves the
//! principal against that state first, so a value that is no principal address at all is
//! refused as an invalid request and a well-formed address naming no principal of the
//! account as a missing resource, never as an empty page. Reading the listing changes
//! nothing, and the two ends of the relation agree: a pair the attach recorded is reported
//! by the listing of the thing's principals and by the listing of the principal's things
//! alike, and the detach removes it from both.
//!
//! The tables span what the rule generalises over rather than one recorded request: both
//! forms a thing principal is named in (a certificate ARN and an Amazon Cognito identity),
//! two ARNs of ONE certificate, several counts of attached things, several page sizes
//! including one larger than the listing, and every class of value that names no principal
//! of the account.

use std::collections::BTreeSet;
use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region the harness signs for, and the one the emulated ARNs are scoped to.
const REGION: &str = "us-east-1";

/// Amazon Cognito identities of the `<region>:<id>` form `AttachThingPrincipal$principal`
/// documents for a principal ("a certificate ARN (as returned from the CreateCertificate
/// operation) or an Amazon Cognito ID",
/// `smithy_query(query="operation",operation="AttachThingPrincipal")`). They are principals
/// no record of this account has to exist for — identity pools live in another service — and
/// one of them is scoped to another region, so the form is covered rather than one value.
const COGNITO_IDENTITIES: [&str; 2] = [
    "us-east-1:11111111-1111-1111-1111-111111111111",
    "eu-west-1:22222222-2222-2222-2222-222222222222",
];

/// A `nextToken` no listing of this emulator ever issued, in the hex alphabet a continuation
/// token of one is spelled in.
const FOREIGN_NEXT_TOKEN: &str = "6e6f742d612d746f6b656e";

/// The `x-amzn-errortype` this service answers `InvalidRequestException` under: the shape
/// name with the Coral locator AWS IoT appends to it.
const INVALID_REQUEST: &str = "InvalidRequestException:http://iot.amazonaws.com/doc/2015-10-02/";

/// The `x-amzn-errortype` this service answers `ResourceNotFoundException` under.
const NOT_FOUND: &str = "ResourceNotFoundException";

/// A SigV4 `Authorization` header whose credential scope names the `iot` signing name and
/// the region: the scope is how the router picks the claiming service, and how the service
/// picks the region its ARNs are scoped to. The signature itself is never checked here —
/// the harness turns request authentication off, exactly as an unauthenticated emulator
/// runs.
fn signed_for_iot() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTESTIOT/20260101/{REGION}/iot/aws4_request, \
         SignedHeaders=host;x-amz-date, Signature=0000"
    )
}

/// One response, reduced to what a client branches on.
#[derive(Debug)]
struct Reply {
    status: u16,
    error_code: Option<String>,
    body: Value,
}

impl Reply {
    /// A string member of the body, or `None` when it is absent or not a string.
    fn text(&self, name: &str) -> Option<String> {
        self.body
            .get(name)
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    /// The `message` of an error body.
    fn message(&self) -> String {
        self.text("message").unwrap_or_default()
    }

    /// The string entries of a named list member of the body, empty when it is absent.
    fn string_list(&self, name: &str) -> Vec<String> {
        self.body
            .get(name)
            .and_then(Value::as_array)
            .map(|entries| {
                entries
                    .iter()
                    .map(|entry| entry.as_str().unwrap_or_default().to_string())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The `things` of a `ListPrincipalThings` answer, in the order it reported them.
    fn things(&self) -> Vec<String> {
        self.string_list("things")
    }

    /// The `principals` of a `ListThingPrincipals` answer, in the order it reported them.
    fn principals(&self) -> Vec<String> {
        self.string_list("principals")
    }

    /// The `nextToken` the answer carries, absent when the listing is not truncated.
    fn next_token(&self) -> Option<String> {
        self.text("nextToken")
    }
}

/// The emulator under test: ONE shared engine with the IoT service mounted on it, reached
/// the way the production router reaches it.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(engine));
        Self { router }
    }

    /// One request, with any number of extra headers — a thing principal is carried in one
    /// of them (`principal` is bound to `x-amzn-principal`).
    async fn call(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        headers: &[(&str, &str)],
    ) -> Reply {
        let payload = body.map_or_else(Vec::new, |value| {
            serde_json::to_vec(value).expect("a request body serializes")
        });
        let mut builder = http::Request::builder()
            .method(method)
            .uri(format!("https://iot.{REGION}.amazonaws.com{path}"))
            .header("authorization", signed_for_iot())
            .header("content-type", "application/json");
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let request = builder
            .body(bytes::Bytes::from(payload))
            .expect("a well-formed request");

        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let error_code = response
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
            error_code,
            body,
        }
    }

    /// `CreateThing`: `POST /things/{thingName}`.
    async fn create_thing(&self, thing_name: &str) -> Reply {
        self.call("POST", &format!("/things/{thing_name}"), Some(&json!({})), &[])
            .await
    }

    /// `CreateKeysAndCertificate`: `POST /keys-and-certificate`, which issues the
    /// certificate a device is reached through.
    async fn create_certificate(&self) -> Reply {
        self.call("POST", "/keys-and-certificate?setAsActive=true", None, &[])
            .await
    }

    /// `AttachThingPrincipal`: `PUT /things/{thingName}/principals`.
    async fn attach_principal(&self, thing_name: &str, principal: &str) -> Reply {
        self.call(
            "PUT",
            &format!("/things/{thing_name}/principals"),
            None,
            &[("x-amzn-principal", principal)],
        )
        .await
    }

    /// `DetachThingPrincipal`: `DELETE /things/{thingName}/principals`.
    async fn detach_principal(&self, thing_name: &str, principal: &str) -> Reply {
        self.call(
            "DELETE",
            &format!("/things/{thing_name}/principals"),
            None,
            &[("x-amzn-principal", principal)],
        )
        .await
    }

    /// `ListThingPrincipals`: `GET /things/{thingName}/principals`, the relation read from
    /// the thing's end.
    async fn list_thing_principals(&self, thing_name: &str) -> Reply {
        self.call("GET", &format!("/things/{thing_name}/principals"), None, &[])
            .await
    }

    /// `ListPrincipalThings`: `GET /principals/things`, with the principal in the header the
    /// model binds it to and the page the case asked for.
    async fn list_principal_things(
        &self,
        principal: &str,
        max_results: Option<i32>,
        next_token: Option<&str>,
    ) -> Reply {
        let mut query: Vec<String> = Vec::new();
        if let Some(max) = max_results {
            query.push(format!("maxResults={max}"));
        }
        if let Some(token) = next_token {
            query.push(format!("nextToken={token}"));
        }
        let path = if query.is_empty() {
            "/principals/things".to_string()
        } else {
            format!("/principals/things?{}", query.join("&"))
        };
        self.call("GET", &path, None, &[("x-amzn-principal", principal)])
            .await
    }

    /// A certificate of this account, answered as `(certificateId, certificateArn)`.
    async fn given_certificate(&self, case: &str) -> (String, String) {
        let issued = self.create_certificate().await;
        assert_eq!(
            issued.status, 200,
            "case {case}: expected a certificate to be issued, got {issued:?}"
        );
        let id = issued
            .text("certificateId")
            .unwrap_or_else(|| panic!("case {case}: the issued certificate reports an id"));
        let arn = issued
            .text("certificateArn")
            .unwrap_or_else(|| panic!("case {case}: the issued certificate reports an ARN"));
        (id, arn)
    }

    /// Register each of `thing_names` and attach `principal` to every one of them.
    async fn given_things_attached_to(&self, case: &str, principal: &str, thing_names: &[String]) {
        for thing_name in thing_names {
            let created = self.create_thing(thing_name).await;
            assert_eq!(
                created.status, 200,
                "case {case}: expected thing {thing_name} to be registered, got {created:?}"
            );
            let attached = self.attach_principal(thing_name, principal).await;
            assert_eq!(
                attached.status, 200,
                "case {case}: expected {principal} attached to {thing_name}, got {attached:?}"
            );
        }
    }
}

/// How this service states that the value a listing was asked for the things of names no
/// principal the account holds, as `crate::state::listing_missing_principal_message` words
/// it.
fn missing_principal_message(principal: &str) -> String {
    format!(
        "Failed to list things for principal {principal} because the principal does not \
         exist in your account"
    )
}

/// How this service states that a value bound where it takes a thing principal is no
/// principal address at all, as `crate::state::invalid_principal_type_message` words it.
fn invalid_principal_type_message(principal: &str) -> String {
    format!("Invalid principal type: {principal}")
}

/// `count` thing names of one case, deliberately NOT in ascending order, so the order the
/// listing reports is the relation's own and not the order the attaches happened in.
fn thing_names(prefix: &str, count: usize) -> Vec<String> {
    let scrambled = ["z", "a", "m", "c", "t", "b"];
    (0..count)
        .map(|index| format!("{}-{prefix}-{index}", scrambled[index % scrambled.len()]))
        .collect()
}

/// Walk the whole listing one page of `page_size` rows at a time, answering every row in the
/// order the pages reported them. Each page is asserted to carry at most the rows the request
/// asked for, and `nextToken` to be present only while rows remain.
async fn walk_pages(emulator: &Emulator, case: &str, principal: &str, page_size: i32) -> Vec<String> {
    let mut walked: Vec<String> = Vec::new();
    let mut token: Option<String> = None;
    // One page per row at worst, plus the page that ends the walk: a bound on the loop that
    // cannot be reached while the paging is correct.
    for _ in 0..=(walk_bound(emulator, principal).await) {
        let page = emulator
            .list_principal_things(principal, Some(page_size), token.as_deref())
            .await;
        assert_eq!(
            page.status, 200,
            "case {case}: expected a page of {page_size} rows, got {page:?}"
        );
        assert!(
            page.things().len() <= page_size as usize,
            "case {case}: expected at most {page_size} rows in a page, got {:?}",
            page.things()
        );
        walked.extend(page.things());
        token = page.next_token();
        if token.is_none() {
            return walked;
        }
    }
    panic!("case {case}: the paged walk of {principal} never ended, having read {walked:?}")
}

/// How many rows the listing holds in one unpaged answer: the bound the paged walk above may
/// not exceed.
async fn walk_bound(emulator: &Emulator, principal: &str) -> usize {
    emulator
        .list_principal_things(principal, None, None)
        .await
        .things()
        .len()
}

/// One row of the principal-form table: the forms a thing principal is named in, and how
/// many things the case attaches it to.
#[derive(Debug)]
struct PrincipalCase {
    /// What the case is, named in every assertion of it.
    case: &'static str,
    /// Whether the principal is a certificate of this account (its ARN is minted by the
    /// emulator) rather than a value supplied as it stands.
    certificate: bool,
    /// The principal as supplied, for a case that does not need one minted.
    supplied: &'static str,
    /// How many things the case attaches the principal to.
    things: usize,
}

/// The listing reports exactly the things the relation holds for the principal, whichever
/// form the principal is named in, in ascending order and without the rows of any other
/// principal — and the two ends of the relation agree about every pair.
#[tokio::test]
async fn a_listing_of_a_principals_things_reports_the_relation_from_the_principals_end() {
    let emulator = Emulator::new();

    let cases = [
        PrincipalCase {
            case: "a certificate attached to three things",
            certificate: true,
            supplied: "",
            things: 3,
        },
        PrincipalCase {
            case: "a certificate attached to one thing",
            certificate: true,
            supplied: "",
            things: 1,
        },
        PrincipalCase {
            case: "a certificate attached to nothing",
            certificate: true,
            supplied: "",
            things: 0,
        },
        PrincipalCase {
            case: "a Cognito identity of this region attached to two things",
            certificate: false,
            supplied: COGNITO_IDENTITIES[0],
            things: 2,
        },
        PrincipalCase {
            case: "a Cognito identity of another region attached to one thing",
            certificate: false,
            supplied: COGNITO_IDENTITIES[1],
            things: 1,
        },
    ];

    // Every pair recorded in this test, so each case can be asserted to report ITS rows and
    // none of another principal's.
    let mut recorded: Vec<(String, Vec<String>)> = Vec::new();

    for (index, case) in cases.iter().enumerate() {
        let principal = if case.certificate {
            emulator.given_certificate(case.case).await.1
        } else {
            case.supplied.to_string()
        };
        let mut names = thing_names(&format!("case{index}"), case.things);
        emulator
            .given_things_attached_to(case.case, &principal, &names)
            .await;
        names.sort();

        let listed = emulator.list_principal_things(&principal, None, None).await;
        assert_eq!(
            (listed.status, listed.things(), listed.next_token()),
            (200, names.clone(), None),
            "case {}: expected the things {names:?} in one page, got {listed:?}",
            case.case
        );

        // A READ changes nothing a later caller sees: the same question answers the same
        // thing.
        let again = emulator.list_principal_things(&principal, None, None).await;
        assert_eq!(
            (again.status, again.things()),
            (200, names.clone()),
            "case {}: expected a second read to report {names:?}, got {again:?}",
            case.case
        );

        // The relation has ONE record, so the listing from the thing's end reports the same
        // pair the listing from the principal's end does.
        for thing_name in &names {
            let principals = emulator.list_thing_principals(thing_name).await;
            assert!(
                principals.principals().contains(&principal),
                "case {}: expected {thing_name}'s principals to carry {principal}, got {principals:?}",
                case.case
            );
        }

        recorded.push((principal, names));
    }

    // No listing reports a row of another principal: the relation is keyed by the principal
    // the row belongs to.
    for (principal, names) in &recorded {
        let listed = emulator.list_principal_things(principal, None, None).await;
        let reported: BTreeSet<String> = listed.things().into_iter().collect();
        for (other, other_names) in &recorded {
            if other == principal {
                continue;
            }
            for other_name in other_names {
                assert!(
                    !reported.contains(other_name),
                    "case {principal}: expected no row of {other} ({other_name}), got {listed:?}"
                );
            }
        }
        let expected: BTreeSet<String> = names.iter().cloned().collect();
        assert_eq!(
            reported, expected,
            "case {principal}: expected exactly {expected:?}, got {listed:?}"
        );
    }
}

/// Two ARNs of ONE certificate name ONE principal, so either of them lists the same things:
/// a principal's identity is the resource it names and not the string that names it.
#[tokio::test]
async fn two_arns_of_one_certificate_list_the_same_things() {
    let emulator = Emulator::new();
    let case = "the same certificate named by two ARNs";

    let (certificate_id, certificate_arn) = emulator.given_certificate(case).await;
    let mut names = thing_names("two-arns", 2);
    emulator
        .given_things_attached_to(case, &certificate_arn, &names)
        .await;
    names.sort();

    // The ACCOUNT and REGION fields of an ARN do not name the resource — the certificate
    // registry is keyed by `certificateId` — so an ARN of the same certificate scoped to
    // another region addresses the same principal.
    let other_region_arn = format!("arn:aws:iot:eu-west-1:000000000000:cert/{certificate_id}");
    for principal in [certificate_arn.as_str(), other_region_arn.as_str()] {
        let listed = emulator.list_principal_things(principal, None, None).await;
        assert_eq!(
            (listed.status, listed.things()),
            (200, names.clone()),
            "case {case} ({principal}): expected {names:?}, got {listed:?}"
        );
    }
}

/// A detach removes the pair from the listing of the principal's things, as it does from the
/// listing of the thing's principals: one relation, two views of it.
#[tokio::test]
async fn a_detached_principal_stops_reporting_the_thing() {
    let emulator = Emulator::new();
    let case = "detach one of two attached things";

    let (_id, certificate_arn) = emulator.given_certificate(case).await;
    let mut names = thing_names("detach", 2);
    emulator
        .given_things_attached_to(case, &certificate_arn, &names)
        .await;
    names.sort();

    // An attach of a pair already recorded adds nothing: the relation is keyed by the
    // resource the principal names, so the thing is reported ONCE.
    let again = emulator.attach_principal(&names[0], &certificate_arn).await;
    assert_eq!(
        again.status, 200,
        "case {case}: expected a repeated attach to be taken, got {again:?}"
    );
    let listed = emulator
        .list_principal_things(&certificate_arn, None, None)
        .await;
    assert_eq!(
        (listed.status, listed.things()),
        (200, names.clone()),
        "case {case}: expected a repeated attach to leave {names:?}, got {listed:?}"
    );

    let detached = emulator.detach_principal(&names[0], &certificate_arn).await;
    assert_eq!(
        detached.status, 200,
        "case {case}: expected the detach to be taken, got {detached:?}"
    );
    let remaining = vec![names[1].clone()];
    let listed = emulator
        .list_principal_things(&certificate_arn, None, None)
        .await;
    assert_eq!(
        (listed.status, listed.things()),
        (200, remaining.clone()),
        "case {case}: expected {remaining:?} after the detach, got {listed:?}"
    );
    let principals = emulator.list_thing_principals(&names[0]).await;
    assert!(
        !principals.principals().contains(&certificate_arn),
        "case {case}: expected {} to hold no principal, got {principals:?}",
        names[0]
    );
}

/// The listing is `@paginated`: whatever page size the request asks for, the pages walked end
/// to end carry the whole listing once, in its own order, and `nextToken` is present exactly
/// while rows remain. A token no listing issued is refused.
#[tokio::test]
async fn a_listing_of_a_principals_things_pages_over_its_own_order() {
    let emulator = Emulator::new();
    let case = "paged walk";

    let (_id, certificate_arn) = emulator.given_certificate(case).await;
    let mut names = thing_names("paged", 5);
    emulator
        .given_things_attached_to(case, &certificate_arn, &names)
        .await;
    names.sort();

    let whole = emulator
        .list_principal_things(&certificate_arn, None, None)
        .await;
    assert_eq!(
        (whole.status, whole.things(), whole.next_token()),
        (200, names.clone(), None),
        "case {case}: expected the unpaged listing to carry {names:?}, got {whole:?}"
    );

    // `maxResults` is modelled `range 1..250`, so the page sizes span the smallest page, two
    // that divide the listing unevenly, the exact row count and one larger than the listing.
    for page_size in [1, 2, 3, 5, 7] {
        let walked = walk_pages(&emulator, case, &certificate_arn, page_size).await;
        assert_eq!(
            walked,
            names.clone(),
            "case {case} at maxResults={page_size}: expected {names:?}, got {walked:?}"
        );
    }

    // A page size at least as large as the listing is not truncated, so it carries no token.
    for page_size in [5, 250] {
        let page = emulator
            .list_principal_things(&certificate_arn, Some(page_size), None)
            .await;
        assert_eq!(
            (page.status, page.things(), page.next_token()),
            (200, names.clone(), None),
            "case {case} at maxResults={page_size}: expected an untruncated page, got {page:?}"
        );
    }

    // A token THIS listing did not issue selects no page of it: the request is to be fixed
    // rather than answered from the middle of some other listing.
    let refused = emulator
        .list_principal_things(&certificate_arn, None, Some(FOREIGN_NEXT_TOKEN))
        .await;
    assert_eq!(
        (refused.status, refused.error_code.as_deref()),
        (400, Some(INVALID_REQUEST)),
        "case {case}: expected a foreign nextToken to be refused, got {refused:?}"
    );

    // A token issued for ANOTHER principal's listing is just as foreign, whatever rows that
    // listing holds.
    let (_other_id, other_arn) = emulator.given_certificate(case).await;
    let other_names = thing_names("paged-other", 2);
    emulator
        .given_things_attached_to(case, &other_arn, &other_names)
        .await;
    let issued = emulator
        .list_principal_things(&other_arn, Some(1), None)
        .await;
    let other_token = issued
        .next_token()
        .unwrap_or_else(|| panic!("case {case}: a truncated page carries a token, got {issued:?}"));
    let crossed = emulator
        .list_principal_things(&certificate_arn, None, Some(&other_token))
        .await;
    assert_eq!(
        (crossed.status, crossed.error_code.as_deref()),
        (400, Some(INVALID_REQUEST)),
        "case {case}: expected another principal's token to be refused, got {crossed:?}"
    );
}

/// One row of the refusal table: a value bound where the listing takes a principal, and the
/// class the listing answers it with.
#[derive(Debug)]
struct RefusalCase {
    /// What the case is, named in every assertion of it.
    case: &'static str,
    /// The value the request carries as its principal.
    principal: &'static str,
    /// The HTTP status the listing answers.
    status: u16,
    /// The `x-amzn-errortype` the answer carries.
    error_code: &'static str,
    /// Whether the message is the "no principal address at all" sentence rather than the
    /// "no such principal in this account" one.
    invalid_type: bool,
}

/// The listing resolves the principal before it reads a row, so a value naming no principal
/// of this account is refused in the class that says WHY — never answered with an empty page.
#[tokio::test]
async fn a_listing_refuses_a_value_that_names_no_principal_of_this_account() {
    let emulator = Emulator::new();

    let cases = [
        RefusalCase {
            case: "a value that is no address at all",
            principal: "not-a-principal",
            status: 400,
            error_code: INVALID_REQUEST,
            invalid_type: true,
        },
        RefusalCase {
            case: "a path-shaped value that is no address",
            principal: "cert/1234567890abcdef",
            status: 400,
            error_code: INVALID_REQUEST,
            invalid_type: true,
        },
        RefusalCase {
            case: "a region-shaped pair that is no Cognito identity",
            principal: "US-EAST-1:not a cognito identity",
            status: 400,
            error_code: INVALID_REQUEST,
            invalid_type: true,
        },
        RefusalCase {
            case: "a certificate ARN of a certificate this account does not hold",
            principal:
                "arn:aws:iot:us-east-1:000000000000:cert/0000000000000000000000000000000000000000000000000000000000000000",
            status: 404,
            error_code: NOT_FOUND,
            invalid_type: false,
        },
        RefusalCase {
            case: "an ARN of a resource that is no principal of a thing",
            principal: "arn:aws:iot:us-east-1:000000000000:thinggroup/some-group",
            status: 404,
            error_code: NOT_FOUND,
            invalid_type: false,
        },
        RefusalCase {
            case: "an ARN of another service's resource",
            principal: "arn:aws:iam::000000000000:role/some-role",
            status: 404,
            error_code: NOT_FOUND,
            invalid_type: false,
        },
    ];

    for case in &cases {
        let refused = emulator
            .list_principal_things(case.principal, None, None)
            .await;
        let expected_message = if case.invalid_type {
            invalid_principal_type_message(case.principal)
        } else {
            missing_principal_message(case.principal)
        };
        assert_eq!(
            (
                refused.status,
                refused.error_code.as_deref(),
                refused.message()
            ),
            (case.status, Some(case.error_code), expected_message.clone()),
            "case {}: expected {} {} {expected_message:?}, got {refused:?}",
            case.case,
            case.status,
            case.error_code
        );
    }
}
