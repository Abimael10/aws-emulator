// SPDX-License-Identifier: CC0-1.0

//! The refusal a create is answered with when the NAME it asks for is already held, end to
//! end over HTTP.
//!
//! The wire harness is the sibling suite's ([`crate::kinesis_video_stream_pool_lifecycle`]):
//! one `MultiServiceRouter` with this service mounted on one shared engine, so every
//! assertion here is about the emulator's HTTP behaviour rather than an internal helper.
//!
//! The RULE pinned here: a create whose name is already held by a live resource of the same
//! kind is refused as the modelled 400 `BadRequestException` — never a 409 — with a message
//! that is fixed text around the name the request carried, so a caller can read out WHICH
//! name collided; the resource that holds the name is left exactly as it was, and the same
//! create succeeds under a free name and once the holder is deleted.
//!
//! The table spans what the rule generalises over rather than one row:
//!
//! * the resource KIND, i.e. every name-keyed create this service implements — Kinesis
//!   Video Stream pools and media insights pipeline configurations;
//! * the NAME's form and length across the modelled pattern `^[0-9a-zA-Z._-]+$` and both
//!   of each member's `@length` bounds (`KinesisVideoStreamPoolName` 1..128,
//!   `MediaInsightsPipelineConfigurationNameString` 2..64, from
//!   `smithy_query(query="operation", operation="CreateMediaPipelineKinesisVideoStreamPool,
//!   CreateMediaInsightsPipelineConfiguration")`);
//! * whether the colliding create differs from the served one in its payload or carries a
//!   `ClientRequestToken` of its own.

use serde_json::{json, Value};

use crate::kinesis_video_stream_pool_lifecycle::{
    encode_label, pool_name_in_use_message, Emulator, Response, REGION,
};

/// The collection routes the name-keyed creates hang off, from
/// `smithy_query(query="digest")`.
const POOLS: &str = "/media-pipeline-kinesis-video-stream-pools";
const CONFIGURATIONS: &str = "/media-insights-pipeline-configurations";

/// The wire error-shape name restJson1 resolves from `x-amzn-errortype`.
const BAD_REQUEST_EXCEPTION: &str = "BadRequestException";

/// The `ErrorCode` value the modelled `BadRequestException` carries in its body `Code`
/// member. Source: `smithy_query(query="shape",shape="ErrorCode")` — `BadRequest |
/// Forbidden | NotFound | ResourceLimitExceeded | ServiceFailure | ServiceUnavailable |
/// Throttling`.
const ERROR_CODE_BAD_REQUEST: &str = "BadRequest";

/// A `ResourceAccessRoleArn` satisfying the modelled `Arn` shape (len 1..1024, pattern
/// `^arn[\/\:\-\_\.a-zA-Z0-9]+$`).
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/media-insights-access";

/// A name-keyed create of this service: the kind whose name must be unique.
#[derive(Debug, Clone, Copy)]
enum Kind {
    /// `CreateMediaPipelineKinesisVideoStreamPool`, keyed by `PoolName`.
    StreamPool,
    /// `CreateMediaInsightsPipelineConfiguration`, keyed by
    /// `MediaInsightsPipelineConfigurationName`.
    InsightsConfiguration,
}

impl Kind {
    /// Both kinds, so a row drives every name-keyed create this service implements.
    const ALL: [Kind; 2] = [Kind::StreamPool, Kind::InsightsConfiguration];

