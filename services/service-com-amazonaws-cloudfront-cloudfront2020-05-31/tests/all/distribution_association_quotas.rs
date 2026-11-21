// SPDX-License-Identifier: CC0-1.0

//! What a distribution's PER-DISTRIBUTION quotas count, end to end over HTTP.
//!
//! Every assertion goes through the router, so each case exercises routing, restXml
//! (de)serialization, the handler, the persisted state and serialization — the same path a
//! client takes. Two rules:
//!
//! * a trusted signer and a trusted key group are each ASSOCIATED WITH THE DISTRIBUTION, and
//!   a cache behaviour names one by id — so what the quota counts is the DISTINCT accounts and
//!   the DISTINCT key groups the configuration names across all of its behaviours, however
//!   many behaviours name the same one, which is also the set the distribution reports as
//!   `ActiveTrustedSigners` / `ActiveTrustedKeyGroups`. The ordinary signed-URL
//!   configuration — one key group trusted by every behaviour — is therefore one association,
//!   and only a configuration naming MORE DISTINCT ones than the quota admits is refused;
//! * the cache-behaviour quota bounds the ORDERED `CacheBehaviors` list ([`BEHAVIOR_CEILING`]),
//!   and the configuration is stored whole: a configuration at the ceiling is created and
//!   reports its behaviours back in the order it submitted them, one past it is refused with
//!   `TooManyCacheBehaviors`.
//!
//! The rows span what each rule generalises over: WHICH behaviour names an association (the
//! default one, one ordered one, several at once), how many entries one behaviour's list
//! carries (including the same id twice in one list), how many DISTINCT ids the configuration
//! names (below the quota, at it, one past it) and how they are spread over the behaviours
//! (all in one list, one per behaviour, a mixture) — and, for the behaviour count, zero, one,
//! a realistic multi-behaviour configuration, the ceiling itself and one past it.
//!
//! The harness (the router, the request helpers and the element scanner) is
//! [`crate::distribution_lifecycle`]'s, and the key groups a behaviour trusts are created
//! through the key group surface with [`crate::key_group_lifecycle`]'s helpers, rather than
//! through second copies of either.

use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{
    call, create_distribution, emulator, get_distribution, Answer, Node, API_VERSION,
};
use crate::key_group_lifecycle::{create_group, mint_public_keys};

/// The most DISTINCT key groups one distribution may be associated with: CloudFront's
/// published quota is 4 key groups associated with a single distribution (the Amazon
/// CloudFront Developer Guide's "Quotas", quotas on signed URLs and signed cookies). A
/// literal here, so a wrong constant in the crate cannot make this suite pass.
const KEY_GROUP_QUOTA: usize = 4;

/// The most DISTINCT trusted signers one distribution may be associated with: CloudFront's
/// published quota is 5 trusted signers (Amazon Web Services accounts) for a distribution. A
/// literal here for the reason [`KEY_GROUP_QUOTA`] is one.
const SIGNER_QUOTA: usize = 5;

/// The most behaviours the ordered `CacheBehaviors` list of one configuration may carry: the
/// recorded service creates a configuration stating this many and refuses one stating one
/// more. A literal here for the reason [`KEY_GROUP_QUOTA`] is one.
const BEHAVIOR_CEILING: usize = 74;

/// The sentence a configuration stating more cache behaviours than the quota admits is
/// refused with, as a literal rather than read back from the crate: the WORDING is what this
/// refusal is held to.
const TOO_MANY_CACHE_BEHAVIORS: &str =
    "Your request contains more CacheBehaviors than are allowed per distribution.";

/// The sentence a configuration associating more key groups than the quota admits is refused
/// with.
const TOO_MANY_KEY_GROUPS: &str =
    "The number of key groups referenced by this distribution is more than the maximum \
     allowed.";

/// The sentence a configuration associating more trusted signers than the quota admits is
/// refused with.
const TOO_MANY_TRUSTED_SIGNERS: &str =
    "Your request contains more trusted signers than are allowed per distribution.";

/// One cache behaviour of a configuration under test: the key groups and the signer accounts
/// it trusts, each named by its position in the ids the suite created.
#[derive(Debug, Clone, Default)]
struct Behavior {
    /// Positions into the created key groups, in the order the behaviour lists them —
    /// repeats included, since a list may name one group twice.
    groups: Vec<usize>,
    /// Positions into [`signer_accounts`], in the order the behaviour lists them.
    signers: Vec<usize>,
}

