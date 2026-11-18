// SPDX-License-Identifier: CC0-1.0

//! The WORDING of the refusals this service states for a request it will not serve: the
//! one that reports no view exists for the ARN a request named, and the one that reports a
//! request BODY the model's own rules do not admit.
//!
//! The view-not-found class is one fact — "the `ViewArn` you gave resolves to no view this
//! account holds" — so every operation that resolves a view ARN against the store and
//! reports that fact says it with the same fixed sentence, followed by the ARN the REQUEST
//! carried, verbatim and unaltered. The rows below span every way a well-formed view ARN
//! can fail to resolve, and the published maximum length of the member that carries it,
//! so the message is shown to be a template over the request rather than text tied to one
//! value.
//!
//! The body-rejection class is the other way round: a body that breaks ANY rule the model
//! declares is one answer, carrying nothing from the request at all. Its rows span the
//! operations that bind members to the body, the constraint classes those members declare
//! (a range, a string length, a collection size, an absent required member) and both sides
//! of each published bound, so the sentence is shown to belong to the class rather than to
//! one member or one value — while a rule the model states only in prose, and a value the
//! request carries outside its body, keep the wording that names what failed.
//!
//! Every assertion goes over the wire: the suite mounts this service on a
//! [`MultiServiceRouter`] over one shared engine and dispatches restJson1 requests at
//! it, so the status, the `x-amzn-errortype` code and the serialized `Message` member
//! are all exercised the way an SDK client sees them.

use std::collections::BTreeMap;
use std::sync::Arc;

use bytes::Bytes;
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The FIXED text of this service's view-not-found refusal. Spelled out here rather than
/// imported, because the sentence itself is the requirement: the only variable part is
/// the ARN that follows it.
const VIEW_NOT_FOUND_PREFIX: &str = "Views Not Found for provided arn: ";

/// `AssociateDefaultViewInput$ViewArn` is `length {min: 1, max: 1011}` and carries no
/// `@pattern` (`smithy_query(query="operation",operation="AssociateDefaultView")`), so a
/// value inside those bounds reaches the store and is answered by the lookup, while one
/// past the maximum is refused by the modelled constraint before any lookup happens.
const VIEW_ARN_MAX: usize = 1011;

/// One emulator: the service mounted on a router over a single shared engine.
struct Emulator {
    router: MultiServiceRouter,
}

/// What one call answered: the HTTP status, the restJson1 error code (the
/// `x-amzn-errortype` header), and the decoded JSON body.
struct Answer {
    status: u16,
    error: Option<String>,
    body: Value,
}

impl Answer {
    /// The body of a call that had to succeed.
    fn ok(&self, case: &str) -> &Value {
        assert_eq!(
            self.status, 200,
            "case {case:?}: expected HTTP 200, got {} ({:?}) with body {}",
            self.status, self.error, self.body
        );
        &self.body
    }

    /// Assert this call failed with `status` and the error code `code`.
    fn failed(&self, case: &str, status: u16, code: &str) {
        assert_eq!(
            (self.status, self.error.as_deref()),
            (status, Some(code)),
            "case {case:?}: expected HTTP {status} {code}, got {} ({:?}) with body {}",
            self.status,
            self.error,
            self.body
        );
    }

