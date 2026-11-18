// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack that declares an `AWS::CloudFront::CachePolicy`, an
//! `AWS::CloudFront::PublicKey` or an `AWS::CloudFront::KeyGroup` really creates it in Amazon
//! CloudFront, and the id the stack publishes is the one that service's own listing and read
//! answer to.
//!
//! The rule under test: **a resource type CloudFormation recognises and whose owning service
//! this emulator mounts is created through that service's own API, under the identity that
//! service mints — so the stack's `Ref`/`Fn::GetAtt` outputs carry a value that service's own
//! read and list operations resolve to the resource the template configured, and deleting the
//! stack takes it away there.** Two crates have to agree for that to hold (CloudFormation
//! makes the calls; CloudFront mints the ids and holds the configurations), so the suite lives
//! here rather than in either crate.
//!
//! The rows differ along what the rule generalises over: which resource type is declared (a
//! policy whose configuration nests three behaviour structures and the lists CloudFront wraps
//! in `{Quantity, Items}` containers, a public key whose body is a multi-line PEM, a key group
//! whose configuration carries a flat list), whether the template is written in JSON or YAML,
//! which collection the resource is listed in, and which intrinsic each output reads the
//! resource with (`Ref`, `Fn::GetAtt … Id`).
//!
//! Every expected value is computed here from what the row declares, never read back out of
//! the answer being checked — except the ids, which are CloudFront's to mint: what is checked
//! about an id is that ONE value appears everywhere the stack reports the resource's identity,
//! and that CloudFront's own listing and read resolve it to the resource the template
//! configured.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use smithy_server_runtime::MultiServiceRouter;

// The awsQuery form encoder and the XML element readers this crate writes ONCE
// (`tests/all/query_wire.rs`): "Nothing new should write another one — call this module."
use super::query_wire::{element, elements, form_encode, opt_element, outputs_of};

/// How long a stack operation is given to reach its terminal status. GENEROUS: what is waited
/// for is the CONDITION (the status the stack reports), polled until it holds, so the only
/// thing this bound decides is when a stack that never settles is reported as a failure.
const SETTLES_WITHIN: Duration = Duration::from_secs(60);

/// How often the stack's status is read while waiting for it to settle.
const POLL_EVERY: Duration = Duration::from_millis(50);

/// The Region every row deploys in. CloudFront is a global service, and the Region is what
/// the credential scope of each call names.
const REGION: &str = "us-east-1";

/// The logical id each row declares its resource under.
const LOGICAL_ID: &str = "TheResource";

