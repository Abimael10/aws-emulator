// SPDX-License-Identifier: CC0-1.0

//! A single `GetParameter` read of an AWS-published parameter, in EVERY Region the endpoint
//! serves.
//!
//! RULE under test: AWS publishes its own parameters in the `/aws` hierarchy of every
//! commercial Region alike, so `GetParameter` ANSWERS a name that hierarchy holds — HTTP 200
//! carrying `Parameter`, with `Name` echoing the request, `Type` `String`, `DataType` `text`,
//! a server-assigned `Version`, a `LastModifiedDate` and the ARN of a parameter that belongs
//! to no account, `arn:aws:ssm:<region>::parameter<name>`, whose Region is the one the call
//! was signed for — and reserves the modelled `ParameterNotFound` for a name neither that
//! hierarchy nor the account's store holds. The Region a read resolves in comes from the
//! signed credential scope, so no Region is a special case: the one a recording happens to
//! have been made in resolves exactly as every other does.
//!
//! Every case goes through the wire a client uses: `POST /` with
//! `X-Amz-Target: AmazonSSM.GetParameter` and an `awsJson1_1` body (`OPERATIONS.md`, "Wire
//! protocol"), signed for the Region of the row. Nothing reaches into a state struct.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::parameter_store::{call_in_region, error_code};
use super::public_parameter_catalog::{emulator, is_ami_id, names_an_image};

/// The Regions the rows are read in. Commercial Region codes spanning the geographies an
/// endpoint is addressed by, including the one the Amazon Linux lookup idiom is most often
/// recorded in (`us-west-2`) — a published parameter resolves in each of them, so the table
/// spans the dimension the rule generalises over.
const REGIONS: &[&str] = &[
    "us-west-2",
    "us-east-1",
    "eu-central-1",
    "ap-northeast-1",
    "sa-east-1",
];

/// The published names the rows read, one per namespace and name form AWS publishes:
/// the Amazon Linux AMI aliases and the Windows AMI aliases (Amazon EC2 User Guide, "Use a
/// Systems Manager parameter instead of an AMI ID"), the `image_id` leaf of a service's
/// recommended-image hierarchy (Amazon ECS Developer Guide, "Retrieve Amazon ECS-optimized
/// AMI metadata"), a non-image leaf of that same hierarchy, and the Bottlerocket build
/// aliases that project publishes. WHICH builds a family publishes is service-published data
/// no model describes, so these are names of the catalog this emulator publishes.
const PUBLISHED_NAMES: &[&str] = &[
    "/aws/service/ami-amazon-linux-latest/al2023-ami-kernel-default-x86_64",
    "/aws/service/ami-amazon-linux-latest/al2023-ami-kernel-default-arm64",
    "/aws/service/ami-amazon-linux-latest/al2023-ami-minimal-kernel-default-x86_64",
    "/aws/service/ami-amazon-linux-latest/amzn2-ami-kernel-default-hvm-x86_64-gp2",
    "/aws/service/ami-windows-latest/Windows_Server-2022-English-Full-Base",
    "/aws/service/ecs/optimized-ami/amazon-linux-2023/recommended/image_id",
    "/aws/service/ecs/optimized-ami/amazon-linux-2023/recommended/os",
    "/aws/service/bottlerocket/aws-ecs-2/arm64/latest/image_id",
];

/// Names in AWS's own hierarchy that AWS publishes nothing under: the one refusal this read
/// makes for a name in it. Each is a leaf of a namespace the catalog does publish, so the
/// miss is the NAME missing rather than the namespace.
const UNPUBLISHED_NAMES: &[&str] = &[
    "/aws/service/ami-amazon-linux-latest/al2023-ami-kernel-default-sparc64",
    "/aws/service/ami-windows-latest/Windows_Server-1995-English-Full-Base",
    "/aws/service/ecs/optimized-ami/amazon-linux-2023/recommended/no_such_leaf",
];

/// The `ParameterType` a published parameter carries, from the modelled enum
/// (`smithy_query(query="shape", shape="ParameterType")`).
const PUBLISHED_TYPE: &str = "String";

/// The `ParameterDataType` it carries (`smithy_query(query="doc",
/// shape="PutParameterRequest", member="DataType")`: "text aws:ec2:image
/// aws:ssm:integration").
const PUBLISHED_DATA_TYPE: &str = "text";

