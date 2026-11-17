// SPDX-License-Identifier: CC0-1.0

//! What S3 Control accepts as a policy document, how it scopes an access point
//! listing to a bucket, and how it answers a bucket it cannot address — all
//! driven end to end over HTTP through the same router a client meets.
//!
//! The rules under test:
//!
//! * a member documented as a policy carries a document in the IAM JSON policy
//!   language, so a text whose first byte does not open a JSON object, or that
//!   is not JSON at all, is refused `MalformedPolicy` with the one sentence AWS
//!   words that clause, and a document carrying a member name the language does
//!   not define — at its own level or inside a statement — is refused
//!   `MalformedPolicy` naming that member, as is one that carries none of the
//!   element the language requires or declares no version the language defines;
//!   an action element the language has no reading for leaves the document with
//!   no policy syntax to read, and one that reads as an action but names none
//!   the S3 APIs publish is refused as an invalid action, naming the offending
//!   token; a refused put stores nothing, so the document already on the access
//!   point is what the next read answers, byte for byte;
//! * an access point that carries no policy is reported as such naming the
//!   access point the request addressed;
//! * an access point is a resource OF a bucket, so a listing scoped to a bucket
//!   the store that owns buckets does not hold is refused `InvalidRequest`:
//!   there is no bucket whose access points could be listed (a bucket that IS
//!   held and carries none is an empty page, pinned in the bucket-scope suite
//!   beside this one);
//! * the bucket routes address an S3 on Outposts bucket in one of the two forms
//!   they document — the bucket's ARN in the label, or its name together with
//!   the `x-amz-outpost-id` header the REST API requires — so a request in
//!   neither form addresses a URI this service cannot parse, refused before the
//!   request's body is read at all, naming the path the caller sent.
//!
//! Each table's rows differ along what its rule generalises over: the JSON type
//! and the first byte of the submitted text, which member name is undefined and
//! at which level it sits, the bucket a listing is scoped to, and the bucket
//! sub-resource and HTTP method a bucket request addresses.
//!
//! The harness is the lifecycle suite's: the same router, request builder and
//! XML readers a client's request meets.

use crate::access_point_lifecycle::{xml_escape, Emulator, Reply, ACCOUNT, XMLNS};

/// The wire `<Code>` every refusal of a policy document carries.
const MALFORMED_POLICY: &str = "MalformedPolicy";

/// The sentence AWS words a text that is not a JSON policy object with, read
/// from the one place the workspace states the policy language's clauses.
const NOT_A_POLICY: &str = service_common::iam_policy::NOT_A_JSON_POLICY_MESSAGE;

/// The sentence AWS words a document declaring no version the policy language
/// defines with.
const BAD_VERSION: &str = service_common::iam_policy::INVALID_POLICY_VERSION_MESSAGE;

/// The sentence AWS words a document naming an action no S3 API publishes with.
const BAD_ACTION: &str = service_common::iam_policy::POLICY_INVALID_ACTION_MESSAGE;

/// The sentence AWS words a document whose elements the policy language has no
/// reading for with, read from the surface that states this service's clauses.
const SYNTAX_INVALID: &str = service_com_amazonaws_s3control_awss3controlservicev20180820::state::access_point_policy_actions::POLICY_SYNTAX_INVALID_MESSAGE;

/// A document the language defines every member of, used wherever a row needs a
/// policy that is simply accepted.
///
/// Its resource names [`ACCESS_POINT`], the access point every suite below
/// attaches it to — a policy applies to the access point it is attached to. The
/// account and Region fields of the ARN are not read: the request already names
/// the account and is signed for the Region.
const GOOD_POLICY: &str = r#"{"Version":"2012-10-17","Statement":[{"Sid":"read","Effect":"Allow","Principal":{"AWS":"arn:aws:iam::123456789012:root"},"Action":"s3:GetObject","Resource":"arn:aws:s3:us-west-2:123456789012:accesspoint/grammar-ap/object/*","Condition":{"StringEquals":{"aws:PrincipalAccount":"123456789012"}}}]}"#;

/// The access point the documents of these suites are attached to.
const ACCESS_POINT: &str = "grammar-ap";

