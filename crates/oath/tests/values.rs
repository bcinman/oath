use std::net::IpAddr;

use oath::Invalid;
use serde_json::{from_str, to_string};

// Layer 2: oath's rules on a String and on a number.
#[oath::id(prefix = "ord_", len = 5..=40, chars = "a-z0-9_")]
pub struct OrderId(String);

#[oath::value(range = 0..=100)]
pub struct Percent(u8);

#[oath::value(range = 1..)]
pub struct Quantity(u32);

// Layer 1: the inner type parses the format. Layer 3: a custom check narrows it.
#[oath::value(check = is_private)]
pub struct InternalAddress(IpAddr);

fn is_private(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_private(),
        IpAddr::V6(_) => false,
    }
}

fn broken(type_name: &'static str, rule: &'static str) -> Invalid {
    Invalid { type_name, rule }
}

#[test]
fn new_accepts_a_value_that_keeps_every_rule_and_names_the_first_it_breaks() {
    assert_eq!(OrderId::new("ord_8f2k").unwrap().get(), "ord_8f2k");
    assert_eq!(OrderId::new("8f2k_ord"), Err(broken("OrderId", r#"prefix = "ord_""#)));
    assert_eq!(OrderId::new("ord_"), Err(broken("OrderId", "len = 5..=40")));
    assert_eq!(OrderId::new("ord_8F2K"), Err(broken("OrderId", r#"chars = "a-z0-9_""#)));
    assert_eq!(
        OrderId::new("ord_X").unwrap_err().to_string(),
        r#"OrderId breaks the rule `chars = "a-z0-9_"`"#,
    );

    assert_eq!(*Percent::new(0).unwrap().get(), 0);
    assert_eq!(*Percent::new(100).unwrap().get(), 100);
    assert_eq!(Percent::new(101), Err(broken("Percent", "range = 0..=100")));
    assert_eq!(Quantity::new(0u32), Err(broken("Quantity", "range = 1..")));
    assert!(Quantity::new(u32::MAX).is_ok());

    let private: IpAddr = "10.0.0.7".parse().unwrap();
    let public: IpAddr = "8.8.8.8".parse().unwrap();
    assert!(InternalAddress::new(private).is_ok());
    assert_eq!(InternalAddress::new(public), Err(broken("InternalAddress", "check = is_private")));
}

#[test]
fn values_go_over_the_wire_bare_and_are_checked_when_read() {
    assert_eq!(to_string(&OrderId::new("ord_1").unwrap()).unwrap(), r#""ord_1""#);
    assert_eq!(from_str::<OrderId>(r#""ord_1""#).unwrap(), OrderId::new("ord_1").unwrap());
    for bad in [r#""""#, r#""o-1""#, r#""ord_X""#] {
        let error = from_str::<OrderId>(bad).unwrap_err().to_string();
        assert!(error.starts_with("OrderId breaks the rule"), "{bad}: {error}");
    }
    assert_eq!(from_str::<Percent>("42").unwrap(), Percent::new(42).unwrap());
    assert!(from_str::<Percent>("101").is_err());
    assert_eq!(from_str::<InternalAddress>(r#""10.0.0.7""#).unwrap().get().to_string(), "10.0.0.7");
    assert!(from_str::<InternalAddress>(r#""8.8.8.8""#).is_err());
    assert!(from_str::<InternalAddress>(r#""10.0.0.999""#).is_err());
}

// A value with several fields: each field is checked by its own type, and `check` holds the
// rule across fields.
#[oath::value]
#[derive(PartialOrd, Ord)]
pub struct Money(u64);

#[oath::value(check = discount_within_price)]
pub struct DiscountedPrice {
    price: Money,
    discount: Money,
}

fn discount_within_price(p: &DiscountedPrice) -> bool {
    p.discount() <= p.price()
}

#[test]
fn a_value_with_several_fields_keeps_its_rule_across_them() {
    let money = |cents: u64| Money::new(cents).unwrap();
    let ok = DiscountedPrice::new(money(100), money(20)).unwrap();
    assert_eq!((ok.price(), ok.discount()), (&money(100), &money(20)));
    assert!(DiscountedPrice::new(money(100), money(100)).is_ok());
    assert_eq!(
        DiscountedPrice::new(money(10), money(20)),
        Err(broken("DiscountedPrice", "check = discount_within_price")),
    );

    assert_eq!(to_string(&ok).unwrap(), r#"{"price":100,"discount":20}"#);
    assert_eq!(from_str::<DiscountedPrice>(r#"{"price":100,"discount":20}"#).unwrap(), ok);
    let error = from_str::<DiscountedPrice>(r#"{"price":10,"discount":20}"#).unwrap_err();
    assert!(error.to_string().starts_with("DiscountedPrice breaks the rule"), "{error}");
    assert!(from_str::<DiscountedPrice>(r#"{"price":100}"#).is_err());
}

#[oath::value(range = 0.0..=1.0)]
#[derive(PartialOrd)]
pub struct Ratio(f64);

#[test]
fn a_float_value_is_finite_so_it_has_eq_and_hash() {
    assert_eq!(*Ratio::new(0.25).unwrap().get(), 0.25);
    assert_eq!(Ratio::new(f64::NAN), Err(broken("Ratio", "finite")));
    assert_eq!(Ratio::new(f64::INFINITY), Err(broken("Ratio", "finite")));
    assert_eq!(Ratio::new(1.5), Err(broken("Ratio", "range = 0.0..=1.0")));
    assert!(Ratio::new(0.25).unwrap() < Ratio::new(0.5).unwrap());

    let zeros = std::collections::HashSet::from([Ratio::new(0.0).unwrap(), Ratio::new(-0.0).unwrap()]);
    assert_eq!(zeros.len(), 1);

    assert_eq!(from_str::<Ratio>("0.25").unwrap(), Ratio::new(0.25).unwrap());
    assert!(from_str::<Ratio>("1.5").is_err());
}

// `range` bounds any ordered type, with constants of that type.
const SHORTEST: std::time::Duration = std::time::Duration::from_secs(1);

#[oath::value(range = SHORTEST..=std::time::Duration::from_secs(3600))]
pub struct Timeout(std::time::Duration);

#[test]
fn range_bounds_any_ordered_type_with_constants() {
    use std::time::Duration;
    assert!(Timeout::new(Duration::from_secs(1)).is_ok());
    assert!(Timeout::new(Duration::from_secs(3600)).is_ok());
    assert_eq!(
        Timeout::new(Duration::from_millis(999)),
        Err(broken("Timeout", "range = SHORTEST..=std::time::Duration::from_secs(3600)")),
    );
    assert!(Timeout::new(Duration::from_secs(3601)).is_err());
    assert!(from_str::<Timeout>(r#"{"secs":0,"nanos":5}"#).is_err());
}
