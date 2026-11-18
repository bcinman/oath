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
//!   outside world reaches them. Declare it as an enum of aggregate names with
//!   `#[oath::context]`. See [`Context`].
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
//! events, under the names given in `command = ...` and `event = ...`. Each holds every
//! command its states accept and every event in their transitions, as `PlaceOrder(PlaceOrder)`,
//! and has `From` for each struct. The aggregate's states are listed as bare names, and each
//! name expands the same way.
//!
//! Each state implements [`Handle`] for every command it accepts (the decide step) and
//! [`Transition`] for every event that moves it to another state (the evolve step). The
//! `#[state(...)]` attribute on each state is the transition table, and `#[oath::aggregate]`
//! turns it into an [`Aggregate`] implementation. A command a state does not accept is
//! rejected with [`NotAllowed`]. The `#[initial]` state means no event has happened yet, so
//! it holds no data: write it as `struct Draft;`.
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
//! pub struct Draft;
//! pub struct Placed { total: u64 }
//! pub struct Paid;
//!
//! #[oath::aggregate(id = OrderId, command = OrderCommand, event = OrderEvent, rejection = OrderRejection)]
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
//! let order = Order::replay(&events);
//! assert_eq!(order.state(), "Placed");
//! assert_eq!(
//!     order.decide(&place),
//!     Err(OrderRejection::NotAllowed(NotAllowed { state: "Placed".into(), command: "PlaceOrder".into() })),
//! );
//! let pay = PayOrder { order: id() }.into();
//! assert_eq!(order.decide(&pay), Ok(vec![OrderPaid { amount: 40 }.into()]));
//! # }
//! ```

pub use oath_macros::{aggregate, command, context, event, id, rejection, value};

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
    Clone + Eq + core::hash::Hash + core::fmt::Debug + serde::Serialize + serde::de::DeserializeOwned
{
}

/// A consistency boundary: a state machine that decides which commands are accepted.
///
/// Implemented only by `#[oath::aggregate]`. Its `Default` is the `#[initial]` state, which
/// holds no data.
pub trait Aggregate: Default + __private::Sealed {
    /// What identifies one instance of this aggregate.
    type Id: Id;
    /// The enum of commands this aggregate decides. Each one targets an aggregate by its id.
    type Command: Command<Id = Self::Id>;
    /// The enum of events this aggregate records.
    type Event: Event;
    /// The enum of reasons this aggregate rejects a command for.
    type Rejection: Rejection;

    /// Every `(from, event, to)` transition the aggregate declares.
    const TRANSITIONS: &'static [Edge];

    /// The current state's name.
    fn state(&self) -> &'static str;

    /// Pure: the current state and a command give the events to record, or a rejection.
    fn decide(&self, command: &Self::Command) -> Result<Vec<Self::Event>, Self::Rejection>;

    /// Pure and infallible: the state after `event`.
    ///
    /// # Panics
    ///
    /// When the current state declares no transition for `event`, which means the history
    /// and the model disagree.
    fn evolve(self, event: &Self::Event) -> Self;

    /// The state after every event in `events`, starting from the initial state.
    fn replay<'a>(events: impl IntoIterator<Item = &'a Self::Event>) -> Self
    where
        Self::Event: 'a,
    {
        events.into_iter().fold(Self::default(), Self::evolve)
    }
}

/// A bounded context: the aggregates of one domain, and the one way the outside world reaches
/// them.
///
/// Implemented only by `#[oath::context]` on an enum of aggregate names, such as
/// `#[oath::context(command = ShopCommand, event = ShopEvent, rejection = ShopRejection)]
/// pub enum Shop { Order, Customer }`. It defines the three enums named there, with one variant
/// per aggregate holding that aggregate's enum, such as `ShopEvent::Order(OrderEvent)`.
///
/// The context keeps no aggregate instance: it holds a [`Store`] of their events. Build it with
/// `Shop::new(store)`, which takes one store that keeps the events of every aggregate the
/// context lists, such as a [`MemoryStore`]. Each aggregate belongs to exactly one context,
/// and a context is as visible as its aggregates.
///
/// A context only decides commands: its aggregates are opaque, and it has no way to read them.
/// Reads will come from views, which the events `send` returns keep up to date.
///
/// On the wire a context's command is its aggregate's command, unchanged, such as
/// `{"kind":"PlaceOrder","order":"ord_1"}`: its name says which aggregate decides it, so no two
/// aggregates in one context may have a command with the same name. Events and rejections go
/// under their aggregate's name, such as `{"Order":{"kind":"OrderPlaced","total":40}}`, since
/// every aggregate has a `NotAllowed` rejection.
pub trait Context: __private::Sealed {
    /// The enum of every command the context's aggregates decide.
    type Command;
    /// The enum of every event the context's aggregates record.
    type Event;
    /// The enum of every reason the context's aggregates reject a command for.
    type Rejection;

