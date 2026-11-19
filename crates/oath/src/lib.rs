//! Commands, events, and aggregates: the core of a domain-driven, event-sourced system.
//!
//! - An **id** identifies one aggregate. Declare it as a branded newtype with `#[oath::id]`,
//!   such as `#[oath::id(prefix = "ord_")] struct OrderId(String)`.
//! - A **value** is a domain value that keeps its rules. Declare it with `#[oath::value(...)]`
//!   around one inner value, such as `#[oath::value(range = 0..=100)] struct Percent(u8);`,
//!   or around named fields, such as `struct DiscountedPrice { price: Money, discount: Money }`.
//!   Rules are checked in three layers: the inner type parses the format (`IpAddr`, `u8`);
//!   `range`, `len`, `prefix`, and `chars` narrow it; and `check = function` covers the rest,
//!   including rules across named fields. `range` bounds any ordered type with constants of
//!   that type, such as `range = 0..=100` or `range = EARLIEST..` for a `const EARLIEST:
//!   NaiveDate`; a bound that isn't a constant, such as `Utc::now()`, doesn't compile, because
//!   a stored value must keep its rules on every replay.
//!   A value is built only by `new`, which returns
//!   [`Invalid`] naming the first rule it breaks, and reading it from the wire runs the same
//!   rules. Ids take the same rules. A float value is always finite: `new` refuses NaN and
//!   infinity, so float values have `Eq` and `Hash` like every other value.
//! - A **command** requests a business action. Declare it as a struct with `#[oath::command]`,
//!   and mark the field holding the id of the aggregate it targets `#[id]`.
//! - An **event** records something that happened. Declare it as a struct with `#[oath::event]`.
//! - An **aggregate** decides which commands are accepted and how events change its state.
//!   Every aggregate is a state machine: an enum of state names with `#[oath::aggregate]`.
//!
//! - A **rejection** says why an aggregate refused a command. Declare one enum of reasons per
//!   aggregate with `#[oath::rejection]`; it adds a `NotAllowed` variant for commands the
//!   current state does not accept.
//! - A **context** is a bounded context: the aggregates of one domain, and the one way the
//!   outside world reaches them. Declare it on a unit struct with `#[oath::context]`, listing
//!   its aggregates and views. See [`Context`].
//! - A **view** is a read model that the events of a context keep up to date. Declare it on a
//!   struct with `#[oath::view(on(...))]`, naming the events it subscribes to. See [`View`].
//!
//! `#[oath::command]`, `#[oath::event]`, and `#[oath::rejection]` derive `Debug`, `Clone`,
//! `PartialEq`, and serde's `Serialize` and `Deserialize`, so every command, event, and
//! rejection can go over the wire. Your crate needs no serde dependency of its own.
//!
//! In an aggregate's command or event enum, each value goes over the wire as its struct's
//! fields plus a `kind` field holding its name. The name is the struct's name unless you give
//! one: `#[oath::event(name = "OrderPlaced")]`. Stored events are read back by this name, so
//! before renaming an event struct, pin its old name this way.
//!
//! `#[oath::aggregate]` defines the enum of the aggregate's commands and the enum of its
//! events, named after the aggregate: `Order` defines `OrderCommand` and `OrderEvent`. Each holds every
//! command its states accept and every event in their transitions, as `PlaceOrder(PlaceOrder)`,
//! and has `From` for each struct. The aggregate's states are listed as bare names, and each
//! name expands the same way.
//!
//! Each state implements [`Handle`] for every command it accepts (the decide step) and
//! [`Transition`] for every event that moves it to another state (the evolve step). The
//! `#[state(...)]` attribute on each state is the transition table, and `#[oath::aggregate]`
//! turns it into an [`Aggregate`] implementation. A command a state does not accept is
//! rejected with [`NotAllowed`]. The `#[initial]` state means no event has happened yet, so
//! it holds no data: write it as `struct Draft;`. Mark every state struct `#[oath::state]`,
//! which lets a snapshot store it.
//!
//! The aggregate itself is opaque: `#[oath::aggregate]` turns the enum into a struct whose
//! state no code can build or change except [`Aggregate::evolve`], starting from the
//! `#[initial]` state. Read the current state's name with [`Aggregate::state`]. Declare
//! aggregates at module level, not inside a function body.
//!
//! ```
//! use oath::{Aggregate, Command, Decision, Handle, NotAllowed, Transition};
//!
//! #[oath::id(prefix = "ord_", len = 5..=40, chars = "a-z0-9_")] pub struct OrderId(String);
//!
//! #[oath::command] pub struct PlaceOrder { #[id] pub order: OrderId, pub total: u64 }
//! #[oath::command] pub struct PayOrder { #[id] pub order: OrderId }
//!
//! #[oath::event] pub struct OrderPlaced { pub total: u64 }
//! #[oath::event] pub struct OrderPaid { pub amount: u64 }
//!
//! #[oath::rejection] pub enum OrderRejection { WrongAmount { expected: u64 } }
//!
//! #[oath::state] pub struct Draft;
//! #[oath::state] pub struct Placed { total: u64 }
//! #[oath::state] pub struct Paid;
//!
//! #[oath::aggregate(id = OrderId, rejection = OrderRejection)]
//! pub enum Order {
//!     #[initial]
//!     #[state(accepts(PlaceOrder), on(OrderPlaced => Placed))]
//!     Draft,
//!     #[state(accepts(PayOrder), on(OrderPaid => Paid))]
//!     Placed,
//!     Paid,
//! }
//!
//! impl Handle<PlaceOrder> for Draft {
//!     fn decide(&self, c: &PlaceOrder) -> Decision<Self> {
//!         Ok(self.emit(OrderPlaced { total: c.total }).into())
//!     }
//! }
//! impl Transition<OrderPlaced> for Draft {
//!     fn evolve(self, e: &OrderPlaced) -> Placed { Placed { total: e.total } }
//! }
//! impl Handle<PayOrder> for Placed {
//!     fn decide(&self, _: &PayOrder) -> Decision<Self> {
//!         Ok(self.emit(OrderPaid { amount: self.total }).into())
//!     }
//! }
//! impl Transition<OrderPaid> for Placed {
//!     fn evolve(self, _: &OrderPaid) -> Paid { Paid }
//! }
//!
//! # fn main() {
//! let id = || OrderId::new("ord_1").unwrap();
//! let place: OrderCommand = PlaceOrder { order: id(), total: 40 }.into();
//! assert_eq!(place.target(), &id());
//! let events = Order::default().decide(&place).unwrap();
//! let order = Order::replay(&events).unwrap();
//! assert_eq!(order.state(), "Placed");
//! assert_eq!(
//!     order.decide(&place),
//!     Err(OrderRejection::NotAllowed(NotAllowed { state: "Placed".into(), command: "PlaceOrder".into() })),
//! );
//! let pay = PayOrder { order: id() }.into();
//! assert_eq!(order.decide(&pay), Ok(vec![OrderPaid { amount: 40 }.into()]));
//! # }
//! ```

pub use oath_macros::{aggregate, command, context, event, id, rejection, state, value, view};

