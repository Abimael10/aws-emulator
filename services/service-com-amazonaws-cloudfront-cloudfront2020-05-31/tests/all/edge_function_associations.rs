// SPDX-License-Identifier: CC0-1.0

//! The CLOUDFRONT FUNCTION a cache behaviour associates, RUN at the edge — end to end over
//! HTTP, with Amazon S3 mounted beside CloudFront as the distribution's origin.
//!
//! RULE: a cache behaviour that associates a CloudFront function runs that function's `LIVE`
//! code on the viewer event the association names, and what the function RETURNS is what
//! CloudFront continues with: on the viewer-request event the returned request is the one the
//! edge keys on and fetches (a returned response is served straight away, with no origin
//! request made), on the viewer-response event the returned response is the one the viewer
//! receives; and a run that produced no event object at all is answered `503`, CloudFront
//! having neither a request to fetch with nor a response to serve.
//!
//! The rows span what the rule generalises over: which event the association names, what the
//! function read from the event object to decide (the URI, a query-string parameter, a
//! cookie, the response's own status), and what it returned (a rewritten request, a generated
//! response, a response carrying an added header, a changed status, and a raise). The row
//! that associates NO function is the anchor: the same origin, the same object, and the
//! answer the origin produced reaches the viewer untouched — which is what makes the
//! association, rather than the origin, the thing deciding.
//!
//! Every function is created and PUBLISHED through the service's own API, so the code that
//! runs at the edge is code a client really stored; the origin, the bucket and the viewer
//! client are [`crate::edge_origin_response_headers`]'s, reused rather than copied.

use crate::distribution_lifecycle::{awaited_deployment, ACCOUNT_ID};
use crate::edge_origin_response_headers::{
    create_distribution_over_bucket, create_origin_access_control, view, Emulator, Staged, Viewed,
    MISS,
};
use crate::function_lifecycle::Create;
use crate::function_publish::{create_one, publish_function};

/// The object the distribution's origin holds at the path a viewer asks for, and the one a
/// rewritten URI names: two different objects, so a rewrite is visible in the BYTES the
/// viewer receives rather than only in a header.
const ASKED_FOR: Staged = Staged::plain("original.txt", "the object the viewer asked for");
const REWRITTEN_TO: Staged = Staged::plain("rewritten.txt", "the object the function chose");

/// A path the origin bucket holds NOTHING at: a row whose function generates its own response
/// asks for this, so an answer that is not the function's could only have come from the origin.
const ABSENT_PATH: &str = "/nothing-is-stored-here.txt";

/// `X-Cache` for an answer a CloudFront function generated itself, and for the page CloudFront
/// generates when a function's run failed.
const FUNCTION_GENERATED: &str = "FunctionGeneratedResponse from cloudfront";
const FUNCTION_EXECUTION_ERROR: &str = "FunctionExecutionError from cloudfront";

/// `X-Cache` for an answer CloudFront produced itself, which is what a refusal of its own
/// carries.
const ERROR: &str = "Error from cloudfront";

/// The runtime every function below declares (`FunctionRuntime`'s later value).
const RUNTIME: &str = "cloudfront-js-2.0";

// ---------------------------------------------------------------------------
// The function code under test
// ---------------------------------------------------------------------------

/// A viewer-request function that rewrites the URI it was handed.
const REWRITES_THE_URI: &str = "function handler(event) {\n\
     \x20   var request = event.request;\n\
     \x20   if (request.uri === '/original.txt') {\n\
     \x20       request.uri = '/rewritten.txt';\n\
     \x20   }\n\
     \x20   return request;\n\
     }\n";

/// A viewer-request function that decides from a QUERY-STRING parameter of the event object.
///
/// The parameter is named in MIXED case, because a query-string name is case-sensitive: the
/// function sees the name the viewer spelled, and the edge asks the origin for that name.
const REWRITES_ON_A_PARAMETER: &str = "function handler(event) {\n\
     \x20   var request = event.request;\n\
     \x20   if (request.querystring.Pick && request.querystring.Pick.value === 'other') {\n\
     \x20       request.uri = '/rewritten.txt';\n\
     \x20   }\n\
     \x20   return request;\n\
     }\n";

