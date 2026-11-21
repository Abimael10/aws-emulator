// SPDX-License-Identifier: CC0-1.0

//! What `CreateVpcOrigin` REFUSES, end to end over HTTP.
//!
//! Every assertion goes through the router, so each case exercises routing, restXml
//! (de)serialization, the handler, the persisted state and serialization — the same path a
//! client takes.
//!
//! The rules under test:
//!
//! 1. `VpcOriginEndpointConfig$Name` is the one value a caller CHOOSES about a VPC origin, so
//!    the account holds at most one VPC origin under a name: a create naming a name already
//!    held is refused `EntityAlreadyExists` (409) with the fixed sentence this service states
//!    for that conflict, and creates NOTHING — no id is minted, and the VPC origin that holds
//!    the name keeps its own identity, its own stored configuration and its own version
//!    token, even where the repeat submits a different configuration;
//! 2. the refusal is about the NAME alone: names that differ at all are different names, so
//!    each of them is admitted — including two that differ only in case, or only by one
//!    character;
//! 3. a member whose value CloudFront's argument validation cannot read is refused by the
//!    PATH of that member in the request, with the sentence for the way it failed: a
//!    `VpcOriginEndpointConfig$Arn` outside the FORM the member takes — no ARN at all, or an
//!    ARN of a service that owns no resource a VPC origin fronts — fails that form ("The
//!    parameter VpcOriginEndpointConfig Arn Invalid Format."), while one inside that form
//!    whose resource part names no such resource fails the member's validation ("The parameter
//!    VpcOriginEndpointConfig Arn ValidationError.") — and neither refusal takes the name it
//!    was submitted with, which a later create proves by taking it.
//!
//! The rows span what the rules generalise over: the character classes and lengths a name is
//! written in, the kind of resource an endpoint ARN names, and each shape an unreadable or
//! unusable ARN takes.
//!
//! The codes and sentences are LITERALS here rather than the crate's own constants, so a
//! reworded refusal fails this suite. The wire harness is [`crate::distribution_lifecycle`]'s
//! and the create is [`crate::vpc_origin_lifecycle`]'s.

use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{call, emulator, Answer, ACCOUNT_ID, API_VERSION};
use crate::key_value_store_lifecycle::escape;
use crate::vpc_origin_lifecycle::{create_vpc_origin, load_balancer_arn};

/// The sentence a create naming a VPC origin name the account already holds answers with.
const ALREADY_EXISTS_MESSAGE: &str =
    "Another vpc origin with the same name already exists within the aws account.";

/// The two sentences this service's argument validation refuses an input MEMBER with, for the
/// member path the endpoint configuration's ARN arrives under.
const ARN_INVALID_FORMAT_MESSAGE: &str = "The parameter VpcOriginEndpointConfig Arn Invalid \
     Format.";
const ARN_VALIDATION_ERROR_MESSAGE: &str = "The parameter VpcOriginEndpointConfig Arn \
     ValidationError.";

/// The `CreateVpcOriginRequest` document for one endpoint configuration.
fn create_body(name: &str, arn: &str, http_port: i32, https_port: i32) -> String {
    format!(
        "<CreateVpcOriginRequest xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <VpcOriginEndpointConfig>\
         <Name>{}</Name><Arn>{}</Arn>\
         <HTTPPort>{http_port}</HTTPPort><HTTPSPort>{https_port}</HTTPSPort>\
         <OriginProtocolPolicy>http-only</OriginProtocolPolicy>\
         </VpcOriginEndpointConfig></CreateVpcOriginRequest>",
        escape(name),
        escape(arn)
    )
}

/// `GET /2020-05-31/vpc-origin/{Id}`.
async fn get_vpc_origin(router: &MultiServiceRouter, id: &str) -> Answer {
    call(
        router,
        "GET",
        &format!("/{API_VERSION}/vpc-origin/{id}"),
        None,
        None,
    )
    .await
}

/// The names rules 1 and 2 are asserted over: the shapes a caller writes one in.
///
/// `VpcOriginEndpointConfig$Name` is modelled as a plain `string`
/// (`smithy_query(query="shape",shape="VpcOriginEndpointConfig")` states no length or pattern
/// for it), so the rows differ along the character classes and lengths a caller may choose
/// rather than along a published bound.
fn name_table() -> Vec<(&'static str, String)> {
    vec![
        ("the shortest name", "a".to_string()),
        ("hyphens and digits", "orders-alb-01".to_string()),
        ("underscores and dots", "checkout.internal_1".to_string()),
        ("mixed case", "OrdersAlb".to_string()),
        ("sixty-four characters", "n".repeat(64)),
    ]
}

