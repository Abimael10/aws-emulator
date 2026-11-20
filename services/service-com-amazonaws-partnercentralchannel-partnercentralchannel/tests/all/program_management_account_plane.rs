// SPDX-License-Identifier: CC0-1.0

//! The program-management-account plane — `ListProgramManagementAccounts` and
//! `DeleteProgramManagementAccount` — and the ORDER in which every operation of
//! this service answers.
//!
//! Two rules are under test.
//!
//! 1. **An operation this service gates on an active AWS Partner benefit refuses a
//!    caller that holds none with the modelled `AccessDeniedException` (HTTP 403)
//!    carrying the fixed partner-benefit sentence — after the modelled shape
//!    constraints and before any resource lookup.** So a malformed request is a
//!    `ValidationException` (HTTP 400) whether the caller is entitled or not, an
//!    un-entitled caller never learns whether a resource exists, and an immediate
//!    repeat of the same call is refused identically. `ListChannelHandshakes` is
//!    the one operation outside the gate: it is how a prospective partner sees the
//!    invitations addressed to it.
//! 2. **The plane itself reports persisted state.** `ListProgramManagementAccounts`
//!    always serializes `items` — the empty list when the catalog and the filters
//!    select nothing — pages it by `maxResults` behind an opaque `nextToken`, and
//!    `DeleteProgramManagementAccount` removes exactly the account its
//!    `identifier` addresses, which the list then no longer reports.
//!
//! Every case drives the service the way a client does: an awsJson1_0 `POST /`
//! with `X-Amz-Target: PartnerCentralChannel.<Operation>`, dispatched through the
//! same [`MultiServiceRouter`] the emulator binary mounts. Nothing reaches into
//! the store to read, so a response that is not computed from persisted state
//! fails here.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_partnercentralchannel_partnercentralchannel::{
    ComAmazonawsPartnercentralchannelPartnercentralchannelState, EmulatedService,
};
use smithy_server_runtime::MultiServiceRouter;

/// The region every case signs for.
const REGION: &str = "us-east-1";

/// The sentence the entitlement gate refuses with, written out here rather than
/// read from the crate so the wording itself is pinned. It has no variable part:
/// it names no resource, no principal and no action.
const PARTNER_BENEFIT_REQUIRED: &str = "This action requires an active AWS Partner benefit. \
     Register as an AWS Partner if you haven't already, or contact AWS Partner Central Support.";

/// The status the modelled `AccessDeniedException` is served with
/// (`smithy_query(query="shape",shape="AccessDeniedException")` — `@httpError(403)`).
const DENIED_STATUS: u16 = 403;

/// A well-formed `ProgramManagementAccountId` that no create in this suite mints,
/// from the model's `^pma-[a-z0-9]{13}$`.
const ABSENT_ACCOUNT_ID: &str = "pma-1111111111111";

/// A well-formed `RelationshipId` that no create in this suite mints, from the
/// model's `^rs-[a-z0-9]{13}$`.
const ABSENT_RELATIONSHIP_ID: &str = "rs-1111111111111";

/// One mounted copy of the service, plus the engine it was mounted on so a test
/// can record the AWS Partner benefit the gate admits on.
struct Harness {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

impl Harness {
    /// A router with one freshly-engined copy of this service, for a caller that
    /// holds NO partner benefit — the state every store starts in, because no
    /// modelled operation registers a partner.
    fn unentitled() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // Deterministic regardless of the process-wide default: these cases are
        // about the service's behaviour, not about SigV4.
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        Self { router, engine }
    }

    /// The same, for a caller that holds an active AWS Partner benefit.
    fn entitled() -> Self {
        let harness = Self::unentitled();
        harness.grant_partner_benefit();
        harness
    }

    /// Record the benefit on the store this router serves. Nothing on the wire can
    /// do this — the API models no partner registration — so it stands in for the
    /// out-of-band registration an entitled account has.
    fn grant_partner_benefit(&self) {
        self.engine
            .state::<ComAmazonawsPartnercentralchannelPartnercentralchannelState>()
            .lock()
            .grant_partner_entitlement(engine::emulator_account_id());
    }

