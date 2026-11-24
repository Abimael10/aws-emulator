// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of the Security Hub CSPM member association slice:
//! `EnableSecurityHub` → `CreateMembers` → `GetMembers`, and of the organization
//! slice beside it: `EnableOrganizationAdminAccount` →
//! `ListOrganizationAdminAccounts`.
//!
//! Every case drives the real restJson1 wire path through `MultiServiceRouter`, so a
//! route, a status code or a serialized member that is wrong on the wire fails here.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region most cases sign for; the handler reads it out of the SigV4 credential
/// scope, exactly as a signed SDK request carries it.
const REGION: &str = "us-east-1";

/// A second Region, to pin that member associations are scoped to one hub.
const OTHER_REGION: &str = "us-west-2";

/// The account every emulated caller presents — the administrator account of every
/// association these cases create.
const ADMINISTRATOR_ID: &str = "000000000000";

/// `Member$MemberStatus` after `CreateMembers`: "Created - Indicates that the
/// administrator account added the member account, but has not yet invited the member
/// account" (`smithy_query(query="shape", shape="Member")`, member `MemberStatus`).
const STATUS_CREATED: &str = "Created";

/// `AdminStatus$ENABLED`: the status of a designated delegated administrator
/// (`smithy_query(query="shape", shape="AdminStatus")`).
const ADMIN_STATUS_ENABLED: &str = "ENABLED";

/// Every value of `SecurityHubFeature` (`smithy_query(query="shape",
/// shape="SecurityHubFeature")`), plus the absent parameter, which "defaults to
/// Security Hub CSPM" — the first value below.
const SECURITY_HUB_FEATURES: [&str; 2] = ["SecurityHub", "SecurityHubV2"];

/// `AdminsMaxResults`' published bounds: `@range(min: 1, max: 10)`
/// (`smithy_query(query="operation", operation="ListOrganizationAdminAccounts")`).
const ADMINS_MAX_RESULTS_MIN: usize = 1;
const ADMINS_MAX_RESULTS_MAX: usize = 10;

/// One emulator with the Security Hub service mounted, driven over HTTP.
struct Harness {
    router: MultiServiceRouter,
}

impl Harness {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_securityhub_securityhubapiservice::EmulatedService::new(engine),
        );
        Self { router }
    }

    /// Dispatch one request signed for `region` and return `(status, parsed body)`.
    async fn call(
        &self,
        method: &str,
        path: &str,
        region: &str,
        body: Option<Value>,
    ) -> (u16, Value) {
        let serialized = body.map(|b| serde_json::to_vec(&b).expect("serializable request body"));
        let request = http::Request::builder()
            .method(method)
            .uri(path)
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20261021/{region}/securityhub/aws4_request, \
                     SignedHeaders=host, Signature=0000"
                ),
            )
            .header("content-type", "application/json")
            .body(bytes::Bytes::from(serialized.unwrap_or_default()))
            .expect("valid request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let raw = response.into_body();
        let parsed = if raw.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&raw).unwrap_or(Value::Null)
        };
        (status, parsed)
    }

    async fn enable(&self, region: &str) -> (u16, Value) {
        self.call("POST", "/accounts", region, Some(json!({}))).await
    }

    async fn create_members(&self, region: &str, account_details: Value) -> (u16, Value) {
        self.call(
            "POST",
            "/members",
            region,
            Some(json!({ "AccountDetails": account_details })),
        )
        .await
    }

    async fn get_members(&self, region: &str, account_ids: &[&str]) -> (u16, Value) {
        self.call(
            "POST",
            "/members/get",
            region,
            Some(json!({ "AccountIds": account_ids })),
        )
        .await
    }

    /// A harness whose caller is subscribed in `REGION`.
    async fn enabled() -> Self {
        let harness = Self::new();
        let (status, body) = harness.enable(REGION).await;
        assert_eq!(
            status, 200,
            "EnableSecurityHub must succeed, got {status} with {body}"
        );
        harness
    }

    /// One `EnableOrganizationAdminAccount` call. `Feature` is sent only when named,
    /// so a row can exercise the documented default.
    async fn enable_organization_admin_account(
        &self,
        region: &str,
        admin_account_id: &str,
        feature: Option<&str>,
    ) -> (u16, Value) {
        let body = match feature {
            Some(feature) => {
                json!({ "AdminAccountId": admin_account_id, "Feature": feature })
            }
            None => json!({ "AdminAccountId": admin_account_id }),
        };
        self.call("POST", "/organization/admin/enable", region, Some(body))
            .await
    }

    /// One `ListOrganizationAdminAccounts` call. Every parameter is `@httpQuery`-bound
    /// (`smithy_query(query="operation",
    /// operation="ListOrganizationAdminAccounts")`), so each is sent on the query
    /// string exactly as an SDK would.
    async fn list_organization_admin_accounts(
        &self,
        region: &str,
        feature: Option<&str>,
        max_results: Option<usize>,
        next_token: Option<&str>,
    ) -> (u16, Value) {
        let mut query: Vec<String> = Vec::new();
        if let Some(feature) = feature {
            query.push(format!("Feature={feature}"));
        }
        if let Some(max_results) = max_results {
            query.push(format!("MaxResults={max_results}"));
        }
        if let Some(next_token) = next_token {
            query.push(format!("NextToken={next_token}"));
        }
        let path = if query.is_empty() {
            "/organization/admin".to_string()
        } else {
            format!("/organization/admin?{}", query.join("&"))
        };
        self.call("GET", &path, region, None).await
    }
}

