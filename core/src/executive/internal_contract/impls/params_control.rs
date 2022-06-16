// Copyright 2019 Conflux Foundation. All rights reserved.
// Conflux is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

use std::convert::TryInto;

use crate::internal_bail;
use cfx_state::state_trait::StateOpsTrait;
use cfx_statedb::Result as DbResult;
use cfx_types::{Address, U256, U512};
use lazy_static::lazy_static;

use crate::{
    state::power_two_fractional,
    vm::{self, ActionParams},
};

use super::super::{
    components::InternalRefContext, contracts::params_control::*,
    impls::staking::get_vote_power,
};

pub fn cast_vote(
    address: Address, version: u64, votes: Vec<Vote>, params: &ActionParams,
    context: &mut InternalRefContext,
) -> vm::Result<()>
{
    // If this is called, `env.number` must be larger than the activation
    // number. And version starts from 1 to tell if an account has ever voted in
    // the first version.
    let current_voting_version = (context.env.number
        - context.spec.cip94_activation_block_number)
        / context.spec.params_dao_vote_period
        + 1;
    if version != current_voting_version {
        internal_bail!(
            "vote version unmatch: current={} voted={}",
            current_voting_version,
            version
        );
    }
    let old_version =
        context.storage_at(params, &storage_key::versions(&address))?;
    let is_new_vote = old_version.as_u64() != version;

    let mut vote_counts = [None; PARAMETER_INDEX_MAX];
    for vote in votes {
        if vote.index >= PARAMETER_INDEX_MAX as u16 {
            internal_bail!("invalid vote index or opt_index");
        }
        let entry = &mut vote_counts[vote.index as usize];
        match entry {
            None => {
                *entry = Some(vote.votes);
            }
            Some(_) => {
                internal_bail!(
                    "Parameter voted twice: vote.index={}",
                    vote.index
                );
            }
        }
    }
    if is_new_vote {
        // If this is the first vote in the version, even for the not-voted
        // parameters, we still reset the account's votes from previous
        // versions.
        for index in 0..PARAMETER_INDEX_MAX {
            if vote_counts[index].is_none() {
                vote_counts[index] = Some([U256::zero(); OPTION_INDEX_MAX]);
            }
        }
    }

    let vote_power = get_vote_power(
        address,
        U256::from(context.env.number),
        context.env.number,
        context.state,
    )?;
    debug!("vote_power:{}", vote_power);
    for index in 0..PARAMETER_INDEX_MAX {
        if vote_counts[index].is_none() {
            continue;
        }
        let param_vote = vote_counts[index].unwrap();
        let total_counts = param_vote[0]
            .saturating_add(param_vote[1])
            .saturating_add(param_vote[2]);
        if total_counts > vote_power {
            internal_bail!(
                "not enough vote power: power={} votes={}",
                vote_power,
                total_counts
            );
        }
        for opt_index in 0..OPTION_INDEX_MAX {
            let vote_in_storage = context.storage_at(
                params,
                &storage_key::votes(&address, index, opt_index),
            )?;
            let old_vote = if is_new_vote {
                U256::zero()
            } else {
                vote_in_storage
            };
            debug!(
                "index:{}, opt_index{}, old_vote: {}, new_vote: {}",
                index, opt_index, old_vote, param_vote[opt_index]
            );

            // Update the global votes if the account vote changes.
            if param_vote[opt_index] != old_vote {
                let old_total_votes = context.state.get_system_storage(
                    &CURRENT_VOTES_ENTRIES[index][opt_index],
                )?;
                debug!("old_total_vote: {}", old_total_votes,);
                let new_total_votes = if old_vote > param_vote[opt_index] {
                    let dec = old_vote - param_vote[opt_index];
                    // If total votes are accurate, `old_total_votes` is
                    // larger than `old_vote`.
                    old_total_votes - dec
                } else if old_vote < param_vote[opt_index] {
                    let inc = param_vote[opt_index] - old_vote;
                    old_total_votes + inc
                } else {
                    assert!(is_new_vote);
                    old_total_votes
                };
                debug!("new_total_vote:{}", new_total_votes);
                context.state.set_system_storage(
                    CURRENT_VOTES_ENTRIES[index][opt_index].to_vec(),
                    new_total_votes,
                )?;
            }

            // Overwrite the account vote entry if needed.
            if param_vote[opt_index] != vote_in_storage {
                context.set_storage(
                    params,
                    storage_key::votes(&address, index, opt_index).to_vec(),
                    param_vote[opt_index],
                )?;
            }
        }
    }
    if is_new_vote {
        context.set_storage(
            params,
            storage_key::versions(&address).to_vec(),
            U256::from(version),
        )?;
    }
    Ok(())
}

