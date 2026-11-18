// SPDX-License-Identifier: CC0-1.0

//! A declared value that COINCIDES with the stand-in of a resource whose identity its
//! owning service mints, end to end over the awsQuery wire with those services mounted
//! beside CloudFormation.
//!
//! **The rule.** A value a resource's own declaration states is that declaration's own, so
//! taking over the identifier an owning service minted for another resource may only change
//! the values a REFERENCE to that resource put there. Which values those are follows from
//! what the stand-in is: an identifier CloudFormation COMPUTED for a resource carries a
//! digest of the stack id and the logical id, so no declaration states one by accident and it
//! is taken over wherever it appears (including as one whole token of a longer string, which
//! is how a template composes an identifier into its own text); a stand-in that is the NAME
//! the declaration asked for is a value ANY declaration may state, so it is taken over only
//! in the declarations that reference that resource. A stack in which two resources state the
//! same text — a bucket named after the stack, in a stack whose user pool is created under
//! that same name — therefore deploys each resource with the value its own declaration
//! states, and reaches `CREATE_COMPLETE`.
//!
//! The rows of the table differ along what the rule generalises over: WHICH resource states
//! the shared text and in which OWNING SERVICE (an Amazon S3 bucket, an Amazon SNS topic, an
//! Amazon DynamoDB table, an AWS Systems Manager parameter — four services, four naming
//! members), what the type's `Ref` returns for it (the name itself, and an ARN built around
//! it), and WHERE the text sits (a resource's name property, a tag value, and a property
//! whose value a `Ref` to a resource named by the shared text put there). The same stack
//! carries the other half of the rule: a reference to a resource whose identity Amazon
//! Cognito mints resolves to the minted identifier, both as a whole value (an identity pool
//! fed the app client's id, and that pool's id inside a bucket policy's condition) and as one
//! token of a longer string (a user pool id composed into an object-prefix ARN).
//!
//! The identifiers Amazon Cognito mints are per-run, so what the cases assert is the FORM,
//! the round trip through the owning service and which value each declaration ended up with —
//! never a recorded value.

use serde_json::{json, Value};
use service_com_amazonaws_cloudformation_cloudformation::state::status;
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{
    call_in, create_stack, member, router, settled_stack_status, stack_event_reasons, xml_texts,
    REGION,
};
use crate::stack_resource_provisioning::s3;

/// The ONE text every resource of the stack states: the name the user pool is created under,
/// and the name a bucket, a topic, a table and a parameter of the same stack each ask for.
///
/// Inside every one of those services' naming rules at once — lowercase letters and hyphens
/// only, 3..63 characters — so each row's name is refused by nothing but the defect.
const SHARED_NAME: &str = "coinciding-name-of-the-whole-stack";

/// The emulator's account id, which the ARN an `AWS::SNS::Topic`'s `Ref` returns carries.
const ACCOUNT_ID: &str = "000000000000";

/// The stack the table deploys.
const STACK: &str = "coinciding-declared-names";

/// One resource that states [`SHARED_NAME`] as its own name, and what the stack must then
/// publish for it.
struct Row {
    case: &'static str,
    logical_id: &'static str,
    /// What the type's `Ref` returns once the resource is created: the name itself, or an ARN
    /// built around it (`AWS::SNS::Topic`: "Ref returns the topic ARN").
    published: fn() -> String,
}

/// The rows: one per owning service whose resource is named by the shared text.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "an Amazon S3 bucket named after the stack",
            logical_id: "Bucket",
            // "Ref returns the bucket name".
            published: || SHARED_NAME.to_string(),
        },
        Row {
            case: "an Amazon SNS topic named after the stack",
            logical_id: "Topic",
            // "Ref returns the topic ARN", which carries the name the topic was created
            // under.
            published: || format!("arn:aws:sns:{REGION}:{ACCOUNT_ID}:{SHARED_NAME}"),
        },
        Row {
            case: "an Amazon DynamoDB table named after the stack",
            logical_id: "Table",
            // "Ref returns the table name".
            published: || SHARED_NAME.to_string(),
        },
        Row {
            case: "an AWS Systems Manager parameter named after the stack",
            logical_id: "Parameter",
            // "Ref returns the parameter name".
            published: || SHARED_NAME.to_string(),
        },
    ]
}

