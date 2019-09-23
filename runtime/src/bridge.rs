/// runtime module implementing Substrate side of AkropolisOS token exchange bridge
/// You can use mint to create tokens backed by locked funds on Ethereum side
/// and transfer tokens on substrate side freely
///
use crate::token;
use crate::types::{MemberId, ProposalId, TokenBalance, TokenId};
use parity_codec::{Decode, Encode};
use rstd::prelude::Vec;
use runtime_primitives::traits::{As, Hash};
use support::{
    decl_event, decl_module, decl_storage, dispatch::Result, ensure, StorageMap, StorageValue,
};
use system::{self, ensure_signed};

#[derive(Encode, Decode, Default, Clone, PartialEq)]
#[cfg_attr(feature = "std", derive(Debug))]
pub struct Validator<AccountId> {
    validator_id: MemberId,
    account: AccountId,
}

#[derive(Encode, Decode, Clone, PartialEq)]
#[cfg_attr(feature = "std", derive(Debug))]
pub struct BridgeProposal<AccountId, VotingDeadline> {
    proposal_id: ProposalId,
    action: Action<AccountId>,
    open: bool,
    voting_deadline: VotingDeadline,
    votes_count: MemberId,
}

impl<A, V> Default for BridgeProposal<A, V>
where
    A: Default,
    V: Default,
{
    fn default() -> Self {
        BridgeProposal {
            proposal_id: ProposalId::default(),
            action: Action::EmptyAction,
            open: true,
            voting_deadline: V::default(),
            votes_count: MemberId::default(),
        }
    }
}

#[derive(Encode, Decode, Clone, PartialEq)]
#[cfg_attr(feature = "std", derive(Debug))]
pub enum Action<AccountId> {
    EmptyAction,
    Ethereum2Substrate(TokenId, AccountId, TokenBalance),
    Substrate2Ethereum(TokenId, AccountId, TokenBalance),
}

decl_event!(
    pub enum Event<T>
    where
        AccountId = <T as system::Trait>::AccountId,
    {
        NewVote(ProposalId, AccountId, bool),
        ProposeToMint(TokenId, AccountId, TokenBalance),
        ProposeToBurn(TokenId, AccountId, TokenBalance),
        ProposalIsAccepted(ProposalId),
        ProposalIsExpired(ProposalId),
        ProposalIsRejected(ProposalId),
    }
);

pub trait Trait: token::Trait + system::Trait {
    type Event: From<Event<Self>> + Into<<Self as system::Trait>::Event>;
}

decl_storage! {
    trait Store for Module<T: Trait> as TokenStorage {
        BridgeProposals get(proposals): map ProposalId => BridgeProposal<T::AccountId, T::BlockNumber>;
        BridgeProposalsVotes get(proposal_votes): map ProposalId => MemberId;
        BridgeProposalsPeriodLimit get(proposals_period_limit) config(): T::BlockNumber = T::BlockNumber::sa(30);
        BridgeProposalsCount get(bridge_proposals_count): ProposalId;

        OpenBridgeProposalsLimit get(open_proposals_per_block) config(): usize = 2;
        OpenBridgeProposals get(open_bridge_proposals): map(T::BlockNumber) => Vec<ProposalId>;
        OpenBridgeProposalsIndex get(open_proposal_deadline_by_index): map(ProposalId) => T::BlockNumber;
        OpenBridgeProposalsHashes get(open_proposal_index_by_hash): map(T::Hash) => ProposalId;
        OpenBridgeProposalsHashesIndex get(open_proposal_hash_by_index): map(ProposalId) => T::Hash;

        EthereumAdressHashes get(ethereum_address): map(ProposalId) => Vec<u8>;
        ValidatorsCount get(validators_count) config(): usize = 3;
        Validators get(validators): map MemberId => Validator<T::AccountId>;
        ValidatorsAccounts get(validators_accounts): map MemberId => T::AccountId;
    }
}

