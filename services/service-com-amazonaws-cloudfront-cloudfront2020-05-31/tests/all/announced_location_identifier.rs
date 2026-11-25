// SPDX-License-Identifier: CC0-1.0

//! WHICH IDENTIFIER a create's `Location` names the new resource by, for the resource kinds
//! this API references by ARN, end to end over HTTP.
//!
//! RULE: the path of the URI a create announces ends at the identifier its own answer
//! publishes for the new resource to be REFERENCED BY. Where the resource kind's own route
//! label is not its minted id — a caller-chosen `{Name}` (`GetFunction`
//! `/2020-05-31/function/{Name}`) or a general `{Identifier}` (`GetConnectionFunction`
//! `/2020-05-31/connection-function/{Identifier}`) — that identifier is the ARN the create
//! minted and publishes as required output (`ConnectionFunctionSummary$ConnectionFunctionArn`
//! "The connection function Amazon Resource Name (ARN)",
//! `FunctionSummary$FunctionMetadata$FunctionARN`), never the bare id and never the name the
//! request supplied: the ARN is what names such a resource to every other surface — a tag, a
//! policy, a distribution's association — so a client that parses the announced URI recovers
//! the identifier AWS uses for that resource type, and the owning account with it. Where the
//! route label IS the resource's minted id, the path ends at that id.
//!
//! Scope: this suite drives the ARN half of that rule and one id-named route no other suite
//! covers (the invalidations of a distribution TENANT, nested under the tenant). The id half
//! on the flat and distribution-nested routes, and the key value store's ARN, are
//! [`crate::create_location_header`]'s table; the authority those paths hang under — the
//! endpoint the request reached — is [`crate::announced_location_uri`]'s.
//!
//! The rows span what the rule generalises over: the identifier kind the path ends at (an ARN
//! for a resource whose route label is a general identifier, an ARN for one whose route label
//! is the caller's own name, and a minted id under a NESTED collection), and, for the ARN
//! rows, the shape of the name the ARN was minted around (both published bounds of
//! `FunctionName`, `length 1..64, pattern ^[a-zA-Z0-9-_]{1,64}$`, and the character classes
//! that pattern admits). Each announced identifier is then proved to be the resource's own: by
//! FOLLOWING the URI where its path is a read route, and by reading the record back through
//! its describe route where the identifier is an ARN — an ARN carries a `/`, so the announced
//! path is not one route label and the URI is AWS's form rather than a dialable route.
//!
//! The harness (the router, the request helpers and the element scanner) is
//! [`crate::distribution_lifecycle`]'s and every request document is written by the writer its
//! own suite already states, so nothing here is a second spelling of them.

use smithy_server_runtime::MultiServiceRouter;

use crate::connection_function_lifecycle::{
    describe_connection_function, Create as ConnectionFunctionCreate,
};
use crate::distribution_lifecycle::{
    batch_xml, call, emulator, is_invalidation_id, Answer, API_VERSION, HOST,
};
use crate::function_lifecycle::{describe_function, Create as FunctionCreate, VIEWER_REQUEST_CODE};
use crate::tenant_invalidations::{create_invalidation_for_tenant, distribution_with_tenant};

/// How the identifier the announced path ends at is proved to name the resource the create
/// just made.
enum ReadBack {
    /// The announced path is the resource's own read route, so FOLLOWING it reaches the
    /// resource: the read answers 200 and reports the announced identifier as its `Id`.
    Follow,
    /// The announced identifier is an ARN, which carries a `/` and so is not one path label:
    /// the record is read through `GET /2020-05-31/connection-function/{Identifier}/describe`
    /// by the id the create reported, and must report the announced ARN.
    ConnectionFunction { id: String },
    /// As above for a CloudFront function, read by the name the create submitted.
    Function { name: String },
}

/// One create under test: what it answered, where the new resource lives and how the
/// identifier its `Location` ends at is proved to be the resource's own.
struct Case {
    case: String,
    /// The collection path the new resource lives under, between the API version and the
    /// identifier.
    collection: String,
    /// The identifier the announced path has to end at, read out of the same answer.
    identifier: String,
    /// Another identifier of the SAME resource, which that path must NOT end at.
    not_addressed_by: String,
    read_back: ReadBack,
    answer: Answer,
}

