// SPDX-License-Identifier: CC0-1.0

//! Every paginated read of this service hands back an OPAQUE `nextToken`, over a table of
//! the listings rather than over the one that exposed the rule.
//!
//! The rule under test: `nextToken` "should be treated as an opaque identifier that is only
//! used to retrieve the next items in a list and not for other programmatic purposes"
//! (every paginated member of this API says so), so for each listing
//!
//! 1. the token it hands back carries NO identifier of what it walks — not the ARN, the
//!    name or the id of the entry the next page starts at, nor of any other entry — so
//!    nothing a client logs, caches or routes on exposes a resource of the account, and the
//!    token's own form stays the service's to change;
//! 2. the token still resumes the walk exactly where the page stopped: walking a listing one
//!    page at a time reports every entry exactly once, in order;
//! 3. a value the listing did not mint names no position — one a caller assembled, a plain
//!    resource identifier, and a token another listing of the same store minted — and is
//!    refused with the error and the sentence that listing answers for an unusable cursor
//!    rather than being read as some insertion point in the sequence.
//!
//! The rows span the listings of this service that page: what each one walks (clusters,
//! services, tasks, container instances, task-definition revisions, families, account
//! settings, capacity providers), how its entries are identified (an ARN, a name, a
//! `family:revision` pair) and which refusal each answers.
//!
//! Every case goes through the shared wire harness in [`crate::ecs_wire`], the same front
//! door a client uses, so a token that is not minted and read by the handler cannot pass.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{
    list, ok, refusal, register_capacity, register_definition, service, ACCOUNT, SIGNED_REGION,
};

/// How many entries each listing in the table is seeded with, and the page size every walk
/// uses: three entries at one per page is two tokens, so a token is minted, presented and
/// minted again.
const SEEDED: usize = 3;

/// One listing under test.
struct Listing {
    /// The case name.
    case: &'static str,
    /// The operation to call.
    operation: &'static str,
    /// The request, without `maxResults`/`nextToken`.
    request: fn() -> Value,
    /// The output member carrying the page.
    member: &'static str,
    /// The entries expected, in page order, each rendered as the listing reports it.
    expected: fn() -> Vec<Value>,
    /// Every string the token must NOT carry: the identifiers of what this listing walks.
    identifiers: fn() -> Vec<String>,
    /// The error code this listing answers for a cursor it never minted.
    refusal_code: &'static str,
    /// The sentence that refusal carries.
    refusal_message: &'static str,
}

/// The cluster every cluster-scoped listing in the table walks.
const CLUSTER: &str = "paging";

/// The task-definition family the revision listings walk.
const FAMILY: &str = "pagingfamily";

/// `ClientException`, which the task-definition and attribute listings answer for a cursor
/// they never minted.
const CLIENT: &str = "ClientException";

/// `InvalidParameterException`, which the cluster, service, task, container-instance,
/// capacity-provider and account-setting reads answer for the same condition.
const INVALID_PARAMETER: &str = "InvalidParameterException";

/// The sentence `crate::state::INVALID_NEXT_TOKEN_MESSAGE` spells, as the listings that
/// answer it carry it.
const INVALID_NEXT_TOKEN: &str = "Invalid nextToken.";

fn cluster_arn(name: &str) -> String {
    format!("arn:aws:ecs:{SIGNED_REGION}:{ACCOUNT}:cluster/{name}")
}

fn service_arn(name: &str) -> String {
    format!("arn:aws:ecs:{SIGNED_REGION}:{ACCOUNT}:service/{CLUSTER}/{name}")
}

fn definition_arn(revision: usize) -> String {
    format!("arn:aws:ecs:{SIGNED_REGION}:{ACCOUNT}:task-definition/{FAMILY}:{revision}")
}

/// The names the seeded clusters carry, in page order.
fn cluster_names() -> Vec<String> {
    (0..SEEDED).map(|index| format!("pag-{index}")).collect()
}

/// The names the seeded services carry, in page order.
fn service_names() -> Vec<String> {
    (0..SEEDED).map(|index| format!("svc-{index}")).collect()
}

/// The names the seeded capacity providers carry, in page order.
fn provider_names() -> Vec<String> {
    (0..SEEDED).map(|index| format!("prov-{index}")).collect()
}

