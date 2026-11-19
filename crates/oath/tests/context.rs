use std::any::Any;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Mutex};

use oath::{
    Aggregate, AppendError, Checkpoint, Context, Decided, Logged, Position, Projected, Row, View, ViewStore, Decision, Handle, Loaded, MemoryStore, NotAllowed, Project, SendError,
    Store, Transition, Version,
};
use pollster::block_on;

#[oath::id(prefix = "ord_")] pub struct OrderId(String);
#[oath::id(prefix = "cus_")] pub struct CustomerId(String);

#[oath::command] pub struct PlaceOrder { #[id] pub order: OrderId, pub total: u64 }
#[oath::command] pub struct PayOrder { #[id] pub order: OrderId, pub amount: u64 }
#[oath::event] pub struct OrderPlaced { pub total: u64 }
#[oath::event] pub struct OrderPaid { pub amount: u64 }
#[oath::rejection] pub enum OrderRejection { WrongAmount { expected: u64 } }

#[oath::state] pub struct Draft;
#[oath::state] pub struct Placed { total: u64 }
#[oath::state] pub struct Paid;

#[oath::aggregate(id = OrderId, rejection = OrderRejection)]
pub enum Order {
    #[initial]
    #[state(accepts(PlaceOrder), on(OrderPlaced => Placed))]
    Draft,
    #[state(accepts(PayOrder), on(OrderPaid => Paid))]
    Placed,
    Paid,
}

impl Handle<PlaceOrder> for Draft {
    fn decide(&self, c: &PlaceOrder) -> Decision<Self> {
        Ok(self.emit(OrderPlaced { total: c.total }).into())
    }
}
impl Transition<OrderPlaced> for Draft {
    fn evolve(self, e: &OrderPlaced) -> Placed { Placed { total: e.total } }
}
impl Handle<PayOrder> for Placed {
    fn decide(&self, c: &PayOrder) -> Decision<Self> {
        if c.amount != self.total {
            return Err(OrderRejection::WrongAmount { expected: self.total });
        }
        Ok(self.emit(OrderPaid { amount: c.amount }).into())
    }
}
impl Transition<OrderPaid> for Placed {
    fn evolve(self, _: &OrderPaid) -> Paid { Paid }
}

#[oath::command] pub struct OpenAccount { #[id] pub customer: CustomerId }
#[oath::event] pub struct AccountOpened;
#[oath::rejection] pub enum CustomerRejection {}

#[oath::state] pub struct New;
#[oath::state] pub struct Active;

#[oath::aggregate(id = CustomerId, rejection = CustomerRejection)]
pub enum Customer {
    #[initial]
    #[state(accepts(OpenAccount), on(AccountOpened => Active))]
    New,
    Active,
}

impl Handle<OpenAccount> for New {
    fn decide(&self, _: &OpenAccount) -> Decision<Self> {
        Ok(self.emit(AccountOpened).into())
    }
}
impl Transition<AccountOpened> for New {
    fn evolve(self, _: &AccountOpened) -> Active { Active }
}

#[oath::context(aggregates = [Order, Customer], views = [Revenue, Spent, Timeline])]
pub struct Shop;

/// Money placed and paid across every order: one row.
#[oath::view(key = (), on(OrderPlaced, OrderPaid))]
#[derive(Default)]
pub struct Revenue { pub placed: u64, pub paid: u64 }

impl Project<OrderPlaced> for Revenue {
    fn key(_: &OrderId, _: &OrderPlaced) {}
    fn project(&mut self, e: &OrderPlaced) { self.placed += e.total; }
}
impl Project<OrderPaid> for Revenue {
    fn key(_: &OrderId, _: &OrderPaid) {}
    fn project(&mut self, e: &OrderPaid) { self.paid += e.amount; }
}

/// What each order has been paid: one row per order.
#[oath::view(key = OrderId, on(OrderPaid))]
#[derive(Default)]
pub struct Spent { pub amount: u64 }

impl Project<OrderPaid> for Spent {
    fn key(order: &OrderId, _: &OrderPaid) -> OrderId { order.clone() }
    fn project(&mut self, e: &OrderPaid) { self.amount += e.amount; }
}

/// Every order placed and account opened, in the order they happened: one row.
#[oath::view(key = (), on(OrderPlaced, AccountOpened))]
#[derive(Default)]
pub struct Timeline(pub Vec<String>);

