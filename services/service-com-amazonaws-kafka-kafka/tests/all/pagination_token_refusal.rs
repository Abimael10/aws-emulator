// SPDX-License-Identifier: CC0-1.0

//! How every paginated listing of this service answers a resume token it cannot
//! read, driven end to end over the same router the emulator serves from.
//!
//! The RULE this suite asserts, over a table rather than one recorded request: a
//! `nextToken` the service cannot read is a PARAMETER it cannot act on, so it is
//! refused with HTTP 400 `BadRequestException` carrying the service's one
//! generic parameter sentence — Amazon MSK words such a refusal the same way
//! whatever the parameter was, and has no pagination-specific sentence — and
//! naming the token member by the name the request spells it with on the wire.
//! The sentence is fixed text end to end, so the unusable token is never echoed
//! back.
//!
//! Both names come from the model
//! (`smithy_query(query="shape",shape="ListConfigurationsRequest,BadRequestException")`
//! on this crate's `smithy-model/com-amazonaws-kafka-kafka.json`): `NextToken` is
//! bound `@httpQuery("nextToken")`, and the member that reports the parameter at
//! fault is `BadRequestException$InvalidParameter`, `jsonName`
//! `invalidParameter`, on an error whose `@httpError` is 400.
//!
//! The rows span what the rule generalises over: every operation of this service
//! that takes a `nextToken` — fifteen, each on its own route, scoped to the
//! region, to a cluster, to a configuration or to a topic, and each with its own
//! token codec — and five forms of unreadable token: a value that is not base64,
//! base64 of an unrelated plaintext, a very long opaque value, a non-ASCII
//! value, and a token ANOTHER listing of this same service minted.
//!
//! The control holds the rest of the surface fixed: a listing handed back the
//! token IT minted resumes with HTTP 200, so this refusal is about a token the
//! service cannot read rather than about paging at all.

use super::kafka_harness::{pct, poll_until_active, ClusterRequest, Emulator, Reply};
// The service's one generic parameter sentence, shared with the other suites of
// this binary so one wording serves every refusal of a parameter the service
// will not read.
use super::refusal_wording::PARAMETERS_NOT_VALID;

/// The parameter an unusable resume token is reported under: `NextToken`'s
/// `@httpQuery` name, which is how the caller's own request spells it.
const NEXT_TOKEN_MEMBER: &str = "nextToken";

/// A topic name for the partition reader's route: well-formed, so the request
/// reaches the token at all. No topic has to exist for a token to be unreadable.
const TOPIC_NAME: &str = "pagination-token-topic";

/// One paginated listing: the operation's name and the route the model binds it
/// to, with `{cluster}`, `{configuration}` and `{topic}` standing for the
/// resource a cluster-, configuration- or topic-scoped listing is addressed
/// under.
struct Listing {
    name: &'static str,
    route: &'static str,
}

impl Listing {
    /// `GET <route>?nextToken=<token>`, which is where the model puts the token
    /// on every one of these operations.
    fn uri(&self, cluster: &str, configuration: &str, token: &str) -> String {
        let route = self
            .route
            .replace("{cluster}", &pct(cluster))
            .replace("{configuration}", &pct(configuration))
            .replace("{topic}", &pct(TOPIC_NAME));
        format!("{route}?nextToken={}", pct(token))
    }
}