/// One `CreateMembers` batch as a case writes it: an account id and the `Email` the
/// `AccountDetails` entry carries, if any.
type Batch = Vec<(String, Option<String>)>;

/// A 12-digit account id built from `n`, so a case can span batch sizes without a
/// hand-written list.
fn account_id(n: usize) -> String {
    format!("{:012}", 100_000_000_000u64 + n as u64)
}

/// One `AccountDetails` entry, with `Email` present only when supplied.
fn detail(account_id: &str, email: Option<&str>) -> Value {
    match email {
        Some(email) => json!({ "AccountId": account_id, "Email": email }),
        None => json!({ "AccountId": account_id }),
    }
}

/// The instant a modelled `Timestamp` member carries, in epoch seconds. `Member$UpdatedAt`
/// is `@timestampFormat("date-time")`, so it arrives as an RFC 3339 string (pinned by
/// `tests/all/timestamp_wire_form.rs`); an epoch number is read too, so the assertions
/// below are about the instant and not its encoding.
fn instant_seconds(value: &Value) -> Option<f64> {
    if let Some(seconds) = value.as_f64() {
        return Some(seconds);
    }
    let text = value.as_str()?;
    aws_smithy_types::DateTime::from_str(text, aws_smithy_types::date_time::Format::DateTime)
        .ok()
        .map(|parsed| parsed.as_secs_f64())
}

/// The `Members` list of a `GetMembers` response.
fn members_of(body: &Value) -> &Vec<Value> {
    body["Members"]
        .as_array()
        .unwrap_or_else(|| panic!("GetMembers must answer a Members list, got {body}"))
}

/// The `UnprocessedAccounts` list of a response.
fn unprocessed_of(body: &Value) -> &Vec<Value> {
    body["UnprocessedAccounts"]
        .as_array()
        .unwrap_or_else(|| panic!("response must carry UnprocessedAccounts, got {body}"))
}

/// RULE: `CreateMembers` creates one member association per `AccountDetails` entry of
/// the calling account's hub, in the `Created` status with the administrator account
/// on both `AdministratorId` and the deprecated `MasterId`, the submitted `Email` when
/// one was given, an `UpdatedAt` instant and no `InvitedAt` (no invitation has been
/// sent); `GetMembers` reports exactly those associations, in the order asked for.
///
/// The table spans what the rule generalises over: how many accounts one call carries
/// (1 → the modelled maximum of 50), whether each entry carries an `Email`, and
/// whether the same account appears twice in one call.
#[tokio::test]
async fn create_members_round_trips_every_batch_shape() {
    // `AccountDetailsList`: "len 1..50" (`smithy_query(query="operation",
    // operation="CreateMembers")`), so 1 and 50 are the published bounds.
    let max_batch: Batch = (0..50)
        .map(|n| {
            let id = account_id(n);
            let email = (n % 2 == 0).then(|| format!("member{n}@example.com"));
            (id, email)
        })
        .collect();

    let cases: Vec<(&str, Batch)> = vec![
        (
            "one account with an email",
            vec![("111111111111".to_string(), Some("one@example.com".to_string()))],
        ),
        (
            "one account without an email",
            vec![("222222222222".to_string(), None)],
        ),
        (
            "three accounts, some with an email",
            vec![
                ("333333333333".to_string(), Some("three@example.com".to_string())),
                ("444444444444".to_string(), None),
                ("555555555555".to_string(), Some("five@example.com".to_string())),
            ],
        ),
        (
            "the same account twice in one call",
            vec![
                ("666666666666".to_string(), None),
                ("666666666666".to_string(), Some("six@example.com".to_string())),
            ],
        ),
        ("the maximum batch of 50 accounts", max_batch),
    ];

    for (case, rows) in cases {
        let harness = Harness::enabled().await;
        let details: Vec<Value> = rows
            .iter()
            .map(|(id, email)| detail(id, email.as_deref()))
            .collect();
        let (status, body) = harness.create_members(REGION, json!(details)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateMembers expected 200, got {status} with {body}"
        );
        assert!(
            unprocessed_of(&body).is_empty(),
            "case {case:?}: every account must be processed, got {body}"
        );

        // The association a repeated account id ends up with is the LAST entry for
        // it, so collapse the request the way the store does — keyed by account id,
        // last write wins — and read it back in reverse request order to pin that
        // `GetMembers` answers in the order it was asked.
        let mut expected: Batch = Vec::new();
        for (id, email) in &rows {
            match expected.iter_mut().find(|(seen, _)| seen == id) {
                Some(entry) => {
                    if email.is_some() {
                        entry.1 = email.clone();
                    }
                }
                None => expected.push((id.clone(), email.clone())),
            }
        }
        expected.reverse();

        let requested: Vec<&str> = expected.iter().map(|(id, _)| id.as_str()).collect();
        let (status, body) = harness.get_members(REGION, &requested).await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetMembers expected 200, got {status} with {body}"
        );
        assert!(
            unprocessed_of(&body).is_empty(),
            "case {case:?}: every created account must be found, got {body}"
        );
        let members = members_of(&body);
        assert_eq!(
            members.len(),
            expected.len(),
            "case {case:?}: expected {} members, got {body}",
            expected.len()
        );
        for (index, (id, email)) in expected.iter().enumerate() {
            let member = &members[index];
            assert_eq!(
                member["AccountId"].as_str(),
                Some(id.as_str()),
                "case {case:?}: member {index} must be {id}, got {member}"
            );
            assert_eq!(
                member["Email"].as_str(),
                email.as_deref(),
                "case {case:?}: member {id} Email must be {email:?}, got {member}"
            );
            assert_eq!(
                member["MemberStatus"].as_str(),
                Some(STATUS_CREATED),
                "case {case:?}: member {id} must be {STATUS_CREATED}, got {member}"
            );
            assert_eq!(
                member["AdministratorId"].as_str(),
                Some(ADMINISTRATOR_ID),
                "case {case:?}: member {id} AdministratorId must be the caller, got {member}"
            );
            assert_eq!(
                member["MasterId"].as_str(),
                Some(ADMINISTRATOR_ID),
                "case {case:?}: member {id} MasterId must mirror AdministratorId, got {member}"
            );
            assert!(
                instant_seconds(&member["UpdatedAt"]).is_some_and(|seconds| seconds > 0.0),
                "case {case:?}: member {id} must carry an UpdatedAt instant, got {member}"
            );
            assert!(
                member["InvitedAt"].is_null(),
                "case {case:?}: member {id} was never invited, so InvitedAt must be absent, \
                 got {member}"
            );
        }
    }
}