/// A request to perform one business action, named in the imperative.
///
/// Implemented by `#[oath::command]` on each command struct, and by `#[oath::aggregate]` on
/// the enum of one aggregate's commands. A command targets exactly one aggregate: the field
/// marked `#[id]` holds that aggregate's id.
pub trait Command {
    /// The id type of the aggregate this command targets.
    type Id: Id;
    /// The command's stable name: the struct's name, or for an enum, its variant's.
    fn name(&self) -> &'static str;
    /// The id of the aggregate this command targets: the field marked `#[id]`.
    fn target(&self) -> &Self::Id;
}

/// A fact that happened, named in the past tense.
///
/// Implemented by `#[oath::event]` on each event struct, and by `#[oath::aggregate]` on the
/// enum of one aggregate's events. Events carry no id: they belong to their aggregate's history.
#[diagnostic::on_unimplemented(
    message = "`{Self}` is not an event",
    label = "not an event",
    note = "declare events with #[oath::event]; a view's #[oath::view(on(...))] lists events, not commands"
)]
pub trait Event {
    /// The event's stable name: the struct's name, or for an enum, its variant's.
    fn name(&self) -> &'static str;
}

/// Why an aggregate refused a command. A normal, typed outcome, not an error in the code.
///
/// Implemented by `#[oath::rejection]` on the enum of one aggregate's rejection reasons. The
/// attribute adds a `NotAllowed(NotAllowed)` variant for commands the current state does not
/// accept.
pub trait Rejection: From<NotAllowed> {
    /// The reason's stable name: its variant's name.
    fn name(&self) -> &'static str;
}

/// An aggregate's identity: a branded newtype around one value, so an `OrderId` is never
/// mistaken for a `CustomerId` or a plain string.
///
/// Implemented by `#[oath::id]`, which derives `Debug`, `Clone`, `PartialEq`, `Eq`, `Hash`, and
/// serde. On the wire an id is its inner value: `OrderId::new("ord_1")?` is `"ord_1"`.
pub trait Id:
    Clone
    + Eq
    + core::hash::Hash
    + core::fmt::Debug
    + Send
    + Sync
    + 'static
    + serde::Serialize
    + serde::de::DeserializeOwned
{
}

/// A consistency boundary: a state machine that decides which commands are accepted.
///
/// Implemented only by `#[oath::aggregate]`. Its `Default` is the `#[initial]` state, which
/// holds no data.
pub trait Aggregate: Default + Send + Sync + 'static + __private::Sealed + __private::Persist {
    /// What identifies one instance of this aggregate.
    type Id: Id;
    /// The enum of commands this aggregate decides. Each one targets an aggregate by its id.
    type Command: Command<Id = Self::Id> + Send + Sync + 'static;
    /// The enum of events this aggregate records.
    type Event: Event
        + Clone
        + Send
        + Sync
        + 'static
        + serde::Serialize
        + serde::de::DeserializeOwned;
    /// The enum of reasons this aggregate rejects a command for.
    type Rejection: Rejection + Send + 'static;

    /// Every `(from, event, to)` transition the aggregate declares.
    const TRANSITIONS: &'static [Edge];

    /// The current state's name.
    fn state(&self) -> &'static str;

    /// Pure: the current state and a command give the events to record, or a rejection.
    fn decide(&self, command: &Self::Command) -> Result<Vec<Self::Event>, Self::Rejection>;

    /// Pure: the state after `event`, or [`Unreplayable`] when the current state declares no
    /// transition for it, which means the history and the model disagree.
    fn evolve(self, event: &Self::Event) -> Result<Self, Unreplayable>;

    /// The state after every event in `events`, starting from the initial state.
    fn replay<'a>(events: impl IntoIterator<Item = &'a Self::Event>) -> Result<Self, Unreplayable>
    where
        Self::Event: 'a,
    {
        events.into_iter().try_fold(Self::default(), Self::evolve)
    }
}

/// A history the model can't apply: in state `state`, the aggregate declares no transition
/// for event `event`. It means the stored events and the code disagree.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Unreplayable {
    /// The state the aggregate was in.
    pub state: &'static str,
    /// The event it has no transition for.
    pub event: &'static str,
}

impl core::fmt::Display for Unreplayable {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "event {} has no transition from state {}; the history and the model disagree",
            self.event, self.state
        )
    }
}

impl std::error::Error for Unreplayable {}

/// A bounded context: the aggregates of one domain, and the one way the outside world reaches
/// them.
///
/// Implemented only by `#[oath::context]` on a unit struct:
///
/// ```text
/// #[oath::context(aggregates = [Order, Customer], views = [Revenue])]
/// pub struct Shop;
/// ```
///
/// It defines three enums named after the context, `ShopCommand`, `ShopEvent`, and
/// `ShopRejection`, with one variant per aggregate holding that aggregate's enum, such as
/// `ShopEvent::Order(OrderEvent)`. `views = [...]` may be left out.
///
/// The context keeps no aggregate instance: it holds a [`Store`] of their events. Build it with
/// `Shop::new(store)`, which takes one store that keeps the events of every aggregate the
/// context lists, such as a [`MemoryStore`]. Each aggregate belongs to exactly one context,
/// and a context is as visible as its aggregates.
///
/// A context only decides commands: its aggregates are opaque, and it has no way to read them.
/// Reads come from its [`View`]s, which `send` passes the events it records.
///
/// On the wire a context's command is its aggregate's command, unchanged, such as
/// `{"kind":"PlaceOrder","order":"ord_1"}`: its name says which aggregate decides it, so no two
/// aggregates in one context may have a command with the same name. Events and rejections go
/// under their aggregate's name, such as `{"Order":{"kind":"OrderPlaced","total":40}}`, since
/// every aggregate has a `NotAllowed` rejection.
pub trait Context: Send + Sync + __private::Sealed + __private::Log {
    /// The enum of every command the context's aggregates decide.
    type Command;
    /// The enum of every event the context's aggregates record.
    type Event;
    /// The enum of every reason the context's aggregates reject a command for.
    type Rejection;

    /// Routes `command` to the aggregate instance it targets and records what it decides.
    ///
    /// It loads the instance's snapshot and any events after it, lets the instance decide,
    /// and appends the events with a snapshot of the new state, only if no other writer has
    /// appended to the instance since it loaded. When another writer has, it loads and decides
    /// again, up to three times in all, before returning [`SendError::Conflict`]. A rejection
    /// appends nothing. On success it brings the context's views up to date and returns the
    /// events. The events are recorded even when the store fails while updating a view; the
    /// view catches up the next time it is read or updated.
    ///
    /// Takes a command struct, an aggregate's command enum, or the context's own command enum,
    /// such as one read from the wire. A command of an aggregate outside this context doesn't
    /// compile.
    fn send(
        &mut self,
        command: impl Into<Self::Command>,
    ) -> impl Future<Output = Result<Vec<Self::Event>, SendError<Self::Rejection>>> + Send;

    /// The row `key` of view `V`, after the view has taken in every event in the store's log.
    /// A row no event has touched yet is the view's `Default`. A view the context doesn't list
    /// in `views = [...]` doesn't compile.
    fn view<V: View>(&self, key: &V::Key) -> impl Future<Output = Result<V, StoreError>> + Send
    where
        Self: __private::Holds<V>,
    {
        async move {
            let views = __private::Holds::<V>::view_store(self);
            __private::catch_up::<V, _, _>(views, self).await?;
            let row = views.dyn_row(key).await?;
            Ok(row.and_then(|row| row.restore()).unwrap_or_default())
        }
    }
}