/// RULE 1: a name the account already holds cannot name a second VPC origin, and the refusal
/// creates nothing.
#[tokio::test]
async fn a_name_the_account_already_holds_is_refused_and_creates_nothing() {
    let router = emulator();

    for (case, name) in name_table() {
        let first = create_vpc_origin(
            &router,
            &create_body(&name, &load_balancer_arn(&name), 80, 443),
        )
        .await;
        assert_eq!(
            first.status, 202,
            "case {case}: expected HTTP 202 from the first create, got {} with {}",
            first.status, first.raw
        );
        let id = first.root().find_text("Id").unwrap_or_default().to_string();
        let etag = first.header("etag").to_string();

        // The repeat carries a DIFFERENT configuration under the SAME name — a different
        // endpoint ARN and different ports — so a service that replaced the stored record
        // rather than refusing the name would show up in the read-back below.
        let repeat = create_vpc_origin(
            &router,
            &create_body(
                &name,
                &format!("arn:aws:ec2:us-east-1:{ACCOUNT_ID}:instance/i-0123456789abcdef0"),
                8080,
                8443,
            ),
        )
        .await;
        assert_eq!(
            repeat.status, 409,
            "case {case}: expected HTTP 409 from the repeat, got {} with {}",
            repeat.status, repeat.raw
        );
        assert_eq!(
            repeat.error_code(),
            "EntityAlreadyExists",
            "case {case}: expected EntityAlreadyExists, got {:?} ({})",
            repeat.error_code(),
            repeat.raw
        );
        assert_eq!(
            repeat.root().find_text("Message").unwrap_or_default(),
            ALREADY_EXISTS_MESSAGE,
            "case {case}: expected message {ALREADY_EXISTS_MESSAGE:?}, got {}",
            repeat.raw
        );
        // Nothing was minted: the refusal carries no VPC origin, no id and no location.
        assert_eq!(
            repeat.root().find_text("Id"),
            None,
            "case {case}: a refused create must mint nothing, got {}",
            repeat.raw
        );
        assert_eq!(
            repeat.header("location"),
            "",
            "case {case}: a refused create must announce no location, got {:?}",
            repeat.header("location")
        );

        // The VPC origin that holds the name is exactly as its own create left it.
        let read = get_vpc_origin(&router, &id).await;
        assert_eq!(
            read.status, 200,
            "case {case}: expected the holder to still read back, got {} with {}",
            read.status, read.raw
        );
        assert_eq!(
            read.header("etag"),
            etag,
            "case {case}: the refusal must not rewrite the holder's version token, got {:?}",
            read.header("etag")
        );
        let config = read
            .root()
            .at("VpcOriginEndpointConfig")
            .unwrap_or_else(|| panic!("case {case}: no VpcOriginEndpointConfig: {}", read.raw));
        assert_eq!(
            config.text_at("Name").unwrap_or_default(),
            name,
            "case {case}: expected the stored Name, got {}",
            read.raw
        );
        assert_eq!(
            config.text_at("Arn").unwrap_or_default(),
            load_balancer_arn(&name),
            "case {case}: expected the FIRST create's endpoint Arn, got {}",
            read.raw
        );
        assert_eq!(
            config.text_at("HTTPPort").unwrap_or_default(),
            "80",
            "case {case}: expected the FIRST create's HTTPPort, got {}",
            read.raw
        );
    }
}

/// RULE 2: names that differ at all are different names, so each of them is admitted — the
/// uniqueness rule is over the whole value, not a normalisation of it.
#[tokio::test]
async fn names_that_differ_each_name_their_own_vpc_origin() {
    let router = emulator();
    // Rows differing by one character, by case alone, and by a trailing character — the
    // distinctions a service that compared names loosely would lose.
    let names = [
        "orders-alb",
        "orders-alb2",
        "Orders-Alb",
        "orders-alb-",
        "orders_alb",
    ];
    let mut ids = Vec::new();

    for name in names {
        let created = create_vpc_origin(
            &router,
            &create_body(name, &load_balancer_arn(name), 80, 443),
        )
        .await;
        assert_eq!(
            created.status, 202,
            "case {name}: expected HTTP 202, got {} with {}",
            created.status, created.raw
        );
        let id = created
            .root()
            .find_text("Id")
            .unwrap_or_default()
            .to_string();
        assert!(
            !ids.contains(&id),
            "case {name}: expected a freshly minted id, got the held {id}"
        );
        ids.push(id);
    }

    // Each name reads back on its own record.
    for (name, id) in names.iter().zip(&ids) {
        let read = get_vpc_origin(&router, id).await;
        assert_eq!(
            read.root()
                .at("VpcOriginEndpointConfig")
                .and_then(|config| config.text_at("Name"))
                .unwrap_or_default(),
            *name,
            "case {name}: expected the VPC origin created under this name, got {}",
            read.raw
        );
    }
}