/// RULE: re-creating an account that is already a member of the calling hub is
/// idempotent — it takes over the details the call carries and leaves the association
/// in the status it already had, rather than failing or duplicating it.
#[tokio::test]
async fn re_creating_a_member_updates_it_in_place() {
    let harness = Harness::enabled().await;
    let id = "777777777777";

    // Each row is one more CreateMembers call for the same account, with the Email it
    // carries and the Email the association must report afterwards.
    let rows = [
        ("first create, with an email", Some("first@example.com"), Some("first@example.com")),
        ("re-create without an email keeps the stored one", None, Some("first@example.com")),
        ("re-create with a new email replaces it", Some("second@example.com"), Some("second@example.com")),
    ];

    let mut previous_updated_at = 0.0_f64;
    for (case, submitted, expected_email) in rows {
        let (status, body) = harness
            .create_members(REGION, json!([detail(id, submitted)]))
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateMembers expected 200, got {status} with {body}"
        );
        assert!(
            unprocessed_of(&body).is_empty(),
            "case {case:?}: the account must be processed, got {body}"
        );

        let (status, body) = harness.get_members(REGION, &[id]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetMembers expected 200, got {status} with {body}"
        );
        let members = members_of(&body);
        assert_eq!(
            members.len(),
            1,
            "case {case:?}: the account must be a member exactly once, got {body}"
        );
        assert_eq!(
            members[0]["Email"].as_str(),
            expected_email,
            "case {case:?}: expected Email {expected_email:?}, got {}",
            members[0]
        );
        assert_eq!(
            members[0]["MemberStatus"].as_str(),
            Some(STATUS_CREATED),
            "case {case:?}: expected {STATUS_CREATED}, got {}",
            members[0]
        );
        let updated_at = instant_seconds(&members[0]["UpdatedAt"]).unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected an UpdatedAt instant, got {}",
                members[0]
            )
        });
        assert!(
            updated_at >= previous_updated_at,
            "case {case:?}: UpdatedAt must not go backwards, got {updated_at} after \
             {previous_updated_at}"
        );
        previous_updated_at = updated_at;
    }
}

/// RULE: an account the call cannot associate is reported per account in
/// `UnprocessedAccounts` with a reason, and the accounts in the SAME call that can be
/// associated are still created. The administrator account is one such account: it is
/// the other end of the association, so it cannot be its own member. A later
/// `GetMembers` for it answers nothing at all: it is a report of stored associations,
/// and the hub holds none for that id.
#[tokio::test]
async fn the_administrator_account_is_reported_unprocessed() {
    let harness = Harness::enabled().await;
    let member = "888888888888";
    let (status, body) = harness
        .create_members(
            REGION,
            json!([
                detail(ADMINISTRATOR_ID, Some("admin@example.com")),
                detail(member, None),
            ]),
        )
        .await;
    assert_eq!(
        status, 200,
        "CreateMembers expected 200, got {status} with {body}"
    );
    let unprocessed = unprocessed_of(&body);
    assert_eq!(
        unprocessed.len(),
        1,
        "only the administrator account must be unprocessed, got {body}"
    );
    assert_eq!(
        unprocessed[0]["AccountId"].as_str(),
        Some(ADMINISTRATOR_ID),
        "expected the administrator account to be unprocessed, got {}",
        unprocessed[0]
    );
    assert!(
        unprocessed[0]["ProcessingResult"]
            .as_str()
            .is_some_and(|reason| !reason.is_empty()),
        "an unprocessed account must say why, got {}",
        unprocessed[0]
    );

    // The administrator never became a member, and the other account in the same
    // call did.
    let (status, body) = harness.get_members(REGION, &[ADMINISTRATOR_ID, member]).await;
    assert_eq!(
        status, 200,
        "GetMembers expected 200, got {status} with {body}"
    );
    let members = members_of(&body);
    assert_eq!(
        members.len(),
        1,
        "only {member} must be a member, got {body}"
    );
    assert_eq!(
        members[0]["AccountId"].as_str(),
        Some(member),
        "expected {member}, got {}",
        members[0]
    );
    assert!(
        unprocessed_of(&body).is_empty(),
        "the hub holds no association for the administrator account, so GetMembers must \
         report it in neither list, got {body}"
    );
}