    /// The error's `Message` member, however the envelope cases it.
    fn error_message(&self) -> String {
        self.body
            .get("Message")
            .or_else(|| self.body.get("message"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_resourceexplorer2_resourceexplorer::EmulatedService::new(engine),
        );
        Self { router }
    }

    /// Dispatch `method path` as a client signed for `region` would — the credential
    /// scope is the only place the wire states which Region the call is made in.
    async fn send(&self, region: &str, method: &str, path: &str, body: Option<Value>) -> Answer {
        let payload = match body {
            Some(value) => serde_json::to_vec(&value).expect("the request body serializes"),
            None => Vec::new(),
        };
        let request = http::Request::builder()
            .method(method)
            .uri(format!(
                "https://resource-explorer-2.{region}.amazonaws.com{path}"
            ))
            .header("content-type", "application/json")
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDTESTTESTTESTTEST/20260101/{region}\
                     /resource-explorer-2/aws4_request, SignedHeaders=host, Signature=00"
                ),
            )
            .body(Bytes::from(payload))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let error = response
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
        Answer {
            status,
            error,
            body,
        }
    }

    /// `POST /{operation}` with a JSON body.
    async fn call(&self, region: &str, operation: &str, body: Value) -> Answer {
        self.send(region, "POST", &format!("/{operation}"), Some(body))
            .await
    }

    /// `POST /AssociateDefaultView` naming `view_arn`.
    async fn associate(&self, region: &str, view_arn: &str) -> Answer {
        self.call(region, "AssociateDefaultView", json!({ "ViewArn": view_arn }))
            .await
    }

    /// `GET /tags/{resourceArn}` — `ListTagsForResource`.
    async fn list_tags(&self, region: &str, arn: &str) -> Answer {
        self.send(region, "GET", &format!("/tags/{}", label(arn)), None)
            .await
    }

    /// `DELETE /tags/{resourceArn}?tagKeys=…` — `UntagResource`.
    async fn untag(&self, region: &str, arn: &str, keys: &[&str]) -> Answer {
        let query = keys
            .iter()
            .map(|key| format!("tagKeys={}", label(key)))
            .collect::<Vec<_>>()
            .join("&");
        self.send(
            region,
            "DELETE",
            &format!("/tags/{}?{query}", label(arn)),
            None,
        )
        .await
    }

    /// `POST /GetDefaultView`, carrying NO body: the input is `smithy.api#Unit`.
    async fn get_default_view(&self, region: &str) -> Answer {
        self.send(region, "POST", "/GetDefaultView", None).await
    }
}

/// One URI path segment or query value, percent-encoded the way an SDK encodes a
/// non-greedy `@httpLabel`, so an ARN's `:` and `/` stay inside one segment.
fn label(value: &str) -> String {
    utf8_percent_encode(value, NON_ALPHANUMERIC).to_string()
}

/// `CreateIndex` in `region`, asserting success, and the index ARN it answered with.
async fn create_index(emulator: &Emulator, region: &str, token: &str) -> String {
    let answer = emulator
        .call(region, "CreateIndex", json!({ "ClientToken": token }))
        .await;
    answer.ok(region)["Arn"]
        .as_str()
        .expect("CreateIndex answers an Arn")
        .to_string()
}

/// `CreateView` in `region`, asserting success, and the view ARN it answered with.
async fn create_view(emulator: &Emulator, region: &str, name: &str) -> String {
    let answer = emulator
        .call(region, "CreateView", json!({ "ViewName": name }))
        .await;
    answer.ok(name)["View"]["ViewArn"]
        .as_str()
        .expect("CreateView answers a View.ViewArn")
        .to_string()
}

/// Every way a `ViewArn` can name no view this account holds, each paired with the case
/// name the assertions report. `held_index` is an index ARN the account really does
/// hold: a view lookup still misses on it, because it names a resource of another kind.
fn unresolvable_view_arns(region: &str, held_index: &str, deleted: &str) -> Vec<(String, String)> {
    vec![
        ("a view just deleted".to_string(), deleted.to_string()),
        (
            "a view id never minted".to_string(),
            engine::emulator_arn(
                "resource-explorer-2",
                region,
                "view/absent/0f8c2a1e-5d3b-8a47-9c61-2b7e4d905a13",
            ),
        ),
        (
            "a view in a Region with no index".to_string(),
            engine::emulator_arn(
                "resource-explorer-2",
                "ap-northeast-1",
                "view/absent/3a6e9c25-1b74-8de0-95f2-8c417b6d2035",
            ),
        ),
        (
            "another account's view".to_string(),
            format!(
                "arn:aws:resource-explorer-2:{region}:999999999999:view/other/\
                 5c2d8f31-7a46-8b92-ae03-1d49f6520b78"
            ),
        ),
        (
            "an index this account does hold, which is not a view".to_string(),
            held_index.to_string(),
        ),
        (
            "a view ARN in another partition".to_string(),
            format!(
                "arn:aws-cn:resource-explorer-2:{region}:000000000000:view/partition/\
                 9b1d7e40-2c58-8f36-a0e9-47d2b6315c08"
            ),
        ),
        (
            format!("a view ARN at the member's maximum length of {VIEW_ARN_MAX}"),
            view_arn_of_length(region, VIEW_ARN_MAX),
        ),
    ]
}

