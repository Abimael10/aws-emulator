// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what a scheduled deletion does to a secret, what a
//! forced delete of a secret the account never held answers, and which secrets
//! `ListSecrets` filters select, driven over the wire through the multi-service
//! router.
//!
//! The rules under test:
//!
//! * scheduling a secret for deletion turns rotation OFF — `RotationEnabled`
//!   reads back false — and `RestoreSecret`, which cancels the deletion and
//!   nothing else, leaves it off;
//! * `DeleteSecret(ForceDeleteWithoutRecovery=true)` on a name the account holds
//!   no secret for succeeds idempotently and answers for the secret the request
//!   NAMED — ARN, Name and DeletionDate — while creating nothing; without the
//!   flag the same call is `ResourceNotFoundException`;
//! * a `ListSecrets` filter selects by a prefix of what its key names — of any
//!   WORD of it, and ignoring case, for the two keys that search text
//!   (`description` and `all`) — its plain values are alternatives, each of its
//!   `!`-prefixed values excludes what it matches, and `all` requires every word
//!   of one value;
//! * separate `ListSecrets` filters are ANDed, so a scoping `name` filter beside
//!   a text-searching one selects exactly the secrets both hold for, and the
//!   whole selection is reported by ONE paginated walk that leaves no
//!   `NextToken` outstanding on its final page — including the walk over a
//!   selection that is empty;
//! * an empty `Description` describes nothing, so it is indistinguishable on
//!   read-back from a create or an update that named none;
//! * the six characters a secret's ARN ends in belong to the SECRET and not to its
//!   name, so a secret created under the name of one the service has already
//!   removed carries six fresh ones — and every handle on the removed secret, its
//!   ARN and the ids of the versions it held, names nothing from then on.
//!
//! Every test is a TABLE over the class its rule generalises: the recovery window
//! and the rotation schedule, the form of the `SecretId` and the number of
//! repeats, the filter key and value and the case it is written in, and which
//! operation set the description.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_secretsmanager_secretsmanager::state::{
    value_version_not_found_message, ARN_SUFFIX_LEN, MAX_RECOVERY_WINDOW_DAYS,
    MIN_RECOVERY_WINDOW_DAYS, NAME_SCHEDULED_FOR_DELETION_MESSAGE, SECRET_NOT_FOUND_MESSAGE,
};
use service_com_amazonaws_secretsmanager_secretsmanager::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request below signs for, and so the Region every secret it
/// creates is homed in.
const REGION: &str = "us-west-2";

/// The ARN of a Lambda rotation function, as `RotationLambdaARN` carries it.
const LAMBDA_ARN: &str = "arn:aws:lambda:us-west-2:123456789012:function:RotateMySecret";

/// A fresh emulator holding only this service, over its own engine.
fn emulator() -> MultiServiceRouter {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.register_service(EmulatedService::new(engine));
    router
}

/// Send one `awsJson1_1` request and return its status and JSON body.
async fn call(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("secretsmanager.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 \
                 Credential=AKIDEXAMPLE/20260101/{REGION}/secretsmanager/aws4_request, \
                 SignedHeaders=host;x-amz-date, Signature=deadbeef"
            ),
        )
        .body(bytes::Bytes::from(
            // A write that stores a value creates a version the caller has to
            // name; see `secret_client_request_token::with_staked_version`.
            serde_json::to_vec(&crate::secret_client_request_token::with_staked_version(
                operation, body,
            ))
            .expect("the request body is serializable"),
        ))
        .expect("a valid request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// Send one request that must succeed, and return its body.
async fn ok(router: &MultiServiceRouter, operation: &str, body: Value, case: &str) -> Value {
    let (status, parsed) = call(router, operation, &body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from {operation}, got {status} with body {parsed}"
    );
    parsed
}

/// Send one request that must fail, and return its wire error code.
async fn err(router: &MultiServiceRouter, operation: &str, body: Value, case: &str) -> String {
    let (status, parsed) = call(router, operation, &body).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected a 400 client error from {operation}, got {status} with body \
         {parsed}"
    );
    let raw = parsed
        .get("__type")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: no __type in error body {parsed}"));
    raw.split('#').next_back().unwrap_or(raw).to_string()
}

/// A required string member of a response body.
fn text(parsed: &Value, member: &str, case: &str) -> String {
    parsed
        .get(member)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: no {member} in {parsed}"))
        .to_string()
}

