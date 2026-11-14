// SPDX-License-Identifier: CC0-1.0

//! The WORDING of the refusal that reports no view exists for the ARN a request named.
//!
//! The class is one fact — "the `ViewArn` you gave resolves to no view this account
//! holds" — so every operation that resolves a view ARN against the store and reports
//! that fact says it with the same fixed sentence, followed by the ARN the REQUEST
//! carried, verbatim and unaltered. The rows below span every way a well-formed view ARN
//! can fail to resolve, and the published maximum length of the member that carries it,
//! so the message is shown to be a template over the request rather than text tied to one
//! value.
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