/// A well-formed view ARN of exactly `length` characters, padded in the view-name
/// segment — the one segment a view's name can make arbitrarily long — so a length bound
/// of the `ViewArn` member can be exercised with a value that really is an ARN.
fn view_arn_of_length(region: &str, length: usize) -> String {
    let id = "4e7c1b92-8d05-8f31-a26b-0c93d7154e68";
    let of_name = |name: &str| {
        engine::emulator_arn(
            "resource-explorer-2",
            region,
            &format!("view/{name}/{id}"),
        )
    };
    let probe = of_name("n");
    assert!(
        probe.len() <= length,
        "a view ARN of {length} characters must be at least as long as the shortest one, \
         which is {} characters",
        probe.len()
    );
    let padded = of_name(&"n".repeat(1 + length - probe.len()));
    assert_eq!(
        padded.len(),
        length,
        "case {length}: the padded ARN must be exactly {length} characters, got {padded:?}"
    );
    padded
}

/// `AssociateDefaultView` on a `ViewArn` that resolves to no view this account holds is
/// refused 404 `ResourceNotFoundException`, worded with AWS's fixed view-not-found
/// sentence followed by the ARN the request carried, verbatim.
///
/// The rows span every way a well-formed view ARN can fail to resolve — a deleted view,
/// identifiers never minted, a Region with no index, another account, another partition,
/// an ARN of the service's OTHER resource kind, and an ARN at the member's published
/// maximum length. The value past that maximum is refused by the modelled length
/// constraint instead, which is a different class, and a view the account DOES hold is
/// served: so the wording is shown to belong to the lookup miss and not to the operation.
#[tokio::test]
async fn associate_default_view_words_an_unresolvable_view_arn_as_aws_does() {
    let emulator = Emulator::new();
    let region = "us-east-1";
    let held_index = create_index(&emulator, region, "view-not-found-wording").await;
    let held = create_view(&emulator, region, "still-held").await;

    let deleted = create_view(&emulator, region, "deleted-view").await;
    emulator
        .call(region, "DeleteView", json!({ "ViewArn": deleted.clone() }))
        .await
        .ok("delete a view to unresolve its ARN");

    // The Region has a default to begin with, so a refused association is also shown to
    // leave the pointer where it was.
    emulator
        .associate(region, &held)
        .await
        .ok("associate the held view");

    for (case, arn) in unresolvable_view_arns(region, &held_index, &deleted) {
        let answer = emulator.associate(region, &arn).await;
        answer.failed(&case, 404, "ResourceNotFoundException");
        assert_eq!(
            answer.error_message(),
            format!("{VIEW_NOT_FOUND_PREFIX}{arn}"),
            "case {case:?}: the view-not-found class carries AWS's fixed text and the \
             ARN the request named, got {}",
            answer.body
        );
        let reported = emulator.get_default_view(region).await;
        assert_eq!(
            reported.ok(&case)["ViewArn"].as_str(),
            Some(held.as_str()),
            "case {case:?}: a refused association leaves the Region's default alone, \
             got {}",
            reported.body
        );
    }

    // One character past the maximum is the LENGTH constraint's refusal, not this class:
    // nothing is looked up, so nothing can be reported as not found.
    let too_long = view_arn_of_length(region, VIEW_ARN_MAX + 1);
    let answer = emulator.associate(region, &too_long).await;
    let case = format!("a view ARN one character past the maximum of {VIEW_ARN_MAX}");
    answer.failed(&case, 400, "ValidationException");
    assert!(
        !answer.error_message().contains(VIEW_NOT_FOUND_PREFIX),
        "case {case:?}: a value the constraint refuses is not reported as a missing \
         view, got {:?}",
        answer.error_message()
    );

    // The positive control: the view the account holds is still associated by the same
    // operation, so the refusals above are about the ARN and not about the call.
    let answer = emulator.associate(region, &held).await;
    assert_eq!(
        answer.ok("the held view")["ViewArn"].as_str(),
        Some(held.as_str()),
        "AssociateDefaultView answers the ARN it stored, got {}",
        answer.body
    );
}