/// RULE: an account id that is not a 12-digit Amazon Web Services account id is an
/// invalid input parameter — `InvalidInputException` (400) — and the request is
/// refused whole, so none of the accounts it carries is created.
#[tokio::test]
async fn malformed_account_ids_are_refused() {
    // One row per way an id can miss the 12-digit form, plus the published bounds
    // (11 and 13 digits).
    let cases = [
        ("empty", ""),
        ("too short", "123"),
        ("one digit short", "11111111111"),
        ("one digit long", "1111111111111"),
        ("not all digits", "00000000000a"),
        ("padded with a space", " 00000000000"),
        ("hyphenated", "0000-0000-000"),
    ];
    let valid = "999999999999";

    for (case, bad) in cases {
        let harness = Harness::enabled().await;
        let (status, body) = harness
            .create_members(REGION, json!([detail(valid, None), detail(bad, None)]))
            .await;
        assert_eq!(
            status, 400,
            "case {case:?} ({bad:?}): CreateMembers expected 400, got {status} with {body}"
        );

        let (status, body) = harness.get_members(REGION, &[valid]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetMembers expected 200, got {status} with {body}"
        );
        assert!(
            members_of(&body).is_empty(),
            "case {case:?}: a refused request must persist nothing, got {body}"
        );
    }

    // `GetMembers` applies the same rule to the ids it is asked about. The empty id
    // is left out of this half: `AccountIdList`'s member is a `NonEmptyString`
    // (pattern `\S`), so the framework's boundary validation refuses it before the
    // handler runs and answers `ValidationException` rather than this rule's
    // `InvalidInputException`.
    let harness = Harness::enabled().await;
    for (case, bad) in cases.iter().filter(|(_, bad)| !bad.trim().is_empty()) {
        let (status, body) = harness.get_members(REGION, &[bad]).await;
        assert_eq!(
            status, 400,
            "case {case:?} ({bad:?}): GetMembers expected 400, got {status} with {body}"
        );
    }
}

/// RULE: the bounds a member operation enforces are refused AT the bound and accepted
/// below it: an `Email` is kept up to the longest address SMTP allows (RFC 5321's
/// 254-character forward path) and refused past it, a `GetMembers` call addresses at
/// most the batch the model bounds `CreateMembers`' own list to (50) and is refused
/// past it, and both refusals persist nothing.
#[tokio::test]
async fn member_input_bounds_are_enforced_at_their_edges() {
    // The published bound, one below it and one past it. The address keeps a valid
    // local@domain form at every length, so only its LENGTH differs between rows.
    const MAX_EMAIL_LEN: usize = 254;
    let email_of = |len: usize| {
        let suffix = "@example.com";
        format!("{}{suffix}", "a".repeat(len - suffix.len()))
    };
    let email_rows = [
        ("one below the maximum email length", MAX_EMAIL_LEN - 1, 200),
        ("exactly the maximum email length", MAX_EMAIL_LEN, 200),
        ("one past the maximum email length", MAX_EMAIL_LEN + 1, 400),
    ];

    for (case, len, expected) in email_rows {
        let harness = Harness::enabled().await;
        let member = "141414141414";
        let email = email_of(len);
        assert_eq!(email.chars().count(), len, "case {case:?}: test built a {} character address", email.chars().count());
        let (status, body) = harness
            .create_members(REGION, json!([detail(member, Some(&email))]))
            .await;
        assert_eq!(
            status, expected,
            "case {case:?}: CreateMembers expected {expected}, got {status} with {body}"
        );

        let (status, body) = harness.get_members(REGION, &[member]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetMembers expected 200, got {status} with {body}"
        );
        assert_eq!(
            members_of(&body).len(),
            usize::from(expected == 200),
            "case {case:?}: only an accepted Email may be persisted, got {body}"
        );
    }

    // `CreateMembers`' own list is bounded by the model (`AccountDetailsList`, len
    // 1..50); `GetMembers`' `AccountIds` carries no such trait, so the handler holds
    // the same bound. Rows: the bound, and one past it.
    const MAX_BATCH: usize = 50;
    let harness = Harness::enabled().await;
    for (case, count, expected) in [
        ("the maximum batch", MAX_BATCH, 200),
        ("one past the maximum batch", MAX_BATCH + 1, 400),
    ] {
        let ids: Vec<String> = (0..count).map(account_id).collect();
        let requested: Vec<&str> = ids.iter().map(String::as_str).collect();
        let (status, body) = harness.get_members(REGION, &requested).await;
        assert_eq!(
            status, expected,
            "case {case:?} ({count} ids): GetMembers expected {expected}, got {status} with {body}"
        );
    }
}

