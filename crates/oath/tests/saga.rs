use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};

use oath::{
    AppendError, Aggregate, Rejected, Checkpoint, CommandId, Commands, Context, Decided, Decision, Handle, Loaded, Logged,
    MemoryStore, NotAllowed, Pending, Position, Project, Projected, React, Reacted, Row, Saga, SagaStore, SendError,
    Store, Transition, Version, View, ViewStore,
};
use pollster::block_on;

#[oath::id(prefix = "acc_")] pub struct AccountId(String);
#[oath::id(prefix = "tr_")] pub struct TransferId(String);

#[oath::command] pub struct OpenAccount { #[id] pub account: AccountId }
#[oath::command] pub struct Deposit { #[id] pub account: AccountId, pub amount: u64 }
#[oath::command] pub struct Withdraw { #[id] pub account: AccountId, pub amount: u64, pub transfer: TransferId }
#[oath::event] pub struct AccountOpened;
#[oath::event] pub struct Deposited { pub amount: u64 }
#[oath::event] pub struct Withdrawn { pub amount: u64, pub transfer: TransferId }
#[oath::rejection] pub enum AccountRejection { Insufficient { balance: u64 } }

#[oath::state] pub struct New;
#[oath::state] pub struct Open { balance: u64 }

#[oath::aggregate(id = AccountId, rejection = AccountRejection)]
pub enum Account {
    #[initial]
    #[state(accepts(OpenAccount), on(AccountOpened => Open))]
    New,
    #[state(accepts(Deposit, Withdraw), on(Deposited => Open, Withdrawn => Open))]
    Open,
}

impl Handle<OpenAccount> for New {
    fn decide(&self, _: &OpenAccount) -> Decision<Self> { Ok(self.emit(AccountOpened).into()) }
}
impl Transition<AccountOpened> for New {
    fn evolve(self, _: &AccountOpened) -> Open { Open { balance: 0 } }
}
impl Handle<Deposit> for Open {
    fn decide(&self, c: &Deposit) -> Decision<Self> { Ok(self.emit(Deposited { amount: c.amount }).into()) }
}
impl Handle<Withdraw> for Open {
    fn decide(&self, c: &Withdraw) -> Decision<Self> {
        if c.amount > self.balance {
            return Err(AccountRejection::Insufficient { balance: self.balance });
        }
        Ok(self.emit(Withdrawn { amount: c.amount, transfer: c.transfer.clone() }).into())
    }
}
impl Transition<Deposited> for Open {
    fn evolve(self, e: &Deposited) -> Open { Open { balance: self.balance + e.amount } }
}
impl Transition<Withdrawn> for Open {
    fn evolve(self, e: &Withdrawn) -> Open { Open { balance: self.balance - e.amount } }
}

#[oath::command] pub struct RequestTransfer { #[id] pub transfer: TransferId, pub from: AccountId, pub to: AccountId, pub amount: u64 }
#[oath::command] pub struct CompleteTransfer { #[id] pub transfer: TransferId }
#[oath::command] pub struct FailTransfer { #[id] pub transfer: TransferId }
#[oath::event] pub struct TransferRequested { pub from: AccountId, pub to: AccountId, pub amount: u64 }
#[oath::event] pub struct TransferCompleted;
#[oath::event] pub struct TransferFailed;
#[oath::rejection] pub enum TransferRejection {}

#[oath::state] pub struct Start;
#[oath::state] pub struct Requested;
#[oath::state] pub struct Completed;
#[oath::state] pub struct Failed;

#[oath::aggregate(id = TransferId, rejection = TransferRejection)]
pub enum Transfer {
    #[initial]
    #[state(accepts(RequestTransfer), on(TransferRequested => Requested))]
    Start,
    #[state(accepts(CompleteTransfer, FailTransfer), on(TransferCompleted => Completed, TransferFailed => Failed))]
    Requested,
    Completed,
    Failed,
}