/// `POST /2020-05-31/{collection}` with `body`.
async fn create(router: &MultiServiceRouter, collection: &str, body: &str) -> Answer {
    call(
        router,
        "POST",
        &format!("/{API_VERSION}/{collection}"),
        Some(body),
        None,
    )
    .await
}

/// The text of the member `name` the answer reports — an identifier the SERVICE published for
/// the new resource, read out of the same answer whose header is under test.
fn member(answer: &Answer, name: &str, case: &str) -> String {
    answer
        .root()
        .find_text(name)
        .unwrap_or_else(|| panic!("case {case}: the answer carries no {name}: {}", answer.raw))
        .to_string()
}

/// The names the two ARN-minting creates are issued under: both published bounds of
/// `FunctionName` and the character classes its pattern admits, so the ARN each row announces
/// is one minted around a different name rather than one shape of name.
fn names(resource: &str) -> Vec<(&'static str, String)> {
    vec![
        ("the shortest admissible name", "a".to_string()),
        ("the longest admissible name (64)", "n".repeat(64)),
        (
            "hyphens, underscores and digits",
            format!("{resource}-by_region-2"),
        ),
    ]
}

/// The connection function creates: a resource whose read route takes a general
/// `{Identifier}`, so its announced URI carries the ARN rather than the id the store keys it
/// by.
async fn connection_function_cases(router: &MultiServiceRouter) -> Vec<Case> {
    let mut cases: Vec<Case> = Vec::new();
    for (shape, name) in names("connection") {
        let case = format!("CreateConnectionFunction / {shape}");
        let body = ConnectionFunctionCreate {
            case: "the announced location",
            name: name.clone(),
            comment: "announced location".to_string(),
            runtime: "cloudfront-js-2.0",
            code: b"function handler(event) { return event.request; }".to_vec(),
            associations: Vec::new(),
            tags: Vec::new(),
        }
        .xml();
        let answer = create(router, "connection-function", &body).await;
        let id = member(&answer, "Id", &case);
        let arn = member(&answer, "ConnectionFunctionArn", &case);
        assert!(
            arn.ends_with(&format!(":connection-function/{id}")),
            "case {case}: expected the ARN of the connection function {id:?}, got {arn:?}"
        );
        cases.push(Case {
            case,
            collection: "connection-function".to_string(),
            identifier: arn,
            not_addressed_by: id.clone(),
            read_back: ReadBack::ConnectionFunction { id },
            answer,
        });
    }
    cases
}

/// The CloudFront function creates: a resource whose read route takes the caller's own
/// `{Name}`, and whose create "contains an Amazon Resource Name (ARN) that uniquely identifies
/// the function" — the identifier its announced URI carries.
async fn function_cases(router: &MultiServiceRouter) -> Vec<Case> {
    let mut cases: Vec<Case> = Vec::new();
    for (shape, name) in names("function") {
        let case = format!("CreateFunction / {shape}");
        let body = FunctionCreate {
            case: "the announced location",
            name: name.clone(),
            comment: "announced location".to_string(),
            runtime: "cloudfront-js-2.0",
            code: VIEWER_REQUEST_CODE.as_bytes().to_vec(),
            associations: Vec::new(),
            tags: Vec::new(),
        }
        .xml();
        let answer = create(router, "function", &body).await;
        let arn = member(&answer, "FunctionARN", &case);
        assert!(
            arn.ends_with(&format!(":function/{name}")),
            "case {case}: expected the ARN of the function {name:?}, got {arn:?}"
        );
        cases.push(Case {
            case,
            collection: "function".to_string(),
            identifier: arn,
            not_addressed_by: name.clone(),
            read_back: ReadBack::Function { name },
            answer,
        });
    }
    cases
}