/// Create one access point fronting `bucket`, with every public-access setting
/// spelled off so this suite stays about the document rather than about
/// publicness.
async fn create(emulator: &Emulator, name: &str, bucket: &str) {
    // An access point is created only over a bucket that exists.
    emulator.provision_bucket(name, bucket);
    let body = format!(
        "<CreateAccessPointRequest xmlns=\"{XMLNS}\"><Bucket>{}</Bucket>\
         <PublicAccessBlockConfiguration><BlockPublicAcls>false</BlockPublicAcls>\
         <IgnorePublicAcls>false</IgnorePublicAcls><BlockPublicPolicy>false</BlockPublicPolicy>\
         <RestrictPublicBuckets>false</RestrictPublicBuckets>\
         </PublicAccessBlockConfiguration></CreateAccessPointRequest>",
        xml_escape(bucket)
    );
    let created = emulator
        .call(
            "PUT",
            &format!("/v20180820/accesspoint/{name}"),
            ACCOUNT,
            &body,
        )
        .await;
    assert_eq!(
        created.status, 200,
        "expected the access point {name:?} to be created, got {} with body {}",
        created.status, created.body
    );
}

async fn put_policy(emulator: &Emulator, name: &str, policy: &str) -> Reply {
    let body = format!(
        "<PutAccessPointPolicyRequest xmlns=\"{XMLNS}\"><Policy>{}</Policy>\
         </PutAccessPointPolicyRequest>",
        xml_escape(policy)
    );
    emulator
        .call(
            "PUT",
            &format!("/v20180820/accesspoint/{name}/policy"),
            ACCOUNT,
            &body,
        )
        .await
}

async fn get_policy(emulator: &Emulator, name: &str) -> Reply {
    emulator
        .call(
            "GET",
            &format!("/v20180820/accesspoint/{name}/policy"),
            ACCOUNT,
            "",
        )
        .await
}

/// Assert `reply` names the request it answers, which every refusal of this
/// service carries beside the failure itself.
fn assert_names_the_request(case: &str, reply: &Reply) {
    for element in ["RequestId", "HostId"] {
        let value = reply.text(element).unwrap_or_default();
        assert!(
            !value.is_empty(),
            "case {case:?}: expected the refusal to carry a <{element}>, got body {}",
            reply.body
        );
    }
}

/// One submitted text and the refusal it is owed, or `None` for a text that is a
/// policy document.
struct Row {
    case: &'static str,
    document: &'static str,
    /// The message the refusal states, or `None` when the document is stored.
    refused: Option<String>,
}

