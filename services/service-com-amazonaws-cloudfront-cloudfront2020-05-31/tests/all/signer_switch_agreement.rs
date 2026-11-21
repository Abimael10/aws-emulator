// SPDX-License-Identifier: CC0-1.0

//! A SWITCHED list wrapper's `Enabled` flag and its `Items` have to agree, end to end over
//! HTTP.
//!
//! RULE under test: the two wrappers a cache behaviour carries that state a switch over their
//! own list — `TrustedSigners` and `TrustedKeyGroups`, each "whether … is enabled" beside the
//! list the switch governs
//! (`smithy_query(query="shape",shape="TrustedSigners,TrustedKeyGroups")`) — are refused when
//! the two disagree: a wrapper that is enabled and names nothing switches nothing on, and one
//! that is disabled and names entries states entries nothing reads. Both directions are
//! `InvalidArgument` (HTTP 400) with their OWN sentence, each naming the member that carried
//! the wrapper and nothing else of the request, and neither creates a distribution.
//!
//! The ORDER matters too: a wrapper whose `Quantity` disagrees with the length of its `Items`
//! is refused on THAT first (`InconsistentQuantities`), and a wrapper that names a key group
//! this account does not hold is refused on the switch first, since the switch is read from
//! the request alone while the group has to be resolved against the store.
//!
//! What the rows span: both switched members; both directions of the disagreement; the two
//! agreeing shapes (switched on with entries, switched off with none) which are accepted; the
//! disagreement sitting on the DEFAULT cache behaviour and on an ORDERED one; and the two
//! refusals that out-rank it. Every row is submitted on `CreateDistribution`,
//! `CreateDistributionWithTags` and `UpdateDistribution`.

use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{
    call, create_distribution, emulator, get_distribution, update_distribution, Answer,
    ACCOUNT_ID, API_VERSION,
};
use crate::key_group_lifecycle::{create_group, mint_public_keys, Group};

/// The sentence a wrapper that is switched ON while naming nothing is refused with, written
/// out as the service emits it; the only variable part is the member name.
fn must_contain_items(member: &str) -> String {
    format!("The parameter {member} must contain items if enabled.")
}

/// The sentence a wrapper that is switched OFF while naming entries is refused with.
fn must_not_contain_items(member: &str) -> String {
    format!("The parameter {member} must not contain items if disabled.")
}

/// `POST /2020-05-31/distribution?WithTags`, the second create path.
async fn create_with_tags(router: &MultiServiceRouter, config: &str) -> Answer {
    let body = format!(
        "<DistributionConfigWithTags \
         xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">{config}<Tags/>\
         </DistributionConfigWithTags>"
    );
    call(
        router,
        "POST",
        &format!("/{API_VERSION}/distribution?WithTags"),
        Some(&body),
        None,
    )
    .await
}

/// Which switched wrapper a row states, and therefore what its entries are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Member {
    /// `TrustedSigners`, whose entries are Amazon Web Services account numbers.
    Signers,
    /// `TrustedKeyGroups`, whose entries are key group ids.
    KeyGroups,
}

impl Member {
    /// The member name the model declares, which is the one the refusal names.
    fn name(self) -> &'static str {
        match self {
            Self::Signers => "TrustedSigners",
            Self::KeyGroups => "TrustedKeyGroups",
        }
    }

    /// The wrapper element stating `enabled`, the `Quantity` `quantity` and `entries`.
    fn xml(self, enabled: bool, quantity: usize, entries: &[String]) -> String {
        let items: String = entries
            .iter()
            .map(|entry| match self {
                Self::Signers => format!("<AwsAccountNumber>{entry}</AwsAccountNumber>"),
                Self::KeyGroups => format!("<KeyGroup>{entry}</KeyGroup>"),
            })
            .collect();
        let member = self.name();
        let list = if entries.is_empty() {
            String::new()
        } else {
            format!("<Items>{items}</Items>")
        };
        format!(
            "<{member}><Enabled>{enabled}</Enabled><Quantity>{quantity}</Quantity>{list}\
             </{member}>"
        )
    }
}

