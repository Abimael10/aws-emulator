// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of `ListScheduledAudits`: the paginated reader over the
//! account's DEVICE DEFENDER SCHEDULED AUDITS, driven against the `CreateScheduledAudit`
//! writer and the `DescribeScheduledAudit` reader of the same names, so a listing is
//! compared with what the single-audit reader reports for each entry.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a restJson1
//! `GET /audit/scheduledaudits` request signed for the `iot` signing name — so what is
//! asserted is what an AWS SDK client would see, not an internal function's return value.
//!
//! The rules under test, none of them specific to one schedule:
//!
//! * the listing reports ONE PAGE of the scheduled audits the account holds, each
//!   projected from the record a create persisted as the members
//!   `ScheduledAuditMetadata` declares — `scheduledAuditName`, `scheduledAuditArn`,
//!   `frequency`, `dayOfMonth`, `dayOfWeek`
//!   (`smithy_query(query="shape",shape="ScheduledAuditMetadata")`) — and nothing beside
//!   them, so the checks a schedule targets stay with the single-audit reader; an account
//!   holding none is answered an empty list rather than an absent one;
//! * the order is the scheduled audit's NAME, ascending, which is what makes the
//!   `@paginated` page boundary meaningful;
//! * `maxResults` bounds one page — "The maximum number of results to return at one time.
//!   The default is 25" — and `nextToken` resumes the SAME listing exactly where the page
//!   stopped, so walking the token chain visits every scheduled audit once and the last
//!   page carries no token; a token this listing did not issue, including one another
//!   listing of this service issued, is refused `InvalidRequestException` (400);
//! * the listing projects the store as it IS and changes nothing a later caller sees: two
//!   identical reads answer identically and every entry still matches what
//!   `DescribeScheduledAudit` reports for that name.
//!
//! The tables span what those rules generalise over rather than one recorded request:
//! every modelled `AuditFrequency` (read through the generated constant, so a value the
//! model gains is a case this suite demands), each published form of `DayOfMonth`
//! including both bounds of its pattern `^([1-9]|[12][0-9]|3[01])$|^LAST$` and `LAST`,
//! days of the week, names whose ascending order is not their creation order and where one
//! name is another's prefix, every page size from one to past the number of schedules held
//! plus the modelled maximum and the absent member, and a store holding from none to more
//! schedules than one default page.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Map, Value};
use service_com_amazonaws_iot_awsiotservice::ops::types::AUDIT_FREQUENCY_VALUES;
use service_com_amazonaws_iot_awsiotservice::state::{AUDIT_CHECK_NAMES, MAX_LIST_PAGE_SIZE};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The account id every emulated ARN carries (`emulator_server::EMULATOR_ACCOUNT_ID`).
const ACCOUNT_ID: &str = "000000000000";

/// The region this suite signs for; the ARN assertions read it back.
const REGION: &str = "eu-west-3";

/// The role the account's audit configuration names (`RoleArn`, len 20..2048).
const AUDIT_ROLE_ARN: &str = "arn:aws:iam::000000000000:role/device-defender-audit";

/// The page size a request that names no `maxResults` gets:
/// `ListScheduledAudits$maxResults` documents "The maximum number of results to return at
/// one time. The default is 25."
const DEFAULT_PAGE_SIZE: usize = 25;

/// One scheduled audit the tables below create.
struct Row {
    /// The `scheduledAuditName` the request line carries, also the listing's sort key.
    name: &'static str,
    /// The `frequency` the schedule runs at.
    frequency: &'static str,
    /// The `dayOfMonth` it names, for a frequency that demands one.
    day_of_month: Option<&'static str>,
    /// The `dayOfWeek` it names, for a frequency that demands one.
    day_of_week: Option<&'static str>,
}

