use std::collections::HashMap;

use bitcoin::{util::uint::Uint256, BlockHash, BlockHeader, Network};

use crate::{
    constants::{
        checkpoints, max_target, no_pow_retargeting, pow_limit_bits,
        DIFFICULTY_ADJUSTMENT_INTERVAL, TEN_MINUTES,
    },
    BlockHeight,
};

/// An error thrown when trying to validate a header.
#[derive(Debug)]
pub enum ValidateHeaderError {
    /// Used when the timestamp in the header is lower than
    /// the median of timestamps of past 11 headers.
    HeaderIsOld,
    /// Used when the header doesn't match with a checkpoint.
    DoesNotMatchCheckpoint,
    /// Used when the PoW in the header is invalid as per the target mentioned
    /// in the header.
    InvalidPoWForHeaderTarget,
    /// Used when the PoW in the header is invalid as per the target
    /// computed based on the previous headers.
    InvalidPoWForComputedTarget,
    /// Used when the target in the header is greater than the max possible
    /// value.
    TargetDifficultyAboveMax,
    /// Used when the predecessor of the input header is not found in the
    /// HeaderStore.
    PrevHeaderNotFound,
}

pub struct StoredHeader {
    pub header: BlockHeader,
    pub height: BlockHeight,
}

pub trait HeaderStore {
    /// Retrieves the header from the store.
    fn get_header(&self, hash: &BlockHash) -> Option<&StoredHeader>;
    /// Retrieves the initial hash the store starts from.
    fn get_initial_hash(&self) -> BlockHash;
}

pub trait ChainValidator {
    /// Validates a header and returns an error
    fn validate_header(
        &self,
        store: &impl HeaderStore,
        header: &BlockHeader,
    ) -> Result<(), ValidateHeaderError>;
}

pub struct ChainValidatorImpl {
    /// The network checkpoints used to validate headers at defined heights.
    checkpoints: HashMap<BlockHeight, BlockHash>,
    /// The network mode that determines how the validator operates.
    network: Network,
}

impl ChainValidatorImpl {
    /// Initializes the implementation with the current network and the header
    /// store.
    pub fn new(network: &Network) -> Self {
        let checkpoints = checkpoints(network);
        Self {
            checkpoints,
            network: *network,
        }
    }

    /// If the height of the header is the same as a checkpoint,
    /// then ensure that the header is matching.
    fn is_checkpoint_valid(&self, prev_header: &StoredHeader, header: &BlockHeader) -> bool {
        let next_height = prev_header.height.saturating_add(1);
        if let Some(next_hash) = self.checkpoints.get(&next_height) {
            return *next_hash == header.block_hash();
        }

        true
    }

    /// Validates if a header's timestamp is valid.
    /// Bitcoin Protocol Rules wiki https://en.bitcoin.it/wiki/Protocol_rules says,
    /// "Reject if timestamp is the median time of the last 11 blocks or before"
    fn is_timestamp_valid(&self, store: &impl HeaderStore, header: &BlockHeader) -> bool {
        let mut times = vec![];
        let mut current_header = header;
        for _ in 0..11 {
            if let Some(StoredHeader {
                header: prev_header,
                ..
            }) = store.get_header(&current_header.prev_blockhash)
            {
                times.push(prev_header.time);
                if prev_header.block_hash() == store.get_initial_hash() {
                    break;
                }
                current_header = prev_header;
            }
        }

        times.sort_unstable();
        let median = times[times.len() / 2];
        header.time > median
    }