/// Every secret `ListSecrets` reports for one filter, by name, in the order it
/// reports them.
async fn filtered(router: &MultiServiceRouter, filters: Value, case: &str) -> Vec<String> {
    let listed = ok(router, "ListSecrets", json!({"Filters": filters}), case).await;
    listed
        .get("SecretList")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: no SecretList in {listed}"))
        .iter()
        .filter_map(|entry| entry.get("Name").and_then(Value::as_str))
        .map(str::to_string)
        .collect()
}

#[tokio::test]
async fn a_recoverable_delete_turns_rotation_off_and_restore_leaves_it_off() {
    // The rows run over the recovery window the delete names — absent, and both
    // published bounds of `RecoveryWindowInDays` ("The number of days from 7 to
    // 30 that Secrets Manager waits", this service's Smithy model) — and over the
    // form of the rotation schedule that was turned on, since the rule is about
    // the deletion rather than about which schedule rotation was on.
    let windows: &[(&str, Option<i64>)] = &[
        ("the default window", None),
        ("the shortest window", Some(MIN_RECOVERY_WINDOW_DAYS)),
        ("the longest window", Some(MAX_RECOVERY_WINDOW_DAYS)),
    ];
    let schedules: &[(&str, Value)] = &[
        ("an interval in days", json!({"AutomaticallyAfterDays": 10})),
        ("a rate() expression", json!({"ScheduleExpression": "rate(12 hours)"})),
        (
            "a cron() expression",
            json!({"ScheduleExpression": "cron(0 8 1 * ? *)"}),
        ),
    ];
    for (window_index, (window_case, window)) in windows.iter().enumerate() {
        for (schedule_index, (schedule_case, rules)) in schedules.iter().enumerate() {
            let case = format!("{window_case} over {schedule_case}");
            let router = emulator();
            let name = format!("rotating-{window_index}-{schedule_index}");
            ok(
                &router,
                "CreateSecret",
                json!({"Name": &name, "SecretString": "s3cret"}),
                &case,
            )
            .await;
            ok(
                &router,
                "RotateSecret",
                json!({
                    "SecretId": &name,
                    "RotationLambdaARN": LAMBDA_ARN,
                    "RotationRules": rules,
                    "RotateImmediately": false,
                }),
                &case,
            )
            .await;
            let rotating = ok(&router, "DescribeSecret", json!({"SecretId": &name}), &case).await;
            assert_eq!(
                rotating.get("RotationEnabled").and_then(Value::as_bool),
                Some(true),
                "case {case:?}: expected rotation to be on before the delete, got {rotating}"
            );

            let mut delete = json!({"SecretId": &name});
            if let Some(window) = window {
                delete["RecoveryWindowInDays"] = json!(window);
            }
            ok(&router, "DeleteSecret", delete, &case).await;
            let deleted = ok(&router, "DescribeSecret", json!({"SecretId": &name}), &case).await;
            assert_eq!(
                deleted.get("RotationEnabled").and_then(Value::as_bool),
                Some(false),
                "case {case:?}: a secret scheduled for deletion is not rotated, got {deleted}"
            );
            assert!(
                deleted.get("NextRotationDate").is_none(),
                "case {case:?}: a secret scheduled for deletion has no rotation due, got \
                 {deleted}"
            );
            // The configuration itself is kept, so the caller can turn rotation
            // back on with RotateSecret.
            assert_eq!(
                deleted.get("RotationLambdaARN").and_then(Value::as_str),
                Some(LAMBDA_ARN),
                "case {case:?}: the rotation configuration is kept, got {deleted}"
            );

            ok(&router, "RestoreSecret", json!({"SecretId": &name}), &case).await;
            let restored = ok(&router, "DescribeSecret", json!({"SecretId": &name}), &case).await;
            assert!(
                restored.get("DeletedDate").is_none(),
                "case {case:?}: the restore cancels the deletion, got {restored}"
            );
            assert_eq!(
                restored.get("RotationEnabled").and_then(Value::as_bool),
                Some(false),
                "case {case:?}: the restore does not turn rotation back on, got {restored}"
            );

            // And asking for it explicitly does.
            ok(
                &router,
                "RotateSecret",
                json!({"SecretId": &name, "RotateImmediately": false}),
                &case,
            )
            .await;
            let again = ok(&router, "DescribeSecret", json!({"SecretId": &name}), &case).await;
            assert_eq!(
                again.get("RotationEnabled").and_then(Value::as_bool),
                Some(true),
                "case {case:?}: expected rotation to be back on once asked for, got {again}"
            );
        }
    }
}