impl Behavior {
    /// A behaviour trusting the key groups at `groups`.
    fn groups(groups: &[usize]) -> Self {
        Self {
            groups: groups.to_vec(),
            signers: Vec::new(),
        }
    }

    /// A behaviour trusting the signer accounts at `signers`.
    fn signers(signers: &[usize]) -> Self {
        Self {
            groups: Vec::new(),
            signers: signers.to_vec(),
        }
    }

    /// The members this behaviour adds inside a `DefaultCacheBehavior` or a `CacheBehavior`
    /// element: the two trust wrappers, each present only where the behaviour names anything
    /// for it, with `Quantity` stating the length of `Items` as every CloudFront list wrapper
    /// requires.
    fn trust_xml(&self, group_ids: &[String]) -> String {
        let mut out = String::new();
        if !self.signers.is_empty() {
            let items: String = self
                .signers
                .iter()
                .map(|index| format!("<AwsAccountNumber>{}</AwsAccountNumber>", account(*index)))
                .collect();
            out.push_str(&format!(
                "<TrustedSigners><Enabled>true</Enabled><Quantity>{}</Quantity><Items>{items}\
                 </Items></TrustedSigners>",
                self.signers.len()
            ));
        }
        if !self.groups.is_empty() {
            let items: String = self
                .groups
                .iter()
                .map(|index| format!("<KeyGroup>{}</KeyGroup>", group_ids[*index]))
                .collect();
            out.push_str(&format!(
                "<TrustedKeyGroups><Enabled>true</Enabled><Quantity>{}</Quantity><Items>{items}\
                 </Items></TrustedKeyGroups>",
                self.groups.len()
            ));
        }
        out
    }
}

/// One signer account: a 12-digit Amazon Web Services account number, which is all
/// `TrustedSigners$Items` carries.
fn account(index: usize) -> String {
    format!("{:012}", index + 1)
}

/// The `DistributionConfig` a case submits: one custom origin, a default cache behaviour
/// trusting what `default` names, and one ordered behaviour per entry of `ordered` on its own
/// path pattern.
fn config_xml(
    reference: &str,
    default: &Behavior,
    ordered: &[Behavior],
    group_ids: &[String],
) -> String {
    let mut behaviors = format!("<Quantity>{}</Quantity>", ordered.len());
    if !ordered.is_empty() {
        behaviors.push_str("<Items>");
        for (position, behavior) in ordered.iter().enumerate() {
            behaviors.push_str(&format!(
                "<CacheBehavior><PathPattern>/path-{position}/*</PathPattern>\
                 <TargetOriginId>primary</TargetOriginId>\
                 <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>{}</CacheBehavior>",
                behavior.trust_xml(group_ids)
            ));
        }
        behaviors.push_str("</Items>");
    }
    format!(
        "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CallerReference>{reference}</CallerReference>\
         <Origins><Quantity>1</Quantity><Items><Origin>\
         <Id>primary</Id><DomainName>origin.example.com</DomainName>\
         <CustomOriginConfig><HTTPPort>80</HTTPPort><HTTPSPort>443</HTTPSPort>\
         <OriginProtocolPolicy>https-only</OriginProtocolPolicy></CustomOriginConfig>\
         </Origin></Items></Origins>\
         <DefaultCacheBehavior><TargetOriginId>primary</TargetOriginId>\
         <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>{}</DefaultCacheBehavior>\
         <CacheBehaviors>{behaviors}</CacheBehaviors>\
         <Comment>{reference}</Comment>\
         <Enabled>true</Enabled>\
         </DistributionConfig>",
        default.trust_xml(group_ids)
    )
}

/// Create `count` key groups, each naming one public key the public key surface minted, and
/// report their ids in creation order.
async fn create_groups(router: &MultiServiceRouter, tag: &str, count: usize) -> Vec<String> {
    let keys = mint_public_keys(router, tag, count).await;
    let mut ids: Vec<String> = Vec::new();
    for (index, key) in keys.iter().enumerate() {
        let body = format!(
            "<KeyGroupConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
             <Name>{tag}-{index}</Name>\
             <Items><PublicKey>{key}</PublicKey></Items></KeyGroupConfig>"
        );
        let answer = create_group(router, &body).await;
        assert_eq!(
            answer.status, 201,
            "case {tag}-{index}: expected HTTP 201 from CreateKeyGroup, got {} with {}",
            answer.status, answer.raw
        );
        ids.push(
            answer
                .root()
                .find_text("Id")
                .unwrap_or_default()
                .to_string(),
        );
    }
    ids
}