/// RULE: a hub holds at most the published maximum of member accounts; a create that
/// would take it past that is the modelled `LimitExceededException` (429), and
/// re-creating accounts the hub already holds stays inside the bound.
#[tokio::test]
async fn the_member_account_maximum_is_enforced() {
    // The AWS Security Hub quotas table publishes 1,000 member accounts per
    // administrator account.
    const MAX_MEMBERS: usize = 1_000;
    let harness = Harness::enabled().await;

    // Fill the hub in calls of the modelled maximum batch size.
    for chunk_start in (0..MAX_MEMBERS).step_by(50) {
        let details: Vec<Value> = (chunk_start..chunk_start + 50)
            .map(|n| detail(&account_id(n), None))
            .collect();
        let (status, body) = harness.create_members(REGION, json!(details)).await;
        assert_eq!(
            status, 200,
            "filling at {chunk_start}: CreateMembers expected 200, got {status} with {body}"
        );
    }

    // Re-creating accounts the hub already holds adds nothing, so it is still inside
    // the bound.
    let held = account_id(0);
    let (status, body) = harness
        .create_members(REGION, json!([detail(&held, Some("again@example.com"))]))
        .await;
    assert_eq!(
        status, 200,
        "re-creating an existing member expected 200, got {status} with {body}"
    );

    // One more NEW account is past the bound.
    let one_too_many = account_id(MAX_MEMBERS);
    let (status, body) = harness
        .create_members(REGION, json!([detail(&one_too_many, None)]))
        .await;
    assert_eq!(
        status, 429,
        "the {}th member expected 429, got {status} with {body}",
        MAX_MEMBERS + 1
    );
    let (status, body) = harness.get_members(REGION, &[&one_too_many]).await;
    assert_eq!(
        status, 200,
        "GetMembers expected 200, got {status} with {body}"
    );
    assert!(
        members_of(&body).is_empty(),
        "a refused create must persist nothing, got {body}"
    );
}

/// RULE: the member operations act on the calling account's hub, so they are refused
/// with `InvalidAccessException` (401) while that account is not subscribed in the
/// Region of the call — "This action can only be used by an account that has Security
/// Hub CSPM enabled".
#[tokio::test]
async fn member_operations_need_a_subscribed_account() {
    let harness = Harness::new();
    let member = "121212121212";

    let (status, body) = harness
        .create_members(REGION, json!([detail(member, None)]))
        .await;
    assert_eq!(
        status, 401,
        "CreateMembers before EnableSecurityHub: expected 401, got {status} with {body}"
    );
    let (status, body) = harness.get_members(REGION, &[member]).await;
    assert_eq!(
        status, 401,
        "GetMembers before EnableSecurityHub: expected 401, got {status} with {body}"
    );

    // Subscribing in one Region does not subscribe the account in another.
    let (status, body) = harness.enable(REGION).await;
    assert_eq!(status, 200, "EnableSecurityHub: got {status} with {body}");
    let (status, body) = harness
        .create_members(OTHER_REGION, json!([detail(member, None)]))
        .await;
    assert_eq!(
        status, 401,
        "CreateMembers in an unsubscribed Region: expected 401, got {status} with {body}"
    );
}

/// RULE: a member association belongs to one hub — the administrator account AND the
/// Region it was created in — so the same account id is a member in that Region only.
#[tokio::test]
async fn members_are_scoped_to_the_hub_that_created_them() {
    let harness = Harness::new();
    for region in [REGION, OTHER_REGION] {
        let (status, body) = harness.enable(region).await;
        assert_eq!(
            status, 200,
            "EnableSecurityHub in {region}: got {status} with {body}"
        );
    }

    let member = "131313131313";
    let (status, body) = harness
        .create_members(REGION, json!([detail(member, Some("east@example.com"))]))
        .await;
    assert_eq!(
        status, 200,
        "CreateMembers in {REGION}: got {status} with {body}"
    );

    for (region, expect_member) in [(REGION, true), (OTHER_REGION, false)] {
        let (status, body) = harness.get_members(region, &[member]).await;
        assert_eq!(
            status, 200,
            "GetMembers in {region}: expected 200, got {status} with {body}"
        );
        assert_eq!(
            members_of(&body).len(),
            usize::from(expect_member),
            "GetMembers in {region}: expected member present = {expect_member}, got {body}"
        );
        assert!(
            unprocessed_of(&body).is_empty(),
            "GetMembers in {region}: an id that is not a member there is reported in \
             neither list, got {body}"
        );
    }
}

/// The `AdminAccounts` list of a `ListOrganizationAdminAccounts` response.
fn admin_accounts_of(body: &Value) -> &Vec<Value> {
    body["AdminAccounts"].as_array().unwrap_or_else(|| {
        panic!("ListOrganizationAdminAccounts must answer an AdminAccounts list, got {body}")
    })
}