impl Row {
    /// The `CreateScheduledAudit` body this row asks for. Every row targets one check of
    /// the account's catalog, which the listing does not report at all.
    fn body(&self) -> Value {
        let mut request = Map::new();
        request.insert("frequency".to_string(), json!(self.frequency));
        if let Some(day_of_month) = self.day_of_month {
            request.insert("dayOfMonth".to_string(), json!(day_of_month));
        }
        if let Some(day_of_week) = self.day_of_week {
            request.insert("dayOfWeek".to_string(), json!(day_of_week));
        }
        request.insert(
            "targetCheckNames".to_string(),
            json!([AUDIT_CHECK_NAMES[0]]),
        );
        Value::Object(request)
    }

    /// The entry a listing must report for this row: the five members
    /// `ScheduledAuditMetadata` declares, the two day members only where the schedule
    /// names one.
    fn expected_entry(&self) -> Value {
        let mut expected = Map::new();
        expected.insert("scheduledAuditName".to_string(), json!(self.name));
        expected.insert(
            "scheduledAuditArn".to_string(),
            json!(expected_arn(self.name)),
        );
        expected.insert("frequency".to_string(), json!(self.frequency));
        if let Some(day_of_month) = self.day_of_month {
            expected.insert("dayOfMonth".to_string(), json!(day_of_month));
        }
        if let Some(day_of_week) = self.day_of_week {
            expected.insert("dayOfWeek".to_string(), json!(day_of_week));
        }
        Value::Object(expected)
    }
}

/// The scheduled audits each table below creates, in the order they are created — which is
/// NOT their ascending name order, so an order that holds by accident is caught.
///
/// The rows differ along what the rules generalise over: every modelled `AuditFrequency`,
/// both bounds of `DayOfMonth`'s pattern and its `LAST` form, days of the week, and names
/// where one is another's prefix, of mixed case, with a leading digit and with both
/// character classes `ScheduledAuditName`'s `^[a-zA-Z0-9_-]+$` admits.
const ROWS: [Row; 6] = [
    Row {
        name: "beta-audit",
        frequency: "DAILY",
        day_of_month: None,
        day_of_week: None,
    },
    Row {
        name: "Alpha_audit",
        frequency: "WEEKLY",
        day_of_month: None,
        day_of_week: Some("MON"),
    },
    Row {
        name: "0-nightly",
        frequency: "BIWEEKLY",
        day_of_month: None,
        day_of_week: Some("SAT"),
    },
    Row {
        name: "alpha",
        frequency: "MONTHLY",
        day_of_month: Some("1"),
        day_of_week: None,
    },
    Row {
        name: "alpha-2",
        frequency: "MONTHLY",
        day_of_month: Some("31"),
        day_of_week: None,
    },
    Row {
        name: "zeta99",
        frequency: "MONTHLY",
        day_of_month: Some("LAST"),
        day_of_week: None,
    },
];

/// The ARN this plane mints for `scheduled_audit_name`.
fn expected_arn(scheduled_audit_name: &str) -> String {
    format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:scheduledaudit/{scheduled_audit_name}")
}