/// The emulator with the two services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them. Request authentication is off, as in every other
/// suite here; the in-process call CloudFormation makes signs its request either way (the
/// credential scope is what routes it).
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_cloudfront_cloudfront2020_05_31::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service` in [`REGION`], which
/// is what routes the request and what each handler reads its Region from. The signature is
/// never checked: this router runs with request authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Call one CloudFormation operation over its awsQuery wire.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("cloudformation.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = String::from_utf8_lossy(response.body()).to_string();
    (status, payload)
}

/// Call one CloudFront read over its restXml wire.
async fn cloudfront(router: &MultiServiceRouter, path: &str) -> (u16, String) {
    let request = http::Request::builder()
        .method("GET")
        .uri(path)
        .header("host", format!("cloudfront.{REGION}.amazonaws.com"))
        .header("authorization", authorization("cloudfront"))
        .body(Bytes::new())
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = String::from_utf8_lossy(response.body()).to_string();
    (status, payload)
}

/// Read the stack's status until it is one `DescribeStacks` reports as terminal, or until
/// [`SETTLES_WITHIN`] has passed.
///
/// The CONDITION is what is waited for — a status that is not still in progress — rather than
/// a fixed span, so the suite answers as soon as the stack has settled however fast or slow
/// the host is.
async fn settled_stack(router: &MultiServiceRouter, stack_name: &str, case: &str) -> String {
    let deadline = Instant::now() + SETTLES_WITHIN;
    loop {
        let (status, described) =
            cloudformation(router, "DescribeStacks", &[("StackName", stack_name)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStacks, got {status} with body \
             {described}"
        );
        let reported = opt_element(&described, "StackStatus").unwrap_or_default();
        if !reported.ends_with("_IN_PROGRESS") {
            return described;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the stack was still {reported:?} when the wait ran out"
        );
        tokio::time::sleep(POLL_EVERY).await;
    }
}

/// Read the stack until `DescribeStacks` no longer reports it at all, or until
/// [`SETTLES_WITHIN`] has passed — which is how a client watches a deletion through.
async fn deleted_stack(router: &MultiServiceRouter, stack_name: &str, case: &str) {
    let deadline = Instant::now() + SETTLES_WITHIN;
    loop {
        let (_, described) =
            cloudformation(router, "DescribeStacks", &[("StackName", stack_name)]).await;
        let reported = opt_element(&described, "StackStatus").unwrap_or_default();
        if reported == "DELETE_COMPLETE" || reported.is_empty() {
            return;
        }
        assert_ne!(
            reported, "DELETE_FAILED",
            "case {case:?}: expected the stack's resources to be deleted, got {described}"
        );
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the stack was still {reported:?} when the wait ran out"
        );
        tokio::time::sleep(POLL_EVERY).await;
    }
}

/// The public key body a row declares: a PEM document, multi-line and newline-terminated as a
/// client's own key file is, so what is checked is that the body the template declared reaches
/// CloudFront byte for byte rather than as some trimmed rendering of it.
///
/// It is a WHOLE RSA public key — the X.509 `SubjectPublicKeyInfo` of a 2048-bit modulus and
/// the public exponent 65537 — because `PublicKeyConfig$EncodedKey` carries the public half of
/// a key pair and CloudFront decodes the material it is given: a body that is not a readable
/// RSA public key is refused rather than stored, so a template declaring one would provision
/// nothing.
const ENCODED_KEY: &str = "-----BEGIN PUBLIC KEY-----\n\
     MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEAzk60INA65ckmJeCEER8b\n\
     NiMVtsSBRbFSavdPJRMrhhLFiZKbkIFtKUL5UgbvoAaYaTJmpk1b/g0C2RzZwcj4\n\
     q83vq83vq83vq83vq83vq83vq83vq83vq83vq83vq83vq83vq83vq83vq83vq83v\n\
     q83vq83vq83vq83vq83vq83vq83vq83vq83vq83vq83vq83vq83vq83vq83vq83v\n\
     q83vq83vq83vq83vq83vq83vq83vq83vq83vq83vq83vq83vq83vq83vq83vq83v\n\
     q83vq83vq83vq83vq83vq83vq83vq83vq83vq83vq83vq83vq83vq83vq83vq83v\n\
     qwIDAQAB\n\
     -----END PUBLIC KEY-----\n";

/// One row of the table: a template, and what the rule says the resource it declares must then
/// be in CloudFront.
struct Row {
    case: &'static str,
    /// The template body, in the form the row writes it (JSON or YAML), given the name its
    /// one resource is declared under.
    template: fn(&str) -> String,
    /// The `Name` the declaration gives the resource, which is what the row's own reads check
    /// the stored configuration by.
    name: &'static str,
    /// The output keys the template declares. EVERY one of them must carry a value: a stack
    /// CloudFormation reports CREATE_COMPLETE has resolved each output it publishes.
    outputs: &'static [&'static str],
    /// The collection the resource is listed in, as the path of that list operation.
    list_path: &'static str,
    /// The resource's own path, given the id CloudFront minted.
    resource_path: fn(&str) -> String,
    /// `(element, value)` pairs the read's answer must report: the declaration reaching
    /// CloudFront.
    reported: &'static [(&'static str, &'static str)],
}

/// The cache-policy row, in JSON: one custom cache policy whose configuration carries the
/// three behaviour structures and the header list CloudFront's model wraps in a
/// `{Quantity, Items}` container.
fn cache_policy_template(name: &str) -> String {
    serde_json::json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            LOGICAL_ID: {
                "Type": "AWS::CloudFront::CachePolicy",
                "Properties": {"CachePolicyConfig": {
                    "Name": name,
                    "Comment": "the policy the stack declares",
                    "MinTTL": 1,
                    "DefaultTTL": 50,
                    "MaxTTL": 100,
                    "ParametersInCacheKeyAndForwardedToOrigin": {
                        "EnableAcceptEncodingGzip": true,
                        "EnableAcceptEncodingBrotli": false,
                        "HeadersConfig": {
                            "HeaderBehavior": "whitelist",
                            "Headers": ["Accept", "Authorization"]
                        },
                        "CookiesConfig": {
                            "CookieBehavior": "whitelist",
                            "Cookies": ["session"]
                        },
                        "QueryStringsConfig": {"QueryStringBehavior": "none"}
                    }
                }}
            }
        },
        "Outputs": {
            "PolicyId": {"Value": {"Ref": LOGICAL_ID}},
            "PolicyIdAtt": {"Value": {"Fn::GetAtt": [LOGICAL_ID, "Id"]}}
        }
    })
    .to_string()
}

/// The public-key row, in YAML: one public key whose encoded body is the PEM above.
fn public_key_template(name: &str) -> String {
    [
        "AWSTemplateFormatVersion: '2010-09-09'",
        "Resources:",
        &format!("  {LOGICAL_ID}:"),
        "    Type: AWS::CloudFront::PublicKey",
        "    Properties:",
        "      PublicKeyConfig:",
        &format!("        Name: {name}"),
        &format!("        CallerReference: {name}-reference"),
        "        Comment: the key the stack declares",
        "        EncodedKey: |",
        // A YAML literal block keeps every newline of the body and ends it with one, which is
        // how a client writes a PEM into a template.
        &ENCODED_KEY
            .lines()
            .map(|line| format!("          {line}"))
            .collect::<Vec<String>>()
            .join("\n"),
        "Outputs:",
        "  KeyId:",
        &format!("    Value: !Ref {LOGICAL_ID}"),
        "  KeyIdAtt:",
        &format!("    Value: !GetAtt {LOGICAL_ID}.Id"),
    ]
    .join("\n")
}

/// The key-group row, in JSON: a public key and the group that holds it, so the group's own
/// `Items` names the key by the id CloudFront minted for it.
fn key_group_template(name: &str) -> String {
    serde_json::json!({
        "Resources": {
            "TheKey": {
                "Type": "AWS::CloudFront::PublicKey",
                "Properties": {"PublicKeyConfig": {
                    "Name": format!("{name}-key"),
                    "CallerReference": format!("{name}-key-reference"),
                    "EncodedKey": ENCODED_KEY
                }}
            },
            LOGICAL_ID: {
                "Type": "AWS::CloudFront::KeyGroup",
                "Properties": {"KeyGroupConfig": {
                    "Name": name,
                    "Comment": "the group the stack declares",
                    "Items": [{"Ref": "TheKey"}]
                }}
            }
        },
        "Outputs": {
            "GroupId": {"Value": {"Ref": LOGICAL_ID}},
            "GroupIdAtt": {"Value": {"Fn::GetAtt": [LOGICAL_ID, "Id"]}},
            "KeyId": {"Value": {"Ref": "TheKey"}}
        }
    })
    .to_string()
}

/// The table: one row per resource type, each spanning a different shape of declaration.
const ROWS: &[Row] = &[
    Row {
        case: "a custom cache policy, declared in JSON",
        template: cache_policy_template,
        name: "stack-cache-policy",
        outputs: &["PolicyId", "PolicyIdAtt"],
        list_path: "/2020-05-31/cache-policy?Type=custom",
        resource_path: cache_policy_path,
        reported: &[
            ("Name", "stack-cache-policy"),
            ("Comment", "the policy the stack declares"),
            ("MinTTL", "1"),
            ("DefaultTTL", "50"),
            ("MaxTTL", "100"),
            ("HeaderBehavior", "whitelist"),
            ("CookieBehavior", "whitelist"),
            ("QueryStringBehavior", "none"),
        ],
    },
    Row {
        case: "a public key, declared in YAML",
        template: public_key_template,
        name: "stack-public-key",
        outputs: &["KeyId", "KeyIdAtt"],
        list_path: "/2020-05-31/public-key",
        resource_path: public_key_path,
        reported: &[
            ("Name", "stack-public-key"),
            ("Comment", "the key the stack declares"),
            ("EncodedKey", ENCODED_KEY),
        ],
    },
    Row {
        case: "a key group holding the stack's own public key",
        template: key_group_template,
        name: "stack-key-group",
        outputs: &["GroupId", "GroupIdAtt", "KeyId"],
        list_path: "/2020-05-31/key-group",
        resource_path: key_group_path,
        reported: &[
            ("Name", "stack-key-group"),
            ("Comment", "the group the stack declares"),
        ],
    },
];

fn cache_policy_path(id: &str) -> String {
    format!("/2020-05-31/cache-policy/{id}")
}

fn public_key_path(id: &str) -> String {
    format!("/2020-05-31/public-key/{id}")
}

fn key_group_path(id: &str) -> String {
    format!("/2020-05-31/key-group/{id}")
}

/// RULE: the resource a stack declares of one of these types is created in CloudFront itself —
/// listed there, readable there under the id every output of the stack reports, and taken away
/// when the stack is deleted.
#[tokio::test]
async fn a_declared_cloudfront_resource_is_created_in_cloudfront_itself() {
    for row in ROWS {
        let case = row.case;
        let router = build_router();
        let stack_name = format!("cloudfront-{}", row.name);
        let (status, created) = cloudformation(
            &router,
            "CreateStack",
            &[
                ("StackName", &stack_name),
                ("TemplateBody", &(row.template)(row.name)),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {created}"
        );
        let described = settled_stack(&router, &stack_name, case).await;
        assert_eq!(
            opt_element(&described, "StackStatus").as_deref(),
            Some("CREATE_COMPLETE"),
            "case {case:?}: expected the stack to reach CREATE_COMPLETE, got {described}"
        );

        // Every output the template declares carries a VALUE. `outputs_of` reads the pair out
        // of the entry that carries both, so an entry serialized without its `OutputValue`
        // contributes nothing and is reported here as a missing key.
        let outputs = outputs_of(&described);
        for key in row.outputs {
            assert!(
                outputs.contains_key(*key),
                "case {case:?}: expected the output {key:?} to carry a value, got \
                 {outputs:?} in {described}"
            );
        }
        // And every output over the resource's identity reports ONE value.
        let identity = outputs
            .get(row.outputs[0])
            .expect("the identity output carries a value")
            .clone();
        for key in &row.outputs[..2] {
            assert_eq!(
                outputs.get(*key),
                Some(&identity),
                "case {case:?}: expected the output {key:?} to be the id {identity:?}, got \
                 {outputs:?}"
            );
        }

        // CloudFront LISTS the resource, under the very id the stack published.
        let (status, listed) = cloudfront(&router, row.list_path).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from the listing of {}, got {status} with body \
             {listed}",
            row.list_path
        );
        assert!(
            elements(&listed, "Id").contains(&identity),
            "case {case:?}: expected the listing to report the id {identity:?} the stack \
             published, got {listed}"
        );

        // … and resolves it to the configuration the template declared.
        let (status, read) = cloudfront(&router, &(row.resource_path)(&identity)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 reading the id the stack published, got {status} \
             with body {read}"
        );
        for (member, expected) in row.reported {
            assert_eq!(
                elements(&read, member).first().map(String::as_str),
                Some(*expected),
                "case {case:?}: expected {member} {expected:?}, got {read}"
            );
        }

        // Deleting the stack takes the resource away in CloudFront.
        let (status, deleted) =
            cloudformation(&router, "DeleteStack", &[("StackName", &stack_name)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DeleteStack, got {status} with body {deleted}"
        );
        deleted_stack(&router, &stack_name, case).await;
        let (status, gone) = cloudfront(&router, &(row.resource_path)(&identity)).await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected the deleted stack to leave no resource, got {status} with \
             body {gone}"
        );
    }
}

/// RULE, from the other side: a resource of one of these types that names ANOTHER of them is
/// wired to the identity CloudFront minted for it — so the key group a stack deploys beside its
/// public key holds the key CloudFront really has, rather than an id nothing resolves.
#[tokio::test]
async fn a_key_group_holds_the_minted_id_of_the_stacks_own_public_key() {
    let case = "a key group over the stack's own key";
    let router = build_router();
    let stack_name = "cloudfront-key-group-pair";
    let (status, created) = cloudformation(
        &router,
        "CreateStack",
        &[
            ("StackName", stack_name),
            ("TemplateBody", &key_group_template("paired-key-group")),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {created}"
    );
    let described = settled_stack(&router, stack_name, case).await;
    assert_eq!(
        opt_element(&described, "StackStatus").as_deref(),
        Some("CREATE_COMPLETE"),
        "case {case:?}: expected the stack to reach CREATE_COMPLETE, got {described}"
    );
    let outputs = outputs_of(&described);
    let group_id = outputs
        .get("GroupId")
        .unwrap_or_else(|| panic!("case {case:?}: expected a GroupId output, got {outputs:?}"))
        .clone();
    let key_id = outputs
        .get("KeyId")
        .unwrap_or_else(|| panic!("case {case:?}: expected a KeyId output, got {outputs:?}"))
        .clone();

    let (status, group) = cloudfront(&router, &key_group_path(&group_id)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from GetKeyGroup on the id the stack published, got \
         {status} with body {group}"
    );
    assert_eq!(
        elements(&element(&group, "KeyGroupConfig"), "PublicKey"),
        vec![key_id.clone()],
        "case {case:?}: expected the group to hold the key id {key_id:?} the stack published, \
         got {group}"
    );
    // And that id is one CloudFront itself resolves to the key the template declared.
    let (status, key) = cloudfront(&router, &public_key_path(&key_id)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from GetPublicKey on the group's key, got {status} \
         with body {key}"
    );
    assert_eq!(
        elements(&key, "EncodedKey").first().map(String::as_str),
        Some(ENCODED_KEY),
        "case {case:?}: expected the declared key body, got {key}"
    );
}
