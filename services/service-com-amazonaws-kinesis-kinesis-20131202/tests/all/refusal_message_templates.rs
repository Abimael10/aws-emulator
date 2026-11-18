// SPDX-License-Identifier: CC0-1.0

//! Two refusals this service words from AWS's own FIXED templates, end to end over the wire:
//! a `CreateStream` whose name this account and region already hold a stream under, and a
//! `GetShardIterator` whose `ShardIteratorType` and `StartingSequenceNumber` do not go
//! together.
//!
//! RULE: an error code and an error message are part of the contract, so each refusal carries
//! the template AWS words that error CLASS with — the fixed words exactly, and every variable
//! part taken from the request and from the emulator's own state, nothing else:
//!
//! * a name this account and region already hold a stream under is the modelled
//!   `ResourceInUseException` `Stream <streamName> under account <accountId> already exists.`
//!   — the name from the request and the 12-digit account id from state, in the same spans
//!   every other "under account" sentence of this service uses. A stream name is scoped by
//!   region ("The stream name identifies the stream... It is also scoped by Amazon Web
//!   Services Region", `CreateStream`), so the SAME name in another region is served instead;
//! * an iterator type and a sequence number that do not go together is the modelled
//!   `InvalidArgumentException` enumerating the two legal combinations and then restating
//!   what the request named: `Must either specify (1) AT_SEQUENCE_NUMBER or
//!   AFTER_SEQUENCE_NUMBER and <member> or (2) TRIM_HORIZON or LATEST and no <member>.
//!   Request specified <shardIteratorType> and <no <member>|<member>>.` — one sentence for
//!   the one rule, with the member the request spells a sequence number with (this operation's
//!   `StartingSequenceNumber`; the other reader of a shard spells it
//!   `StartingPosition.SequenceNumber`, which [`crate::enhanced_fanout_subscriptions`]
//!   pins);
//! * an operation served only for an account holding a minimum throughput billing commitment
//!   refuses an account that holds none with the modelled `ValidationException` `Account
//!   <accountId> is not enabled for minimum throughput billing commitment` — the account id
//!   from state is the only variable part — and it does so BEFORE the request's stream is
//!   resolved, so the answer does not depend on whether that stream exists;
//! * a `DeleteResourcePolicy` for a resource that carries no policy is the modelled
//!   `ResourceNotFoundException` `No resource policy found for resource ARN <resourceARN>.` —
//!   the ARN the request addressed the resource by is the one variable part, whichever
//!   resource kind it names;
//! * a `DeleteStream` of a stream that still carries registered consumers, with
//!   `EnforceConsumerDeletion` unset or false, is the modelled `ResourceInUseException`
//!   `DeleteStream can’t delete <accountId>:<streamName> because there are consumers
//!   associated with this data stream. To delete <streamName> anyway, set the
//!   EnforceConsumerDeletion parameter to true to force the deletion of all the consumers that
//!   are associated with it.` — the account id from state and the stream's name are its only
//!   variable parts;
//! * a request asking to write a tag key inside the namespace AWS reserves for itself (one
//!   opening with `aws:`) is the modelled `InvalidArgumentException` `Some tag keys in the
//!   request started with 'aws:'. System tags cannot be modified.` — entirely fixed text — on
//!   every way into a resource's tags, leaving the stored tag set as it was and provisioning
//!   nothing when the request was a create;
//! * a `GetRecords` whose `ShardIterator` is past its age is the modelled
//!   `ExpiredIteratorException` `Iterator expired. The iterator was created at time <created>
//!   while right now it is <now> which is further in the future than the tolerated delay of
//!   300000 milliseconds.` — the mint instant from the iterator the request carried and the
//!   server's own instant, both spelled `EEE MMM dd HH:mm:ss UTC yyyy`;
//! * a listing whose `NextToken` is past its lifetime is the modelled
//!   `ExpiredNextTokenException` `NextToken expired at time <expiry>. Invoke <Operation> again
//!   to get a new pagination token.` — the instant the token lapsed (its mint instant plus the
//!   cursor lifetime), spelled in that same layout, and the listing that was invoked.
//!
//! The recorded cases this was derived from are one row each — one duplicate create, one
//! `AT_SEQUENCE_NUMBER` read carrying no sequence number, one second removal of an already
//! removed policy, one blocked delete and one reserved tag key. The other rows differ along
//! what the rule generalises over: the form and length of the stream name, the capacity mode
//! the held stream was created in, the lifecycle status it is in when the second create
//! arrives, the region the create is signed for, the resource KIND a `ResourceARN` names, how
//! many consumers a stream carries and which state they are in, the identifier a delete
//! addresses its stream by, which operation carries a tag map and what follows the reserved
//! prefix in a key — and, for the reader, every `ShardIteratorType` the model declares, each
//! with and without a sequence number, so both the refused and the served combinations are
//! covered.
//!
//! Every case drives the real awsJson1_1 request path (`POST /` with `X-Amz-Target:
//! Kinesis_20131202.<Operation>` through `MultiServiceRouter::dispatch`), so the statuses,
//! bodies and modelled errors asserted here are the ones a client sees.
//!
//! The wire harness and the ARN builder are [`crate::stream_tags`]', the channel helpers
//! [`crate::channel_lifecycle`]' and the resource-policy calls [`crate::resource_policies`]',
//! reused rather than copied.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use service_com_amazonaws_kinesis_kinesis_20131202::state::{
    account_not_enabled_for_commitment_message, channel_arn, shard_id,
    stream_already_exists_message, ConsumerPageToken, RegionalListing, RegionalPageToken,
    ShardIteratorToken, ShardPageToken, CONSUMER_ACTIVE, STREAM_MODE_ON_DEMAND,
};

use crate::channel_lifecycle::{
    on_demand_stream, role_arn, storage_configuration, stream_configuration,
};
use crate::resource_policies::{delete as delete_policy, policy_for, put as put_policy};
use crate::stream_tags::{
    error_code, error_message, poll_until, poll_until_active, stream_arn, tag_map, Emulator,
    REGION,
};
use crate::warm_throughput::commit_the_account;

/// The `@length` maximum of `StreamName`, in characters
/// (`smithy_query(query="shape",shape="com.amazonaws.kinesis#StreamName")` -> len 1..128,
/// pattern `^[a-zA-Z0-9_.-]+$`).
const STREAM_NAME_MAX: usize = 128;