impl Project<OrderPlaced> for Timeline {
    fn key(_: &OrderId, _: &OrderPlaced) {}
    fn project(&mut self, e: &OrderPlaced) { self.0.push(format!("placed {}", e.total)); }
}
impl Project<AccountOpened> for Timeline {
    fn key(_: &CustomerId, _: &AccountOpened) {}
    fn project(&mut self, _: &AccountOpened) { self.0.push("opened".into()); }
}

fn ord(id: &str) -> OrderId { OrderId::new(id).unwrap() }
fn cus(id: &str) -> CustomerId { CustomerId::new(id).unwrap() }
fn shop() -> Shop { Shop::new(MemoryStore::default()) }

/// Sends `command`, and returns the events or the rejection; any other error fails the test.
fn send(shop: &mut Shop, command: impl Into<ShopCommand>) -> Result<Vec<ShopEvent>, ShopRejection> {
    match block_on(shop.send(command)) {
        Ok(events) => Ok(events),
        Err(SendError::Rejected(rejection)) => Err(rejection),
        Err(error) => panic!("{error}"),
    }
}

fn view<V: oath::View>(shop: &Shop, key: &V::Key) -> V
where
    Shop: oath::__private::Holds<V>,
{
    block_on(shop.view::<V>(key)).unwrap()
}

/// The rejection for `command`, which order instances in state `state` don't accept.
fn not_allowed(state: &str, command: &str) -> Result<Vec<ShopEvent>, ShopRejection> {
    Err(ShopRejection::Order(OrderRejection::NotAllowed(NotAllowed { state: state.into(), command: command.into() })))
}

/// A store shared by several contexts, as a database is. It counts its calls, refuses the first
/// `conflicts` appends as if another writer got there first, and can load what a lagging
/// replica or older code would return.
#[derive(Clone, Default)]
struct Shared {
    store: Arc<MemoryStore>,
    conflicts: Arc<AtomicUsize>,
    loads: Arc<AtomicUsize>,
    replays: Arc<AtomicUsize>,
    /// Returned by the next load instead of what the store holds: a load from before another
    /// writer's append.
    lagging: Arc<Mutex<Option<Box<dyn Any + Send>>>>,
    /// Loads snapshots as if written by older states: a different schema, and a state that
    /// still reads as `Placed { total: 1 }`.
    old_snapshots: Arc<AtomicBool>,
    /// Loads view checkpoints and rows as if saved by an older view struct.
    old_views: Arc<AtomicBool>,
    rows_loaded: Arc<AtomicUsize>,
    /// Returned by the next checkpoint load instead of the saved one: a load from before
    /// another context saved.
    lagging_checkpoint: Arc<Mutex<Option<Box<dyn Any + Send>>>>,
}

/// `saved` as if an older version of its view saved it, holding `state`.
fn older<T: serde::Serialize + serde::de::DeserializeOwned>(saved: T, state: Option<serde_json::Value>) -> T {
    let mut wire = serde_json::to_value(&saved).unwrap();
    wire["schema"] = serde_json::json!(1);
    if let Some(state) = state {
        wire["state"] = state;
    }
    serde_json::from_value(wire).unwrap()
}

impl<V: View> ViewStore<V> for Shared
where
    MemoryStore: ViewStore<V>,
{
    type Error = <MemoryStore as ViewStore<V>>::Error;

    async fn checkpoint(&self) -> Result<Option<Checkpoint<V>>, Self::Error> {
        if let Some(lagging) = self.lagging_checkpoint.lock().unwrap().take() {
            return Ok(Some(*lagging.downcast::<Checkpoint<V>>().unwrap()));
        }
        let checkpoint = ViewStore::<V>::checkpoint(&*self.store).await?;
        Ok(if self.old_views.load(SeqCst) { checkpoint.map(|c| older(c, None)) } else { checkpoint })
    }
    async fn row(&self, key: &V::Key) -> Result<Option<Row<V>>, Self::Error> {
        self.rows_loaded.fetch_add(1, SeqCst);
        let row = ViewStore::<V>::row(&*self.store, key).await?;
        let garbage = serde_json::json!({ "placed": 999, "paid": 999 });
        Ok(if self.old_views.load(SeqCst) { row.map(|row| older(row, Some(garbage.clone()))) } else { row })
    }
    async fn save(&self, projected: &Projected<V>) -> Result<(), AppendError<Self::Error>> {
        // A save is checked against the checkpoint the store holds, not the one it loaded.
        self.old_views.store(false, SeqCst);
        ViewStore::<V>::save(&*self.store, projected).await
    }
}

