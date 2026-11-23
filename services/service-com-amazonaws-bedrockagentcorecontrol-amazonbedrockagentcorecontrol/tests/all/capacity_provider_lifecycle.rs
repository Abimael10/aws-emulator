// SPDX-License-Identifier: CC0-1.0

//! The capacity provider end to end over HTTP: what `CreateCapacityProvider` persists, how
//! `GetCapacityProvider` and `ListCapacityProviders` report it, and the status the clock owes
//! it.
//!
//! Every row drives the operations as a client does — one shared engine behind one router —
//! so a create and the reads that follow it are asserted against the same store rather than
//! against each other's envelopes.
//!
//! Sources: `smithy_query(query="closure", operation="CreateCapacityProvider")` and
//! `smithy_query(query="operation",
//! operation="GetCapacityProvider,ListCapacityProviders")` for every member, id and ARN form
//! asserted here, and the operation's own documentation for the `CREATING` -> `READY`
//! transition ("After you create the capacity provider, it enters a CREATING state and
//! transitions to READY when it is available for use") and for the provider being identified
//! by the `capacityProviderId` this service mints rather than by its name.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol::EmulatedService;

/// The region the rows sign for, which the ARNs they assert carry.
const REGION: &str = "us-east-1";

/// The account id every emulated ARN carries.
const ACCOUNT: &str = "000000000000";

/// How long a poll for the clock-owed `READY` keeps looking, and how long it waits between
/// looks. Generous: the condition is what the test waits for, not a duration.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);
const SETTLE_POLL: Duration = Duration::from_millis(50);

fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::default();
    router.register_service(EmulatedService::new(engine));
    router
}

fn authorization() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/bedrock-agentcore/\
         aws4_request, SignedHeaders=host, Signature=abc"
    )
}

