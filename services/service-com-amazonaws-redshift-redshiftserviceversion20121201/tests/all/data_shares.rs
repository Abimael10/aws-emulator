// SPDX-License-Identifier: CC0-1.0

//! The DATASHARE family, end to end over the awsQuery wire.
//!
//! The RULES these cases assert:
//!
//! * every operation that acts on a datashare resolves the `DataShareArn` against
//!   this account's datashares first, and an ARN that resolves to nothing is the
//!   modelled `InvalidDataShareFault` — a datashare is created on the producer's
//!   DATABASE and Amazon Redshift publishes no API that creates one, so an account
//!   that has created none holds none — carrying the fixed sentence Amazon
//!   Redshift words for that path, with nothing of the request in it;
//! * an authorization change validates the `ConsumerIdentifier` it carries BEFORE
//!   any datashare is resolved, so a value that names no consumer is the
//!   parameter-value refusal whether or not the datashare exists;
//! * every operation that names a NAMESPACE resolves it against the control plane
//!   that owns it — this one for a provisioned cluster's namespace, Amazon Redshift
//!   Serverless for one of its namespaces — and an ARN that control plane holds no
//!   namespace under is the modelled `InvalidNamespaceFault`, while a peer that
//!   cannot be asked at all establishes nothing and so refuses nothing;
//! * the reads report the account's datashares, which is an EMPTY page for an
//!   account that holds none — not a refusal, because the account exists.
//!
//! The rows differ along what those rules generalise over: which operation, the
//! ARN's spelling and which control plane it belongs to. The harness is shared
//! with [`crate::cluster_lifecycle`], which owns it.

use service_com_amazonaws_redshift_redshiftserviceversion20121201::state;

use crate::cluster_lifecycle::{all_text, Answer, Emulator};

/// A datashare ARN this account holds no datashare under, in the form Amazon
/// Redshift mints one in.
const ABSENT_DATASHARE: &str =
    "arn:aws:redshift:us-west-2:000000000000:datashare:870e2a92-1aab-9599-6cd2-efe9d3cdd491/sales";

/// A namespace ARN of THIS control plane that no cluster of this account carries.
const ABSENT_NAMESPACE: &str =
    "arn:aws:redshift:us-west-2:000000000000:namespace:11111111-2222-3333-4444-555555555555";

/// A namespace ARN of Amazon Redshift Serverless — a DIFFERENT control plane,
/// which this suite does not mount, so it cannot be asked about it.
const SERVERLESS_NAMESPACE: &str =
    "arn:aws:redshift-serverless:us-west-2:000000000000:namespace/analytics";

/// The members one row of a table sends beside the ARN it names.
type Params<'a> = Vec<(&'a str, &'a str)>;

/// One row of an operation table: the case, the action, and the members beside
/// the ARN every row carries.
type OperationRow<'a> = (&'a str, &'a str, Params<'a>);

/// Assert one refusal: the status and the `<Code>` the model gives the fault.
fn assert_refused(case: &str, answer: &Answer, status: u16, code: &str) {
    assert_eq!(
        answer.status, status,
        "case {case}: expected status {status}, got {} with body {}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.error_code().as_deref(),
        Some(code),
        "case {case}: expected error code {code:?}, got {:?} with body {}",
        answer.error_code(),
        answer.body
    );
}

/// An emulator with one serving cluster, and that cluster's own namespace ARN —
/// which is a namespace of this account and so resolves.
async fn harness(cluster: &str) -> (Emulator, String) {
    let emulator = Emulator::new();
    let created = emulator.create_minimal(cluster).await;
    assert_eq!(
        created.status, 200,
        "case seed {cluster}: expected 200, got {} with body {}",
        created.status, created.body
    );
    let namespace = created.text("ClusterNamespaceArn").unwrap_or_default();
    assert!(
        !namespace.is_empty(),
        "case seed {cluster}: expected the cluster's namespace ARN, got body {}",
        created.body
    );
    (emulator, namespace)
}