/// Every operation of this service that takes a `nextToken`, with the `http`
/// binding the model gives it (`smithy_query(query="digest")`).
fn listings() -> Vec<Listing> {
    vec![
        Listing {
            name: "ListClusters",
            route: "/v1/clusters",
        },
        Listing {
            name: "ListClustersV2",
            route: "/api/v2/clusters",
        },
        Listing {
            name: "ListConfigurations",
            route: "/v1/configurations",
        },
        Listing {
            name: "ListKafkaVersions",
            route: "/v1/kafka-versions",
        },
        Listing {
            name: "ListReplicators",
            route: "/replication/v1/replicators",
        },
        Listing {
            name: "ListVpcConnections",
            route: "/v1/vpc-connections",
        },
        Listing {
            name: "ListClusterOperations",
            route: "/v1/clusters/{cluster}/operations",
        },
        Listing {
            name: "ListClusterOperationsV2",
            route: "/api/v2/clusters/{cluster}/operations",
        },
        Listing {
            name: "ListNodes",
            route: "/v1/clusters/{cluster}/nodes",
        },
        Listing {
            name: "ListScramSecrets",
            route: "/v1/clusters/{cluster}/scram-secrets",
        },
        Listing {
            name: "ListTopics",
            route: "/v1/clusters/{cluster}/topics",
        },
        Listing {
            name: "ListChannels",
            route: "/v1/clusters/{cluster}/channels",
        },
        Listing {
            name: "ListClientVpcConnections",
            route: "/v1/clusters/{cluster}/client-vpc-connections",
        },
        Listing {
            name: "ListConfigurationRevisions",
            route: "/v1/configurations/{configuration}/revisions",
        },
        Listing {
            name: "DescribeTopicPartitions",
            route: "/v1/clusters/{cluster}/topics/{topic}/partitions",
        },
    ]
}

/// Every way a submitted token can fail to be one this service can read, with
/// `foreign` a token one of the OTHER listings really minted — the form a caller
/// reaches by paging one listing and resuming another.
fn unreadable_tokens(foreign: &str) -> Vec<(&'static str, String)> {
    vec![
        ("not-base64", "not-a-token".to_string()),
        (
            "base64-of-another-plaintext",
            ::aws_smithy_types::base64::encode("not-a-token"),
        ),
        ("a-long-opaque-value", "x".repeat(300)),
        ("a-non-ascii-value", "トークン-ではない".to_string()),
        ("another-listings-token", foreign.to_string()),
    ]
}

/// The token a listing minted for its own next page, or `None` when the listing
/// reported every entry it holds in one page.
fn minted_token(reply: &Reply, case: &str) -> Option<String> {
    assert_eq!(
        reply.status, 200,
        "case {case}: expected a first page to be served, got {} {}",
        reply.status, reply.body
    );
    reply.at("/nextToken").as_str().map(ToString::to_string)
}

/// Assert ONE refusal of an unreadable token: HTTP 400 `BadRequestException`,
/// the service's generic parameter sentence as the whole message, `nextToken` as
/// the parameter at fault, and nothing of the submitted token in the message.
fn assert_token_refused(reply: &Reply, case: &str, token: &str) {
    assert_eq!(
        (reply.status, reply.error_code()),
        (400, "BadRequestException"),
        "case {case}: expected 400 BadRequestException, got {} {}",
        reply.status,
        reply.body
    );
    assert_eq!(
        reply.error_message(),
        PARAMETERS_NOT_VALID,
        "case {case}: expected the fixed sentence {PARAMETERS_NOT_VALID:?}, got {:?}",
        reply.error_message()
    );
    assert_eq!(
        reply.at("/invalidParameter").as_str(),
        Some(NEXT_TOKEN_MEMBER),
        "case {case}: expected the refusal to name {NEXT_TOKEN_MEMBER}, got {}",
        reply.body
    );
    assert!(
        !reply.error_message().contains(token),
        "case {case}: the refusal must not echo the submitted token, got {:?}",
        reply.error_message()
    );
}