/// Where in the configuration the wrapper under test sits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Position {
    /// On the `DefaultCacheBehavior`.
    Default,
    /// On one `CacheBehavior` of the ordered list, behind a default behaviour that states no
    /// switched wrapper at all.
    Ordered,
}

impl Position {
    /// The path, inside a reported `Distribution`, of the behaviour the wrapper sits on.
    fn config_path(self) -> &'static str {
        match self {
            Self::Default => "DistributionConfig/DefaultCacheBehavior",
            Self::Ordered => "DistributionConfig/CacheBehaviors/Items/CacheBehavior",
        }
    }
}

/// The `DistributionConfig` for `reference` carrying `wrapper` at `position`.
fn config_xml(reference: &str, position: Position, wrapper: &str) -> String {
    let (default_extra, behaviors) = match position {
        Position::Default => (wrapper.to_string(), String::new()),
        Position::Ordered => (
            String::new(),
            format!(
                "<CacheBehaviors><Quantity>1</Quantity><Items>\
                 <CacheBehavior><PathPattern>/signed/*</PathPattern>\
                 <TargetOriginId>primary</TargetOriginId>\
                 <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>{wrapper}\
                 </CacheBehavior></Items></CacheBehaviors>"
            ),
        ),
    };
    format!(
        "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CallerReference>{reference}</CallerReference>\
         <Origins><Quantity>1</Quantity><Items>\
         <Origin><Id>primary</Id><DomainName>origin.example.com</DomainName></Origin>\
         </Items></Origins>\
         <DefaultCacheBehavior><TargetOriginId>primary</TargetOriginId>\
         <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>{default_extra}\
         </DefaultCacheBehavior>{behaviors}\
         <Comment>for {reference}</Comment>\
         <PriceClass>PriceClass_All</PriceClass>\
         <Enabled>true</Enabled></DistributionConfig>",
    )
}

/// What a row expects of every write that carries it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Expected {
    /// The write is accepted and the configuration stored.
    Stored,
    /// The write is refused with this error code and this sentence.
    Refused(&'static str, String),
}

/// One row: the wrapper, where it sits, what it states, and what every write carrying it
/// answers.
struct Row {
    case: String,
    member: Member,
    position: Position,
    enabled: bool,
    /// The `Quantity` the wrapper states — written out rather than derived, so a row can state
    /// one that disagrees with its entries.
    quantity: usize,
    entries: Vec<String>,
    expected: Expected,
}