impl<A: Aggregate> Store<A> for Shared
where
    MemoryStore: Store<A>,
{
    type Error = <MemoryStore as Store<A>>::Error;

    async fn load(&self, id: &A::Id) -> Result<Loaded<A>, Self::Error> {
        self.loads.fetch_add(1, SeqCst);
        if let Some(lagging) = self.lagging.lock().unwrap().take() {
            return Ok(*lagging.downcast::<Loaded<A>>().unwrap());
        }
        let mut loaded = Store::<A>::load(&*self.store, id).await?;
        if self.old_snapshots.load(SeqCst) {
            loaded.snapshot = loaded.snapshot.map(|snapshot| {
                let mut wire = serde_json::to_value(&snapshot).unwrap();
                wire["schema"] = serde_json::json!(1);
                wire["state"] = serde_json::json!({ "Placed": { "total": 1 } });
                serde_json::from_value(wire).unwrap()
            });
        }
        Ok(loaded)
    }
    async fn events(&self, id: &A::Id, after: Version) -> Result<Vec<A::Event>, Self::Error> {
        self.replays.fetch_add(1, SeqCst);
        Store::<A>::events(&*self.store, id, after).await
    }
    async fn append(&self, decided: &Decided<A>) -> Result<(), AppendError<Self::Error>> {
        if self.conflicts.try_update(SeqCst, SeqCst, |n| n.checked_sub(1)).is_ok() {
            return Err(AppendError::Conflict);
        }
        Store::<A>::append(&*self.store, decided).await
    }
    async fn head(&self) -> Result<Position, Self::Error> {
        Store::<A>::head(&*self.store).await
    }
    async fn log(&self, after: Position, limit: usize) -> Result<Vec<Logged<A>>, Self::Error> {
        Store::<A>::log(&*self.store, after, limit).await
    }
}

#[test]
fn send_routes_each_command_to_the_instance_it_targets() {
    let mut shop = shop();
    assert_eq!(
        send(&mut shop, PlaceOrder { order: ord("ord_1"), total: 40 }),
        Ok(vec![ShopEvent::Order(OrderPlaced { total: 40 }.into())]),
    );
    send(&mut shop, PlaceOrder { order: ord("ord_2"), total: 7 }).unwrap();
    let pay: OrderCommand = PayOrder { order: ord("ord_1"), amount: 40 }.into();
    assert_eq!(send(&mut shop, pay), Ok(vec![ShopEvent::Order(OrderPaid { amount: 40 }.into())]));
    assert_eq!(
        send(&mut shop, OpenAccount { customer: cus("cus_1") }),
        Ok(vec![ShopEvent::Customer(AccountOpened.into())]),
    );

    // Each instance is in the state its own events moved it to.
    assert_eq!(send(&mut shop, PayOrder { order: ord("ord_1"), amount: 40 }), not_allowed("Paid", "PayOrder"));
    assert_eq!(send(&mut shop, PlaceOrder { order: ord("ord_2"), total: 7 }), not_allowed("Placed", "PlaceOrder"));
    assert_eq!(send(&mut shop, PayOrder { order: ord("ord_3"), amount: 7 }), not_allowed("Draft", "PayOrder"));
    assert_eq!(
        send(&mut shop, OpenAccount { customer: cus("cus_1") }),
        Err(ShopRejection::Customer(CustomerRejection::NotAllowed(NotAllowed {
            state: "Active".into(),
            command: "OpenAccount".into(),
        }))),
    );
}