#[tokio::test]
async fn forcing_the_delete_of_a_secret_the_account_never_held_answers_for_the_name() {
    // The rows run over the form of the `SecretId` the call names — "The ARN or
    // name of the secret to delete" (`DeleteSecretRequest$SecretId`, this
    // service's Smithy model), plus the partial ARN every `SecretId` accepts —
    // and over the charset and shape of the name inside it.
    let account = engine::EMULATOR_ACCOUNT_ID;
    let rows: &[(&str, String, &str)] = &[
        ("a plain name", "never-created".to_string(), "never-created"),
        (
            "a path-like name",
            "team/prod/db-password".to_string(),
            "team/prod/db-password",
        ),
        (
            "a long mixed-charset name",
            "Prod_Secret.With-Mixed+Chars=1234567890".to_string(),
            "Prod_Secret.With-Mixed+Chars=1234567890",
        ),
        (
            "a full ARN",
            format!("arn:aws:secretsmanager:{REGION}:{account}:secret:absent-by-arn-AbCdEf"),
            "absent-by-arn",
        ),
        (
            "a partial ARN",
            format!("arn:aws:secretsmanager:{REGION}:{account}:secret:absent-by-partial"),
            "absent-by-partial",
        ),
    ];
    let router = emulator();
    for (case, secret_id, expected_name) in rows {
        // Without the flag the same call reports the secret missing.
        let code = err(
            &router,
            "DeleteSecret",
            json!({"SecretId": secret_id}),
            case,
        )
        .await;
        assert_eq!(
            code, "ResourceNotFoundException",
            "case {case:?}: expected a delete with no force flag to report the secret missing, \
             got {code}"
        );

        // "If you forcibly delete an already deleted or nonexistent secret, the
        // operation does not return ResourceNotFoundException" — and it is
        // idempotent, so a repeat answers the same way.
        for attempt in 1..=3 {
            let forced = ok(
                &router,
                "DeleteSecret",
                json!({"SecretId": secret_id, "ForceDeleteWithoutRecovery": true}),
                case,
            )
            .await;
            assert_eq!(
                text(&forced, "Name", case),
                *expected_name,
                "case {case:?}: forced delete #{attempt} answered the wrong Name"
            );
            let arn = text(&forced, "ARN", case);
            let prefix = format!("arn:aws:secretsmanager:{REGION}:{account}:secret:{expected_name}-");
            assert!(
                arn.starts_with(&prefix),
                "case {case:?}: forced delete #{attempt} answered {arn:?}, expected an ARN \
                 starting {prefix:?}"
            );
            assert_eq!(
                arn.len(),
                prefix.len() + 6,
                "case {case:?}: forced delete #{attempt} answered {arn:?}, expected the name \
                 plus a hyphen and six characters"
            );
            assert!(
                forced.get("DeletionDate").and_then(Value::as_f64).is_some(),
                "case {case:?}: forced delete #{attempt} answered no DeletionDate, got {forced}"
            );
        }
    }

    // And none of it created anything.
    let listed = ok(
        &router,
        "ListSecrets",
        json!({"IncludePlannedDeletion": true}),
        "after forcing the delete of secrets that were never created",
    )
    .await;
    assert_eq!(
        listed
            .get("SecretList")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(0),
        "expected the forced deletes to have created nothing, got {listed}"
    );
}