/// Every operation that acts on a datashare answers the datashare's own modelled
/// fault for an ARN this account holds no datashare under — for a table spanning
/// the operations and the ARN's spelling.
#[tokio::test]
async fn every_datashare_operation_refuses_an_absent_datashare() {
    let (emulator, namespace) = harness("datashare-cluster").await;

    // (case, the action, the members beside the `DataShareArn` every row carries)
    let operations: Vec<OperationRow> = vec![
        ("DescribeDataShares", "DescribeDataShares", Vec::new()),
        (
            "AuthorizeDataShare",
            "AuthorizeDataShare",
            vec![("ConsumerIdentifier", "000000000001")],
        ),
        (
            "DeauthorizeDataShare",
            "DeauthorizeDataShare",
            vec![("ConsumerIdentifier", "000000000001")],
        ),
        (
            "AssociateDataShareConsumer",
            "AssociateDataShareConsumer",
            vec![("AssociateEntireAccount", "true")],
        ),
        (
            "AssociateDataShareConsumer naming a namespace of this account",
            "AssociateDataShareConsumer",
            vec![("ConsumerArn", namespace.as_str())],
        ),
        (
            "DisassociateDataShareConsumer",
            "DisassociateDataShareConsumer",
            vec![("DisassociateEntireAccount", "true")],
        ),
        ("RejectDataShare", "RejectDataShare", Vec::new()),
    ];

    // The ARN's spelling is what the rule generalises over, so each operation is
    // driven with several values that name no datashare of this account.
    let absent_arns: Vec<(&str, &str)> = vec![
        ("a datashare ARN nothing created", ABSENT_DATASHARE),
        (
            "a namespace ARN of this account, which names no datashare",
            namespace.as_str(),
        ),
        ("a value that is not an ARN at all", "sales"),
    ];

    for (arn_case, arn) in &absent_arns {
        for (operation, action, members) in &operations {
            let case = format!("{operation} with {arn_case}");
            let mut params: Vec<(&str, &str)> = vec![("DataShareArn", arn)];
            params.extend(members.iter().copied());
            let answer = emulator.call(action, &params).await;
            assert_refused(&case, &answer, 400, "InvalidDataShareFault");
            let expected = expected_absent_message(action);
            assert_eq!(
                answer.text("Message").as_deref(),
                Some(expected),
                "case {case}: expected the sentence {expected:?}, got {:?}",
                answer.text("Message")
            );
            // The sentence is entirely fixed: it reports nothing of the request
            // back, so no identifier the caller sent appears in it.
            assert!(
                !expected.contains(*arn),
                "case {case}: expected no request value in the refusal, got {expected:?}"
            );
        }
    }
}

/// The fixed sentence the datashare fault carries for `action`.
///
/// The READ names the ARN its optional filter carried as the thing that does not
/// exist, and every operation that ACTS on a datashare speaks about the share's
/// availability instead — two sentences, one per path, held beside the handlers
/// that answer them.
fn expected_absent_message(action: &str) -> &'static str {
    if action == "DescribeDataShares" {
        state::DATA_SHARE_ARN_DOES_NOT_EXIST
    } else {
        state::DATA_SHARE_NOT_AVAILABLE
    }
}