pub fn read_vote(
    address: Address, params: &ActionParams, context: &mut InternalRefContext,
) -> vm::Result<Vec<Vote>> {
    let mut votes_list = Vec::new();
    for index in 0..PARAMETER_INDEX_MAX {
        let mut param_vote = [U256::zero(); OPTION_INDEX_MAX];
        for opt_index in 0..OPTION_INDEX_MAX {
            let votes = context.storage_at(
                params,
                &storage_key::votes(&address, index, opt_index),
            )?;
            param_vote[opt_index] = votes;
        }
        votes_list.push(Vote {
            index: index as u16,
            votes: param_vote,
        })
    }
    Ok(votes_list)
}

lazy_static! {
    static ref CURRENT_VOTES_ENTRIES: [[[u8; 32]; OPTION_INDEX_MAX]; PARAMETER_INDEX_MAX] = {
        let mut answer: [[[u8; 32]; OPTION_INDEX_MAX]; PARAMETER_INDEX_MAX] =
            Default::default();
        for index in 0..PARAMETER_INDEX_MAX {
            for opt_index in 0..OPTION_INDEX_MAX {
                answer[index][opt_index] =
                    system_storage_key::current_votes(index, opt_index);
            }
        }
        answer
    };
    static ref SETTLED_VOTES_ENTRIES: [[[u8; 32]; OPTION_INDEX_MAX]; PARAMETER_INDEX_MAX] = {
        let mut answer: [[[u8; 32]; OPTION_INDEX_MAX]; PARAMETER_INDEX_MAX] =
            Default::default();
        for index in 0..PARAMETER_INDEX_MAX {
            for opt_index in 0..OPTION_INDEX_MAX {
                answer[index][opt_index] =
                    system_storage_key::settled_votes(index, opt_index);
            }
        }
        answer
    };
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ParamVoteCount {
    unchange: U256,
    increase: U256,
    decrease: U256,
}

impl ParamVoteCount {
    pub fn new(unchange: U256, increase: U256, decrease: U256) -> Self {
        Self {
            unchange,
            increase,
            decrease,
        }
    }

    pub fn from_state<T: StateOpsTrait, U: AsRef<[u8]>>(
        state: &T, slot_entry: &[U; 3],
    ) -> DbResult<Self> {
        Ok(ParamVoteCount {
            unchange: state.get_system_storage(
                slot_entry[OPTION_UNCHANGE_INDEX as usize].as_ref(),
            )?,
            increase: state.get_system_storage(
                &slot_entry[OPTION_INCREASE_INDEX as usize].as_ref(),
            )?,
            decrease: state.get_system_storage(
                &slot_entry[OPTION_DECREASE_INDEX as usize].as_ref(),
            )?,
        })
    }

    pub fn compute_next_params(&self, old_value: U256) -> U256 {
        let answer = self.compute_next_params_inner(old_value);
        // The return value should be in `[2^8, 2^192]`
        let min_value = U256::from(256u64);
        let max_value = U256::one() << 192usize;
        if answer < min_value {
            return min_value;
        }
        if answer > max_value {
            return max_value;
        }
        return answer;
    }

    fn compute_next_params_inner(&self, old_value: U256) -> U256 {
        // `VoteCount` only counts valid votes, so this will not overflow.
        let total = self.unchange + self.increase + self.decrease;

        if total == U256::zero() || self.increase == self.decrease {
            // If no one votes, we just keep the value unchanged.
            return old_value;
        } else if self.increase == total {
            return old_value * 2u64;
        } else if self.decrease == total {
            return old_value / 2u64;
        };

        let weight = if self.increase > self.decrease {
            self.increase - self.decrease
        } else {
            self.decrease - self.increase
        };
        let increase = self.increase > self.decrease;

        let frac_power = (U512::from(weight) << 64u64) / U512::from(total);
        assert!(frac_power < (U512::one() << 64u64));
        let frac_power = frac_power.as_u64();

        let ratio = power_two_fractional(frac_power, increase, 96);
        let new_value = (U512::from(old_value) * U512::from(ratio)) >> 96u64;

        if new_value > (U512::one() << 192u64) {
            return U256::one() << 192u64;
        } else {
            return new_value.try_into().unwrap();
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct AllParamsVoteCount {
    pub pow_base_reward: ParamVoteCount,
    pub pos_reward_interest: ParamVoteCount,
}

/// If the vote counts are not initialized, all counts will be zero, and the
/// parameters will be unchanged.
pub fn get_settled_param_vote_count<T: StateOpsTrait>(
    state: &T,
) -> DbResult<AllParamsVoteCount> {
    let pow_base_reward = ParamVoteCount::from_state(
        state,
        &SETTLED_VOTES_ENTRIES[POW_BASE_REWARD_INDEX as usize],
    )?;
    let pos_reward_interest = ParamVoteCount::from_state(
        state,
        &SETTLED_VOTES_ENTRIES[POS_REWARD_INTEREST_RATE_INDEX as usize],
    )?;
    Ok(AllParamsVoteCount {
        pow_base_reward,
        pos_reward_interest,
    })
}

/// Move the next vote counts into settled and reset the counts.
pub fn settle_current_votes<T: StateOpsTrait>(state: &mut T) -> DbResult<()> {
    for index in 0..PARAMETER_INDEX_MAX {
        for opt_index in 0..OPTION_INDEX_MAX {
            let vote_count = state
                .get_system_storage(&CURRENT_VOTES_ENTRIES[index][opt_index])?;
            state.set_system_storage(
                SETTLED_VOTES_ENTRIES[index][opt_index].to_vec(),
                vote_count,
            )?;
            state.set_system_storage(
                CURRENT_VOTES_ENTRIES[index][opt_index].to_vec(),
                U256::zero(),
            )?;
        }
    }
    Ok(())
}

/// Solidity variable sequences.
/// ```solidity
/// struct VoteInfo {
///     uint version,
///     uint[3] pow_base_reward dynamic,
///     uint[3] pos_interest_rate dynamic,
/// }
/// mapping(address => VoteInfo) votes;
/// ```
mod storage_key {
    use cfx_types::{Address, BigEndianHash, H256, U256};

    use super::super::super::components::storage_layout::*;

    const VOTES_SLOT: usize = 0;

    // TODO: add cache to avoid duplicated hash computing
    pub fn versions(address: &Address) -> [u8; 32] {
        // Position of `votes`
        let base = U256::from(VOTES_SLOT);

        // Position of `votes[address]`
        let address_slot = mapping_slot(base, H256::from(*address).into_uint());

        // Position of `votes[address].version`
        let version_slot = address_slot;

        return u256_to_array(version_slot);
    }

    pub fn votes(
        address: &Address, index: usize, opt_index: usize,
    ) -> [u8; 32] {
        const TOPIC_OFFSET: [usize; 2] = [1, 2];

        // Position of `votes`
        let base = U256::from(VOTES_SLOT);

        // Position of `votes[address]`
        let address_slot = mapping_slot(base, H256::from(*address).into_uint());

        // Position of `votes[address].<topic>` (static slot)
        let topic_slot = address_slot + TOPIC_OFFSET[index];

        // Position of `votes[address].<topic>` (dynamic slot)
        let topic_slot = dynamic_slot(topic_slot);

        // Position of `votes[address].<topic>[opt_index]`
        let opt_slot = array_slot(topic_slot, opt_index, 1);

        return u256_to_array(opt_slot);
    }
}

/// Solidity variable sequences.
/// ```solidity
/// struct VoteStats {
///     uint[3] pow_base_reward dynamic,
///     uint[3] pos_interest_rate dynamic,
/// }
/// VoteStats current_votes dynamic;
/// VoteStats settled_votes dynamic;
/// ```
mod system_storage_key {
    use cfx_parameters::internal_contract_addresses::PARAMS_CONTROL_CONTRACT_ADDRESS;
    use cfx_types::U256;

    use super::super::super::{
        components::storage_layout::*, contracts::system_storage::base_slot,
    };

    const CURRENT_VOTES_SLOT: usize = 0;
    const SETTLED_VOTES_SLOT: usize = 1;

    fn vote_stats(base: U256, index: usize, opt_index: usize) -> U256 {
        // Position of `.<topic>` (static slot)
        let topic_slot = base + index;

        // Position of `.<topic>` (dynamic slot)
        let topic_slot = dynamic_slot(topic_slot);

        // Position of `.<topic>[opt_index]`
        return array_slot(topic_slot, opt_index, 1);
    }

    pub(super) fn current_votes(index: usize, opt_index: usize) -> [u8; 32] {
        // Position of `current_votes` (static slot)
        let base = base_slot(*PARAMS_CONTROL_CONTRACT_ADDRESS)
            + U256::from(CURRENT_VOTES_SLOT);

        // Position of `current_votes` (dynamic slot)
        let base = dynamic_slot(base);

        u256_to_array(vote_stats(base, index, opt_index))
    }

    pub(super) fn settled_votes(index: usize, opt_index: usize) -> [u8; 32] {
        // Position of `settled_votes` (static slot)
        let base = base_slot(*PARAMS_CONTROL_CONTRACT_ADDRESS)
            + U256::from(SETTLED_VOTES_SLOT);

        // Position of `settled_votes` (dynamic slot)
        let base = dynamic_slot(base);

        u256_to_array(vote_stats(base, index, opt_index))
    }
}