#[tokio::test]
async fn list_secrets_filters_select_by_a_prefix_of_a_word() {
    let router = emulator();
    // Three secrets whose descriptions and tags carry the words the rows search
    // for in several positions and several cases.
    for (name, description, tag) in [
        (
            "svc/payments",
            "Payment gateway credentials",
            ("env", "prod-payments"),
        ),
        ("svc/login", "LOGIN service session key", ("env", "staging")),
        ("svc/cache", "staging cache", ("team", "platform")),
    ] {
        ok(
            &router,
            "CreateSecret",
            json!({
                "Name": name,
                "SecretString": "v",
                "Description": description,
                "Tags": [{"Key": tag.0, "Value": tag.1}],
            }),
            name,
        )
        .await;
    }

    // The rows run over the filter key, the position of the matched word, the
    // case it is written in, how many words one value carries and whether it is
    // negated. The expected sets are in the name order `ListSecrets` pages in.
    let rows: &[(&str, &str, &[&str], &[&str])] = &[
        (
            "description, the first word",
            "description",
            &["Payment"],
            &["svc/payments"],
        ),
        (
            "description, a later word",
            "description",
            &["gateway"],
            &["svc/payments"],
        ),
        (
            "description, a later word in another case",
            "description",
            &["GATEWAY"],
            &["svc/payments"],
        ),
        (
            "description, a prefix of a later word",
            "description",
            &["creden"],
            &["svc/payments"],
        ),
        (
            "description, a word two secrets carry",
            "description",
            &["staging"],
            &["svc/cache"],
        ),
        (
            "description, nothing carries",
            "description",
            &["absent"],
            &[],
        ),
        (
            "all, over the name",
            "all",
            &["login"],
            &["svc/login"],
        ),
        (
            "all, over the description in another case",
            "all",
            &["GATEWAY"],
            &["svc/payments"],
        ),
        (
            "all, over a tag value",
            "all",
            &["platform"],
            &["svc/cache"],
        ),
        (
            "all, two words one secret carries between its attributes",
            "all",
            &["payment payments"],
            &["svc/payments"],
        ),
        (
            "all, two words no secret carries together",
            "all",
            &["payment login"],
            &[],
        ),
        (
            "description, two alternatives",
            "description",
            &["payment", "login"],
            &["svc/login", "svc/payments"],
        ),
        (
            "description, one negated value",
            "description",
            &["!staging"],
            &["svc/login", "svc/payments"],
        ),
        (
            "all, one negated value",
            "all",
            &["!payment"],
            &["svc/cache", "svc/login"],
        ),
        (
            "description, two negated values",
            "description",
            &["!staging", "!payment"],
            &["svc/login"],
        ),
        (
            "description, an alternative narrowed by a negation",
            "description",
            &["session", "!staging"],
            &["svc/login"],
        ),
        (
            "name, a prefix of the whole name",
            "name",
            &["svc/pay"],
            &["svc/payments"],
        ),
        // "name: Prefix match, case-sensitive." (`Filter$Key`.)
        ("name, in another case", "name", &["SVC/PAY"], &[]),
    ];
    for (case, key, values, expected) in rows {
        let selected = filtered(
            &router,
            json!([{"Key": key, "Values": values}]),
            case,
        )
        .await;
        assert_eq!(
            selected,
            expected
                .iter()
                .map(|name| (*name).to_string())
                .collect::<Vec<String>>(),
            "case {case:?}: filter {key}={values:?} selected the wrong set"
        );
    }

    // Separate filters all have to hold.
    let both = filtered(
        &router,
        json!([
            {"Key": "description", "Values": ["staging"]},
            {"Key": "tag-key", "Values": ["team"]},
        ]),
        "two filters",
    )
    .await;
    assert_eq!(
        both,
        vec!["svc/cache".to_string()],
        "expected both filters to have to hold"
    );
}

/// How many pages one walk below may take before it is treated as
/// non-terminating: far more than the handful of secrets any case here creates,
/// so only a walk that never ends trips it.
const PAGE_WALK_CAP: usize = 16;

/// Walk `ListSecrets` for one filter set to its end, and report the names it
/// reported — in the order it reported them — beside the number of pages the
/// walk took.
///
/// A page is asked for with `MaxResults` when `page_size` is given, and as the
/// whole remainder when it is not. The walk follows `NextToken` until a page
/// carries none, which is what makes an outstanding token on the final page, or
/// a token that never clears, a failure rather than a hang.
async fn walked(
    router: &MultiServiceRouter,
    filters: &Value,
    page_size: Option<usize>,
    case: &str,
) -> (Vec<String>, usize) {
    let mut seen: Vec<String> = Vec::new();
    let mut token: Option<String> = None;
    let mut pages = 0;
    loop {
        let mut body = json!({"Filters": filters.clone()});
        if let Some(size) = page_size {
            body["MaxResults"] = json!(size);
        }
        if let Some(token) = &token {
            body["NextToken"] = json!(token);
        }
        let listed = ok(router, "ListSecrets", body, case).await;
        let page: Vec<String> = listed
            .get("SecretList")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("case {case:?}: no SecretList in {listed}"))
            .iter()
            .map(|entry| text(entry, "Name", case))
            .collect();
        if let Some(size) = page_size {
            assert!(
                page.len() <= size,
                "case {case:?}: expected at most {size} entries in a page, got {page:?}"
            );
        }
        seen.extend(page);
        pages += 1;
        assert!(
            pages <= PAGE_WALK_CAP,
            "case {case:?}: the paginated walk did not terminate — it saw {seen:?} over {pages} \
             pages and was still handed a NextToken"
        );
        match listed.get("NextToken").and_then(Value::as_str) {
            Some(next) => token = Some(next.to_string()),
            None => return (seen, pages),
        }
    }
}

