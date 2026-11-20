// SPDX-License-Identifier: CC0-1.0

//! Which values an `apiId` resolves a GraphQL API from, over the whole class of references
//! AppSync publishes for one.
//!
//! The rule these cases assert, one level above the request that exposed the defect (a
//! `GetGraphqlApi` handed the ARN CloudFormation recorded as the API resource's physical id):
//!
//! > AppSync publishes a GraphQL API's identity in TWO forms — the `apiId` it minted and the
//! > ARN built on that id, which a create answers together and which every read of the API
//! > reports side by side — so an operation addressed at an `apiId` resolves EITHER form to
//! > the same API, reaches the same records held under it, and reports a reference of either
//! > form that names no API this account holds as the missing resource it is: 404
//! > `NotFoundException` with AppSync's fixed sentence (`state::API_NOT_FOUND_MESSAGE`).
//! > Only a value that is NEITHER form names no API at all, which is the malformed request
//! > 400 `BadRequestException` echoes ("Invalid API ID `<apiId>`").
//!
//! The rows span what the rule generalises over: the FORM the reference takes (the minted id
//! and the ARN built on it), the KIND of operation it is handed to (a read of the API, reads
//! of the collections held under it, a write under it, and the delete of the API itself), and
//! — for the absent class — why the reference resolves to nothing (an API deleted after it
//! was published, an ARN naming an id that was never minted, and an ARN of another Region or
//! another account).
//!
//! The harness is the one the error-contract suite carries (the router the production
//! emulator mounts, `call`), reused rather than copied; the reference travels in the single
//! non-greedy `{apiId}` path segment percent-encoded, the way an AWS SDK sends one.

use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use serde_json::{json, Value};
use service_com_amazonaws_appsync_awsdeepdishcontrolplaneservice::state;
use smithy_server_runtime::MultiServiceRouter;

use super::error_contract::{call, router};

/// One created API, in both forms AppSync publishes its identity in.
#[derive(Debug)]
struct Published {
    api_id: String,
    arn: String,
}