impl Handle<RequestTransfer> for Start {
    fn decide(&self, c: &RequestTransfer) -> Decision<Self> {
        Ok(self.emit(TransferRequested { from: c.from.clone(), to: c.to.clone(), amount: c.amount }).into())
    }
}
impl Transition<TransferRequested> for Start {
    fn evolve(self, _: &TransferRequested) -> Requested { Requested }
}
impl Handle<CompleteTransfer> for Requested {
    fn decide(&self, _: &CompleteTransfer) -> Decision<Self> { Ok(self.emit(TransferCompleted).into()) }
}
impl Transition<TransferCompleted> for Requested {
    fn evolve(self, _: &TransferCompleted) -> Completed { Completed }
}
impl Handle<FailTransfer> for Requested {
    fn decide(&self, _: &FailTransfer) -> Decision<Self> { Ok(self.emit(TransferFailed).into()) }
}
impl Transition<TransferFailed> for Requested {
    fn evolve(self, _: &TransferFailed) -> Failed { Failed }
}

#[oath::context(aggregates = [Account, Transfer], views = [Balance], sagas = [MoveMoney])]
pub struct Bank;

#[oath::view(key = AccountId, on(Deposited, Withdrawn))]
#[derive(Default)]
pub struct Balance { pub amount: u64 }

impl Project<Deposited> for Balance {
    fn key(account: &AccountId, _: &Deposited) -> AccountId { account.clone() }
    fn project(&mut self, e: &Deposited) { self.amount += e.amount; }
}
impl Project<Withdrawn> for Balance {
    fn key(account: &AccountId, _: &Withdrawn) -> AccountId { account.clone() }
    fn project(&mut self, e: &Withdrawn) { self.amount -= e.amount; }
}

/// Moves money for a requested transfer: withdraws from one account, then deposits into the
/// other and completes the transfer.
#[oath::saga(key = TransferId, on(TransferRequested, Withdrawn))]
#[derive(Default)]
pub struct MoveMoney { to: Option<AccountId>, amount: u64 }

impl React<TransferRequested> for MoveMoney {
    fn key(transfer: &TransferId, _: &TransferRequested) -> TransferId { transfer.clone() }
    fn react(&mut self, transfer: &TransferId, e: &TransferRequested) -> Commands<Self> {
        self.to = Some(e.to.clone());
        self.amount = e.amount;
        vec![Withdraw { account: e.from.clone(), amount: e.amount, transfer: transfer.clone() }.into()]
    }
}
impl React<Withdrawn> for MoveMoney {
    fn key(_: &AccountId, e: &Withdrawn) -> TransferId { e.transfer.clone() }
    fn react(&mut self, transfer: &TransferId, _: &Withdrawn) -> Commands<Self> {
        let to = self.to.clone().expect("a transfer is requested before its money is withdrawn");
        vec![Deposit { account: to, amount: self.amount }.into(), CompleteTransfer { transfer: transfer.clone() }.into()]
    }
}

// A withdrawal the account refuses fails the transfer.
impl Rejected for MoveMoney {
    fn rejected(&mut self, transfer: &TransferId, _: &BankRejection) -> Commands<Self> {
        vec![FailTransfer { transfer: transfer.clone() }.into()]
    }
}

fn acc(id: &str) -> AccountId { AccountId::new(id).unwrap() }
fn tr(id: &str) -> TransferId { TransferId::new(id).unwrap() }

fn send(bank: &mut Bank, command: impl Into<BankCommand>) -> Result<Vec<BankEvent>, BankRejection> {
    match block_on(bank.send(command)) {
        Ok(events) => Ok(events),
        Err(SendError::Rejected(rejection)) => Err(rejection),
        Err(error) => panic!("{error}"),
    }
}

fn balance(bank: &Bank, account: &str) -> u64 {
    block_on(bank.view::<Balance>(&acc(account))).unwrap().amount
}

/// Opens accounts `acc_a` with `funds` and `acc_b` with nothing.
fn open(bank: &mut Bank, funds: u64) {
    send(bank, OpenAccount { account: acc("acc_a") }).unwrap();
    send(bank, OpenAccount { account: acc("acc_b") }).unwrap();
    send(bank, Deposit { account: acc("acc_a"), amount: funds }).unwrap();
}

fn not_allowed(state: &str, command: &str) -> Result<Vec<BankEvent>, BankRejection> {
    Err(BankRejection::Transfer(TransferRejection::NotAllowed(NotAllowed { state: state.into(), command: command.into() })))
}

/// A store that keeps sagas apart from aggregates and views, so a test can give a context a
/// saga store with nothing in it, as when the saga is new. Its first `fail_delivered` calls
/// to mark a saga command sent fail, as when the process crashes after sending it.
#[derive(Clone, Default)]
struct Crashing {
    store: Arc<MemoryStore>,
    sagas: Arc<MemoryStore>,
    fail_delivered: Arc<AtomicUsize>,
    /// How many times a view or a saga loaded its checkpoint.
    view_checkpoints: Arc<AtomicUsize>,
    saga_checkpoints: Arc<AtomicUsize>,
}