    /// Gets the next target by doing the following:
    /// * If the network allows blocks to have the max target (testnet &
    ///   regtest), the next difficulty is searched for unless the header's
    ///   timestamp is greater than 20 minutes from the previous header's
    ///   timestamp.
    /// * If the network does not allow blocks with the max target, the next
    ///   difficulty is then computed and then cast into the next target.
    fn get_next_target(
        &self,
        store: &impl HeaderStore,
        prev_stored_header: &StoredHeader,
        header: &BlockHeader,
    ) -> Uint256 {
        match self.network {
            Network::Testnet | Network::Regtest => {
                if prev_stored_header.height + 1 % DIFFICULTY_ADJUSTMENT_INTERVAL != 0 {
                    // This if statements is reached only for Regtest and Testnet networks
                    // Here is the quote from "https://en.bitcoin.it/wiki/Testnet"
                    // "If no block has been found in 20 minutes, the difficulty automatically
                    // resets back to the minimum for a single block, after which it
                    // returns to its previous value."
                    if header.time > prev_stored_header.header.time + TEN_MINUTES * 2 {
                        //If no block has been found in 20 minutes, then use the maximum difficulty
                        // target
                        max_target(&self.network)
                    } else {
                        //If the block has been found within 20 minutes, then use the previous
                        // difficulty target that is not equal to the maximum difficulty target
                        BlockHeader::u256_from_compact_target(
                            self.find_next_difficulty_in_chain(store, prev_stored_header),
                        )
                    }
                } else {
                    BlockHeader::u256_from_compact_target(
                        self.compute_next_difficulty(store, prev_stored_header),
                    )
                }
            }
            Network::Bitcoin | Network::Signet => BlockHeader::u256_from_compact_target(
                self.compute_next_difficulty(store, prev_stored_header),
            ),
        }
    }

    /// This method is only valid when used for testnet and regtest networks.
    /// As per "https://en.bitcoin.it/wiki/Testnet",
    /// "If no block has been found in 20 minutes, the difficulty automatically
    /// resets back to the minimum for a single block, after which it
    /// returns to its previous value." This function is used to compute the
    /// difficulty target in case the block has been found within 20
    /// minutes.
    fn find_next_difficulty_in_chain(
        &self,
        store: &impl HeaderStore,
        prev_header: &StoredHeader,
    ) -> u32 {
        // This is the maximum difficulty target for the network
        let pow_limit_bits = pow_limit_bits(&self.network);
        match self.network {
            Network::Testnet | Network::Regtest => {
                let mut current_header = prev_header;
                let initial_header_hash = store.get_initial_hash();

                // Keep traversing the blockchain backwards from the recent block to initial
                // header hash.
                while current_header.header.block_hash() != initial_header_hash {
                    if current_header.header.bits != pow_limit_bits
                        || current_header.height % DIFFICULTY_ADJUSTMENT_INTERVAL == 0
                    {
                        return current_header.header.bits;
                    }

                    // Traverse to the previous header
                    current_header = store
                        .get_header(&current_header.header.prev_blockhash)
                        .unwrap();
                }
                pow_limit_bits
            }
            Network::Bitcoin | Network::Signet => pow_limit_bits,
        }
    }