    fn operation(self) -> &'static str {
        match self {
            Kind::StreamPool => "CreateMediaPipelineKinesisVideoStreamPool",
            Kind::InsightsConfiguration => "CreateMediaInsightsPipelineConfiguration",
        }
    }

    /// The noun this service names the kind with in its name-in-use refusal.
    fn noun(self) -> &'static str {
        match self {
            Kind::StreamPool => "Media Pipeline Kinesis Video Stream Pool",
            Kind::InsightsConfiguration => "Media Insights Pipeline Configuration",
        }
    }

    /// The message this service refuses a name already in use with: fixed text around the
    /// name the request carried. Written out HERE rather than taken from the service's own
    /// formatter, so any change to that wording fails this suite. Recorded behaviour for
    /// the stream pool; the configuration takes the same family template with its own
    /// kind noun.
    fn expected_message(self, name: &str) -> String {
        match self {
            Kind::StreamPool => pool_name_in_use_message(name),
            Kind::InsightsConfiguration => {
                format!("Media Insights Pipeline Configuration already exists with {name} name")
            }
        }
    }

    fn collection(self) -> &'static str {
        match self {
            Kind::StreamPool => POOLS,
            Kind::InsightsConfiguration => CONFIGURATIONS,
        }
    }

    /// The output member a create and a read render the resource under.
    fn member(self) -> &'static str {
        match self {
            Kind::StreamPool => "/KinesisVideoStreamPoolConfiguration",
            Kind::InsightsConfiguration => "/MediaInsightsPipelineConfiguration",
        }
    }

    /// The listing member, and the summary member each entry carries the name under.
    fn listing(self) -> (&'static str, &'static str) {
        match self {
            Kind::StreamPool => ("/KinesisVideoStreamPools", "PoolName"),
            Kind::InsightsConfiguration => (
                "/MediaInsightsPipelineConfigurations",
                "MediaInsightsPipelineConfigurationName",
            ),
        }
    }

    /// A create body for `name`. `variant` only varies the parts of the request that are
    /// NOT the name, so a refusal is decided by the name and nothing else.
    fn create_body(self, name: &str, variant: usize) -> Value {
        match self {
            Kind::StreamPool => json!({
                "PoolName": name,
                "StreamConfiguration": {
                    "Region": REGION,
                    "DataRetentionInHours": variant as i64,
                },
            }),
            Kind::InsightsConfiguration => json!({
                "MediaInsightsPipelineConfigurationName": name,
                "ResourceAccessRoleArn": ROLE_ARN,
                "Elements": [{
                    "Type": "AmazonTranscribeProcessor",
                    "AmazonTranscribeProcessorConfiguration": {
                        "LanguageCode": if variant.is_multiple_of(2) { "en-US" } else { "fr-FR" },
                    },
                }],
            }),
        }
    }

    /// The names this kind's rows are driven with: the classes the shared pattern admits,
    /// and this member's own `@length` bounds.
    fn names(self) -> Vec<(&'static str, String)> {
        let mut rows: Vec<(&'static str, String)> = vec![
            ("a name of the shortest length both kinds allow", "ab".to_string()),
            ("a name carrying every class the pattern allows", "Name.1_x-2".to_string()),
            ("a purely numeric name", "1234567890".to_string()),
        ];
        match self {
            Kind::StreamPool => {
                rows.push((
                    "the shortest name KinesisVideoStreamPoolName allows",
                    "p".to_string(),
                ));
                rows.push((
                    "the longest name KinesisVideoStreamPoolName allows",
                    "p".repeat(128),
                ));
            }
            Kind::InsightsConfiguration => {
                rows.push((
                    "the longest name MediaInsightsPipelineConfigurationNameString allows",
                    "c".repeat(64),
                ));
            }
        }
        rows
    }
}

/// Create one resource of `kind` named `name`, with the `variant` payload.
async fn create(emulator: &Emulator, kind: Kind, name: &str, variant: usize) -> Response {
    emulator
        .call(
            "POST",
            kind.collection(),
            Some(kind.create_body(name, variant)),
        )
        .await
}

/// Read one resource of `kind` back by its name.
async fn read(emulator: &Emulator, kind: Kind, name: &str) -> Response {
    emulator
        .call(
            "GET",
            &format!("{}/{}", kind.collection(), encode_label(name)),
            None,
        )
        .await
}

/// How many entries the listing of `kind` reports under `name`.
async fn listed_under(emulator: &Emulator, kind: Kind, name: &str) -> usize {
    let (list_member, name_member) = kind.listing();
    let listed = emulator.call("GET", kind.collection(), None).await;
    assert_eq!(
        listed.status, 200,
        "{}: the listing must answer 200, got {} body {:?}",
        kind.operation(),
        listed.status,
        listed.body
    );
    listed
        .body
        .pointer(list_member)
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter(|entry| entry[name_member].as_str() == Some(name))
                .count()
        })
        .unwrap_or_default()
}

/// Assert `response` is this service's name-in-use refusal for `name` of `kind`.
fn assert_name_in_use(case: &str, kind: Kind, name: &str, response: &Response) {
    assert_eq!(
        response.status, 400,
        "case {case:?}: expected the modelled 400, got {} body {:?}",
        response.status, response.body
    );
    assert_eq!(
        response.error_code(),
        BAD_REQUEST_EXCEPTION,
        "case {case:?}: expected the modelled error shape, got {:?}",
        response.body
    );
    assert_eq!(
        response.str_at("/Code"),
        ERROR_CODE_BAD_REQUEST,
        "case {case:?}: expected the modelled ErrorCode, got {:?}",
        response.body
    );
    let message = response.str_at("/Message");
    assert_eq!(
        message,
        kind.expected_message(name),
        "case {case:?}: expected the name-in-use wording, got {message:?}"
    );
    assert!(
        message.starts_with(kind.noun()),
        "case {case:?}: the refusal must name the resource kind {:?}, got {message:?}",
        kind.noun()
    );
    assert!(
        message.contains(name),
        "case {case:?}: the refusal must name the name that is in use, got {message:?}"
    );
    assert!(
        !response.str_at("/RequestId").is_empty(),
        "case {case:?}: every error of this service carries a RequestId: {:?}",
        response.body
    );
}