/// Every row, built against the key group `group` this account really holds and one account
/// number a signer entry names.
fn rows(group: &str) -> Vec<Row> {
    let account = ACCOUNT_ID.to_string();
    let mut rows: Vec<Row> = Vec::new();
    // The two directions of the disagreement, for each switched member, on each behaviour the
    // wrapper can sit on.
    for member in [Member::Signers, Member::KeyGroups] {
        let entry = match member {
            Member::Signers => account.clone(),
            Member::KeyGroups => group.to_string(),
        };
        for position in [Position::Default, Position::Ordered] {
            rows.push(Row {
                case: format!("{} switched on with no items, on {position:?}", member.name()),
                member,
                position,
                enabled: true,
                quantity: 0,
                entries: Vec::new(),
                expected: Expected::Refused(
                    "InvalidArgument",
                    must_contain_items(member.name()),
                ),
            });
            rows.push(Row {
                case: format!("{} switched off with one item, on {position:?}", member.name()),
                member,
                position,
                enabled: false,
                quantity: 1,
                entries: vec![entry.clone()],
                expected: Expected::Refused(
                    "InvalidArgument",
                    must_not_contain_items(member.name()),
                ),
            });
            // The two shapes that AGREE, which are stored.
            rows.push(Row {
                case: format!("{} switched on with one item, on {position:?}", member.name()),
                member,
                position,
                enabled: true,
                quantity: 1,
                entries: vec![entry.clone()],
                expected: Expected::Stored,
            });
            rows.push(Row {
                case: format!("{} switched off with no items, on {position:?}", member.name()),
                member,
                position,
                enabled: false,
                quantity: 0,
                entries: Vec::new(),
                expected: Expected::Stored,
            });
        }
    }
    // What out-ranks the switch: a stated length that disagrees with the list beside it is
    // refused FIRST, on both directions of the switch.
    rows.push(Row {
        case: "TrustedKeyGroups switched on, stating two of one item".to_string(),
        member: Member::KeyGroups,
        position: Position::Default,
        enabled: true,
        quantity: 2,
        entries: vec![group.to_string()],
        expected: Expected::Refused(
            "InconsistentQuantities",
            "The specified quantity of TrustedKeyGroups (2) does not match the actual quantity \
             supplied (1)."
                .to_string(),
        ),
    });
    rows.push(Row {
        case: "TrustedSigners switched off, stating one of no items".to_string(),
        member: Member::Signers,
        position: Position::Default,
        enabled: false,
        quantity: 1,
        entries: Vec::new(),
        expected: Expected::Refused(
            "InconsistentQuantities",
            "The specified quantity of TrustedSigners (1) does not match the actual quantity \
             supplied (0)."
                .to_string(),
        ),
    });
    // And what the switch out-ranks: an entry naming a key group this account does not hold is
    // resolved against the store, which happens only once the request's own shape agrees.
    rows.push(Row {
        case: "TrustedKeyGroups switched off, naming a key group nothing holds".to_string(),
        member: Member::KeyGroups,
        position: Position::Default,
        enabled: false,
        quantity: 1,
        entries: vec!["4fbc5e5c-1ee4-4f3b-9e9f-4a2a3b3b7c11".to_string()],
        expected: Expected::Refused(
            "InvalidArgument",
            must_not_contain_items("TrustedKeyGroups"),
        ),
    });
    rows
}

/// Assert `answer` is the refusal `code`/`sentence` names, carries nothing of the request's
/// entries, and created nothing.
fn assert_refused(case: &str, path: &str, code: &str, sentence: &str, row: &Row, answer: &Answer) {
    assert_eq!(
        answer.status, 400,
        "case {case:?} ({path}): expected HTTP 400, got {} with {}",
        answer.status, answer.raw
    );
    assert_eq!(
        answer.error_code(),
        code,
        "case {case:?} ({path}): expected {code}, got {:?} with {}",
        answer.error_code(),
        answer.raw
    );
    let message = answer.root().find_text("Message").unwrap_or_default();
    assert_eq!(
        message, sentence,
        "case {case:?} ({path}): expected {sentence:?}, got {message:?}"
    );
    for entry in &row.entries {
        assert!(
            !message.contains(entry.as_str()),
            "case {case:?} ({path}): the sentence names no entry of the request, but it carries \
             {entry:?}: {message:?}"
        );
    }
    assert!(
        answer.root().find_text("Id").is_none(),
        "case {case:?} ({path}): a refused write creates no distribution, got {}",
        answer.raw
    );
}

