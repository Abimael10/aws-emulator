// SPDX-License-Identifier: CC0-1.0

//! What `ListProvisioningTemplateVersions` reports, end to end over HTTP.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a restJson1
//! request signed for the `iot` signing name — so what is asserted is the status, the
//! members and the `message` an SDK surfaces, not an internal function's return value.
//!
//! The rule under test: the listing reports every version the named template holds —
//! oldest-first, each summary the version's own `versionId`, `creationDate` and whether it
//! is the version in effect — in pages of at most `maxResults`, with a `nextToken` issued
//! only while a next set exists; a `templateName` the account holds no template under names
//! no version set to page through and is refused.
//!
//! The table spans what the rule generalises over: the page size (every value from one
//! version per page to past the whole set, and a request that names none), how many versions
//! the template holds, a version that was deleted, which version is in effect, and the
//! `templateName` value classes the model admits
//! (`smithy_query(query="operation",operation="ListProvisioningTemplateVersions")`:
//! `length 1..36`, `^[0-9A-Za-z_-]+$`).

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region this suite signs for. None of this behaviour is regional.
const REGION: &str = "ap-southeast-2";
/// The role a template provisions under: its form is the caller's, so the suite uses one
/// well-formed ARN throughout.
const PROVISIONING_ROLE: &str = "arn:aws:iam::000000000000:role/IoTProvisioningRole";
/// How many versions one provisioning template holds, which is what bounds the rows below.
const MAX_VERSIONS: i64 = 5;

/// One emulator: the shared engine plus a router with this service mounted.
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

    async fn dispatch(&self, method: &str, path: &str, body: Option<&Value>) -> Reply {
        let host = format!("iot.{REGION}.amazonaws.com");
        let payload = body.map_or_else(Bytes::new, |value| Bytes::from(value.to_string()));
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://{host}{path}"))
            .header("host", host)
            .header("content-type", "application/json")
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/iot/aws4_request, \
                     SignedHeaders=host, Signature=0123456789abcdef"
                ),
            )
            .body(payload)
            .expect("the provisioning-template request is well formed");
        Reply::of(self.router.dispatch(request).await)
    }

    async fn create_template(&self, template_name: &str) -> Reply {
        self.dispatch(
            "POST",
            "/provisioning-templates",
            Some(&json!({
                "templateName": template_name,
                "templateBody": template_body(1),
                "provisioningRoleArn": PROVISIONING_ROLE,
            })),
        )
        .await
    }

    async fn create_version(&self, template_name: &str, index: usize, set_as_default: bool) -> Reply {
        self.dispatch(
            "POST",
            &format!(
                "/provisioning-templates/{template_name}/versions?setAsDefault={set_as_default}"
            ),
            Some(&json!({ "templateBody": template_body(index) })),
        )
        .await
    }

    async fn delete_version(&self, template_name: &str, version_id: i64) -> Reply {
        self.dispatch(
            "DELETE",
            &format!("/provisioning-templates/{template_name}/versions/{version_id}"),
            None,
        )
        .await
    }

    async fn describe_version(&self, template_name: &str, version_id: i64) -> Reply {
        self.dispatch(
            "GET",
            &format!("/provisioning-templates/{template_name}/versions/{version_id}"),
            None,
        )
        .await
    }

    async fn point_default_at(&self, template_name: &str, version_id: i64) -> Reply {
        self.dispatch(
            "PATCH",
            &format!("/provisioning-templates/{template_name}"),
            Some(&json!({ "defaultVersionId": version_id })),
        )
        .await
    }

    /// One page of a template's versions: the raw reply, so a case reads its status too.
    async fn list_versions(
        &self,
        template_name: &str,
        max_results: Option<i64>,
        next_token: Option<&str>,
    ) -> Reply {
        let mut query: Vec<String> = Vec::new();
        if let Some(max_results) = max_results {
            query.push(format!("maxResults={max_results}"));
        }
        if let Some(next_token) = next_token {
            query.push(format!("nextToken={next_token}"));
        }
        let suffix = if query.is_empty() {
            String::new()
        } else {
            format!("?{}", query.join("&"))
        };
        self.dispatch(
            "GET",
            &format!("/provisioning-templates/{template_name}/versions{suffix}"),
            None,
        )
        .await
    }

    /// Every version the listing reports, read page by page until it issues no further
    /// token, with how many pages that took.
    async fn every_version(&self, template_name: &str, max_results: Option<i64>) -> (Vec<Value>, usize) {
        let mut rows: Vec<Value> = Vec::new();
        let mut pages = 0usize;
        let mut token: Option<String> = None;
        loop {
            let reply = self
                .list_versions(template_name, max_results, token.as_deref())
                .await;
            assert_eq!(
                reply.status,
                200,
                "case maxResults={max_results:?} page {pages}: expected 200, got {} body {}",
                reply.status,
                reply.text()
            );
            let body = reply.json();
            let page = body["versions"]
                .as_array()
                .unwrap_or_else(|| panic!("no versions array in {}", reply.text()))
                .clone();
            if let Some(max_results) = max_results {
                assert!(
                    page.len() as i64 <= max_results,
                    "case maxResults={max_results} page {pages}: expected at most {max_results} \
                     rows, got {}",
                    page.len()
                );
            }
            rows.extend(page);
            pages += 1;
            token = body["nextToken"].as_str().map(str::to_string);
            if token.is_none() {
                return (rows, pages);
            }
            assert!(
                pages <= MAX_VERSIONS as usize + 1,
                "case maxResults={max_results:?}: the listing kept issuing tokens past \
                 {pages} pages"
            );
        }
    }
}

