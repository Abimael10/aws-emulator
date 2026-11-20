// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the WORDING of the MicroVM image API's authorization refusal,
//! driven over the same router the emulator boots.
//!
//! RULE: a customer-facing call refused on a MicroVM image identifier answers an IAM
//! denial that names the CALLING PRINCIPAL — the identity `sts:GetCallerIdentity` reports
//! for the credentials that signed the request — in one of two forms, chosen by why the
//! caller holds no grant on the identifier:
//!
//! * an identifier of AWS's OWN image namespace names a resource no policy of any account
//!   can name, so the service refuses the OPERATION and the sentence names neither an
//!   action nor a resource: `User: <caller> is not authorized to perform this operation`;
//! * every other refused identifier is one IAM evaluated this action against, so the
//!   sentence is IAM's own: `User: <caller> is not authorized to perform:
//!   lambda:<OperationName> on resource: <identifier>`, with the identifier the request
//!   carried, verbatim.
//!
//! Neither form says whether the identifier names anything, and neither carries a value
//! this service holds: both are built from the request and the calling identity alone.
//!
//! Each test states that rule and exercises it over a SET of inputs; the values any one
//! row happens to carry are examples, not the specification. The wire harness
//! (`emulator`, `call`, `image_body`, …) is the one `microvm_lifecycle` owns.

