//! Commands, events, and aggregates: the core of a domain-driven, event-sourced system.
//!
//! - An **id** identifies one aggregate. Declare it as a branded newtype with `#[oath::id]`,
//!   such as `struct OrderId(pub String)`.
//! - A **command** requests a business action. Declare it as a struct with `#[oath::command]`,
//!   and mark the field holding the id of the aggregate it targets `#[id]`.
//! - An **event** records something that happened. Declare it as a struct with `#[oath::event]`.
//! - An **aggregate** decides which commands are accepted and how events change its state.
//!   Every aggregate is a state machine: an enum of state names with `#[oath::aggregate]`.
//!
//! - A **rejection** says why an aggregate refused a command. Declare one enum of reasons per
//!   aggregate with `#[oath::rejection]`; it adds a `NotAllowed` variant for commands the
//!   current state does not accept.
//!
//! `#[oath::command]`, `#[oath::event]`, and `#[oath::rejection]` derive `Debug`, `Clone`,
//! `PartialEq`, and serde's `Serialize` and `Deserialize`, so every command, event, and
//! rejection can go over the wire. Your crate needs no serde dependency of its own.
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
//! rejected with [`NotAllowed`]. The `#[initial]` state's struct must implement `Default`.
//!
//! The aggregate itself is opaque: `#[oath::aggregate]` turns the enum into a struct whose
//! state no code can build or change except [`Aggregate::evolve`], starting from the
//! `#[initial]` state. Read the current state's name with [`Aggregate::state`]. Declare
//! aggregates at module level, not inside a function body.
//!
//! ```
//! use oath::{Aggregate, Command, Decision, Handle, NotAllowed, Transition};
//!
//! #[oath::id] pub struct OrderId(pub String);
//!
//! #[oath::command] pub struct PlaceOrder { #[id] pub order: OrderId, pub total: u64 }
//! #[oath::command] pub struct PayOrder { #[id] pub order: OrderId }
//!
//! #[oath::event] pub struct OrderPlaced { pub total: u64 }
//! #[oath::event] pub struct OrderPaid { pub amount: u64 }
//!
//! #[oath::rejection] pub enum OrderRejection { WrongAmount { expected: u64 } }
//!
//! #[derive(Default)] pub struct Draft;
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
//! let id = || OrderId("o-1".into());
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

pub use oath_macros::{aggregate, command, event, id, rejection};

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
/// serde. On the wire an id is its inner value: `OrderId("o-1".into())` is `"o-1"`.
pub trait Id:
    Clone + Eq + core::hash::Hash + core::fmt::Debug + serde::Serialize + serde::de::DeserializeOwned
{
}

/// A consistency boundary: a state machine that decides which commands are accepted.
///
/// Implemented only by `#[oath::aggregate]`. Its `Default` is the `#[initial]` state.
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

    /// Starts the events a state emits. Called by the `emit` method `#[oath::aggregate]`
    /// gives each state.
    pub fn emit<S: On<E>, E: Into<EventOf<S>>>(event: E) -> Emitted<S, <S as On<E>>::To> {
        Emitted::<S, S> { events: Vec::new(), states: core::marker::PhantomData }.then(event)
    }
}