/// The names [`ROWS`] must be listed in: the scheduled audit's name, ascending.
fn expected_order() -> Vec<String> {
    let mut names: Vec<String> = ROWS.iter().map(|row| row.name.to_string()).collect();
    names.sort();
    names
}

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

    /// An emulator whose account has enabled every audit check in the catalog, which is
    /// the state a scheduled audit may target checks in.
    async fn with_every_check_enabled() -> Self {
        let emulator = Self::new();
        let mut checks = Map::new();
        for check_name in AUDIT_CHECK_NAMES {
            checks.insert((*check_name).to_string(), json!({ "enabled": true }));
        }
        let request = json!({
            "roleArn": AUDIT_ROLE_ARN,
            "auditCheckConfigurations": Value::Object(checks),
        });
        let reply = emulator
            .dispatch("PATCH", "/audit/configuration", Some(&request))
            .await;
        assert_eq!(
            reply.status, 200,
            "case setup: expected UpdateAccountAuditConfiguration to answer 200, got {} body {}",
            reply.status,
            reply.text()
        );
        emulator
    }

    async fn dispatch(&self, method: &str, path: &str, body: Option<&Value>) -> Reply {
        let host = format!("iot.{REGION}.amazonaws.com");
        let payload = body.map_or_else(Bytes::new, |value| Bytes::from(value.to_string()));
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://{host}{path}"))
            .header("host", host)
            .header("content-type", "application/json")
            // The credential scope names the `iot` signing name the model's
            // `aws.auth#sigv4` trait declares, which is what routes the request.
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/iot/aws4_request, \
                     SignedHeaders=host, Signature=0123456789abcdef"
                ),
            )
            .body(payload)
            .expect("the scheduled-audit request is well formed");
        Reply::of(self.router.dispatch(request).await)
    }

    /// Create `row` and require it to succeed.
    async fn create(&self, row: &Row) {
        let reply = self
            .dispatch(
                "POST",
                &format!("/audit/scheduledaudits/{}", row.name),
                Some(&row.body()),
            )
            .await;
        assert_eq!(
            reply.status, 200,
            "case {}: expected CreateScheduledAudit to answer 200, got {} body {}",
            row.name,
            reply.status,
            reply.text()
        );
    }

    /// Create a schedule under `name` that runs daily, for the tables that only care how
    /// MANY schedules the account holds.
    async fn create_daily(&self, name: &str) {
        let body = json!({
            "frequency": "DAILY",
            "targetCheckNames": [AUDIT_CHECK_NAMES[0]],
        });
        let reply = self
            .dispatch("POST", &format!("/audit/scheduledaudits/{name}"), Some(&body))
            .await;
        assert_eq!(
            reply.status, 200,
            "case {name}: expected CreateScheduledAudit to answer 200, got {} body {}",
            reply.status,
            reply.text()
        );
    }

    /// One page of the listing: `GET /audit/scheduledaudits` with the query members the
    /// caller names.
    async fn list(&self, max_results: Option<i32>, next_token: Option<&str>) -> Reply {
        let mut query = ::form_urlencoded::Serializer::new(String::new());
        if let Some(max_results) = max_results {
            query.append_pair("maxResults", &max_results.to_string());
        }
        if let Some(next_token) = next_token {
            query.append_pair("nextToken", next_token);
        }
        let encoded = query.finish();
        let path = if encoded.is_empty() {
            "/audit/scheduledaudits".to_string()
        } else {
            format!("/audit/scheduledaudits?{encoded}")
        };
        self.dispatch("GET", &path, None).await
    }

    /// One page of the listing, required to succeed: its entries and the token it issued.
    async fn page(
        &self,
        case: &str,
        max_results: Option<i32>,
        next_token: Option<&str>,
    ) -> (Vec<Value>, Option<String>) {
        let reply = self.list(max_results, next_token).await;
        assert_eq!(
            reply.status, 200,
            "case {case}: expected ListScheduledAudits to answer 200, got {} body {}",
            reply.status,
            reply.text()
        );
        let body = reply.json();
        let entries = body["scheduledAudits"].as_array().cloned().unwrap_or_else(|| {
            panic!("case {case}: expected a scheduledAudits list, got {}", reply.text())
        });
        let token = body["nextToken"].as_str().map(str::to_string);
        (entries, token)
    }

    /// Walk the whole listing at `max_results` per page, answering every entry in the
    /// order the pages reported them and the length of each page.
    async fn walk(&self, case: &str, max_results: Option<i32>) -> (Vec<Value>, Vec<usize>) {
        let mut entries = Vec::new();
        let mut lengths = Vec::new();
        let mut token: Option<String> = None;
        // A listing of a bounded store terminates; the bound keeps a token chain that
        // never ends from looping forever.
        for _ in 0..ROWS.len().saturating_add(DEFAULT_PAGE_SIZE).saturating_add(8) {
            let (page, next) = self.page(case, max_results, token.as_deref()).await;
            lengths.push(page.len());
            entries.extend(page);
            match next {
                None => return (entries, lengths),
                Some(next) => token = Some(next),
            }
        }
        panic!("case {case}: the token chain did not end after {lengths:?}");
    }

    /// What `DescribeScheduledAudit` reports for `scheduled_audit_name`.
    async fn describe(&self, case: &str, scheduled_audit_name: &str) -> Value {
        let reply = self
            .dispatch(
                "GET",
                &format!("/audit/scheduledaudits/{scheduled_audit_name}"),
                None,
            )
            .await;
        assert_eq!(
            reply.status, 200,
            "case {case}: expected DescribeScheduledAudit to answer 200, got {} body {}",
            reply.status,
            reply.text()
        );
        reply.json()
    }

    /// A `nextToken` ANOTHER listing of this service issued: the thing registry's, over a
    /// store holding more things than one page.
    async fn foreign_next_token(&self) -> String {
        for thing_name in ["thing-one", "thing-two"] {
            let reply = self
                .dispatch("POST", &format!("/things/{thing_name}"), Some(&json!({})))
                .await;
            assert_eq!(
                reply.status, 200,
                "case setup: expected CreateThing to answer 200 for {thing_name}, got {} body {}",
                reply.status,
                reply.text()
            );
        }
        let reply = self.dispatch("GET", "/things?maxResults=1", None).await;
        assert_eq!(
            reply.status, 200,
            "case setup: expected ListThings to answer 200, got {} body {}",
            reply.status,
            reply.text()
        );
        reply.json()["nextToken"]
            .as_str()
            .unwrap_or_else(|| panic!("case setup: ListThings issued no nextToken: {}", reply.text()))
            .to_string()
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

    /// The error code a restJson1 error response carries.
    fn error_code(&self) -> String {
        self.json()["__type"]
            .as_str()
            .unwrap_or_else(|| panic!("no __type in {}", self.text()))
            .to_string()
    }

    /// The message a restJson1 error response carries.
    fn error_message(&self) -> String {
        self.json()["message"]
            .as_str()
            .unwrap_or_else(|| panic!("no message in {}", self.text()))
            .to_string()
    }
}