/// RULE: `EnableOrganizationAdminAccount` designates the account it names as the
/// organization's delegated administrator for the feature of the call (Security Hub
/// CSPM when the call names none) in the Region it was signed for, and answers that
/// designation; `ListOrganizationAdminAccounts` reports exactly the designation the
/// same feature and Region holds, `ENABLED`, and nothing for any other feature or
/// Region.
///
/// The table spans what the rule generalises over: the account designated, the feature
/// (absent, so the documented default, and each value `SecurityHubFeature` declares)
/// and the Region the call is signed for.
#[tokio::test]
async fn designating_an_organization_administrator_round_trips_per_feature_and_region() {
    // Rows: the feature as the request carries it, and the feature the designation is
    // therefore filed under.
    let features = [
        (None, SECURITY_HUB_FEATURES[0]),
        (Some(SECURITY_HUB_FEATURES[0]), SECURITY_HUB_FEATURES[0]),
        (Some(SECURITY_HUB_FEATURES[1]), SECURITY_HUB_FEATURES[1]),
    ];

    for (row, (requested, effective)) in features.iter().enumerate() {
        for region in [REGION, OTHER_REGION] {
            // A different account per row, so no row can pass on another's state.
            let admin = account_id(row * 2 + usize::from(region == OTHER_REGION));
            let case = format!("Feature={requested:?} in {region} for {admin}");
            let harness = Harness::new();

            let (status, body) = harness
                .enable_organization_admin_account(region, &admin, *requested)
                .await;
            assert_eq!(
                status, 200,
                "case {case:?}: EnableOrganizationAdminAccount expected 200, got {status} with \
                 {body}"
            );
            assert_eq!(
                body["AdminAccountId"].as_str(),
                Some(admin.as_str()),
                "case {case:?}: the response must name the designated account, got {body}"
            );
            assert_eq!(
                body["Feature"].as_str(),
                Some(*effective),
                "case {case:?}: expected Feature {effective}, got {body}"
            );

            // The designation is read back through the operation that reports it.
            let (status, body) = harness
                .list_organization_admin_accounts(region, *requested, None, None)
                .await;
            assert_eq!(
                status, 200,
                "case {case:?}: ListOrganizationAdminAccounts expected 200, got {status} with \
                 {body}"
            );
            let listed = admin_accounts_of(&body);
            assert_eq!(
                listed.len(),
                1,
                "case {case:?}: expected exactly the designated account, got {body}"
            );
            assert_eq!(
                listed[0]["AccountId"].as_str(),
                Some(admin.as_str()),
                "case {case:?}: expected {admin}, got {}",
                listed[0]
            );
            assert_eq!(
                listed[0]["Status"].as_str(),
                Some(ADMIN_STATUS_ENABLED),
                "case {case:?}: expected {ADMIN_STATUS_ENABLED}, got {}",
                listed[0]
            );
            assert_eq!(
                body["Feature"].as_str(),
                Some(*effective),
                "case {case:?}: the list must report the feature it answered for, got {body}"
            );
            assert!(
                body["NextToken"].is_null(),
                "case {case:?}: one designation is one page, so NextToken must be absent, got \
                 {body}"
            );

            // Every other feature, and every other Region, holds no designation: the
            // designation is scoped to the pair it was made for.
            let other_feature = SECURITY_HUB_FEATURES
                .iter()
                .find(|value| **value != *effective)
                .expect("SecurityHubFeature declares more than one value");
            let other_region = if region == REGION { OTHER_REGION } else { REGION };
            for (scope, scope_region, scope_feature) in [
                ("the other feature", region, Some(*other_feature)),
                ("the other Region", other_region, *requested),
            ] {
                let (status, body) = harness
                    .list_organization_admin_accounts(scope_region, scope_feature, None, None)
                    .await;
                assert_eq!(
                    status, 200,
                    "case {case:?} / {scope}: expected 200, got {status} with {body}"
                );
                assert!(
                    admin_accounts_of(&body).is_empty(),
                    "case {case:?} / {scope}: expected no designation, got {body}"
                );
            }
        }
    }
}

/// RULE: an `AdminAccountId` that is not a 12-digit Amazon Web Services account id is
/// an invalid input parameter — `InvalidInputException` (400) — and that is the
/// refusal whatever the call could otherwise do to the organization, so it holds both
/// on an organization with no delegated administrator and on one that already has a
/// different account designated; nothing is designated either way.
#[tokio::test]
async fn malformed_admin_account_ids_are_refused_before_the_designation_is_looked_at() {
    // One row per way an id can miss the 12-digit form, plus the published bounds (11
    // and 13 digits). The empty id is left out: `AdminAccountId` is a
    // `NonEmptyString` (pattern `\S`), so the boundary validation refuses it before
    // the handler runs.
    let malformed = [
        ("not an account id at all", "not-an-account"),
        ("too short", "123"),
        ("one digit short", "11111111111"),
        ("one digit long", "1111111111111"),
        ("not all digits", "00000000000a"),
        ("padded with a space", " 00000000000"),
        ("hyphenated", "0000-0000-000"),
    ];
    let designated = "919191919191";

    for (case, bad) in malformed {
        // The scopes differ in what the SAME call would otherwise run into: an
        // organization that has designated nobody, and one that already has a
        // DIFFERENT account designated — the state with its own refusal, which the
        // malformed parameter must take precedence over.
        for (scope, designate_first) in [
            ("with no administrator designated", false),
            ("with another account already designated", true),
        ] {
            let harness = Harness::new();
            if designate_first {
                let (status, body) = harness
                    .enable_organization_admin_account(REGION, designated, None)
                    .await;
                assert_eq!(
                    status, 200,
                    "case {case:?} / {scope}: the first designation expected 200, got {status} \
                     with {body}"
                );
            }

            let (status, body) = harness
                .enable_organization_admin_account(REGION, bad, None)
                .await;
            assert_eq!(
                status, 400,
                "case {case:?} ({bad:?}) / {scope}: expected 400, got {status} with {body}"
            );
            assert_eq!(
                body["Code"].as_str(),
                Some("InvalidInputException"),
                "case {case:?} ({bad:?}) / {scope}: expected InvalidInputException, got {body}"
            );
            // The refusal names the member in its wire spelling and quotes back the
            // value the request carried, whatever that value was.
            let expected_message = format!("Invalid accountId: {bad}");
            assert_eq!(
                body["Message"].as_str(),
                Some(expected_message.as_str()),
                "case {case:?} ({bad:?}) / {scope}: expected {expected_message:?}, got {body}"
            );

            // A refused designation writes nothing: the organization still holds
            // exactly what it held before the call.
            let (status, body) = harness
                .list_organization_admin_accounts(REGION, None, None, None)
                .await;
            assert_eq!(
                status, 200,
                "case {case:?} / {scope}: ListOrganizationAdminAccounts expected 200, got \
                 {status} with {body}"
            );
            let listed = admin_accounts_of(&body);
            let expected: Vec<&str> = if designate_first {
                vec![designated]
            } else {
                Vec::new()
            };
            let actual: Vec<&str> = listed
                .iter()
                .filter_map(|entry| entry["AccountId"].as_str())
                .collect();
            assert_eq!(
                actual, expected,
                "case {case:?} ({bad:?}) / {scope}: expected {expected:?}, got {body}"
            );
        }
    }
}