/// RULE: a create whose name a live resource of the same kind already holds is the modelled
/// 400 `BadRequestException` naming that name, for every name-keyed create of this service
/// and every name form the model admits; the holder is untouched, and the same create is
/// served under a free name and again once the holder is deleted.
#[tokio::test]
async fn a_name_already_in_use_is_the_modelled_bad_request_naming_that_name() {
    for kind in Kind::ALL {
        for (name_case, name) in kind.names() {
            let case = format!("{} / {name_case}", kind.operation());
            let emulator = Emulator::new();

            let served = create(&emulator, kind, &name, 2).await;
            assert_eq!(
                served.status, 201,
                "case {case:?}: the first create must be served, got {} body {:?}",
                served.status, served.body
            );
            let held = served.body.pointer(kind.member()).cloned();

            // A second create of the same name, differing in everything the request
            // carries BESIDES the name.
            let refused = create(&emulator, kind, &name, 4).await;
            assert_name_in_use(&case, kind, &name, &refused);

            // The refused create wrote nothing: the holder still renders as it did, and it
            // is still the only resource under that name.
            let read_back = read(&emulator, kind, &name).await;
            assert_eq!(
                read_back.body.pointer(kind.member()).cloned(),
                held,
                "case {case:?}: a refused create must not change the resource holding the name"
            );
            assert_eq!(
                listed_under(&emulator, kind, &name).await,
                1,
                "case {case:?}: exactly one resource may exist under the name"
            );

            // A create under a FREE name is served, so the refusal is about the name alone.
            let free = format!("free-{}", name.len());
            let other = create(&emulator, kind, &free, 4).await;
            assert_eq!(
                other.status, 201,
                "case {case:?}: a create under a free name must be served, got {} body {:?}",
                other.status, other.body
            );

            // And the name is free again once its holder is deleted.
            let deleted = emulator
                .call(
                    "DELETE",
                    &format!("{}/{}", kind.collection(), encode_label(&name)),
                    None,
                )
                .await;
            assert_eq!(
                deleted.status, 204,
                "case {case:?}: the holder must delete, got {} body {:?}",
                deleted.status, deleted.body
            );
            let retaken = create(&emulator, kind, &name, 4).await;
            assert_eq!(
                retaken.status, 201,
                "case {case:?}: a freed name must be usable again, got {} body {:?}",
                retaken.status, retaken.body
            );
        }
    }
}

/// RULE (the same refusal, reached with the create's own idempotency token): a
/// `ClientRequestToken` the service has not served before does not make a name in use
/// available — the refusal is the same modelled 400 naming the name — while the token the
/// first create carried answers with the resource that create minted.
#[tokio::test]
async fn a_fresh_client_request_token_under_a_live_name_is_the_same_refusal() {
    for kind in Kind::ALL {
        let name = "token-held-name";
        let case = format!("{} / a fresh token under a live name", kind.operation());
        let emulator = Emulator::new();

        let mut first_body = kind.create_body(name, 2);
        first_body["ClientRequestToken"] = json!("token-one");
        let served = emulator
            .call("POST", kind.collection(), Some(first_body.clone()))
            .await;
        assert_eq!(
            served.status, 201,
            "case {case:?}: the first create must be served, got {} body {:?}",
            served.status, served.body
        );

        // The SAME token: the resource the first create minted, not a refusal.
        let replay = emulator
            .call("POST", kind.collection(), Some(first_body))
            .await;
        assert_eq!(
            replay.status, 201,
            "case {case:?}: a replayed token must be served, got {} body {:?}",
            replay.status, replay.body
        );
        assert_eq!(
            replay.body.pointer(kind.member()),
            served.body.pointer(kind.member()),
            "case {case:?}: a replayed token must answer with the resource the first create minted"
        );

        // A DIFFERENT token under the same name is a create of a name in use.
        let mut other_body = kind.create_body(name, 4);
        other_body["ClientRequestToken"] = json!("token-two");
        let refused = emulator
            .call("POST", kind.collection(), Some(other_body))
            .await;
        assert_name_in_use(&case, kind, name, &refused);
        assert_eq!(
            listed_under(&emulator, kind, name).await,
            1,
            "case {case:?}: exactly one resource may exist under the name"
        );
    }
}