decl_module! {
    pub struct Module<T: Trait> for enum Call where origin: T::Origin {
        fn deposit_event<T>() = default;


        //bridge specific Extrinsics
        fn substrate2eth(origin,
            message_id: Vec<u8>,
            to: Vec<u8>, //Ethereum address
            from: T::AccountId,
            #[compact] amount: TokenBalance
        )-> Result{
            let validator =  ensure_signed(origin)?;

            let proposal_hash = message_id.using_encoded(<T as system::Trait>::Hashing::hash);
            let token_id = <token::Module<T>>::token_default().id;
            let action = Action::Substrate2Ethereum(token_id, from.clone(), amount);

            let proposal_id = match <OpenBridgeProposalsHashes<T>>::exists(proposal_hash) {
                true => <OpenBridgeProposalsHashes<T>>::get(proposal_hash),
                false => {
                    Self::create_proposal(proposal_hash, action)?;
                    Self::deposit_event(RawEvent::ProposeToBurn(token_id, from, amount));
                    <OpenBridgeProposalsHashes<T>>::get(proposal_hash)
                }
            };

            Self::_vote(validator, proposal_id, true)?;
            <EthereumAdressHashes<T>>::insert(proposal_id, to);
            Ok(())
        }

        fn eth2substrate(origin,
            message_id: Vec<u8>,
            from: Vec<u8>, //Ethereum address
            to: T::AccountId,
            #[compact] amount: TokenBalance
        )-> Result {
            let validator = ensure_signed(origin)?;

            let proposal_hash = message_id.using_encoded(<T as system::Trait>::Hashing::hash);
            let default_token = <token::Module<T>>::token_default().clone();
            <token::Module<T>>::check_token_exist(validator.clone(), &default_token.symbol)?;
            let token_id = <token::Module<T>>::token_id_by_symbol(default_token.symbol);
            let action = Action::Ethereum2Substrate(token_id, to.clone(), amount);
            let proposal_id = match <OpenBridgeProposalsHashes<T>>::exists(proposal_hash) {
                true => <OpenBridgeProposalsHashes<T>>::get(proposal_hash),
                false => {
                    Self::create_proposal(proposal_hash, action)?;
                    Self::deposit_event(RawEvent::ProposeToMint(token_id, to, amount));
                    <OpenBridgeProposalsHashes<T>>::get(proposal_hash)
                }
            };

            Self::_vote(validator, proposal_id, true)?;
            <EthereumAdressHashes<T>>::insert(proposal_id, from);
            Ok(())
        }

        fn on_finalize() {
            let block_number = <system::Module<T>>::block_number();
            Self::open_bridge_proposals(block_number)
                .iter()
                .for_each(|&proposal_id| {
                    let proposal = <BridgeProposals<T>>::get(proposal_id);

                    if proposal.open {
                        Self::close_proposal(proposal);

                        Self::deposit_event(RawEvent::ProposalIsExpired(proposal_id));
                    }
                });

            <OpenBridgeProposals<T>>::remove(block_number);
        }
    }
}