async fn send(router: &MultiServiceRouter, method: &str, uri: &str, body: Option<&Value>) -> (u16, Value) {
    let payload = body.map_or_else(Bytes::new, |value| {
        Bytes::from(serde_json::to_vec(value).expect("the request body serializes"))
    });
    let mut request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", authorization())
        .header("host", "bedrock-agentcore-control.us-east-1.amazonaws.com");
    if body.is_some() {
        request = request.header("content-type", "application/json");
    }
    let request = request.body(payload).expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

async fn create_provider(router: &MultiServiceRouter, body: &Value) -> (u16, Value) {
    send(router, "PUT", "/capacity-providers", Some(body)).await
}

async fn get_provider(router: &MultiServiceRouter, id: &str) -> (u16, Value) {
    send(router, "GET", &format!("/capacity-providers/{id}"), None).await
}

async fn list_providers(router: &MultiServiceRouter, query: &str) -> (u16, Value) {
    send(router, "POST", &format!("/capacity-providers{query}"), None).await
}

fn text(body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The error code `restJson1` puts in `__type`.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// The compute configuration a provider is created with, as one union member carrying the
/// EC2 configuration — every value inside its own modelled constraint.
fn compute_configuration(idle_timeout: i64) -> Value {
    json!({
        "ec2Configuration": {
            "launchTemplateSource": {
                "launchParameters": {
                    "operatingSystem": "LINUX_X86_64",
                    "instanceRequirements": {"allowedInstanceTypes": ["m5.large", "m6i.large"]},
                    "monitoring": "DETAILED"
                }
            },
            "vpcConfiguration": {
                "subnets": ["subnet-0123456789abcdef0"],
                "securityGroups": ["sg-0123456789abcdef0"]
            },
            "lifecycleConfiguration": {"idleInstanceTimeout": idle_timeout},
            "rootVolume": {"volumeType": "gp3", "iops": 3000, "throughput": 125}
        }
    })
}

fn permissions_configuration() -> Value {
    json!({"capacityProviderOperatorRoleArn":
        "arn:aws:iam::000000000000:role/AgentCoreCapacityProvider"})
}

/// One create this suite drives, and what a read of the provider must report back.
struct Row {
    case: &'static str,
    body: Value,
}

/// What `CreateCapacityProvider` persists is what a read of the provider reports: the
/// configuration, the description and the status, over creates that differ in the members
/// they carry.
#[tokio::test]
async fn a_created_capacity_provider_reads_back() {
    let router = router();
    let rows = vec![
        Row {
            case: "the required members alone",
            body: json!({
                "name": "minimalProvider",
                "permissionsConfiguration": permissions_configuration(),
                "computeConfiguration": compute_configuration(900)
            }),
        },
        Row {
            case: "a description and tags beside them",
            body: json!({
                "name": "describedProvider",
                "description": "the capacity provider the suite reads back",
                "permissionsConfiguration": permissions_configuration(),
                "computeConfiguration": compute_configuration(60),
                "tags": {"owner": "agentcore", "tier": "test"}
            }),
        },
        Row {
            case: "a name at the member's maximum length",
            body: json!({
                // `CapacityProviderName` is @length 1..48 with pattern
                // `^[a-zA-Z][a-zA-Z0-9_]{0,47}$`.
                "name": format!("p{}", "x".repeat(47)),
                "permissionsConfiguration": permissions_configuration(),
                "computeConfiguration": compute_configuration(1_209_600)
            }),
        },
    ];

    for row in &rows {
        let case = row.case;
        let (status, created) = create_provider(&router, &row.body).await;
        assert_eq!(
            status, 202,
            "case {case:?}: expected HTTP 202 from CreateCapacityProvider, got {status} \
             ({created})"
        );
        let name = text(&row.body, "name");
        let id = text(&created, "capacityProviderId");
        let expected_arn = format!("arn:aws:bedrock-agentcore:{REGION}:{ACCOUNT}:capacity-provider/{id}");
        assert_eq!(
            (
                text(&created, "name"),
                text(&created, "capacityProviderArn"),
                text(&created, "status")
            ),
            (name.clone(), expected_arn.clone(), "CREATING".to_string()),
            "case {case:?}: the create reports the name, the provider's ARN and the status it \
             enters, got {created}"
        );
        // `CapacityProviderId` is `^[a-zA-Z][a-zA-Z0-9_]{0,47}-[a-zA-Z0-9]{10}$`: the name,
        // then a 10-character suffix this service minted.
        let suffix = id
            .strip_prefix(&format!("{name}-"))
            .unwrap_or_else(|| panic!("case {case:?}: {id:?} is not {name:?} plus a suffix"));
        assert!(
            suffix.len() == 10 && suffix.chars().all(|character| character.is_ascii_alphanumeric()),
            "case {case:?}: the minted id's suffix is 10 alphanumerics, got {suffix:?}"
        );

        let (status, fetched) = get_provider(&router, &id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from GetCapacityProvider, got {status} ({fetched})"
        );
        assert_eq!(
            (
                text(&fetched, "capacityProviderId"),
                text(&fetched, "capacityProviderArn"),
                text(&fetched, "name"),
                text(&fetched, "status")
            ),
            (id.clone(), expected_arn, name.clone(), "CREATING".to_string()),
            "case {case:?}: the read reports the provider the create minted, got {fetched}"
        );
        assert_eq!(
            fetched.get("computeConfiguration"),
            row.body.get("computeConfiguration"),
            "case {case:?}: the read reports the compute configuration the create persisted, \
             got {fetched}"
        );
        assert_eq!(
            fetched.get("permissionsConfiguration"),
            row.body.get("permissionsConfiguration"),
            "case {case:?}: the read reports the permissions configuration the create \
             persisted, got {fetched}"
        );
        assert_eq!(
            fetched.get("description"),
            row.body.get("description"),
            "case {case:?}: the read reports the description the create carried, got {fetched}"
        );
        // `statusCode` and `statusReason` explain a provider that is NOT on its way to
        // READY, so a provider this create minted reports neither.
        assert!(
            fetched.get("statusCode").is_none() && fetched.get("statusReason").is_none(),
            "case {case:?}: a provider on its way to READY carries no reason code, got {fetched}"
        );
        for instant in ["createdAt", "lastUpdatedAt"] {
            assert!(
                fetched.get(instant).is_some(),
                "case {case:?}: the read reports {instant}, got {fetched}"
            );
        }
    }

    // Every provider created above is listed, each with the summary the read agrees with.
    let (status, listed) = list_providers(&router, "").await;
    assert_eq!(
        status, 200,
        "case \"the listing\": expected HTTP 200 from ListCapacityProviders, got {status} \
         ({listed})"
    );
    let summaries = listed
        .get("capacityProviders")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut listed_names: Vec<String> = summaries
        .iter()
        .map(|summary| text(summary, "name"))
        .collect();
    listed_names.sort();
    let mut created_names: Vec<String> = rows.iter().map(|row| text(&row.body, "name")).collect();
    created_names.sort();
    assert_eq!(
        listed_names, created_names,
        "case \"the listing\": expected every created provider, got {listed}"
    );
    for summary in &summaries {
        let id = text(summary, "capacityProviderId");
        let (_, fetched) = get_provider(&router, &id).await;
        assert_eq!(
            (
                text(summary, "capacityProviderArn"),
                text(summary, "name"),
                text(summary, "status")
            ),
            (
                text(&fetched, "capacityProviderArn"),
                text(&fetched, "name"),
                text(&fetched, "status")
            ),
            "case \"the listing\": a summary and a read of {id} report the same provider, got \
             {summary} and {fetched}"
        );
        assert!(
            summary.get("lastUpdatedAt").is_some(),
            "case \"the listing\": a summary reports lastUpdatedAt, got {summary}"
        );
    }
}

/// A capacity provider is identified by the id this service mints, not by its name: a create
/// carrying a name the account already holds is served and mints a second, independently
/// identified provider, and both read back and list.
///
/// The rows differ along what the rule generalises over: how many creates share the one name,
/// whether the repeats carry the same configuration or a different one, and the shape of the
/// name itself (a short lower-case word, a mixed-case name with digits and an underscore, and
/// the longest `CapacityProviderName` admits — @length 1..48, pattern
/// `^[a-zA-Z][a-zA-Z0-9_]{0,47}$`).
#[tokio::test]
async fn a_name_this_account_already_holds_mints_a_second_provider() {
    /// The idle timeout the `attempt`-th create of a row carries: the same one every time,
    /// or its own when the row varies the configuration.
    fn idle_timeout(attempt: usize, vary: bool) -> i64 {
        let attempt = i64::try_from(attempt).expect("a row drives a handful of creates");
        if vary {
            900 + attempt
        } else {
            900
        }
    }

    // (case, the shared name, how many creates carry it, whether the repeats differ in
    // configuration)
    let rows: [(&str, String, usize, bool); 4] = [
        (
            "a second create of the same name",
            "twiceProvider".to_string(),
            2,
            false,
        ),
        (
            "a second create of the same name with another configuration",
            "reconfiguredProvider".to_string(),
            2,
            true,
        ),
        (
            "a mixed-case name with digits and an underscore, created three times",
            "Fleet_7Provider".to_string(),
            3,
            false,
        ),
        (
            "a name at the member's maximum length, created twice",
            format!("q{}", "y".repeat(47)),
            2,
            true,
        ),
    ];

    for (case, name, creates, vary) in rows {
        // One router per row, so the account each row starts from holds no provider.
        let router = router();
        let mut ids: Vec<String> = Vec::new();
        for attempt in 0..creates {
            let body = json!({
                "name": name,
                "permissionsConfiguration": permissions_configuration(),
                "computeConfiguration": compute_configuration(idle_timeout(attempt, vary))
            });
            let (status, created) = create_provider(&router, &body).await;
            assert_eq!(
                status, 202,
                "case {case:?} / create {attempt}: expected HTTP 202 for the reused name, got \
                 {status} ({created})"
            );
            assert_eq!(
                text(&created, "name"),
                name,
                "case {case:?} / create {attempt}: the create reports the shared name, got \
                 {created}"
            );
            let id = text(&created, "capacityProviderId");
            let suffix = id
                .strip_prefix(&format!("{name}-"))
                .unwrap_or_else(|| panic!("case {case:?}: {id:?} is not {name:?} plus a suffix"));
            assert!(
                suffix.len() == 10 && suffix.chars().all(|character| character.is_ascii_alphanumeric()),
                "case {case:?} / create {attempt}: the minted id's suffix is 10 alphanumerics, \
                 got {suffix:?}"
            );
            assert_eq!(
                text(&created, "capacityProviderArn"),
                format!("arn:aws:bedrock-agentcore:{REGION}:{ACCOUNT}:capacity-provider/{id}"),
                "case {case:?} / create {attempt}: the ARN carries the minted id, got {created}"
            );
            assert!(
                !ids.contains(&id),
                "case {case:?} / create {attempt}: every create of the name mints its own id, \
                 got {id} again among {ids:?}"
            );
            ids.push(id);
        }

        // Each provider is its own resource: readable by its own id, reporting the shared
        // name and the configuration ITS create carried.
        for (attempt, id) in ids.iter().enumerate() {
            let (status, fetched) = get_provider(&router, id).await;
            assert_eq!(
                status, 200,
                "case {case:?} / provider {attempt}: expected HTTP 200 from \
                 GetCapacityProvider, got {status} ({fetched})"
            );
            assert_eq!(
                (text(&fetched, "capacityProviderId"), text(&fetched, "name")),
                (id.clone(), name.clone()),
                "case {case:?} / provider {attempt}: the read reports this provider under the \
                 shared name, got {fetched}"
            );
            let expected_timeout = json!(idle_timeout(attempt, vary));
            assert_eq!(
                fetched.pointer(
                    "/computeConfiguration/ec2Configuration/lifecycleConfiguration/\
                     idleInstanceTimeout"
                ),
                Some(&expected_timeout),
                "case {case:?} / provider {attempt}: the read reports the configuration its own \
                 create persisted, got {fetched}"
            );
        }

        // And the listing reports one entry per provider, all sharing the name.
        let (status, listed) = list_providers(&router, "").await;
        let summaries = listed
            .get("capacityProviders")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut listed_ids: Vec<String> = summaries
            .iter()
            .map(|summary| text(summary, "capacityProviderId"))
            .collect();
        listed_ids.sort();
        let mut expected_ids = ids.clone();
        expected_ids.sort();
        assert_eq!(
            (status, listed_ids),
            (200, expected_ids),
            "case {case:?}: the listing reports every provider minted under the name, got \
             {listed}"
        );
        assert!(
            summaries
                .iter()
                .all(|summary| text(summary, "name") == name),
            "case {case:?}: every listed provider reports the shared name, got {listed}"
        );
    }
}

/// A replayed `clientToken` answers with the provider that token first produced rather than
/// minting a second one — whatever else the repeated request carries.
#[tokio::test]
async fn a_repeated_client_token_answers_with_the_first_provider() {
    let router = router();
    // `ClientToken` is @length 33..256 with pattern `^[a-zA-Z0-9](-*[a-zA-Z0-9]){0,256}$`.
    let token = "a".repeat(33);
    let body = json!({
        "name": "idempotentProvider",
        "clientToken": token,
        "permissionsConfiguration": permissions_configuration(),
        "computeConfiguration": compute_configuration(900)
    });
    let (status, first) = create_provider(&router, &body).await;
    assert_eq!(
        status, 202,
        "case \"the first create\": expected HTTP 202, got {status} ({first})"
    );

    let (status, replayed) = create_provider(&router, &body).await;
    assert_eq!(
        (status, text(&replayed, "capacityProviderId")),
        (202, text(&first, "capacityProviderId")),
        "case \"the replayed token\": expected the provider the token first produced, got \
         {status} ({replayed})"
    );

    let (_, listed) = list_providers(&router, "").await;
    let summaries = listed
        .get("capacityProviders")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        summaries.len(),
        1,
        "case \"the replayed token\": the replay minted no second provider, got {listed}"
    );
}

/// An id naming no provider this account holds is the modelled 404, for ids that differ only
/// in which part of a well-formed id they carry.
#[tokio::test]
async fn an_id_naming_no_provider_is_not_found() {
    let router = router();
    let (status, created) = create_provider(
        &router,
        &json!({
            "name": "heldProvider",
            "permissionsConfiguration": permissions_configuration(),
            "computeConfiguration": compute_configuration(900)
        }),
    )
    .await;
    assert_eq!(
        status, 202,
        "case \"the create\": expected HTTP 202, got {status} ({created})"
    );
    let held = text(&created, "capacityProviderId");

    // (case, the id the read names) — each inside `CapacityProviderId`'s own format.
    let rows: [(&str, String); 3] = [
        ("a name this account never created", "otherProvider-abcdefghij".to_string()),
        (
            "the held provider's name with another suffix",
            format!("heldProvider-{}", "z".repeat(10)),
        ),
        ("an id at the member's minimum length", "a-bcdefghijk".to_string()),
    ];

    for (case, id) in rows {
        let (status, refused) = get_provider(&router, &id).await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (404, "ResourceNotFoundException"),
            "case {case:?} ({id}): expected HTTP 404 ResourceNotFoundException, got {status} {} \
             ({refused})",
            error_code(&refused)
        );
        assert_eq!(
            text(&refused, "message"),
            format!("No capacity provider found with id {id}"),
            "case {case:?}: got {refused}"
        );
    }

    let (status, fetched) = get_provider(&router, &held).await;
    assert_eq!(
        status, 200,
        "case \"the held provider\": the id the create minted is still served, got {status} \
         ({fetched})"
    );
}