    /// Routes `command` to the aggregate instance it targets: replays that instance's events
    /// from the store, lets it decide, and appends the events it records, which are returned.
    /// A rejection appends nothing.
    ///
    /// Takes a command struct, an aggregate's command enum, or the context's own command enum,
    /// such as one read from the wire. A command of an aggregate outside this context doesn't
    /// compile.
    fn send(&mut self, command: impl Into<Self::Command>) -> Result<Vec<Self::Event>, Self::Rejection>;
}

/// The one context an aggregate belongs to. Implemented by `#[oath::context]` for every
/// aggregate it lists.
///
/// An aggregate belongs to exactly one context, so two contexts that list the same aggregate
/// don't compile: the second one is a conflicting implementation of this trait.
pub trait BelongsToContext: Aggregate {
    /// The context that lists this aggregate.
    type Context: Context;
}

/// Where a context keeps the events of aggregate `A`, by instance id. The events are the
/// only record of an instance: its state is replayed from them.
pub trait Store<A: Aggregate> {
    /// Every event recorded for instance `id`, oldest first; empty when it has none.
    fn load(&self, id: &A::Id) -> Vec<A::Event>;
    /// Records `events` after the ones already recorded for instance `id`.
    fn append(&mut self, id: &A::Id, events: &[A::Event]);
}

/// A [`Store`] that keeps every aggregate's events in memory, for tests and prototypes. Its
/// `Default` holds no events.
#[derive(Default)]
pub struct MemoryStore {
    /// One `HashMap<A::Id, Vec<A::Event>>` per aggregate `A`, keyed by `A`'s type.
    histories: std::collections::HashMap<core::any::TypeId, Box<dyn core::any::Any + Send + Sync>>,
}

impl<A> Store<A> for MemoryStore
where
    A: Aggregate + 'static,
    A::Id: Send + Sync + 'static,
    A::Event: Clone + Send + Sync + 'static,
{
    fn load(&self, id: &A::Id) -> Vec<A::Event> {
        self.histories
            .get(&core::any::TypeId::of::<A>())
            .and_then(|histories| histories.downcast_ref::<std::collections::HashMap<A::Id, Vec<A::Event>>>())
            .and_then(|histories| histories.get(id))
            .cloned()
            .unwrap_or_default()
    }

    fn append(&mut self, id: &A::Id, events: &[A::Event]) {
        self.histories
            .entry(core::any::TypeId::of::<A>())
            .or_insert_with(|| Box::new(std::collections::HashMap::<A::Id, Vec<A::Event>>::new()))
            .downcast_mut::<std::collections::HashMap<A::Id, Vec<A::Event>>>()
            .expect("each aggregate's histories are keyed by its type")
            .entry(id.clone())
            .or_default()
            .extend_from_slice(events);
    }
}

/// The one aggregate a command or event struct belongs to. Implemented by `#[oath::aggregate]`
/// for every command and event in its transition table.
///
/// Each command and event belongs to exactly one aggregate's history, so two aggregates that
/// list the same struct don't compile: the second one is a conflicting implementation of this
/// trait. For another aggregate to react to an event, give it its own command and event.
/// A context routes each command to its aggregate through this trait.
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

    /// Replays the instance `command` targets from `store`, lets it decide, and appends the
    /// events it records.
    pub fn send<A: crate::Aggregate, S: crate::Store<A> + ?Sized>(
        store: &mut S,
        command: &A::Command,
    ) -> Result<Vec<A::Event>, A::Rejection> {
        let target = crate::Command::target(command);
        let events = A::replay(&store.load(target)).decide(command)?;
        store.append(target, &events);
        Ok(events)
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