impl<A: Aggregate> Store<A> for Crashing
where
    MemoryStore: Store<A>,
{
    type Error = <MemoryStore as Store<A>>::Error;
    async fn load(&self, id: &A::Id) -> Result<Loaded<A>, Self::Error> { Store::<A>::load(&*self.store, id).await }
    async fn events(&self, id: &A::Id, after: Version) -> Result<Vec<A::Event>, Self::Error> {
        Store::<A>::events(&*self.store, id, after).await
    }
    async fn append(&self, decided: &Decided<A>) -> Result<(), AppendError<Self::Error>> {
        Store::<A>::append(&*self.store, decided).await
    }
    async fn head(&self) -> Result<Position, Self::Error> { Store::<A>::head(&*self.store).await }
    async fn log(&self, after: Position, limit: usize) -> Result<Vec<Logged<A>>, Self::Error> {
        Store::<A>::log(&*self.store, after, limit).await
    }
}

impl<V: View> ViewStore<V> for Crashing
where
    MemoryStore: ViewStore<V>,
{
    type Error = <MemoryStore as ViewStore<V>>::Error;
    async fn checkpoint(&self) -> Result<Option<Checkpoint<V>>, Self::Error> {
        self.view_checkpoints.fetch_add(1, SeqCst);
        ViewStore::<V>::checkpoint(&*self.store).await
    }
    async fn row(&self, key: &V::Key) -> Result<Option<Row<V>>, Self::Error> { ViewStore::<V>::row(&*self.store, key).await }
    async fn save(&self, projected: &Projected<V>) -> Result<(), AppendError<Self::Error>> {
        ViewStore::<V>::save(&*self.store, projected).await
    }
}

#[derive(Debug)]
struct Crashed;
impl std::fmt::Display for Crashed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str("crashed") }
}
impl std::error::Error for Crashed {}

impl<S: Saga> SagaStore<S> for Crashing
where
    MemoryStore: SagaStore<S>,
{
    type Error = Crashed;
    async fn checkpoint(&self) -> Result<Option<Checkpoint<S>>, Crashed> {
        self.saga_checkpoints.fetch_add(1, SeqCst);
        Ok(SagaStore::<S>::checkpoint(&*self.sagas).await.unwrap())
    }
    async fn row(&self, key: &S::Key) -> Result<Option<Row<S>>, Crashed> { Ok(SagaStore::<S>::row(&*self.sagas, key).await.unwrap()) }
    async fn save(&self, reacted: &Reacted<S>) -> Result<(), AppendError<Crashed>> {
        SagaStore::<S>::save(&*self.sagas, reacted).await.map_err(|_| AppendError::Conflict)
    }
    async fn pending(&self) -> Result<Vec<Pending<S>>, Crashed> { Ok(SagaStore::<S>::pending(&*self.sagas).await.unwrap()) }
    async fn delivered(&self, id: &CommandId) -> Result<(), Crashed> {
        if self.fail_delivered.try_update(SeqCst, SeqCst, |n| n.checked_sub(1)).is_ok() {
            return Err(Crashed);
        }
        SagaStore::<S>::delivered(&*self.sagas, id).await.unwrap();
        Ok(())
    }
}

#[test]
fn a_saga_carries_a_transfer_to_its_end_within_one_send() {
    let mut bank = Bank::new(MemoryStore::default());
    open(&mut bank, 100);
    let requested = send(&mut bank, RequestTransfer { transfer: tr("tr_1"), from: acc("acc_a"), to: acc("acc_b"), amount: 40 });
    // send returns the events of the command it was given; the saga's commands ran after it.
    assert_eq!(requested.map(|events| events.len()), Ok(1));
    assert_eq!((balance(&bank, "acc_a"), balance(&bank, "acc_b")), (60, 40));
    assert_eq!(send(&mut bank, CompleteTransfer { transfer: tr("tr_1") }), not_allowed("Completed", "CompleteTransfer"));
}