/// The clock-owed transition: a provider read straight after the create is still `CREATING`,
/// and reports `READY` once the span the create owes has passed. The test waits for the
/// CONDITION, polling the read until it holds.
#[tokio::test]
async fn a_created_provider_settles_from_creating_to_ready() {
    let router = router();
    let (status, created) = create_provider(
        &router,
        &json!({
            "name": "settlingProvider",
            "permissionsConfiguration": permissions_configuration(),
            "computeConfiguration": compute_configuration(900)
        }),
    )
    .await;
    assert_eq!(
        status, 202,
        "case \"the create\": expected HTTP 202, got {status} ({created})"
    );
    let id = text(&created, "capacityProviderId");
    assert_eq!(
        text(&created, "status"),
        "CREATING",
        "case \"straight after the create\": a new provider enters CREATING, got {created}"
    );

    let (_, fetched) = get_provider(&router, &id).await;
    assert_eq!(
        text(&fetched, "status"),
        "CREATING",
        "case \"a caller acting straight after the create\": the transient status is still \
         reported, got {fetched}"
    );

    let started = Instant::now();
    let mut last = fetched;
    while started.elapsed() < SETTLE_DEADLINE && text(&last, "status") != "READY" {
        tokio::time::sleep(SETTLE_POLL).await;
        let (_, fresh) = get_provider(&router, &id).await;
        last = fresh;
    }
    assert_eq!(
        text(&last, "status"),
        "READY",
        "case \"past the settle span\": the CREATING -> READY transition is due, got {last}"
    );

    // The listing reports the same settled status, from the same record.
    let (_, listed) = list_providers(&router, "").await;
    let summary = listed["capacityProviders"][0].clone();
    assert_eq!(
        (text(&summary, "capacityProviderId"), text(&summary, "status")),
        (id, "READY".to_string()),
        "case \"the listing past the settle span\": got {listed}"
    );
}

