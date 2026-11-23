// SPDX-License-Identifier: CC0-1.0

//! What CloudFront decides about a request's IDENTIFYING argument before it consults the
//! store, end to end over HTTP, on the two surfaces whose refusals the store used to decide
//! first.
//!
//! Every assertion goes through the router, so each case exercises routing, restXml
//! (de)serialization, the handler, the persisted state and serialization — the same path a
//! client takes. One rule, in the two shapes the argument takes:
//!
//! * an operation addressing an Anycast static IP list by its path `Id` decides whether the
//!   value could be an identifier of a namespaced resource AT ALL before it resolves it and
//!   before it evaluates the `If-Match` the mutation runs under: a value of no such form is
//!   the argument the operation cannot read — 400 `InvalidArgument`,
//!   [`id_format_invalid_message`] naming the member it arrived in — whatever the header
//!   carries, while a value of that form the store holds nothing for is the absent list (404
//!   `EntityNotFound`, [`ANYCAST_IP_LIST_NOT_FOUND_MESSAGE`]);
//! * the resource-policy surface decides the resource TYPE from the parsed `ResourceArn`
//!   before it resolves it: an ARN naming any type other than the one the surface attaches
//!   policies to is refused on that TYPE (400 `InvalidArgument`, each operation's own
//!   sentence) identically whether or not this service holds the resource it names, and 404
//!   `EntityNotFound` is reserved for an ARN of the ACCEPTED type that names nothing
//!   ([`NO_SUCH_RESOURCE_MESSAGE`]).
//!
//! The rows differ along what the rule generalises over: for the identifier, the form that
//! makes a value unreadable (an upper-cased namespace, a label too long to be one, a label
//! with no separator, another resource kind's id, a lone separator) crossed with every
//! `If-Match` the mutation can carry (the list's current version, a token of no version shape
//! and a well-formed token never minted), and the parts of a readable identifier nothing
//! constrains (the namespace label, the body's width, its alphabet and the separator); for
//! the ARN, the resource type it names, whether the resource EXISTS (each type is asked about
//! twice, once by the ARN its create minted and once by an ARN of the same type naming
//! nothing), how this emulator holds the resource, and which of the three operations asks.
//!
//! The harness (the router and every request helper) is the lifecycle suites', reused rather
//! than copied.