/// Every value `ShardIteratorType` admits
/// (`smithy_query(query="shape",shape="com.amazonaws.kinesis#ShardIteratorType")`), which is
/// the catalog the reader rows below are written over.
const SHARD_ITERATOR_TYPES: [&str; 5] = [
    "AT_SEQUENCE_NUMBER",
    "AFTER_SEQUENCE_NUMBER",
    "TRIM_HORIZON",
    "LATEST",
    "AT_TIMESTAMP",
];

/// The code and the message `operation` refuses `body` with, asserting the modelled client
/// status.
async fn refusal(
    emulator: &Emulator,
    case: &str,
    operation: &str,
    body: &Value,
) -> (String, String) {
    let (status, response) = emulator.call(operation, body).await;
    assert_eq!(
        status, 400,
        "case {case}: expected the modelled client refusal, got {status} with body {response}"
    );
    (error_code(&response), error_message(&response))
}

/// The sentence AWS words a duplicate create with, written out here rather than borrowed from
/// the crate, so the wording itself is what these cases pin.
fn already_exists(name: &str) -> String {
    format!(
        "Stream {name} under account {} already exists.",
        engine::EMULATOR_ACCOUNT_ID
    )
}

/// A create whose name this account and region already hold a stream under is refused with
/// AWS's `already exists` template — whatever the name looks like, whichever capacity mode
/// either create names, and whether the held stream is still `CREATING` or already `ACTIVE` —
/// while the same name in another region is a stream of its own.
#[tokio::test]
async fn a_duplicate_create_states_the_name_and_the_account() {
    let emulator = Emulator::new();

    // (case, the stream's name, the body the first create sends, the body the second sends).
    // The names differ in length and in the punctuation the member's `@pattern` admits, and
    // the bodies differ in the capacity mode they name, so the sentence is the rule's and not
    // one shape of request's.
    let longest = "d".repeat(STREAM_NAME_MAX);
    let rows: Vec<(&str, String, Value, Value)> = vec![
        (
            "a provisioned stream created twice",
            "dup-provisioned".to_string(),
            json!({ "ShardCount": 1 }),
            json!({ "ShardCount": 1 }),
        ),
        (
            "an on-demand stream created twice",
            "dup.on_demand-stream".to_string(),
            json!({ "StreamModeDetails": { "StreamMode": STREAM_MODE_ON_DEMAND } }),
            json!({ "StreamModeDetails": { "StreamMode": STREAM_MODE_ON_DEMAND } }),
        ),
        (
            "a provisioned name asked for in the on-demand mode",
            "dup-mode-crossed".to_string(),
            json!({ "ShardCount": 2 }),
            json!({ "StreamModeDetails": { "StreamMode": STREAM_MODE_ON_DEMAND } }),
        ),
        (
            "the second create naming more shards and tags",
            "dup-richer-second-create".to_string(),
            json!({ "ShardCount": 1 }),
            json!({ "ShardCount": 3, "Tags": { "owner": "second" } }),
        ),
        (
            "the longest name the model admits",
            longest,
            json!({ "ShardCount": 1 }),
            json!({ "ShardCount": 1 }),
        ),
    ];

    for (case, name, first, _) in &rows {
        let mut body = first.clone();
        body["StreamName"] = json!(name);
        emulator.ok(case, "CreateStream", &body).await;
    }

    // While the held stream is still inside the transition its create armed, the name is
    // already taken: the refusal is about the NAME and not about the stream being ready.
    for (case, name, _, second) in &rows {
        let case = format!("{case}, while the held stream is still creating");
        let mut body = second.clone();
        body["StreamName"] = json!(name);
        let (code, message) = refusal(&emulator, &case, "CreateStream", &body).await;
        assert_eq!(
            code, "ResourceInUseException",
            "case {case}: expected the modelled conflict, got {code} with {message:?}"
        );
        assert_eq!(
            message,
            already_exists(name),
            "case {case}: expected AWS's already-exists sentence, got {message:?}"
        );
        assert_eq!(
            message,
            stream_already_exists_message(name),
            "case {case}: the crate's own sentence is the one the wire carries"
        );
    }

    // And once it is ACTIVE, the same name is refused with the same sentence. The CONDITION
    // each held stream reaches on the emulator's own clock is what is waited for, never a
    // duration this case measures.
    for (case, name, _, _) in &rows {
        poll_until_active(&emulator, case, name).await;
    }
    for (case, name, _, second) in &rows {
        let case = format!("{case}, once the held stream is active");
        let mut body = second.clone();
        body["StreamName"] = json!(name);
        let (code, message) = refusal(&emulator, &case, "CreateStream", &body).await;
        assert_eq!(
            code, "ResourceInUseException",
            "case {case}: expected the modelled conflict, got {code} with {message:?}"
        );
        assert_eq!(
            message,
            already_exists(name),
            "case {case}: expected AWS's already-exists sentence, got {message:?}"
        );
    }

    // The other half of the rule: a stream name is scoped by region, so the same name signed
    // for another region names no stream yet and the create is SERVED.
    for (case, name, first, _) in &rows {
        let mut body = first.clone();
        body["StreamName"] = json!(name);
        for region in ["us-west-2", "eu-central-1"] {
            assert_ne!(
                region, REGION,
                "case {case}: the served rows must be signed for another region"
            );
            emulator.ok_in(case, region, "CreateStream", &body).await;
        }
    }
}

