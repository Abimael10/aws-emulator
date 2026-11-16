// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the refusal a MIXED `ParameterNameValues` batch carries.
//!
//! The rule under test: a parameter value the published `Parameter$AllowedValues` of its
//! catalog row do not cover is refused HTTP 400 `InvalidParameterValue` in whichever of two
//! sibling templates that published set's own KIND calls for — a set that BOUNDS A NUMBER is
//! reported `value:<the value> is outside of range:<the set>`, and a set that ENUMERATES
//! literal values is reported `invalid parameter value, allowed values are:<the set>`, which
//! does not echo the rejected value. The template is the refused PARAMETER's, whichever
//! position it holds in the batch and whatever the pairs beside it are, because in a batch
//! the message is the only thing that says which member was refused — and the whole request
//! changes nothing, so every parameter of the group keeps the value and `Source` it had.
//!
//! The rows differ along what the rule generalises over: the KIND of the refused set (an
//! enumeration of words, an enumeration of flags, a bottom-bounded range, a range bounded at
//! both ends), the POSITION of the refused pair in the batch (first, last, between two valid
//! pairs), the size of the batch, and the engine catalog the family draws on. The published
//! set each message must carry is READ from `DescribeCacheParameters` rather than written
//! out here, so a row cannot pass by agreeing with a literal; the template text itself is
//! written out on purpose, because a test that built it from the helper the handler calls
//! would pass whatever that helper said.
//!
//! Every assertion goes over the wire through [`crate::harness`], driving the writer and the
//! reader through the helpers the `ModifyCacheParameterGroup` and `DescribeCacheParameters`
//! suites already share.

use service_com_amazonaws_elasticache_amazonelasticachev9::state;

use crate::describe_cache_parameters::{create_group, describe_ok, one};
use crate::harness::Emulator;
use crate::modify_cache_parameter_group::{effective, modify, refused};

/// The families the rows draw their parameters from, one per engine catalog.
const REDIS_FAMILY: &str = "redis7";
const MEMCACHED_FAMILY: &str = "memcached1.6";

/// Which of the two templates the refused parameter's published set calls for.
#[derive(Debug, Clone, Copy)]
enum Template {
    /// The set bounds a number, so the rejected value is reported beside the bounds.
    OutsideRange,
    /// The set enumerates literal values, so the values allowed are listed instead.
    AllowedValues,
}

/// One batch row: what it is called, the group it is sent to, the pairs of the batch in the
/// order they are sent, the parameter whose value must be the one refused, and the template
/// that parameter's published set calls for.
struct BatchCase {
    case: &'static str,
    group: &'static str,
    pairs: &'static [(&'static str, &'static str)],
    refused_parameter: &'static str,
    template: Template,
}

/// The published `AllowedValues` of `parameter` of `group`, as a read reports them — the set
/// a refusal must name is the one a client can see.
async fn published_set(emulator: &Emulator, case: &str, group: &str, parameter: &str) -> String {
    let reply = describe_ok(emulator, group, &[]).await;
    let allowed = one(&reply, case, parameter)
        .field("AllowedValues")
        .unwrap_or_default()
        .to_string();
    assert!(
        !allowed.is_empty(),
        "case {case}: this row needs {parameter} to publish a value set to be outside of"
    );
    allowed
}

#[tokio::test]
async fn a_mixed_batch_is_refused_in_the_template_of_the_set_its_bad_pair_is_outside() {
    let emulator = Emulator::new();
    create_group(&emulator, "batch-redis", REDIS_FAMILY).await;
    create_group(&emulator, "batch-memcached", MEMCACHED_FAMILY).await;

    let rows: &[BatchCase] = &[
        BatchCase {
            case: "an enumeration of words refused last",
            group: "batch-redis",
            pairs: &[("timeout", "300"), ("maxmemory-policy", "not-a-policy")],
            refused_parameter: "maxmemory-policy",
            template: Template::AllowedValues,
        },
        BatchCase {
            case: "an enumeration of words refused first",
            group: "batch-redis",
            pairs: &[("maxmemory-policy", "LRU"), ("timeout", "300")],
            refused_parameter: "maxmemory-policy",
            template: Template::AllowedValues,
        },
        BatchCase {
            case: "an enumeration of flags refused between two valid pairs",
            group: "batch-redis",
            pairs: &[
                ("timeout", "300"),
                ("acl-pubsub-default", "every-channel"),
                ("slowlog-max-len", "128"),
            ],
            refused_parameter: "acl-pubsub-default",
            template: Template::AllowedValues,
        },
        BatchCase {
            case: "a bottom-bounded range refused last",
            group: "batch-redis",
            pairs: &[("maxmemory-policy", "noeviction"), ("slowlog-max-len", "-5")],
            refused_parameter: "slowlog-max-len",
            template: Template::OutsideRange,
        },
        BatchCase {
            case: "a range bounded at both ends refused first",
            group: "batch-redis",
            pairs: &[
                ("active-defrag-cycle-max", "76"),
                ("maxmemory-policy", "noeviction"),
                ("timeout", "300"),
            ],
            refused_parameter: "active-defrag-cycle-max",
            template: Template::OutsideRange,
        },
        BatchCase {
            case: "an enumeration of numbers in the other engine's catalog",
            group: "batch-memcached",
            pairs: &[("max_item_size", "2048"), ("slab_automove", "3")],
            refused_parameter: "slab_automove",
            template: Template::AllowedValues,
        },
    ];

    for BatchCase {
        case,
        group,
        pairs,
        refused_parameter,
        template,
    } in rows
    {
        let allowed = published_set(&emulator, case, group, refused_parameter).await;
        let refused_value = pairs
            .iter()
            .find(|(name, _)| name == refused_parameter)
            .map(|(_, value)| *value)
            .unwrap_or_else(|| panic!("case {case}: the row must send the refused parameter"));
        // What every parameter of the batch reports BEFORE the request, so the reads below
        // show the whole batch was left unapplied.
        let mut before = Vec::with_capacity(pairs.len());
        for (name, _) in *pairs {
            before.push((*name, effective(&emulator, case, group, name).await));
        }

        let reply = modify(&emulator, group, pairs).await;
        let message = refused(case, &reply, 400, "InvalidParameterValue").to_string();
        let expected = match template {
            Template::OutsideRange => {
                format!("value:{refused_value} is outside of range:{allowed}")
            }
            Template::AllowedValues => {
                format!("invalid parameter value, allowed values are:{allowed}")
            }
        };
        assert_eq!(
            message, expected,
            "case {case}: expected message {expected:?}, got {message:?}"
        );

        for (name, was) in before {
            let now = effective(&emulator, case, group, name).await;
            assert_eq!(
                now, was,
                "case {case}: the refused batch must leave {name} at {was:?}, got {now:?}"
            );
            assert_eq!(
                now.1,
                state::PARAMETER_SOURCE_SYSTEM,
                "case {case}: {name} must still report the engine's own value, got {now:?}"
            );
        }
    }
}