use service_com_amazonaws_cloudfront_cloudfront2020_05_31::state::{
    id_format_invalid_message, ANYCAST_IP_LIST_NOT_FOUND_MESSAGE, EMPTY_RESOURCE_POLICY_DOCUMENT,
    INVALID_RESOURCE_POLICY_ARN_MESSAGE, INVALID_RESOURCE_POLICY_RESOURCE_TYPE_MESSAGE,
    IP_ADDRESS_TYPE_IPV4, IP_ADDRESS_TYPE_IPV6, MALFORMED_ARN_MESSAGE, NO_SUCH_RESOURCE_MESSAGE,
    RESOURCE_POLICY_RESOURCE_TYPE,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::anycast_ip_list_lifecycle::{create_anycast_ip_list, get_anycast_ip_list, Create};
use crate::anycast_ip_list_update::update_anycast_ip_list;
use crate::distribution_lifecycle::{create_distribution, emulator, Answer, ACCOUNT_ID};
use crate::function_lifecycle::create_key_value_store;
use crate::public_key_lifecycle::{create_key, pem, Key};
use crate::resource_policy_delete::delete_resource_policy;
use crate::resource_policy_lifecycle::{
    created_vpc_origin_arn, distribution_config, get_resource_policy, message, policy,
    put_resource_policy, reported_document, stored,
};

// ---------------------------------------------------------------------------
// The identifier of an Anycast static IP list
// ---------------------------------------------------------------------------

/// Create one Anycast static IP list and report `(Id, ETag)`.
async fn live_list(router: &MultiServiceRouter, name: &str) -> (String, String) {
    let create = Create {
        case: "the list the ordering is asserted around",
        name: name.to_string(),
        ip_count: 3,
        ip_address_type: Some(IP_ADDRESS_TYPE_IPV4),
        ipam_cidr_configs: Vec::new(),
        tags: Vec::new(),
    };
    let answer = create_anycast_ip_list(router, &create.xml()).await;
    assert_eq!(
        answer.status, 202,
        "case {name}: expected HTTP 202 from the create, got {} ({})",
        answer.status, answer.raw
    );
    (
        answer.root().text_at("Id").unwrap_or_default().to_string(),
        answer.header("etag").to_string(),
    )
}

/// The forms a value takes that are no identifier CloudFront writes at all — neither a
/// namespaced one nor one of the entity-identifier alphabet: `(case, value)`.
///
/// `minted` is the id a create really handed out and `name` the list's own name, so the rows
/// are built from this run's record rather than from literals.
fn unreadable(minted: &str, name: &str) -> Vec<(String, String)> {
    vec![
        ("the minted id upper-cased".to_string(), minted.to_uppercase()),
        (
            "the namespace label with no separator".to_string(),
            minted.replacen('-', "", 1),
        ),
        ("the list's own name".to_string(), name.to_string()),
        (
            "a UUID, the shape of a kind whose ids carry no namespace".to_string(),
            "f9a6a0b4-8d39-4a1e-9f67-1b0f7f5d2c3e".to_string(),
        ),
        ("a lone separator".to_string(), "-".to_string()),
        (
            "a namespace label longer than any this service writes".to_string(),
            "anycastiplist-0123456789".to_string(),
        ),
    ]
}

/// The identifiers that COULD be an Anycast static IP list's and that the store holds nothing
/// for: `(case, value)`.
///
/// `Id` is a plain `string` in the model — no pattern and no length — so the namespace label,
/// the body's width, the body's alphabet and the separator are all parts nothing may refuse a
/// value on, and each row varies one of them; the last row carries no namespace at all and is
/// written in the alphabet CloudFront writes an entity identifier in, which is the other form
/// an identifier it issued takes.
fn readable_but_absent() -> Vec<(String, String)> {
    vec![
        (
            "an id never minted".to_string(),
            "ipl-00000000000000000".to_string(),
        ),
        (
            "an id never minted at another width".to_string(),
            "ipl-0123456789abcdef".to_string(),
        ),
        (
            "an id never minted outside the minted alphabet".to_string(),
            "ipl-notaminted-id".to_string(),
        ),
        (
            "an id under another namespace label".to_string(),
            "aip_1A2B3C4D5E6F7G8H9J0K1L".to_string(),
        ),
        (
            "the same label under the other separator".to_string(),
            "ipl_0123456789abcdef0".to_string(),
        ),
        (
            "an id written in the entity-identifier alphabet".to_string(),
            "IAL1A2B3C4D5E6".to_string(),
        ),
    ]
}

/// Every `If-Match` the update can run under for a row about the IDENTIFIER: the list's
/// current version, a value that is no version token at all, and a well-formed token this
/// service never minted. `IfMatch` is a required member, so there is no row without one.
fn if_match_forms(current: &str) -> Vec<(String, String)> {
    vec![
        (
            "the list's current version".to_string(),
            current.to_string(),
        ),
        (
            "a value that is no version token".to_string(),
            "not-a-version".to_string(),
        ),
        (
            "a well-formed token never minted".to_string(),
            "E1PDK09ESLDGHD".to_string(),
        ),
    ]
}

#[tokio::test]
async fn the_anycast_update_reads_its_identifier_before_the_store_and_the_precondition() {
    let router = emulator();
    let name = "update-precedence-list";
    let (id, etag) = live_list(&router, name).await;
    let id_format_invalid = id_format_invalid_message("Id");

    // A value that is no identifier of a namespaced resource is the ARGUMENT failure, under
    // every header the mutation can carry: neither the store nor the precondition can say
    // anything about an identifier that addresses nothing.
    for (case, candidate) in unreadable(&id, name) {
        // The read of the same resource answers the same argument failure for the same value,
        // so the two sides of the surface agree about what an identifier is.
        let read = get_anycast_ip_list(&router, &candidate).await;
        assert_eq!(
            read.status, 400,
            "case {case} read ({candidate:?}): expected HTTP 400, got {} ({})",
            read.status, read.raw
        );
        let actual = read.root().find_text("Message").unwrap_or_default();
        assert_eq!(
            actual, id_format_invalid,
            "case {case} read ({candidate:?}): expected {id_format_invalid:?}, got {actual:?} \
             in {}",
            read.raw
        );

        for (header_case, if_match) in if_match_forms(&etag) {
            let answer = update_anycast_ip_list(
                &router,
                &candidate,
                Some(&if_match),
                Some(IP_ADDRESS_TYPE_IPV6),
                None,
            )
            .await;
            assert_eq!(
                answer.status, 400,
                "case {case}/{header_case} ({candidate:?}): expected HTTP 400, got {} ({})",
                answer.status, answer.raw
            );
            assert_eq!(
                answer.error_code(),
                "InvalidArgument",
                "case {case}/{header_case} ({candidate:?}): expected InvalidArgument, got {:?} \
                 ({})",
                answer.error_code(),
                answer.raw
            );
            let actual = answer.root().find_text("Message").unwrap_or_default();
            assert_eq!(
                actual, id_format_invalid,
                "case {case}/{header_case} ({candidate:?}): expected {id_format_invalid:?}, got \
                 {actual:?} in {}",
                answer.raw
            );
        }
    }

    // An identifier the operation CAN read that no list carries is the ABSENT list — again
    // under every header, since the list is resolved before the precondition it would be
    // checked against.
    for (case, candidate) in readable_but_absent() {
        for (header_case, if_match) in if_match_forms(&etag) {
            let answer = update_anycast_ip_list(
                &router,
                &candidate,
                Some(&if_match),
                Some(IP_ADDRESS_TYPE_IPV6),
                None,
            )
            .await;
            assert_eq!(
                answer.status, 404,
                "case {case}/{header_case} ({candidate:?}): expected HTTP 404, got {} ({})",
                answer.status, answer.raw
            );
            assert_eq!(
                answer.error_code(),
                "EntityNotFound",
                "case {case}/{header_case} ({candidate:?}): expected EntityNotFound, got {:?} \
                 ({})",
                answer.error_code(),
                answer.raw
            );
            let actual = answer.root().find_text("Message").unwrap_or_default();
            assert_eq!(
                actual, ANYCAST_IP_LIST_NOT_FOUND_MESSAGE,
                "case {case}/{header_case} ({candidate:?}): expected \
                 {ANYCAST_IP_LIST_NOT_FOUND_MESSAGE:?}, got {actual:?} in {}",
                answer.raw
            );
        }
    }

    // Every refusal above left the list exactly as it was — the type it was created with,
    // under the token the create minted — and the call the gate must SERVE is still served.
    let read = get_anycast_ip_list(&router, &id).await;
    assert_eq!(
        read.root().text_at("IpAddressType"),
        Some(IP_ADDRESS_TYPE_IPV4),
        "case after the refusals: the list must still run under the type it was created with, \
         got {}",
        read.raw
    );
    assert_eq!(
        read.header("etag"),
        etag,
        "case after the refusals: a refused update must spend no version token, got {}",
        read.raw
    );
    let served = update_anycast_ip_list(
        &router,
        &id,
        Some(&etag),
        Some(IP_ADDRESS_TYPE_IPV6),
        None,
    )
    .await;
    assert_eq!(
        served.status, 200,
        "case the live list at its current version: expected HTTP 200, got {} ({})",
        served.status, served.raw
    );
    assert_eq!(
        served.root().text_at("IpAddressType"),
        Some(IP_ADDRESS_TYPE_IPV6),
        "case the live list at its current version: expected the submitted type, got {}",
        served.raw
    );
}

// ---------------------------------------------------------------------------
// The resource type a ResourceArn names
// ---------------------------------------------------------------------------

/// What the resource-policy surface answers a row with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// The value is no ARN at all, so it is refused on its FORM.
    Malformed,
    /// The ARN names a resource type the surface attaches no policy to.
    WrongType,
    /// The ARN names the accepted type and this service holds no resource it names.
    Absent,
}