/// A `GetShardIterator` whose `ShardIteratorType` and `StartingSequenceNumber` do not go
/// together is refused with AWS's two-clause template — the legal combinations enumerated,
/// then the type the request named and whether it carried a sequence number — while every
/// combination that DOES go together is served.
#[tokio::test]
async fn an_inconsistent_iterator_type_and_sequence_number_states_both_combinations() {
    let emulator = Emulator::new();
    let stream = "iterator-combinations";
    emulator
        .ok(
            "setup",
            "CreateStream",
            &json!({ "StreamName": stream, "ShardCount": 1 }),
        )
        .await;
    poll_until_active(&emulator, "setup", stream).await;

    // A record, so the sequence number the rows carry is one the shard really holds: a value
    // outside the shard's published range is a different refusal of this same error class.
    let sequence_number = emulator
        .ok(
            "setup",
            "PutRecord",
            &json!({
                "StreamName": stream,
                "PartitionKey": "pk",
                "Data": aws_smithy_types::base64::encode("x"),
            }),
        )
        .await["SequenceNumber"]
        .as_str()
        .expect("PutRecord answers the sequence number it stored")
        .to_string();

    // Every `ShardIteratorType` the model declares, with and without a sequence number: the
    // two sequence-number types need one, the three others take none, and `AT_TIMESTAMP`
    // carries its own instant so that only the sequence number is at fault in its refused row.
    for shard_iterator_type in SHARD_ITERATOR_TYPES {
        let takes_sequence_number = shard_iterator_type == "AT_SEQUENCE_NUMBER"
            || shard_iterator_type == "AFTER_SEQUENCE_NUMBER";
        for carried_sequence_number in [false, true] {
            let case = format!(
                "{shard_iterator_type} with{} a sequence number",
                if carried_sequence_number { "" } else { "out" }
            );
            let mut body = json!({
                "StreamName": stream,
                "ShardId": shard_id(0),
                "ShardIteratorType": shard_iterator_type,
            });
            if carried_sequence_number {
                body["StartingSequenceNumber"] = json!(sequence_number);
            }
            if shard_iterator_type == "AT_TIMESTAMP" {
                body["Timestamp"] = json!(1_700_000_000);
            }

            if takes_sequence_number == carried_sequence_number {
                // The combination goes together, so an iterator is handed out: the refusal
                // below is about the combination and not about the member being there at all.
                let served = emulator.ok(&case, "GetShardIterator", &body).await;
                assert!(
                    served
                        .get("ShardIterator")
                        .and_then(Value::as_str)
                        .is_some_and(|iterator| !iterator.is_empty()),
                    "case {case}: a combination the type admits is served an iterator, got \
                     {served}"
                );
                continue;
            }

            let (code, message) = refusal(&emulator, &case, "GetShardIterator", &body).await;
            assert_eq!(
                code, "InvalidArgumentException",
                "case {case}: expected the modelled invalid-argument, got {code} with \
                 {message:?}"
            );
            let expected = format!(
                "Must either specify (1) AT_SEQUENCE_NUMBER or AFTER_SEQUENCE_NUMBER and \
                 StartingSequenceNumber or (2) TRIM_HORIZON or LATEST and no \
                 StartingSequenceNumber. Request specified {shard_iterator_type} and \
                 {}StartingSequenceNumber.",
                if carried_sequence_number { "" } else { "no " }
            );
            assert_eq!(
                message, expected,
                "case {case}: expected AWS's two-clause template, got {message:?}"
            );
        }
    }
}

/// `UpdateStreamWarmThroughput` is served only for an account holding a minimum throughput
/// billing commitment, and that ACCOUNT-level precondition is judged before the request's
/// stream is resolved: every request an account holding none sends is refused with the one
/// sentence, whatever stream it names and whatever else about it is wrong, and the same
/// requests are judged on their own merits once the commitment is held.
#[tokio::test]
async fn the_account_commitment_is_judged_before_the_stream_is_resolved() {
    let emulator = Emulator::new();
    let on_demand = "commitment-on-demand";
    let provisioned = "commitment-provisioned";
    emulator
        .ok(
            "setup",
            "CreateStream",
            &json!({
                "StreamName": on_demand,
                "StreamModeDetails": { "StreamMode": STREAM_MODE_ON_DEMAND },
            }),
        )
        .await;
    emulator
        .ok(
            "setup",
            "CreateStream",
            &json!({ "StreamName": provisioned, "ShardCount": 1 }),
        )
        .await;
    poll_until_active(&emulator, "setup", on_demand).await;
    poll_until_active(&emulator, "setup", provisioned).await;

    // (case, the request body). The rows differ in the stream they address and in what ELSE
    // the request gets wrong — a stream nothing holds, an ARN nothing holds, a stream that
    // would be served, a stream whose capacity mode does not admit the change, a request
    // naming no stream at all, and a target past the published ceiling — because an account
    // that holds no commitment is refused before any of that is reached.
    let rows: Vec<(&str, Value)> = vec![
        (
            "a stream name nothing holds",
            json!({ "StreamName": "commitment-absent", "WarmThroughputMiBps": 1 }),
        ),
        (
            "a stream ARN nothing holds",
            json!({
                "StreamARN": format!(
                    "arn:aws:kinesis:{REGION}:{}:stream/commitment-absent-too",
                    engine::EMULATOR_ACCOUNT_ID
                ),
                "WarmThroughputMiBps": 64,
            }),
        ),
        (
            "an on-demand stream that would be served",
            json!({ "StreamName": on_demand, "WarmThroughputMiBps": 64 }),
        ),
        (
            "a stream whose capacity mode does not admit the change",
            json!({ "StreamName": provisioned, "WarmThroughputMiBps": 64 }),
        ),
        (
            "a request naming no stream at all",
            json!({ "WarmThroughputMiBps": 64 }),
        ),
        (
            "a target past the published ceiling",
            json!({ "StreamName": on_demand, "WarmThroughputMiBps": 1_000_000 }),
        ),
    ];

    for (case, body) in &rows {
        let (code, message) = refusal(&emulator, case, "UpdateStreamWarmThroughput", body).await;
        assert_eq!(
            code, "ValidationException",
            "case {case}: expected the modelled validation refusal, got {code} with {message:?}"
        );
        assert_eq!(
            message,
            format!(
                "Account {} is not enabled for minimum throughput billing commitment",
                engine::EMULATOR_ACCOUNT_ID
            ),
            "case {case}: expected AWS's account-precondition sentence, got {message:?}"
        );
        assert_eq!(
            message,
            account_not_enabled_for_commitment_message(),
            "case {case}: the crate's own sentence is the one the wire carries"
        );
    }

    // Once the account holds the commitment, the precondition is out of the way and each of
    // those requests is judged on its own: the on-demand stream is SERVED, and the other rows
    // are refused with the sentence their own fault earns instead.
    commit_the_account(&emulator).await;
    let served = emulator
        .ok(
            "an on-demand stream, with the commitment held",
            "UpdateStreamWarmThroughput",
            &json!({ "StreamName": on_demand, "WarmThroughputMiBps": 64 }),
        )
        .await;
    assert_eq!(
        served["WarmThroughput"]["TargetMiBps"],
        json!(64),
        "case an on-demand stream, with the commitment held: expected the change to be served, \
         got {served}"
    );
    let (code, message) = refusal(
        &emulator,
        "a stream name nothing holds, with the commitment held",
        "UpdateStreamWarmThroughput",
        &json!({ "StreamName": "commitment-absent", "WarmThroughputMiBps": 1 }),
    )
    .await;
    assert_eq!(
        (code.as_str(), message.as_str()),
        (
            "ResourceNotFoundException",
            format!(
                "Stream commitment-absent under account {} not found.",
                engine::EMULATOR_ACCOUNT_ID
            )
            .as_str()
        ),
        "case a stream name nothing holds, with the commitment held: expected the stream \
         lookup to be reached once the account precondition is met"
    );
}