/// A viewer-request function that decides from a COOKIE of the event object, named in mixed
/// case for the same reason.
const REWRITES_ON_A_COOKIE: &str = "function handler(event) {\n\
     \x20   var request = event.request;\n\
     \x20   if (request.cookies.Pick && request.cookies.Pick.value === 'other') {\n\
     \x20       request.uri = '/rewritten.txt';\n\
     \x20   }\n\
     \x20   return request;\n\
     }\n";

/// A viewer-request function that GENERATES the response itself, so the origin is never asked.
const GENERATES_A_RESPONSE: &str = "function handler(event) {\n\
     \x20   return {\n\
     \x20       statusCode: 302,\n\
     \x20       statusDescription: 'Found',\n\
     \x20       headers: { 'location': { value: '/somewhere-else' } },\n\
     \x20       body: 'the function answered this itself'\n\
     \x20   };\n\
     }\n";

/// A viewer-response function that adds a header to the answer.
const ADDS_A_RESPONSE_HEADER: &str = "function handler(event) {\n\
     \x20   var response = event.response;\n\
     \x20   response.headers['x-added-by-the-function'] = { value: 'yes' };\n\
     \x20   return response;\n\
     }\n";

/// A viewer-response function that decides from the STATUS the edge was about to serve and
/// changes it.
const CHANGES_THE_STATUS: &str = "function handler(event) {\n\
     \x20   var response = event.response;\n\
     \x20   if (response.statusCode === 200) {\n\
     \x20       response.statusCode = 410;\n\
     \x20       response.statusDescription = 'Gone';\n\
     \x20   }\n\
     \x20   return response;\n\
     }\n";

/// A function that RAISES, whichever event it runs on.
const RAISES: &str = "function handler(event) {\n\
     \x20   throw new Error('this function does not finish');\n\
     }\n";

// ---------------------------------------------------------------------------
// The rows
// ---------------------------------------------------------------------------

/// Which viewer event a row's association names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Event {
    ViewerRequest,
    ViewerResponse,
}

impl Event {
    /// `FunctionAssociation$EventType` for this event.
    fn token(self) -> &'static str {
        match self {
            Self::ViewerRequest => "viewer-request",
            Self::ViewerResponse => "viewer-response",
        }
    }
}

