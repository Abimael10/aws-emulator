// SPDX-License-Identifier: CC0-1.0

//! The WORDS this service refuses a request with.
//!
//! Amazon Keyspaces states a refusal of its stream APIs in FIXED text: one sentence per
//! error class, naming no member, no value and no identifier of the request. Two callers
//! that send two different bad requests of the same class read the same sentence, which is
//! what lets a client branch on it at all — and what keeps the text free of the per-run
//! keyspace name, table name, shard id, sequence number or ARN a request happens to carry.
//! The class a refusal belongs to is carried by the error SHAPE and, for
//! `ValidationException`, by its `errorCode` member (`ValidationExceptionType`): that is
//! where the detail lives, not in the message.
//!
//! So the messages live here, once, rather than being formatted at each refusal site: every
//! operation of this service words one class identically, and a sentence stated in one
//! place cannot drift between two handlers.

/// `ResourceNotFoundException`: the resource the request named — a stream, a shard — is not
/// one this service holds. Interpolates nothing: the request already knows what it asked
/// for, and the sentence must not disclose anything about what the store does hold.
pub const RESOURCE_NOT_FOUND: &str = "Requested resource not found";

/// `ValidationException`: a parameter value the request carries, or a combination of them,
/// is not one the operation accepts — a missing `sequenceNumber` for a sequence-positioned
/// iterator, a filter without the member it needs, a sequence number outside the shard.
/// Which of them is wrong is not named: the sentence is the same for all of them.
pub const INVALID_PARAMETER_VALUES: &str = "One or more parameter values were invalid";

/// `ValidationException` with `errorCode` `InvalidFormat`: the `shardIterator` presented is
/// not one this service issued, so it names no position to read from. A permanent
/// client-side format failure rather than a missing resource — an iterator this service
/// could have issued and that has since lapsed is `ExpiredIterator` instead.
pub const INVALID_SHARD_ITERATOR: &str = "Invalid ShardIterator";

/// `ValidationException` with `errorCode` `InvalidFormat`: the `streamArn` presented is not
/// an ARN naming a stream of an Amazon Keyspaces table, so it names no stream this service
/// could ever hold. A permanent client-side format failure, which is a different answer
/// from [`RESOURCE_NOT_FOUND`]: that one is about a well-formed name nothing is stored
/// under, and only this one tells the caller to fix the value rather than create the stream.
pub const INVALID_STREAM_ARN: &str = "Invalid stream ARN.";

/// `ValidationException`: the `nextToken` presented is not one this service can resume the
/// query with — it issued no such token, or issued it for another query. Which it was is not
/// disclosed, and the token is not echoed: a pagination token is opaque, so the only thing a
/// caller can do with either answer is start the enumeration again. A token this service DID
/// issue and that has since lapsed is `ExpiredNextToken`, which is the signal to re-read
/// from the start rather than a permanent format failure.
pub const INVALID_NEXT_TOKEN: &str = "Invalid nextToken.";