impl<T: Trait> Module<T> {
    fn _vote(voter: T::AccountId, proposal_id: ProposalId, vote: bool) -> Result {
        ensure!(
            <BridgeProposals<T>>::exists(proposal_id),
            "This proposal not exists"
        );

        let mut proposal = <BridgeProposals<T>>::get(proposal_id);
        ensure!(proposal.open, "This proposal is not open");

        if vote {
            proposal.votes_count += 1;
        }

        let proposal_is_accepted = Self::votes_are_enough(proposal.votes_count);
        let all_validators_voted = proposal.votes_count == 3;

        if proposal_is_accepted {
            Self::execute_proposal(proposal.clone())?;
        }

        if proposal_is_accepted || all_validators_voted {
            Self::close_proposal(proposal.clone());
        } else {
            <BridgeProposals<T>>::insert(proposal_id, proposal);
        }

        Self::deposit_event(RawEvent::NewVote(proposal_id, voter, vote));

        match (proposal_is_accepted, all_validators_voted) {
            (true, _) => Self::deposit_event(RawEvent::ProposalIsAccepted(proposal_id)),
            (_, true) => Self::deposit_event(RawEvent::ProposalIsRejected(proposal_id)),
            (_, _) => (),
        }

        Ok(())
    }
    fn close_proposal(mut proposal: BridgeProposal<T::AccountId, T::BlockNumber>) {
        let proposal_id = proposal.proposal_id.clone();
        proposal.open = false;
        let proposal_hash = <OpenBridgeProposalsHashesIndex<T>>::get(proposal_id);

        <BridgeProposals<T>>::insert(proposal_id, proposal);
        <OpenBridgeProposalsHashes<T>>::remove(proposal_hash);
        <OpenBridgeProposalsHashesIndex<T>>::remove(proposal_id);
    }

    fn votes_are_enough(votes: MemberId) -> bool {
        votes as f64 / Self::validators_count() as f64 >= 0.51
    }

    fn execute_proposal(proposal: BridgeProposal<T::AccountId, T::BlockNumber>) -> Result {
        match proposal.action {
            Action::Substrate2Ethereum(token_id, from, amount) => {
                <token::Module<T>>::_burn(token_id, from, amount)
            }
            Action::Ethereum2Substrate(token_id, to, amount) => {
                <token::Module<T>>::_mint(token_id, to, amount)
            }
            Action::EmptyAction => Ok(()),
        }
    }
    fn create_proposal(proposal_hash: T::Hash, action: Action<T::AccountId>) -> Result {
        let voting_deadline = <system::Module<T>>::block_number() + Self::proposals_period_limit();
        let mut open_proposals = Self::open_bridge_proposals(voting_deadline);

        ensure!(
            open_proposals.len() < Self::open_proposals_per_block(),
            "Maximum number of open proposals is reached for the target block, try later"
        );
        ensure!(
            !<OpenBridgeProposalsHashes<T>>::exists(proposal_hash),
            "This proposal already open"
        );
        let proposal_id = <BridgeProposalsCount<T>>::get();
        let bridge_proposals_count = <BridgeProposalsCount<T>>::get();
        let new_bridge_proposals_count = bridge_proposals_count
            .checked_add(1)
            .ok_or("Overflow adding a new bridge proposal")?;

        let proposal = BridgeProposal {
            proposal_id,
            action,
            open: true,
            voting_deadline,
            votes_count: MemberId::default(),
        };

        open_proposals.push(proposal_id);
        <BridgeProposals<T>>::insert(proposal_id, proposal);
        <BridgeProposalsCount<T>>::mutate(|count| *count += new_bridge_proposals_count);
        <OpenBridgeProposals<T>>::insert(voting_deadline, open_proposals);
        <OpenBridgeProposalsHashes<T>>::insert(proposal_hash, proposal_id);
        <OpenBridgeProposalsHashesIndex<T>>::insert(proposal_id, proposal_hash);

        Ok(())
    }
}

/// tests for this module
#[cfg(test)]
mod tests {
    use super::*;

    use primitives::{Blake2Hasher, H256};
    use runtime_io::with_externalities;
    use runtime_primitives::{
        testing::{Digest, DigestItem, Header},
        traits::{BlakeTwo256, IdentityLookup},
        BuildStorage,
    };
    use support::{assert_ok, impl_outer_origin};

    impl_outer_origin! {
        pub enum Origin for Test {}
    }