/// One row: a function associated on one event, the viewer request that exercises it, and the
/// answer the rule says the viewer receives.
struct Row {
    case: &'static str,
    /// The function's name, which also names this row's bucket, distribution and caller
    /// reference, so each row stands on its own resources.
    slug: &'static str,
    /// The association this row's behaviour carries, or `None` for the anchor row.
    association: Option<(Event, &'static str)>,
    /// The method the viewer asks with.
    method: &'static str,
    /// The path and query the viewer asks for.
    path: &'static str,
    /// The headers the viewer sends beside its `Host`.
    headers: &'static [(&'static str, &'static str)],
    status: u16,
    x_cache: &'static str,
    /// What the answer's body must carry.
    body_carries: &'static str,
    /// A response header the answer must carry, and its value.
    header: Option<(&'static str, &'static str)>,
}

/// The table.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "no function at all: the origin's answer reaches the viewer",
            slug: "edgefn-none",
            association: None,
            method: "GET",
            path: "/original.txt",
            headers: &[],
            status: 200,
            x_cache: MISS,
            body_carries: ASKED_FOR.body,
            header: None,
        },
        Row {
            case: "a viewer-request function that rewrites the URI",
            slug: "edgefn-rewrite",
            association: Some((Event::ViewerRequest, REWRITES_THE_URI)),
            method: "GET",
            path: "/original.txt",
            headers: &[],
            status: 200,
            x_cache: MISS,
            body_carries: REWRITTEN_TO.body,
            header: None,
        },
        Row {
            case: "a viewer-request function deciding from a query-string parameter",
            slug: "edgefn-querystring",
            association: Some((Event::ViewerRequest, REWRITES_ON_A_PARAMETER)),
            method: "GET",
            path: "/original.txt?Pick=other",
            headers: &[],
            status: 200,
            x_cache: MISS,
            body_carries: REWRITTEN_TO.body,
            header: None,
        },
        Row {
            case: "the same function on a request that does not ask for the other object",
            slug: "edgefn-querystring-not-picked",
            association: Some((Event::ViewerRequest, REWRITES_ON_A_PARAMETER)),
            method: "GET",
            path: "/original.txt?Pick=this",
            headers: &[],
            status: 200,
            x_cache: MISS,
            body_carries: ASKED_FOR.body,
            header: None,
        },
        Row {
            case: "the same function on the same name in another case, which is another name",
            slug: "edgefn-querystring-other-case",
            association: Some((Event::ViewerRequest, REWRITES_ON_A_PARAMETER)),
            method: "GET",
            path: "/original.txt?pick=other",
            headers: &[],
            status: 200,
            x_cache: MISS,
            body_carries: ASKED_FOR.body,
            header: None,
        },
        Row {
            case: "a viewer-request function deciding from a cookie",
            slug: "edgefn-cookie",
            association: Some((Event::ViewerRequest, REWRITES_ON_A_COOKIE)),
            method: "GET",
            path: "/original.txt",
            headers: &[("cookie", "first=1; Pick=other")],
            status: 200,
            x_cache: MISS,
            body_carries: REWRITTEN_TO.body,
            header: None,
        },
        Row {
            case: "a viewer-request function that generates the response itself",
            slug: "edgefn-generated",
            association: Some((Event::ViewerRequest, GENERATES_A_RESPONSE)),
            method: "GET",
            path: ABSENT_PATH,
            headers: &[],
            status: 302,
            x_cache: FUNCTION_GENERATED,
            body_carries: "the function answered this itself",
            header: Some(("location", "/somewhere-else")),
        },
        Row {
            // The enforcement CloudFront performs itself comes FIRST: a method the behaviour
            // does not allow is refused by the edge, and the function of that behaviour does
            // not run on it — so the viewer gets CloudFront's refusal and not the response the
            // function would have generated.
            case: "a method the behaviour does not allow, on a behaviour with a function",
            slug: "edgefn-method-refused",
            association: Some((Event::ViewerRequest, GENERATES_A_RESPONSE)),
            method: "POST",
            path: "/original.txt",
            headers: &[],
            status: 403,
            x_cache: ERROR,
            body_carries: "403 ERROR",
            header: None,
        },
        Row {
            case: "a viewer-request function that raises",
            slug: "edgefn-request-raises",
            association: Some((Event::ViewerRequest, RAISES)),
            method: "GET",
            path: "/original.txt",
            headers: &[],
            status: 503,
            x_cache: FUNCTION_EXECUTION_ERROR,
            body_carries: "503 ERROR",
            header: None,
        },
        Row {
            case: "a viewer-response function that adds a header",
            slug: "edgefn-response-header",
            association: Some((Event::ViewerResponse, ADDS_A_RESPONSE_HEADER)),
            method: "GET",
            path: "/original.txt",
            headers: &[],
            status: 200,
            x_cache: MISS,
            body_carries: ASKED_FOR.body,
            header: Some(("x-added-by-the-function", "yes")),
        },
        Row {
            case: "a viewer-response function that changes the status",
            slug: "edgefn-response-status",
            association: Some((Event::ViewerResponse, CHANGES_THE_STATUS)),
            method: "GET",
            path: "/original.txt",
            headers: &[],
            status: 410,
            x_cache: MISS,
            body_carries: ASKED_FOR.body,
            header: None,
        },
        Row {
            case: "a viewer-response function that raises",
            slug: "edgefn-response-raises",
            association: Some((Event::ViewerResponse, RAISES)),
            method: "GET",
            path: "/original.txt",
            headers: &[],
            status: 503,
            x_cache: FUNCTION_EXECUTION_ERROR,
            body_carries: "503 ERROR",
            header: None,
        },
    ]
}