/// AWS's sentence for a `DeleteResourcePolicy` that finds no policy on the resource the
/// request named: fixed text with the request's own `ResourceARN` as its one variable part —
/// the resource is addressed by that ARN, which carries the account it belongs to already, so
/// nothing else of the caller appears in it. Spelled out here rather than borrowed from the
/// crate, so the wording itself is what these cases pin.
fn no_resource_policy(arn: &str) -> String {
    format!("No resource policy found for resource ARN {arn}.")
}

/// A delete finds no policy on a resource that carries none — whichever resource KIND the
/// `ResourceARN` names, whether the resource never carried one or a first delete removed it —
/// and states that resource's ARN and nothing else.
#[tokio::test]
async fn a_delete_finding_no_policy_states_the_resource_arn() {
    let emulator = Emulator::new();
    let stream = on_demand_stream(&emulator, "policy-absent-source").await;

    // A consumer of that stream, whose ARN is the stream's extended, and a channel sourcing
    // it: the three resource kinds a `ResourceARN` may name
    // (`^arn:aws.*:kinesis:.*:\d{12}:.*(stream|channel)/\S+$` and the consumer form beside
    // it), so the sentence is the rule's rather than one ARN form's.
    let registered = emulator
        .ok(
            "consumer",
            "RegisterStreamConsumer",
            &json!({ "StreamARN": stream, "ConsumerName": "policy-absent-consumer" }),
        )
        .await;
    let consumer = registered["Consumer"]["ConsumerARN"]
        .as_str()
        .unwrap_or_else(|| panic!("the registration must carry a ConsumerARN, got {registered}"))
        .to_string();

    let channel_name = "policy-absent-channel";
    emulator
        .ok(
            "channel",
            "CreateChannel",
            &json!({
                "ChannelName": channel_name,
                "ServiceExecutionRoleARN": role_arn("channel-delivery"),
                "StreamConfigurationList": [stream_configuration(&stream, "JSON", None)],
                "S3DestinationConfiguration": {
                    "StorageConfiguration": storage_configuration("policy-absent-bucket"),
                },
            }),
        )
        .await;
    let channel = channel_arn(REGION, channel_name);

    let rows: Vec<(&str, String)> = vec![
        ("a data stream", stream.clone()),
        ("a registered consumer", consumer),
        ("a channel", channel),
    ];
    for (case, arn) in &rows {
        // A resource that never carried a policy: the delete has nothing to remove.
        let (status, response) = delete_policy(&emulator, REGION, arn).await;
        assert_eq!(
            status, 400,
            "case {case}/never attached: expected the modelled refusal, got {status} with body \
             {response}"
        );
        assert_eq!(
            error_code(&response),
            "ResourceNotFoundException",
            "case {case}/never attached: expected ResourceNotFoundException, got {response}"
        );
        assert_eq!(
            error_message(&response),
            no_resource_policy(arn),
            "case {case}/never attached: expected AWS's no-policy sentence naming {arn}, got \
             {response}"
        );

        // The recorded flow: attach, remove, and remove again — the second removal finds no
        // policy and states the very same sentence.
        let (status, response) = put_policy(
            &emulator,
            REGION,
            arn,
            &policy_for(arn, "AbsenceLifecycle", "kinesis:DescribeStreamSummary"),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case}/attach: expected the attach to be served, got {status} with body \
             {response}"
        );
        let (status, response) = delete_policy(&emulator, REGION, arn).await;
        assert_eq!(
            status, 200,
            "case {case}/first delete: expected the removal to be served, got {status} with \
             body {response}"
        );
        let (status, response) = delete_policy(&emulator, REGION, arn).await;
        assert_eq!(
            status, 400,
            "case {case}/second delete: expected the modelled refusal, got {status} with body \
             {response}"
        );
        assert_eq!(
            error_message(&response),
            no_resource_policy(arn),
            "case {case}/second delete: expected AWS's no-policy sentence naming {arn}, got \
             {response}"
        );
    }
}

/// AWS's sentence for a `DeleteStream` a stream's registered consumers stand in the way of:
/// fixed text — note the typographic apostrophe in "can’t" — whose variable parts are the
/// 12-digit account id and the stream's name, each appearing where the template puts it.
/// Spelled out here rather than borrowed from the crate, so the wording itself is what these
/// cases pin.
fn stream_has_consumers(stream_name: &str) -> String {
    format!(
        "DeleteStream can\u{2019}t delete {}:{stream_name} because there are consumers \
         associated with this data stream. To delete {stream_name} anyway, set the \
         EnforceConsumerDeletion parameter to true to force the deletion of all the consumers \
         that are associated with it.",
        engine::EMULATOR_ACCOUNT_ID
    )
}

/// Look at the consumer `name` of the stream `stream_arn` until it reports `ACTIVE` — the
/// state its registration owes it — through the crate's one poll loop.
async fn poll_until_consumer_active(emulator: &Emulator, case: &str, stream_arn: &str, name: &str) {
    poll_until(
        case,
        &format!("the consumer {name} reports {CONSUMER_ACTIVE}"),
        || async {
            let (status, response) = emulator
                .call(
                    "DescribeStreamConsumer",
                    &json!({ "StreamARN": stream_arn, "ConsumerName": name }),
                )
                .await;
            status == 200
                && response["ConsumerDescription"]["ConsumerStatus"].as_str()
                    == Some(CONSUMER_ACTIVE)
        },
    )
    .await;
}

