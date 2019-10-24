use sr_primitives::{RuntimeAppPublic};
use sr_primitives::traits::{
	SimpleArithmetic, Member, One, Zero,
	CheckedAdd, CheckedSub,
	Saturating, Bounded, SaturatedConversion,
};
use sr_primitives::transaction_validity::{
	TransactionValidity, TransactionLongevity, ValidTransaction, InvalidTransaction,
};
use rstd::result;
use support::dispatch::Result;
use support::{
	decl_module, decl_storage, decl_event, Parameter, ensure,
	traits::{
		LockableCurrency, Currency,
		OnUnbalanced,
	}
};
use system::ensure_signed;
use system::offchain::SubmitUnsignedTransaction;
use codec::{Encode, Decode};
use rstd::vec::Vec;
use crate::traits::ItemTransfer;

/// The module's configuration trait.
pub trait Trait: timestamp::Trait + aura::Trait {
	/// Item Id
	type ItemId: Parameter
		+ Member
		+ SimpleArithmetic
		+ Bounded
		+ Default
		+ Copy;

	/// Auction Id
	type AuctionId: Parameter
		+ Member
		+ SimpleArithmetic
		+ Bounded
		+ Default
		+ Copy;

	/// Currency type for this module.
	type Currency: LockableCurrency<Self::AccountId>;

	/// The overarching event type.
	type Event: From<Event<Self>> + Into<<Self as system::Trait>::Event>;

	/// A dispatchable call type.
	type Call: From<Call<Self>>;

	/// A transaction submitter.
	type SubmitTransaction: SubmitUnsignedTransaction<Self, <Self as Trait>::Call>;
	
	/// Interface for transfer item
	type AuctionTransfer: ItemTransfer<Self::AccountId, Self::ItemId>;

	/// Handler for the unbalanced reduction when taking a auction fee.
	type OnAuctionPayment: OnUnbalanced<NegativeImbalanceOf<Self>>;
}

pub type BalanceOf<T> = <<T as Trait>::Currency as Currency<<T as system::Trait>::AccountId>>::Balance;
type NegativeImbalanceOf<T> =
	<<T as Trait>::Currency as Currency<<T as system::Trait>::AccountId>>::NegativeImbalance;

#[derive(Encode, Decode, Clone, Copy, Eq, PartialEq)]
#[cfg_attr(feature = "std", derive(Debug))]
pub enum AuctionStatus {
	PendingStart,
	Paused,
	Active,
	Stopped,
}

#[derive(Encode, Decode, Clone, PartialEq, Copy)]
#[cfg_attr(feature = "std", derive(Debug))]
pub struct Auction<T> where T: Trait {
	id: T::AuctionId,
	item: T::ItemId, // 拍卖物品id
	owner: T::AccountId, // 拍卖管理账户，可以控制暂停和继续
	start_at: Option<T::Moment>, // 自动开始时间
	stop_at: Option<T::Moment>, // 截止时间
	wait_period: Option<T::Moment>, // 等待时间
	begin_price: BalanceOf<T>, // 起拍价
	upper_bound_price: Option<BalanceOf<T>>, // 封顶价（可选）
	minimum_step: BalanceOf<T>, // 最小加价幅度
	latest_participate: Option<(T::AccountId, T::Moment)>, // 最后出价人/时间
	status: AuctionStatus,
}

// This module's storage items.
decl_storage! {
	trait Store for Module<T: Trait> as Auction {
		NextAuctionId get(next_auction_id): T::AuctionId;
		
		// 物品id映射auctionid，一个物品只能在一个auction中参拍，创建auction后添加映射，auction结束后删除映射
		AuctionItems get(auction_items): map T::ItemId => Option<T::AuctionId>;
		Auctions get(auctions): map T::AuctionId => Option<Auction<T>>;
		AuctionBids get(auction_bids): double_map T::AuctionId, twox_128(T::AccountId) => Option<BalanceOf<T>>;
		AuctionParticipants get(action_participants): map T::AuctionId => Option<Vec<T::AccountId>>;
		PendingAuctions get(pending_auctions): Vec<T::AuctionId>; // 尚未开始的auction
		ActiveAuctions get(active_auctions): Vec<T::AuctionId>; // 尚未结束的auction，已经暂停的也在这里
	}
}