    /// This function returns the difficult target to be used for the current
    /// header given the previous header
    fn compute_next_difficulty(&self, store: &impl HeaderStore, prev_header: &StoredHeader) -> u32 {
        // Difficulty is adjusted only once in every interval of 2 weeks (2016 blocks)
        // If an interval boundary is not reached, then previous difficulty target is
        // returned Regtest network doesn't adjust PoW difficult levels. For
        // regtest, simply return the previous difficulty target

        if (prev_header.height + 1) % DIFFICULTY_ADJUSTMENT_INTERVAL != 0
            || no_pow_retargeting(&self.network)
        {
            return prev_header.header.bits;
        }

        // Computing the last header with height multiple of 2016
        let mut current_header = prev_header;
        for _i in 0..(DIFFICULTY_ADJUSTMENT_INTERVAL - 1) {
            if let Some(h) = store.get_header(&current_header.header.prev_blockhash) {
                current_header = h;
            }
        }
        // last_adjustment_header is the last header with height multiple of 2016
        let last_adjustment_header = current_header;
        let last_adjustment_time = last_adjustment_header.header.time;

        // Computing the time interval between the last adjustment header time and
        // current time. The expected value actual_interval is 2 weeks assuming
        // the expected block time is 10 mins. But most of the time, the
        // actual_interval will deviate slightly from 2 weeks. Our goal is to
        // readjust the difficulty target so that the expected time taken for the next
        // 2016 blocks is again 2 weeks.
        let actual_interval = prev_header.header.time - last_adjustment_time;
        let mut adjusted_interval = actual_interval as u32;

        // The target_adjustment_interval_time is 2 weeks of time expressed in seconds
        let target_adjustment_interval_time: u32 = DIFFICULTY_ADJUSTMENT_INTERVAL * TEN_MINUTES; //Number of seconds in 2 weeks

        // Adjusting the actual_interval to [0.5 week, 8 week] range in case the
        // actual_interval deviates too much from the expected 2 weeks.
        adjusted_interval = u32::max(adjusted_interval, target_adjustment_interval_time / 4);
        adjusted_interval = u32::min(adjusted_interval, target_adjustment_interval_time * 4);

        // Computing new difficulty target.
        // new difficulty target = old difficult target * (adjusted_interval /
        // 2_weeks);
        let mut target = prev_header.header.target();
        target = target.mul_u32(adjusted_interval);
        target = target / Uint256::from_u64(target_adjustment_interval_time as u64).unwrap();

        // Adjusting the newly computed difficulty target so that it doesn't exceed the
        // max_difficulty_target limit
        target = Uint256::min(target, max_target(&self.network));

        // Converting the target (Uint256) into a 32 bit representation used by Bitcoin
        BlockHeader::compact_target_from_u256(&target)
    }
}

impl ChainValidator for ChainValidatorImpl {
    /// Validates a header. If a failure occurs, an
    /// [IsHeaderValidError](IsHeaderValidError) will be returned.
    fn validate_header(
        &self,
        store: &impl HeaderStore,
        header: &BlockHeader,
    ) -> Result<(), ValidateHeaderError> {
        let prev_stored_header = match store.get_header(&header.prev_blockhash) {
            Some(header) => header,
            None => {
                return Err(ValidateHeaderError::PrevHeaderNotFound);
            }
        };

        if !self.is_timestamp_valid(store, header) {
            return Err(ValidateHeaderError::HeaderIsOld);
        }

        if !self.is_checkpoint_valid(prev_stored_header, header) {
            return Err(ValidateHeaderError::DoesNotMatchCheckpoint);
        }

        let header_target = header.target();
        if header_target > max_target(&self.network) {
            return Err(ValidateHeaderError::TargetDifficultyAboveMax);
        }

        if header.validate_pow(&header_target).is_err() {
            return Err(ValidateHeaderError::InvalidPoWForHeaderTarget);
        }

        let target = self.get_next_target(store, prev_stored_header, header);
        if let Err(err) = header.validate_pow(&target) {
            match err {
                bitcoin::Error::BlockBadProofOfWork => println!("bad proof of work"),
                bitcoin::Error::BlockBadTarget => println!("bad target"),
                _ => {}
            };
            return Err(ValidateHeaderError::InvalidPoWForComputedTarget);
        }

        Ok(())
    }
}

#[cfg(test)]
mod test {

    use std::str::FromStr;

    use bitcoin::{consensus::deserialize, hashes::hex::FromHex, TxMerkleNode};
    use csv::Reader;

    use super::*;
    use crate::constants::test::{
        MAINNET_HEADER_586656, MAINNET_HEADER_705600, MAINNET_HEADER_705601, MAINNET_HEADER_705602,
        TESTNET_HEADER_2132555, TESTNET_HEADER_2132556,
    };

    struct SimpleHeaderStore {
        headers: HashMap<BlockHash, StoredHeader>,
        initial_hash: BlockHash,
    }

    impl SimpleHeaderStore {
        fn new(initial_header: BlockHeader, height: BlockHeight) -> Self {
            let initial_hash = initial_header.block_hash();
            let mut headers = HashMap::new();
            headers.insert(
                initial_hash,
                StoredHeader {
                    header: initial_header,
                    height,
                },
            );

            Self {
                headers,
                initial_hash,
            }
        }

