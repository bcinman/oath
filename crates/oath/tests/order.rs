use oath::{Aggregate, Command, Edge, Event, Handle, NotAllowed, Rejection, Transition};

#[oath::command] pub struct PlaceOrder { pub total: u64 }
#[oath::command] pub struct PayOrder { pub amount: u64 }
#[oath::command] pub struct CancelOrder;
#[oath::command] pub enum OrderCommand { PlaceOrder, PayOrder, CancelOrder }

#[oath::event] pub struct OrderPlaced { pub total: u64 }
#[oath::event] pub struct OrderPaid { pub amount: u64 }
#[oath::event] pub struct OrderCancelled;
#[oath::event] pub struct RefundIssued { pub amount: u64 }
#[oath::event] pub enum OrderEvent { OrderPlaced, OrderPaid, OrderCancelled, RefundIssued }

#[oath::rejection] pub enum OrderRejection { WrongAmount { expected: u64 }, AlreadyFinal }

#[derive(Default)] pub struct Draft;
pub struct Placed { total: u64 }
pub struct Paid { amount: u64 }
pub struct Cancelled;

#[oath::aggregate(command = OrderCommand, event = OrderEvent, rejection = OrderRejection)]
pub enum Order {
    #[initial]
    #[state(accepts(PlaceOrder), on(OrderPlaced => Placed))]
    Draft,
    #[state(accepts(PayOrder, CancelOrder), on(OrderPaid => Paid, OrderCancelled => Cancelled))]
    Placed,
    #[state(accepts(CancelOrder), on(OrderCancelled => Cancelled, RefundIssued => Paid))]
    Paid,
    Cancelled,
}

impl Handle<PlaceOrder> for Draft {
    fn decide(&self, c: &PlaceOrder) -> Result<Vec<OrderEvent>, OrderRejection> {
        Ok(vec![OrderPlaced { total: c.total }.into()])
    }
}
impl Transition<OrderPlaced> for Draft {
    type To = Placed;
    fn evolve(self, e: &OrderPlaced) -> Placed { Placed { total: e.total } }
}

impl Handle<PayOrder> for Placed {
    fn decide(&self, c: &PayOrder) -> Result<Vec<OrderEvent>, OrderRejection> {
        if c.amount != self.total {
            return Err(OrderRejection::WrongAmount { expected: self.total });
        }
        Ok(vec![OrderPaid { amount: c.amount }.into()])
    }
}
impl Handle<CancelOrder> for Placed {
    fn decide(&self, _: &CancelOrder) -> Result<Vec<OrderEvent>, OrderRejection> {
        Ok(vec![OrderCancelled.into()])
    }
}
impl Transition<OrderPaid> for Placed {
    type To = Paid;
    fn evolve(self, e: &OrderPaid) -> Paid { Paid { amount: e.amount } }
}
impl Transition<OrderCancelled> for Placed {
    type To = Cancelled;
    fn evolve(self, _: &OrderCancelled) -> Cancelled { Cancelled }
}

// Cancelling a paid order records two events: the refund, then the cancellation.
impl Handle<CancelOrder> for Paid {
    fn decide(&self, _: &CancelOrder) -> Result<Vec<OrderEvent>, OrderRejection> {
        Ok(vec![RefundIssued { amount: self.amount }.into(), OrderCancelled.into()])
    }
}
impl Transition<RefundIssued> for Paid {
    type To = Paid;
    fn evolve(self, _: &RefundIssued) -> Paid { Paid { amount: 0 } }
}
impl Transition<OrderCancelled> for Paid {
    type To = Cancelled;
    fn evolve(self, _: &OrderCancelled) -> Cancelled { Cancelled }
}

/// Decides `command` against `order` and applies the resulting events.
fn run(order: Order, command: impl Into<OrderCommand>) -> (Order, Vec<OrderEvent>) {
    let events = order.decide(&command.into()).unwrap();
    (events.iter().fold(order, Order::evolve), events)
}

#[test]
fn a_paid_order_cancels_through_a_refund_and_replays_to_the_same_state() {
    let (order, mut history) = run(Order::default(), PlaceOrder { total: 40 });
    let (order, events) = run(order, PayOrder { amount: 40 });
    history.extend(events);
    let (order, events) = run(order, CancelOrder);
    assert_eq!(events, vec![RefundIssued { amount: 40 }.into(), OrderCancelled.into()]);
    history.extend(events);

    assert_eq!(order.state(), "Cancelled");
    assert_eq!(Order::replay(&history).state(), "Cancelled");
    let names: Vec<_> = history.iter().map(Event::name).collect();
    assert_eq!(names, ["OrderPlaced", "OrderPaid", "RefundIssued", "OrderCancelled"]);
}