/// The ids an `ActiveTrustedKeyGroups` element lists, in document order. Each entry travels
/// in the `KeyGroup` element the model's `xmlName` gives `KGKeyPairIdsList`'s member.
fn active_key_groups(distribution: &Node) -> Vec<String> {
    distribution
        .at("ActiveTrustedKeyGroups/Items")
        .map(|items| {
            items
                .children_named("KeyGroup")
                .into_iter()
                .map(|entry| entry.text_at("KeyGroupId").unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// The accounts an `ActiveTrustedSigners` element lists, in document order. Each entry travels
/// in the `Signer` element the model's `xmlName` gives `SignerList`'s member.
fn active_signers(distribution: &Node) -> Vec<String> {
    distribution
        .at("ActiveTrustedSigners/Items")
        .map(|items| {
            items
                .children_named("Signer")
                .into_iter()
                .map(|entry| {
                    entry
                        .text_at("AwsAccountNumber")
                        .unwrap_or_default()
                        .to_string()
                })
                .collect()
        })
        .unwrap_or_default()
}

/// What a row expects of the create.
#[derive(Debug)]
enum Expected {
    /// HTTP 201, reporting these key groups and these signer accounts as active, in order.
    Created {
        groups: Vec<usize>,
        signers: Vec<usize>,
    },
    /// HTTP 400 under this error code, carrying this sentence.
    Refused(&'static str, &'static str),
}

/// One row of the association table.
struct Row {
    case: &'static str,
    default: Behavior,
    ordered: Vec<Behavior>,
    expected: Expected,
}

/// The rows: what the quota counts, over the ways a configuration can name an association.
fn rows() -> Vec<Row> {
    let past_groups: Vec<usize> = (0..=KEY_GROUP_QUOTA).collect();
    let past_signers: Vec<usize> = (0..=SIGNER_QUOTA).collect();
    vec![
        Row {
            // The ordinary signed-URL configuration: ONE key group, trusted by every
            // behaviour — five references to one association.
            case: "one key group trusted by the default behaviour and four ordered ones",
            default: Behavior::groups(&[0]),
            ordered: vec![
                Behavior::groups(&[0]),
                Behavior::groups(&[0]),
                Behavior::groups(&[0]),
                Behavior::groups(&[0]),
            ],
            expected: Expected::Created {
                groups: vec![0],
                signers: Vec::new(),
            },
        },
        Row {
            case: "one behaviour naming the same key group twice",
            default: Behavior::groups(&[1, 1]),
            ordered: Vec::new(),
            expected: Expected::Created {
                groups: vec![1],
                signers: Vec::new(),
            },
        },
        Row {
            case: "the quota's worth of distinct key groups in one behaviour",
            default: Behavior::groups(&[0, 1, 2, 3]),
            ordered: Vec::new(),
            expected: Expected::Created {
                groups: vec![0, 1, 2, 3],
                signers: Vec::new(),
            },
        },
        Row {
            case: "the quota's worth of distinct key groups, one per behaviour",
            default: Behavior::groups(&[0]),
            ordered: vec![
                Behavior::groups(&[1]),
                Behavior::groups(&[2]),
                Behavior::groups(&[3, 0]),
            ],
            expected: Expected::Created {
                groups: vec![0, 1, 2, 3],
                signers: Vec::new(),
            },
        },
        Row {
            case: "one key group past the quota, in one behaviour",
            default: Behavior::groups(&past_groups),
            ordered: Vec::new(),
            expected: Expected::Refused(
                "TooManyKeyGroupsAssociatedToDistribution",
                TOO_MANY_KEY_GROUPS,
            ),
        },
        Row {
            case: "one key group past the quota, spread over the behaviours",
            default: Behavior::groups(&[0, 1]),
            ordered: vec![Behavior::groups(&[2, 3]), Behavior::groups(&[4])],
            expected: Expected::Refused(
                "TooManyKeyGroupsAssociatedToDistribution",
                TOO_MANY_KEY_GROUPS,
            ),
        },
        Row {
            case: "one signer account trusted by five behaviours",
            default: Behavior::signers(&[0]),
            ordered: vec![
                Behavior::signers(&[0]),
                Behavior::signers(&[0]),
                Behavior::signers(&[0]),
                Behavior::signers(&[0]),
            ],
            expected: Expected::Created {
                groups: Vec::new(),
                signers: vec![0],
            },
        },
        Row {
            case: "the quota's worth of distinct signers, spread over the behaviours with \
                   repeats",
            default: Behavior::signers(&[0, 1]),
            ordered: vec![
                Behavior::signers(&[2]),
                Behavior::signers(&[0]),
                Behavior::signers(&[3, 4]),
            ],
            expected: Expected::Created {
                groups: Vec::new(),
                signers: vec![0, 1, 2, 3, 4],
            },
        },
        Row {
            case: "one signer past the quota",
            default: Behavior::signers(&past_signers),
            ordered: Vec::new(),
            expected: Expected::Refused("TooManyTrustedSigners", TOO_MANY_TRUSTED_SIGNERS),
        },
        Row {
            case: "one key group and one signer account, each trusted by two behaviours",
            default: Behavior {
                groups: vec![2],
                signers: vec![1],
            },
            ordered: vec![Behavior {
                groups: vec![2],
                signers: vec![1],
            }],
            expected: Expected::Created {
                groups: vec![2],
                signers: vec![1],
            },
        },
    ]
}

/// A per-distribution association quota counts the DISTINCT key groups and signer accounts a
/// configuration names, whichever behaviours name them and however often — and the
/// distribution reports exactly that set as the active one, both from the create and from a
/// later read.
#[tokio::test]
async fn an_association_quota_counts_the_distinct_entities_the_behaviours_name() {
    let router = emulator();
    // One more group than the quota admits, so the rows can name one past it.
    let groups = create_groups(&router, "quota-group", KEY_GROUP_QUOTA + 1).await;

    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let body = config_xml(
            &format!("ref-association-{index}"),
            &row.default,
            &row.ordered,
            &groups,
        );
        let answer = create_distribution(&router, &body).await;

        match row.expected {
            Expected::Refused(code, sentence) => {
                assert_eq!(
                    answer.status, 400,
                    "case {case}: expected HTTP 400, got {} with {}",
                    answer.status, answer.raw
                );
                assert_eq!(
                    answer.error_code(),
                    code,
                    "case {case}: expected the error code {code:?}, got {:?}",
                    answer.error_code()
                );
                let message = answer.root().find_text("Message").unwrap_or_default();
                assert_eq!(
                    message, sentence,
                    "case {case}: expected the sentence {sentence:?}, got {message:?}"
                );
            }
            Expected::Created { groups: expected_groups, signers: expected_signers } => {
                assert_eq!(
                    answer.status, 201,
                    "case {case}: expected HTTP 201, got {} with {}",
                    answer.status, answer.raw
                );
                let expected_group_ids: Vec<String> = expected_groups
                    .iter()
                    .map(|index| groups[*index].clone())
                    .collect();
                let expected_accounts: Vec<String> =
                    expected_signers.iter().map(|index| account(*index)).collect();
                let id = answer
                    .root()
                    .text_at("Id")
                    .unwrap_or_default()
                    .to_string();

                // The create's own payload and a later read answer from the one stored
                // record, so both report the same active sets.
                let read = get_distribution(&router, &id).await;
                assert_eq!(
                    read.status, 200,
                    "case {case}: expected GetDistribution to serve the distribution, got {} \
                     with {}",
                    read.status, read.raw
                );
                for (surface, served) in [("CreateDistribution", &answer), ("GetDistribution", &read)]
                {
                    let distribution = served.root();
                    let listed = active_key_groups(distribution);
                    assert_eq!(
                        listed, expected_group_ids,
                        "case {case} ({surface}): expected the active key groups \
                         {expected_group_ids:?}, got {listed:?}"
                    );
                    let quantity = distribution.text_at("ActiveTrustedKeyGroups/Quantity");
                    assert_eq!(
                        quantity,
                        Some(expected_group_ids.len().to_string().as_str()),
                        "case {case} ({surface}): expected ActiveTrustedKeyGroups.Quantity {}, \
                         got {quantity:?}",
                        expected_group_ids.len()
                    );
                    let signers = active_signers(distribution);
                    assert_eq!(
                        signers, expected_accounts,
                        "case {case} ({surface}): expected the active signers \
                         {expected_accounts:?}, got {signers:?}"
                    );
                    let quantity = distribution.text_at("ActiveTrustedSigners/Quantity");
                    assert_eq!(
                        quantity,
                        Some(expected_accounts.len().to_string().as_str()),
                        "case {case} ({surface}): expected ActiveTrustedSigners.Quantity {}, \
                         got {quantity:?}",
                        expected_accounts.len()
                    );
                }
            }
        }
    }
}

/// `GET /2020-05-31/distribution/{Id}/config` — the configuration as the service stored it.
async fn get_config(router: &MultiServiceRouter, id: &str) -> Answer {
    call(
        router,
        "GET",
        &format!("/{API_VERSION}/distribution/{id}/config"),
        None,
        None,
    )
    .await
}

/// The ordered cache behaviours a stored configuration reports, as their path patterns in
/// document order.
fn behavior_patterns(config: &Node) -> Vec<String> {
    config
        .at("CacheBehaviors/Items")
        .map(|items| {
            items
                .children_named("CacheBehavior")
                .into_iter()
                .map(|behavior| {
                    behavior
                        .text_at("PathPattern")
                        .unwrap_or_default()
                        .to_string()
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The ordered `CacheBehaviors` list is bounded by the cache-behaviour quota and by nothing
/// else: a configuration at the ceiling is created and reports its behaviours back in the
/// order it submitted them, one past the ceiling is refused with `TooManyCacheBehaviors`, and
/// the default cache behaviour every configuration carries is charged against neither.
#[tokio::test]
async fn the_cache_behavior_quota_bounds_the_ordered_list_and_preserves_its_order() {
    let router = emulator();

    // (case, how many ordered behaviours the configuration states, whether it is created)
    let rows: [(&str, usize, bool); 5] = [
        ("no ordered behaviour at all", 0, true),
        ("one ordered behaviour", 1, true),
        ("a realistic multi-behaviour configuration", 26, true),
        ("the ceiling itself", BEHAVIOR_CEILING, true),
        ("one past the ceiling", BEHAVIOR_CEILING + 1, false),
    ];

    for (case, count, is_created) in rows {
        let ordered: Vec<Behavior> = vec![Behavior::default(); count];
        let body = config_xml(
            &format!("ref-behaviors-{count}"),
            &Behavior::default(),
            &ordered,
            &[],
        );
        let answer = create_distribution(&router, &body).await;

        if !is_created {
            assert_eq!(
                answer.status, 400,
                "case {case}: expected HTTP 400 for {count} behaviours, got {} with {}",
                answer.status,
                answer.raw.chars().take(400).collect::<String>()
            );
            assert_eq!(
                answer.error_code(),
                "TooManyCacheBehaviors",
                "case {case}: expected TooManyCacheBehaviors, got {:?}",
                answer.error_code()
            );
            let message = answer.root().find_text("Message").unwrap_or_default();
            assert_eq!(
                message, TOO_MANY_CACHE_BEHAVIORS,
                "case {case}: expected the sentence {TOO_MANY_CACHE_BEHAVIORS:?}, got \
                 {message:?}"
            );
            continue;
        }

        assert_eq!(
            answer.status, 201,
            "case {case}: expected HTTP 201 for {count} behaviours, got {} with {}",
            answer.status,
            answer.raw.chars().take(400).collect::<String>()
        );
        let id = answer
            .root()
            .text_at("Id")
            .unwrap_or_default()
            .to_string();
        let expected: Vec<String> = (0..count).map(|index| format!("/path-{index}/*")).collect();

        let read = get_config(&router, &id).await;
        assert_eq!(
            read.status, 200,
            "case {case}: expected GetDistributionConfig to serve the configuration, got {} \
             with {}",
            read.status,
            read.raw.chars().take(400).collect::<String>()
        );
        let config = read.root();
        let quantity = config.text_at("CacheBehaviors/Quantity");
        assert_eq!(
            quantity,
            Some(count.to_string().as_str()),
            "case {case}: expected CacheBehaviors.Quantity {count}, got {quantity:?}"
        );
        let patterns = behavior_patterns(config);
        assert_eq!(
            patterns, expected,
            "case {case}: expected the behaviours back in the submitted order, got \
             {patterns:?}"
        );
    }
}