#[tokio::test]
async fn a_name_scoped_text_filter_selects_the_secrets_both_filters_hold_for() {
    let router = emulator();
    // The secrets the rows search. Their names, descriptions and tags carry the
    // searched words in several positions, several cases, and inside hyphenated
    // names and tag keys — so a row selecting one of them exercises the WORD as
    // the unit the two text keys match a prefix of. The last secret carries the
    // same description and tag value as the first but sits outside the scope the
    // rows filter by, which is what makes the scoping filter's AND observable.
    let scope = "search-scope";
    for (name, description, tag) in [
        (
            "search-scope-app-1",
            Some("Payment gateway credentials"),
            Some(("search-scope-tier", "web")),
        ),
        ("search-scope-app-2", Some("reporting Database login"), None),
        (
            "search-scope-db-1",
            None,
            Some(("search-scope-tier", "data")),
        ),
        (
            "other-scope-app-9",
            Some("Payment gateway credentials"),
            Some(("other-scope-tier", "web")),
        ),
    ] {
        let mut create = json!({"Name": name, "SecretString": "v"});
        if let Some(description) = description {
            create["Description"] = json!(description);
        }
        if let Some((key, value)) = tag {
            create["Tags"] = json!([{"Key": key, "Value": value}]);
        }
        ok(&router, "CreateSecret", create, name).await;
    }

    // The rows run over the filter key (`description` and `all`, the two keys
    // `Filter$Key` documents as matching text and not case-sensitively), over
    // which attribute carries the match (name, description, tag key, tag value),
    // over the position of the matched word and the case it is written in, over
    // how many words one value carries, and over how many secrets the answer
    // holds — none, one and two. Each expected set is in the name order the
    // listing pages in.
    let rows: &[(&str, &str, &str, &[&str])] = &[
        (
            "description, the first word",
            "description",
            "Payment",
            &["search-scope-app-1"],
        ),
        (
            "description, the first word in another case",
            "description",
            "payment",
            &["search-scope-app-1"],
        ),
        (
            "description, a later word",
            "description",
            "gateway",
            &["search-scope-app-1"],
        ),
        (
            "description, a prefix of a later word",
            "description",
            "cred",
            &["search-scope-app-1"],
        ),
        (
            "description, a later word upper-cased",
            "description",
            "DATABASE",
            &["search-scope-app-2"],
        ),
        (
            "description, the last word",
            "description",
            "login",
            &["search-scope-app-2"],
        ),
        (
            "description, a word nothing in scope carries",
            "description",
            "absent",
            &[],
        ),
        (
            "all, a description word",
            "all",
            "gateway",
            &["search-scope-app-1"],
        ),
        (
            "all, a description word upper-cased",
            "all",
            "GATEWAY",
            &["search-scope-app-1"],
        ),
        (
            "all, a tag value",
            "all",
            "web",
            &["search-scope-app-1"],
        ),
        (
            "all, a whole hyphenated tag key",
            "all",
            "search-scope-tier",
            &["search-scope-app-1", "search-scope-db-1"],
        ),
        (
            "all, one word of a hyphenated tag key",
            "all",
            "tier",
            &["search-scope-app-1", "search-scope-db-1"],
        ),
        (
            "all, a word of a hyphenated name",
            "all",
            "db",
            &["search-scope-db-1"],
        ),
        (
            "all, a prefix shared by a description word and a tag value",
            "all",
            "data",
            &["search-scope-app-2", "search-scope-db-1"],
        ),
        (
            "all, two words one secret carries between its attributes",
            "all",
            "payment gateway",
            &["search-scope-app-1"],
        ),
        (
            "all, two words no secret in scope carries together",
            "all",
            "payment login",
            &[],
        ),
    ];

    // The scope on its own lists every secret inside it and nothing outside it,
    // which is the baseline each row narrows.
    let scoping = json!([{"Key": "name", "Values": [scope]}]);
    let (baseline, pages) = walked(&router, &scoping, None, "the name scope alone").await;
    assert_eq!(
        baseline,
        vec![
            "search-scope-app-1".to_string(),
            "search-scope-app-2".to_string(),
            "search-scope-db-1".to_string(),
        ],
        "expected the name scope to list exactly the secrets inside it"
    );
    assert_eq!(
        pages, 1,
        "expected one page with no MaxResults and no NextToken outstanding, took {pages}"
    );

    for (case, key, value, expected) in rows {
        let filters = json!([
            {"Key": "name", "Values": [scope]},
            {"Key": key, "Values": [value]},
        ]);
        let expected: Vec<String> = expected.iter().map(|name| (*name).to_string()).collect();

        // One walk over the whole remainder: the documented answer, reported in
        // a single page that leaves no NextToken behind.
        let (selected, pages) = walked(&router, &filters, None, case).await;
        assert_eq!(
            selected, expected,
            "case {case:?}: filter {key}={value:?} inside the scope selected the wrong set"
        );
        assert_eq!(
            pages, 1,
            "case {case:?}: expected the whole selection in one page with no NextToken \
             outstanding, took {pages}"
        );

        // And the same selection walked one secret at a time, which is what an
        // SDK paginator does: the same names, in the same order, and the walk
        // ends.
        let (paged, _) = walked(&router, &filters, Some(1), case).await;
        assert_eq!(
            paged, expected,
            "case {case:?}: paging {key}={value:?} one secret at a time selected the wrong set"
        );
    }

    // The narrowing is observable from the other side too: the same text filter
    // WITHOUT the scope beside it selects the secret outside the scope as well,
    // so the rows above leave it out because the two filters are ANDed and not
    // because it fails the text filter.
    let unscoped = json!([{"Key": "description", "Values": ["Payment"]}]);
    let (both_scopes, pages) = walked(&router, &unscoped, None, "the text filter alone").await;
    assert_eq!(
        both_scopes,
        vec![
            "other-scope-app-9".to_string(),
            "search-scope-app-1".to_string(),
        ],
        "expected the text filter alone to select the matching secret in either scope"
    );
    assert_eq!(
        pages, 1,
        "expected one page with no MaxResults and no NextToken outstanding, took {pages}"
    );

    // "name: Prefix match, case-sensitive." (`Filter$Key`, this service's Smithy
    // model.) So a scope differing only in case selects nothing, whatever the
    // filter beside it matches.
    let shouted = json!([
        {"Key": "description", "Values": ["Payment"]},
        {"Key": "name", "Values": [scope.to_uppercase()]},
    ]);
    let (none, pages) = walked(&router, &shouted, None, "an upper-cased name scope").await;
    assert_eq!(
        none,
        Vec::<String>::new(),
        "expected an upper-cased name scope to select nothing, since `name` is case-sensitive"
    );
    assert_eq!(
        pages, 1,
        "expected the empty selection to be one page with no NextToken outstanding, took {pages}"
    );
}