/// A delete of a stream that still carries registered consumers, with the enforcing member
/// unset or false, is refused with AWS's template — whatever the stream's name looks like,
/// whichever capacity mode it was created in, however many consumers it carries, whichever
/// state those consumers are in, and whichever identifier the delete addresses the stream by —
/// and the consumers it names are still registered afterwards.
#[tokio::test]
async fn a_delete_blocked_by_consumers_states_the_account_the_stream_and_the_member() {
    let emulator = Emulator::new();

    // (case, the stream's name, the body its create sends, how many consumers it carries,
    // whether the case waits for those consumers to report ACTIVE). The rows differ along the
    // name's form and length, the capacity mode, the consumer count and the consumer state, so
    // the sentence is the rule's and not one shape of stream's.
    let longest = format!("{}{}", "c".repeat(STREAM_NAME_MAX - 8), "-blocked");
    let rows: Vec<(&str, String, Value, usize, bool)> = vec![
        (
            "an on-demand stream with one ACTIVE consumer",
            "del-block-on-demand".to_string(),
            json!({ "StreamModeDetails": { "StreamMode": STREAM_MODE_ON_DEMAND } }),
            1,
            true,
        ),
        (
            "a provisioned stream with one consumer still CREATING",
            "del.block_provisioned".to_string(),
            json!({ "ShardCount": 2 }),
            1,
            false,
        ),
        (
            "a stream with several ACTIVE consumers",
            "del-block-several".to_string(),
            json!({ "ShardCount": 1 }),
            3,
            true,
        ),
        (
            "a stream whose name is as long as the member admits",
            longest,
            json!({ "ShardCount": 1 }),
            1,
            true,
        ),
    ];

    for (case, name, create, consumers, wait_for_active) in &rows {
        let mut body = create.clone();
        body["StreamName"] = json!(name);
        emulator.ok(case, "CreateStream", &body).await;
        poll_until_active(&emulator, case, name).await;
        let arn = stream_arn(REGION, name);

        let registered: Vec<String> = (0..*consumers)
            .map(|index| format!("del-block-consumer-{index}"))
            .collect();
        for consumer in &registered {
            emulator
                .ok(
                    case,
                    "RegisterStreamConsumer",
                    &json!({ "StreamARN": arn, "ConsumerName": consumer }),
                )
                .await;
            if *wait_for_active {
                poll_until_consumer_active(&emulator, case, &arn, consumer).await;
            }
        }

        // The member unset and the member false are the two forms the refusal covers: "If
        // this parameter is unset (null) or if you set it to false, and the stream has
        // registered consumers, the call to DeleteStream fails"
        // (`DeleteStreamInput$EnforceConsumerDeletion`). The stream is addressed by its name
        // and by its ARN in turn, so the sentence does not depend on how it was addressed.
        let attempts: Vec<(&str, Value)> = vec![
            ("by name, the member unset", json!({ "StreamName": name })),
            (
                "by ARN, the member false",
                json!({ "StreamARN": arn, "EnforceConsumerDeletion": false }),
            ),
        ];
        for (how, request) in &attempts {
            let label = format!("{case}/{how}");
            let (code, message) = refusal(&emulator, &label, "DeleteStream", request).await;
            assert_eq!(
                code, "ResourceInUseException",
                "case {label}: expected ResourceInUseException, got {code} with {message:?}"
            );
            assert_eq!(
                message,
                stream_has_consumers(name),
                "case {label}: expected AWS's consumers-present template, got {message:?}"
            );
        }

        // The refusals changed nothing: every consumer is still registered, and the delete
        // that sets the member is served.
        for consumer in &registered {
            emulator
                .ok(
                    case,
                    "DescribeStreamConsumer",
                    &json!({ "StreamARN": arn, "ConsumerName": consumer }),
                )
                .await;
        }
        emulator
            .ok(
                case,
                "DeleteStream",
                &json!({ "StreamName": name, "EnforceConsumerDeletion": true }),
            )
            .await;
    }
}

/// AWS's sentence for a request that asks to write a tag key inside the namespace AWS keeps
/// for itself: entirely fixed text, with no variable part at all — the offending keys are not
/// quoted back. Spelled out here rather than borrowed from the crate, so the wording itself is
/// what these cases pin.
const RESERVED_TAG_KEYS_SAYS: &str =
    "Some tag keys in the request started with 'aws:'. System tags cannot be modified.";

/// Keys inside the namespace AWS reserves: every one of them OPENS with `aws:`. The rows
/// differ in what follows the prefix — nothing at all, one segment, the several segments a
/// service-applied tag uses, and the punctuation `Tag$Key` documents as valid
/// (`smithy_query(query="shape",shape="Tag")`, "Valid characters: Unicode letters, digits,
/// white space, _ . / = + - % @").
fn reserved_tag_keys() -> Vec<(&'static str, &'static str)> {
    vec![
        ("the bare prefix", "aws:"),
        ("one segment after the prefix", "aws:reserved-tag"),
        ("a service-applied key", "aws:cloudformation:stack-name"),
        ("punctuation after the prefix", "aws:cost/center=1"),
    ]
}

/// The tags the resource `arn` carries, read through `ListTagsForResource` as pairs ordered by
/// key. `Tags` is optional there, so a resource carrying none reads as an empty set.
async fn tags_of(emulator: &Emulator, case: &str, arn: &str) -> Vec<(String, String)> {
    let response = emulator
        .ok(case, "ListTagsForResource", &json!({ "ResourceARN": arn }))
        .await;
    let mut reported: Vec<(String, String)> = response
        .get("Tags")
        .and_then(Value::as_array)
        .map_or(&[] as &[Value], Vec::as_slice)
        .iter()
        .map(|tag| {
            let key = tag["Key"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case}: a Tag must carry a Key, got {tag}"));
            let value = tag.get("Value").and_then(Value::as_str).unwrap_or_default();
            (key.to_string(), value.to_string())
        })
        .collect();
    reported.sort();
    reported
}