/// A failure of a store, such as a lost connection.
pub type StoreError = Box<dyn std::error::Error + Send + Sync>;

/// Why [`Context::send`] recorded nothing.
#[derive(Debug)]
pub enum SendError<R> {
    /// The aggregate refused the command: a normal, typed outcome.
    Rejected(R),
    /// Other writers appended to the instance each time it was decided, three times in all.
    Conflict,
    /// The store failed.
    Store(StoreError),
    /// The instance's stored history can't be applied by the current model.
    Unreplayable(Unreplayable),
}

impl<R: core::fmt::Debug> core::fmt::Display for SendError<R> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            SendError::Rejected(rejection) => write!(f, "the command was rejected: {rejection:?}"),
            SendError::Conflict => f.write_str("other writers kept appending to the instance"),
            SendError::Store(error) => write!(f, "the store failed: {error}"),
            SendError::Unreplayable(error) => error.fmt(f),
        }
    }
}

impl<R: core::fmt::Debug> std::error::Error for SendError<R> {}

/// A read model the events of a context keep up to date, as rows by key. Implemented only by
/// `#[oath::view]` on a struct with `Default`, which is one row:
///
/// ```text
/// #[oath::view(key = AccountId, on(AccountOpened, Deposited))]
/// #[derive(Default)]
/// pub struct Balance { pub amount: u64 }
/// ```
///
/// The view implements [`Project`] for each event in its `on(...)`: `key` names the row an
/// event updates, and `project` updates it. A view with one row, such as a total, has
/// `key = ()`. Every event a view lists must be recorded by an aggregate in the context that
/// lists the view in its `views = [...]`, which the compiler checks.
///
/// The rows live in the context's [`ViewStore`], never all in memory. The view takes in the
/// store's log in order, from the position it last saved: `send` brings it up to date after
/// recording events, and so does reading a row with [`Context::view`]. A view whose struct,
/// key, or events changed since its rows were saved is rebuilt from the start of the log.
pub trait View:
    Default
    + Send
    + Sync
    + 'static
    + serde::Serialize
    + serde::de::DeserializeOwned
    + __private::Sealed
    + __private::Persisted
{
    /// What names one row, such as the account an event is about.
    type Key: Clone
        + Eq
        + core::hash::Hash
        + core::fmt::Debug
        + Send
        + Sync
        + 'static
        + serde::Serialize
        + serde::de::DeserializeOwned;

    #[doc(hidden)]
    fn key_of(id: &dyn core::any::Any, event: &dyn core::any::Any) -> Option<Self::Key>;
    #[doc(hidden)]
    fn apply(&mut self, event: &dyn core::any::Any);
}

/// Marks that a view lists event `E` in its `on(...)`. Implemented only by `#[oath::view]`.
#[diagnostic::on_unimplemented(
    message = "view `{Self}` does not subscribe to event `{E}`",
    label = "`{Self}` does not list `{E}`",
    note = "add `{E}` to {Self}'s #[oath::view(on(...))], or remove this Project impl"
)]
pub trait Subscribes<E>: View {}

/// How a view takes in event `E`. Implement it for each event listed in the view's `on(...)`.
#[diagnostic::on_unimplemented(
    message = "view `{Self}` lists event `{E}` but does not implement `Project<{E}>`",
    label = "`{Self}` needs `impl Project<{E}>`",
    note = "implement Project<{E}> for {Self}, or remove `{E}` from its #[oath::view(on(...))]"
)]
pub trait Project<E: BelongsToAggregate>: Subscribes<E> {
    /// The row `event` updates, which the aggregate instance `id` recorded.
    fn key(id: &<E::Aggregate as Aggregate>::Id, event: &E) -> <Self as View>::Key;
    /// Updates the row with `event`.
    fn project(&mut self, event: &E);
}

/// The one context an aggregate belongs to. Implemented by `#[oath::context]` for every
/// aggregate it lists.
///
/// An aggregate belongs to exactly one context, so two contexts that list the same aggregate
/// don't compile: the second one is a conflicting implementation of this trait.
#[diagnostic::on_unimplemented(
    message = "aggregate `{Self}` is not in this context",
    label = "its aggregate `{Self}` is not in this context",
    note = "a context takes commands for, and its views subscribe to events of, only the aggregates \
            in its #[oath::context(aggregates = [...])]"
)]
pub trait BelongsToContext: Aggregate {
    /// The context that lists this aggregate.
    type Context: Context;
}

/// Where an event sits in a store's log: every event recorded for any aggregate gets the next
/// position. A log with no events is at `Position(0)`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
pub struct Position(pub u64);

/// How many events an aggregate instance has recorded. An instance with no events is at
/// `Version(0)`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
pub struct Version(pub u64);

/// Where a context keeps the events of aggregate `A`, and a snapshot of each instance's
/// current state, by instance id. Implement it for a database; [`MemoryStore`] keeps them in
/// memory.
///
/// The events are the record of an instance; the snapshot saves replaying them. A context
/// writes a snapshot with every append, so loading an instance reads its snapshot and, at
/// most, the few events after it.
///
/// The store also keeps one log of every event it records, for every aggregate: each event
/// gets the next [`Position`], and an event becomes readable only after every event before it.
/// Views take in events through it, in the order they happened.
pub trait Store<A: Aggregate>: Send + Sync + 'static {
    /// What a failure of the store is, such as a lost connection.
    type Error: std::error::Error + Send + Sync + 'static;

    /// The latest snapshot of instance `id`, and every event recorded after it, oldest first.
    /// With no snapshot, every event of the instance; with no events either, nothing.
    fn load(&self, id: &A::Id) -> impl Future<Output = Result<Loaded<A>, Self::Error>> + Send;

    /// Every event of instance `id` after its first `after` events, oldest first. A context
    /// calls it when a snapshot was written by code whose states have since changed.
    fn events(
        &self,
        id: &A::Id,
        after: Version,
    ) -> impl Future<Output = Result<Vec<A::Event>, Self::Error>> + Send;

    /// In one transaction, records the events of `decided` after the ones already recorded for
    /// its instance and replaces the instance's snapshot with its snapshot, but only when the
    /// instance is still at version [`Decided::expected`], which is how many events it had when
    /// it was loaded. Otherwise records nothing and returns [`AppendError::Conflict`].
    ///
    /// Only a context builds a [`Decided`], from what an aggregate decided, so no code can
    /// record events that skipped `decide`.
    ///
    /// Each event recorded gets the next position in the store's log, shared by every aggregate
    /// in the store.
    fn append(&self, decided: &Decided<A>) -> impl Future<Output = Result<(), AppendError<Self::Error>>> + Send;

    /// The position of the latest event in the store's log, of any aggregate; `Position(0)`
    /// when it has none.
    fn head(&self) -> impl Future<Output = Result<Position, Self::Error>> + Send;

    /// The first `limit` events of aggregate `A` in the store's log after position `after`,
    /// in order.
    fn log(&self, after: Position, limit: usize) -> impl Future<Output = Result<Vec<Logged<A>>, Self::Error>> + Send;
}

/// One event of aggregate `A` in a store's log.
pub struct Logged<A: Aggregate> {
    /// The event's position in the log.
    pub position: Position,
    /// The instance that recorded it.
    pub id: A::Id,
    /// The event.
    pub event: A::Event,
}