/// RULE: a switched wrapper whose flag and items disagree is refused on both create paths,
/// with the sentence for the direction it is wrong in; the agreeing shapes are stored.
#[tokio::test]
async fn a_switched_wrapper_is_refused_when_its_flag_and_its_items_disagree() {
    let router = emulator();
    let group = held_key_group(&router, "switch-agreement").await;

    for (index, row) in rows(&group).into_iter().enumerate() {
        let wrapper = row.member.xml(row.enabled, row.quantity, &row.entries);
        for (position, path) in ["CreateDistribution", "CreateDistributionWithTags"]
            .into_iter()
            .enumerate()
        {
            let reference = format!("switch-{index}-{position}");
            let config = config_xml(&reference, row.position, &wrapper);
            let answer = match position {
                0 => create_distribution(&router, &config).await,
                _ => create_with_tags(&router, &config).await,
            };
            match &row.expected {
                Expected::Stored => {
                    assert_eq!(
                        answer.status, 201,
                        "case {:?} ({path}): expected HTTP 201, got {} with {}",
                        row.case, answer.status, answer.raw
                    );
                    let id = answer.root().text_at("Id").unwrap_or_default().to_string();
                    let read = get_distribution(&router, &id).await;
                    assert_eq!(
                        read.status, 200,
                        "case {:?} ({path}): expected the distribution to read back, got {} \
                         with {}",
                        row.case, read.status, read.raw
                    );
                    // The wrapper reads back where the row put it, switched the way the row
                    // switched it: the stored configuration is the one submitted.
                    let stored = read.root().text_at(&format!(
                        "{}/{}/Enabled",
                        row.position.config_path(),
                        row.member.name()
                    ));
                    assert_eq!(
                        stored,
                        Some(if row.enabled { "true" } else { "false" }),
                        "case {:?} ({path}): expected the wrapper stored as submitted, got {}",
                        row.case,
                        read.raw
                    );
                }
                Expected::Refused(code, sentence) => {
                    assert_refused(&row.case, path, code, sentence, &row, &answer);
                }
            }
        }
    }
}

/// RULE: the same judgement is made on the way IN to a stored distribution, so an update
/// cannot leave a behaviour carrying a wrapper a create would have refused — and the refused
/// update changes nothing.
#[tokio::test]
async fn an_update_cannot_store_a_wrapper_a_create_would_have_refused() {
    let router = emulator();
    let group = held_key_group(&router, "switch-agreement-update").await;

    let created = create_distribution(
        &router,
        &config_xml(
            "switch-agreement-update",
            Position::Default,
            &Member::KeyGroups.xml(true, 1, std::slice::from_ref(&group)),
        ),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "setup: expected HTTP 201 from CreateDistribution, got {} with {}",
        created.status, created.raw
    );
    let id = created.root().text_at("Id").unwrap_or_default().to_string();
    let etag = created.header("etag").to_string();

    for row in rows(&group) {
        let Expected::Refused(code, sentence) = &row.expected else {
            continue;
        };
        let refused = update_distribution(
            &router,
            &id,
            &config_xml(
                "switch-agreement-update",
                row.position,
                &row.member.xml(row.enabled, row.quantity, &row.entries),
            ),
            Some(&etag),
        )
        .await;
        assert_refused(&row.case, "UpdateDistribution", code, sentence, &row, &refused);

        let read = get_distribution(&router, &id).await;
        assert_eq!(
            read.header("etag"),
            etag,
            "case {:?}: a refused update mints no new version, got {:?}",
            row.case,
            read.header("etag")
        );
        assert_eq!(
            read.root()
                .at("DistributionConfig/DefaultCacheBehavior/TrustedKeyGroups")
                .and_then(|wrapper| wrapper.text_at("Enabled")),
            Some("true"),
            "case {:?}: a refused update leaves the stored wrapper in place, got {}",
            row.case,
            read.raw
        );
    }
}

/// One key group this account really holds, naming one public key `CreatePublicKey` minted —
/// the entry a `TrustedKeyGroups` row names, since a behaviour may only trust a group that
/// exists.
async fn held_key_group(router: &MultiServiceRouter, tag: &str) -> String {
    let keys = mint_public_keys(router, tag, 1).await;
    let group = Group {
        case: tag.to_string(),
        name: tag.to_string(),
        items: keys,
        comment: None,
    };
    let answer = create_group(router, &group.xml()).await;
    assert_eq!(
        answer.status, 201,
        "case {tag}: expected HTTP 201 from CreateKeyGroup, got {} with {}",
        answer.status, answer.raw
    );
    answer.root().text_at("Id").unwrap_or_default().to_string()
}