/// RULE: every paginated listing of this service refuses a `nextToken` it cannot
/// read with HTTP 400 `BadRequestException`, the service's generic parameter
/// sentence and `nextToken` as the parameter at fault — whichever listing
/// answers and whatever makes the token unreadable.
#[tokio::test]
async fn every_listing_refuses_a_resume_token_it_cannot_read() {
    let emulator = Emulator::new();

    // One settled cluster and two configurations, so every cluster- and
    // configuration-scoped route below addresses a resource this endpoint really
    // holds: the refusal under test must be the token's, not the reference's.
    let cluster = emulator
        .create_ok(&ClusterRequest::named("pagination-token"))
        .await;
    poll_until_active(&emulator, &cluster, "pagination-token").await;
    let configuration = emulator
        .create_configuration_ok("pagination-token-a", None, b"auto.create.topics.enable=true\n")
        .await;
    emulator
        .create_configuration_ok("pagination-token-b", None, b"auto.create.topics.enable=true\n")
        .await;

    // Two tokens this service really minted, each from a listing that holds more
    // entries than the page asked for. A listing is fed the OTHER one, so the row
    // is always a token minted by a different listing of the same service.
    let versions_token = minted_token(
        &emulator.call("GET", "/v1/kafka-versions?maxResults=1", None).await,
        "ListKafkaVersions/first-page",
    )
    .expect("the Kafka version catalog holds more than one version");
    let configurations_token = minted_token(
        &emulator.call("GET", "/v1/configurations?maxResults=1", None).await,
        "ListConfigurations/first-page",
    )
    .expect("two configurations do not fit in a page of one");

    for listing in listings() {
        let foreign = if listing.name == "ListKafkaVersions" {
            &configurations_token
        } else {
            &versions_token
        };
        for (form, token) in unreadable_tokens(foreign) {
            let case = format!("{}/{form}", listing.name);
            let reply = emulator
                .call("GET", &listing.uri(&cluster, &configuration, &token), None)
                .await;
            assert_token_refused(&reply, &case, &token);
        }
    }
}

/// RULE: a token a listing DID mint resumes that listing — the refusal above is
/// about a token this service cannot read, not about paging.
#[tokio::test]
async fn a_listing_resumes_the_token_it_minted() {
    let emulator = Emulator::new();
    emulator
        .create_configuration_ok("resumes-a", None, b"auto.create.topics.enable=true\n")
        .await;
    emulator
        .create_configuration_ok("resumes-b", None, b"auto.create.topics.enable=true\n")
        .await;

    // The rows differ along which listing minted the token: the region-wide
    // catalog reader and the configuration listing, each resuming its own page.
    for (case, route, items) in [
        (
            "ListKafkaVersions",
            "/v1/kafka-versions",
            "/kafkaVersions",
        ),
        ("ListConfigurations", "/v1/configurations", "/configurations"),
    ] {
        let first = emulator
            .call("GET", &format!("{route}?maxResults=1"), None)
            .await;
        let token = minted_token(&first, case)
            .unwrap_or_else(|| panic!("case {case}: a page of one left nothing to resume"));
        let resumed = emulator
            .call("GET", &format!("{route}?nextToken={}", pct(&token)), None)
            .await;
        assert_eq!(
            resumed.status, 200,
            "case {case}: expected the listing to resume its own token, got {} {}",
            resumed.status, resumed.body
        );
        assert!(
            resumed.at(items).as_array().is_some_and(|page| !page.is_empty()),
            "case {case}: expected the resumed page to report entries, got {}",
            resumed.body
        );
    }
}

/// RULE: the refusal of an unreadable token does not depend on the other
/// pagination inputs a listing takes — a page size the model allows is still
/// answered on the token.
#[tokio::test]
async fn the_token_is_refused_whatever_page_size_the_request_asks_for() {
    let emulator = Emulator::new();

    // The rows are the published bounds of `MaxResults` (`range` 1..100 on
    // `ListConfigurationsRequest$MaxResults`) and a size between them, plus a
    // request that names no page size at all.
    for size in [None, Some(1), Some(50), Some(100)] {
        let case = format!("maxResults={size:?}");
        let query = match size {
            Some(size) => format!("?maxResults={size}&nextToken=not-a-token"),
            None => "?nextToken=not-a-token".to_string(),
        };
        let reply = emulator
            .call("GET", &format!("/v1/configurations{query}"), None)
            .await;
        assert_token_refused(&reply, &case, "not-a-token");
    }
}