// add by sunhao 20191023
decl_event!(
	pub enum Event<T> where
		<T as system::Trait>::AccountId,
		<T as Trait>::AuctionId,
		Balance = BalanceOf<T>,
	{
		/// A price and/or amount is changed in some auction. 
		/// (auction_id, latest_bidder, latest_price, remain_amount)
		BidderUpdated(AuctionId, AccountId, Balance, u32),
		/// A auction's status has changed. (auction_id, status_from, status_to)
		AuctionUpdated(AuctionId, AuctionStatus, AuctionStatus),
	}
);

// The module's dispatchable functions.
decl_module! {
	/// The module declaration.
	pub struct Module<T: Trait> for enum Call where origin: T::Origin {
		// Initializing events
		fn deposit_event() = default;

		pub fn create_auction(origin,
			// item: T::ItemId,//竞拍对象
			begin_price: BalanceOf<T>,//起拍价
			minimum_step: BalanceOf<T>,//最小加价幅度
			upper_bound_price: Option<BalanceOf<T>>,//封顶价
			// start_at: T::Moment,//起拍时间
			// stop_at: T::Moment,//结束时间
			// wait_period: T::Moment //竞价等待时间
		) -> Result {
			let sender = ensure_signed(origin)?;

			Self::do_create_auction(&sender, begin_price,minimum_step, upper_bound_price)?;

			Ok(())
		}

		// setup start and/or stop Moment, and wait_period after someone's bid
		// add by sunhao 20191023
		pub fn setup_moments(origin,
			auction_id: T::AuctionId, 
			start_at: Option<T::Moment>,  //起拍时间
			stop_at: Option<T::Moment>,  //结束时间
			wait_period: Option<T::Moment>  //竞价等待时间
		) -> Result {
			let sender = ensure_signed(origin)?;

			// unwrap auction and ensure its status is PendingStart
			let auction = Self::auctions(auction_id);
			ensure!(auction.is_some(), "Auction does not exist");
			let mut auction = auction.unwrap();
			ensure!(auction.status == AuctionStatus::PendingStart, 
				"Auction is already started or over.");
			
			// ensure only owner can call this
			ensure!(auction.owner == sender, "Only owner can call this fn.");

			// set moments into storage
			if start_at.is_some() {
				auction.start_at = start_at;
			}
			if stop_at.is_some() {
				auction.stop_at = stop_at;
			}
			if wait_period.is_some() {
				auction.wait_period = wait_period;
			}
				
			Ok(())
		}

		// Owner can pause the auction when it is in active.
		// add by sunhao 20191024
		pub fn pause_auction(origin, auction_id: T::AuctionId) -> Result {
			let sender = ensure_signed(origin)?;

			// unwrap auction and ensure its status is Active
			let auction = Self::auctions(auction_id);
			ensure!(auction.is_some(), "Auction does not exist");
			let mut auction = auction.unwrap();
			ensure!(auction.status == AuctionStatus::Active, 
				"Auction can NOT be paused now.");
			
			// ensure only owner can call this
			ensure!(auction.owner == sender, "Only owner can call this fn.");

			// change status of auction
			auction.status = AuctionStatus::Paused;

			// emit event
			Self::deposit_event(RawEvent::AuctionUpdated(auction_id, 
				AuctionStatus::Active, AuctionStatus::Paused));

			Ok(())
		}

		// Owner can resume the auction paused before.
		// add by sunhao 20191024
		pub fn resume_auction(origin, auction_id: T::AuctionId) -> Result {
			let sender = ensure_signed(origin)?;

			// unwrap auction and ensure its status is Paused
			let auction = Self::auctions(auction_id);
			ensure!(auction.is_some(), "Auction does not exist");
			let mut auction = auction.unwrap();
			ensure!(auction.status == AuctionStatus::Paused, 
				"Auction can NOT be resumed now.");
			
			// ensure only owner can call this
			ensure!(auction.owner == sender, "Only owner can call this fn.");

			// change status of auction
			auction.status = AuctionStatus::Active;

			// emit event
			Self::deposit_event(RawEvent::AuctionUpdated(auction_id, 
				AuctionStatus::Paused, AuctionStatus::Active));

			Ok(())
		}

		pub fn start_auction(
			origin,
			auction: T::AuctionId,
			signature: <<T as aura::Trait>::AuthorityId as RuntimeAppPublic>::Signature
		) -> Result { // Called by offchain worker
			Ok(())
		}

		// owner can stop an active or paused auction by his will.
		// add by sunhao 20191024
		pub fn stop_auction(
			origin,
			auction_id: T::AuctionId //,
			// signature: <<T as aura::Trait>::AuthorityId as RuntimeAppPublic>::Signature
		) -> Result {
			let sender = ensure_signed(origin)?;

			// unwrap auction and ensure its status is not stopped yet.
			let auction = Self::auctions(auction_id);
			ensure!(auction.is_some(), "Auction does not exist");
			let mut auction = auction.unwrap();
			ensure!(auction.status != AuctionStatus::Stopped,
				"Auction can NOT be stopped now.");
			
			// ensure only owner can call this
			ensure!(auction.owner == sender, "Only owner can call this fn.");

			Self::do_stop_auction(&mut auction)

		}

		pub fn participate_auction(
			origin,
			auction: T::AuctionId,
			price: BalanceOf<T>
		) -> Result {
			Ok(())
		}

		// Runs after every block.
		fn offchain_worker(now: <T as system::Trait>::BlockNumber) {
			// Only send messages if we are a potential validator.
			if runtime_io::is_validator() {
				Self::offchain(now);
			}
		}
	}
}