#[test]
fn a_rejected_saga_command_goes_to_the_saga_which_fails_the_transfer() {
    let store = Crashing::default();
    let mut bank = Bank::new(store.clone());
    open(&mut bank, 10);
    send(&mut bank, RequestTransfer { transfer: tr("tr_1"), from: acc("acc_a"), to: acc("acc_b"), amount: 40 }).unwrap();

    // The withdrawal was refused, so no money moved and the saga failed the transfer.
    assert_eq!((balance(&bank, "acc_a"), balance(&bank, "acc_b")), (10, 0));
    assert!(block_on(SagaStore::<MoveMoney>::pending(&*store.sagas)).unwrap().is_empty());
    assert_eq!(send(&mut bank, CompleteTransfer { transfer: tr("tr_1") }), not_allowed("Failed", "CompleteTransfer"));
}

#[test]
fn a_send_skips_the_views_and_sagas_that_listen_for_none_of_its_events() {
    let store = Crashing::default();
    let mut bank = Bank::new(store.clone());
    open(&mut bank, 100);
    store.view_checkpoints.store(0, SeqCst);
    store.saga_checkpoints.store(0, SeqCst);

    // Balance listens for neither; MoveMoney listens for neither.
    send(&mut bank, OpenAccount { account: acc("acc_c") }).unwrap();
    assert_eq!((store.view_checkpoints.load(SeqCst), store.saga_checkpoints.load(SeqCst)), (0, 0));
    // Balance listens for Deposited; MoveMoney doesn't.
    send(&mut bank, Deposit { account: acc("acc_c"), amount: 5 }).unwrap();
    assert!(store.view_checkpoints.load(SeqCst) > 0);
    assert_eq!(store.saga_checkpoints.load(SeqCst), 0);

    // A transfer still runs: the saga reads past the events it skipped.
    send(&mut bank, RequestTransfer { transfer: tr("tr_1"), from: acc("acc_a"), to: acc("acc_c"), amount: 40 }).unwrap();
    assert_eq!((balance(&bank, "acc_a"), balance(&bank, "acc_c")), (60, 45));
}

#[test]
fn a_saga_command_sent_again_after_a_crash_is_recorded_once() {
    let store = Crashing::default();
    let mut bank = Bank::new(store.clone());
    open(&mut bank, 100);

    // The withdrawal is recorded, but the crash loses the note that it was sent.
    store.fail_delivered.store(1, SeqCst);
    send(&mut bank, RequestTransfer { transfer: tr("tr_1"), from: acc("acc_a"), to: acc("acc_b"), amount: 40 }).unwrap();
    assert_eq!((balance(&bank, "acc_a"), balance(&bank, "acc_b")), (60, 0));
    assert_eq!(block_on(SagaStore::<MoveMoney>::pending(&*store.sagas)).unwrap().len(), 1);

    // The next send sends the withdrawal again, which is not recorded twice, and finishes the
    // transfer.
    send(&mut bank, OpenAccount { account: acc("acc_c") }).unwrap();
    assert_eq!((balance(&bank, "acc_a"), balance(&bank, "acc_b")), (60, 40));
    assert!(block_on(SagaStore::<MoveMoney>::pending(&*store.sagas)).unwrap().is_empty());
}

#[test]
fn a_new_saga_reacts_only_to_events_recorded_after_it_was_added() {
    // tr_1 was requested before there was enough money, and its withdrawal was refused.
    let store = Crashing::default();
    let mut bank = Bank::new(store.clone());
    open(&mut bank, 10);
    send(&mut bank, RequestTransfer { transfer: tr("tr_1"), from: acc("acc_a"), to: acc("acc_b"), amount: 40 }).unwrap();

    // A context whose saga store is empty, as for a saga just added: it starts at the end of
    // the log, so it never withdraws for tr_1, but it carries tr_2 through.
    let mut added = Bank::new(Crashing { sagas: Arc::default(), ..store });
    send(&mut added, Deposit { account: acc("acc_a"), amount: 100 }).unwrap();
    assert_eq!(balance(&added, "acc_a"), 110);
    send(&mut added, RequestTransfer { transfer: tr("tr_2"), from: acc("acc_a"), to: acc("acc_b"), amount: 5 }).unwrap();
    assert_eq!((balance(&added, "acc_a"), balance(&added, "acc_b")), (105, 5));
}

#[oath::id(prefix = "ech_")] pub struct EchoId(String);
#[oath::command] pub struct Shout { #[id] pub echo: EchoId }
#[oath::event] pub struct Shouted;
#[oath::rejection] pub enum EchoRejection {}