/// Where a context keeps the rows of view `V`, by key, and the log position the rows include.
/// Implement it for a database; [`MemoryStore`] keeps them in memory.
pub trait ViewStore<V: View>: Send + Sync + 'static {
    /// What a failure of the store is, such as a lost connection.
    type Error: std::error::Error + Send + Sync + 'static;

    /// The view's latest checkpoint, if it has saved one.
    fn checkpoint(&self) -> impl Future<Output = Result<Option<Checkpoint<V>>, Self::Error>> + Send;

    /// The row `key` of the view, if it has saved one.
    fn row(&self, key: &V::Key) -> impl Future<Output = Result<Option<Row<V>>, Self::Error>> + Send;

    /// In one transaction, saves the rows of `projected` and replaces the view's checkpoint with
    /// its checkpoint, but only when the saved checkpoint is still at position
    /// [`Projected::expected`] (`Position(0)` for none). Otherwise saves nothing and returns
    /// [`AppendError::Conflict`]. Only a context builds a [`Projected`].
    fn save(&self, projected: &Projected<V>) -> impl Future<Output = Result<(), AppendError<Self::Error>>> + Send;
}

/// How far view `V` has taken in the store's log. A store keeps it through serde.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(bound = "")]
pub struct Checkpoint<V> {
    schema: u64,
    position: Position,
    #[serde(skip)]
    view: core::marker::PhantomData<fn() -> V>,
}

impl<V> Checkpoint<V> {
    /// The position of the last event in the log the view's rows include.
    pub fn position(&self) -> Position {
        self.position
    }
}

/// One row of view `V`. A store keeps it through serde.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(bound = "")]
pub struct Row<V> {
    schema: u64,
    state: serde_value::Value,
    #[serde(skip)]
    view: core::marker::PhantomData<fn() -> V>,
}

impl<V: View> Row<V> {
    fn restore(self) -> Option<V> {
        (self.schema == V::FINGERPRINT).then(|| self.state.deserialize_into().ok()).flatten()
    }
}

/// The rows a view updated from the log, and how far it got, for [`ViewStore::save`]. Only a
/// context builds one; a store reads it.
pub struct Projected<V: View> {
    expected: Position,
    checkpoint: Checkpoint<V>,
    rows: Vec<(V::Key, Row<V>)>,
}

impl<V: View> Projected<V> {
    /// The position of the checkpoint the view loaded; `Position(0)` for none.
    pub fn expected(&self) -> Position {
        self.expected
    }
    /// The view's new checkpoint.
    pub fn checkpoint(&self) -> &Checkpoint<V> {
        &self.checkpoint
    }
    /// The rows that changed, by key.
    pub fn rows(&self) -> &[(V::Key, Row<V>)] {
        &self.rows
    }
}

/// What an aggregate instance decided, for [`Store::append`] to record: its new events and a
/// snapshot of the state after them. Only a context builds one, in `send`; a store reads it.
pub struct Decided<A: Aggregate> {
    id: A::Id,
    expected: Version,
    events: Vec<A::Event>,
    snapshot: Snapshot<A>,
}

impl<A: Aggregate> Decided<A> {
    /// The instance that decided.
    pub fn id(&self) -> &A::Id {
        &self.id
    }
    /// How many events the instance had when it was loaded.
    pub fn expected(&self) -> Version {
        self.expected
    }
    /// The events to record, oldest first.
    pub fn events(&self) -> &[A::Event] {
        &self.events
    }
    /// The instance's state after the events, which replaces its snapshot.
    pub fn snapshot(&self) -> &Snapshot<A> {
        &self.snapshot
    }
}

/// What [`Store::load`] returns for one instance.
pub struct Loaded<A: Aggregate> {
    /// The instance's latest snapshot, if it has one.
    pub snapshot: Option<Snapshot<A>>,
    /// The events recorded after the snapshot, or every event when there is no snapshot.
    pub events: Vec<A::Event>,
}

/// Why [`Store::append`] recorded nothing.
#[derive(Debug)]
pub enum AppendError<E> {
    /// Another writer appended to the instance after it was loaded.
    Conflict,
    /// The store failed.
    Store(E),
}

/// The state of one aggregate instance after its first [`version`](Snapshot::version) events.
///
/// Only a context builds a snapshot. A store keeps it through serde, as a struct of
/// `schema`, `version`, and `state`. `schema` identifies the aggregate's state structs, as
/// declared with `#[oath::state]` when the snapshot was written; when they have changed
/// since, the context ignores the snapshot and replays the instance's events instead.
pub struct Snapshot<A> {
    schema: u64,
    version: Version,
    state: serde_value::Value,
    aggregate: core::marker::PhantomData<fn() -> A>,
}

impl<A> Snapshot<A> {
    /// How many events of the instance the snapshot includes.
    pub fn version(&self) -> Version {
        self.version
    }
}

impl<A> Clone for Snapshot<A> {
    fn clone(&self) -> Self {
        Snapshot { schema: self.schema, version: self.version, state: self.state.clone(), aggregate: self.aggregate }
    }
}

impl<A> core::fmt::Debug for Snapshot<A> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Snapshot").field("schema", &self.schema).field("version", &self.version).finish()
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename = "Snapshot")]
struct SnapshotFields {
    schema: u64,
    version: Version,
    state: serde_value::Value,
}

impl<A> serde::Serialize for Snapshot<A> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        SnapshotFields { schema: self.schema, version: self.version, state: self.state.clone() }.serialize(serializer)
    }
}

impl<'de, A> serde::Deserialize<'de> for Snapshot<A> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let SnapshotFields { schema, version, state } = SnapshotFields::deserialize(deserializer)?;
        Ok(Snapshot { schema, version, state, aggregate: core::marker::PhantomData })
    }
}

/// A [`Store`] and [`ViewStore`] that keeps everything in memory, for tests and prototypes. It
/// behaves as a database store does: it writes events, snapshots, and rows through serde and
/// reads them back, keeps one ordered log, and refuses an append or a view save whose expected
/// version or position is stale. Its `Default` holds nothing.
#[derive(Default)]
pub struct MemoryStore {
    memory: std::sync::Mutex<Memory>,
}

#[derive(Default)]
struct Memory {
    head: u64,
    /// One table per aggregate (`Instances<A>`) and per view (`Rows<V>`), keyed by its type.
    tables: std::collections::HashMap<core::any::TypeId, Box<dyn core::any::Any + Send + Sync>>,
}

/// The instances of one aggregate in a [`MemoryStore`], and its part of the log.
struct Instances<A: Aggregate> {
    by_id: std::collections::HashMap<A::Id, Stored>,
    log: Vec<(Position, serde_value::Value, serde_value::Value)>,
}

impl<A: Aggregate> Default for Instances<A> {
    fn default() -> Self {
        Instances { by_id: Default::default(), log: Vec::new() }
    }
}

/// One instance in a [`MemoryStore`], as it would be written to a database.
#[derive(Default)]
struct Stored {
    events: Vec<serde_value::Value>,
    snapshot: Option<serde_value::Value>,
}

/// The rows of one view in a [`MemoryStore`].
struct Rows<V: View> {
    checkpoint: Option<serde_value::Value>,
    by_key: std::collections::HashMap<V::Key, serde_value::Value>,
}

