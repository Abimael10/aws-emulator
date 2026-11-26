// SPDX-License-Identifier: CC0-1.0

//! What `Subscribe` judges a `lambda` endpoint by, over the whole mounted service set: the
//! FORM of the value, never whether the function it names exists.
//!
//! The rule under test: **for a protocol whose endpoint is the ARN of another service's
//! resource — `lambda`, whose endpoint "is the ARN of an Lambda function"
//! (`smithy_query(query="doc",shape="SubscribeInput",member="Endpoint")`) — `Subscribe`
//! admits a value with an ARN's six `arn:`-led fields naming a function, whatever Lambda
//! holds, and refuses a value that is not such an ARN with HTTP 400 under the modelled
//! `InvalidParameterException` (`awsQueryError` code `InvalidParameter`) and the sentence
//! naming the FORM that protocol requires, `Invalid parameter: Lambda endpoint ARN`.** An
//! admitted subscription needs no confirmation — SNS confirms a subscription of a resource
//! the caller already owns itself — so it reads back with `PendingConfirmation` `false`; a
//! refused one creates nothing.
//!
//! Two services have to disagree about the same name for that to be observable, which is why
//! the suite lives here rather than in either crate: the refused forms below are read against
//! a function Lambda really HOLDS (its bare name and its partial `<account>:function:<name>`
//! reference — the two forms Lambda's own `FunctionName` member accepts — are still not
//! endpoints), and the admitted forms against names Lambda holds NOTHING for, each checked
//! through that service's own `GetFunction`. So neither half can be passing because the two
//! services happen to agree about existence.
//!
//! The rows differ along what the rule generalises over: for the refusal, each way a value can
//! fail to be an ARN (a bare name, the partial reference, the resource part alone, the
//! service's own API URL, a six-field value that does not begin with `arn:`), crossed with
//! whether the function exists; for the admission, the function-name and qualifier forms a
//! well-formed function ARN carries (a hyphenated name, an underscored one, the longest name
//! Lambda takes, a published version, an alias).

use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudwatch_alarm_action_reaches_its_subscribed_queue::{authorization, router};
use crate::lambda_signing_name_routing::{ACCOUNT, REGION, ROLE, ZIP_FILE};
use crate::query_wire::{decoded_entries, element, elements, form_encode};
use crate::sns_lambda_subscription_permission_boundary::lambda;

/// The protocol every `Subscribe` below asks for.
const LAMBDA_PROTOCOL: &str = "lambda";

/// The code SNS refuses a parameter under, as the modelled `InvalidParameterException`'s
/// `awsQueryError` declares it, and the sentence the `lambda` protocol's endpoint form takes.
const INVALID_PARAMETER: &str = "InvalidParameter";
const LAMBDA_FORM_REFUSAL: &str = "Invalid parameter: Lambda endpoint ARN";

/// The function this suite really creates in Lambda, so a refusal of its own bare name is
/// about the FORM of the value and nothing else.
const PRESENT: &str = "sns-endpoint-form-present";

/// Names no function is ever created under here. They differ in the forms a function name
/// takes — hyphens, underscores and digits, and the longest one Lambda accepts: a bare function
/// name "is limited to 64 characters in length"
/// (`smithy_query(query="doc",shape="CreateFunctionRequest",member="FunctionName")`, which is
/// also where the three name formats that member takes are listed).
const ABSENT: &str = "sns-endpoint-form-absent";
const ABSENT_UNDERSCORED: &str = "sns_endpoint_form_absent_2";
const ABSENT_LONGEST: &str = "sns-endpoint-form-absent-name-of-the-longest-length-lambda-takes";

/// The qualifier forms a function ARN may carry: a published version and an alias.
const VERSION_QUALIFIER: &str = "1";
const ALIAS_QUALIFIER: &str = "live";

/// The unqualified ARN of the function `name` in this emulator's account and Region.
fn function_arn(name: &str) -> String {
    format!("arn:aws:lambda:{REGION}:{ACCOUNT}:function:{name}")
}