fn rows() -> Vec<Row> {
    let unknown = |name: &str| {
        Some(
            service_common::iam_policy::AttachedPolicyDefect::UnknownElement(name.to_string())
                .message(),
        )
    };
    let missing = |name: &'static str| {
        Some(service_common::iam_policy::AttachedPolicyDefect::MissingElement(name).message())
    };
    vec![
        Row {
            case: "a document the language defines every member of",
            document: GOOD_POLICY,
            refused: None,
        },
        Row {
            case: "the shortest document the language describes",
            document: r#"{"Version":"2012-10-17","Statement":[]}"#,
            refused: None,
        },
        Row {
            case: "one statement written as the object itself",
            document: r#"{"Version":"2012-10-17","Statement":{"Effect":"Deny","NotAction":"s3:*","NotPrincipal":"*"}}"#,
            refused: None,
        },
        Row {
            case: "a document carrying the optional policy id and the legacy version",
            document: r#"{"Id":"ap-policy","Version":"2008-10-17","Statement":[{"Effect":"Deny"}]}"#,
            refused: None,
        },
        // Every row below varies the FIRST BYTE and the JSON type of the text:
        // the clause is about what the text opens with, not about what a parser
        // makes of it. A text whose first byte is WHITESPACE is not a row — the
        // protocol's own reader trims an element's surrounding whitespace, so
        // no such text reaches the handler over the wire.
        Row {
            case: "text that is not JSON at all",
            document: "this is not a policy",
            refused: Some(NOT_A_POLICY.to_string()),
        },
        Row {
            case: "an empty text",
            document: "",
            refused: Some(NOT_A_POLICY.to_string()),
        },
        Row {
            case: "a JSON array of statements with no document around them",
            document: r#"[{"Effect":"Deny"}]"#,
            refused: Some(NOT_A_POLICY.to_string()),
        },
        Row {
            case: "a bare JSON number",
            document: "5",
            refused: Some(NOT_A_POLICY.to_string()),
        },
        Row {
            case: "a bare JSON string",
            document: "\"hello\"",
            refused: Some(NOT_A_POLICY.to_string()),
        },
        Row {
            case: "a bare JSON null",
            document: "null",
            refused: Some(NOT_A_POLICY.to_string()),
        },
        Row {
            case: "a bare JSON boolean",
            document: "true",
            refused: Some(NOT_A_POLICY.to_string()),
        },
        Row {
            case: "a truncated object",
            document: r#"{"Statement":[]"#,
            refused: Some(NOT_A_POLICY.to_string()),
        },
        // Every row below varies WHICH undefined member name is carried and at
        // which level of the document it sits.
        Row {
            case: "an undefined member at the document's own level",
            document: r#"{"hello":"world","Version":"2012-10-17","Statement":[]}"#,
            refused: unknown("hello"),
        },
        Row {
            case: "a statement element written at the document's level",
            document: r#"{"Effect":"Allow","Version":"2012-10-17","Statement":[]}"#,
            refused: unknown("Effect"),
        },
        Row {
            case: "a misspelled statement list",
            document: r#"{"Statements":[]}"#,
            refused: unknown("Statements"),
        },
        Row {
            case: "an undefined member inside the one statement",
            document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Deny","Comment":"why"}]}"#,
            refused: unknown("Comment"),
        },
        Row {
            case: "an undefined member inside the second statement",
            document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Deny"},{"Effect":"Allow","Actions":"s3:*"}]}"#,
            refused: unknown("Actions"),
        },
        Row {
            case: "an undefined member in a statement written as the object itself",
            document: r#"{"Version":"2012-10-17","Statement":{"Effect":"Deny","Resources":"*"}}"#,
            refused: unknown("Resources"),
        },
        Row {
            case: "a document element written inside a statement",
            document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Deny","Version":"2012-10-17"}]}"#,
            refused: unknown("Version"),
        },
        Row {
            case: "arbitrary JSON nested under undefined members",
            document: r#"{"a":{"b":{"c":[1,2,{"d":[[],[{}]]}]}}}"#,
            refused: unknown("a"),
        },
        // Every row below carries only names the language defines but not the
        // one it requires.
        Row {
            case: "an object declaring nothing at all",
            document: "{}",
            refused: missing("Statement"),
        },
        Row {
            case: "a document declaring only a version",
            document: r#"{"Version":"2012-10-17"}"#,
            refused: missing("Statement"),
        },
        Row {
            case: "a document declaring a version and an id but no statement",
            document: r#"{"Version":"2012-10-17","Id":"ap-policy"}"#,
            refused: missing("Statement"),
        },
        // Every row below carries the required element but declares no version
        // string the language defines: one per way a version can fail to be one
        // — absent, a date the language does not define, the right date written
        // differently, and a value that is not a string at all.
        Row {
            case: "a document declaring no version",
            document: r#"{"Statement":[{"Effect":"Deny"}]}"#,
            refused: Some(BAD_VERSION.to_string()),
        },
        Row {
            case: "a version date the language does not define",
            document: r#"{"Version":"2024-01-01","Statement":[{"Effect":"Deny"}]}"#,
            refused: Some(BAD_VERSION.to_string()),
        },
        Row {
            case: "the current version written without its separators",
            document: r#"{"Version":"20121017","Statement":[{"Effect":"Deny"}]}"#,
            refused: Some(BAD_VERSION.to_string()),
        },
        Row {
            case: "an empty version",
            document: r#"{"Version":"","Statement":[{"Effect":"Deny"}]}"#,
            refused: Some(BAD_VERSION.to_string()),
        },
        Row {
            case: "a version that is not a string",
            document: r#"{"Version":2012,"Statement":[{"Effect":"Deny"}]}"#,
            refused: Some(BAD_VERSION.to_string()),
        },
        // The action clause has a table of its own below, because its two
        // readings earn two different refusals. Here are the forms the language
        // and this surface DO admit: the wildcards an action name may carry, and
        // the published actions of the S3 namespaces beside `s3` itself.
        Row {
            case: "the wildcard that stands for every action",
            document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"*"}]}"#,
            refused: None,
        },
        Row {
            case: "an action name carrying a wildcard",
            document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["s3:Get*","s3express:*"]}]}"#,
            refused: None,
        },
        Row {
            case: "an action of another namespace of the S3 family",
            document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"s3-object-lambda:GetObject"}]}"#,
            refused: None,
        },
        Row {
            case: "a published action of each namespace of the family",
            document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["s3:ListBucket","s3-outposts:PutObject","s3express:CreateSession","s3-object-lambda:WriteGetObjectResponse"]}]}"#,
            refused: None,
        },
    ]
}

