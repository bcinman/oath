use oath::{Context, Decision, Handle, MemoryStore, NotAllowed, Store, Transition};

#[oath::id(prefix = "ord_")] pub struct OrderId(String);
#[oath::id(prefix = "cus_")] pub struct CustomerId(String);

#[oath::command] pub struct PlaceOrder { #[id] pub order: OrderId, pub total: u64 }
#[oath::command] pub struct PayOrder { #[id] pub order: OrderId, pub amount: u64 }
#[oath::event] pub struct OrderPlaced { pub total: u64 }
#[oath::event] pub struct OrderPaid { pub amount: u64 }
#[oath::rejection] pub enum OrderRejection { WrongAmount { expected: u64 } }

pub struct Draft;
pub struct Placed { total: u64 }
pub struct Paid;

#[oath::aggregate(id = OrderId, command = OrderCommand, event = OrderEvent, rejection = OrderRejection)]
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

pub struct New;
pub struct Active;

#[oath::aggregate(id = CustomerId, command = CustomerCommand, event = CustomerEvent, rejection = CustomerRejection)]
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

#[oath::context(command = ShopCommand, event = ShopEvent, rejection = ShopRejection)]
pub enum Shop { Order, Customer }

fn ord(id: &str) -> OrderId { OrderId::new(id).unwrap() }
fn cus(id: &str) -> CustomerId { CustomerId::new(id).unwrap() }
fn shop() -> Shop { Shop::new(MemoryStore::default()) }

/// The rejection for `command`, which order instances in state `state` don't accept.
fn not_allowed(state: &str, command: &str) -> Result<Vec<ShopEvent>, ShopRejection> {
    Err(ShopRejection::Order(OrderRejection::NotAllowed(NotAllowed { state: state.into(), command: command.into() })))
}

#[test]
fn send_routes_each_command_to_the_instance_it_targets() {
    let mut shop = shop();
    assert_eq!(
        shop.send(PlaceOrder { order: ord("ord_1"), total: 40 }),
        Ok(vec![ShopEvent::Order(OrderPlaced { total: 40 }.into())]),
    );
    shop.send(PlaceOrder { order: ord("ord_2"), total: 7 }).unwrap();
    let pay: OrderCommand = PayOrder { order: ord("ord_1"), amount: 40 }.into();
    assert_eq!(shop.send(pay), Ok(vec![ShopEvent::Order(OrderPaid { amount: 40 }.into())]));
    assert_eq!(
        shop.send(OpenAccount { customer: cus("cus_1") }),
        Ok(vec![ShopEvent::Customer(AccountOpened.into())]),
    );

    // Each instance is in the state its own events moved it to.
    assert_eq!(shop.send(PayOrder { order: ord("ord_1"), amount: 40 }), not_allowed("Paid", "PayOrder"));
    assert_eq!(shop.send(PlaceOrder { order: ord("ord_2"), total: 7 }), not_allowed("Placed", "PlaceOrder"));
    assert_eq!(shop.send(PayOrder { order: ord("ord_3"), amount: 7 }), not_allowed("Draft", "PayOrder"));
    assert_eq!(
        shop.send(OpenAccount { customer: cus("cus_1") }),
        Err(ShopRejection::Customer(CustomerRejection::NotAllowed(NotAllowed {
            state: "Active".into(),
            command: "OpenAccount".into(),
        }))),
    );
}

#[test]
fn a_rejected_command_records_nothing() {
    let mut shop = shop();
    assert_eq!(shop.send(PayOrder { order: ord("ord_1"), amount: 40 }), not_allowed("Draft", "PayOrder"));
    // The order is still a draft, so it can be placed.
    shop.send(PlaceOrder { order: ord("ord_1"), total: 40 }).unwrap();
    assert_eq!(
        shop.send(PayOrder { order: ord("ord_1"), amount: 39 }),
        Err(ShopRejection::Order(OrderRejection::WrongAmount { expected: 40 })),
    );
    // The order is still placed, with the total it was placed with.
    assert!(shop.send(PayOrder { order: ord("ord_1"), amount: 40 }).is_ok());
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

    shop.send(open).unwrap();
    let events = shop.send(place).unwrap();
    let wire = to_string(&events).unwrap();
    assert_eq!(wire, r#"[{"Order":{"kind":"OrderPlaced","total":40}}]"#);
    assert_eq!(from_str::<Vec<ShopEvent>>(&wire).unwrap(), events);

    let rejection = shop.send(PayOrder { order: ord("ord_1"), amount: 1 }).unwrap_err();
    let wire = to_string(&rejection).unwrap();
    assert_eq!(wire, r#"{"Order":{"kind":"WrongAmount","expected":40}}"#);
    assert_eq!(from_str::<ShopRejection>(&wire).unwrap(), rejection);
}

#[test]
fn a_memory_store_keeps_each_instance_history_in_order_and_apart() {
    let mut store = MemoryStore::default();
    Store::<Order>::append(&mut store, &ord("ord_1"), &[OrderPlaced { total: 40 }.into()]);
    Store::<Order>::append(&mut store, &ord("ord_2"), &[OrderPlaced { total: 7 }.into()]);
    Store::<Order>::append(&mut store, &ord("ord_1"), &[OrderPaid { amount: 40 }.into()]);
    Store::<Customer>::append(&mut store, &cus("cus_1"), &[AccountOpened.into()]);

    assert_eq!(
        Store::<Order>::load(&store, &ord("ord_1")),
        vec![OrderPlaced { total: 40 }.into(), OrderPaid { amount: 40 }.into()],
    );
    assert_eq!(Store::<Order>::load(&store, &ord("ord_2")), vec![OrderPlaced { total: 7 }.into()]);
    assert_eq!(Store::<Order>::load(&store, &ord("ord_3")), vec![]);
    assert_eq!(Store::<Customer>::load(&store, &cus("cus_1")), vec![AccountOpened.into()]);
    assert_eq!(Store::<Customer>::load(&store, &cus("cus_2")), vec![]);

    // A context replays an instance from the events already in its store.
    let mut shop = Shop::new(store);
    assert_eq!(shop.send(PayOrder { order: ord("ord_1"), amount: 40 }), not_allowed("Paid", "PayOrder"));
    assert_eq!(shop.send(PayOrder { order: ord("ord_2"), amount: 7 }).map(|events| events.len()), Ok(1));
}