#[test]
fn a_rejected_command_records_nothing() {
    let store = Shared::default();
    let mut shop = Shop::new(store.clone());
    assert_eq!(send(&mut shop, PayOrder { order: ord("ord_1"), amount: 40 }), not_allowed("Draft", "PayOrder"));
    send(&mut shop, PlaceOrder { order: ord("ord_1"), total: 40 }).unwrap();
    assert_eq!(
        send(&mut shop, PayOrder { order: ord("ord_1"), amount: 39 }),
        Err(ShopRejection::Order(OrderRejection::WrongAmount { expected: 40 })),
    );
    let loaded = block_on(Store::<Order>::load(&*store.store, &ord("ord_1"))).unwrap();
    assert_eq!(loaded.snapshot.map(|snapshot| snapshot.version()), Some(Version(1)));
    // The order is still placed, with the total it was placed with.
    assert!(send(&mut shop, PayOrder { order: ord("ord_1"), amount: 40 }).is_ok());
}

#[test]
fn send_loads_the_latest_snapshot_and_never_replays_the_history() {
    let store = Shared::default();
    let mut shop = Shop::new(store.clone());
    send(&mut shop, PlaceOrder { order: ord("ord_1"), total: 40 }).unwrap();
    send(&mut shop, PayOrder { order: ord("ord_1"), amount: 40 }).unwrap();

    let loaded = block_on(Store::<Order>::load(&*store.store, &ord("ord_1"))).unwrap();
    assert_eq!(loaded.snapshot.map(|snapshot| snapshot.version()), Some(Version(2)));
    assert!(loaded.events.is_empty());
    assert_eq!(block_on(Store::<Order>::events(&*store.store, &ord("ord_1"), Version(0))).unwrap().len(), 2);
    assert_eq!(send(&mut shop, PayOrder { order: ord("ord_1"), amount: 40 }), not_allowed("Paid", "PayOrder"));
    assert_eq!(store.replays.load(SeqCst), 0);
}

#[test]
fn a_snapshot_from_changed_states_is_ignored_and_the_events_replayed() {
    let store = Shared::default();
    let mut shop = Shop::new(store.clone());
    send(&mut shop, PlaceOrder { order: ord("ord_1"), total: 40 }).unwrap();

    // The snapshot's state still reads as the current `Placed`, but it meant something else then:
    // the order is replayed from its events, with the total it was placed with.
    store.old_snapshots.store(true, SeqCst);
    assert_eq!(
        send(&mut shop, PayOrder { order: ord("ord_1"), amount: 1 }),
        Err(ShopRejection::Order(OrderRejection::WrongAmount { expected: 40 })),
    );
    assert_eq!(store.replays.load(SeqCst), 1);
    assert!(send(&mut shop, PayOrder { order: ord("ord_1"), amount: 40 }).is_ok());
}

#[test]
fn a_memory_store_refuses_an_append_decided_against_a_stale_version() {
    let store = Shared::default();
    let mut shop = Shop::new(store.clone());
    send(&mut shop, PlaceOrder { order: ord("ord_1"), total: 40 }).unwrap();
    let placed = block_on(Store::<Order>::load(&*store.store, &ord("ord_1"))).unwrap();
    send(&mut shop, PayOrder { order: ord("ord_1"), amount: 40 }).unwrap();

    // The next load returns the order as it was before it was paid, as for a writer that loaded
    // it before the payment was appended. Its append is refused, and deciding again sees `Paid`.
    let lagging = Loaded::<Order> { snapshot: placed.snapshot.clone(), events: placed.events.clone() };
    *store.lagging.lock().unwrap() = Some(Box::new(lagging));
    assert_eq!(send(&mut shop, PayOrder { order: ord("ord_1"), amount: 40 }), not_allowed("Paid", "PayOrder"));
    assert_eq!(store.loads.load(SeqCst), 4);
    assert_eq!(block_on(Store::<Order>::events(&*store.store, &ord("ord_1"), Version(0))).unwrap().len(), 2);
}

#[test]
fn send_decides_again_after_a_conflict_and_gives_up_after_three() {
    // Two conflicts: the third attempt records the order.
    let store = Shared { conflicts: Arc::new(AtomicUsize::new(2)), ..Shared::default() };
    let mut shop = Shop::new(store.clone());
    assert!(send(&mut shop, PlaceOrder { order: ord("ord_1"), total: 40 }).is_ok());
    assert_eq!(store.loads.load(SeqCst), 3);

    // Three conflicts: nothing is recorded, and no view sees the event.
    store.conflicts.store(3, SeqCst);
    let error = block_on(shop.send(PlaceOrder { order: ord("ord_2"), total: 7 })).unwrap_err();
    assert!(matches!(error, SendError::Conflict), "{error}");
    assert!(block_on(Store::<Order>::events(&*store.store, &ord("ord_2"), Version(0))).unwrap().is_empty());
    assert_eq!(view::<Revenue>(&shop, &()).placed, 40);
}