impl Published {
    /// The two references a caller can be holding for this API, each labelled by its form.
    fn references(&self) -> [(&'static str, &str); 2] {
        [("the minted id", &self.api_id), ("the API's ARN", &self.arn)]
    }

    /// The Region the ARN was built in, read off the ARN itself.
    fn region(&self) -> &str {
        self.arn
            .split(':')
            .nth(3)
            .expect("an ARN carries its Region in the fourth field")
    }

    /// An ARN of the same Region and account naming `api_id` instead.
    fn arn_naming(&self, api_id: &str) -> String {
        let (prefix, _) = self
            .arn
            .rsplit_once('/')
            .expect("an appsync API ARN ends in apis/<apiId>");
        format!("{prefix}/{api_id}")
    }
}

/// Create one GraphQL API and read back both forms of its identity.
async fn publish(router: &MultiServiceRouter, name: &str) -> Published {
    let answer = call(
        router,
        "POST",
        "/v1/apis",
        Some(json!({"name": name, "authenticationType": "API_KEY"})),
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "case create {name:?}: expected 200, got {answer:?}"
    );
    let api = &answer.body["graphqlApi"];
    let published = Published {
        api_id: api["apiId"].as_str().unwrap_or_default().to_string(),
        arn: api["arn"].as_str().unwrap_or_default().to_string(),
    };
    // The create answers the PAIR this rule is about, so every row below rests on what the
    // service really published rather than on an ARN written out here.
    assert!(
        published
            .arn
            .ends_with(&format!(":apis/{}", published.api_id))
            && published.arn.starts_with("arn:"),
        "case create {name:?}: expected an appsync API ARN built on the minted id, got \
         {answer:?}"
    );
    published
}

/// `reference` as it travels in the single non-greedy `{apiId}` path segment.
fn label(reference: &str) -> String {
    utf8_percent_encode(reference, NON_ALPHANUMERIC).to_string()
}

/// The calls that address an API by an `apiId`, one per kind: a read of the API itself,
/// reads of three collections held under it, and a write under it — each a
/// `(operation, method, path suffix, body)` to be sent under one reference. `suffix` names
/// the record the write creates, so the same table can be sent under both forms.
fn addressed(suffix: &str) -> Vec<(&'static str, &'static str, String, Option<Value>)> {
    vec![
        ("GetGraphqlApi", "GET", String::new(), None),
        ("ListDataSources", "GET", "/datasources".to_string(), None),
        ("ListApiKeys", "GET", "/apikeys".to_string(), None),
        ("ListFunctions", "GET", "/functions".to_string(), None),
        (
            "CreateDataSource",
            "POST",
            "/datasources".to_string(),
            Some(json!({"name": format!("source_{suffix}"), "type": "NONE"})),
        ),
    ]
}

#[tokio::test]
async fn either_published_form_of_an_api_reference_addresses_the_same_api() {
    let router = router();
    let published = publish(&router, "both-forms-address-one-api").await;

    for (index, (form, reference)) in published.references().into_iter().enumerate() {
        for (operation, method, suffix, body) in addressed(&format!("form{index}")) {
            let case = format!("{operation} addressed by {form}");
            let path = format!("/v1/apis/{}{suffix}", label(reference));
            let answer = call(&router, method, &path, body).await;
            assert_eq!(
                answer.status, 200,
                "case {case:?}: expected 200, got {answer:?}"
            );
        }
    }

    // The two forms are not two APIs: each read answers about the one API, so the same read
    // answers the same body under either reference.
    for (operation, suffix) in [
        ("GetGraphqlApi", ""),
        ("ListDataSources", "/datasources"),
        ("ListApiKeys", "/apikeys"),
        ("ListTypes", "/types?format=SDL"),
    ] {
        let by_id = call(
            &router,
            "GET",
            &format!("/v1/apis/{}{suffix}", label(&published.api_id)),
            None,
        )
        .await;
        let by_arn = call(
            &router,
            "GET",
            &format!("/v1/apis/{}{suffix}", label(&published.arn)),
            None,
        )
        .await;
        assert_eq!(
            (by_arn.status, &by_arn.body),
            (by_id.status, &by_id.body),
            "case {operation} under either form: expected one API's answer, got {by_arn:?} \
             against {by_id:?}"
        );
    }
}

#[tokio::test]
async fn a_write_addressed_by_the_arn_lands_on_the_api_the_id_names() {
    let router = router();
    let published = publish(&router, "a-write-under-either-form").await;

    // Created under the ARN, read back under the id: a record is held under the API's own id,
    // so the two calls address one resource.
    let created = call(
        &router,
        "POST",
        &format!("/v1/apis/{}/datasources", label(&published.arn)),
        Some(json!({"name": "written_under_the_arn", "type": "NONE"})),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "case CreateDataSource under the ARN: expected 200, got {created:?}"
    );
    let read = call(
        &router,
        "GET",
        &format!(
            "/v1/apis/{}/datasources/written_under_the_arn",
            label(&published.api_id)
        ),
        None,
    )
    .await;
    assert_eq!(
        (read.status, read.body["dataSource"]["name"].as_str()),
        (200, Some("written_under_the_arn")),
        "case GetDataSource under the id: expected the data source written under the ARN, \
         got {read:?}"
    );
    // And the record's own ARN is built under the API's minted id, not under the reference
    // the request was written with.
    assert_eq!(
        read.body["dataSource"]["dataSourceArn"].as_str(),
        Some(
            format!("{}/datasources/written_under_the_arn", published.arn).as_str()
        ),
        "case GetDataSource under the id: expected the record's ARN under the API's own ARN, \
         got {read:?}"
    );

    // The other way round: created under the id, read back under the ARN.
    let created = call(
        &router,
        "POST",
        &format!("/v1/apis/{}/datasources", label(&published.api_id)),
        Some(json!({"name": "written_under_the_id", "type": "NONE"})),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "case CreateDataSource under the id: expected 200, got {created:?}"
    );
    let read = call(
        &router,
        "GET",
        &format!(
            "/v1/apis/{}/datasources/written_under_the_id",
            label(&published.arn)
        ),
        None,
    )
    .await;
    assert_eq!(
        (read.status, read.body["dataSource"]["name"].as_str()),
        (200, Some("written_under_the_id")),
        "case GetDataSource under the ARN: expected the data source written under the id, \
         got {read:?}"
    );

    // A key created under the ARN is a key of that API, so the listing under the id reports
    // it: what is held under the API is one collection, not one per form.
    let key = call(
        &router,
        "POST",
        &format!("/v1/apis/{}/apikeys", label(&published.arn)),
        Some(json!({})),
    )
    .await;
    assert_eq!(
        key.status, 200,
        "case CreateApiKey under the ARN: expected 200, got {key:?}"
    );
    let listed = call(
        &router,
        "GET",
        &format!("/v1/apis/{}/apikeys", label(&published.api_id)),
        None,
    )
    .await;
    assert_eq!(
        (listed.status, listed.body["apiKeys"].as_array().map(Vec::len)),
        (200, Some(1)),
        "case ListApiKeys under the id: expected the key created under the ARN, got {listed:?}"
    );
}

#[tokio::test]
async fn a_reference_that_names_no_api_is_a_not_found_in_either_form() {
    let router = router();
    let published = publish(&router, "absent-in-either-form").await;
    let standing = publish(&router, "left-standing").await;

    // The API is deleted THROUGH its ARN, so the delete resolves the published reference too.
    let deleted = call(
        &router,
        "DELETE",
        &format!("/v1/apis/{}", label(&published.arn)),
        None,
    )
    .await;
    assert_eq!(
        deleted.status, 200,
        "case DeleteGraphqlApi under the ARN: expected 200, got {deleted:?}"
    );

    // One API was deleted, not every API: the one left standing still resolves.
    let read = call(
        &router,
        "GET",
        &format!("/v1/apis/{}", label(&standing.arn)),
        None,
    )
    .await;
    assert_eq!(
        read.status, 200,
        "case GetGraphqlApi on the API left standing: expected 200, got {read:?}"
    );

    // Every way a reference of a published form can resolve to nothing. The id in the
    // never-minted row is of the form the service issues, so what makes it absent is that
    // nothing was created under it.
    let absent = [
        ("the deleted API's id", published.api_id.clone()),
        ("the deleted API's ARN", published.arn.clone()),
        (
            "an ARN naming an id that was never minted",
            published.arn_naming("zzzzzzzzzzzzzzzzzzzzzzzzzz"),
        ),
        (
            "an ARN of another Region",
            published.arn.replace(published.region(), "ap-south-1"),
        ),
        (
            "an ARN of another account",
            published
                .arn
                .replace(engine::EMULATOR_ACCOUNT_ID, "999999999999"),
        ),
    ];

    for (form, reference) in &absent {
        for (operation, method, suffix, body) in addressed("absent") {
            let case = format!("{operation} addressed by {form}");
            let answer = call(
                &router,
                method,
                &format!("/v1/apis/{}{suffix}", label(reference)),
                body,
            )
            .await;
            assert_eq!(
                (
                    answer.status,
                    answer.error_type.as_str(),
                    answer.message.as_str()
                ),
                (404, "NotFoundException", state::API_NOT_FOUND_MESSAGE),
                "case {case:?}: expected 404 NotFoundException {:?}, got {answer:?}",
                state::API_NOT_FOUND_MESSAGE
            );
        }
    }

    // The classification is unchanged for a value of NEITHER published form: it names no API
    // at all, so the request itself is malformed and the refusal echoes the rejected value.
    let malformed = [
        (
            "one character past the id length",
            "a".repeat(state::API_ID_LEN + 1),
        ),
        (
            "a token of another form altogether",
            "f3a91c07e6b5d0a48c2e1976badcafe0".to_string(),
        ),
    ];
    for (form, reference) in &malformed {
        let answer = call(
            &router,
            "GET",
            &format!("/v1/apis/{}", label(reference)),
            None,
        )
        .await;
        assert_eq!(
            (
                answer.status,
                answer.error_type.as_str(),
                answer.message.as_str()
            ),
            (
                400,
                "BadRequestException",
                format!("Invalid API ID {reference}").as_str()
            ),
            "case GetGraphqlApi addressed by {form}: expected 400 BadRequestException naming \
             the rejected value, got {answer:?}"
        );
    }
}