    /// Call one operation and return `(http status, parsed body)`.
    async fn call(&self, operation: &str, body: &Value) -> (u16, Value) {
        let request = http::Request::builder()
            .method("POST")
            .uri(format!(
                "https://partnercentral-channel.{REGION}.amazonaws.com/"
            ))
            .header("content-type", "application/x-amz-json-1.0")
            .header("x-amz-target", format!("PartnerCentralChannel.{operation}"))
            // The router keys on the SigV4 credential scope's service field
            // (`partnercentral-channel`, this service's signing name) and the
            // service reads the region segment out of the same scope. The
            // signature itself is not checked: authentication is off above.
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIAEXAMPLEEXAMPLE/20260101/{REGION}/\
                     partnercentral-channel/aws4_request, SignedHeaders=host;x-amz-target, \
                     Signature=0"
                ),
            )
            .body(bytes::Bytes::from(
                serde_json::to_vec(body).expect("the request body serializes"),
            ))
            .expect("a well-formed request");

        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let bytes = response.body();
        let parsed = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(bytes).unwrap_or(Value::Null)
        };
        (status, parsed)
    }

    /// Call one operation, requiring HTTP 200.
    async fn call_ok(&self, operation: &str, body: &Value, case: &str) -> Value {
        let (status, parsed) = self.call(operation, body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected {operation} to succeed with HTTP 200, got {status} \
             and {parsed}"
        );
        parsed
    }
}

/// The awsJson error code an error body carries in `__type`.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .rsplit(['#', '/'])
        .next()
        .unwrap_or_default()
        .to_string()
}

/// The message of a failure body, under either casing: the framework's own
/// pre-dispatch refusals and this service's modelled errors both spell the member
/// `message`, but a protocol that upper-cases it must not read as "no message".
fn error_message(body: &Value) -> String {
    ["message", "Message"]
        .into_iter()
        .find_map(|member| body.get(member).and_then(Value::as_str))
        .unwrap_or("<no message>")
        .to_string()
}

fn member<'a>(body: &'a Value, name: &str) -> &'a Value {
    body.get(name).unwrap_or(&Value::Null)
}

fn string_member(body: &Value, name: &str) -> String {
    member(body, name).as_str().unwrap_or_default().to_string()
}

/// The `items` of a list response, requiring the member to be present: an absent
/// `items` is the defect, not an empty page.
fn items(body: &Value, case: &str) -> Vec<Value> {
    let items = body.get("items");
    assert!(
        items.is_some(),
        "case {case:?}: items must always be serialized, got {body}"
    );
    items
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: items must be a list, got {body}"))
}

/// The `id` of every account on a list page, in the order the page reports them.
fn listed_ids(body: &Value, case: &str) -> Vec<String> {
    items(body, case)
        .iter()
        .map(|item| string_member(item, "id"))
        .collect()
}

// ---------------------------------------------------------------------------
// Rule 1: the entitlement gate
// ---------------------------------------------------------------------------

/// Every operation the gate fronts, with a request whose FORM is valid, so the
/// only thing left to refuse is the caller's entitlement. The identifiers name
/// resources that do not exist, which is the point: the gate answers ahead of
/// the lookup that would otherwise report them missing.
fn gated_calls() -> Vec<(&'static str, Value)> {
    vec![
        (
            "CreateProgramManagementAccount",
            json!({
                "catalog": "AWS",
                "program": "SOLUTION_PROVIDER",
                "displayName": "Acme PMA",
                "accountId": "123456789012",
            }),
        ),
        ("ListProgramManagementAccounts", json!({"catalog": "AWS"})),
        (
            "DeleteProgramManagementAccount",
            json!({"catalog": "AWS", "identifier": ABSENT_ACCOUNT_ID}),
        ),
        (
            "CreateRelationship",
            json!({
                "catalog": "AWS",
                "associationType": "END_CUSTOMER",
                "programManagementAccountIdentifier": ABSENT_ACCOUNT_ID,
                "associatedAccountId": "210987654321",
                "displayName": "Acme Customer",
                "sector": "COMMERCIAL",
            }),
        ),
        (
            "GetRelationship",
            json!({
                "catalog": "AWS",
                "programManagementAccountIdentifier": ABSENT_ACCOUNT_ID,
                "identifier": ABSENT_RELATIONSHIP_ID,
            }),
        ),
        (
            "UpdateRelationship",
            json!({
                "catalog": "AWS",
                "programManagementAccountIdentifier": ABSENT_ACCOUNT_ID,
                "identifier": ABSENT_RELATIONSHIP_ID,
                "displayName": "Acme Renamed",
            }),
        ),
        (
            "DeleteRelationship",
            json!({
                "catalog": "AWS",
                "programManagementAccountIdentifier": ABSENT_ACCOUNT_ID,
                "identifier": ABSENT_RELATIONSHIP_ID,
            }),
        ),
        ("ListRelationships", json!({"catalog": "AWS"})),
        (
            "CreateChannelHandshake",
            json!({
                "catalog": "AWS",
                "handshakeType": "PROGRAM_MANAGEMENT_ACCOUNT",
                "associatedResourceIdentifier": ABSENT_ACCOUNT_ID,
            }),
        ),
    ]
}