/// The template the table deploys.
///
/// `Pool` is created under [`SHARED_NAME`] — so the identifier Amazon Cognito mints for it
/// replaces a stand-in that is exactly the text four other resources state as their own name
/// — while `UnnamedPool` states no name at all, so ITS stand-in is the identifier
/// CloudFormation computed and is taken over wherever it appears.
fn template() -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            // The resource whose identity its owning service mints, created under the shared
            // text: `UserPoolIdType` is `^[\w-]+_[0-9a-zA-Z]+$`, which no name here can be.
            "Pool": {
                "Type": "AWS::Cognito::UserPool",
                "Properties": {"UserPoolName": SHARED_NAME}
            },
            // A second pool the template does not name, whose stand-in is therefore the
            // identifier CloudFormation computed for it.
            "UnnamedPool": {"Type": "AWS::Cognito::UserPool", "Properties": {}},
            // An app client, whose own `ClientId` the same service mints.
            "Client": {
                "Type": "AWS::Cognito::UserPoolClient",
                "Properties": {"UserPoolId": {"Ref": "Pool"}, "GenerateSecret": false}
            },
            // The identity pool fed that client id: `CognitoIdentityProviderClientId` is
            // `^[\w_]+$`, so it is created only if the reference really carries the minted id.
            "Identities": {
                "Type": "AWS::Cognito::IdentityPool",
                "Properties": {
                    "IdentityPoolName": "coinciding_name_identities",
                    "AllowUnauthenticatedIdentities": false,
                    "CognitoIdentityProviders": [{
                        "ClientId": {"Ref": "Client"},
                        "ProviderName": {"Fn::GetAtt": ["Pool", "ProviderName"]}
                    }]
                }
            },
            "Bucket": {
                "Type": "AWS::S3::Bucket",
                "Properties": {
                    "BucketName": SHARED_NAME,
                    // A value that coincides with the pool's stand-in in a property that is
                    // not a name at all.
                    "Tags": [{"Key": "deployed-by", "Value": SHARED_NAME}]
                }
            },
            // The policy of that bucket: its `Bucket` is a reference to a resource whose
            // identity IS the shared text, and the document reads two resources whose
            // identity Amazon Cognito mints — one as a whole value, one as a token of a
            // longer string.
            "BucketPolicy": {
                "Type": "AWS::S3::BucketPolicy",
                "Properties": {
                    "Bucket": {"Ref": "Bucket"},
                    "PolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Sid": "DenyReadsFromAnotherIdentityPool",
                            "Effect": "Deny",
                            "Principal": {"AWS": "*"},
                            "Action": "s3:GetObject",
                            "Resource": {"Fn::Sub": "${Bucket.Arn}/${UnnamedPool}/*"},
                            "Condition": {"StringNotEquals": {
                                "cognito-identity.amazonaws.com:aud": {"Ref": "Identities"}
                            }}
                        }]
                    }
                }
            },
            "Topic": {
                "Type": "AWS::SNS::Topic",
                "Properties": {"TopicName": SHARED_NAME}
            },
            "Table": {
                "Type": "AWS::DynamoDB::Table",
                "Properties": {
                    "TableName": SHARED_NAME,
                    "BillingMode": "PAY_PER_REQUEST",
                    "AttributeDefinitions": [{"AttributeName": "id", "AttributeType": "S"}],
                    "KeySchema": [{"AttributeName": "id", "KeyType": "HASH"}]
                }
            },
            "Parameter": {
                "Type": "AWS::SSM::Parameter",
                "Properties": {
                    "Name": SHARED_NAME,
                    "Type": "String",
                    "Value": "a value of the parameter's own"
                }
            }
        },
        "Outputs": {
            "Bucket": {"Value": {"Ref": "Bucket"}},
            "Topic": {"Value": {"Ref": "Topic"}},
            "Table": {"Value": {"Ref": "Table"}},
            "Parameter": {"Value": {"Ref": "Parameter"}},
            "Pool": {"Value": {"Ref": "Pool"}},
            "UnnamedPool": {"Value": {"Ref": "UnnamedPool"}},
            "Client": {"Value": {"Ref": "Client"}},
            "Identities": {"Value": {"Ref": "Identities"}}
        }
    })
    .to_string()
}

/// Every output the settled stack publishes, keyed by `OutputKey`.
async fn outputs(case: &str, router: &MultiServiceRouter, stack: &str) -> Vec<(String, String)> {
    let (code, body) =
        call_in(router, REGION, "DescribeStacks", &[member("StackName", stack)]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: DescribeStacks must answer 200, got {code} with {body}"
    );
    xml_texts(&body, "OutputKey")
        .into_iter()
        .zip(xml_texts(&body, "OutputValue"))
        .collect()
}

/// The value published under `key`, or the empty string where the stack publishes none.
fn published(outputs: &[(String, String)], key: &str) -> String {
    outputs
        .iter()
        .find(|(output_key, _)| output_key == key)
        .map(|(_, value)| value.clone())
        .unwrap_or_default()
}

/// Whether `candidate` is a user pool id of the form the owning service mints:
/// `<region>_<id>`, the modelled `UserPoolIdType` `^[\w-]+_[0-9a-zA-Z]+$`
/// (`smithy_query(query="shape",shape="UserPoolIdType")`).
fn is_a_user_pool_id(candidate: &str) -> bool {
    candidate.split_once('_').is_some_and(|(region, id)| {
        region == REGION && !id.is_empty() && id.chars().all(char::is_alphanumeric)
    })
}