impl<V: View> Default for Rows<V> {
    fn default() -> Self {
        Rows { checkpoint: None, by_key: Default::default() }
    }
}

impl MemoryStore {
    fn with<T: Default + Send + Sync + 'static, R>(&self, f: impl FnOnce(&mut T, &mut u64) -> R) -> R {
        let mut memory = self.memory.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let Memory { head, tables } = &mut *memory;
        let table = tables
            .entry(core::any::TypeId::of::<T>())
            .or_insert_with(|| Box::new(T::default()))
            .downcast_mut::<T>()
            .expect("each table is keyed by its type");
        f(table, head)
    }
}

/// Reads back a value a [`MemoryStore`] wrote.
fn read_back<T: serde::de::DeserializeOwned>(value: &serde_value::Value) -> T {
    value.clone().deserialize_into().expect("MemoryStore reads back what it wrote")
}

fn write<T: serde::Serialize>(value: &T) -> serde_value::Value {
    serde_value::to_value(value).expect("MemoryStore writes every value it keeps")
}

impl<A: Aggregate> Store<A> for MemoryStore {
    type Error = core::convert::Infallible;

    fn load(&self, id: &A::Id) -> impl Future<Output = Result<Loaded<A>, Self::Error>> + Send {
        let loaded = self.with(|instances: &mut Instances<A>, _| {
            let Some(stored) = instances.by_id.get(id) else {
                return Loaded { snapshot: None, events: Vec::new() };
            };
            let snapshot: Option<Snapshot<A>> = stored.snapshot.as_ref().map(read_back);
            let after = snapshot.as_ref().map_or(0, |snapshot| snapshot.version.0 as usize);
            Loaded { events: stored.events[after..].iter().map(read_back).collect(), snapshot }
        });
        core::future::ready(Ok(loaded))
    }

    fn events(&self, id: &A::Id, after: Version) -> impl Future<Output = Result<Vec<A::Event>, Self::Error>> + Send {
        let events = self.with(|instances: &mut Instances<A>, _| {
            let events = instances.by_id.get(id).map_or(&[][..], |stored| &stored.events[..]);
            events.iter().skip(after.0 as usize).map(read_back).collect()
        });
        core::future::ready(Ok(events))
    }

    fn append(&self, decided: &Decided<A>) -> impl Future<Output = Result<(), AppendError<Self::Error>>> + Send {
        let appended = self.with(|instances: &mut Instances<A>, head| {
            let stored = instances.by_id.entry(decided.id().clone()).or_default();
            if stored.events.len() as u64 != decided.expected().0 {
                return Err(AppendError::Conflict);
            }
            for event in decided.events() {
                *head += 1;
                stored.events.push(write(event));
                instances.log.push((Position(*head), write(decided.id()), write(event)));
            }
            stored.snapshot = Some(write(decided.snapshot()));
            Ok(())
        });
        core::future::ready(appended)
    }

    fn head(&self) -> impl Future<Output = Result<Position, Self::Error>> + Send {
        core::future::ready(Ok(self.with(|_: &mut Instances<A>, head| Position(*head))))
    }

    fn log(&self, after: Position, limit: usize) -> impl Future<Output = Result<Vec<Logged<A>>, Self::Error>> + Send {
        let logged = self.with(|instances: &mut Instances<A>, _| {
            let start = instances.log.partition_point(|(position, ..)| *position <= after);
            instances.log[start..]
                .iter()
                .take(limit)
                .map(|(position, id, event)| Logged { position: *position, id: read_back(id), event: read_back(event) })
                .collect()
        });
        core::future::ready(Ok(logged))
    }
}

impl<V: View> ViewStore<V> for MemoryStore {
    type Error = core::convert::Infallible;

    fn checkpoint(&self) -> impl Future<Output = Result<Option<Checkpoint<V>>, Self::Error>> + Send {
        core::future::ready(Ok(self.with(|rows: &mut Rows<V>, _| rows.checkpoint.as_ref().map(read_back))))
    }

    fn row(&self, key: &V::Key) -> impl Future<Output = Result<Option<Row<V>>, Self::Error>> + Send {
        core::future::ready(Ok(self.with(|rows: &mut Rows<V>, _| rows.by_key.get(key).map(read_back))))
    }

    fn save(&self, projected: &Projected<V>) -> impl Future<Output = Result<(), AppendError<Self::Error>>> + Send {
        let saved = self.with(|rows: &mut Rows<V>, _| {
            let saved = rows.checkpoint.as_ref().map(read_back::<Checkpoint<V>>);
            if saved.map_or(Position(0), |checkpoint| checkpoint.position) != projected.expected() {
                return Err(AppendError::Conflict);
            }
            rows.checkpoint = Some(write(projected.checkpoint()));
            for (key, row) in projected.rows() {
                rows.by_key.insert(key.clone(), write(row));
            }
            Ok(())
        });
        core::future::ready(saved)
    }
}

/// The one aggregate a command or event struct belongs to. Implemented by `#[oath::aggregate]`
/// for every command and event in its transition table.
///
/// Each command and event belongs to exactly one aggregate's history, so two aggregates that
/// list the same struct don't compile: the second one is a conflicting implementation of this
/// trait. For another aggregate to react to an event, give it its own command and event.
/// A context routes each command to its aggregate through this trait.
#[diagnostic::on_unimplemented(
    message = "no aggregate lists `{Self}`",
    label = "no aggregate's #[state(...)] lists `{Self}`",
    note = "list it in an aggregate's #[state(accepts(...))] or #[state(on(...))]"
)]
pub trait BelongsToAggregate {
    /// The aggregate whose table lists this struct.
    type Aggregate: Aggregate;
}

/// One state of an aggregate. Implemented by `#[oath::aggregate]` for each state struct.
pub trait State: Sized {
    /// The aggregate this state belongs to.
    type Aggregate: Aggregate;
}

/// The event type of the aggregate that state `S` belongs to.
pub type EventOf<S> = <<S as State>::Aggregate as Aggregate>::Event;
/// The rejection type of the aggregate that state `S` belongs to.
pub type RejectionOf<S> = <<S as State>::Aggregate as Aggregate>::Rejection;
/// What a state's [`Handle::decide`] returns: the events to record, or a rejection.
pub type Decision<S> = Result<Events<S>, RejectionOf<S>>;

/// Marks that a state lists command `C` in its `accepts(...)`. Implemented only by
/// `#[oath::aggregate]`, from the transition table.
#[diagnostic::on_unimplemented(
    message = "state `{Self}` does not accept command `{C}`",
    label = "`{Self}` does not list `{C}`",
    note = "add `{C}` to {Self}'s #[state(accepts(...))], or remove this Handle impl"
)]
pub trait Accepts<C>: State {}

/// Marks that a state lists `E => To` in its `on(...)`. Implemented only by
/// `#[oath::aggregate]`, from the transition table.
#[diagnostic::on_unimplemented(
    message = "state `{Self}` has no transition for event `{E}`",
    label = "`{Self}` cannot record `{E}`",
    note = "add `{E} => ...` to {Self}'s #[state(on(...))], or record `{E}` after an event that \
            moves to a state that has it"
)]
pub trait On<E>: State {
    /// The state `E` moves to: the state named after `=>`.
    type To: State<Aggregate = Self::Aggregate>;
}

