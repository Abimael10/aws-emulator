// SPDX-License-Identifier: CC0-1.0

//! What CLIENT AUTHENTICATION `CreateClusterV2` admits a SERVERLESS cluster with.
//!
//! The rule under test: a serverless cluster's clients are admitted through SASL/IAM and
//! through nothing else, so a `Serverless` body that does not turn that scheme ON describes a
//! cluster nothing could connect to and is refused HTTP 400 `BadRequestException` with MSK's
//! one fixed sentence for it, attributed to the `clientAuthentication` parameter, registering
//! no cluster. The model makes every level of that structure optional
//! (`ServerlessRequest$ClientAuthentication` -> `ServerlessClientAuthentication$Sasl` ->
//! `ServerlessSasl$Iam` -> `Iam$Enabled`, `smithy_query(query="shape",
//! shape="ServerlessRequest,ServerlessClientAuthentication,ServerlessSasl,Iam")`), so the
//! requirement is recorded behaviour — and a level the request left out states no more about
//! IAM being on than an explicit `false` does, which is what the rows below span.
//!
//! The rows differ along WHERE the body stops short of enabling the scheme: no
//! `ClientAuthentication` at all, one naming no `Sasl`, a `Sasl` naming no `Iam`, an `Iam`
//! stating no `Enabled`, and an `Iam` stating `false` — beside the admitted body, which
//! states `true` and is created. Each refused row is checked over a second shape of the rest
//! of the body too (one VPC configuration against two), so what is pinned is the
//! authentication rather than one request.

use serde_json::{json, Value};

use crate::kafka_harness::{Emulator, Reply};

/// What MSK answers a serverless create that does not turn SASL/IAM on, written out here
/// rather than read from the service's own constant, so the wording is pinned by this suite.
const SASL_IAM_REQUIRED_MESSAGE: &str = "A serverless cluster must use SASL/IAM authentication.";

/// The parameter the refusal is attributed to: the whole authentication structure, which is
/// what the caller has to supply, as the API spells it on the wire.
const CLIENT_AUTHENTICATION_MEMBER: &str = "clientAuthentication";

/// A `serverless` block over `configurations` VPC configurations of one subnet each, carrying
/// `authentication` as its `clientAuthentication` where the row states one.
fn serverless(configurations: usize, authentication: Option<Value>) -> Value {
    let mut body = json!({
        "vpcConfigs": (0..configurations)
            .map(|index| json!({ "subnetIds": [format!("subnet-{index:017x}")] }))
            .collect::<Vec<Value>>(),
    });
    if let Some(authentication) = authentication {
        body["clientAuthentication"] = authentication;
    }
    body
}

/// The refusal as a comparable tuple: status, error code, message and the member blamed.
fn answered(reply: &Reply) -> (u16, String, String, Option<String>) {
    (
        reply.status,
        reply.error_code().to_string(),
        reply.error_message().to_string(),
        reply.at("/invalidParameter").as_str().map(str::to_string),
    )
}

/// The client authentications that do NOT turn SASL/IAM on, with the case each one is.
fn unauthenticated_rows() -> Vec<(&'static str, Option<Value>)> {
    vec![
        ("no ClientAuthentication at all", None),
        ("a ClientAuthentication naming no Sasl", Some(json!({}))),
        ("a Sasl naming no Iam", Some(json!({ "sasl": {} }))),
        (
            "an Iam stating no Enabled",
            Some(json!({ "sasl": { "iam": {} } })),
        ),
        (
            "an Iam stating Enabled false",
            Some(json!({ "sasl": { "iam": { "enabled": false } } })),
        ),
    ]
}

/// RULE: a serverless create that does not turn SASL/IAM on reads ONE refusal — 400
/// `BadRequestException`, MSK's fixed sentence, `clientAuthentication` blamed — invariant
/// over how the body stops short of enabling it and over the rest of the body, and registers
/// nothing.
#[tokio::test]
async fn a_serverless_create_without_sasl_iam_is_refused() {
    let emulator = Emulator::new();
    let expected = (
        400u16,
        "BadRequestException".to_string(),
        SASL_IAM_REQUIRED_MESSAGE.to_string(),
        Some(CLIENT_AUTHENTICATION_MEMBER.to_string()),
    );

    for (case, authentication) in unauthenticated_rows() {
        for configurations in [1usize, 2] {
            let name = format!("sl-auth-{configurations}-{}", case.len());
            let reply = emulator
                .create_cluster_v2(json!({
                    "clusterName": name,
                    "serverless": serverless(configurations, authentication.clone()),
                }))
                .await;
            assert_eq!(
                answered(&reply),
                expected,
                "case {case} over {configurations} VPC configuration(s): expected the \
                 authentication refusal, got {} {}",
                reply.status,
                reply.body
            );
        }
    }

    let listed = emulator
        .list_v2("")
        .await
        .at("/clusterInfoList")
        .as_array()
        .map(Vec::len)
        .unwrap_or_default();
    assert_eq!(
        listed, 0,
        "a refused create must register no cluster, got {listed} of them"
    );
}

/// RULE, the admitted side: a body that turns SASL/IAM ON is created, and every reader reports
/// the scheme on — so the refusal above is about the authentication and not about the
/// serverless body.
#[tokio::test]
async fn a_serverless_create_with_sasl_iam_is_served() {
    let emulator = Emulator::new();

    for configurations in [1usize, 2] {
        let name = format!("sl-auth-on-{configurations}");
        let reply = emulator
            .create_cluster_v2(json!({
                "clusterName": name,
                "serverless": serverless(
                    configurations,
                    Some(json!({ "sasl": { "iam": { "enabled": true } } })),
                ),
            }))
            .await;
        assert_eq!(
            reply.status, 200,
            "case {configurations} VPC configuration(s) with SASL/IAM on: expected the create \
             to be served, got {} {}",
            reply.status, reply.body
        );
        let arn = reply
            .at("/clusterArn")
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {configurations} VPC configuration(s): no clusterArn in {}", reply.body)
            })
            .to_string();
        let described = emulator.describe_v2(&arn).await;
        assert_eq!(
            described.status, 200,
            "case {configurations} VPC configuration(s): expected DescribeClusterV2 to serve \
             the minted ARN, got {} {}",
            described.status, described.body
        );
        assert_eq!(
            described
                .at("/clusterInfo/serverless/clientAuthentication/sasl/iam/enabled")
                .as_bool(),
            Some(true),
            "case {configurations} VPC configuration(s): expected SASL/IAM reported on, got {}",
            described.body
        );
    }
}