#[tokio::test]
async fn an_empty_description_is_the_same_as_none_at_all() {
    // The rows run over the operation that set the description and the value it
    // carried: `DescriptionType` is modelled with a minimum length of 0, so the
    // empty string is accepted on the wire — and it describes nothing.
    let router = emulator();
    let rows: &[(&str, Option<&str>, Option<&str>)] = &[
        ("an absent Description", None, None),
        ("an empty Description", Some(""), None),
        ("a Description of one space", Some(" "), Some(" ")),
        ("a Description with text", Some("described"), Some("described")),
    ];
    for (index, (case, sent, expected)) in rows.iter().enumerate() {
        let name = format!("described/{index}");
        let mut create = json!({"Name": &name, "SecretString": "v"});
        if let Some(sent) = sent {
            create["Description"] = json!(sent);
        }
        let created = ok(&router, "CreateSecret", create, case).await;
        assert_eq!(
            text(&created, "Name", case),
            name,
            "case {case:?}: expected the create to answer for the secret it created"
        );
        let described = ok(&router, "DescribeSecret", json!({"SecretId": &name}), case).await;
        assert_eq!(
            described.get("Description").and_then(Value::as_str),
            *expected,
            "case {case:?}: expected Description {expected:?} on read-back, got {described}"
        );

        // And the same value through UpdateSecret reads back the same way.
        let updated_name = format!("described/updated/{index}");
        ok(
            &router,
            "CreateSecret",
            json!({"Name": &updated_name, "SecretString": "v", "Description": "to be replaced"}),
            case,
        )
        .await;
        if let Some(sent) = sent {
            ok(
                &router,
                "UpdateSecret",
                json!({"SecretId": &updated_name, "Description": sent}),
                case,
            )
            .await;
            let updated = ok(
                &router,
                "DescribeSecret",
                json!({"SecretId": &updated_name}),
                case,
            )
            .await;
            assert_eq!(
                updated.get("Description").and_then(Value::as_str),
                *expected,
                "case {case:?}: expected Description {expected:?} after the update, got {updated}"
            );
        }
    }

    // The two creates that describe nothing are indistinguishable on read-back:
    // a client diffing them sees no change.
    let absent = ok(
        &router,
        "DescribeSecret",
        json!({"SecretId": "described/0"}),
        "absent against empty",
    )
    .await;
    let empty = ok(
        &router,
        "DescribeSecret",
        json!({"SecretId": "described/1"}),
        "absent against empty",
    )
    .await;
    assert_eq!(
        absent.get("Description"),
        empty.get("Description"),
        "expected an empty Description and an absent one to read back alike, got {absent} and \
         {empty}"
    );
    assert!(
        empty.get("Description").is_none(),
        "expected no Description key at all, got {empty}"
    );
}