/// An account that has created no datashare has none: the reads report an empty
/// page rather than a refusal, whichever way the request asks.
#[tokio::test]
async fn an_account_with_no_datashare_is_served_an_empty_page() {
    let (emulator, namespace) = harness("empty-datashare-cluster").await;

    // (case, the action, the members the request carries)
    let rows: Vec<OperationRow> = vec![
        ("every datashare of the account", "DescribeDataShares", Vec::new()),
        (
            "the datashares of a producer namespace of this account",
            "DescribeDataSharesForProducer",
            vec![("ProducerArn", namespace.as_str())],
        ),
        (
            "the datashares of a consumer namespace of this account",
            "DescribeDataSharesForConsumer",
            vec![("ConsumerArn", namespace.as_str())],
        ),
        (
            "the datashares of every producer",
            "DescribeDataSharesForProducer",
            Vec::new(),
        ),
        (
            "the datashares of every consumer",
            "DescribeDataSharesForConsumer",
            Vec::new(),
        ),
        (
            "a producer namespace of a control plane this emulator cannot ask",
            "DescribeDataSharesForProducer",
            vec![("ProducerArn", SERVERLESS_NAMESPACE)],
        ),
        (
            "the datashares of a producer in one status",
            "DescribeDataSharesForProducer",
            vec![("ProducerArn", namespace.as_str()), ("Status", "ACTIVE")],
        ),
    ];

    for (case, action, params) in rows {
        let answer = emulator.call(action, &params).await;
        assert_eq!(
            answer.status, 200,
            "case {case}: expected 200, got {} with body {}",
            answer.status, answer.body
        );
        assert!(
            all_text(&answer.body, "DataShareArn").is_empty(),
            "case {case}: expected no datashare reported, got body {}",
            answer.body
        );
        // A page that holds everything there is hands out no cursor.
        assert_eq!(
            answer.text("Marker"),
            None,
            "case {case}: expected no Marker on a complete page, got {:?}",
            answer.text("Marker")
        );
    }
}

/// A namespace an operation names has to be one the control plane that owns it
/// holds: this service's own store answers for a provisioned cluster's namespace,
/// and a peer that cannot be asked establishes nothing — so silence refuses
/// nothing.
#[tokio::test]
async fn a_namespace_an_operation_names_is_resolved_where_it_lives() {
    let (emulator, namespace) = harness("namespace-datashare-cluster").await;

    // (case, the action, the member naming the namespace, the ARN, the refusal or
    // `None` when the call is served)
    let rows: Vec<(&str, &str, &str, &str, Option<&str>)> = vec![
        (
            "a producer namespace of this account",
            "DescribeDataSharesForProducer",
            "ProducerArn",
            namespace.as_str(),
            None,
        ),
        (
            "a namespace ARN of this control plane that no cluster carries",
            "DescribeDataSharesForProducer",
            "ProducerArn",
            ABSENT_NAMESPACE,
            Some("InvalidNamespaceFault"),
        ),
        (
            "a consumer namespace ARN of this control plane that no cluster carries",
            "DescribeDataSharesForConsumer",
            "ConsumerArn",
            ABSENT_NAMESPACE,
            Some("InvalidNamespaceFault"),
        ),
        (
            "a value that is not a namespace ARN of either control plane",
            "DescribeDataSharesForConsumer",
            "ConsumerArn",
            "analytics",
            Some("InvalidNamespaceFault"),
        ),
        (
            // Amazon Redshift Serverless is not mounted beside this emulator, so
            // nothing is known about the namespace either way: the read is served
            // rather than refused on the strength of a peer that never answered.
            "a namespace of a control plane this emulator cannot ask",
            "DescribeDataSharesForConsumer",
            "ConsumerArn",
            SERVERLESS_NAMESPACE,
            None,
        ),
    ];

    for (case, action, member, arn, refusal) in rows {
        let answer = emulator.call(action, &[(member, arn)]).await;
        match refusal {
            None => assert_eq!(
                answer.status, 200,
                "case {case}: expected 200, got {} with body {}",
                answer.status, answer.body
            ),
            Some(code) => {
                assert_refused(case, &answer, 400, code);
                assert_eq!(
                    answer.text("Message").as_deref(),
                    Some(state::invalid_namespace_message(arn).as_str()),
                    "case {case}: expected the ARN named in the refusal, got {:?}",
                    answer.text("Message")
                );
            }
        }
    }

    // The association pair resolves the NAMESPACE before the datashare, so a
    // request naming a namespace that does not exist is answered about the
    // namespace however the datashare resolves …
    for action in [
        "AssociateDataShareConsumer",
        "DisassociateDataShareConsumer",
    ] {
        let answer = emulator
            .call(
                action,
                &[
                    ("DataShareArn", ABSENT_DATASHARE),
                    ("ConsumerArn", ABSENT_NAMESPACE),
                ],
            )
            .await;
        assert_refused(
            &format!("{action} naming a namespace that does not exist"),
            &answer,
            400,
            "InvalidNamespaceFault",
        );
    }

    // … and a request naming NO consumer at all names nothing to associate with.
    for (action, case) in [
        ("AssociateDataShareConsumer", "an association naming no consumer"),
        (
            "DisassociateDataShareConsumer",
            "a disassociation naming no consumer",
        ),
    ] {
        let answer = emulator
            .call(action, &[("DataShareArn", ABSENT_DATASHARE)])
            .await;
        assert_refused(case, &answer, 400, "InvalidParameterCombination");
    }
}