/// The `FunctionAssociations` element one row's behaviour carries.
fn association_xml(arn: &str, event: Event) -> String {
    format!(
        "<FunctionAssociations><Quantity>1</Quantity><Items><FunctionAssociation>\
         <FunctionARN>{arn}</FunctionARN><EventType>{}</EventType>\
         </FunctionAssociation></Items></FunctionAssociations>",
        event.token()
    )
}

/// Create the function `row` associates and publish it to `LIVE`, answering its ARN — "your
/// functions must be published to the `LIVE` stage to associate them with a cache behavior".
async fn published_function(emulator: &Emulator, row: &Row, code: &str) -> String {
    let create = Create {
        case: row.case,
        name: row.slug.replace('-', "_"),
        comment: String::new(),
        runtime: RUNTIME,
        code: code.as_bytes().to_vec(),
        associations: Vec::new(),
        tags: Vec::new(),
    };
    let etag = create_one(&emulator.router, &create).await;
    let published = publish_function(&emulator.router, &create.name, &etag).await;
    assert_eq!(
        published.status, 200,
        "case {:?}: the publish must answer HTTP 200, got {} with {}",
        row.case, published.status, published.raw
    );
    format!("arn:aws:cloudfront::{ACCOUNT_ID}:function/{}", create.name)
}

/// Every row: what the associated function returned is what CloudFront continued with.
#[tokio::test]
async fn an_associated_function_decides_what_the_edge_serves() {
    let emulator = Emulator::new();
    let table = rows();
    // Every row's resources are staged first, so the propagation each distribution owes runs
    // on the one clock rather than one row's wait after another's.
    let mut staged = Vec::new();
    for row in &table {
        let bucket = row.slug;
        emulator.staged_bucket(row.case, bucket).await;
        emulator.staged_object(row.case, bucket, &ASKED_FOR).await;
        emulator
            .staged_object(row.case, bucket, &REWRITTEN_TO)
            .await;
        let control = create_origin_access_control(&emulator.router, row.case, row.slug).await;
        let behavior = match row.association {
            Some((event, code)) => {
                let arn = published_function(&emulator, row, code).await;
                association_xml(&arn, event)
            }
            None => String::new(),
        };
        let (id, domain, arn) = create_distribution_over_bucket(
            &emulator.router,
            row.case,
            row.slug,
            bucket,
            &control,
            &behavior,
        )
        .await;
        emulator
            .granted_to_cloudfront(row.case, bucket, &arn)
            .await;
        staged.push((id, domain));
    }

    for (row, (id, domain)) in table.iter().zip(staged) {
        let case = row.case;
        awaited_deployment(&emulator.router, case, &id).await;
        let viewed: Viewed =
            view(&emulator.router, row.method, &domain, row.path, row.headers).await;
        assert_eq!(
            viewed.status, row.status,
            "case {case:?}: expected HTTP {}, got {} with {:?}",
            row.status, viewed.status, viewed.body
        );
        assert_eq!(
            viewed.x_cache(),
            row.x_cache,
            "case {case:?}: expected X-Cache {:?}, got {:?}",
            row.x_cache,
            viewed.x_cache()
        );
        assert!(
            viewed.body.contains(row.body_carries),
            "case {case:?}: expected the answer to carry {:?}, got {:?}",
            row.body_carries,
            viewed.body
        );
        if let Some((name, value)) = row.header {
            let actual = viewed.header(name);
            assert_eq!(
                actual,
                Some(value),
                "case {case:?} ({name:?}): expected {value:?}, got {actual:?} (headers {:?})",
                viewed.headers
            );
        }
        // Whatever ran, the answer is still the edge's: it carries the identifier of the
        // request CloudFront served.
        assert!(
            viewed
                .header("x-amz-cf-id")
                .is_some_and(|value| !value.is_empty()),
            "case {case:?}: expected the edge's own request identifier, got {:?}",
            viewed.headers
        );
    }
}