/// The listings under test.
fn listings() -> Vec<Listing> {
    vec![
        Listing {
            case: "ListClusters walks cluster ARNs",
            operation: "ListClusters",
            request: || json!({}),
            member: "clusterArns",
            expected: || {
                let mut arns: Vec<String> = cluster_names()
                    .iter()
                    .map(|name| cluster_arn(name))
                    .collect();
                arns.push(cluster_arn(CLUSTER));
                arns.sort();
                arns.into_iter().map(Value::from).collect()
            },
            identifiers: || {
                cluster_names()
                    .iter()
                    .map(|name| cluster_arn(name))
                    .chain(cluster_names())
                    .collect()
            },
            refusal_code: INVALID_PARAMETER,
            // `ListClusters` carries the token the request sent in its own sentence.
            refusal_message: "",
            },
        Listing {
            case: "ListServices walks service ARNs",
            operation: "ListServices",
            request: || json!({ "cluster": CLUSTER }),
            member: "serviceArns",
            expected: || {
                service_names()
                    .iter()
                    .map(|name| Value::from(service_arn(name)))
                    .collect()
            },
            identifiers: || {
                service_names()
                    .iter()
                    .map(|name| service_arn(name))
                    .chain(service_names())
                    .collect()
            },
            refusal_code: INVALID_PARAMETER,
            refusal_message: INVALID_NEXT_TOKEN,
        },
        Listing {
            case: "ListTaskDefinitions walks revision ARNs",
            operation: "ListTaskDefinitions",
            request: || json!({ "familyPrefix": FAMILY }),
            member: "taskDefinitionArns",
            expected: || {
                (1..=SEEDED)
                    .map(|revision| Value::from(definition_arn(revision)))
                    .collect()
            },
            identifiers: || {
                (1..=SEEDED)
                    .map(definition_arn)
                    .chain((1..=SEEDED).map(|revision| format!("{FAMILY}:{revision}")))
                    .chain(::std::iter::once(FAMILY.to_string()))
                    .collect()
            },
            refusal_code: CLIENT,
            refusal_message: INVALID_NEXT_TOKEN,
        },
        Listing {
            case: "DescribeCapacityProviders walks providers by name",
            operation: "DescribeCapacityProviders",
            request: || json!({}),
            member: "capacityProviders",
            expected: || {
                // The two providers Amazon ECS manages are reported beside the account's
                // own, and the page sequence is ordered by name.
                let mut names: Vec<String> = provider_names();
                names.push("FARGATE".to_string());
                names.push("FARGATE_SPOT".to_string());
                names.sort();
                names.into_iter().map(Value::from).collect()
            },
            identifiers: provider_names,
            refusal_code: INVALID_PARAMETER,
            refusal_message: INVALID_NEXT_TOKEN,
        },
    ]
}

/// One page of `operation`: the entries it carries and the token it handed back.
async fn page(
    router: &MultiServiceRouter,
    listing: &Listing,
    token: Option<&str>,
    case: &str,
) -> (Vec<Value>, Option<String>) {
    let mut request = (listing.request)();
    request["maxResults"] = json!(1);
    if let Some(token) = token {
        request["nextToken"] = json!(token);
    }
    let body = ok(router, listing.operation, request, case).await;
    let entries = list(&body, listing.member, case);
    let next = body["nextToken"].as_str().map(str::to_string);
    (entries, next)
}

/// A capacity provider's name, which is how `DescribeCapacityProviders` identifies the
/// entries it reports: the member every row of that listing is compared on.
fn entry_identity(listing: &Listing, entry: &Value) -> Value {
    if listing.operation == "DescribeCapacityProviders" {
        entry["name"].clone()
    } else {
        entry.clone()
    }
}

/// Seed the store every row walks: a cluster holding `SEEDED` services, `SEEDED` more
/// clusters, `SEEDED` revisions of one family, and `SEEDED` capacity providers.
async fn seeded() -> MultiServiceRouter {
    let router = service();
    let case = "setup";
    crate::ecs_wire::create_cluster(&router, CLUSTER, case).await;
    register_capacity(&router, CLUSTER, case).await;
    for name in cluster_names() {
        crate::ecs_wire::create_cluster(&router, &name, case).await;
    }
    for revision in 1..=SEEDED {
        let arn = register_definition(&router, FAMILY, case).await;
        assert_eq!(
            arn,
            definition_arn(revision),
            "case {case:?}: expected revision {revision} of {FAMILY}, got {arn:?}"
        );
    }
    for name in service_names() {
        ok(
            &router,
            "CreateService",
            json!({
                "cluster": CLUSTER,
                "serviceName": name,
                "taskDefinition": FAMILY,
                "desiredCount": 0,
                "launchType": "EC2",
            }),
            case,
        )
        .await;
    }
    for name in provider_names() {
        crate::ecs_wire::create_auto_scaling_group(&router, &format!("asg-{name}"), case).await;
        ok(
            &router,
            "CreateCapacityProvider",
            json!({
                "name": name,
                "autoScalingGroupProvider": { "autoScalingGroupArn": format!("asg-{name}") },
            }),
            case,
        )
        .await;
    }
    router
}