/// The listing pages over the providers on the opaque cursor, one page at a time, and the
/// pages together are every provider exactly once.
#[tokio::test]
async fn the_listing_pages_over_every_provider() {
    let router = router();
    let names = ["pagedOne", "pagedThree", "pagedTwo"];
    for name in names {
        let (status, created) = create_provider(
            &router,
            &json!({
                "name": name,
                "permissionsConfiguration": permissions_configuration(),
                "computeConfiguration": compute_configuration(900)
            }),
        )
        .await;
        assert_eq!(
            status, 202,
            "case {name:?}: expected HTTP 202, got {status} ({created})"
        );
    }

    let mut seen: Vec<String> = Vec::new();
    let mut query = "?maxResults=2".to_string();
    for page in 0..names.len() + 1 {
        let (status, listed) = list_providers(&router, &query).await;
        assert_eq!(
            status, 200,
            "case \"page {page}\": expected HTTP 200, got {status} ({listed})"
        );
        let summaries = listed
            .get("capacityProviders")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert!(
            summaries.len() <= 2,
            "case \"page {page}\": a page holds at most the maxResults asked for, got {listed}"
        );
        seen.extend(summaries.iter().map(|summary| text(summary, "name")));
        match listed.get("nextToken").and_then(Value::as_str) {
            Some(token) => query = format!("?maxResults=2&nextToken={token}"),
            None => break,
        }
    }

    let mut expected: Vec<String> = names.iter().map(|name| (*name).to_string()).collect();
    expected.sort();
    let mut actual = seen.clone();
    actual.sort();
    assert_eq!(
        actual, expected,
        "case \"the pages together\": expected every provider exactly once, got {seen:?}"
    );
}