/// A datashare of this account, written straight into the shared store the
/// handlers read.
///
/// A datashare is created on the PRODUCER's database and Amazon Redshift
/// publishes no API of this control plane that mints one, so a case about a
/// datashare that EXISTS seeds it the way the store holds it — exactly as the
/// suites about a store bound seed the records they need.
fn seed_data_share(emulator: &Emulator, data_share_arn: &str, producer_arn: &str) {
    let store = state::service_state(&*emulator.engine).expect("the Redshift store");
    store.lock().data_shares.push(state::StoredDataShare {
        data_share_arn: Some(data_share_arn.to_string()),
        producer_arn: Some(producer_arn.to_string()),
        allow_publicly_accessible_consumers: Some(false),
        data_share_associations: None,
        managed_by: None,
        data_share_type: Some("INTERNAL".to_string()),
    });
}

/// The datashare ARN the seeded cases act on, in the form Amazon Redshift mints
/// one in.
const HELD_DATASHARE: &str =
    "arn:aws:redshift:us-west-2:000000000000:datashare:1f0d60a4-5a11-4d93-a1ed-2a03da4a7cd1/sales";

/// An authorization change checks the consumer identifier it carries BEFORE any
/// datashare is resolved — over a table spanning the forms a consumer is named by
/// and the forms that name none, each driven against a datashare that exists and
/// one that does not.
#[tokio::test]
async fn an_authorization_change_validates_the_consumer_identifier_first() {
    let (emulator, namespace) = harness("consumer-identifier-cluster").await;
    seed_data_share(&emulator, HELD_DATASHARE, &namespace);

    // The forms a consumer IS named by: an account identifier, the managing
    // entity keyword the model names (`smithy_query(query="doc",
    // shape="AuthorizeDataShareMessage", member="ConsumerIdentifier")` — "an
    // Amazon Web Services account ID or a keyword, such as ADX"), and a namespace
    // ARN of either control plane in either spelling the model's own namespace
    // pattern admits.
    let named: Vec<(&str, &str)> = vec![
        ("another account of this partition", "000000000001"),
        ("this emulator's own account", ::engine::emulator_account_id()),
        (
            "the managing entity the model names",
            state::CONSUMER_IDENTIFIER_KEYWORDS[0],
        ),
        ("a namespace of this account", namespace.as_str()),
        (
            "a namespace ARN of this control plane that no cluster carries",
            ABSENT_NAMESPACE,
        ),
        (
            "a namespace ARN of the serverless control plane",
            SERVERLESS_NAMESPACE,
        ),
    ];

    // The forms that name NO consumer: everything the account-identifier shape
    // and the two ARN spellings exclude.
    let unnamed: Vec<(&str, &str)> = vec![
        ("an account identifier one digit short", "00000000001"),
        ("an account identifier one digit too long", "0000000000012"),
        ("an account identifier with a letter in it", "00000000000a"),
        ("a name rather than an identifier", "sales-consumer"),
        (
            "an ARN that names no namespace",
            "arn:aws:redshift:us-west-2:000000000000:cluster:consumer-identifier-cluster",
        ),
        ("the managing entity keyword in another case", "adx"),
    ];

    // Both halves are driven against a datashare that EXISTS and one that does
    // not, because what the consumer identifier is checked against is the request
    // alone.
    let shares: [(&str, &str); 2] = [
        ("a datashare of this account", HELD_DATASHARE),
        ("a datashare nothing created", ABSENT_DATASHARE),
    ];

    for action in ["AuthorizeDataShare", "DeauthorizeDataShare"] {
        for (consumer_case, consumer) in &unnamed {
            for (share_case, data_share_arn) in &shares {
                let case = format!("{action} naming {consumer_case} against {share_case}");
                let answer = emulator
                    .call(
                        action,
                        &[
                            ("DataShareArn", data_share_arn),
                            ("ConsumerIdentifier", consumer),
                        ],
                    )
                    .await;
                assert_refused(&case, &answer, 400, "InvalidParameterValue");
                assert_eq!(
                    answer.text("Message").as_deref(),
                    Some(state::CONSUMER_IDENTIFIER_INVALID),
                    "case {case}: expected the consumer-identifier sentence, got {:?}",
                    answer.text("Message")
                );
            }
        }

        // A consumer that IS named gets past the check, so the datashare is
        // resolved next and an ARN that resolves to nothing is the datashare's own
        // fault.
        for (consumer_case, consumer) in &named {
            let case = format!("{action} naming {consumer_case} against a datashare nothing created");
            let answer = emulator
                .call(
                    action,
                    &[
                        ("DataShareArn", ABSENT_DATASHARE),
                        ("ConsumerIdentifier", consumer),
                    ],
                )
                .await;
            assert_refused(&case, &answer, 400, "InvalidDataShareFault");
            assert_eq!(
                answer.text("Message").as_deref(),
                Some(state::DATA_SHARE_NOT_AVAILABLE),
                "case {case}: expected the datashare-availability sentence, got {:?}",
                answer.text("Message")
            );
        }
    }
}