/// The tenant invalidation creates: the other half of the rule, on the one route whose
/// collection is nested in a distribution TENANT — an invalidation publishes an id and no ARN,
/// so the id is what its announced URI ends at.
async fn tenant_invalidation_cases(router: &MultiServiceRouter, tenant: &str) -> Vec<Case> {
    let mut cases: Vec<Case> = Vec::new();
    for (shape, paths) in [
        ("one path", vec!["/index.html"]),
        ("several paths", vec!["/a/*", "/b.css", "/deep/c.json"]),
    ] {
        let case = format!("CreateInvalidationForDistributionTenant / {shape}");
        let reference = format!("announced-tenant-{}", paths.len());
        let answer =
            create_invalidation_for_tenant(router, tenant, &batch_xml(&reference, &paths)).await;
        let id = member(&answer, "Id", &case);
        assert!(
            is_invalidation_id(&id),
            "case {case}: expected a minted invalidation id, got {id:?}"
        );
        cases.push(Case {
            case,
            collection: format!("distribution-tenant/{tenant}/invalidation"),
            identifier: id,
            not_addressed_by: reference,
            read_back: ReadBack::Follow,
            answer,
        });
    }
    cases
}

/// Every create announces the new resource by the identifier its own answer publishes for that
/// resource kind — the whole ARN for a connection function and a CloudFront function, the
/// minted id for a tenant invalidation — and never by another identifier of the same resource.
/// The announced identifier is then proved to name the resource the create made: by following
/// the URI where its path is a read route, and by the record's own describe where it is an ARN.
#[tokio::test]
async fn a_create_announces_the_identifier_the_resource_is_addressed_by() {
    let router = emulator();
    let tenant = distribution_with_tenant(&router, "announced-identifier").await;

    let mut cases = connection_function_cases(&router).await;
    cases.extend(function_cases(&router).await);
    cases.extend(tenant_invalidation_cases(&router, &tenant).await);

    for Case {
        case,
        collection,
        identifier,
        not_addressed_by,
        read_back,
        answer,
    } in cases
    {
        assert_eq!(
            answer.status, 201,
            "case {case}: expected the create to be accepted with HTTP 201, got {} with {}",
            answer.status, answer.raw
        );

        let path = format!("/{API_VERSION}/{collection}/{identifier}");
        let location = answer.header("location").to_string();
        assert_eq!(
            location,
            format!("https://{HOST}{path}"),
            "case {case}: expected the new resource announced at {identifier:?}, got {location:?}"
        );
        // The same resource answers to more than one identifier — an id and an ARN, a name and
        // an ARN, a caller reference and an id — and the announced URI ends at the one the
        // answer publishes for it, not at another.
        assert_ne!(
            location,
            format!("https://{HOST}/{API_VERSION}/{collection}/{not_addressed_by}"),
            "case {case}: the announced URI must not address the resource by {not_addressed_by:?}"
        );

        match read_back {
            ReadBack::Follow => {
                let followed = call(&router, "GET", &path, None, None).await;
                assert_eq!(
                    followed.status, 200,
                    "case {case}: following {location:?} must reach the resource, got {} with {}",
                    followed.status, followed.raw
                );
                let read = member(&followed, "Id", &case);
                assert_eq!(
                    read, identifier,
                    "case {case}: following the announced URI must reach {identifier:?}, got \
                     {read:?}"
                );
            }
            ReadBack::ConnectionFunction { id } => {
                let described = describe_connection_function(&router, &id, None).await;
                assert_eq!(
                    described.status, 200,
                    "case {case}: expected the describe of {id} to answer 200, got {} with {}",
                    described.status, described.raw
                );
                let stored = member(&described, "ConnectionFunctionArn", &case);
                assert_eq!(
                    stored, identifier,
                    "case {case}: the announced URI must name the connection function by the ARN \
                     its record reports, got {identifier:?} against {stored:?}"
                );
            }
            ReadBack::Function { name } => {
                let described = describe_function(&router, &name, None).await;
                assert_eq!(
                    described.status, 200,
                    "case {case}: expected the describe of {name} to answer 200, got {} with {}",
                    described.status, described.raw
                );
                let stored = member(&described, "FunctionARN", &case);
                assert_eq!(
                    stored, identifier,
                    "case {case}: the announced URI must name the function by the ARN its record \
                     reports, got {identifier:?} against {stored:?}"
                );
            }
        }
    }
}