#[test]
fn a_second_writer_decides_against_the_first_writers_events() {
    // Two contexts on one store, as two servers on one database.
    let store = Shared::default();
    let (mut first, mut second) = (Shop::new(store.clone()), Shop::new(store));
    send(&mut first, PlaceOrder { order: ord("ord_1"), total: 40 }).unwrap();
    assert!(send(&mut first, PayOrder { order: ord("ord_1"), amount: 40 }).is_ok());
    assert_eq!(send(&mut second, PayOrder { order: ord("ord_1"), amount: 40 }), not_allowed("Paid", "PayOrder"));
}

#[test]
fn commands_arrive_by_name_and_events_and_rejections_leave_under_their_aggregate() {
    use serde_json::{from_str, to_string};

    let mut shop = shop();
    let open: ShopCommand = from_str(r#"{"kind":"OpenAccount","customer":"cus_1"}"#).unwrap();
    assert_eq!(open, ShopCommand::Customer(OpenAccount { customer: cus("cus_1") }.into()));
    let place: ShopCommand = from_str(r#"{"total":40,"order":"ord_1","kind":"PlaceOrder"}"#).unwrap();
    assert_eq!(to_string(&place).unwrap(), r#"{"kind":"PlaceOrder","order":"ord_1","total":40}"#);
    let unknown = from_str::<ShopCommand>(r#"{"kind":"OrderPlaced","total":40}"#).unwrap_err();
    assert!(unknown.to_string().contains("no aggregate in Shop has a command named `OrderPlaced`"), "{unknown}");
    assert!(from_str::<ShopCommand>(r#"{"kind":"PlaceOrder","order":"cus_1","total":40}"#).is_err());

    send(&mut shop, open).unwrap();
    let events = send(&mut shop, place).unwrap();
    let wire = to_string(&events).unwrap();
    assert_eq!(wire, r#"[{"Order":{"kind":"OrderPlaced","total":40}}]"#);
    assert_eq!(from_str::<Vec<ShopEvent>>(&wire).unwrap(), events);

    let rejection = send(&mut shop, PayOrder { order: ord("ord_1"), amount: 1 }).unwrap_err();
    let wire = to_string(&rejection).unwrap();
    assert_eq!(wire, r#"{"Order":{"kind":"WrongAmount","expected":40}}"#);
    assert_eq!(from_str::<ShopRejection>(&wire).unwrap(), rejection);
}

#[test]
fn each_recorded_event_reaches_the_rows_it_updates() {
    let mut shop = shop();
    send(&mut shop, PlaceOrder { order: ord("ord_1"), total: 40 }).unwrap();
    send(&mut shop, PlaceOrder { order: ord("ord_2"), total: 7 }).unwrap();
    send(&mut shop, PayOrder { order: ord("ord_1"), amount: 40 }).unwrap();
    // Rejected commands record nothing, so no view sees them.
    send(&mut shop, PayOrder { order: ord("ord_2"), amount: 1 }).unwrap_err();

    let revenue = view::<Revenue>(&shop, &());
    assert_eq!((revenue.placed, revenue.paid), (47, 40));
    assert_eq!(view::<Spent>(&shop, &ord("ord_1")).amount, 40);
    // A row no event touched is the view's Default.
    assert_eq!(view::<Spent>(&shop, &ord("ord_2")).amount, 0);
}

#[test]
fn a_view_takes_in_events_of_several_aggregates_in_the_order_they_happened() {
    let mut shop = shop();
    send(&mut shop, PlaceOrder { order: ord("ord_1"), total: 1 }).unwrap();
    send(&mut shop, OpenAccount { customer: cus("cus_1") }).unwrap();
    send(&mut shop, PlaceOrder { order: ord("ord_2"), total: 2 }).unwrap();
    send(&mut shop, OpenAccount { customer: cus("cus_2") }).unwrap();
    assert_eq!(view::<Timeline>(&shop, &()).0, ["placed 1", "opened", "placed 2", "opened"]);
}

#[test]
fn a_new_context_catches_its_views_up_from_the_store_without_counting_twice() {
    let store = Shared::default();
    let mut first = Shop::new(store.clone());
    send(&mut first, PlaceOrder { order: ord("ord_1"), total: 40 }).unwrap();
    send(&mut first, PayOrder { order: ord("ord_1"), amount: 40 }).unwrap();

    // A second context on the same store, as after a restart or on another server.
    let mut second = Shop::new(store);
    assert_eq!(view::<Revenue>(&second, &()).placed, 40);
    send(&mut second, PlaceOrder { order: ord("ord_2"), total: 7 }).unwrap();
    // Both contexts update the same rows from the same checkpoint: each event counts once.
    assert_eq!(view::<Revenue>(&first, &()).placed, 47);
    assert_eq!(view::<Revenue>(&second, &()).placed, 47);
    assert_eq!(view::<Spent>(&first, &ord("ord_1")).amount, 40);
}

#[test]
fn a_view_saved_by_an_older_struct_is_rebuilt_from_the_log() {
    let store = Shared::default();
    let mut shop = Shop::new(store.clone());
    send(&mut shop, PlaceOrder { order: ord("ord_1"), total: 40 }).unwrap();
    send(&mut shop, PayOrder { order: ord("ord_1"), amount: 40 }).unwrap();

    // The saved checkpoint and rows now read as written by an older Revenue holding 999s.
    store.old_views.store(true, SeqCst);
    let revenue = view::<Revenue>(&shop, &());
    assert_eq!((revenue.placed, revenue.paid), (40, 40));
}

#[test]
fn an_event_loads_only_the_rows_it_updates() {
    let store = Shared::default();
    let mut shop = Shop::new(store.clone());
    for n in 0..50 {
        send(&mut shop, PlaceOrder { order: ord(&format!("ord_{n}")), total: 1 }).unwrap();
        send(&mut shop, PayOrder { order: ord(&format!("ord_{n}")), amount: 1 }).unwrap();
    }
    store.rows_loaded.store(0, SeqCst);
    send(&mut shop, PlaceOrder { order: ord("ord_x"), total: 3 }).unwrap();
    send(&mut shop, PayOrder { order: ord("ord_x"), amount: 3 }).unwrap();
    // Placing loads Revenue's and Timeline's one row; paying loads Revenue's and ord_x's Spent row.
    assert_eq!(store.rows_loaded.load(SeqCst), 4);
}

#[test]
fn a_view_catches_up_on_a_log_longer_than_one_batch() {
    let store = Shared::default();
    let mut writer = Shop::new(store.clone());
    let orders = oath::__private::LOG_BATCH + 300;
    for n in 0..orders {
        send(&mut writer, PlaceOrder { order: ord(&format!("ord_{n}")), total: 1 }).unwrap();
        if n % 3 == 0 {
            send(&mut writer, OpenAccount { customer: cus(&format!("cus_{n}")) }).unwrap();
        }
    }
    // Rebuilding the timeline reads the whole log, a batch at a time, merging both aggregates.
    store.old_views.store(true, SeqCst);
    let timeline = view::<Timeline>(&Shop::new(store), &());
    let expected: Vec<String> = (0..orders)
        .flat_map(|n| {
            let opened = (n % 3 == 0).then(|| "opened".to_string());
            std::iter::once("placed 1".to_string()).chain(opened)
        })
        .collect();
    assert_eq!(timeline.0, expected);
}

#[test]
fn a_view_save_from_a_stale_checkpoint_is_refused_and_taken_in_again() {
    let store = Shared::default();
    let mut shop = Shop::new(store.clone());
    send(&mut shop, PlaceOrder { order: ord("ord_1"), total: 40 }).unwrap();
    let stale = block_on(ViewStore::<Revenue>::checkpoint(&*store.store)).unwrap().unwrap();
    send(&mut shop, PlaceOrder { order: ord("ord_2"), total: 7 }).unwrap();

    // The next catch-up loads the checkpoint from before ord_2 was taken in, as a context that
    // loaded it just before another one saved. Its save is refused, and ord_2 counts once.
    *store.lagging_checkpoint.lock().unwrap() = Some(Box::new(stale));
    assert_eq!(view::<Revenue>(&shop, &()).placed, 47);
}