/// Rule 1 and rule 2: each listing's token carries none of the identifiers it walks, and
/// resuming with it reports every entry exactly once, in order.
#[tokio::test]
async fn a_page_token_carries_no_identifier_and_resumes_where_its_page_stopped() {
    let router = seeded().await;

    for listing in listings() {
        let case = listing.case;
        let expected = (listing.expected)();
        let identifiers = (listing.identifiers)();

        let mut walked: Vec<Value> = Vec::new();
        let mut token: Option<String> = None;
        let mut pages = 0_usize;
        loop {
            let (entries, next) = page(&router, &listing, token.as_deref(), case).await;
            walked.extend(
                entries
                    .iter()
                    .map(|entry| entry_identity(&listing, entry)),
            );
            pages += 1;
            assert!(
                pages <= expected.len() + 1,
                "case {case:?}: a one-entry-per-page walk of {} entries must not take \
                 {pages} pages, got {walked:?}",
                expected.len()
            );
            match next {
                None => break,
                Some(next) => {
                    for identifier in &identifiers {
                        assert!(
                            !next.contains(identifier.as_str()),
                            "case {case:?}: the token must carry no identifier of what the \
                             listing walks, got {next:?} carrying {identifier:?}"
                        );
                    }
                    token = Some(next);
                }
            }
        }

        assert_eq!(
            walked, expected,
            "case {case:?}: a paged walk must report every entry exactly once, in order"
        );
    }
}

/// Rule 3: a value the listing did not mint — one a caller assembled, a plain resource
/// identifier, and a token ANOTHER listing of the same store minted — names no position.
#[tokio::test]
async fn a_page_token_another_listing_minted_or_a_caller_built_is_refused() {
    let router = seeded().await;

    // One minted token per listing, kept so each row can be offered every other row's.
    let mut minted: Vec<(&'static str, String)> = Vec::new();
    for listing in listings() {
        let (_, next) = page(&router, &listing, None, listing.case).await;
        let token = next.unwrap_or_else(|| {
            panic!(
                "case {:?}: expected a nextToken while entries remain",
                listing.case
            )
        });
        minted.push((listing.operation, token));
    }

    for listing in listings() {
        let case = listing.case;
        // Values a caller could assemble, plus the identifiers this listing used to hand
        // out as its cursor, plus every token another listing minted.
        let mut forged: Vec<(String, String)> = vec![
            ("empty".to_string(), String::new()),
            ("not a token".to_string(), "not a token".to_string()),
            (
                "a base64-looking blob".to_string(),
                "c2ltdWxhdGVkLXRva2Vu==".to_string(),
            ),
            (
                "a hexadecimal value of the right length".to_string(),
                "00".repeat(40),
            ),
        ];
        for identifier in (listing.identifiers)() {
            forged.push((format!("the identifier {identifier:?}"), identifier));
        }
        for (operation, token) in &minted {
            if *operation != listing.operation {
                forged.push((
                    format!("a token {operation} minted"),
                    token.clone(),
                ));
            }
        }

        for (what, token) in forged {
            let row = format!("{case}: {what}");
            let row = row.as_str();
            let mut request = (listing.request)();
            request["nextToken"] = json!(token);
            let (code, message) = refusal(&router, listing.operation, request, row).await;
            assert_eq!(
                code, listing.refusal_code,
                "case {row:?}: expected the error this listing answers for an unusable \
                 cursor, got {code} with {message:?}"
            );
            if !listing.refusal_message.is_empty() {
                assert_eq!(
                    message, listing.refusal_message,
                    "case {row:?}: expected the sentence this listing answers for an \
                     unusable cursor, got {message:?}"
                );
            }
        }
    }
}

/// A token one STORE minted is refused by another: the position travels inside the token
/// under a secret each store draws for itself, so a cursor cannot be carried from one
/// emulator to the next.
#[tokio::test]
async fn a_page_token_of_another_store_names_no_position() {
    let router = seeded().await;
    let other = seeded().await;

    for listing in listings() {
        let case = listing.case;
        let (_, next) = page(&other, &listing, None, case).await;
        let token = next.unwrap_or_else(|| {
            panic!("case {case:?}: expected a nextToken while entries remain")
        });
        let mut request = (listing.request)();
        request["nextToken"] = json!(token.clone());
        let (code, message) = refusal(&router, listing.operation, request, case).await;
        assert_eq!(
            code, listing.refusal_code,
            "case {case:?}: another store's token must name no position, got {code} with \
             {message:?}"
        );
        // And it still resumes in the store that minted it: the refusal is about the key,
        // not about the token's shape.
        let (entries, _) = page(&other, &listing, Some(&token), case).await;
        assert!(
            !entries.is_empty(),
            "case {case:?}: the minting store must resume its own token, got {entries:?}"
        );
    }
}