/// Whether `candidate` is an identity pool id of the form the owning service mints:
/// `<region>:<guid>`, the modelled `IdentityPoolId` `^[\w-]+:[0-9a-f-]+$`
/// (`smithy_query(query="shape",shape="IdentityPoolId")`).
fn is_an_identity_pool_id(candidate: &str) -> bool {
    candidate.split_once(':').is_some_and(|(region, guid)| {
        region == REGION
            && !guid.is_empty()
            && guid
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
    })
}

/// Whether `candidate` is a value `ClientIdType` admits: 1..128 characters over `^[\w+]+$`
/// (`smithy_query(query="shape",shape="ClientIdType")`).
fn is_a_client_id(candidate: &str) -> bool {
    !candidate.is_empty()
        && candidate.chars().count() <= 128
        && candidate
            .chars()
            .all(|character| character.is_alphanumeric() || character == '_' || character == '+')
}

/// THE RULE: see the module header. Each resource is created with the value its own
/// declaration states, and a reference to a resource whose identity its owning service mints
/// carries the minted identifier.
#[tokio::test]
async fn a_declared_value_that_coincides_with_a_minted_identifier_is_kept() {
    let router = router();
    let body = template();
    let (code, answered) = create_stack(&router, STACK, &body, &[]).await;
    assert_eq!(
        code, 200,
        "the create must be accepted, got {code} with {answered}"
    );
    let settled = settled_stack_status("the whole estate", &router, REGION, STACK).await;
    assert_eq!(
        settled,
        status::CREATE_COMPLETE,
        "expected the estate to complete, got {settled:?}; the stack reports {:?}",
        stack_event_reasons(&router, REGION, STACK).await
    );

    let outputs = outputs("the whole estate", &router, STACK).await;
    // Each resource states its own name, whatever the pool of the same stack was created
    // under.
    for row in rows() {
        let case = row.case;
        let expected = (row.published)();
        let actual = published(&outputs, row.logical_id);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected the stack to publish {expected:?} for \
             {:?}, got {actual:?}",
            row.logical_id
        );
    }

    // ...and the pool itself publishes the identifier Amazon Cognito minted, which is what
    // makes the coincidence a coincidence at all.
    let pool = published(&outputs, "Pool");
    assert!(
        is_a_user_pool_id(&pool) && pool != SHARED_NAME,
        "expected the pool to publish the <region>_<id> the owning service minted, got \
         {pool:?}"
    );
    let unnamed_pool = published(&outputs, "UnnamedPool");
    assert!(
        is_a_user_pool_id(&unnamed_pool) && unnamed_pool != pool,
        "expected the unnamed pool to publish a minted id of its own, got {unnamed_pool:?}"
    );
    // The other half of the rule: a reference to such a resource DOES carry the minted
    // identifier — the app client's id, which the identity pool fed with it could not have
    // been created without.
    let client = published(&outputs, "Client");
    assert!(
        is_a_client_id(&client),
        "expected the app client to publish the ClientId the owning service minted, got \
         {client:?}"
    );
    let identities = published(&outputs, "Identities");
    assert!(
        is_an_identity_pool_id(&identities),
        "expected the identity pool to publish the <region>:<guid> it was minted, got \
         {identities:?}"
    );

    // The bucket named by the shared text really exists in Amazon S3, carrying the tag value
    // that coincides with the pool's stand-in...
    let tagging = s3(&router, "GET", &format!("/{SHARED_NAME}?tagging"), b"").await;
    assert_eq!(
        tagging.status, 200,
        "expected the bucket named {SHARED_NAME:?} to answer GetBucketTagging, got {} with {}",
        tagging.status, tagging.body
    );
    let tag_values = xml_texts(&tagging.body, "Value");
    assert!(
        tag_values.iter().any(|value| value == SHARED_NAME),
        "expected the declared tag value {SHARED_NAME:?} in force on the bucket, got \
         {tag_values:?}"
    );

    // ...and its POLICY is the declared document with each reference resolved to the
    // identifier in force: the bucket's own ARN, the computed stand-in of the unnamed pool
    // taken over as one token of a longer string, and the identity pool's minted id as a
    // whole value.
    let answered = s3(&router, "GET", &format!("/{SHARED_NAME}?policy"), b"").await;
    assert_eq!(
        answered.status, 200,
        "expected the declared policy in force on the bucket, got {} with {}",
        answered.status, answered.body
    );
    let in_force: Value = serde_json::from_str(&answered.body).unwrap_or_else(|error| {
        panic!(
            "GetBucketPolicy must answer a JSON document, got {} ({error})",
            answered.body
        )
    });
    let statement = &in_force["Statement"][0];
    assert_eq!(
        statement["Resource"],
        json!(format!("arn:aws:s3:::{SHARED_NAME}/{unnamed_pool}/*")),
        "expected the statement to name the bucket's own objects under the minted pool id, \
         got {in_force}"
    );
    assert_eq!(
        statement["Condition"]["StringNotEquals"]["cognito-identity.amazonaws.com:aud"],
        json!(identities),
        "expected the condition to carry the minted identity pool id {identities:?}, got \
         {in_force}"
    );
}