    // For testing the module, we construct most of a mock runtime. This means
    // first constructing a configuration type (`Test`) which `impl`s each of the
    // configuration traits of modules we want to use.
    #[derive(Clone, Eq, PartialEq)]
    pub struct Test;
    impl system::Trait for Test {
        type Origin = Origin;
        type Index = u64;
        type BlockNumber = u64;
        type Hash = H256;
        type Hashing = BlakeTwo256;
        type Digest = Digest;
        type AccountId = u64;
        type Lookup = IdentityLookup<Self::AccountId>;
        type Header = Header;
        type Event = ();
        type Log = DigestItem;
    }
    impl balances::Trait for Test {
        type Balance = u128;
        type OnFreeBalanceZero = ();
        type OnNewAccount = ();
        type TransactionPayment = ();
        type TransferPayment = ();
        type DustRemoval = ();
        type Event = ();
    }
    impl timestamp::Trait for Test {
        type Moment = u64;
        type OnTimestampSet = ();
    }
    impl token::Trait for Test {
        type Event = ();
    }
    impl Trait for Test {
        type Event = ();
    }

    type BridgeModule = Module<Test>;
    type TokenModule = token::Module<Test>;

    const MESSAGE_ID: &[u8; 67] =
        b"0x5617efe391579685918e26bf24504b9602268c70d8edbf01b5dc8230db92ba65b";
    const ETH_ADDRESS: &[u8; 42] = b"0x00b46c2526e227482e2ebb8f4c69e4674d262e75";
    const USER1: u64 = 1;
    const USER2: u64 = 2;

    // This function basically just builds a genesis storage key/value store according to
    // our desired mockup.
    fn new_test_ext() -> runtime_io::TestExternalities<Blake2Hasher> {
        let mut r = system::GenesisConfig::<Test>::default()
            .build_storage()
            .unwrap()
            .0;

        r.extend(
            balances::GenesisConfig::<Test> {
                balances: vec![(USER1, 100000), (USER2, 300000)],
                vesting: vec![],
                transaction_base_fee: 0,
                transaction_byte_fee: 0,
                existential_deposit: 500,
                transfer_fee: 0,
                creation_fee: 0,
            }
            .build_storage()
            .unwrap()
            .0,
        );

        r.into()
    }

    #[test]
    fn token_eth2sub_mint_works() {
        with_externalities(&mut new_test_ext(), || {
            assert_ok!(BridgeModule::eth2substrate(
                Origin::signed(USER2),
                MESSAGE_ID.to_vec(),
                ETH_ADDRESS.to_vec(),
                USER2,
                1000
            ));
            assert_ok!(BridgeModule::eth2substrate(
                Origin::signed(USER1),
                MESSAGE_ID.to_vec(),
                ETH_ADDRESS.to_vec(),
                USER2,
                1000
            ));
            assert_eq!(TokenModule::balance_of((0, USER2)), 1000);
            assert_eq!(TokenModule::total_supply(0), 1000);
        })
    }

    #[test]
    fn token_sub2eth_burn_works() {
        with_externalities(&mut new_test_ext(), || {
            assert_ok!(BridgeModule::eth2substrate(
                Origin::signed(USER2),
                MESSAGE_ID.to_vec(),
                ETH_ADDRESS.to_vec(),
                USER2,
                1000
            ));
            assert_ok!(BridgeModule::eth2substrate(
                Origin::signed(USER1),
                MESSAGE_ID.to_vec(),
                ETH_ADDRESS.to_vec(),
                USER2,
                1000
            ));
            assert_eq!(TokenModule::balance_of((0, USER2)), 1000);
            assert_eq!(TokenModule::total_supply(0), 1000);
            assert_ok!(BridgeModule::substrate2eth(
                Origin::signed(USER1),
                MESSAGE_ID.to_vec(),
                ETH_ADDRESS.to_vec(),
                USER2,
                500
            ));
            assert_ok!(BridgeModule::substrate2eth(
                Origin::signed(USER2),
                MESSAGE_ID.to_vec(),
                ETH_ADDRESS.to_vec(),
                USER2,
                500
            ));
            assert_eq!(TokenModule::balance_of((0, USER2)), 500);
            assert_eq!(TokenModule::total_supply(0), 500);
        })
    }
}