#[test]
fn a_command_the_state_does_not_accept_is_not_allowed() {
    let not_allowed = |state: &str, command: &str| {
        Err(OrderRejection::NotAllowed(NotAllowed { state: state.into(), command: command.into() }))
    };
    assert_eq!(Order::default().decide(&CancelOrder.into()), not_allowed("Draft", "CancelOrder"));

    let (placed, _) = run(Order::default(), PlaceOrder { total: 40 });
    assert_eq!(placed.decide(&PlaceOrder { total: 1 }.into()), not_allowed("Placed", "PlaceOrder"));

    let (cancelled, _) = run(placed, CancelOrder);
    let pay: OrderCommand = PayOrder { amount: 40 }.into();
    assert_eq!(pay.name(), "PayOrder");
    assert_eq!(cancelled.decide(&pay), not_allowed("Cancelled", "PayOrder"));
}

#[test]
fn a_state_rejects_an_accepted_command_with_its_own_reason() {
    let (placed, _) = run(Order::default(), PlaceOrder { total: 40 });
    let rejection = placed.decide(&PayOrder { amount: 39 }.into()).unwrap_err();
    assert_eq!(rejection, OrderRejection::WrongAmount { expected: 40 });
    assert_eq!(rejection.name(), "WrongAmount");
    assert_eq!(placed.state(), "Placed");

    let not_allowed = placed.decide(&PlaceOrder { total: 1 }.into()).unwrap_err();
    assert_eq!(not_allowed.name(), "NotAllowed");
    assert_eq!(OrderRejection::AlreadyFinal.name(), "AlreadyFinal");
}

#[test]
fn commands_events_and_rejections_go_over_the_wire_as_tagged_structs() {
    use serde_json::{from_str, to_string};

    let command: OrderCommand = PayOrder { amount: 40 }.into();
    assert_eq!(to_string(&command).unwrap(), r#"{"kind":"PayOrder","amount":40}"#);
    assert_eq!(from_str::<OrderCommand>(r#"{"amount":40,"kind":"PayOrder"}"#).unwrap(), command);
    let unit: OrderCommand = CancelOrder.into();
    assert_eq!(to_string(&unit).unwrap(), r#"{"kind":"CancelOrder"}"#);
    assert_eq!(from_str::<OrderCommand>(r#"{"kind":"CancelOrder"}"#).unwrap(), unit);

    let (order, mut history) = run(Order::default(), PlaceOrder { total: 40 });
    let (order, events) = run(order, PayOrder { amount: 40 });
    history.extend(events);
    let (_, events) = run(order, CancelOrder);
    history.extend(events);
    let wire = to_string(&history).unwrap();
    assert_eq!(
        wire,
        r#"[{"kind":"OrderPlaced","total":40},{"kind":"OrderPaid","amount":40},{"kind":"RefundIssued","amount":40},{"kind":"OrderCancelled"}]"#,
    );
    let received: Vec<OrderEvent> = from_str(&wire).unwrap();
    assert_eq!(received, history);
    assert_eq!(Order::replay(&received).state(), "Cancelled");

    let (placed, _) = run(Order::default(), PlaceOrder { total: 40 });
    let rejection = placed.decide(&PlaceOrder { total: 1 }.into()).unwrap_err();
    let wire = to_string(&rejection).unwrap();
    assert_eq!(wire, r#"{"kind":"NotAllowed","state":"Placed","command":"PlaceOrder"}"#);
    assert_eq!(from_str::<OrderRejection>(&wire).unwrap(), rejection);
    for (reason, json) in [
        (OrderRejection::WrongAmount { expected: 40 }, r#"{"kind":"WrongAmount","expected":40}"#),
        (OrderRejection::AlreadyFinal, r#"{"kind":"AlreadyFinal"}"#),
    ] {
        assert_eq!(to_string(&reason).unwrap(), json);
        assert_eq!(from_str::<OrderRejection>(json).unwrap(), reason);
    }
}

#[test]
fn transitions_list_every_declared_edge_in_order() {
    let edge = |from, event, to| Edge { from, event, to };
    assert_eq!(
        Order::TRANSITIONS,
        [
            edge("Draft", "OrderPlaced", "Placed"),
            edge("Placed", "OrderPaid", "Paid"),
            edge("Placed", "OrderCancelled", "Cancelled"),
            edge("Paid", "OrderCancelled", "Cancelled"),
            edge("Paid", "RefundIssued", "Paid"),
        ],
    );
}

#[test]
#[should_panic(expected = "event OrderPaid has no transition from state Draft")]
fn replaying_an_event_with_no_transition_panics() {
    Order::replay(&[OrderPaid { amount: 1 }.into()]);
}