/// The same fact reported by the OTHER operations that resolve a caller-supplied ARN
/// against the store — the tagging operations, which address either resource kind — is
/// worded the same way when the ARN names a view, and names the index instead when the
/// ARN names an index: the wording follows the kind of resource the ARN names, not the
/// operation that reported it.
#[tokio::test]
async fn every_operation_words_a_missing_view_the_same_way() {
    let emulator = Emulator::new();
    let region = "us-east-1";
    let held_index = create_index(&emulator, region, "tagging-view-not-found").await;
    let held_view = create_view(&emulator, region, "tagged-view").await;

    let deleted = create_view(&emulator, region, "deleted-tagged-view").await;
    emulator
        .call(region, "DeleteView", json!({ "ViewArn": deleted.clone() }))
        .await
        .ok("delete a view to unresolve its ARN");

    // The tagging routes authorize an ARN of another vendor before they resolve it, so
    // the rows here are the own-namespace view ARNs that name nothing.
    let view_rows: Vec<(String, String)> = unresolvable_view_arns(region, &held_index, &deleted)
        .into_iter()
        .filter(|(_, arn)| arn.contains(":view/"))
        .collect();
    assert!(
        view_rows.len() >= 3,
        "the table must still span several unresolvable view ARNs, got {}",
        view_rows.len()
    );

    for (case, arn) in view_rows {
        for (operation, answer) in [
            ("ListTagsForResource", emulator.list_tags(region, &arn).await),
            ("UntagResource", emulator.untag(region, &arn, &["k"]).await),
        ] {
            let case = format!("{operation} on {case}");
            answer.failed(&case, 404, "ResourceNotFoundException");
            assert_eq!(
                answer.error_message(),
                format!("{VIEW_NOT_FOUND_PREFIX}{arn}"),
                "case {case:?}: a missing view is worded the same whichever operation \
                 reports it, got {}",
                answer.body
            );
        }
    }

    // An ARN of the other kind the service owns is reported as the missing INDEX it
    // names, so the view wording is keyed on the resource kind rather than on the 404.
    let absent_index = engine::emulator_arn(
        "resource-explorer-2",
        region,
        "index/7d1f4b60-9e2c-8351-a4d8-6c3b05e71924",
    );
    let answer = emulator.list_tags(region, &absent_index).await;
    let case = "ListTagsForResource on an index id never minted";
    answer.failed(case, 404, "ResourceNotFoundException");
    let message = answer.error_message();
    assert!(
        !message.contains(VIEW_NOT_FOUND_PREFIX) && message.contains(&absent_index),
        "case {case:?}: an index ARN is reported as the index it names, got {message:?}"
    );

    // The positive controls: both resources the account DOES hold still answer their
    // tags, so the refusals above are about the ARN.
    for (case, arn) in [("the held view", &held_view), ("the held index", &held_index)] {
        let answer = emulator.list_tags(region, arn).await;
        let tags = answer.ok(case).get("Tags").cloned().unwrap_or(Value::Null);
        assert_eq!(
            tags.as_object().map(|map| map.len()),
            Some(BTreeMap::<String, String>::new().len()),
            "case {case:?}: a resource that exists answers an empty tag map, got {tags}"
        );
    }
}

// ---------------------------------------------------------------------------
// A body the model's own rules do not admit is one fixed sentence
// ---------------------------------------------------------------------------

/// The WHOLE message this service's body rejection carries. Spelled out here rather than
/// imported, because the sentence itself is the requirement: it names neither the member
/// that failed nor the value the request submitted, so there is no variable part.
const INVALID_REQUEST_BODY: &str = "Invalid request body";

/// The minimum every paginated input's `MaxResults` declares
/// (`smithy_query(query="shape",shape="ListResourcesInput,SearchInput,ListIndexesInput,ListViewsInput,GetResourceExplorerSetupInput")`
/// -> `MaxResults` `range {min: 1, max: …}`). Spelled out independently of the crate's
/// own constants, so a wrong bound in `src/` fails here rather than agreeing with itself.
const MAX_RESULTS_MIN: i64 = 1;

/// The published maximum of `ListResourcesInput$MaxResults` and `SearchInput$MaxResults`,
/// from the same query.
const QUERY_MAX_RESULTS_MAX: i64 = 1000;

/// The published maximum of `ListIndexesInput$MaxResults` and
/// `GetResourceExplorerSetupInput$MaxResults`.
const INDEX_MAX_RESULTS_MAX: i64 = 100;

/// The published maximum of `ListViewsInput$MaxResults`.
const VIEWS_MAX_RESULTS_MAX: i64 = 50;

/// `ListResourcesInput$NextToken` is `length {min: 1, max: 2048}`, so the empty string is
/// outside it (`smithy_query(query="shape",shape="ListResourcesInput")` -> `NextToken`).
const EMPTY_NEXT_TOKEN: &str = "";