/// RULE: a feature holds one delegated administrator per Region — designating the
/// account it already holds is the same designation and changes nothing, while
/// designating a different account is refused and leaves the one in place.
#[tokio::test]
async fn one_delegated_administrator_per_feature_and_region() {
    for feature in [None, Some(SECURITY_HUB_FEATURES[1])] {
        let harness = Harness::new();
        let first = "151515151515";
        let second = "161616161616";

        for attempt in 1..=2 {
            let (status, body) = harness
                .enable_organization_admin_account(REGION, first, feature)
                .await;
            assert_eq!(
                status, 200,
                "case Feature={feature:?} attempt {attempt}: designating {first} expected 200, \
                 got {status} with {body}"
            );
        }

        let (status, body) = harness
            .enable_organization_admin_account(REGION, second, feature)
            .await;
        assert_eq!(
            status, 400,
            "case Feature={feature:?}: a second administrator expected 400, got {status} with \
             {body}"
        );
        assert!(
            body["Message"]
                .as_str()
                .is_some_and(|message| message.contains(first)),
            "case Feature={feature:?}: the refusal must name the account that holds the \
             designation, got {body}"
        );

        let (status, body) = harness
            .list_organization_admin_accounts(REGION, feature, None, None)
            .await;
        assert_eq!(
            status, 200,
            "case Feature={feature:?}: ListOrganizationAdminAccounts expected 200, got {status} \
             with {body}"
        );
        let listed = admin_accounts_of(&body);
        assert_eq!(
            listed.len(),
            1,
            "case Feature={feature:?}: the feature holds one administrator, got {body}"
        );
        assert_eq!(
            listed[0]["AccountId"].as_str(),
            Some(first),
            "case Feature={feature:?}: the first designation must stand, got {body}"
        );
    }
}

/// RULE: a `Feature` outside the modelled `SecurityHubFeature` names no feature of
/// this service, and a `MaxResults` outside its published range or a `NextToken` this
/// operation never issued are refused too — each a 400 rather than silently read as
/// something else. The published bounds themselves are accepted.
#[tokio::test]
async fn organization_admin_parameters_are_refused_outside_their_published_values() {
    let harness = Harness::new();
    let admin = "171717171717";
    let (status, body) = harness
        .enable_organization_admin_account(REGION, admin, None)
        .await;
    assert_eq!(
        status, 200,
        "EnableOrganizationAdminAccount expected 200, got {status} with {body}"
    );

    // `SecurityHubFeature` declares exactly two values; anything else is refused by
    // both operations, and neither published value is.
    for (case, feature, expected) in [
        ("the default feature", SECURITY_HUB_FEATURES[0], 200),
        ("the V2 feature", SECURITY_HUB_FEATURES[1], 200),
        ("a feature of another service", "GuardDuty", 400),
        ("a lowercased published value", "securityhub", 400),
        ("an empty feature", "", 400),
    ] {
        let (status, body) = harness
            .list_organization_admin_accounts(REGION, Some(feature), None, None)
            .await;
        assert_eq!(
            status, expected,
            "case {case:?} (Feature={feature:?}): ListOrganizationAdminAccounts expected \
             {expected}, got {status} with {body}"
        );
        let (status, body) = harness
            .enable_organization_admin_account(REGION, admin, Some(feature))
            .await;
        assert_eq!(
            status, expected,
            "case {case:?} (Feature={feature:?}): EnableOrganizationAdminAccount expected \
             {expected}, got {status} with {body}"
        );
    }

    // `MaxResults` is `@range(min: 1, max: 10)`: each bound is accepted, and the value
    // one past each is refused.
    for (case, max_results, expected) in [
        ("the minimum page size", ADMINS_MAX_RESULTS_MIN, 200),
        ("the maximum page size", ADMINS_MAX_RESULTS_MAX, 200),
        (
            "one below the minimum page size",
            ADMINS_MAX_RESULTS_MIN - 1,
            400,
        ),
        (
            "one past the maximum page size",
            ADMINS_MAX_RESULTS_MAX + 1,
            400,
        ),
    ] {
        let (status, body) = harness
            .list_organization_admin_accounts(REGION, None, Some(max_results), None)
            .await;
        assert_eq!(
            status, expected,
            "case {case:?} (MaxResults={max_results}): expected {expected}, got {status} with \
             {body}"
        );
    }

    // A `NextToken` is only ever one this operation issued: a value the caller
    // invented, and one minted by another list of this service, are both refused.
    for (case, token) in [
        ("not a token at all", "not-a-token".to_string()),
        ("an empty-looking cursor", "00".to_string()),
        (
            "a token minted by another list",
            ::hex::encode("securityhub:members:0"),
        ),
    ] {
        let (status, body) = harness
            .list_organization_admin_accounts(REGION, None, None, Some(&token))
            .await;
        assert_eq!(
            status, 400,
            "case {case:?} ({token}): expected 400, got {status} with {body}"
        );
    }
}