/// How a state decides command `C`. Implement it for each command listed in the state's
/// `accepts(...)`.
///
/// Build the events with the state's `emit` method and [`Emitted::then`]. Each event must
/// have a transition from the state the previous events moved to, which the compiler checks,
/// so `decide` can never record an event the aggregate cannot apply. At least one event is
/// required; to refuse a command, return a rejection.
pub trait Handle<C>: Accepts<C> {
    /// Pure: the events to record, or a rejection.
    fn decide(&self, command: &C) -> Decision<Self>;
}

/// How event `E` moves a state to the next one. Implement it for each `E => To` listed in the
/// state's `on(...)`; the table fixes the next state.
pub trait Transition<E>: On<E> {
    /// Pure and infallible: the next state.
    fn evolve(self, event: &E) -> Self::To;
}

/// Events emitted while deciding a command in state `S`, which have moved the aggregate to
/// state `At`. Start one with the state's `emit` method; add more with [`then`](Self::then);
/// finish with `.into()`.
#[must_use = "return it from `decide` with `Ok(...into())`"]
pub struct Emitted<S: State, At> {
    events: Vec<EventOf<S>>,
    states: core::marker::PhantomData<fn() -> (S, At)>,
}

impl<S: State, At: State> Emitted<S, At> {
    /// Records `event` after the previous ones. Compiles only when state `At` has a
    /// transition for `E`.
    pub fn then<E>(mut self, event: E) -> Emitted<S, <At as On<E>>::To>
    where
        At: On<E>,
        E: Into<EventOf<S>>,
    {
        self.events.push(event.into());
        Emitted { events: self.events, states: core::marker::PhantomData }
    }
}

/// The events a state decided to record: at least one, each valid after the ones before it.
/// Only [`Emitted`] produces it.
pub struct Events<S: State> {
    events: Vec<EventOf<S>>,
}

impl<S: State, At: State> From<Emitted<S, At>> for Events<S> {
    fn from(emitted: Emitted<S, At>) -> Self {
        Events { events: emitted.events }
    }
}

impl<S: State> Events<S> {
    /// The events, in the order they were emitted.
    pub fn into_vec(self) -> Vec<EventOf<S>> {
        self.events
    }
}

/// The rejection for a command the current state does not accept.
///
/// Its fields are owned so a rejection received over the wire can be deserialized.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct NotAllowed {
    /// The state the aggregate was in.
    pub state: String,
    /// The command that was rejected.
    pub command: String,
}

/// Why a value was refused: its type, and the first rule it breaks, as written.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Invalid {
    /// The value's type, such as `"OrderId"`.
    pub type_name: &'static str,
    /// The rule it breaks, such as `prefix = "ord_"`.
    pub rule: &'static str,
}

impl core::fmt::Display for Invalid {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{} breaks the rule `{}`", self.type_name, self.rule)
    }
}

impl std::error::Error for Invalid {}

/// One declared transition: in state `from`, event `event` moves the aggregate to `to`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Edge {
    pub from: &'static str,
    pub event: &'static str,
    pub to: &'static str,
}

#[doc(hidden)]
pub mod __private {
    /// Keeps [`Aggregate`](crate::Aggregate) implemented only by `#[oath::aggregate]`.
    pub trait Sealed {}

    /// The serde the attribute macros derive with, so user crates need no serde dependency.
    pub use serde;
    /// The serde-value the generated snapshot code writes states with.
    pub use serde_value;

    use crate::{Emitted, EventOf, On};