/// A request that asks to write a key inside the reserved namespace is refused with AWS's
/// fixed sentence — on every way into a resource's tags, whatever the key looks like after the
/// prefix, and whether it arrives alone or beside ordinary keys — and the stored tag set is
/// left exactly as it was.
#[tokio::test]
async fn a_reserved_tag_key_is_refused_on_every_tag_write() {
    let emulator = Emulator::new();
    let name = "reserved-tag-writes";
    emulator
        .ok(
            name,
            "CreateStream",
            &json!({ "StreamName": name, "ShardCount": 1 }),
        )
        .await;
    poll_until_active(&emulator, name, name).await;
    let arn = stream_arn(REGION, name);

    // The one ordinary tag the stream carries before anything is refused, so each refusal is
    // checked against a set it could have changed.
    let held = vec![("Environment".to_string(), "production".to_string())];
    emulator
        .ok(
            name,
            "AddTagsToStream",
            &json!({ "StreamName": name, "Tags": tag_map(&held) }),
        )
        .await;

    for (label, key) in reserved_tag_keys() {
        // The reserved key ALONE, and the same key beside an ordinary one: the map is refused
        // as a whole either way, so the ordinary pair is not stored on its own.
        let alone = tag_map(&[(key.to_string(), "x".to_string())]);
        let beside = tag_map(&[
            ("Owner".to_string(), "platform".to_string()),
            (key.to_string(), "x".to_string()),
        ]);
        let requests: Vec<(&str, &str, Value)> = vec![
            (
                "AddTagsToStream",
                "alone",
                json!({ "StreamName": name, "Tags": alone }),
            ),
            (
                "TagResource",
                "alone",
                json!({ "ResourceARN": arn, "Tags": alone }),
            ),
            (
                "AddTagsToStream",
                "beside an ordinary key",
                json!({ "StreamName": name, "Tags": beside }),
            ),
            (
                "TagResource",
                "beside an ordinary key",
                json!({ "ResourceARN": arn, "Tags": beside }),
            ),
        ];
        for (operation, how, request) in &requests {
            let label = format!("{operation}/{label}/{how}");
            let (code, message) = refusal(&emulator, &label, operation, request).await;
            assert_eq!(
                code, "InvalidArgumentException",
                "case {label}: expected InvalidArgumentException, got {code} with {message:?}"
            );
            assert_eq!(
                message, RESERVED_TAG_KEYS_SAYS,
                "case {label}: expected AWS's reserved-namespace sentence, got {message:?}"
            );
            let reported = tags_of(&emulator, &label, &arn).await;
            assert_eq!(
                reported, held,
                "case {label}: a refused assignment must leave the stored tags as they were, \
                 got {reported:?}"
            );
        }
    }

    // The near misses the rule does NOT catch: the prefix in another case, inside the key, and
    // without the colon that ends it. Each is an ordinary caller tag and is stored.
    let mut expected = held.clone();
    for (label, key) in [
        ("the prefix in another case", "AWS:reserved-tag"),
        ("the prefix inside the key", "team:aws:owner"),
        ("the prefix without its colon", "awsome"),
    ] {
        emulator
            .ok(
                label,
                "TagResource",
                &json!({
                    "ResourceARN": arn,
                    "Tags": tag_map(&[(key.to_string(), "stored".to_string())]),
                }),
            )
            .await;
        expected.push((key.to_string(), "stored".to_string()));
        expected.sort();
        let reported = tags_of(&emulator, label, &arn).await;
        assert_eq!(
            reported, expected,
            "case {label}: a key that only resembles a reserved one must be stored, got \
             {reported:?}"
        );
    }
}

/// A create that carries a reserved tag key is refused with the same fixed sentence and
/// provisions nothing at all — on every create of this service that takes a `Tags` map —
/// while the same create carrying only ordinary keys is served and stores them.
#[tokio::test]
async fn a_create_carrying_a_reserved_tag_key_provisions_nothing() {
    let emulator = Emulator::new();
    let source = on_demand_stream(&emulator, "reserved-create-source").await;

    for (index, (label, key)) in reserved_tag_keys().into_iter().enumerate() {
        // The reserved key arrives beside an ordinary one, so what is refused is the map
        // rather than a request carrying nothing else. Every row names its own resources, so
        // no refusal can be read off a name an earlier row took; the names stay inside the
        // modelled `^[a-zA-Z0-9_.-]+$`.
        let tags = tag_map(&[
            ("Owner".to_string(), "platform".to_string()),
            (key.to_string(), "x".to_string()),
        ]);
        let stream = format!("reserved-create-stream-{index}");
        let channel = format!("reserved-create-channel-{index}");
        let consumer = format!("reserved-create-consumer-{index}");

        // (case, the operation, the create it refuses, the read that must find nothing).
        let rows: Vec<(String, &str, Value, &str, Value)> = vec![
            (
                format!("CreateStream/{label}"),
                "CreateStream",
                json!({ "StreamName": stream, "ShardCount": 1, "Tags": tags }),
                "DescribeStreamSummary",
                json!({ "StreamName": stream }),
            ),
            (
                format!("CreateChannel/{label}"),
                "CreateChannel",
                json!({
                    "ChannelName": channel,
                    "ServiceExecutionRoleARN": role_arn("channel-delivery"),
                    "StreamConfigurationList": [stream_configuration(&source, "JSON", None)],
                    "S3DestinationConfiguration": {
                        "StorageConfiguration": storage_configuration("reserved-tag-bucket"),
                    },
                    "Tags": tags,
                }),
                "DescribeChannel",
                json!({ "ChannelARN": channel_arn(REGION, &channel) }),
            ),
            (
                format!("RegisterStreamConsumer/{label}"),
                "RegisterStreamConsumer",
                json!({ "StreamARN": source, "ConsumerName": consumer, "Tags": tags }),
                "DescribeStreamConsumer",
                json!({ "StreamARN": source, "ConsumerName": consumer }),
            ),
        ];
        for (case, operation, create, reader, read) in &rows {
            let (code, message) = refusal(&emulator, case, operation, create).await;
            assert_eq!(
                code, "InvalidArgumentException",
                "case {case}: expected InvalidArgumentException, got {code} with {message:?}"
            );
            assert_eq!(
                message, RESERVED_TAG_KEYS_SAYS,
                "case {case}: expected AWS's reserved-namespace sentence, got {message:?}"
            );
            let (status, response) = emulator.call(reader, read).await;
            assert_eq!(
                status, 400,
                "case {case}: a refused create must provision nothing, got {status} from \
                 {reader} with body {response}"
            );
            assert_eq!(
                error_code(&response),
                "ResourceNotFoundException",
                "case {case}: expected {reader} to report the resource as absent, got {response}"
            );
        }
    }

    // The positive control: the same creates, carrying only an ordinary key that opens with
    // the prefix in another case, are served — and what they carried is what a read reports.
    let ordinary = vec![("AWS:reserved-tag".to_string(), "stored".to_string())];
    let stream = "ordinary-create-stream";
    emulator
        .ok(
            "CreateStream/ordinary",
            "CreateStream",
            &json!({ "StreamName": stream, "ShardCount": 1, "Tags": tag_map(&ordinary) }),
        )
        .await;
    let reported = tags_of(&emulator, "CreateStream/ordinary", &stream_arn(REGION, stream)).await;
    assert_eq!(
        reported, ordinary,
        "case CreateStream/ordinary: the create's own tags must be stored on the stream it \
         minted, got {reported:?}"
    );

    let channel = "ordinary-create-channel";
    emulator
        .ok(
            "CreateChannel/ordinary",
            "CreateChannel",
            &json!({
                "ChannelName": channel,
                "ServiceExecutionRoleARN": role_arn("channel-delivery"),
                "StreamConfigurationList": [stream_configuration(&source, "JSON", None)],
                "S3DestinationConfiguration": {
                    "StorageConfiguration": storage_configuration("ordinary-tag-bucket"),
                },
                "Tags": tag_map(&ordinary),
            }),
        )
        .await;
    let arn = channel_arn(REGION, channel);
    emulator
        .ok(
            "CreateChannel/ordinary",
            "DescribeChannel",
            &json!({ "ChannelARN": arn }),
        )
        .await;
    let reported = tags_of(&emulator, "CreateChannel/ordinary", &arn).await;
    assert_eq!(
        reported, ordinary,
        "case CreateChannel/ordinary: the create's own tags must be stored on the channel it \
         minted, got {reported:?}"
    );

    let consumer = "ordinary-create-consumer";
    emulator
        .ok(
            "RegisterStreamConsumer/ordinary",
            "RegisterStreamConsumer",
            &json!({
                "StreamARN": source,
                "ConsumerName": consumer,
                "Tags": tag_map(&ordinary),
            }),
        )
        .await;
    emulator
        .ok(
            "RegisterStreamConsumer/ordinary",
            "DescribeStreamConsumer",
            &json!({ "StreamARN": source, "ConsumerName": consumer }),
        )
        .await;
}