/// `ListResourcesInput$ViewArn` is `length {min: 0, max: 1000}` — a BODY member, unlike
/// the `@httpLabel` `resourceArn` of the tagging routes
/// (`smithy_query(query="shape",shape="ListResourcesInput")` -> `ViewArn`).
const LIST_RESOURCES_VIEW_ARN_MAX: usize = 1000;

/// `BatchGetViewInput$ViewArns` is a list of `length {min: 1, max: 20}`
/// (`smithy_query(query="shape",shape="BatchGetViewInput")`).
const VIEW_ARNS_MIN: usize = 1;
const VIEW_ARNS_MAX: usize = 20;

/// `ListIndexesInput$Regions` is a list of `length {min: 0, max: 20}`
/// (`smithy_query(query="shape",shape="ListIndexesInput")`).
const REGIONS_MAX: usize = 20;

/// A `TaskId` inside the `^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$`
/// pattern `GetResourceExplorerSetupInput` declares for it, so the rows that break a
/// DIFFERENT member of that input break only the one they are about.
const WELL_FORMED_TASK_ID: &str = "3f6b9d21-47ac-4e58-9b02-5c1d8e734a6f";

/// One request whose BODY breaks a rule the model declares, with the text its refusal must
/// not carry: the member at fault and the value the request submitted.
struct BadBody {
    case: String,
    operation: &'static str,
    body: Value,
    withheld: Vec<String>,
}

/// One such row.
fn bad(case: impl Into<String>, operation: &'static str, body: Value, withheld: &[&str]) -> BadBody {
    BadBody {
        case: case.into(),
        operation,
        body,
        withheld: withheld.iter().map(|text| (*text).to_string()).collect(),
    }
}

/// A string of exactly `length` `v` characters: a `ViewArn` past its member's published
/// maximum is about the LENGTH of the value, not about what it names.
fn padded(length: usize) -> String {
    "v".repeat(length)
}