/// One SNS call over the `awsQuery` form wire, with whatever status it answered.
///
/// The sibling suites' SNS callers each assert HTTP 200, so none of them can carry a REFUSAL:
/// this one answers the status and the body, which is what a suite reading a refusal's code and
/// sentence needs.
async fn sns(
    router: &MultiServiceRouter,
    action: &str,
    params: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-03-31");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("sns.{REGION}.amazonaws.com"))
        .header("authorization", authorization("sns", REGION))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(bytes::Bytes::from(body))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (
        status,
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// Subscribe `endpoint` over the `lambda` protocol to `topic`, asking for the subscription's
/// ARN whether or not it is confirmed.
async fn subscribe(router: &MultiServiceRouter, topic: &str, endpoint: &str) -> (u16, String) {
    sns(
        router,
        "Subscribe",
        &[
            ("TopicArn", topic),
            ("Protocol", LAMBDA_PROTOCOL),
            ("Endpoint", endpoint),
            ("ReturnSubscriptionArn", "true"),
        ],
    )
    .await
}

/// The topic every row of one test subscribes to, created where SNS holds it.
async fn created_topic(router: &MultiServiceRouter, name: &str) -> String {
    let (status, body) = sns(router, "CreateTopic", &[("Name", name)]).await;
    assert_eq!(
        status, 200,
        "CreateTopic {name:?}: expected HTTP 200, got {status} with {body}"
    );
    element(&body, "TopicArn")
}

/// Create [`PRESENT`] where Lambda holds it, and assert that service reports it — the half of
/// the setup that makes the refusals below about the form rather than about existence.
async fn present_function(router: &MultiServiceRouter) {
    let (status, created) = lambda(
        router,
        "POST",
        "/2015-03-31/functions",
        Some(json!({
            "FunctionName": PRESENT,
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": "index.handler",
            "Code": { "ZipFile": ZIP_FILE },
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "setup: expected HTTP 201 from CreateFunction for {PRESENT:?}, got {status} with {created}"
    );
    let (status, fetched) = lambda(
        router,
        "GET",
        &format!("/2015-03-31/functions/{PRESENT}"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: Lambda must report the function it created, got {status} with {fetched}"
    );
}

/// Assert that Lambda holds NO function called `name`: the other half of the setup, so an
/// admitted subscription below really does name a function that does not exist.
async fn assert_absent(router: &MultiServiceRouter, name: &str) {
    let (status, fetched) = lambda(router, "GET", &format!("/2015-03-31/functions/{name}"), None)
        .await;
    assert_eq!(
        status, 404,
        "case {name:?}: Lambda must hold no such function, got {status} with {fetched}"
    );
}

/// Every endpoint value that is not a function ARN at all, with the case it stands for.
///
/// Each form is read twice — against the function Lambda HOLDS and against one it does not —
/// because that is the axis the rule is indifferent to and the symptom was reported on.
fn not_an_arn() -> Vec<(String, String)> {
    let mut rows = Vec::new();
    for (existence, name) in [("an existing", PRESENT), ("an absent", ABSENT)] {
        rows.push((
            format!("the bare NAME of {existence} function"),
            name.to_string(),
        ));
        rows.push((
            format!("the partial <account>:function:<name> reference of {existence} function"),
            format!("{ACCOUNT}:function:{name}"),
        ));
        rows.push((
            format!("the resource part alone of {existence} function"),
            format!("function:{name}"),
        ));
        rows.push((
            format!("the Lambda API URL of {existence} function"),
            format!("https://lambda.{REGION}.amazonaws.com/2015-03-31/functions/{name}"),
        ));
        rows.push((
            format!("the six fields of {existence} function's ARN without the arn: prefix"),
            format!("aws:lambda:{REGION}:{ACCOUNT}:function:{name}"),
        ));
    }
    rows
}

/// Every endpoint value that IS a well-formed function ARN, with the case it stands for. Only
/// the first names a function Lambda holds.
fn function_arns() -> Vec<(String, String)> {
    vec![
        (
            "the ARN of an existing function".to_string(),
            function_arn(PRESENT),
        ),
        (
            "the ARN of an absent function".to_string(),
            function_arn(ABSENT),
        ),
        (
            "the ARN of an absent function whose name carries underscores".to_string(),
            function_arn(ABSENT_UNDERSCORED),
        ),
        (
            "the ARN of an absent function with the longest name Lambda takes".to_string(),
            function_arn(ABSENT_LONGEST),
        ),
        (
            "the ARN of an absent function qualified by a version".to_string(),
            format!("{}:{VERSION_QUALIFIER}", function_arn(ABSENT)),
        ),
        (
            "the ARN of an absent function qualified by an alias".to_string(),
            format!("{}:{ALIAS_QUALIFIER}", function_arn(ABSENT)),
        ),
    ]
}

/// An `Endpoint` the `lambda` protocol cannot read as a function ARN is refused with the form
/// that protocol requires — whether or not Lambda holds a function of that name — and creates
/// nothing.
#[tokio::test]
async fn a_lambda_endpoint_that_is_not_a_function_arn_states_the_form_required() {
    let router = router();
    present_function(&router).await;
    assert_absent(&router, ABSENT).await;
    let topic = created_topic(&router, "sns-lambda-endpoint-form-refusals").await;

    for (case, endpoint) in not_an_arn() {
        let (status, body) = subscribe(&router, &topic, &endpoint).await;
        assert_eq!(
            status, 400,
            "case {case:?} ({endpoint:?}): expected HTTP 400, got {status} with {body}"
        );
        assert_eq!(
            element(&body, "Code"),
            INVALID_PARAMETER,
            "case {case:?} ({endpoint:?}): expected the {INVALID_PARAMETER} code, got {body}"
        );
        assert_eq!(
            element(&body, "Message"),
            LAMBDA_FORM_REFUSAL,
            "case {case:?} ({endpoint:?}): expected {LAMBDA_FORM_REFUSAL:?}, got {body}"
        );
    }

    // The refusal is judged before anything is written, so the topic holds no subscription at
    // all — the half a caller checks when the sentence it branches on changed.
    let (status, listed) = sns(
        &router,
        "ListSubscriptionsByTopic",
        &[("TopicArn", &topic)],
    )
    .await;
    assert_eq!(
        status, 200,
        "ListSubscriptionsByTopic: expected HTTP 200, got {status} with {listed}"
    );
    assert!(
        elements(&listed, "SubscriptionArn").is_empty(),
        "a refused Subscribe must create nothing, got {listed}"
    );
}

/// A well-formed function ARN is subscribed whatever Lambda holds: the subscription is created,
/// confirmed and reads back through `GetSubscriptionAttributes` with the endpoint it was asked
/// for.
#[tokio::test]
async fn a_well_formed_function_arn_is_subscribed_whether_or_not_the_function_exists() {
    let router = router();
    present_function(&router).await;
    for absent in [ABSENT, ABSENT_UNDERSCORED, ABSENT_LONGEST] {
        assert_absent(&router, absent).await;
    }
    let topic = created_topic(&router, "sns-lambda-endpoint-form-admitted").await;

    let rows = function_arns();
    for (case, endpoint) in &rows {
        let (status, body) = subscribe(&router, &topic, endpoint).await;
        assert_eq!(
            status, 200,
            "case {case:?} ({endpoint:?}): expected HTTP 200, got {status} with {body}"
        );
        let arn = element(&body, "SubscriptionArn");
        assert!(
            arn.starts_with(&topic),
            "case {case:?} ({endpoint:?}): expected a subscription of {topic}, got {body}"
        );

        let (status, read) = sns(
            &router,
            "GetSubscriptionAttributes",
            &[("SubscriptionArn", &arn)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetSubscriptionAttributes expected HTTP 200, got {status} with {read}"
        );
        let attributes = decoded_entries(&read);
        assert_eq!(
            attributes.get("Endpoint").map(String::as_str),
            Some(endpoint.as_str()),
            "case {case:?}: expected the stored endpoint {endpoint:?}, got {attributes:?}"
        );
        assert_eq!(
            attributes.get("Protocol").map(String::as_str),
            Some(LAMBDA_PROTOCOL),
            "case {case:?}: expected the stored protocol {LAMBDA_PROTOCOL:?}, got {attributes:?}"
        );
        assert_eq!(
            attributes.get("PendingConfirmation").map(String::as_str),
            Some("false"),
            "case {case:?}: a subscription of a resource the caller owns needs no confirmation, \
             got {attributes:?}"
        );
    }

    // Every admitted endpoint is a subscription of the topic, and no two rows collapsed into
    // one: the qualifier is part of the endpoint a subscription is keyed by.
    let (status, listed) = sns(
        &router,
        "ListSubscriptionsByTopic",
        &[("TopicArn", &topic)],
    )
    .await;
    assert_eq!(
        status, 200,
        "ListSubscriptionsByTopic: expected HTTP 200, got {status} with {listed}"
    );
    let mut subscribed = elements(&listed, "Endpoint");
    subscribed.sort();
    let mut wanted: Vec<String> = rows.iter().map(|(_, endpoint)| endpoint.clone()).collect();
    wanted.sort();
    assert_eq!(
        subscribed, wanted,
        "expected one subscription per admitted endpoint, got {listed}"
    );

    // And the functions the admitted ARNs name are still not there: SNS stored a reference, it
    // did not create or require a function.
    for absent in [ABSENT, ABSENT_UNDERSCORED, ABSENT_LONGEST] {
        assert_absent(&router, absent).await;
    }
}