/// RULE 3: the endpoint ARN is refused by the PATH of the member it arrived in, with the
/// sentence that says which of the two ways the value failed — and the refusal takes neither
/// the name nor anything else of the request.
#[tokio::test]
async fn an_endpoint_arn_is_refused_by_the_path_of_the_member_it_arrived_in() {
    let router = emulator();

    // Rows along the shape of a value outside the FORM this member takes: the member holds the
    // ARN of one of the resources a VPC origin fronts, so a value that is no ARN at all and an
    // ARN of a service that owns no such resource are alike outside it.
    let unreadable: [(&str, String); 7] = [
        ("the empty string", String::new()),
        ("a bare word", "not-a-valid-arn".to_string()),
        ("the resource part alone", "vpcorigin/vo-1".to_string()),
        (
            "the prefix in upper case",
            format!("ARN:aws:ec2:us-east-1:{ACCOUNT_ID}:instance/i-0123456789abcdef0"),
        ),
        (
            "a URL rather than an ARN",
            "https://orders.internal.example.com".to_string(),
        ),
        (
            "an ARN of a service that owns no resource a VPC origin fronts",
            "arn:aws:s3:::orders-bucket".to_string(),
        ),
        (
            "another such service's resource",
            format!("arn:aws:lambda:us-east-1:{ACCOUNT_ID}:function:orders"),
        ),
    ];
    for (case, value) in &unreadable {
        let refused =
            create_vpc_origin(&router, &create_body("arn-form", value, 80, 443)).await;
        assert_eq!(
            refused.status, 400,
            "case {case}: expected HTTP 400 for {value:?}, got {} with {}",
            refused.status, refused.raw
        );
        assert_eq!(
            refused.error_code(),
            "InvalidArgument",
            "case {case}: expected InvalidArgument for {value:?}, got {:?} ({})",
            refused.error_code(),
            refused.raw
        );
        assert_eq!(
            refused.root().find_text("Message").unwrap_or_default(),
            ARN_INVALID_FORMAT_MESSAGE,
            "case {case}: expected message {ARN_INVALID_FORMAT_MESSAGE:?}, got {}",
            refused.raw
        );
    }

    // Rows along the shape of a value INSIDE that form — an ARN of a service that owns a
    // resource a VPC origin fronts — whose resource part names no such resource: another
    // resource type of that service, a load balancer kind CloudFront creates no VPC origin for,
    // and an instance id of no width EC2 issues.
    let unusable: [(&str, String); 3] = [
        (
            "another resource type of a service that owns one",
            format!("arn:aws:ec2:us-east-1:{ACCOUNT_ID}:vpc/vpc-0123456789abcdef0"),
        ),
        (
            "a Gateway Load Balancer",
            format!(
                "arn:aws:elasticloadbalancing:us-east-1:{ACCOUNT_ID}:loadbalancer/gwy/edge/\
                 0123456789abcdef"
            ),
        ),
        (
            "an instance id of no width EC2 issues",
            format!("arn:aws:ec2:us-east-1:{ACCOUNT_ID}:instance/i-01"),
        ),
    ];
    for (case, value) in &unusable {
        let refused =
            create_vpc_origin(&router, &create_body("arn-validation", value, 80, 443)).await;
        assert_eq!(
            refused.status, 400,
            "case {case}: expected HTTP 400 for {value:?}, got {} with {}",
            refused.status, refused.raw
        );
        assert_eq!(
            refused.error_code(),
            "InvalidArgument",
            "case {case}: expected InvalidArgument for {value:?}, got {:?} ({})",
            refused.error_code(),
            refused.raw
        );
        assert_eq!(
            refused.root().find_text("Message").unwrap_or_default(),
            ARN_VALIDATION_ERROR_MESSAGE,
            "case {case}: expected message {ARN_VALIDATION_ERROR_MESSAGE:?}, got {}",
            refused.raw
        );
    }

    // Neither refusal took the name it was submitted with: a create that names the same two
    // names, with an ARN that does name a resource a VPC origin can front, is admitted.
    for name in ["arn-form", "arn-validation"] {
        let created = create_vpc_origin(
            &router,
            &create_body(name, &load_balancer_arn(name), 80, 443),
        )
        .await;
        assert_eq!(
            created.status, 202,
            "case {name} after the refusals: expected HTTP 202, got {} with {}",
            created.status, created.raw
        );
    }
}