        fn add(&mut self, header: BlockHeader) {
            let prev = self
                .headers
                .get(&header.prev_blockhash)
                .expect("prev hash missing");
            let stored_header = StoredHeader {
                header,
                height: prev.height + 1,
            };

            self.headers.insert(header.block_hash(), stored_header);
        }
    }

    impl HeaderStore for SimpleHeaderStore {
        fn get_header(&self, hash: &BlockHash) -> Option<&StoredHeader> {
            self.headers.get(hash)
        }

        fn get_initial_hash(&self) -> BlockHash {
            self.initial_hash
        }
    }

    fn deserialize_header(encoded_bytes: &str) -> BlockHeader {
        let bytes = Vec::from_hex(encoded_bytes).expect("failed to decoded bytes");
        deserialize(bytes.as_slice()).expect("failed to deserialize")
    }

    /// This function reads `num_headers` headers from `tests/data/headers.csv`
    /// and returns them.
    /// This function reads `num_headers` headers from `blockchain_headers.csv`
    /// and returns them.
    fn get_bitcoin_headers() -> Vec<BlockHeader> {
        let rdr = Reader::from_path("tests/data/headers.csv");
        assert!(rdr.is_ok(), "Unable to find blockchain_headers.csv file");
        let mut rdr = rdr.unwrap();
        let mut headers = vec![];
        for result in rdr.records() {
            let record = result.unwrap();
            let header = BlockHeader {
                version: i32::from_str_radix(record.get(0).unwrap(), 16).unwrap(),
                prev_blockhash: BlockHash::from_str(record.get(1).unwrap()).unwrap(),
                merkle_root: TxMerkleNode::from_str(record.get(2).unwrap()).unwrap(),
                time: u32::from_str_radix(record.get(3).unwrap(), 16).unwrap(),
                bits: u32::from_str_radix(record.get(4).unwrap(), 16).unwrap(),
                nonce: u32::from_str_radix(record.get(5).unwrap(), 16).unwrap(),
            };
            headers.push(header);
        }
        headers
    }

    #[test]
    fn test_simple_mainnet() {
        let header_705600 = deserialize_header(MAINNET_HEADER_705600);
        let header_705601 = deserialize_header(MAINNET_HEADER_705601);
        let store = SimpleHeaderStore::new(header_705600, 705_600);
        let validator = ChainValidatorImpl::new(&Network::Bitcoin);
        let result = validator.validate_header(&store, &header_705601);
        assert!(result.is_ok());
    }

    #[test]
    fn test_simple_testnet() {
        let header_2132555 = deserialize_header(TESTNET_HEADER_2132555);
        let header_2132556 = deserialize_header(TESTNET_HEADER_2132556);
        let store = SimpleHeaderStore::new(header_2132555, 2_132_555);
        let validator = ChainValidatorImpl::new(&Network::Testnet);
        let result = validator.validate_header(&store, &header_2132556);
        assert!(result.is_ok());
    }

    #[test]
    fn test_is_header_valid() {
        let header_586656 = deserialize_header(MAINNET_HEADER_586656);
        let mut store = SimpleHeaderStore::new(header_586656, 586_656);
        let validator = ChainValidatorImpl::new(&Network::Bitcoin);
        let headers = get_bitcoin_headers();
        for (i, header) in headers.iter().enumerate() {
            let result = validator.validate_header(&store, header);
            assert!(
                result.is_ok(),
                "Failed to validate header on line {}: {:?}",
                i,
                result
            );
            store.add(*header);
        }
    }