/// Rule 1, over every gated operation: the refusal is the modelled
/// `AccessDeniedException` with [`DENIED_STATUS`] and the fixed sentence, it is
/// the WHOLE answer (no resource is reported as missing instead), and an
/// immediate repeat of the identical call is refused identically.
#[tokio::test]
async fn an_unentitled_caller_is_refused_by_the_gate_on_every_operation_it_fronts() {
    let harness = Harness::unentitled();

    for (operation, body) in gated_calls() {
        let first = harness.call(operation, &body).await;
        let repeat = harness.call(operation, &body).await;
        for (case, (status, out)) in [
            (format!("{operation}, first call"), first),
            (format!("{operation}, immediate repeat"), repeat),
        ] {
            assert_eq!(
                status, DENIED_STATUS,
                "case {case:?}: an un-entitled caller must be refused with \
                 {DENIED_STATUS}, got {status} and {out}"
            );
            assert_eq!(
                error_code(&out),
                "AccessDeniedException",
                "case {case:?}: expected the modelled AccessDeniedException, got {out}"
            );
            assert_eq!(
                error_message(&out),
                PARTNER_BENEFIT_REQUIRED,
                "case {case:?}: expected the partner-benefit sentence, got {out}"
            );
        }
    }

    // And nothing was written on the way to any of those refusals: the same store,
    // read once the benefit is recorded, holds no account and no relationship.
    harness.grant_partner_benefit();
    for (operation, case) in [
        ("ListProgramManagementAccounts", "accounts after the refusals"),
        ("ListRelationships", "relationships after the refusals"),
    ] {
        let listed = harness
            .call_ok(operation, &json!({"catalog": "AWS"}), case)
            .await;
        assert!(
            items(&listed, case).is_empty(),
            "case {case:?}: a refused request must store nothing, got {listed}"
        );
    }
}

/// The one operation OUTSIDE the gate: a prospective partner must be able to see
/// the invitations addressed to it before it holds any benefit, so this list is
/// served — with `items` present — to exactly the caller every case above is
/// refused for.
#[tokio::test]
async fn listing_channel_handshakes_is_served_to_an_unentitled_caller() {
    let harness = Harness::unentitled();
    let case = "ListChannelHandshakes, un-entitled";
    let listed = harness
        .call_ok(
            "ListChannelHandshakes",
            &json!({
                "catalog": "AWS",
                "handshakeType": "PROGRAM_MANAGEMENT_ACCOUNT",
                "participantType": "SENDER",
            }),
            case,
        )
        .await;
    assert!(
        items(&listed, case).is_empty(),
        "case {case:?}: a list that matches nothing is an empty page, got {listed}"
    );
}

/// One malformed request: which member is wrong, and the value that makes it so.
struct MalformedCase {
    case: &'static str,
    operation: &'static str,
    body: Value,
    /// The member the refusal must name.
    member: &'static str,
    /// How many of the member's modelled constraints the value violates: the
    /// count the refusal's wrapper sentence states.
    violations: usize,
    /// The `code` of the `fieldList` entry, for a rule THIS SERVICE applies (it
    /// reports the per-member detail in the modelled `fieldList`). `None` for a
    /// rule the model itself declares: the framework's constraint walk refuses
    /// those before any handler runs, and its pre-dispatch envelope carries only
    /// `__type` and `message`, so the member and the rule are stated there.
    field_code: Option<&'static str>,
}