/// A dispatched response, reduced to what the assertions read.
struct Reply {
    status: u16,
    body: Bytes,
}

impl Reply {
    fn of(response: http::Response<Bytes>) -> Self {
        Self {
            status: response.status().as_u16(),
            body: response.into_body(),
        }
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|e| panic!("response body is not JSON ({e}): {}", self.text()))
    }

    fn error_code(&self) -> String {
        self.json()["__type"]
            .as_str()
            .unwrap_or_else(|| panic!("no __type in {}", self.text()))
            .to_string()
    }

    fn error_message(&self) -> String {
        self.json()["message"]
            .as_str()
            .unwrap_or_else(|| panic!("no message in {}", self.text()))
            .to_string()
    }
}

/// A provisioning template body, numbered so each version differs.
fn template_body(index: usize) -> String {
    json!({
        "Parameters": { "SerialNumber": { "Type": "String" } },
        "Resources": {
            "thing": {
                "Type": "AWS::IoT::Thing",
                "Properties": { "ThingName": format!("device-{index}") },
            },
        },
    })
    .to_string()
}

/// The `versionId` of every row of a listing, in the order the listing reported them.
fn version_ids(rows: &[Value]) -> Vec<i64> {
    rows.iter()
        .map(|row| {
            row["versionId"]
                .as_i64()
                .unwrap_or_else(|| panic!("a listing row carries no versionId: {row}"))
        })
        .collect()
}

/// A template holding `versions` versions in all, the last of them the default.
async fn template_with_versions(emulator: &Emulator, template_name: &str, versions: i64) -> Vec<i64> {
    assert_eq!(
        emulator.create_template(template_name).await.status,
        200,
        "case {template_name}: the template every row addresses must be creatable"
    );
    for index in 2..=versions {
        let created = emulator
            .create_version(template_name, index as usize, true)
            .await;
        assert_eq!(
            created.status,
            200,
            "case {template_name} version {index}: expected 200, got {} body {}",
            created.status,
            created.text()
        );
    }
    (1..=versions).collect()
}

/// RULE: the listing reports every version the template holds, oldest-first, in pages of at
/// most `maxResults`, and issues a token only while a next set exists.
#[tokio::test]
async fn every_version_is_listed_oldest_first_however_the_pages_are_sized() {
    // The rows span the version counts a template can hold, from a template that has only
    // ever had its create's version to one at the cap.
    for held in 1..=MAX_VERSIONS {
        let emulator = Emulator::new();
        let template_name = format!("fleet_template-{held}");
        let expected = template_with_versions(&emulator, &template_name, held).await;

        // The page sizes span one version per page, every size up to the whole set, the
        // size past it, the largest `maxResults` the model admits (`range 1..250`) and a
        // request that names no page size at all.
        let page_sizes: Vec<Option<i64>> = (1..=held + 1)
            .map(Some)
            .chain([Some(250), None])
            .collect();

        for max_results in page_sizes {
            let (rows, pages) = emulator.every_version(&template_name, max_results).await;
            assert_eq!(
                version_ids(&rows),
                expected,
                "case {template_name} maxResults={max_results:?}: expected {expected:?} \
                 oldest-first, got {:?}",
                version_ids(&rows)
            );
            // A page size of n over m versions takes ceil(m/n) pages, and the last of them
            // is what carries no token; a request naming no size takes exactly one.
            // Signed arithmetic, so the rounding is written out rather than taken from
            // `div_ceil`, which is unstable for a signed count.
            let expected_pages = max_results
                .map_or(1, |size| (held + size - 1) / size)
                .max(1) as usize;
            assert_eq!(
                pages, expected_pages,
                "case {template_name} maxResults={max_results:?}: expected {expected_pages} \
                 page(s), got {pages}"
            );
        }
    }
}