    #[test]
    fn test_is_timestamp_valid() {
        let header_705600 = deserialize_header(MAINNET_HEADER_705600);
        let header_705601 = deserialize_header(MAINNET_HEADER_705601);
        let header_705602 = deserialize_header(MAINNET_HEADER_705602);
        let mut store = SimpleHeaderStore::new(header_705600, 705_600);
        store.add(header_705601);
        store.add(header_705602);
        let validator = ChainValidatorImpl::new(&Network::Bitcoin);

        let mut header = BlockHeader {
            version: 0x20800004,
            prev_blockhash: BlockHash::from_hex(
                "00000000000000000001eea12c0de75000c2546da22f7bf42d805c1d2769b6ef",
            )
            .unwrap(),
            merkle_root: TxMerkleNode::from_hex(
                "c120ff2ae1363593a0b92e0d281ec341a0cc989b4ee836dc3405c9f4215242a6",
            )
            .unwrap(),
            time: 1634590600,
            bits: 0x170e0408,
            nonce: 0xb48e8b0a,
        };
        assert!(validator.is_timestamp_valid(&store, &header));

        // Monday, October 18, 2021 20:26:40
        header.time = 1634588800;
        assert!(!validator.is_timestamp_valid(&store, &header));

        let result = validator.validate_header(&store, &header);
        assert!(matches!(result, Err(ValidateHeaderError::HeaderIsOld)));
    }

    #[test]
    fn test_is_header_valid_missing_prev_header() {
        let header_705600 = deserialize_header(MAINNET_HEADER_705600);
        let header_705602 = deserialize_header(MAINNET_HEADER_705602);
        let store = SimpleHeaderStore::new(header_705600, 705_600);
        let validator = ChainValidatorImpl::new(&Network::Bitcoin);
        let result = validator.validate_header(&store, &header_705602);
        assert!(matches!(
            result,
            Err(ValidateHeaderError::PrevHeaderNotFound)
        ));
    }

    #[test]
    fn test_is_header_valid_checkpoint_valid() {
        let header_705600 = deserialize_header(MAINNET_HEADER_705600);
        let header = deserialize_header(MAINNET_HEADER_705601);
        let store = SimpleHeaderStore::new(header_705600, 705_600);
        let mut validator = ChainValidatorImpl::new(&Network::Bitcoin);

        validator.checkpoints = [(705_601, BlockHash::default())].iter().cloned().collect();

        let result = validator.validate_header(&store, &header);
        assert!(matches!(
            result,
            Err(ValidateHeaderError::DoesNotMatchCheckpoint)
        ));
    }

    #[test]
    fn test_is_header_valid_invalid_header_target() {
        let header_705600 = deserialize_header(MAINNET_HEADER_705600);
        let mut header = deserialize_header(MAINNET_HEADER_705601);
        header.bits = pow_limit_bits(&Network::Bitcoin);
        let store = SimpleHeaderStore::new(header_705600, 705_600);
        let validator = ChainValidatorImpl::new(&Network::Bitcoin);
        let result = validator.validate_header(&store, &header);
        assert!(matches!(
            result,
            Err(ValidateHeaderError::InvalidPoWForHeaderTarget)
        ));
    }

    #[test]
    fn test_is_header_valid_invalid_computed_target() {
        let header_705600 = deserialize_header(MAINNET_HEADER_705600);
        let header = deserialize_header(MAINNET_HEADER_705601);
        let store = SimpleHeaderStore::new(header_705600, 705_600);
        let validator = ChainValidatorImpl::new(&Network::Regtest);
        let result = validator.validate_header(&store, &header);
        assert!(matches!(
            result,
            Err(ValidateHeaderError::InvalidPoWForComputedTarget)
        ));
    }

    #[test]
    fn test_is_header_valid_target_difficulty_above_max() {
        let header_705600 = deserialize_header(MAINNET_HEADER_705600);
        let mut header = deserialize_header(MAINNET_HEADER_705601);
        header.bits = pow_limit_bits(&Network::Regtest);
        let store = SimpleHeaderStore::new(header_705600, 705_600);
        let validator = ChainValidatorImpl::new(&Network::Bitcoin);
        let result = validator.validate_header(&store, &header);
        assert!(matches!(
            result,
            Err(ValidateHeaderError::TargetDifficultyAboveMax)
        ));
    }
}