impl Outcome {
    /// The HTTP status and error code every operation of the surface answers this outcome
    /// with.
    fn status_and_code(self) -> (u16, &'static str) {
        match self {
            Outcome::Malformed | Outcome::WrongType => (400, "InvalidArgument"),
            Outcome::Absent => (404, "EntityNotFound"),
        }
    }

    /// The sentence `operation` puts on the wire for this outcome: the condition is one for
    /// the whole surface, the wording is each operation's own.
    fn message(self, operation: &str) -> &'static str {
        match (self, operation) {
            (Outcome::Malformed, _) => MALFORMED_ARN_MESSAGE,
            (Outcome::Absent, _) => NO_SUCH_RESOURCE_MESSAGE,
            (Outcome::WrongType, "GetResourcePolicy") => INVALID_RESOURCE_POLICY_ARN_MESSAGE,
            (Outcome::WrongType, _) => INVALID_RESOURCE_POLICY_RESOURCE_TYPE_MESSAGE,
        }
    }
}

/// Ask all three operations of the surface about `arn`, in the order a client that attaches,
/// reads and removes a policy would.
async fn ask_each_operation(router: &MultiServiceRouter, arn: &str) -> Vec<(&'static str, Answer)> {
    vec![
        (
            "PutResourcePolicy",
            put_resource_policy(router, arn, &policy(arn, "precedence-suite")).await,
        ),
        ("GetResourcePolicy", get_resource_policy(router, arn).await),
        (
            "DeleteResourcePolicy",
            delete_resource_policy(router, arn).await,
        ),
    ]
}