/// RULE: each summary is the version's own — the `versionId`, `creationDate` and
/// `isDefaultVersion` the reader of that version reports — so the listing and the reader
/// cannot disagree, including after the version in effect moves.
#[tokio::test]
async fn a_summary_is_the_version_the_reader_reports() {
    let emulator = Emulator::new();
    let template_name = "Fleet_Template-2";
    let held = template_with_versions(&emulator, template_name, 4).await;

    // The default is moved back to an older version, so `isDefaultVersion` is read off the
    // template's pointer rather than from the newest version.
    for default_version_id in [4, 1, 3] {
        let repointed = emulator
            .point_default_at(template_name, default_version_id)
            .await;
        assert_eq!(
            repointed.status,
            200,
            "case default={default_version_id}: expected 200, got {} body {}",
            repointed.status,
            repointed.text()
        );

        let (rows, _pages) = emulator.every_version(template_name, Some(2)).await;
        assert_eq!(
            version_ids(&rows),
            held,
            "case default={default_version_id}: expected {held:?}, got {:?}",
            version_ids(&rows)
        );
        for row in &rows {
            let version_id = row["versionId"].as_i64().expect("a row carries a versionId");
            let read = emulator.describe_version(template_name, version_id).await;
            assert_eq!(
                read.status,
                200,
                "case default={default_version_id} version {version_id}: the listed version \
                 must be readable, got {} body {}",
                read.status,
                read.text()
            );
            let described = read.json();
            assert_eq!(
                (
                    row["versionId"].clone(),
                    row["creationDate"].clone(),
                    row["isDefaultVersion"].clone()
                ),
                (
                    described["versionId"].clone(),
                    described["creationDate"].clone(),
                    json!(version_id == default_version_id)
                ),
                "case default={default_version_id} version {version_id}: expected the row to \
                 be the version the reader reports, got {row} against {described}"
            );
        }
    }
}

/// RULE: the listing follows the version set — a version that is deleted leaves it, and one
/// that is created joins it under a NEWLY minted id: one past the highest the template has
/// ever handed out, so no id a deleted version held is ever handed out again.
#[tokio::test]
async fn the_listing_follows_the_version_set() {
    let emulator = Emulator::new();
    let template_name = "a";
    template_with_versions(&emulator, template_name, 3).await;

    // Versions 1 and 2 are no longer the default (version 3 is), so both are deletable.
    for (case, deleted, expected) in [
        ("the oldest version", 1_i64, vec![2_i64, 3]),
        ("the version in the middle", 2, vec![3]),
    ] {
        let reply = emulator.delete_version(template_name, deleted).await;
        assert_eq!(
            reply.status,
            200,
            "case {case}: expected 200, got {} body {}",
            reply.status,
            reply.text()
        );
        let (rows, _pages) = emulator.every_version(template_name, None).await;
        assert_eq!(
            version_ids(&rows),
            expected,
            "case {case}: expected {expected:?}, got {:?}",
            version_ids(&rows)
        );
    }

    // Every id this template has ever handed out, so each row below checks the id it mints
    // against all of them and not only against the versions still held. Versions 1 and 2
    // are gone by now, which is what makes the check about the ids and not the set.
    let mut ever_minted: Vec<i64> = vec![1, 2, 3];

    // The rows span a create after the delete of an OLDER version and a create after the
    // delete of the HIGHEST one — the case where reusing the top of the live set would hand
    // the same id out twice.
    for case in [
        "a version added after the delete of older versions",
        "a version added after the delete of the highest version",
    ] {
        let created = emulator.create_version(template_name, 9, false).await;
        assert_eq!(
            created.status,
            200,
            "case {case}: expected 200, got {} body {}",
            created.status,
            created.text()
        );
        let added = created.json()["versionId"]
            .as_i64()
            .expect("a created version carries its id");
        assert!(
            !ever_minted.contains(&added),
            "case {case}: expected an id past every one ever minted ({ever_minted:?}), got \
             {added}"
        );
        ever_minted.push(added);

        // The listing reports it in the position its id puts it — after the versions still
        // held, not in the gap a delete left.
        let (rows, _pages) = emulator.every_version(template_name, Some(1)).await;
        assert_eq!(
            version_ids(&rows),
            vec![3, added],
            "case {case}: expected [3, {added}], got {:?}",
            version_ids(&rows)
        );

        // Deleting it again leaves the template at version 3 alone, which sets up the next
        // row: the highest id ever minted is now one the template no longer holds.
        let reply = emulator.delete_version(template_name, added).await;
        assert_eq!(
            reply.status,
            200,
            "case {case}: the version just added must be deletable, got {} body {}",
            reply.status,
            reply.text()
        );
    }
}