/// A named consumer and a datashare that exists is a change the store keeps: the
/// authorization is reported back and read back through `DescribeDataShares`, and
/// the deauthorization moves that same association.
#[tokio::test]
async fn an_authorization_change_is_recorded_on_the_datashare_it_names() {
    let (emulator, namespace) = harness("authorized-datashare-cluster").await;
    seed_data_share(&emulator, HELD_DATASHARE, &namespace);

    // (case, the action, the consumer it names, the status the association is left
    // at afterwards)
    let changes: [(&str, &str, &str, &str); 4] = [
        (
            "an account the producer authorizes",
            "AuthorizeDataShare",
            "000000000001",
            "AUTHORIZED",
        ),
        (
            "the managing entity the producer authorizes",
            "AuthorizeDataShare",
            state::CONSUMER_IDENTIFIER_KEYWORDS[0],
            "AUTHORIZED",
        ),
        (
            "the account the producer takes that authorization back from",
            "DeauthorizeDataShare",
            "000000000001",
            "DEAUTHORIZED",
        ),
        (
            "the managing entity the producer takes it back from",
            "DeauthorizeDataShare",
            state::CONSUMER_IDENTIFIER_KEYWORDS[0],
            "DEAUTHORIZED",
        ),
    ];

    for (case, action, consumer, status) in changes {
        let answer = emulator
            .call(
                action,
                &[
                    ("DataShareArn", HELD_DATASHARE),
                    ("ConsumerIdentifier", consumer),
                ],
            )
            .await;
        assert_eq!(
            answer.status, 200,
            "case {case}: expected 200, got {} with body {}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.text("DataShareArn").as_deref(),
            Some(HELD_DATASHARE),
            "case {case}: expected the datashare reported back, got body {}",
            answer.body
        );

        // Read back through the implemented reader: the association the change
        // wrote is what a later caller sees.
        let read = emulator
            .call("DescribeDataShares", &[("DataShareArn", HELD_DATASHARE)])
            .await;
        assert_eq!(
            read.status, 200,
            "case {case}: expected the read served, got {} with body {}",
            read.status, read.body
        );
        let consumers = all_text(&read.body, "ConsumerIdentifier");
        assert!(
            consumers.iter().any(|held| held == consumer),
            "case {case}: expected {consumer} among the associations, got {consumers:?}"
        );
        assert!(
            all_text(&read.body, "Status").iter().any(|held| held == status),
            "case {case}: expected an association at {status}, got body {}",
            read.body
        );
    }
}
