//! Commands, events, and aggregates: the core of a domain-driven, event-sourced system.
//!
//! - A **command** requests a business action. Declare it as a struct with `#[oath::command]`.
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
//! ```
//! use oath::{Aggregate, Handle, NotAllowed, Transition};
//!
//! #[oath::command] pub struct PlaceOrder { pub total: u64 }
//! #[oath::command] pub struct PayOrder;
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
//! #[oath::aggregate(command = OrderCommand, event = OrderEvent, rejection = OrderRejection)]
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
//!     fn decide(&self, c: &PlaceOrder) -> Result<Vec<OrderEvent>, OrderRejection> {
//!         Ok(vec![OrderPlaced { total: c.total }.into()])
//!     }
//! }
//! impl Transition<OrderPlaced> for Draft {
//!     type To = Placed;
//!     fn evolve(self, e: &OrderPlaced) -> Placed { Placed { total: e.total } }
//! }
//! impl Handle<PayOrder> for Placed {
//!     fn decide(&self, _: &PayOrder) -> Result<Vec<OrderEvent>, OrderRejection> {
//!         Ok(vec![OrderPaid { amount: self.total }.into()])
//!     }
//! }
//! impl Transition<OrderPaid> for Placed {
//!     type To = Paid;
//!     fn evolve(self, _: &OrderPaid) -> Paid { Paid }
//! }
//!
//! let events = Order::default().decide(&PlaceOrder { total: 40 }.into()).unwrap();
//! let order = Order::replay(&events);
//! assert_eq!(order.state(), "Placed");
//! assert_eq!(
//!     order.decide(&PlaceOrder { total: 1 }.into()),
//!     Err(OrderRejection::NotAllowed(NotAllowed { state: "Placed".into(), command: "PlaceOrder".into() })),
//! );
//! assert_eq!(order.decide(&PayOrder.into()), Ok(vec![OrderPaid { amount: 40 }.into()]));
//! ```

pub use oath_macros::{aggregate, command, event, rejection};

/// A request to perform one business action, named in the imperative.
///
/// Implemented by `#[oath::command]` on each command struct and on the enum that groups one
/// aggregate's commands.
pub trait Command {
    /// The command's stable name: the struct's name, or for an enum, its variant's.
    fn name(&self) -> &'static str;
}

/// A fact that happened, named in the past tense.
///
/// Implemented by `#[oath::event]` on each event struct and on the enum that groups one
/// aggregate's events.
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

/// A consistency boundary: a state machine that decides which commands are accepted.
///
/// Implemented only by `#[oath::aggregate]`. Its `Default` is the `#[initial]` state.
pub trait Aggregate: Default + __private::Sealed {
    /// The enum of commands this aggregate decides.
    type Command: Command;
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
pub trait State {
    /// The aggregate this state belongs to.
    type Aggregate: Aggregate;
}

/// The event type of the aggregate that state `S` belongs to.
pub type EventOf<S> = <<S as State>::Aggregate as Aggregate>::Event;
/// The rejection type of the aggregate that state `S` belongs to.
pub type RejectionOf<S> = <<S as State>::Aggregate as Aggregate>::Rejection;

/// How a state decides command `C`. Implement it for each command listed in the state's
/// `accepts(...)`.
pub trait Handle<C>: State {
    /// Pure: the events to record, or a rejection.
    fn decide(&self, command: &C) -> Result<Vec<EventOf<Self>>, RejectionOf<Self>>;
}

/// How event `E` moves a state to the next one. Implement it for each `E => To` listed in the
/// state's `on(...)`.
pub trait Transition<E>: State {
    /// The next state. Must be the state named after `=>`.
    type To;
    /// Pure and infallible: the next state.
    fn evolve(self, event: &E) -> Self::To;
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
}