/// RULE: nothing is written past the cap on a template's versions — the create is refused
/// with the sentence stating that the template's versions are used up, and the listing
/// still reports exactly the versions the template holds.
#[tokio::test]
async fn the_listing_reports_the_capped_version_set_and_nothing_past_it() {
    let emulator = Emulator::new();
    let template_name = "capped-template";
    let expected = template_with_versions(&emulator, template_name, MAX_VERSIONS).await;

    // Past the cap, every further create is refused the same way, however many are tried.
    for attempt in 1..=3 {
        let refused = emulator
            .create_version(template_name, MAX_VERSIONS as usize + attempt, true)
            .await;
        let message = format!(
            "Can't create a template version for the template {template_name}, the maximum \
             number of template versions has been reached"
        );
        assert_eq!(
            (
                refused.status,
                refused.error_code(),
                refused.error_message()
            ),
            (
                409,
                "VersionsLimitExceededException".to_string(),
                message.clone()
            ),
            "case create past the cap, attempt {attempt}: expected 409 \
             VersionsLimitExceededException {message:?}, got {} {}",
            refused.status,
            refused.text()
        );

        let (rows, _pages) = emulator.every_version(template_name, None).await;
        assert_eq!(
            version_ids(&rows),
            expected,
            "case create past the cap, attempt {attempt}: expected {expected:?}, got {:?}",
            version_ids(&rows)
        );
    }
}

/// RULE: a `templateName` the account holds no template under names no version set, and the
/// listing refuses it with its own sentence — the template is not found.
#[tokio::test]
async fn a_template_the_account_does_not_hold_has_no_version_set() {
    let emulator = Emulator::new();
    let held = "held-template";
    template_with_versions(&emulator, held, 2).await;

    // `templateName` is `length 1..36`, `^[0-9A-Za-z_-]+$`: the shortest name, an ordinary
    // one, each punctuation the pattern admits, and the longest.
    let missing = [
        "z".to_string(),
        "fleet-template".to_string(),
        "Fleet_Template-9".to_string(),
        "m".repeat(36),
    ];
    for template_name in &missing {
        let reply = emulator.list_versions(template_name, None, None).await;
        let expected = format!("Template named {template_name} not found");
        assert_eq!(
            (reply.status, reply.error_code(), reply.error_message()),
            (
                404,
                "ResourceNotFoundException".to_string(),
                expected.clone()
            ),
            "case the missing template {template_name:?}: expected 404 \
             ResourceNotFoundException {expected:?}, got {} {}",
            reply.status,
            reply.text()
        );
    }
}

/// RULE: a `nextToken` is this listing's own position in it — a token no page of this
/// listing issued is refused rather than resuming some other listing from the middle.
#[tokio::test]
async fn a_token_this_listing_never_issued_is_refused() {
    let emulator = Emulator::new();
    template_with_versions(&emulator, "first-template", 3).await;
    template_with_versions(&emulator, "second-template", 3).await;

    let issued = emulator
        .list_versions("second-template", Some(1), None)
        .await
        .json()["nextToken"]
        .as_str()
        .expect("a truncated listing issues a token")
        .to_string();

    // The rows span a token of ANOTHER template's listing and the forms a caller can
    // invent: not hex, hex of an odd length, hex of something no listing issued.
    for (case, token) in [
        ("a token of another template's listing", issued.as_str()),
        ("a token that is not hex", "not-a-token"),
        ("a token of an odd hex length", "abc"),
        ("a token that is hex of nothing this listing issued", "6162"),
    ] {
        let reply = emulator
            .list_versions("first-template", Some(2), Some(token))
            .await;
        assert_eq!(
            (reply.status, reply.error_code()),
            (400, "InvalidRequestException".to_string()),
            "case {case}: expected 400 InvalidRequestException, got {} {}",
            reply.status,
            reply.text()
        );
    }

    // The token the listing DID issue resumes it, so the refusals above are the token's
    // provenance and not the paging.
    let resumed = emulator
        .list_versions("second-template", Some(1), Some(&issued))
        .await;
    assert_eq!(
        resumed.status,
        200,
        "case the listing's own token: expected 200, got {} body {}",
        resumed.status,
        resumed.text()
    );
    assert_eq!(
        version_ids(
            resumed.json()["versions"]
                .as_array()
                .expect("a served page carries its versions")
        ),
        vec![2],
        "case the listing's own token: expected the version after the first page, got {}",
        resumed.text()
    );
}