/// Every request body this service refuses, spanning the operations that bind members to
/// the body, the constraint classes those members declare, and the values on the refused
/// side of each published bound.
fn bad_bodies() -> Vec<BadBody> {
    let mut rows = Vec::new();

    // A page size, for each operation that takes one and each published maximum the model
    // declares — the refused side of both ends of the range, and each end of the member's
    // own `integer` range, which is as far past the bound as a value can be sent.
    let page_sizes: [(&'static str, Value, i64); 5] = [
        ("ListResources", json!({}), QUERY_MAX_RESULTS_MAX),
        ("Search", json!({ "QueryString": "" }), QUERY_MAX_RESULTS_MAX),
        ("ListIndexes", json!({}), INDEX_MAX_RESULTS_MAX),
        ("ListViews", json!({}), VIEWS_MAX_RESULTS_MAX),
        (
            "GetResourceExplorerSetup",
            json!({ "TaskId": WELL_FORMED_TASK_ID }),
            INDEX_MAX_RESULTS_MAX,
        ),
    ];
    for (operation, base, max) in page_sizes {
        for value in [
            MAX_RESULTS_MIN - 1,
            -MAX_RESULTS_MIN,
            max + 1,
            max + 1000,
            i64::from(i32::MAX),
            i64::from(i32::MIN),
        ] {
            let mut body = base.clone();
            body["MaxResults"] = json!(value);
            rows.push(bad(
                format!("{operation} MaxResults {value}, outside 1..={max}"),
                operation,
                body,
                &[&value.to_string(), "MaxResults", "maxResults"],
            ));
        }
    }

    // A string length: both members of a query that carry one, and the `ClientToken` of a
    // create.
    rows.push(bad(
        "ListResources NextToken shorter than its minimum of 1",
        "ListResources",
        json!({ "NextToken": EMPTY_NEXT_TOKEN }),
        &["NextToken", "nextToken"],
    ));
    rows.push(bad(
        format!(
            "ListResources ViewArn one character past its maximum of \
             {LIST_RESOURCES_VIEW_ARN_MAX}"
        ),
        "ListResources",
        json!({ "ViewArn": padded(LIST_RESOURCES_VIEW_ARN_MAX + 1) }),
        &["ViewArn", "viewArn", "vvv"],
    ));
    rows.push(bad(
        "CreateView ClientToken shorter than its minimum of 1",
        "CreateView",
        json!({ "ViewName": "token-length", "ClientToken": "" }),
        &["ClientToken", "clientToken"],
    ));
    rows.push(bad(
        "UpdateView ViewArn shorter than its minimum of 1",
        "UpdateView",
        json!({ "ViewArn": "" }),
        &["ViewArn", "viewArn"],
    ));

    // A collection size: a list shorter than its minimum, and one longer than its maximum.
    rows.push(bad(
        format!("BatchGetView ViewArns shorter than its minimum of {VIEW_ARNS_MIN}"),
        "BatchGetView",
        json!({ "ViewArns": Vec::<String>::new() }),
        &["ViewArns", "viewArns"],
    ));
    rows.push(bad(
        format!("BatchGetView ViewArns one past its maximum of {VIEW_ARNS_MAX}"),
        "BatchGetView",
        json!({ "ViewArns": vec![padded(8); VIEW_ARNS_MAX + 1] }),
        &["ViewArns", "viewArns", "vvv"],
    ));
    rows.push(bad(
        format!("ListIndexes Regions one past its maximum of {REGIONS_MAX}"),
        "ListIndexes",
        json!({ "Regions": vec![padded(6); REGIONS_MAX + 1] }),
        &["Regions", "regions", "vvv"],
    ));

    // A required BODY member the request omits: found while READING the body rather than
    // while walking the value it built, and the same failure of the same check to a caller.
    rows.push(bad(
        "Search with no QueryString, which its input requires",
        "Search",
        json!({}),
        &["QueryString", "queryString"],
    ));
    rows.push(bad(
        "CreateView with no ViewName, which its input requires",
        "CreateView",
        json!({}),
        &["ViewName", "viewName"],
    ));
    rows.push(bad(
        "UpdateView with no ViewArn, which its input requires",
        "UpdateView",
        json!({}),
        &["ViewArn", "viewArn"],
    ));

    rows
}

/// A request BODY that breaks any rule the model declares for the operation's input is
/// refused with HTTP 400, the modelled `ValidationException` code and ONE fixed sentence,
/// which carries neither the member at fault nor the value the request submitted — while
/// every value the model DOES admit, each published bound included, is served.
///
/// The rows span the operations that bind members to the body, the constraint classes the
/// model declares over those members (an integer range, a string length, a collection
/// size, an absent required member) and the values just outside and just inside each
/// published bound. The recorded case — a page size one past the maximum of 1000 — is one
/// row of the first table.
#[tokio::test]
async fn a_body_outside_the_models_own_rules_is_refused_with_one_fixed_sentence() {
    let emulator = Emulator::new();
    let region = "us-east-1";
    // Turning the Region on leaves it an index and a default view, so every row below is
    // refused (or served) for what its BODY carries and not for want of a view to query.
    create_index(&emulator, region, "invalid-request-body").await;
    let held_view = create_view(&emulator, region, "body-rules").await;

    for row in bad_bodies() {
        let answer = emulator.call(region, row.operation, row.body.clone()).await;
        answer.failed(&row.case, 400, "ValidationException");
        assert_eq!(
            answer.error_message(),
            INVALID_REQUEST_BODY,
            "case {:?}: a refused body carries this service's one fixed sentence, got {}",
            row.case,
            answer.body
        );
        let rendered = answer.body.to_string();
        for text in &row.withheld {
            assert!(
                !rendered.contains(text.as_str()),
                "case {:?}: the refusal states neither the member nor the value, so it \
                 cannot carry {text:?}, got {rendered}",
                row.case
            );
        }
    }

    // The other side of every bound: the values the model admits are served, so the
    // sentence above belongs to what the model refuses and not to the member itself.
    let served: Vec<(String, &str, Value)> = vec![
        (
            format!("ListResources MaxResults at its minimum of {MAX_RESULTS_MIN}"),
            "ListResources",
            json!({ "MaxResults": MAX_RESULTS_MIN }),
        ),
        (
            format!("ListResources MaxResults at its maximum of {QUERY_MAX_RESULTS_MAX}"),
            "ListResources",
            json!({ "MaxResults": QUERY_MAX_RESULTS_MAX }),
        ),
        (
            format!("Search MaxResults at its minimum of {MAX_RESULTS_MIN}"),
            "Search",
            json!({ "QueryString": "", "MaxResults": MAX_RESULTS_MIN }),
        ),
        (
            format!("Search MaxResults at its maximum of {QUERY_MAX_RESULTS_MAX}"),
            "Search",
            json!({ "QueryString": "", "MaxResults": QUERY_MAX_RESULTS_MAX }),
        ),
        (
            format!("ListIndexes MaxResults at its maximum of {INDEX_MAX_RESULTS_MAX}"),
            "ListIndexes",
            json!({ "MaxResults": INDEX_MAX_RESULTS_MAX }),
        ),
        (
            format!("ListViews MaxResults at its maximum of {VIEWS_MAX_RESULTS_MAX}"),
            "ListViews",
            json!({ "MaxResults": VIEWS_MAX_RESULTS_MAX }),
        ),
        (
            format!("ListIndexes Regions at its maximum of {REGIONS_MAX}"),
            "ListIndexes",
            json!({ "Regions": vec![region.to_string(); REGIONS_MAX] }),
        ),
        (
            format!("BatchGetView ViewArns at its minimum of {VIEW_ARNS_MIN}"),
            "BatchGetView",
            json!({ "ViewArns": vec![held_view.clone(); VIEW_ARNS_MIN] }),
        ),
        (
            format!("BatchGetView ViewArns at its maximum of {VIEW_ARNS_MAX}"),
            "BatchGetView",
            json!({ "ViewArns": vec![held_view.clone(); VIEW_ARNS_MAX] }),
        ),
        (
            format!(
                "ListResources ViewArn at its maximum length of \
                 {LIST_RESOURCES_VIEW_ARN_MAX}"
            ),
            "ListResources",
            json!({ "ViewArn": held_view.clone() }),
        ),
    ];
    for (case, operation, body) in served {
        emulator.call(region, operation, body).await.ok(&case);
    }
}

/// The sentence above belongs to the BODY-schema check alone: a rule the model states only
/// in prose is refused by the operation that owns it, naming the member at fault — the
/// discriminator a client has, since every one of these refusals carries the same
/// `ValidationException` code.
///
/// (A value this service's model binds OUTSIDE a body — the `@httpLabel` `resourceArn` and
/// the `@httpQuery` `tagKeys` of the tagging routes — declares no constraint beyond
/// `required`, and a request that omits a required query parameter matches no route at
/// all, so there is no URI-bound constraint refusal to state here.)
#[tokio::test]
async fn a_rule_the_model_states_in_prose_keeps_the_wording_that_names_it() {
    let emulator = Emulator::new();
    let region = "us-east-1";
    create_index(&emulator, region, "rules-outside-the-body").await;

    // A filter string carrying free-form text: a restriction `ListResourcesInput$Filters`
    // documents in prose, which no constraint in the model expresses, so the operation
    // refuses it and names the member it is about.
    let case = "ListResources with a free-form term in its filter string";
    let answer = emulator
        .call(
            region,
            "ListResources",
            json!({ "Filters": { "FilterString": "orders" } }),
        )
        .await;
    answer.failed(case, 400, "ValidationException");
    let message = answer.error_message();
    assert!(
        message != INVALID_REQUEST_BODY && message.contains("Filters.FilterString"),
        "case {case:?}: a prose rule is refused naming the member it is about, got \
         {message:?}"
    );

    // A tag filter through a view that does not include the `tags` property: another rule
    // of the operation's own, which names the filter at fault.
    let plain = create_view(&emulator, region, "no-tags-property").await;
    let case = "ListResources filtering on tags through a view without the property";
    let answer = emulator
        .call(
            region,
            "ListResources",
            json!({ "ViewArn": plain, "Filters": { "FilterString": "tag:stage=prod" } }),
        )
        .await;
    answer.failed(case, 400, "ValidationException");
    let message = answer.error_message();
    assert!(
        message != INVALID_REQUEST_BODY && message.contains("not included in the view"),
        "case {case:?}: a prose rule is refused naming what it is about, got {message:?}"
    );

    // The positive control: the same operation serves the requests that break no rule, so
    // the refusals above are about the request and not about the call.
    for (case, body) in [
        (
            "ListResources with a filter prefix",
            json!({ "Filters": { "FilterString": "service:sqs" } }),
        ),
        (
            "ListResources through the view without the tags property",
            json!({ "ViewArn": plain, "Filters": { "FilterString": "service:sqs" } }),
        ),
    ] {
        emulator.call(region, "ListResources", body).await.ok(case);
    }
}