/// How long a test waits for a change the clock owes — here the permanent removal a forced
/// delete schedules — before it gives up. Generous on purpose: the test waits for the
/// CONDITION (the name is free again) rather than for a span, so a loaded machine only takes
/// more turns around the loop.
const REMOVAL_WAIT: Duration = Duration::from_secs(60);

/// How often such a loop looks again.
const LOOK_INTERVAL: Duration = Duration::from_millis(20);

/// Send one request that must be refused, and return its wire error code and message.
async fn refusal(
    router: &MultiServiceRouter,
    operation: &str,
    body: Value,
    case: &str,
) -> (String, String) {
    let (status, parsed) = call(router, operation, &body).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected a 400 client error from {operation}, got {status} with body \
         {parsed}"
    );
    let raw = parsed
        .get("__type")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: no __type in error body {parsed}"));
    let message = parsed
        .get("message")
        .or_else(|| parsed.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: no message in error body {parsed}"))
        .to_string();
    (
        raw.split('#').next_back().unwrap_or(raw).to_string(),
        message,
    )
}

/// Assert that `secret_id` names nothing this service holds, through every operation that
/// resolves a `SecretId` and has nothing else to say about it first.
async fn assert_names_nothing(router: &MultiServiceRouter, secret_id: &str, case: &str) {
    for operation in ["GetSecretValue", "DescribeSecret", "RestoreSecret"] {
        let (code, message) =
            refusal(router, operation, json!({"SecretId": secret_id}), case).await;
        assert_eq!(
            code, "ResourceNotFoundException",
            "case {case:?}: expected {operation} on a retired ARN to find nothing, got {code} \
             with message {message:?}"
        );
        assert_eq!(
            message, SECRET_NOT_FOUND_MESSAGE,
            "case {case:?}: expected the secret-not-found sentence from {operation}, got \
             {message:?}"
        );
    }
}