/// One `GetParameter` call for `name`, signed for `region`.
async fn read(router: &MultiServiceRouter, region: &str, name: &str) -> (u16, Value) {
    call_in_region(router, region, "GetParameter", &json!({ "Name": name })).await
}

/// RULE: a published name resolves in every Region, reported as the `String`/`text`
/// parameter AWS publishes, with the account-less ARN of the Region the call was signed for.
#[tokio::test]
async fn a_published_name_resolves_in_every_region() {
    let router = emulator();

    for region in REGIONS {
        for name in PUBLISHED_NAMES {
            let case = format!("{name} read in {region}");
            let (status, body) = read(&router, region, name).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200 from GetParameter, got {status} with {body}"
            );
            let parameter = &body["Parameter"];
            assert!(
                parameter.is_object(),
                "case {case:?}: expected the response to carry Parameter, got {body}"
            );
            assert_eq!(
                parameter["Name"].as_str(),
                Some(*name),
                "case {case:?}: expected Name to echo the request, got {parameter}"
            );
            assert_eq!(
                parameter["Type"].as_str(),
                Some(PUBLISHED_TYPE),
                "case {case:?}: expected Type {PUBLISHED_TYPE}, got {parameter}"
            );
            assert_eq!(
                parameter["DataType"].as_str(),
                Some(PUBLISHED_DATA_TYPE),
                "case {case:?}: expected DataType {PUBLISHED_DATA_TYPE}, got {parameter}"
            );
            assert!(
                parameter["Version"].as_i64().unwrap_or_default() >= 1,
                "case {case:?}: expected a server-assigned version of at least 1, got \
                 {parameter}"
            );
            assert!(
                !parameter["LastModifiedDate"].is_null(),
                "case {case:?}: expected a last-modified instant, got {parameter}"
            );
            assert_eq!(
                parameter["ARN"].as_str(),
                Some(format!("arn:aws:ssm:{region}::parameter{name}").as_str()),
                "case {case:?}: expected the account-less published-parameter ARN of \
                 {region}, got {parameter}"
            );
            let value = parameter["Value"].as_str().unwrap_or_default();
            assert!(
                !value.is_empty(),
                "case {case:?}: expected the published parameter to carry a value, got \
                 {parameter}"
            );
            if names_an_image(name) {
                assert!(
                    is_ami_id(value),
                    "case {case:?}: expected a name that names an image to resolve to an AMI \
                     id, got {value:?}"
                );
            }
        }
    }
}

/// RULE: the refusal is reserved for a name nothing holds — a name in AWS's own hierarchy
/// that AWS publishes nothing under is the modelled `ParameterNotFound` (HTTP 400), in every
/// Region alike.
#[tokio::test]
async fn a_name_nothing_publishes_is_still_the_modelled_miss() {
    let router = emulator();

    for region in REGIONS {
        for name in UNPUBLISHED_NAMES {
            let case = format!("{name} read in {region}");
            let (status, body) = read(&router, region, name).await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected HTTP 400 for a name nothing publishes, got {status} \
                 with {body}"
            );
            assert_eq!(
                error_code(&body),
                "ParameterNotFound",
                "case {case:?}: expected the modelled miss, got {body}"
            );
            assert!(
                body["Parameter"].is_null(),
                "case {case:?}: expected a refusal to carry no Parameter, got {body}"
            );
        }
    }
}

/// RULE: reading a published parameter changes nothing a later caller sees, in its own
/// Region or in any other — the catalog is AWS's published data, not state a read advances.
#[tokio::test]
async fn reading_a_published_name_changes_nothing() {
    let router = emulator();
    let name = PUBLISHED_NAMES[0];

    let mut first_pass: Vec<Value> = Vec::new();
    for region in REGIONS {
        let (status, body) = read(&router, region, name).await;
        assert_eq!(
            status, 200,
            "case {name:?} in {region}: expected HTTP 200, got {status} with {body}"
        );
        first_pass.push(body["Parameter"].clone());
    }

    for (region, before) in REGIONS.iter().zip(&first_pass) {
        let (status, body) = read(&router, region, name).await;
        assert_eq!(
            status, 200,
            "case {name:?} re-read in {region}: expected HTTP 200, got {status} with {body}"
        );
        assert_eq!(
            &body["Parameter"], before,
            "case {name:?} re-read in {region}: expected the published parameter to be \
             unchanged, got {body}"
        );
    }
}