/// The table covers every modelled `AuditFrequency`, so a frequency the model gains is a
/// row this suite demands rather than a case nothing exercises.
#[test]
fn the_table_spans_every_modeled_audit_frequency() {
    let covered: std::collections::BTreeSet<&str> = ROWS.iter().map(|row| row.frequency).collect();
    let modeled: std::collections::BTreeSet<&str> =
        AUDIT_FREQUENCY_VALUES.iter().copied().collect();
    assert_eq!(
        covered, modeled,
        "expected one row per modelled AuditFrequency {AUDIT_FREQUENCY_VALUES:?}, got {covered:?}"
    );
}

#[tokio::test]
async fn an_account_holding_no_scheduled_audit_is_listed_empty() {
    let emulator = Emulator::new();

    let (entries, token) = emulator.page("no scheduled audit", None, None).await;
    assert!(
        entries.is_empty(),
        "case no scheduled audit: expected an empty list, got {entries:?}"
    );
    assert_eq!(
        token, None,
        "case no scheduled audit: expected no nextToken, got {token:?}"
    );
}

#[tokio::test]
async fn the_listing_reports_every_scheduled_audit_as_its_metadata_in_name_order() {
    let emulator = Emulator::with_every_check_enabled().await;
    for row in &ROWS {
        emulator.create(row).await;
    }

    let (entries, token) = emulator.page("every row", None, None).await;
    assert_eq!(
        token, None,
        "case every row: {} schedules fit one default page, so expected no nextToken, got {token:?}",
        ROWS.len()
    );

    let expected: Vec<Value> = {
        let mut rows: Vec<&Row> = ROWS.iter().collect();
        rows.sort_by_key(|row| row.name);
        rows.into_iter().map(Row::expected_entry).collect()
    };
    assert_eq!(
        entries, expected,
        "case every row: expected the account's schedules as ScheduledAuditMetadata in name \
         order {:?}, got {entries:?}",
        expected_order()
    );

    // The entries carry the five members `ScheduledAuditMetadata` declares and nothing
    // beside them: the checks a schedule targets stay with the single-audit reader.
    for entry in &entries {
        let members: Vec<&String> = entry
            .as_object()
            .unwrap_or_else(|| panic!("case {entry:?}: expected an object"))
            .keys()
            .collect();
        assert!(
            !members.iter().any(|member| *member == "targetCheckNames"),
            "case {entry:?}: a listing entry reports only the metadata members, got {members:?}"
        );
    }
}