/// Create a secret under `name`, waiting out the removal a forced delete of a same-named
/// secret owes: while that secret is still there the name is reserved and the create is
/// refused, so the loop looks again until the create goes through or [`REMOVAL_WAIT`]
/// passes. Answers the ARN and the version id the create staked.
async fn create_once_the_name_is_free(
    router: &MultiServiceRouter,
    name: &str,
    material: &str,
    case: &str,
) -> (String, String) {
    let deadline = Instant::now() + REMOVAL_WAIT;
    loop {
        let (status, parsed) = call(
            router,
            "CreateSecret",
            &json!({"Name": name, "SecretString": material}),
        )
        .await;
        if status == 200 {
            return (text(&parsed, "ARN", case), text(&parsed, "VersionId", case));
        }
        let refused = parsed
            .get("message")
            .or_else(|| parsed.get("Message"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        assert_eq!(
            refused, NAME_SCHEDULED_FOR_DELETION_MESSAGE,
            "case {case:?}: expected the create to be refused only while the name is still \
             reserved, got {status} with body {parsed}"
        );
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the name was still reserved when the wait ran out, last answer \
             {refused:?}"
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// The six characters a secret's ARN ends in, and the ARN up to them.
fn suffix_of(arn: &str, case: &str) -> (String, String) {
    let cut = arn
        .len()
        .checked_sub(ARN_SUFFIX_LEN + 1)
        .unwrap_or_else(|| panic!("case {case:?}: {arn} is too short to end in a suffix"));
    assert_eq!(
        arn.as_bytes().get(cut),
        Some(&b'-'),
        "case {case:?}: expected {arn} to end in a hyphen and {ARN_SUFFIX_LEN} characters"
    );
    (arn[..cut].to_string(), arn[cut + 1..].to_string())
}

#[tokio::test]
async fn a_secret_created_under_a_destroyed_secrets_name_gets_an_identity_of_its_own() {
    // A secret's ARN ends in six random characters that belong to the SECRET, not to its
    // name, so a secret created under the name of one the service has already removed is a
    // new secret with a new ARN — and every handle on the removed secret (its ARN, and the
    // ids of the versions it held) names nothing from then on.
    //
    // The rows differ along how the destroyed secret was removed and what it held: the
    // forced delete alone, the same after a windowed delete was already pending, a secret
    // holding several versions, and a name reused twice over.
    let rows: &[(&str, bool, usize, usize)] = &[
        ("a forced delete", false, 1_usize, 1_usize),
        ("a forced delete over a pending window", true, 1, 1),
        ("a forced delete of a secret holding three versions", false, 3, 1),
        ("a name reused twice over", false, 1, 2),
    ];
    for (case, window_first, versions, generations) in rows {
        let router = emulator();
        let name = format!("reused/{}", case.replace(' ', "-"));
        let mut retired: Vec<String> = Vec::new();
        let mut retired_versions: Vec<String> = Vec::new();
        let mut live = {
            let created = ok(
                &router,
                "CreateSecret",
                json!({"Name": &name, "SecretString": "original-material"}),
                case,
            )
            .await;
            let arn = text(&created, "ARN", case);
            retired_versions.push(text(&created, "VersionId", case));
            for version in 1..*versions {
                let written = ok(
                    &router,
                    "PutSecretValue",
                    json!({"SecretId": &arn, "SecretString": format!("material-{version}")}),
                    case,
                )
                .await;
                retired_versions.push(text(&written, "VersionId", case));
            }
            arn
        };

        for generation in 0..*generations {
            if *window_first {
                ok(
                    &router,
                    "DeleteSecret",
                    json!({"SecretId": &live, "RecoveryWindowInDays": 7}),
                    case,
                )
                .await;
            }
            ok(
                &router,
                "DeleteSecret",
                json!({"SecretId": &live, "ForceDeleteWithoutRecovery": true}),
                case,
            )
            .await;
            retired.push(live.clone());

            let (reborn, version) = create_once_the_name_is_free(
                &router,
                &name,
                &format!("reborn-material-{generation}"),
                case,
            )
            .await;

            // The new secret is the same NAME in the same account and Region, so everything
            // of the ARN but those six characters is the same — and those six are its own.
            let (stem, fresh) = suffix_of(&reborn, case);
            let (retired_stem, _) = suffix_of(&live, case);
            assert_eq!(
                stem, retired_stem,
                "case {case:?}: expected the re-created secret's ARN to name the same secret \
                 name, got {reborn} against {live}"
            );
            assert_eq!(
                fresh.len(),
                ARN_SUFFIX_LEN,
                "case {case:?}: expected {ARN_SUFFIX_LEN} trailing characters, got {fresh:?}"
            );
            assert!(
                fresh.chars().all(char::is_alphanumeric),
                "case {case:?}: expected the trailing characters to be letters and digits, got \
                 {fresh:?}"
            );
            for gone in &retired {
                assert_ne!(
                    &reborn, gone,
                    "case {case:?}: expected the re-created secret to carry an ARN of its own, \
                     got the destroyed secret's {gone}"
                );
            }

            // Every handle on a destroyed secret names nothing: its ARN, and the ids of the
            // versions it held — which the live secret of the same name does not answer for
            // either.
            for gone in &retired {
                assert_names_nothing(&router, gone, case).await;
            }
            for gone in &retired_versions {
                let (code, message) = refusal(
                    &router,
                    "GetSecretValue",
                    json!({"SecretId": &name, "VersionId": gone}),
                    case,
                )
                .await;
                assert_eq!(
                    code, "ResourceNotFoundException",
                    "case {case:?}: expected a destroyed secret's version id to resolve to \
                     nothing, got {code} with message {message:?}"
                );
                assert_eq!(
                    message,
                    value_version_not_found_message(gone),
                    "case {case:?}: expected the version-not-found sentence naming {gone}, got \
                     {message:?}"
                );
            }

            // The secret that now holds the name answers from its own material, read back
            // through its own ARN.
            let read = ok(
                &router,
                "GetSecretValue",
                json!({"SecretId": &reborn}),
                case,
            )
            .await;
            assert_eq!(
                text(&read, "SecretString", case),
                format!("reborn-material-{generation}"),
                "case {case:?}: expected the re-created secret to hold its own material, got \
                 {read}"
            );
            retired_versions.push(version);
            live = reborn;
        }

        // The six characters belong to the LIVE secret for as long as it exists: the name
        // keeps resolving to the ARN the last create allocated.
        let described = ok(
            &router,
            "DescribeSecret",
            json!({"SecretId": &name}),
            case,
        )
        .await;
        assert_eq!(
            text(&described, "ARN", case),
            live,
            "case {case:?}: expected the name to keep resolving to the live secret's own ARN, \
             got {described}"
        );
    }
}