use serde_json::{json, Value};
use service_com_amazonaws_lambdamicrovms_lambdamicrovms::state::{
    managed_base_image_arn, FIRST_IMAGE_VERSION, MANAGED_BASE_IMAGE_IDS,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::microvm_lifecycle::{
    as_uri_value, call, emulator, expected_image_arn, image_body, ACCOUNT, REGION,
};

/// `MicrovmImageIdentifier` is `@length(1..256)`
/// (`smithy_query(query="operation",operation="GetMicrovmImage")`), so a refusal has to
/// carry an identifier of up to that many characters back verbatim.
const MAX_IDENTIFIER_LENGTH: usize = 256;

/// A build id of the form this service mints, for the build read's own path label — the
/// refusal is taken before any build is looked for, so which id it carries cannot matter.
const PROBE_BUILD_ID: &str = "build-0000000000000000";

/// The identity that signed the request: the account's own IAM user, which
/// `sts:GetCallerIdentity` reports as `arn:aws:iam::<account>:user/emulator`, and the one
/// variable part both sentences share.
fn caller_arn() -> String {
    format!("arn:aws:iam::{ACCOUNT}:user/emulator")
}

/// The sentence a refusal on AWS's OWN namespace carries, spelled out here rather than
/// read from the crate so a change to the wording fails this suite: fixed text `User: `
/// and ` is not authorized to perform this operation`.
pub(crate) fn operation_denial_sentence() -> String {
    format!(
        "User: {} is not authorized to perform this operation",
        caller_arn()
    )
}

/// The sentence every other refusal carries: fixed text `User: `, ` is not authorized to
/// perform: `, `lambda:` — this service's IAM action namespace, its own
/// `aws.api#service$arnNamespace` (`smithy_query(query="service_info")`) — and ` on
/// resource: `, with the API being refused and the identifier the request carried as its
/// variable parts.
pub(crate) fn action_denial_sentence(operation: &str, identifier: &str) -> String {
    format!(
        "User: {} is not authorized to perform: lambda:{operation} on resource: {identifier}",
        caller_arn()
    )
}

/// Whether `identifier` names a resource of AWS's OWN namespace: an ARN whose ACCOUNT
/// field is literally `aws`, read here by splitting the ARN's fields rather than through
/// the crate, so this suite's classifier is its own.
fn names_an_aws_owned_resource(identifier: &str) -> bool {
    identifier.starts_with("arn:") && identifier.split(':').nth(4) == Some("aws")
}

/// The sentence a refusal of `operation` on `identifier` must carry, by the rule this
/// suite states — the shared expectation the crate's other refusal suites hold their
/// denial rows against, so none of them has to ask the implementation what it wrote.
pub(crate) fn expected_denial_sentence(operation: &str, identifier: &str) -> String {
    match names_an_aws_owned_resource(identifier) {
        true => operation_denial_sentence(),
        false => action_denial_sentence(operation, identifier),
    }
}

/// One refused identifier: why it is refused, the identifier itself, and whether it names
/// a resource of AWS's own namespace — the one dimension that chooses the sentence.
struct Refused {
    case: String,
    identifier: String,
    aws_owned: bool,
}

impl Refused {
    /// The sentence this identifier's refusal of `operation` must carry.
    fn expected_sentence(&self, operation: &str) -> String {
        match self.aws_owned {
            true => operation_denial_sentence(),
            false => action_denial_sentence(operation, &self.identifier),
        }
    }
}

/// Every customer-facing call this suite addresses at one `imageIdentifier`, as
/// `(IAM action name, method, URI, body)`.
///
/// The URIs are the ones the model binds
/// (`smithy_query(query="operation",operation="GetMicrovmImage,…")`), and the operation
/// name is the IAM action of that API — what the refusal of it has to name.
fn image_calls(identifier: &str) -> Vec<(&'static str, &'static str, String, Option<Value>)> {
    image_calls_at_path(&format!(
        "/2025-09-09/microvm-images/{}",
        as_uri_value(identifier)
    ))
}

/// The same calls for an identifier already escaped into a path segment, so a value
/// carrying a character no path segment admits can be addressed too.
fn image_calls_at_path(path: &str) -> Vec<(&'static str, &'static str, String, Option<Value>)> {
    let version = FIRST_IMAGE_VERSION;
    vec![
        ("GetMicrovmImage", "GET", path.to_string(), None),
        (
            "ListMicrovmImageVersions",
            "GET",
            format!("{path}/versions"),
            None,
        ),
        (
            "GetMicrovmImageVersion",
            "GET",
            format!("{path}/versions/{version}"),
            None,
        ),
        (
            "ListMicrovmImageBuilds",
            "GET",
            format!("{path}/versions/{version}/builds"),
            None,
        ),
        (
            "GetMicrovmImageBuild",
            "GET",
            format!("{path}/versions/{version}/builds/{PROBE_BUILD_ID}"),
            None,
        ),
        (
            "UpdateMicrovmImageVersion",
            "PATCH",
            format!("{path}/versions/{version}"),
            Some(json!({"status": "INACTIVE"})),
        ),
    ]
}

/// The refusal a denied row must carry: the modelled status and code, and `expected`.
fn assert_denied(status: u16, body: &Value, case: &str, expected: &str) {
    assert_eq!(status, 403, "case {case}: expected 403, got {status} {body}");
    assert_eq!(
        body["__type"].as_str(),
        Some("AccessDeniedException"),
        "case {case}: expected AccessDeniedException, got {body}"
    );
    assert_eq!(
        body["message"].as_str(),
        Some(expected),
        "case {case}: expected {expected:?}, got {:?}",
        body["message"]
    );
}

/// `POST /2025-09-09/microvm-images`, asserting the create.
async fn create_image(router: &MultiServiceRouter, name: &str) {
    let (status, created) = call(
        router,
        "POST",
        "/2025-09-09/microvm-images",
        Some(image_body(name, json!({}))),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {name:?}: create expected 201, got {status} {created}"
    );
}

/// RULE: the refused identifier chooses the FORM of the sentence — AWS's own namespace is
/// refused as an operation, every other ungranted identifier with IAM's action-and-resource
/// sentence — and the operation chooses the IAM action that sentence names, on every
/// customer-facing call addressed at a `MicrovmImageIdentifier`.
#[tokio::test]
async fn a_denied_image_call_names_the_caller_and_what_it_was_refused() {
    let router = emulator();

    // An image this account DOES hold, so the rows are refused for the identifier they
    // name rather than for an empty store.
    let held = "held-image";
    create_image(&router, held).await;

    // An identifier of another account at the published length bound: the refusal has to
    // carry the longest identifier the API admits back verbatim.
    let prefix = format!("arn:aws:lambda:{REGION}:210987654321:microvm-image:");
    let longest = format!(
        "{prefix}{}",
        "x".repeat(MAX_IDENTIFIER_LENGTH - prefix.len())
    );
    assert_eq!(
        longest.len(),
        MAX_IDENTIFIER_LENGTH,
        "the longest row must sit exactly on the published bound, got {}",
        longest.len()
    );

    // Rows differ in WHY the caller holds no grant on the identifier: AWS's own namespace
    // (an entry the catalogue publishes, and one it does not), another account's, another
    // partition's, another vendor's, another resource type's, and an identifier of this
    // account's own resource type at the length bound.
    let mut rows: Vec<Refused> = MANAGED_BASE_IMAGE_IDS
        .iter()
        .map(|image_id| Refused {
            case: format!("an AWS-owned image the catalogue publishes {image_id:?}"),
            identifier: managed_base_image_arn(REGION, image_id),
            aws_owned: true,
        })
        .collect();
    rows.extend([
        Refused {
            case: "an AWS-owned ARN the catalogue does not publish".to_string(),
            identifier: managed_base_image_arn(REGION, "no-such-base-image"),
            aws_owned: true,
        },
        Refused {
            case: "an AWS-owned ARN of another partition".to_string(),
            identifier: format!(
                "arn:aws-us-gov:lambda:{REGION}:aws:microvm-image:{}",
                MANAGED_BASE_IMAGE_IDS[0]
            ),
            aws_owned: true,
        },
        Refused {
            case: "another account's ARN for a name this one holds".to_string(),
            identifier: format!("arn:aws:lambda:{REGION}:210987654321:microvm-image:{held}"),
            aws_owned: false,
        },
        Refused {
            case: "another account's ARN at the identifier length bound".to_string(),
            identifier: longest,
            aws_owned: false,
        },
        Refused {
            case: "another partition's ARN for a name this account holds".to_string(),
            identifier: format!("arn:aws-us-gov:lambda:{REGION}:{ACCOUNT}:microvm-image:{held}"),
            aws_owned: false,
        },
        Refused {
            case: "another vendor's ARN".to_string(),
            identifier: format!("arn:aws:s3:{REGION}:{ACCOUNT}:microvm-image:{held}"),
            aws_owned: false,
        },
        Refused {
            case: "another resource type of this account".to_string(),
            identifier: format!("arn:aws:lambda:{REGION}:{ACCOUNT}:function:{held}"),
            aws_owned: false,
        },
    ]);

    for row in &rows {
        for (operation, method, uri, body) in image_calls(&row.identifier) {
            let case = format!("{operation} on {} ({})", row.case, row.identifier);
            let expected = row.expected_sentence(operation);
            let (status, response) = call(&router, method, &uri, body).await;
            assert_denied(status, &response, &case, &expected);
        }
    }

    // The positive control: this account's own image is still served through the same
    // path, so the rows above are refused by the verdict and not by a dead path — and no
    // refusal changed what a later caller reads.
    let held_arn = expected_image_arn(held);
    let (status, image) = call(
        &router,
        "GET",
        &format!("/2025-09-09/microvm-images/{}", as_uri_value(&held_arn)),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "this account's own image is served, got {status} {image}"
    );
    assert_eq!(
        image["imageArn"].as_str(),
        Some(held_arn.as_str()),
        "the read must report the stored record, got {image}"
    );
}

/// RULE: the resource the sentence names is the identifier the REQUEST carried, verbatim —
/// not an identifier this service mints and not one it resolved — so a value spelling this
/// account's own resource type in a form `MicrovmImageArn`'s pattern does not admit
/// (`microvm-image/<name>`) is named back exactly as it arrived.
#[tokio::test]
async fn a_denial_names_the_identifier_the_request_carried() {
    let router = emulator();

    let held = "held-image";
    create_image(&router, held).await;

    // Rows are this account's own account, region and resource type in the `type/id`
    // resource form, for a name nothing was created under and for one this account DOES
    // hold: neither names an image of this account, so both are refused, naming the
    // identifier as the path label spells it. The `/` is percent-encoded because it is not
    // a legal character in a single path segment.
    let rows = [
        (
            "the `type/id` form, a name nothing was created under",
            "never-created",
        ),
        ("the `type/id` form, a name this account holds", held),
    ];

    for (case, name) in rows {
        let identifier = format!("arn:aws:lambda:{REGION}:{ACCOUNT}:microvm-image/{name}");
        let path = format!(
            "/2025-09-09/microvm-images/{}",
            as_path_segment(&identifier)
        );
        for (operation, method, uri, body) in image_calls_at_path(&path) {
            let (status, response) = call(&router, method, &uri, body).await;
            assert_denied(
                status,
                &response,
                &format!("{operation} on {case} ({identifier})"),
                &action_denial_sentence(operation, &identifier),
            );
        }
    }
}

/// `UpdateMicrovmImage`'s required body members
/// (`smithy_query(query="operation",operation="UpdateMicrovmImage")`: `baseImageArn`,
/// `buildRoleArn` and `codeArtifact`).
///
/// A refusal on the identifier is taken before anything the body configures is read, so
/// WHAT it configures cannot matter — but it has to satisfy the boundary validator for the
/// call to reach the operation at all.
fn update_image_body() -> Value {
    json!({
        "baseImageArn": managed_base_image_arn(REGION, MANAGED_BASE_IMAGE_IDS[0]),
        "buildRoleArn": format!("arn:aws:iam::{ACCOUNT}:role/microvm-build"),
        "codeArtifact": {"uri": "s3://artifacts/app.zip"},
    })
}

/// `identifier` as ONE URI path segment: `MicrovmImageIdentifier` is bound to an
/// `@httpLabel`, so a `/` the value carries arrives percent-encoded to stay part of that
/// one label instead of splitting it — which is also why [`as_uri_value`] refuses such a
/// value outright.
fn as_path_segment(identifier: &str) -> String {
    match identifier.contains('/') {
        true => identifier.replace('/', "%2F"),
        false => as_uri_value(identifier).to_string(),
    }
}

/// One call that MUTATES or LAUNCHES through an `imageIdentifier`: the IAM action name,
/// the method and URI the model binds it to, its body, and which identifiers it takes the
/// AUTHORIZATION verdict on.
struct Changing {
    operation: &'static str,
    method: &'static str,
    uri: String,
    body: Option<Value>,
    /// Whether this call takes the authorization verdict on EVERY identifier outside this
    /// account's own image namespace, as the read path does
    /// ([`crate::image_read_authorization`]), or only on one of AWS's own namespace and
    /// answers about the RESOURCE for the rest.
    ///
    /// WHICH identifiers a call is refused on is that operation's own behaviour and is
    /// pinned by that operation's own suite — `gone_image_delete_refusal` holds
    /// `DeleteMicrovmImage` to the identifier's RESOLUTION, where another account's,
    /// another partition's and another resource type's ARN are all the modelled 404, and
    /// `microvm_lifecycle` holds `RunMicrovm` to the one failed resolution it reports.
    /// What THIS suite owns is the WORDING wherever the authorization verdict is the
    /// answer, so a row a call answers about the resource instead is still asserted: it
    /// must carry no `AccessDeniedException` at all
    /// ([`assert_not_an_authorization_refusal`]), which is what keeps the two rules from
    /// drifting into one another unnoticed.
    refuses_every_ungranted_identifier: bool,
}

/// Every write and launch this suite addresses at one `imageIdentifier`, for the
/// identifier `identifier`.
///
/// The methods and URIs are the ones the model binds
/// (`smithy_query(query="operation",operation="DeleteMicrovmImage,UpdateMicrovmImage,DeleteMicrovmImageVersion,RunMicrovm")`),
/// and the operation name is the IAM action of that API — what the refusal of it has to
/// name. `RunMicrovm` carries its identifier in the BODY rather than in a path label,
/// which changes nothing about the sentence it is refused with.
fn changing_calls(identifier: &str) -> Vec<Changing> {
    let path = format!("/2025-09-09/microvm-images/{}", as_path_segment(identifier));
    vec![
        Changing {
            operation: "DeleteMicrovmImage",
            method: "DELETE",
            uri: path.clone(),
            body: None,
            refuses_every_ungranted_identifier: false,
        },
        Changing {
            operation: "UpdateMicrovmImage",
            method: "PUT",
            uri: path.clone(),
            body: Some(update_image_body()),
            refuses_every_ungranted_identifier: false,
        },
        Changing {
            operation: "DeleteMicrovmImageVersion",
            method: "DELETE",
            uri: format!("{path}/versions/{FIRST_IMAGE_VERSION}"),
            body: None,
            refuses_every_ungranted_identifier: true,
        },
        Changing {
            operation: "RunMicrovm",
            method: "POST",
            uri: "/2025-09-09/microvms".to_string(),
            body: Some(json!({"imageIdentifier": identifier})),
            refuses_every_ungranted_identifier: false,
        },
    ]
}

/// What a call that does NOT take the authorization verdict on this identifier must
/// answer: the modelled 404 its own suite pins for an identifier that resolves to no
/// record this account holds — never a success, and never the denial this suite words,
/// because a 403 `AccessDeniedException` here would be an authorization answer with no
/// authorization rule behind it.
///
/// The SENTENCE of that 404 belongs to the suite that owns the operation
/// (`gone_image_delete_refusal` for the delete, `microvm_lifecycle` for the launch); what
/// is asserted here is only that the answer is that refusal and not this one.
fn assert_not_an_authorization_refusal(status: u16, body: &Value, case: &str) {
    assert_eq!(
        status, 404,
        "case {case}: an identifier this account holds no record of is the modelled \
         not-found here, got {status} {body}"
    );
    assert_eq!(
        body["__type"].as_str(),
        Some("ResourceNotFoundException"),
        "case {case}: expected the resource answer rather than an authorization one, got \
         {body}"
    );
}

/// RULE: the denial sentence belongs to the API's AUTHORIZATION VERDICT wherever that
/// verdict is what answers, not to the read path — a WRITE and a LAUNCH refused on an
/// `imageIdentifier` carry the same two forms, each naming its OWN IAM action. Which
/// identifiers a given call takes that verdict on is the operation's own behaviour
/// ([`Changing::refuses_every_ungranted_identifier`]), and where it answers about the
/// RESOURCE instead it carries no denial at all. Either way a refused call writes nothing:
/// the image this account does hold is still there afterwards, exactly as it was.
#[tokio::test]
async fn a_denied_write_or_launch_names_the_caller_and_what_it_was_refused() {
    let router = emulator();

    // An image this account DOES hold, so a refusal that let a write through would be
    // visible as a change to it.
    let held = "held-image";
    create_image(&router, held).await;
    let (_, before) = call(
        &router,
        "GET",
        &format!(
            "/2025-09-09/microvm-images/{}",
            as_uri_value(&expected_image_arn(held))
        ),
        None,
    )
    .await;

    // Rows differ in WHY the caller holds no grant on the identifier: AWS's own namespace
    // (an entry the catalogue publishes and one it does not — a denial says nothing about
    // whether the identifier names anything), and two identifiers outside that namespace
    // the account was never granted either.
    let rows: Vec<Refused> = vec![
        Refused {
            case: "an AWS-owned image the catalogue publishes".to_string(),
            identifier: managed_base_image_arn(REGION, MANAGED_BASE_IMAGE_IDS[0]),
            aws_owned: true,
        },
        Refused {
            case: "an AWS-owned ARN the catalogue does not publish".to_string(),
            identifier: managed_base_image_arn(REGION, "no-such-base-image"),
            aws_owned: true,
        },
        Refused {
            case: "another account's ARN for a name this one holds".to_string(),
            identifier: format!("arn:aws:lambda:{REGION}:210987654321:microvm-image:{held}"),
            aws_owned: false,
        },
        Refused {
            case: "this account's resource type in the `type/id` form".to_string(),
            identifier: format!("arn:aws:lambda:{REGION}:{ACCOUNT}:microvm-image/never-created"),
            aws_owned: false,
        },
    ];

    for row in &rows {
        for changing in changing_calls(&row.identifier) {
            let case = format!(
                "{} on {} ({})",
                changing.operation, row.case, row.identifier
            );
            let (status, response) =
                call(&router, changing.method, &changing.uri, changing.body).await;
            // The authorization verdict is what answers an identifier of AWS's own
            // namespace on every one of these calls, and every OTHER ungranted identifier
            // on the calls that take that verdict before they look anything up. Where it
            // is not, the row is still asserted — on the one thing this suite's rule says
            // about it: the answer is a refusal of its own and carries no denial sentence.
            match row.aws_owned || changing.refuses_every_ungranted_identifier {
                true => assert_denied(
                    status,
                    &response,
                    &case,
                    &row.expected_sentence(changing.operation),
                ),
                false => assert_not_an_authorization_refusal(status, &response, &case),
            }
        }
    }

    // The control: not one refusal touched the image this account holds — a denied write
    // leaves the record it was refused on exactly as it was.
    let (status, after) = call(
        &router,
        "GET",
        &format!(
            "/2025-09-09/microvm-images/{}",
            as_uri_value(&expected_image_arn(held))
        ),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "the image this account holds is still served after the refusals, got {status} {after}"
    );
    assert_eq!(
        after, before,
        "no refused write may change the stored image, got {after}"
    );
}

/// RULE: the sentence is built from the request and the calling identity alone, so it
/// carries nothing a later caller can see change: asking twice gets the same sentence, and
/// a denial of an own-namespace identifier the build read refuses still names that
/// identifier rather than anything the store holds.
#[tokio::test]
async fn a_denial_is_stable_and_changes_nothing() {
    let router = emulator();

    let held = "held-image";
    create_image(&router, held).await;

    // The build read refuses an identifier it cannot resolve to an image of this account,
    // which includes a well-formed ARN of this account's own namespace nobody created —
    // so its refusal is the action-and-resource form naming that identifier.
    let never_created = expected_image_arn("never-created");
    let uri = format!(
        "/2025-09-09/microvm-images/{}/versions/{FIRST_IMAGE_VERSION}/builds/{PROBE_BUILD_ID}",
        as_uri_value(&never_created)
    );
    let expected = action_denial_sentence("GetMicrovmImageBuild", &never_created);

    // Asked twice: a refusal is not a consumable, so the second answer is the first.
    for poll in 1..=2 {
        let (status, response) = call(&router, "GET", &uri, None).await;
        assert_denied(
            status,
            &response,
            &format!("GetMicrovmImageBuild on {never_created} poll {poll}"),
            &expected,
        );
    }

    // And the image this account does hold still reads back, so neither refusal touched
    // the store.
    let held_arn = expected_image_arn(held);
    let (status, image) = call(
        &router,
        "GET",
        &format!("/2025-09-09/microvm-images/{}", as_uri_value(&held_arn)),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "this account's own image is served after the refusals, got {status} {image}"
    );
}