impl<T: Trait> Module<T> {
	fn get_next_auction_id() -> result::Result<T::AuctionId, &'static str> {
		let auction_id = Self::next_auction_id();
		if auction_id == T::AuctionId::max_value() {
			return Err("Auction count overflow");
		}
		Ok(auction_id)
	}

	fn insert_auction(owner: &T::AccountId, auction_id: T::AuctionId, auction:Auction<T>) {
		// Create and store kitty
		<Auctions<T>>::insert(auction_id, auction);
		<NextAuctionId<T>>::put(auction_id + 1.into());
	}

	fn do_create_auction(
		owner: &T::AccountId, 
		begin_price: BalanceOf<T>,//起拍价
		minimum_step: BalanceOf<T>,//最小加价幅度
		upper_bound_price: Option<BalanceOf<T>>
	) -> result::Result<T::AuctionId, &'static str> {
		// 判断id
		let auction_id = Self::get_next_auction_id()?;
		let new_auction = Auction {
			id: auction_id,
			item: 0.into(), // 拍卖物品id
			owner: (*owner).clone(), // 拍卖管理账户，可以控制暂停和继续
			begin_price: begin_price, // 起拍价
			minimum_step: minimum_step, // 最小加价幅度
			status: AuctionStatus::PendingStart,
			upper_bound_price: upper_bound_price,
			start_at: None,
			stop_at:None,
			wait_period: None,
			latest_participate: None,
		};
		Self::insert_auction(owner, auction_id, new_auction);
		Ok(auction_id)
	}

	// real work for stopping a auction.
	// added by sunhao 20191024
	fn do_stop_auction(auction: &mut Auction<T>) -> Result {
		// call settle func if needed.
		if auction.status != AuctionStatus::PendingStart {
			Self::do_settle_auction(auction.id)?;
		}

		// change status of auction
		let old_status = auction.status;
		auction.status = AuctionStatus::Stopped;
		
		// emit event
		Self::deposit_event(RawEvent::AuctionUpdated(auction.id, 
			old_status, AuctionStatus::Stopped));
		
		Ok(())
	}

	fn do_enable_auction(auction: T::AuctionId) -> Result {
		Ok(())
	}

	fn do_disable_auction(auction: T::AuctionId) -> Result {
		Ok(())
	}

	fn do_settle_auction(auction: T::AuctionId) -> Result {
		Ok(())
	}

	// ====== offchain worker related methods ======
	/// only run by current validator
	pub(crate) fn offchain(now: T::BlockNumber) {
		// TODO check auction start
		// TODO check auction end
	}
}

impl<T: Trait> support::unsigned::ValidateUnsigned for Module<T> {
	type Call = Call<T>;

	fn validate_unsigned(call: &Self::Call) -> TransactionValidity {
		// TODO
		InvalidTransaction::Call.into()
	}
}