/// RULE: administration is delegated to an account OTHER than the organization's
/// management account — which is the account making the call — so a designation that
/// names the calling account is refused with `InvalidAccessException` (401), stating
/// the permission and nothing else, and designates nothing; every other account can be
/// designated in the same Region straight afterwards.
///
/// The table spans what the rule generalises over: the feature (absent, so the
/// documented default, and each value `SecurityHubFeature` declares) and the Region the
/// call is signed for.
#[tokio::test]
async fn the_calling_account_cannot_be_its_own_delegated_administrator() {
    /// The whole sentence Security Hub refuses the designation with: fixed text, with
    /// no account id, Region or reason interpolated.
    const DENIED: &str = "You don't have permissions to perform the requested operation";

    let features = [
        None,
        Some(SECURITY_HUB_FEATURES[0]),
        Some(SECURITY_HUB_FEATURES[1]),
    ];

    for feature in features {
        for region in [REGION, OTHER_REGION] {
            let case = format!("Feature={feature:?} in {region}");
            let harness = Harness::new();

            let (status, body) = harness
                .enable_organization_admin_account(region, ADMINISTRATOR_ID, feature)
                .await;
            assert_eq!(
                status, 401,
                "case {case:?}: designating the calling account expected 401, got {status} with \
                 {body}"
            );
            assert_eq!(
                body["Code"].as_str(),
                Some("InvalidAccessException"),
                "case {case:?}: expected InvalidAccessException, got {body}"
            );
            assert_eq!(
                body["Message"].as_str(),
                Some(DENIED),
                "case {case:?}: expected {DENIED:?}, got {body}"
            );

            // The refusal designated nothing, so the feature still holds no
            // administrator at all.
            let (status, body) = harness
                .list_organization_admin_accounts(region, feature, None, None)
                .await;
            assert_eq!(
                status, 200,
                "case {case:?}: ListOrganizationAdminAccounts expected 200, got {status} with \
                 {body}"
            );
            assert!(
                admin_accounts_of(&body).is_empty(),
                "case {case:?}: a refused designation must persist nothing, got {body}"
            );

            // Only the calling account is refused: another account is designated in
            // the same Region and feature right after.
            let other = "202020202020";
            let (status, body) = harness
                .enable_organization_admin_account(region, other, feature)
                .await;
            assert_eq!(
                status, 200,
                "case {case:?}: designating {other} expected 200, got {status} with {body}"
            );
            let (status, body) = harness
                .list_organization_admin_accounts(region, feature, None, None)
                .await;
            assert_eq!(
                status, 200,
                "case {case:?}: ListOrganizationAdminAccounts expected 200, got {status} with \
                 {body}"
            );
            let listed: Vec<&str> = admin_accounts_of(&body)
                .iter()
                .filter_map(|entry| entry["AccountId"].as_str())
                .collect();
            assert_eq!(
                listed,
                vec![other],
                "case {case:?}: expected {other} to hold the designation, got {body}"
            );
        }
    }
}

/// RULE: this emulator holds delegated administrator designations for a bounded number
/// of Regions — the Region a designation is filed under comes from the caller's own
/// credential scope — so a designation that would be the first in one Region past that
/// bound is the modelled `LimitExceededException` (429), while a Region the store
/// already holds a designation in is never refused by the bound, whichever feature the
/// further designation is for.
#[tokio::test]
async fn the_region_bound_refuses_one_region_too_many() {
    // The bound this emulator holds (`crate::state::MAX_ORGANIZATION_ADMIN_REGIONS`);
    // it is the emulator's own, not a published Security Hub quota.
    const MAX_REGIONS: usize = 64;

    let harness = Harness::new();
    let admin = "212121212121";
    let region_of = |n: usize| format!("us-emulated-{n}");

    for n in 0..MAX_REGIONS {
        let region = region_of(n);
        let (status, body) = harness
            .enable_organization_admin_account(&region, admin, None)
            .await;
        assert_eq!(
            status, 200,
            "case Region {n} of {MAX_REGIONS}: expected 200, got {status} with {body}"
        );
    }

    // Inside the bound: the same Region again, and the other feature in a Region the
    // store already holds a designation in.
    for (case, region, feature) in [
        ("the same Region and feature again", region_of(0), None),
        (
            "the other feature in a Region already held",
            region_of(0),
            Some(SECURITY_HUB_FEATURES[1]),
        ),
    ] {
        let (status, body) = harness
            .enable_organization_admin_account(&region, admin, feature)
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200, got {status} with {body}"
        );
    }

    // One Region past the bound is refused, and designates nothing there.
    let one_too_many = region_of(MAX_REGIONS);
    let (status, body) = harness
        .enable_organization_admin_account(&one_too_many, admin, None)
        .await;
    assert_eq!(
        status, 429,
        "case one Region past the bound: expected 429, got {status} with {body}"
    );
    let (status, body) = harness
        .list_organization_admin_accounts(&one_too_many, None, None, None)
        .await;
    assert_eq!(
        status, 200,
        "case one Region past the bound: ListOrganizationAdminAccounts expected 200, got \
         {status} with {body}"
    );
    assert!(
        admin_accounts_of(&body).is_empty(),
        "case one Region past the bound: a refused designation must persist nothing, got {body}"
    );
}