/// The table of requests AWS rejects for their FORM. Each row violates exactly one
/// modelled constraint, named beside it; the values come from the model
/// (`ProgramManagementAccountIdentifier` `@length(17..1011)` +
/// `@pattern(^(arn:…/program-management-account/)?pma-[a-z0-9]{13}$)`, `Catalog`
/// `@length(1..64)` + `@pattern(^[a-zA-Z]*$)`, `maxResults` `@range(1..100)`, the
/// `Program` / `ProgramManagementAccountStatus` /
/// `ListProgramManagementAccountsSortName` / `SortOrder` value sets, and
/// `NextToken`).
fn malformed_cases() -> Vec<MalformedCase> {
    vec![
        MalformedCase {
            case: "identifier is not a pma- id at all",
            operation: "DeleteProgramManagementAccount",
            body: json!({"catalog": "AWS", "identifier": "not-a-valid-pma-identifier"}),
            member: "identifier",
            violations: 1,
            field_code: None,
        },
        MalformedCase {
            case: "identifier is a pma- id of the wrong length",
            operation: "DeleteProgramManagementAccount",
            body: json!({"catalog": "AWS", "identifier": "pma-0123456789abcdef"}),
            member: "identifier",
            violations: 1,
            field_code: None,
        },
        MalformedCase {
            case: "identifier names another resource kind",
            operation: "DeleteProgramManagementAccount",
            body: json!({"catalog": "AWS", "identifier": ABSENT_RELATIONSHIP_ID}),
            member: "identifier",
            // A `rs-` id is 16 characters, so it is both shorter than
            // `@length(min: 17)` and outside the `pma-` pattern.
            violations: 2,
            field_code: None,
        },
        MalformedCase {
            case: "catalog carries a character the pattern excludes",
            operation: "DeleteProgramManagementAccount",
            body: json!({"catalog": "Sand box", "identifier": ABSENT_ACCOUNT_ID}),
            member: "catalog",
            violations: 1,
            field_code: None,
        },
        MalformedCase {
            case: "catalog is shorter than the length bound",
            operation: "ListProgramManagementAccounts",
            body: json!({"catalog": ""}),
            member: "catalog",
            violations: 1,
            field_code: None,
        },
        MalformedCase {
            case: "maxResults is one past the upper bound",
            operation: "ListProgramManagementAccounts",
            body: json!({"catalog": "AWS", "maxResults": 101}),
            member: "maxResults",
            violations: 1,
            field_code: None,
        },
        MalformedCase {
            case: "maxResults is one below the lower bound",
            operation: "ListProgramManagementAccounts",
            body: json!({"catalog": "AWS", "maxResults": 0}),
            member: "maxResults",
            violations: 1,
            field_code: None,
        },
        MalformedCase {
            case: "programs carries a value outside the enum",
            operation: "ListProgramManagementAccounts",
            body: json!({"catalog": "AWS", "programs": ["NOT_A_REAL_PROGRAM"]}),
            member: "programs",
            violations: 1,
            field_code: None,
        },
        MalformedCase {
            case: "statuses carries a value outside the enum",
            operation: "ListProgramManagementAccounts",
            body: json!({"catalog": "AWS", "statuses": ["NOT_A_REAL_STATUS"]}),
            member: "statuses",
            violations: 1,
            field_code: None,
        },
        MalformedCase {
            case: "sortBy is outside its value set",
            operation: "ListProgramManagementAccounts",
            body: json!({
                "catalog": "AWS",
                "sort": {"sortBy": "CreatedAt", "sortOrder": "Ascending"},
            }),
            member: "sort.sortBy",
            violations: 1,
            field_code: None,
        },
        MalformedCase {
            case: "sortOrder is outside its value set",
            operation: "ListProgramManagementAccounts",
            body: json!({
                "catalog": "AWS",
                "sort": {"sortBy": "UpdatedAt", "sortOrder": "Sideways"},
            }),
            member: "sort.sortOrder",
            violations: 1,
            field_code: None,
        },
        MalformedCase {
            case: "nextToken was not handed out by this service",
            operation: "ListProgramManagementAccounts",
            body: json!({"catalog": "AWS", "nextToken": "not-a-token-this-service-minted"}),
            member: "nextToken",
            violations: 1,
            // The opaque page token is this service's own, not a modelled
            // constraint, so the handler refuses it — with the per-member report.
            field_code: Some("INVALID_STRING_FORMAT"),
        },
    ]
}