/// The same ARN with its resource id replaced, so it names a resource of the SAME type that
/// this service holds none of: the pair differs only in whether the resource EXISTS.
fn same_type_naming_nothing(arn: &str) -> String {
    match arn.rsplit_once('/') {
        Some((head, _)) => format!("{head}/absent-of-this-type"),
        None => format!("{arn}/absent-of-this-type"),
    }
}

/// Upload one public key through the public-key suite's own writer and report the ARN that
/// key is addressed by — a resource kind that mints no ARN of its own, so the ARN is built
/// from the id its routes use.
async fn minted_public_key_arn(router: &MultiServiceRouter, reference: &str) -> String {
    let key = Key {
        case: format!("the public key of {reference}"),
        caller_reference: format!("ref-{reference}"),
        name: reference.to_string(),
        encoded_key: pem(2, 'a'),
        comment: None,
    };
    let answer = create_key(router, &key.xml()).await;
    assert_eq!(
        answer.status, 201,
        "case {reference}: expected HTTP 201 from CreatePublicKey, got {} ({})",
        answer.status, answer.raw
    );
    let id = answer.root().find_text("Id").unwrap_or_default();
    format!("arn:aws:cloudfront::{ACCOUNT_ID}:public-key/{id}")
}

#[tokio::test]
async fn the_resource_policy_surface_judges_the_resource_type_before_the_store() {
    let router = emulator();

    // The policy every refusal below has to leave exactly as it is: the surface accepts this
    // ARN, so no row may write under it or remove what it carries.
    let kept_arn = created_vpc_origin_arn(&router, "vo-precedence-survivor").await;
    let kept = policy(&kept_arn, "survivor");
    let written = put_resource_policy(&router, &kept_arn, &kept).await;
    assert_eq!(
        written.status, 200,
        "case the surviving policy: expected HTTP 200 from the write, got {} ({})",
        written.status, written.raw
    );

    // Resources of types the surface attaches NO policy to, each named by the ARN its own
    // create minted and each differing in how this emulator holds it: a distribution and a
    // key value store mint an ARN of their own, a public key is addressed by the id its
    // routes use.
    let created = create_distribution(&router, &distribution_config("ref-precedence")).await;
    assert_eq!(
        created.status, 201,
        "the distribution must be created for its row to be about its TYPE, got {} ({})",
        created.status, created.raw
    );
    let distribution_arn = created
        .root()
        .find_text("ARN")
        .unwrap_or_default()
        .to_string();
    let key_value_store_arn = create_key_value_store(&router, "precedence-suite-store").await;
    let public_key_arn = minted_public_key_arn(&router, "precedence-suite-key").await;

    // The PAIRS the rule stands on: for each of those types, the ARN of the resource that
    // EXISTS and an ARN of the same type naming nothing. The answer must be the same for
    // both, which is what shows the TYPE is read off the ARN rather than looked up.
    let pairs = [
        ("a distribution", distribution_arn),
        ("a key value store", key_value_store_arn),
        ("a public key", public_key_arn),
    ];
    for (kind, existing) in &pairs {
        let absent = same_type_naming_nothing(existing);
        for (operation, answer) in ask_each_operation(&router, existing).await {
            let absent_answer = match operation {
                "PutResourcePolicy" => {
                    put_resource_policy(&router, &absent, &policy(&absent, "precedence-suite"))
                        .await
                }
                "GetResourcePolicy" => get_resource_policy(&router, &absent).await,
                _ => delete_resource_policy(&router, &absent).await,
            };
            assert_eq!(
                (answer.status, answer.error_code(), message(&answer)),
                (
                    absent_answer.status,
                    absent_answer.error_code(),
                    message(&absent_answer)
                ),
                "case {kind}/{operation}: the type refusal must not depend on whether the \
                 resource exists; got {:?} for {existing:?} and {:?} for {absent:?}",
                (answer.status, answer.error_code(), message(&answer)),
                (
                    absent_answer.status,
                    absent_answer.error_code(),
                    message(&absent_answer)
                )
            );
        }
    }

    // The table: every row of the three classes, asked of all three operations.
    let mut rows: Vec<(String, String, Outcome)> = Vec::new();
    for (kind, existing) in &pairs {
        rows.push((
            format!("the ARN of {kind} that exists"),
            existing.clone(),
            Outcome::WrongType,
        ));
        rows.push((
            format!("an ARN of {kind} naming nothing"),
            same_type_naming_nothing(existing),
            Outcome::WrongType,
        ));
    }
    rows.extend([
        (
            "an ARN of another service naming the accepted type".to_string(),
            format!("arn:aws:ec2:us-east-1:{ACCOUNT_ID}:{RESOURCE_POLICY_RESOURCE_TYPE}/vo-1"),
            Outcome::WrongType,
        ),
        (
            "an ARN whose resource type merely starts with the accepted one".to_string(),
            format!("arn:aws:cloudfront::{ACCOUNT_ID}:{RESOURCE_POLICY_RESOURCE_TYPE}-group/vo-1"),
            Outcome::WrongType,
        ),
        (
            "an ARN naming the accepted type with no id".to_string(),
            format!("arn:aws:cloudfront::{ACCOUNT_ID}:{RESOURCE_POLICY_RESOURCE_TYPE}"),
            Outcome::WrongType,
        ),
        (
            "an ARN of the accepted type naming nothing".to_string(),
            format!("arn:aws:cloudfront::{ACCOUNT_ID}:{RESOURCE_POLICY_RESOURCE_TYPE}/vo-absent"),
            Outcome::Absent,
        ),
        (
            "the accepted type with an id of another form".to_string(),
            format!("arn:aws:cloudfront::{ACCOUNT_ID}:{RESOURCE_POLICY_RESOURCE_TYPE}/7"),
            Outcome::Absent,
        ),
        (
            "an accepted ARN of this account in another partition".to_string(),
            // The partition segment narrows nothing about WHICH resource an ARN names, so
            // this value still names the accepted resource TYPE and reaches the store, which
            // holds no resource under this ARN: an existence question rather than a form or
            // type one.
            kept_arn.replacen("arn:aws:", "arn:aws-cn:", 1),
            Outcome::Absent,
        ),
        (
            "a value that is no ARN at all".to_string(),
            "not-an-arn".to_string(),
            Outcome::Malformed,
        ),
        (
            "the accepted ARN with its prefix stripped".to_string(),
            kept_arn.strip_prefix("arn:").unwrap_or_default().to_string(),
            Outcome::Malformed,
        ),
    ]);

    for (case, arn, expected) in &rows {
        let (status, code) = expected.status_and_code();
        for (operation, answer) in ask_each_operation(&router, arn).await {
            assert_eq!(
                answer.status, status,
                "case {case}/{operation}: expected HTTP {status} for {arn:?}, got {} ({})",
                answer.status, answer.raw
            );
            assert_eq!(
                answer.error_code(),
                code,
                "case {case}/{operation}: expected {code} for {arn:?}, got {:?} ({})",
                answer.error_code(),
                answer.raw
            );
            let sentence = expected.message(operation);
            assert_eq!(
                message(&answer),
                sentence,
                "case {case}/{operation}: expected message {sentence:?} for {arn:?}, got {:?}",
                message(&answer)
            );
        }
    }

    // None of the refusals wrote or removed anything: the one policy the store holds reads
    // back in the form the write persisted it, and the ARN the surface accepts is still
    // served — the removal answers 200 and leaves the resource reporting the empty document,
    // whether or not it carried a policy when the removal arrived.
    let read = get_resource_policy(&router, &kept_arn).await;
    let expected = stored(&kept);
    assert_eq!(
        reported_document(&read),
        expected.as_str(),
        "case after the refusals: expected the document {expected:?}, got {:?}",
        reported_document(&read)
    );
    let removed = delete_resource_policy(&router, &kept_arn).await;
    assert_eq!(
        removed.status, 200,
        "case the accepted ARN: expected HTTP 200 from the delete, got {} ({})",
        removed.status, removed.raw
    );
    let again = delete_resource_policy(&router, &kept_arn).await;
    assert_eq!(
        again.status, 200,
        "case the accepted ARN removed twice: expected HTTP 200, got {} ({})",
        again.status, again.raw
    );
    let read = get_resource_policy(&router, &kept_arn).await;
    assert_eq!(
        reported_document(&read),
        EMPTY_RESOURCE_POLICY_DOCUMENT,
        "case the accepted ARN removed twice: expected the empty document, got {:?}",
        reported_document(&read)
    );
}