#[oath::state] pub struct Quiet;
#[oath::state] pub struct Loud;

#[oath::aggregate(id = EchoId, rejection = EchoRejection)]
pub enum Echo {
    #[initial]
    #[state(accepts(Shout), on(Shouted => Loud))]
    Quiet,
    #[state(accepts(Shout), on(Shouted => Loud))]
    Loud,
}

impl Handle<Shout> for Quiet {
    fn decide(&self, _: &Shout) -> Decision<Self> { Ok(self.emit(Shouted).into()) }
}
impl Handle<Shout> for Loud {
    fn decide(&self, _: &Shout) -> Decision<Self> { Ok(self.emit(Shouted).into()) }
}
impl Transition<Shouted> for Quiet {
    fn evolve(self, _: &Shouted) -> Loud { Loud }
}
impl Transition<Shouted> for Loud {
    fn evolve(self, _: &Shouted) -> Loud { Loud }
}

#[oath::context(aggregates = [Echo], sagas = [Repeat])]
pub struct Canyon;

/// Shouts back at every shout, without end.
#[oath::saga(key = EchoId, on(Shouted))]
#[derive(Default)]
pub struct Repeat;

impl Rejected for Repeat {
    fn rejected(&mut self, _: &EchoId, _: &CanyonRejection) -> Commands<Self> { vec![] }
}

impl React<Shouted> for Repeat {
    fn key(echo: &EchoId, _: &Shouted) -> EchoId { echo.clone() }
    fn react(&mut self, echo: &EchoId, _: &Shouted) -> Commands<Self> { vec![Shout { echo: echo.clone() }.into()] }
}

#[test]
fn sagas_that_react_to_each_other_without_end_stop_after_the_rounds() {
    let store = Arc::new(MemoryStore::default());
    let mut canyon = Canyon::new(StoreOf(store.clone()));
    let error = block_on(canyon.send(Shout { echo: EchoId::new("ech_1").unwrap() })).unwrap_err();
    assert!(matches!(error, SendError::Runaway), "{error}");
    // The shout and one echo per round are recorded.
    let shouts = block_on(Store::<Echo>::events(&*store, &EchoId::new("ech_1").unwrap(), Version(0))).unwrap();
    assert_eq!(shouts.len(), 1 + oath::ROUNDS);
}

/// A shared MemoryStore, so a test can read what a context wrote.
struct StoreOf(Arc<MemoryStore>);

impl Store<Echo> for StoreOf {
    type Error = std::convert::Infallible;
    async fn load(&self, id: &EchoId) -> Result<Loaded<Echo>, Self::Error> { Store::<Echo>::load(&*self.0, id).await }
    async fn events(&self, id: &EchoId, after: Version) -> Result<Vec<EchoEvent>, Self::Error> {
        Store::<Echo>::events(&*self.0, id, after).await
    }
    async fn append(&self, decided: &Decided<Echo>) -> Result<(), AppendError<Self::Error>> {
        Store::<Echo>::append(&*self.0, decided).await
    }
    async fn head(&self) -> Result<Position, Self::Error> { Store::<Echo>::head(&*self.0).await }
    async fn log(&self, after: Position, limit: usize) -> Result<Vec<Logged<Echo>>, Self::Error> {
        Store::<Echo>::log(&*self.0, after, limit).await
    }
}

impl SagaStore<Repeat> for StoreOf {
    type Error = std::convert::Infallible;
    async fn checkpoint(&self) -> Result<Option<Checkpoint<Repeat>>, Self::Error> { SagaStore::<Repeat>::checkpoint(&*self.0).await }
    async fn row(&self, key: &EchoId) -> Result<Option<Row<Repeat>>, Self::Error> { SagaStore::<Repeat>::row(&*self.0, key).await }
    async fn save(&self, reacted: &Reacted<Repeat>) -> Result<(), AppendError<Self::Error>> {
        SagaStore::<Repeat>::save(&*self.0, reacted).await
    }
    async fn pending(&self) -> Result<Vec<Pending<Repeat>>, Self::Error> { SagaStore::<Repeat>::pending(&*self.0).await }
    async fn delivered(&self, id: &CommandId) -> Result<(), Self::Error> { SagaStore::<Repeat>::delivered(&*self.0, id).await }
}