/// The three-letter weekday names an instant of AWS's two expiry sentences opens with, in the
/// order a week runs: the `EEE` field of the `EEE MMM dd HH:mm:ss UTC yyyy` layout those
/// sentences spell an instant in, which is the catalog this suite reads an instant's first
/// field against.
const WEEKDAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

/// The three-letter month names of that same layout's `MMM` field, in calendar order.
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// The delay an expired-iterator sentence states, in milliseconds, as AWS writes the figure:
/// "A shard iterator expires 5 minutes after it is returned to the requester"
/// (`GetShardIterator`).
const ITERATOR_TOLERATED_DELAY_MILLIS: u64 = 300_000;

/// Whether `text` is an instant spelled the way AWS's expiry sentences spell one: `EEE MMM dd
/// HH:mm:ss UTC yyyy`, each field read against the layout rather than against a sample — the
/// weekday and month from the catalogs above, a two-digit day, a two-digit 24-hour time, the
/// literal zone and a four-digit year.
fn reads_as_an_expiry_instant(text: &str) -> bool {
    let fields: Vec<&str> = text.split(' ').collect();
    let [weekday, month, day, time, zone, year] = fields.as_slice() else {
        return false;
    };
    let clock: Vec<&str> = time.split(':').collect();
    WEEKDAYS.contains(weekday)
        && MONTHS.contains(month)
        && day.len() == 2
        && day.chars().all(char::is_numeric)
        && clock.len() == 3
        && clock
            .iter()
            .all(|part| part.len() == 2 && part.chars().all(char::is_numeric))
        && *zone == "UTC"
        && year.len() == 4
        && year.chars().all(char::is_numeric)
}

/// One row of the instant catalog: an instant, how AWS's layout spells it, and how that layout
/// spells the instant a cursor minted at it lapses at — 300 seconds later, the lifetime
/// `ListStreamConsumersOutput$NextToken` documents ("Tokens expire after 300 seconds").
///
/// The rows differ along every field of the layout — the weekday, the month, a single- and a
/// two-digit day, the time of day, the year — and one of them crosses a month boundary (and a
/// leap day) once the cursor lifetime is added, so the rendering is the layout's and not one
/// instant's.
struct InstantRow {
    case: &'static str,
    /// The instant itself, in milliseconds since the epoch.
    millis: u64,
    /// That instant, as AWS's layout spells it.
    spelled: &'static str,
    /// The instant 300 seconds after it, as AWS's layout spells it.
    spelled_after_the_cursor_lifetime: &'static str,
}

/// The catalog both expiry sentences are read over.
const EXPIRY_INSTANTS: &[InstantRow] = &[
    InstantRow {
        case: "the epoch itself",
        millis: 0,
        spelled: "Thu Jan 01 00:00:00 UTC 1970",
        spelled_after_the_cursor_lifetime: "Thu Jan 01 00:05:00 UTC 1970",
    },
    InstantRow {
        case: "a single-digit day early in a year",
        millis: 1_767_503_167_000,
        spelled: "Sun Jan 04 05:06:07 UTC 2026",
        spelled_after_the_cursor_lifetime: "Sun Jan 04 05:11:07 UTC 2026",
    },
    InstantRow {
        case: "the last second of a leap day",
        millis: 1_709_251_199_000,
        spelled: "Thu Feb 29 23:59:59 UTC 2024",
        spelled_after_the_cursor_lifetime: "Fri Mar 01 00:04:59 UTC 2024",
    },
    InstantRow {
        case: "a two-digit day late in a year",
        millis: 1_794_257_380_000,
        spelled: "Mon Nov 09 20:49:40 UTC 2026",
        spelled_after_the_cursor_lifetime: "Mon Nov 09 20:54:40 UTC 2026",
    },
];