/// One action element and the refusal the document carrying it earns.
struct ActionRow {
    case: &'static str,
    /// The action element, written as the statement member it is in the
    /// document — the element's name and its JSON value.
    element: &'static str,
    /// The sentence the refusal states.
    message: &'static str,
    /// The action the refusal names beside its message, or `None` for a refusal
    /// that names none.
    detail: Option<&'static str>,
}

/// A one-statement document whose statement carries `element` beside the effect
/// and principal every statement of this suite states.
fn action_policy(element: &str) -> String {
    format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Principal":"*",{element}}}]}}"#
    )
}

/// Every way an action element earns a refusal, with the refusal it earns.
///
/// Two readings, so two sentences: a value the policy language has no reading
/// for leaves the document with no policy syntax to interpret, while a value
/// that DOES read as an action but names none the S3 APIs publish is an invalid
/// action, reported with the offending token. The rows of each differ along what
/// its reading generalises over — the JSON form the element takes and the
/// characters the token is written in, and how a readable token fails to name a
/// published action.
fn action_rows() -> Vec<ActionRow> {
    let unreadable = |case, element| ActionRow {
        case,
        element,
        message: SYNTAX_INVALID,
        detail: None,
    };
    let unpublished = |case, element, detail| ActionRow {
        case,
        element,
        message: BAD_ACTION,
        detail: Some(detail),
    };
    vec![
        unreadable(
            "an action naming no service namespace",
            r#""Action":"GetObject""#,
        ),
        unreadable(
            "an action naming a namespace and no action name",
            r#""Action":"s3:""#,
        ),
        unreadable(
            "an action name carrying a space",
            r#""Action":"s3:Get Object""#,
        ),
        unreadable(
            "one readable action beside one that is not, in a list",
            r#""Action":["s3:GetObject","nope"]"#,
        ),
        unreadable("an action that is not a string", r#""Action":5"#),
        unreadable("an action element naming nothing at all", r#""Action":[]"#),
        unreadable(
            "an action written as an ARN rather than as an action",
            r#""Action":"arn:aws:s3:::b""#,
        ),
        unreadable(
            "the complement of the action element, unreadable",
            r#""NotAction":"nope""#,
        ),
        unpublished(
            "an action name no S3 API publishes",
            r#""Action":"s3:Frobnicate""#,
            "s3:Frobnicate",
        ),
        unpublished(
            "a misspelling of a published action name",
            r#""Action":"s3:GetObjectt""#,
            "s3:GetObjectt",
        ),
        unpublished(
            "a wildcard matching no published action name",
            r#""Action":"s3:Zz*""#,
            "s3:Zz*",
        ),
        unpublished(
            "an action of another service's namespace",
            r#""Action":"ec2:RunInstances""#,
            "ec2:RunInstances",
        ),
        unpublished(
            "an action name no API of the family publishes",
            r#""Action":"s3express:Frobnicate""#,
            "s3express:Frobnicate",
        ),
        unpublished(
            "the first unpublished action of a list",
            r#""Action":["s3:GetObject","s3-outposts:Frobnicate"]"#,
            "s3-outposts:Frobnicate",
        ),
        unpublished(
            "an unpublished action in the complement of the action element",
            r#""NotAction":"s3:Frobnicate""#,
            "s3:Frobnicate",
        ),
    ]
}

#[tokio::test]
async fn an_action_element_is_read_for_its_form_and_for_the_actions_it_names() {
    let emulator = Emulator::start();
    create(&emulator, ACCESS_POINT, "reports").await;
    // A document already on the access point, so every refusal below is shown to
    // have stored nothing: the access point keeps the policy it carried.
    let stored = put_policy(&emulator, ACCESS_POINT, GOOD_POLICY).await;
    assert_eq!(
        stored.status,
        service_com_amazonaws_s3control_awss3controlservicev20180820::state::NO_CONTENT_SUCCESS_STATUS,
        "expected the first document to be stored, got {} with body {}",
        stored.status,
        stored.body
    );

    for row in action_rows() {
        let document = action_policy(row.element);
        let put = put_policy(&emulator, ACCESS_POINT, &document).await;
        assert_eq!(
            (put.status, put.code().as_str(), put.text("Message")),
            (400, MALFORMED_POLICY, Some(row.message.to_string())),
            "case {:?}: expected 400 {MALFORMED_POLICY} stating {:?}, got {} with body {}",
            row.case,
            row.message,
            put.status,
            put.body
        );
        assert_eq!(
            put.text("Detail").as_deref(),
            row.detail,
            "case {:?}: expected the refusal to name {:?}, got body {}",
            row.case,
            row.detail,
            put.body
        );
        assert_names_the_request(row.case, &put);

        let read = get_policy(&emulator, ACCESS_POINT).await;
        assert_eq!(
            read.text("Policy").as_deref(),
            Some(GOOD_POLICY),
            "case {:?}: expected the refused put to store nothing, got body {}",
            row.case,
            read.body
        );
    }
}

#[tokio::test]
async fn a_policy_document_is_held_to_the_policy_language_before_it_is_stored() {
    let emulator = Emulator::start();
    create(&emulator, ACCESS_POINT, "reports").await;

    // The document on the access point at each point in the table: a refused put
    // stores nothing, so it stays what the last ACCEPTED put left.
    let mut stored: Option<&str> = None;

    for row in rows() {
        let put = put_policy(&emulator, ACCESS_POINT, row.document).await;
        match &row.refused {
            None => {
                assert_eq!(
                    put.status,
                    service_com_amazonaws_s3control_awss3controlservicev20180820::state::NO_CONTENT_SUCCESS_STATUS,
                    "case {:?}: expected the document to be stored, got {} with body {}",
                    row.case, put.status, put.body
                );
                stored = Some(row.document);
            }
            Some(message) => {
                assert_eq!(
                    (put.status, put.code().as_str(), put.text("Message")),
                    (400, MALFORMED_POLICY, Some(message.clone())),
                    "case {:?}: expected 400 {MALFORMED_POLICY} stating {message:?}, got {} with \
                     body {}",
                    row.case,
                    put.status,
                    put.body
                );
                assert_names_the_request(row.case, &put);
            }
        }

        // Whatever the put answered, what the access point carries now is the
        // document the last accepted put left, byte for byte.
        let read = get_policy(&emulator, ACCESS_POINT).await;
        match stored {
            None => assert_eq!(
                (read.status, read.code().as_str()),
                (404, "NoSuchAccessPointPolicy"),
                "case {:?}: expected no policy to be stored yet, got {} with body {}",
                row.case,
                read.status,
                read.body
            ),
            Some(document) => assert_eq!(
                read.text("Policy").as_deref(),
                Some(document),
                "case {:?}: expected the stored document back byte for byte, got body {}",
                row.case,
                read.body
            ),
        }
    }
}

#[tokio::test]
async fn an_access_point_carrying_no_policy_is_reported_naming_that_access_point() {
    let emulator = Emulator::start();
    // One row per name length the model admits at its floor and above it: the
    // answer names whichever access point the request addressed.
    for name in ["abc", "no-policy-ap"] {
        create(&emulator, name, "reports").await;
        for (call, uri) in [
            ("GetAccessPointPolicy", format!("/v20180820/accesspoint/{name}/policy")),
            (
                "GetAccessPointPolicyStatus",
                format!("/v20180820/accesspoint/{name}/policyStatus"),
            ),
        ] {
            let read = emulator.call("GET", &uri, ACCOUNT, "").await;
            assert_eq!(
                (read.status, read.code().as_str()),
                (404, "NoSuchAccessPointPolicy"),
                "case {name:?}: expected {call} to report no policy, got {} with body {}",
                read.status,
                read.body
            );
            assert_eq!(
                read.text("AccessPointName").as_deref(),
                Some(name),
                "case {name:?}: expected {call}'s refusal to name the access point, got body {}",
                read.body
            );
            assert_names_the_request(name, &read);
        }
    }
}

#[tokio::test]
async fn a_listing_scoped_to_a_bucket_that_is_not_there_is_refused() {
    let emulator = Emulator::start();
    create(&emulator, "reports-ap", "reports").await;
    create(&emulator, "finance-ap", "finance").await;

    // One row per bucket: two the store holds and an access point fronts, and
    // four it does not hold at all — a bucket name nothing created, one that is
    // a PREFIX of a held one, one that EXTENDS a held one, and one differing
    // only in case. A bucket that is HELD and carries no access point is an
    // empty page rather than this refusal, which the bucket-scope suite pins.
    for (case, bucket, attached) in [
        ("a bucket one access point fronts", "reports", true),
        ("another bucket one access point fronts", "finance", true),
        ("a bucket the store does not hold", "archive", false),
        ("a prefix of a bucket that is held", "report", false),
        ("an extension of a bucket that is held", "reportsx", false),
        ("a held bucket's name in another case", "Reports", false),
    ] {
        let listed = emulator
            .call(
                "GET",
                &format!("/v20180820/accesspoint?bucket={bucket}"),
                ACCOUNT,
                "",
            )
            .await;
        if attached {
            assert_eq!(
                listed.status, 200,
                "case {case:?}: expected the scoped listing to answer 200, got {} with body {}",
                listed.status, listed.body
            );
            assert_eq!(
                listed.texts("Bucket"),
                vec![bucket.to_string()],
                "case {case:?}: expected only that bucket's access points, got body {}",
                listed.body
            );
            continue;
        }
        assert_eq!(
            (
                listed.status,
                listed.code().as_str(),
                listed.text("Message")
            ),
            (
                400,
                "InvalidRequest",
                Some(
                    service_com_amazonaws_s3control_awss3controlservicev20180820::state::NO_SUCH_BUCKET_TO_SCOPE_BY_MESSAGE
                        .to_string()
                )
            ),
            "case {case:?}: expected the bucket-scoping refusal, got {} with body {}",
            listed.status,
            listed.body
        );
        assert_names_the_request(case, &listed);
    }

    // The refusals changed nothing: the unscoped listing still reports both
    // access points.
    let all = emulator.call("GET", "/v20180820/accesspoint", ACCOUNT, "").await;
    assert_eq!(
        all.texts("Name"),
        vec!["finance-ap".to_string(), "reports-ap".to_string()],
        "expected both access points after the refusals, got body {}",
        all.body
    );
}

/// An S3 on Outposts bucket ARN, URL-encoded as an SDK writes it into the one
/// path segment the `Bucket` label occupies.
fn encoded_outposts_arn(bucket: &str) -> String {
    format!(
        "arn%3Aaws%3As3-outposts%3Aus-west-2%3A{ACCOUNT}%3Aoutpost%2Fop-0123456789abcdef%2Fbucket%2F{bucket}"
    )
}

/// Send one bucket request, optionally carrying the `x-amz-outpost-id` header
/// the REST API addressing form puts the Outpost in.
///
/// Written here rather than taken from the lifecycle suite's `call` because that
/// builder carries a fixed header set, and the header is exactly what this
/// suite's rows vary.
async fn call_bucket(
    emulator: &Emulator,
    method: &str,
    path: &str,
    outpost_id: Option<&str>,
) -> Reply {
    let mut request = ::http::Request::builder()
        .method(method)
        .uri(path)
        .header(
            "host",
            format!("{ACCOUNT}.s3-control.us-west-2.amazonaws.com"),
        )
        .header(
            "authorization",
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20240101/us-west-2/s3/aws4_request, \
             SignedHeaders=host, Signature=abc",
        )
        .header("x-amz-account-id", ACCOUNT)
        .header("content-type", "application/xml");
    if let Some(outpost_id) = outpost_id {
        request = request.header("x-amz-outpost-id", outpost_id);
    }
    let response = emulator
        .router
        .dispatch(
            request
                .body(::bytes::Bytes::new())
                .expect("build the request"),
        )
        .await;
    Reply {
        status: response.status().as_u16(),
        headers: response
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_ascii_lowercase(),
                    String::from_utf8_lossy(value.as_bytes()).to_string(),
                )
            })
            .collect(),
        body: String::from_utf8(response.body().to_vec()).expect("a UTF-8 response body"),
    }
}