#[tokio::test]
async fn every_listed_entry_is_what_the_single_audit_reader_reports() {
    let emulator = Emulator::with_every_check_enabled().await;
    for row in &ROWS {
        emulator.create(row).await;
    }

    let (entries, _) = emulator.page("projection", None, None).await;
    for entry in entries {
        let name = entry["scheduledAuditName"]
            .as_str()
            .unwrap_or_else(|| panic!("case {entry:?}: expected a scheduledAuditName"))
            .to_string();
        let description = emulator.describe(&name, &name).await;
        for member in [
            "scheduledAuditName",
            "scheduledAuditArn",
            "frequency",
            "dayOfMonth",
            "dayOfWeek",
        ] {
            assert_eq!(
                entry.get(member),
                description.get(member),
                "case {name}: the listing and DescribeScheduledAudit must agree on {member}, \
                 got {:?} and {:?}",
                entry.get(member),
                description.get(member)
            );
        }
    }
}

#[tokio::test]
async fn every_page_size_walks_every_scheduled_audit_exactly_once() {
    let emulator = Emulator::with_every_check_enabled().await;
    for row in &ROWS {
        emulator.create(row).await;
    }

    // Every page size from one to past the number of schedules held, plus both published
    // bounds of the modelled `MaxResults` (range 1..250) and the absent member.
    let page_sizes: Vec<Option<i32>> = vec![
        Some(1),
        Some(2),
        Some(ROWS.len() as i32 - 1),
        Some(ROWS.len() as i32),
        Some(ROWS.len() as i32 + 1),
        Some(MAX_LIST_PAGE_SIZE),
        None,
    ];
    for max_results in page_sizes {
        let case = format!("maxResults={max_results:?}");
        let (entries, lengths) = emulator.walk(&case, max_results).await;
        let names: Vec<String> = entries
            .iter()
            .map(|entry| entry["scheduledAuditName"].as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(
            names,
            expected_order(),
            "case {case}: paging must walk the listing's order exactly once, got pages of \
             {lengths:?}"
        );

        let bound = max_results.unwrap_or(DEFAULT_PAGE_SIZE as i32) as usize;
        for (page_number, length) in lengths.iter().enumerate() {
            assert!(
                *length <= bound,
                "case {case}: page {page_number} holds {length} entries, more than the page \
                 bound {bound}"
            );
        }
        // Only the LAST page of a listing may be short: a token is issued while schedules
        // remain, not instead of filling a page.
        for (page_number, length) in lengths
            .iter()
            .enumerate()
            .take(lengths.len().saturating_sub(1))
        {
            assert_eq!(
                *length, bound,
                "case {case}: page {page_number} is short at {length} entries while the \
                 listing continues"
            );
        }
    }
}

#[tokio::test]
async fn a_request_naming_no_page_size_is_bounded_at_the_documented_default() {
    let emulator = Emulator::with_every_check_enabled().await;
    // One schedule more than a default page holds, so the default bound is observable at
    // all; the names are zero-padded, so the ascending order is the creation order.
    let held = DEFAULT_PAGE_SIZE + 1;
    let names: Vec<String> = (0..held).map(|index| format!("audit-{index:03}")).collect();
    for name in &names {
        emulator.create_daily(name).await;
    }

    let (first, token) = emulator.page("default page size", None, None).await;
    assert_eq!(
        first.len(),
        DEFAULT_PAGE_SIZE,
        "case default page size: expected {DEFAULT_PAGE_SIZE} entries on the first page, got {}",
        first.len()
    );
    let token = token.unwrap_or_else(|| {
        panic!("case default page size: expected a nextToken while {held} schedules are held")
    });

    let (second, last_token) = emulator
        .page("default page size, page two", None, Some(&token))
        .await;
    assert_eq!(
        second.len(),
        held - DEFAULT_PAGE_SIZE,
        "case default page size, page two: expected the remaining {} entries, got {}",
        held - DEFAULT_PAGE_SIZE,
        second.len()
    );
    assert_eq!(
        last_token, None,
        "case default page size, page two: the last page carries no token, got {last_token:?}"
    );

    let walked: Vec<String> = first
        .iter()
        .chain(second.iter())
        .map(|entry| entry["scheduledAuditName"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(
        walked, names,
        "case default page size: the two pages must walk every schedule once in name order"
    );
}

#[tokio::test]
async fn a_token_this_listing_did_not_issue_is_refused() {
    let emulator = Emulator::with_every_check_enabled().await;
    for row in &ROWS {
        emulator.create(row).await;
    }

    // A token of THIS listing, to alter: the one its first page issues.
    let (_, issued) = emulator.page("issued", Some(1), None).await;
    let issued =
        issued.unwrap_or_else(|| panic!("case issued: expected a nextToken on a page of one"));
    let mut flipped: Vec<char> = issued.chars().collect();
    flipped[0] = if flipped[0] == 'a' { 'b' } else { 'a' };
    let flipped: String = flipped.into_iter().collect();

    let cases: Vec<(&str, String)> = vec![
        ("not a token at all", "not-a-token".to_string()),
        ("an empty token", String::new()),
        // Hex of a payload with no listing scope in it, so the token decodes and still
        // names no listing.
        ("a token with no scope", ::hex::encode("alpha")),
        ("an altered token of this listing", flipped),
        // A token another listing of this service issued: the thing registry's.
        ("another listing's token", emulator.foreign_next_token().await),
    ];

    for (case, token) in cases {
        let reply = emulator.list(None, Some(&token)).await;
        assert_eq!(
            reply.status, 400,
            "case {case}: expected ListScheduledAudits to refuse the token with 400, got {} \
             body {}",
            reply.status,
            reply.text()
        );
        assert_eq!(
            reply.error_code(),
            "InvalidRequestException",
            "case {case}: expected the modelled InvalidRequestException, got {}",
            reply.text()
        );
        assert!(
            !reply.error_message().is_empty(),
            "case {case}: the refusal states why the token was not accepted, got {}",
            reply.text()
        );
    }

    // The refusals left the listing itself untouched.
    let (entries, _) = emulator.page("after the refusals", None, None).await;
    assert_eq!(
        entries.len(),
        ROWS.len(),
        "case after the refusals: expected the {} schedules still listed, got {entries:?}",
        ROWS.len()
    );
}

#[tokio::test]
async fn reading_the_listing_changes_nothing_a_later_caller_sees() {
    let emulator = Emulator::with_every_check_enabled().await;
    for row in &ROWS {
        emulator.create(row).await;
    }

    let (first, first_token) = emulator.page("first read", Some(2), None).await;
    let (second, second_token) = emulator.page("second read", Some(2), None).await;
    assert_eq!(
        (first, first_token),
        (second, second_token),
        "case repeated read: two identical listings must answer identically"
    );

    // And every schedule is still readable, member for member, after the reads.
    for row in &ROWS {
        let description = emulator.describe(row.name, row.name).await;
        assert_eq!(
            description["scheduledAuditArn"].as_str(),
            Some(expected_arn(row.name).as_str()),
            "case {}: expected the ARN the create minted after the listings, got {description:?}",
            row.name
        );
    }
}