/// The instant `millis` milliseconds after the epoch names.
fn at_millis(millis: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_millis(millis)
}

/// A `GetRecords` whose iterator is past its age is refused with AWS's expired-iterator
/// sentence: its fixed words exactly, the iterator's own mint instant and the server's current
/// instant, both spelled in AWS's layout.
#[tokio::test]
async fn an_expired_iterator_states_both_instants_in_awss_layout() {
    let emulator = Emulator::new();
    let stream = "expired-iterator-wording";
    emulator
        .ok(
            "setup",
            "CreateStream",
            &json!({ "StreamName": stream, "ShardCount": 1 }),
        )
        .await;
    poll_until_active(&emulator, "setup", stream).await;
    let shard = shard_id(0);
    emulator
        .ok(
            "setup",
            "PutRecord",
            &json!({
                "StreamName": stream,
                "PartitionKey": "expiry",
                "Data": aws_smithy_types::base64::encode("expiry"),
            }),
        )
        .await;

    // Both positions a reader can ask for without naming a sequence number, so the sentence is
    // shown to be the expiry's and not one position's.
    let mut issued: Vec<(&str, String)> = Vec::new();
    for position in ["TRIM_HORIZON", "LATEST"] {
        let answer = emulator
            .ok(
                "setup",
                "GetShardIterator",
                &json!({
                    "StreamName": stream,
                    "ShardId": shard,
                    "ShardIteratorType": position,
                }),
            )
            .await;
        issued.push((
            position,
            answer["ShardIterator"]
                .as_str()
                .unwrap_or_else(|| panic!("case setup: GetShardIterator reported no iterator"))
                .to_string(),
        ));
    }

    let opening = "Iterator expired. The iterator was created at time ";
    let between = " while right now it is ";
    let closing = format!(
        " which is further in the future than the tolerated delay of \
         {ITERATOR_TOLERATED_DELAY_MILLIS} milliseconds."
    );

    for (position, iterator) in &issued {
        for row in EXPIRY_INSTANTS {
            let case = format!("{} read from a {position} iterator", row.case);
            // The iterator this service issued, carrying the mint instant of this row: every
            // other part of it is the one the service handed out.
            let mut aged = ShardIteratorToken::decode(iterator)
                .unwrap_or_else(|| panic!("case {case}: the issued iterator decodes"));
            aged.issued_at_millis = u128::from(row.millis);

            let (status, response) = emulator
                .call("GetRecords", &json!({ "ShardIterator": aged.encode() }))
                .await;
            assert_eq!(
                status, 400,
                "case {case}: expected the modelled client refusal, got {status} with body \
                 {response}"
            );
            assert_eq!(
                error_code(&response),
                "ExpiredIteratorException",
                "case {case}: expected the modelled expiry, got {response}"
            );

            let message = error_message(&response);
            let instants = message
                .strip_prefix(opening)
                .and_then(|rest| rest.strip_suffix(closing.as_str()))
                .unwrap_or_else(|| {
                    panic!(
                        "case {case}: expected AWS's expired-iterator sentence \
                         {opening:?}<created>{between:?}<now>{closing:?}, got {message:?}"
                    )
                });
            let (created, right_now) = instants.split_once(between).unwrap_or_else(|| {
                panic!(
                    "case {case}: expected the sentence to name both instants around \
                     {between:?}, got {message:?}"
                )
            });
            assert_eq!(
                created, row.spelled,
                "case {case}: expected the iterator's mint instant spelled in AWS's layout, \
                 got {created:?} in {message:?}"
            );
            assert!(
                reads_as_an_expiry_instant(right_now),
                "case {case}: expected the server's instant spelled in AWS's layout, got \
                 {right_now:?} in {message:?}"
            );
        }
    }
}

/// Every listing of this service refuses a `NextToken` past its lifetime with AWS's
/// expired-token sentence: the instant that token lapsed, spelled in AWS's layout, and the
/// listing to invoke again for a new one.
#[tokio::test]
async fn an_expired_pagination_token_states_the_lapse_instant_and_the_listing() {
    let emulator = Emulator::new();
    // The cursors below never reach a stream: a token past its lifetime names no page, which
    // is answered before anything is looked up. So this ARN addresses the stream the two
    // stream-scoped cursors are tagged for without one having to be created.
    let arn = stream_arn(REGION, "token-wording-stream");
    let created_at = SystemTime::now();

    for row in EXPIRY_INSTANTS {
        let minted_at = at_millis(row.millis);
        // (the listing, the request resuming it at a cursor minted at this row's instant)
        let listings: Vec<(&str, Value)> = vec![
            (
                "ListStreams",
                json!({
                    "NextToken": RegionalPageToken::new(
                        RegionalListing::Streams,
                        REGION,
                        "a-stream",
                        minted_at,
                    )
                    .encode(),
                }),
            ),
            (
                "ListChannels",
                json!({
                    "NextToken": RegionalPageToken::new(
                        RegionalListing::Channels,
                        REGION,
                        "a-channel",
                        minted_at,
                    )
                    .encode(),
                }),
            ),
            (
                "ListShards",
                json!({
                    "NextToken": ShardPageToken::new(&arn, created_at, &shard_id(0), minted_at)
                        .encode(),
                }),
            ),
            (
                "ListStreamConsumers",
                json!({
                    "StreamARN": arn,
                    "NextToken": ConsumerPageToken::new(
                        &arn,
                        created_at,
                        "a-consumer",
                        minted_at,
                    )
                    .encode(),
                }),
            ),
        ];

        for (operation, body) in listings {
            let case = format!("{operation} resumed at a cursor minted at {}", row.case);
            let (code, message) = refusal(&emulator, &case, operation, &body).await;
            assert_eq!(
                code, "ExpiredNextTokenException",
                "case {case}: expected the modelled expiry, got {code} with {message:?}"
            );
            assert_eq!(
                message,
                format!(
                    "NextToken expired at time {}. Invoke {operation} again to get a new \
                     pagination token.",
                    row.spelled_after_the_cursor_lifetime
                ),
                "case {case}: expected AWS's expired-token sentence naming the lapse instant \
                 {} and this listing, got {message:?}",
                row.spelled_after_the_cursor_lifetime
            );
        }
    }
}