/// Rule 1's ordering half: a request AWS rejects for its FORM is a
/// `ValidationException` (HTTP 400) even for the caller the gate would otherwise
/// refuse — so the 400/403 boundary between "your request is malformed" and "you
/// may not do this" survives — and the SAME request from an entitled caller is
/// refused identically, because the caller's entitlement is no part of the
/// decision.
#[tokio::test]
async fn a_malformed_request_is_refused_for_its_form_whatever_the_caller_holds() {
    let unentitled = Harness::unentitled();
    let entitled = Harness::entitled();

    for malformed in malformed_cases() {
        for (holding, harness) in [("un-entitled", &unentitled), ("entitled", &entitled)] {
            let case = format!("{}, {holding}", malformed.case);
            let (status, out) = harness.call(malformed.operation, &malformed.body).await;
            assert_eq!(
                status, 400,
                "case {case:?}: a malformed request is refused 400, got {status} and {out}"
            );
            assert_eq!(
                error_code(&out),
                "ValidationException",
                "case {case:?}: expected the modelled ValidationException, got {out}"
            );
            let message = error_message(&out);
            let plural = if malformed.violations == 1 { "" } else { "s" };
            let counted = format!("{} validation error{plural} detected", malformed.violations);
            assert!(
                message.starts_with(&counted),
                "case {case:?}: the refusal must open with {counted:?}, got {message:?}"
            );
            match malformed.field_code {
                // A rule the model declares: the member and the rule are stated in
                // the message itself.
                None => assert!(
                    message.contains(malformed.member),
                    "case {case:?}: the refusal must name the member it refused, got \
                     {message:?}"
                ),
                // A rule this service applies: the message is the count, and the
                // modelled `fieldList` is the per-member report.
                Some(code) => {
                    assert_eq!(
                        message, "1 validation error detected.",
                        "case {case:?}: the message is the count sentence, got {message:?}"
                    );
                    assert_eq!(
                        string_member(&out, "reason"),
                        "REQUEST_VALIDATION_FAILED",
                        "case {case:?}: the modelled reason must say what failed, got {out}"
                    );
                    let fields = out
                        .get("fieldList")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    assert_eq!(
                        fields.len(),
                        1,
                        "case {case:?}: one violation is one fieldList entry, got {out}"
                    );
                    assert_eq!(
                        string_member(&fields[0], "name"),
                        malformed.member,
                        "case {case:?}: fieldList must name the offending member, got {out}"
                    );
                    assert_eq!(
                        string_member(&fields[0], "code"),
                        code,
                        "case {case:?}: fieldList must classify the violation, got {out}"
                    );
                    let clause = string_member(&fields[0], "message");
                    assert!(
                        clause.contains(&format!("at '{}'", malformed.member))
                            && clause.contains("failed to satisfy constraint"),
                        "case {case:?}: the entry must state the rule the value broke, got \
                         {clause:?}"
                    );
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Rule 2: the plane reports persisted state
// ---------------------------------------------------------------------------

/// One account to seed, differing along what the list's filters and sort
/// generalise over: the catalog, the program, the AWS account id and the display
/// name. Every enum value is the model's own
/// (`smithy_query(query="shape",shape="Program")`).
struct Seed {
    catalog: &'static str,
    program: &'static str,
    display_name: &'static str,
    account_id: &'static str,
}

const SEEDS: &[Seed] = &[
    Seed {
        catalog: "AWS",
        program: "SOLUTION_PROVIDER",
        display_name: "Acme PMA",
        account_id: "111111111111",
    },
    Seed {
        catalog: "AWS",
        program: "DISTRIBUTION",
        display_name: "Globex PMA",
        account_id: "222222222222",
    },
    Seed {
        catalog: "AWS",
        program: "DISTRIBUTION_SELLER",
        // At the model's upper bound for `ProgramManagementAccountDisplayName`
        // (`@length(min: 1, max: 30)`).
        display_name: "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123",
        account_id: "333333333333",
    },
    Seed {
        catalog: "Sandbox",
        program: "SOLUTION_PROVIDER",
        display_name: "Sandbox PMA",
        account_id: "444444444444",
    },
];

/// Create every [`SEEDS`] account in order and return their minted ids.
async fn seed_accounts(harness: &Harness) -> Vec<String> {
    let mut ids = Vec::with_capacity(SEEDS.len());
    for seed in SEEDS {
        let created = harness
            .call_ok(
                "CreateProgramManagementAccount",
                &json!({
                    "catalog": seed.catalog,
                    "program": seed.program,
                    "displayName": seed.display_name,
                    "accountId": seed.account_id,
                }),
                seed.display_name,
            )
            .await;
        let id = string_member(member(&created, "programManagementAccountDetail"), "id");
        assert!(
            id.starts_with("pma-"),
            "case {:?}: a create must mint a pma- id, got {created}",
            seed.display_name
        );
        ids.push(id);
    }
    ids
}

/// A list page over `catalog` with `extra` merged into the request.
async fn list(harness: &Harness, catalog: &str, extra: Value, case: &str) -> Value {
    let mut body = json!({"catalog": catalog});
    if let (Some(body), Some(extra)) = (body.as_object_mut(), extra.as_object()) {
        for (key, value) in extra {
            body.insert(key.clone(), value.clone());
        }
    }
    harness
        .call_ok("ListProgramManagementAccounts", &body, case)
        .await
}

/// Rule 2's read half: the list reports exactly the stored accounts its catalog
/// and filters select, every member of the summary comes from the stored record,
/// and an empty selection is an empty `items` rather than an absent one.
#[tokio::test]
async fn the_list_reports_exactly_the_stored_accounts_its_filters_select() {
    let harness = Harness::entitled();

    let empty = list(&harness, "AWS", json!({}), "empty store").await;
    assert!(
        items(&empty, "empty store").is_empty(),
        "case \"empty store\": an empty catalog lists an empty page, got {empty}"
    );
    assert!(
        empty.get("nextToken").is_none(),
        "case \"empty store\": the last page carries no nextToken, got {empty}"
    );

    let ids = seed_accounts(&harness).await;

    // The catalog scopes the list: an account lives in exactly one.
    let aws = list(&harness, "AWS", json!({}), "catalog AWS").await;
    assert_eq!(
        listed_ids(&aws, "catalog AWS"),
        ids[..3].to_vec(),
        "case \"catalog AWS\": the list reports the stored accounts of that catalog, in \
         creation order, got {aws}"
    );
    let sandbox = list(&harness, "Sandbox", json!({}), "catalog Sandbox").await;
    assert_eq!(
        listed_ids(&sandbox, "catalog Sandbox"),
        vec![ids[3].clone()],
        "case \"catalog Sandbox\": the list reports only that catalog's accounts, got {sandbox}"
    );

    // Every member of the summary is the stored one.
    let first = items(&aws, "summary members")[0].clone();
    let seed = &SEEDS[0];
    for (name, expected) in [
        ("id", ids[0].clone()),
        ("catalog", seed.catalog.to_string()),
        ("program", seed.program.to_string()),
        ("displayName", seed.display_name.to_string()),
        ("accountId", seed.account_id.to_string()),
        ("revision", "1".to_string()),
        (
            "arn",
            format!(
                "arn:aws:partnercentral:{REGION}:{}:catalog/{}/program-management-account/{}",
                engine::emulator_account_id(),
                seed.catalog,
                ids[0]
            ),
        ),
    ] {
        assert_eq!(
            string_member(&first, name),
            expected,
            "case \"summary members\": {name} must come from the stored account, got {first}"
        );
    }
    for name in ["createdAt", "updatedAt", "startDate", "status"] {
        assert!(
            !member(&first, name).is_null(),
            "case \"summary members\": {name} must be reported, got {first}"
        );
    }

    // Each equality filter selects on one stored field; several are ANDed, and a
    // filter that selects nothing is an empty page, not a 404.
    let filters: Vec<(&str, Value, Vec<String>)> = vec![
        (
            "accountIds",
            json!({"accountIds": ["222222222222"]}),
            vec![ids[1].clone()],
        ),
        (
            "displayNames",
            json!({"displayNames": ["Acme PMA", "Globex PMA"]}),
            vec![ids[0].clone(), ids[1].clone()],
        ),
        (
            "programs",
            json!({"programs": ["DISTRIBUTION", "DISTRIBUTION_SELLER"]}),
            vec![ids[1].clone(), ids[2].clone()],
        ),
        (
            "programs and accountIds ANDed",
            json!({"programs": ["DISTRIBUTION"], "accountIds": ["111111111111"]}),
            Vec::new(),
        ),
        (
            "a status nothing reports",
            json!({"statuses": ["INACTIVE"]}),
            Vec::new(),
        ),
        (
            "an account id nothing carries",
            json!({"accountIds": ["999999999999"]}),
            Vec::new(),
        ),
    ];
    for (case, extra, expected) in filters {
        let page = list(&harness, "AWS", extra, case).await;
        assert_eq!(
            listed_ids(&page, case),
            expected,
            "case {case:?}: the filter must select exactly the matching accounts, got {page}"
        );
    }
}

/// The clock-owed `PENDING` -> `ACTIVE` transition is reported BY THE LIST, and
/// the `statuses` filter selects on the status the store holds at that moment.
/// Polled for the condition rather than timed, so the case is independent of how
/// fast the host runs.
#[tokio::test]
async fn the_status_filter_selects_on_the_status_the_store_holds() {
    let harness = Harness::entitled();
    let ids = seed_accounts(&harness).await;

    // A generous deadline: the activation span is a fraction of it, so a loaded
    // host still passes, and a status that never advances still fails.
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let case = "accounts settle ACTIVE";
        let page = list(&harness, "AWS", json!({"statuses": ["ACTIVE"]}), case).await;
        if listed_ids(&page, case) == ids[..3].to_vec() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: every created account must reach ACTIVE, last page {page}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let case = "PENDING once settled";
    let page = list(&harness, "AWS", json!({"statuses": ["PENDING"]}), case).await;
    assert!(
        listed_ids(&page, case).is_empty(),
        "case {case:?}: a settled account is no longer PENDING, got {page}"
    );
}

/// Rule 2's paging half: `maxResults` bounds the page, the `nextToken` resumes the
/// same scan exactly where it stopped, the last page carries none, and a token
/// presented with different filters is refused rather than answered with a page of
/// another result set. The sort runs over the model's only sort name, in both
/// orders.
#[tokio::test]
async fn the_list_pages_and_sorts_the_selection_it_reports() {
    let harness = Harness::entitled();
    let ids = seed_accounts(&harness).await;
    let in_catalog = ids[..3].to_vec();

    for page_size in [1_usize, 2, 3] {
        let case = format!("maxResults {page_size}");
        let mut seen: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        // One more iteration than pages are possible, so a token that never
        // advances fails here rather than looping.
        for _ in 0..=in_catalog.len() {
            let mut extra = json!({"maxResults": page_size});
            if let (Some(extra), Some(token)) = (extra.as_object_mut(), token.as_ref()) {
                extra.insert("nextToken".to_string(), json!(token));
            }
            let page = list(&harness, "AWS", extra, &case).await;
            let page_ids = listed_ids(&page, &case);
            assert!(
                page_ids.len() <= page_size,
                "case {case:?}: a page must hold at most maxResults items, got {page}"
            );
            seen.extend(page_ids);
            token = page
                .get("nextToken")
                .and_then(Value::as_str)
                .map(str::to_string);
            if token.is_none() {
                break;
            }
        }
        assert!(
            token.is_none(),
            "case {case:?}: the walk must terminate, still holding {token:?}"
        );
        assert_eq!(
            seen, in_catalog,
            "case {case:?}: the paged walk must report every account exactly once, in order"
        );
    }

    // A token is bound to the filters and the sort it was taken with.
    let case = "token presented with other filters";
    let page = list(&harness, "AWS", json!({"maxResults": 1}), case).await;
    let token = string_member(&page, "nextToken");
    let (status, out) = harness
        .call(
            "ListProgramManagementAccounts",
            &json!({
                "catalog": "AWS",
                "maxResults": 1,
                "nextToken": token,
                "programs": ["SOLUTION_PROVIDER"],
            }),
        )
        .await;
    assert_eq!(
        status, 400,
        "case {case:?}: resuming with other filters is refused, got {status} and {out}"
    );
    assert_eq!(
        error_code(&out),
        "ValidationException",
        "case {case:?}: expected the modelled ValidationException, got {out}"
    );

    // The sort runs over `UpdatedAt`, the model's only
    // `ListProgramManagementAccountsSortName`. Ascending is creation order here,
    // because nothing has been updated since; Descending is its reverse.
    let mut descending = in_catalog.clone();
    descending.reverse();
    for (order, expected) in [
        ("Ascending", in_catalog.clone()),
        ("Descending", descending),
    ] {
        let case = format!("sorted {order}");
        let page = list(
            &harness,
            "AWS",
            json!({"sort": {"sortBy": "UpdatedAt", "sortOrder": order}}),
            &case,
        )
        .await;
        assert_eq!(
            listed_ids(&page, &case),
            expected,
            "case {case:?}: the sort must order the page by updatedAt, got {page}"
        );
    }
}

/// Rule 2's delete half: the delete removes exactly the account its `identifier`
/// addresses — in either modelled form, bare id or full ARN — which the list then
/// no longer reports, while every other account stays. A second delete of the same
/// account reports it missing, and one carrying the first call's `clientToken`
/// replays the success instead, because the operation is `@idempotent`.
#[tokio::test]
async fn a_delete_removes_exactly_the_account_its_identifier_addresses() {
    for by_arn in [false, true] {
        let case = if by_arn {
            "addressed by ARN"
        } else {
            "addressed by bare id"
        };
        let harness = Harness::entitled();
        let ids = seed_accounts(&harness).await;
        let target = ids[1].clone();
        let identifier = if by_arn {
            format!(
                "arn:aws:partnercentral:{REGION}:{}:catalog/AWS/program-management-account/{target}",
                engine::emulator_account_id()
            )
        } else {
            target.clone()
        };

        // A catalog the account does not live in addresses no account.
        let (status, out) = harness
            .call(
                "DeleteProgramManagementAccount",
                &json!({"catalog": "Sandbox", "identifier": identifier.clone()}),
            )
            .await;
        assert_eq!(
            status, 404,
            "case {case:?}: an account outside the named catalog is not found, got {status} \
             and {out}"
        );
        assert_eq!(
            error_code(&out),
            "ResourceNotFoundException",
            "case {case:?}: expected the modelled ResourceNotFoundException, got {out}"
        );

        let delete = json!({
            "catalog": "AWS",
            "identifier": identifier.clone(),
            "clientToken": "token-delete",
        });
        harness
            .call_ok("DeleteProgramManagementAccount", &delete, case)
            .await;

        // The list is the proof: the addressed account is gone and the others are not.
        let page = list(&harness, "AWS", json!({}), case).await;
        assert_eq!(
            listed_ids(&page, case),
            vec![ids[0].clone(), ids[2].clone()],
            "case {case:?}: the delete must remove exactly the addressed account, got {page}"
        );
        let other = list(&harness, "Sandbox", json!({}), case).await;
        assert_eq!(
            listed_ids(&other, case),
            vec![ids[3].clone()],
            "case {case:?}: another catalog's accounts are untouched, got {other}"
        );

        // `@idempotent`: the retry carrying the same token replays the success.
        harness
            .call_ok("DeleteProgramManagementAccount", &delete, case)
            .await;

        // Without a token there is nothing to replay, so the removed account is
        // reported missing.
        let (status, out) = harness
            .call(
                "DeleteProgramManagementAccount",
                &json!({"catalog": "AWS", "identifier": identifier}),
            )
            .await;
        assert_eq!(
            status, 404,
            "case {case:?}: a second delete reports the account missing, got {status} and {out}"
        );
        assert_eq!(
            error_code(&out),
            "ResourceNotFoundException",
            "case {case:?}: expected the modelled ResourceNotFoundException, got {out}"
        );
        assert_eq!(
            string_member(&out, "resourceId"),
            target,
            "case {case:?}: the refusal must name the account it did not find, got {out}"
        );
    }
}

/// A well-formed identifier no account answers to is a `ResourceNotFoundException`
/// for an ENTITLED caller — the lookup the gate stands in front of — and the same
/// call is refused identically when repeated.
#[tokio::test]
async fn a_delete_of_an_absent_account_reports_it_missing_to_an_entitled_caller() {
    let harness = Harness::entitled();
    let body = json!({"catalog": "AWS", "identifier": ABSENT_ACCOUNT_ID});
    for case in ["first call", "immediate repeat"] {
        let (status, out) = harness.call("DeleteProgramManagementAccount", &body).await;
        assert_eq!(
            status, 404,
            "case {case:?}: an absent account is not found, got {status} and {out}"
        );
        assert_eq!(
            error_code(&out),
            "ResourceNotFoundException",
            "case {case:?}: expected the modelled ResourceNotFoundException, got {out}"
        );
        assert_eq!(
            string_member(&out, "resourceType"),
            "ProgramManagementAccount",
            "case {case:?}: the refusal must name the resource type, got {out}"
        );
    }
}

/// A relationship is modelled as a CHILD of the account (its ARN hangs off the
/// parent's), so an account that still has one cannot be removed: that is the
/// conflict with the resource's current state this operation models
/// `ConflictException` for. Once the child is gone the parent deletes.
#[tokio::test]
async fn an_account_with_a_relationship_is_not_deleted_while_the_child_exists() {
    let harness = Harness::entitled();
    let ids = seed_accounts(&harness).await;
    let parent = ids[0].clone();

    let created = harness
        .call_ok(
            "CreateRelationship",
            &json!({
                "catalog": "AWS",
                "associationType": "END_CUSTOMER",
                "programManagementAccountIdentifier": parent.clone(),
                "associatedAccountId": "210987654321",
                "displayName": "Acme Customer",
                "sector": "COMMERCIAL",
            }),
            "seed the child",
        )
        .await;
    let child = string_member(member(&created, "relationshipDetail"), "id");

    let case = "parent with a child";
    let (status, out) = harness
        .call(
            "DeleteProgramManagementAccount",
            &json!({"catalog": "AWS", "identifier": parent.clone()}),
        )
        .await;
    assert_eq!(
        status, 409,
        "case {case:?}: deleting a parent that still has a child is a conflict, got {status} \
         and {out}"
    );
    assert_eq!(
        error_code(&out),
        "ConflictException",
        "case {case:?}: expected the modelled ConflictException, got {out}"
    );
    assert_eq!(
        string_member(&out, "resourceId"),
        parent,
        "case {case:?}: the refusal must name the account it would not remove, got {out}"
    );
    // And it removed nothing.
    let page = list(&harness, "AWS", json!({}), case).await;
    assert_eq!(
        listed_ids(&page, case),
        ids[..3].to_vec(),
        "case {case:?}: a refused delete must remove nothing, got {page}"
    );

    harness
        .call_ok(
            "DeleteRelationship",
            &json!({
                "catalog": "AWS",
                "programManagementAccountIdentifier": parent.clone(),
                "identifier": child,
            }),
            "remove the child",
        )
        .await;
    let case = "parent once the child is gone";
    harness
        .call_ok(
            "DeleteProgramManagementAccount",
            &json!({"catalog": "AWS", "identifier": parent.clone()}),
            case,
        )
        .await;
    let page = list(&harness, "AWS", json!({}), case).await;
    assert_eq!(
        listed_ids(&page, case),
        vec![ids[1].clone(), ids[2].clone()],
        "case {case:?}: the parent must be gone once nothing hangs off it, got {page}"
    );
}