#[tokio::test]
async fn a_bucket_this_service_cannot_address_is_refused_before_the_body_is_read() {
    let emulator = Emulator::start();

    // One row per bucket route the model binds, except `PUT /v20180820/bucket/
    // {Bucket}`: that is `CreateBucket`, whose label the model documents as "The
    // name of the bucket" with the Outpost in its own `OutpostId` member, so a
    // plain name there is the documented form and is asserted separately below.
    let routes: [(&str, &str); 7] = [
        ("GET", ""),
        ("DELETE", ""),
        ("PUT", "/replication"),
        ("GET", "/policy"),
        ("DELETE", "/tagging"),
        ("PUT", "/versioning"),
        ("GET", "/lifecycleconfiguration"),
    ];
    // One row per way a label can fail to name an Outposts bucket, beside the
    // two ways one that does is written. The Outpost header is absent on every
    // row here — the OTHER documented addressing form is the table below.
    let labels: Vec<(&str, String, bool)> = vec![
        (
            "a plain bucket name with no Outpost header",
            "reports".to_string(),
            false,
        ),
        (
            "a name that merely starts with the ARN prefix",
            "arnold".to_string(),
            false,
        ),
        (
            "an ARN of another vendor",
            "arn%3Aaws%3As3%3A%3A%3Areports".to_string(),
            false,
        ),
        (
            "an Outposts ACCESS POINT ARN, which names no bucket",
            "arn%3Aaws%3As3-outposts%3Aus-west-2%3A123456789012%3Aoutpost%2Fop-1%2Faccesspoint%2Fap"
                .to_string(),
            false,
        ),
        ("an Outposts bucket ARN", encoded_outposts_arn("reports"), true),
        (
            "an Outposts bucket ARN written with its own separators unescaped",
            format!(
                "arn:aws:s3-outposts:us-west-2:{ACCOUNT}:outpost/op-0123456789abcdef/bucket/reports"
            ),
            true,
        ),
    ];

    for (method, sub_resource) in routes {
        for (case, label, addressable) in &labels {
            let path = format!("/v20180820/bucket/{label}{sub_resource}");
            let reply = call_bucket(&emulator, method, &path, None).await;
            if *addressable {
                assert_ne!(
                    reply.code().as_str(),
                    "InvalidURI",
                    "case {case:?} on {method} {path}: expected the address to parse, got body {}",
                    reply.body
                );
                continue;
            }
            assert_eq!(
                (reply.status, reply.code().as_str(), reply.text("Message")),
                (
                    400,
                    "InvalidURI",
                    Some(
                        service_com_amazonaws_s3control_awss3controlservicev20180820::state::INVALID_URI_MESSAGE
                            .to_string()
                    )
                ),
                "case {case:?} on {method} {path}: expected the unparseable-URI refusal, got {} \
                 with body {}",
                reply.status,
                reply.body
            );
            assert_eq!(
                reply.text("URI").as_deref(),
                Some(format!("bucket/{label}{sub_resource}").as_str()),
                "case {case:?} on {method} {path}: expected the refusal to name the path the \
                 caller sent, got body {}",
                reply.body
            );
            assert_names_the_request(case, &reply);
        }
    }

    // The OTHER documented addressing form: the bucket's NAME in the label
    // together with the `x-amz-outpost-id` header the REST API requires. Every
    // route takes it, so none of these is a URI this service could not parse.
    for (method, sub_resource) in routes {
        let path = format!("/v20180820/bucket/reports{sub_resource}");
        let reply = call_bucket(&emulator, method, &path, Some("op-0123456789abcdef")).await;
        assert_ne!(
            reply.code().as_str(),
            "InvalidURI",
            "case \"a name beside the Outpost header\" on {method} {path}: expected the \
             documented REST-API addressing to parse, got body {}",
            reply.body
        );
    }

    // `CreateBucket` takes the bucket's NAME, with or without the Outpost
    // header, so neither form is a URI this service could not parse.
    for outpost_id in [None, Some("op-0123456789abcdef")] {
        let reply = call_bucket(&emulator, "PUT", "/v20180820/bucket/reports", outpost_id).await;
        assert_ne!(
            reply.code().as_str(),
            "InvalidURI",
            "case \"CreateBucket naming the bucket\" (outpost header {outpost_id:?}): expected \
             the documented name addressing to parse, got body {}",
            reply.body
        );
    }

    // The listing of REGIONAL buckets addresses no bucket, so it is not in the
    // space this stage reads at all.
    let listed = call_bucket(&emulator, "GET", "/v20180820/bucket", None).await;
    assert_ne!(
        listed.code().as_str(),
        "InvalidURI",
        "expected the regional-bucket listing to address no bucket, got body {}",
        listed.body
    );
}