    /// A value `len`, `prefix`, and `chars` can check.
    #[diagnostic::on_unimplemented(
        message = "`{Self}` is not a String: `len`, `prefix`, and `chars` only apply to String values",
        label = "this rule needs a String inside the value"
    )]
    pub trait Text {
        fn text(&self) -> &str;
    }
    impl Text for String {
        fn text(&self) -> &str {
            self
        }
    }
    pub fn text<T: Text>(value: &T) -> &str {
        value.text()
    }

    /// A value `range` can check: any ordered type.
    #[diagnostic::on_unimplemented(
        message = "`{Self}` has no order, so `range` can't bound it",
        label = "this rule needs an ordered value inside",
        note = "for a String's length, use `len`"
    )]
    pub trait Ordered: PartialOrd {}
    impl<T: PartialOrd + ?Sized> Ordered for T {}

    pub fn in_range<T: Ordered>(value: &T, lowest: Option<&T>, highest: Option<&T>) -> bool {
        lowest.is_none_or(|lowest| value >= lowest) && highest.is_none_or(|highest| value <= highest)
    }
    pub fn len_in(len: usize, lowest: Option<usize>, highest: Option<usize>) -> bool {
        in_range(&len, lowest.as_ref(), highest.as_ref())
    }
    use serde::{Deserialize, Serialize};

    /// The stable name of a command or event struct: its wire name and `name()`.
    pub trait Named {
        const NAME: &'static str;
    }

    #[derive(Serialize)]
    struct Tagged<'a, T> {
        kind: &'static str,
        #[serde(flatten)]
        fields: &'a T,
    }

    /// Writes `value` as a struct tagged with `kind`, which comes first.
    pub fn serialize_tagged<T: Serialize, S: serde::Serializer>(
        kind: &'static str,
        value: &T,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        Tagged { kind, fields: value }.serialize(serializer)
    }

    /// Reads a tagged struct: its `kind`, and its other fields to decode with [`untagged`].
    pub fn read_tagged<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<(String, serde_value::Value), D::Error> {
        use serde::de::Error;
        use serde_value::Value;
        let Value::Map(mut fields) = Value::deserialize(deserializer)? else {
            return Err(D::Error::custom("expected a struct tagged with `kind`"));
        };
        match fields.remove(&Value::String("kind".into())) {
            Some(Value::String(kind)) => Ok((kind, Value::Map(fields))),
            Some(_) => Err(D::Error::custom("`kind` must be a string")),
            None => Err(D::Error::missing_field("kind")),
        }
    }

    /// Decodes the fields [`read_tagged`] returned as `T`.
    pub fn untagged<T: serde::de::DeserializeOwned, E: serde::de::Error>(
        fields: serde_value::Value,
    ) -> Result<T, E> {
        let is_empty = matches!(&fields, serde_value::Value::Map(map) if map.is_empty());
        match fields.deserialize_into() {
            Ok(value) => Ok(value),
            // A unit struct, such as `struct OrderCancelled;`, reads from no fields.
            Err(_) if is_empty => serde_value::Value::Unit.deserialize_into().map_err(E::custom),
            Err(error) => Err(E::custom(error)),
        }
    }

    /// Whether two names are equal, in a const context.
    pub const fn same_name(a: &str, b: &str) -> bool {
        let (a, b) = (a.as_bytes(), b.as_bytes());
        if a.len() != b.len() {
            return false;
        }
        let mut i = 0;
        while i < a.len() {
            if a[i] != b[i] {
                return false;
            }
            i += 1;
        }
        true
    }

    /// The stable names of the commands in one aggregate's command enum.
    pub trait Names {
        const NAMES: &'static [&'static str];
    }

    /// Whether two lists of names share none.
    pub const fn disjoint(a: &[&str], b: &[&str]) -> bool {
        let mut i = 0;
        while i < a.len() {
            let mut j = 0;
            while j < b.len() {
                if same_name(a[i], b[j]) {
                    return false;
                }
                j += 1;
            }
            i += 1;
        }
        true
    }

    /// The views of a context: one impl per view in its `views = [...]`.
    #[diagnostic::on_unimplemented(
        message = "`{V}` is not a view of `{Self}`",
        label = "`{Self}` does not list `{V}`",
        note = "add `{V}` to the `views = [...]` of {Self}'s #[oath::context]"
    )]
    pub trait Holds<V: crate::View> {
        fn view_store(&self) -> &dyn DynViewStore<V>;
    }

    /// How many events of each aggregate a context reads from the log at a time.
    pub const LOG_BATCH: usize = 1000;

    /// The log of a context: every event its aggregates recorded, in order. Implemented by
    /// `#[oath::context]`.
    pub trait Log {
        /// The next events after `after`, in order, and the position up to which they are
        /// every event of the context's aggregates in the log.
        fn log<'a>(&'a self, after: crate::Position) -> BoxFuture<'a, Result<(Vec<Entry>, crate::Position), BoxError>>;
    }

    /// One event in a context's log.
    pub struct Entry {
        pub position: crate::Position,
        id: Box<dyn core::any::Any + Send + Sync>,
        event: Box<dyn core::any::Any + Send + Sync>,
    }

    impl Entry {
        pub fn of<A: crate::Aggregate>(logged: crate::Logged<A>) -> Entry
        where
            A::Event: Inner,
        {
            Entry { position: logged.position, id: Box::new(logged.id), event: logged.event.into_any() }
        }
    }

    /// Brings view `V` up to date with the log: takes in the events after its checkpoint, a
    /// batch at a time, saving the rows they touch and the new checkpoint together. When
    /// another context saved first, starts again from the checkpoint it saved.
    pub async fn catch_up<V, S, L>(views: &S, log: &L) -> Result<(), BoxError>
    where
        V: crate::View,
        S: DynViewStore<V> + ?Sized,
        L: Log + Sync + ?Sized,
    {
        loop {
            let saved = views.dyn_checkpoint().await?;
            let expected = saved.as_ref().map_or(crate::Position(0), |checkpoint| checkpoint.position);
            let current = saved.as_ref().is_some_and(|checkpoint| checkpoint.schema == V::FINGERPRINT);
            // A view whose struct, key, or events changed is rebuilt from the start of the log.
            let after = if current { expected } else { crate::Position(0) };
            let (entries, upto) = log.log(after).await?;
            if current && upto <= after {
                return Ok(());
            }
            let mut rows = std::collections::HashMap::<V::Key, V>::new();
            for entry in &entries {
                let Some(key) = V::key_of(&*entry.id, &*entry.event) else { continue };
                if !rows.contains_key(&key) {
                    let row = views.dyn_row(&key).await?.and_then(crate::Row::restore).unwrap_or_default();
                    rows.insert(key.clone(), row);
                }
                rows.get_mut(&key).expect("loaded above").apply(&*entry.event);
            }
            let projected = crate::Projected {
                expected,
                checkpoint: crate::Checkpoint {
                    schema: V::FINGERPRINT,
                    position: upto.max(after),
                    view: core::marker::PhantomData,
                },
                rows: rows
                    .into_iter()
                    .map(|(key, row)| {
                        let state = serde_value::to_value(&row).expect("a view row is written through serde");
                        (key, crate::Row { schema: V::FINGERPRINT, state, view: core::marker::PhantomData })
                    })
                    .collect(),
            };
            match views.dyn_save(&projected).await {
                Ok(()) | Err(crate::AppendError::Conflict) => continue,
                Err(crate::AppendError::Store(error)) => return Err(error),
            }
        }
    }

    /// An event a view can subscribe to in context `C`: one an aggregate in `C` records.
    #[diagnostic::on_unimplemented(
        message = "no aggregate in `{C}` records event `{Self}`",
        label = "`{Self}` is not an event of `{C}`",
        note = "a view subscribes only to events in the #[state(on(...))] of an aggregate its context lists"
    )]
    pub trait EventIn<C> {}
    impl<E, C> EventIn<C> for E
    where
        E: crate::Event + crate::BelongsToAggregate,
        E::Aggregate: crate::BelongsToContext<Context = C>,
    {
    }

    /// A view whose every event is one an aggregate in context `C` records.
    #[diagnostic::on_unimplemented(
        message = "view `{Self}` subscribes to an event no aggregate in `{C}` records",
        label = "`{Self}` listens for an event outside `{C}`"
    )]
    pub trait RecordedIn<C> {}

    /// The event struct inside one variant of an aggregate's event enum.
    pub trait Inner {
        fn into_any(self) -> Box<dyn core::any::Any + Send + Sync>;
    }

    /// What a context needs of a [`Store`](crate::Store): the same methods, as boxed futures
    /// with boxed errors, so a context holds any store behind one trait object.
    pub trait DynStore<A: crate::Aggregate>: Send + Sync {
        fn dyn_load<'a>(&'a self, id: &'a A::Id) -> BoxFuture<'a, Result<crate::Loaded<A>, BoxError>>;
        fn dyn_events<'a>(&'a self, id: &'a A::Id) -> BoxFuture<'a, Result<Vec<A::Event>, BoxError>>;
        fn dyn_append<'a>(
            &'a self,
            decided: &'a crate::Decided<A>,
        ) -> BoxFuture<'a, Result<(), crate::AppendError<BoxError>>>;
        fn dyn_head(&self) -> BoxFuture<'_, Result<crate::Position, BoxError>>;
        fn dyn_log(&self, after: crate::Position, limit: usize) -> BoxFuture<'_, Result<Vec<crate::Logged<A>>, BoxError>>;
    }

    /// What a context needs of a [`ViewStore`](crate::ViewStore), as with [`DynStore`].
    pub trait DynViewStore<V: crate::View>: Send + Sync {
        fn dyn_checkpoint(&self) -> BoxFuture<'_, Result<Option<crate::Checkpoint<V>>, BoxError>>;
        fn dyn_row<'a>(&'a self, key: &'a V::Key) -> BoxFuture<'a, Result<Option<crate::Row<V>>, BoxError>>;
        fn dyn_save<'a>(
            &'a self,
            projected: &'a crate::Projected<V>,
        ) -> BoxFuture<'a, Result<(), crate::AppendError<BoxError>>>;
    }

    fn boxed<E: std::error::Error + Send + Sync + 'static>(error: E) -> BoxError {
        Box::new(error)
    }

    fn boxed_append<E: std::error::Error + Send + Sync + 'static>(
        error: crate::AppendError<E>,
    ) -> crate::AppendError<BoxError> {
        match error {
            crate::AppendError::Conflict => crate::AppendError::Conflict,
            crate::AppendError::Store(error) => crate::AppendError::Store(boxed(error)),
        }
    }

    impl<V: crate::View, S: crate::ViewStore<V>> DynViewStore<V> for S {
        fn dyn_checkpoint(&self) -> BoxFuture<'_, Result<Option<crate::Checkpoint<V>>, BoxError>> {
            Box::pin(async move { crate::ViewStore::checkpoint(self).await.map_err(boxed) })
        }
        fn dyn_row<'a>(&'a self, key: &'a V::Key) -> BoxFuture<'a, Result<Option<crate::Row<V>>, BoxError>> {
            Box::pin(async move { crate::ViewStore::row(self, key).await.map_err(boxed) })
        }
        fn dyn_save<'a>(
            &'a self,
            projected: &'a crate::Projected<V>,
        ) -> BoxFuture<'a, Result<(), crate::AppendError<BoxError>>> {
            Box::pin(async move { crate::ViewStore::save(self, projected).await.map_err(boxed_append) })
        }
    }

    pub type BoxError = Box<dyn std::error::Error + Send + Sync>;
    pub type BoxFuture<'a, T> = core::pin::Pin<Box<dyn core::future::Future<Output = T> + Send + 'a>>;

    impl<A: crate::Aggregate, S: crate::Store<A>> DynStore<A> for S {
        fn dyn_load<'a>(&'a self, id: &'a A::Id) -> BoxFuture<'a, Result<crate::Loaded<A>, BoxError>> {
            Box::pin(async move { crate::Store::load(self, id).await.map_err(|error| Box::new(error) as BoxError) })
        }
        fn dyn_events<'a>(&'a self, id: &'a A::Id) -> BoxFuture<'a, Result<Vec<A::Event>, BoxError>> {
            Box::pin(async move {
                crate::Store::events(self, id, crate::Version(0)).await.map_err(|error| Box::new(error) as BoxError)
            })
        }
        fn dyn_append<'a>(
            &'a self,
            decided: &'a crate::Decided<A>,
        ) -> BoxFuture<'a, Result<(), crate::AppendError<BoxError>>> {
            Box::pin(async move { crate::Store::append(self, decided).await.map_err(boxed_append) })
        }
        fn dyn_head(&self) -> BoxFuture<'_, Result<crate::Position, BoxError>> {
            Box::pin(async move { crate::Store::head(self).await.map_err(boxed) })
        }
        fn dyn_log(&self, after: crate::Position, limit: usize) -> BoxFuture<'_, Result<Vec<crate::Logged<A>>, BoxError>> {
            Box::pin(async move { crate::Store::log(self, after, limit).await.map_err(boxed) })
        }
    }

    /// How an aggregate's state goes into a snapshot. Implemented by `#[oath::aggregate]`.
    pub trait Persist: Sized {
        /// Identifies the aggregate's state structs, from their `#[oath::state]` fingerprints.
        const SCHEMA: u64;
        fn save(&self) -> serde_value::Value;
        fn restore(state: serde_value::Value) -> Option<Self>;
    }

    /// A state struct marked `#[oath::state]`, or a view: serde, and a fingerprint of its
    /// declaration.
    #[diagnostic::on_unimplemented(
        message = "state `{Self}` is not marked #[oath::state]",
        label = "`{Self}` needs #[oath::state]",
        note = "a snapshot stores the current state, so write #[oath::state] on `struct {Self}`"
    )]
    pub trait Persisted {
        const FINGERPRINT: u64;
    }

    /// FNV-1a over `bytes`, continuing from `hash`.
    pub const fn fnv(mut hash: u64, bytes: &[u8]) -> u64 {
        let mut i = 0;
        while i < bytes.len() {
            hash ^= bytes[i] as u64;
            hash = hash.wrapping_mul(0x100000001b3);
            i += 1;
        }
        hash
    }

    /// The schema of an aggregate: its state names and their fingerprints, in order.
    pub const fn schema(states: &[(&str, u64)]) -> u64 {
        let mut hash = 0xcbf29ce484222325;
        let mut i = 0;
        while i < states.len() {
            hash = fnv(hash, states[i].0.as_bytes());
            hash = fnv(hash, &states[i].1.to_le_bytes());
            i += 1;
        }
        hash
    }

    /// The rejection of one aggregate, as the rejection of its context.
    pub fn rejected_as<R, C>(error: crate::SendError<R>, f: impl FnOnce(R) -> C) -> crate::SendError<C> {
        match error {
            crate::SendError::Rejected(rejection) => crate::SendError::Rejected(f(rejection)),
            crate::SendError::Conflict => crate::SendError::Conflict,
            crate::SendError::Store(error) => crate::SendError::Store(error),
            crate::SendError::Unreplayable(error) => crate::SendError::Unreplayable(error),
        }
    }

    /// How many times `send` decides a command before it gives up on conflicts.
    const ATTEMPTS: usize = 3;

    /// Loads the instance `command` targets from `store`, lets it decide, and appends the
    /// events it records with a snapshot of the new state, deciding again on a conflict.
    pub async fn send<A: crate::Aggregate, S: DynStore<A> + ?Sized>(
        store: &S,
        command: &A::Command,
    ) -> Result<Vec<A::Event>, crate::SendError<A::Rejection>> {
        use crate::{AppendError, SendError, Snapshot, Version};
        let id = crate::Command::target(command);
        for _ in 0..ATTEMPTS {
            let loaded = store.dyn_load(id).await.map_err(SendError::Store)?;
            let (start, version, events) = match loaded.snapshot {
                None => (A::default(), Version(0), loaded.events),
                Some(snapshot) => match (snapshot.schema == A::SCHEMA).then(|| A::restore(snapshot.state)).flatten() {
                    Some(state) => (state, snapshot.version, loaded.events),
                    // Written by code whose states have since changed: replay every event.
                    None => (A::default(), Version(0), store.dyn_events(id).await.map_err(SendError::Store)?),
                },
            };
            let version = Version(version.0 + events.len() as u64);
            let current = events.iter().try_fold(start, A::evolve).map_err(SendError::Unreplayable)?;
            let recorded = current.decide(command).map_err(SendError::Rejected)?;
            let next = recorded.iter().try_fold(current, A::evolve).map_err(SendError::Unreplayable)?;
            let decided = crate::Decided {
                id: id.clone(),
                expected: version,
                snapshot: Snapshot {
                    schema: A::SCHEMA,
                    version: Version(version.0 + recorded.len() as u64),
                    state: next.save(),
                    aggregate: core::marker::PhantomData,
                },
                events: recorded,
            };
            match store.dyn_append(&decided).await {
                Ok(()) => return Ok(decided.events),
                Err(AppendError::Conflict) => continue,
                Err(AppendError::Store(error)) => return Err(SendError::Store(error)),
            }
        }
        Err(SendError::Conflict)
    }

    /// Decodes the `kind` and fields [`read_tagged`] returned as the tagged enum `T`.
    pub fn retagged<T: serde::de::DeserializeOwned, E: serde::de::Error>(
        kind: String,
        fields: serde_value::Value,
    ) -> Result<T, E> {
        use serde_value::Value;
        let Value::Map(mut fields) = fields else { unreachable!("read_tagged returns a map") };
        fields.insert(Value::String("kind".into()), Value::String(kind));
        Value::Map(fields).deserialize_into().map_err(E::custom)
    }

    /// Starts the events a state emits. Called by the `emit` method `#[oath::aggregate]`
    /// gives each state.
    pub fn emit<S: On<E>, E: Into<EventOf<S>>>(event: E) -> Emitted<S, <S as On<E>>::To> {
        Emitted::<S, S> { events: Vec::new(), states: core::marker::PhantomData }.then(event)
    }
}
