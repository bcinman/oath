//! Attribute macros for the `oath` crate. Use them through `oath`: `#[oath::command]`,
//! `#[oath::event]`, `#[oath::rejection]`, `#[oath::id]`, `#[oath::value]`,
//! `#[oath::aggregate(...)]`, and `#[oath::context(...)]`.

use proc_macro::TokenStream;
use proc_macro2::{Literal, Span, TokenStream as TokenStream2};
use quote::{format_ident, quote, quote_spanned};
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::{
    Attribute, Error, Expr, Fields, Ident, Item, ItemEnum, ItemStruct, LitStr, Path, Result, Token, Type,
    Visibility, parenthesized, parse_quote,
};

/// Declares a command struct. `#[oath::aggregate]` defines the enum of an aggregate's commands.
#[proc_macro_attribute]
pub fn command(args: TokenStream, item: TokenStream) -> TokenStream {
    expand(message(args.into(), item.into(), Kind::Command))
}

/// Declares an event struct. `#[oath::aggregate]` defines the enum of an aggregate's events.
#[proc_macro_attribute]
pub fn event(args: TokenStream, item: TokenStream) -> TokenStream {
    expand(message(args.into(), item.into(), Kind::Event))
}

/// Declares an aggregate's identity: a value, as with `#[oath::value]`, that also implements
/// `oath::Id`, such as `#[oath::id(prefix = "ord_")] struct OrderId(String);`.
#[proc_macro_attribute]
pub fn id(args: TokenStream, item: TokenStream) -> TokenStream {
    expand(value_impl(args.into(), item.into(), true))
}

/// Declares a domain value: a branded newtype around one value that keeps its rules, such as
/// `#[oath::value(range = 0..=100)] struct Percent(u8);`.
#[proc_macro_attribute]
pub fn value(args: TokenStream, item: TokenStream) -> TokenStream {
    expand(value_impl(args.into(), item.into(), false))
}

/// Declares the enum of one aggregate's rejection reasons, and adds a `NotAllowed` variant.
#[proc_macro_attribute]
pub fn rejection(args: TokenStream, item: TokenStream) -> TokenStream {
    expand(rejection_impl(args.into(), item.into()))
}

/// Declares an aggregate: an enum of states with the transition table on its variants.
#[proc_macro_attribute]
pub fn aggregate(args: TokenStream, item: TokenStream) -> TokenStream {
    expand(aggregate_impl(args.into(), item.into()))
}

/// Declares a bounded context: an enum of the aggregates it holds.
#[proc_macro_attribute]
pub fn context(args: TokenStream, item: TokenStream) -> TokenStream {
    expand(context_impl(args.into(), item.into()))
}

fn expand(result: Result<TokenStream2>) -> TokenStream {
    result.unwrap_or_else(Error::into_compile_error).into()
}

#[derive(Clone, Copy)]
enum Kind {
    Command,
    Event,
    Rejection,
}

impl Kind {
    fn attribute(self) -> &'static str {
        match self {
            Kind::Command => "#[oath::command]",
            Kind::Event => "#[oath::event]",
            Kind::Rejection => "#[oath::rejection]",
        }
    }

    fn noun(self) -> &'static str {
        match self {
            Kind::Command => "command",
            Kind::Event => "event",
            Kind::Rejection => "rejection",
        }
    }

    fn trait_path(self) -> TokenStream2 {
        match self {
            Kind::Command => quote!(::oath::Command),
            Kind::Event => quote!(::oath::Event),
            Kind::Rejection => quote!(::oath::Rejection),
        }
    }
}

const STANDARD_DERIVES: [&str; 5] = ["Debug", "Clone", "PartialEq", "Serialize", "Deserialize"];

/// The derives every command, event, and rejection gets. Serde comes through `oath`, so user
/// crates need no serde dependency of their own.
fn standard_derives() -> TokenStream2 {
    quote! {
        #[derive(
            ::core::fmt::Debug,
            ::core::clone::Clone,
            ::core::cmp::PartialEq,
            ::oath::__private::serde::Serialize,
            ::oath::__private::serde::Deserialize,
        )]
        #[serde(crate = "::oath::__private::serde")]
    }
}

/// The optional `name = "..."` of `#[oath::command]` and `#[oath::event]`.
struct NameArg(Option<LitStr>);

impl Parse for NameArg {
    fn parse(input: ParseStream) -> Result<Self> {
        if input.is_empty() {
            return Ok(NameArg(None));
        }
        let key: Ident = input.parse()?;
        if key != "name" {
            return Err(Error::new_spanned(key, "expected `name = \"...\"`"));
        }
        input.parse::<Token![=]>()?;
        let name: LitStr = input.parse()?;
        if name.value().is_empty() {
            return Err(Error::new_spanned(name, "the name can't be empty"));
        }
        if !input.is_empty() {
            input.parse::<Token![,]>()?;
        }
        if !input.is_empty() {
            return Err(input.error("expected only `name = \"...\"`"));
        }
        Ok(NameArg(Some(name)))
    }
}

fn message(args: TokenStream2, item: TokenStream2, kind: Kind) -> Result<TokenStream2> {
    let NameArg(name) = syn::parse2(args)?;
    match syn::parse2::<Item>(item)? {
        Item::Struct(item) => message_struct(item, kind, name),
        Item::Enum(item) => Err(Error::new_spanned(
            &item.ident,
            format!(
                "{} goes on a struct; #[oath::aggregate] defines the enum of an aggregate's {}s \
                 from its #[state(...)] attributes",
                kind.attribute(),
                kind.noun()
            ),
        )),
        other => Err(Error::new_spanned(other, format!("{} goes on a struct", kind.attribute()))),
    }
}

fn message_struct(item: ItemStruct, kind: Kind, stable_name: Option<LitStr>) -> Result<TokenStream2> {
    reject_standard_derives(&item.attrs, kind)?;
    if matches!(item.fields, Fields::Unnamed(_)) {
        return Err(Error::new_spanned(
            &item.fields,
            format!(
                "a {} goes over the wire as a tagged struct, so give its fields names",
                kind.noun()
            ),
        ));
    }
    reject_tag_field(&item.fields)?;
    let mut item = item;
    let target = match kind {
        Kind::Command => {
            let (field, ty) = take_target_field(&mut item)?;
            quote! {
                type Id = #ty;
                fn target(&self) -> &#ty { &self.#field }
            }
        }
        _ => TokenStream2::new(),
    };
    let name = &item.ident;
    let name_str = stable_name.unwrap_or_else(|| LitStr::new(&name.to_string(), name.span()));
    let trait_path = kind.trait_path();
    let (impl_generics, type_generics, where_clause) = item.generics.split_for_impl();
    let standard = standard_derives();
    Ok(quote! {
        #standard
        #item
        impl #impl_generics ::oath::__private::Named for #name #type_generics #where_clause {
            const NAME: &'static str = #name_str;
        }
        impl #impl_generics #trait_path for #name #type_generics #where_clause {
            fn name(&self) -> &'static str { #name_str }
            #target
        }
    })
}

/// Removes `#[id]` from the one field that names the command's target aggregate, and returns
/// that field and its type.
fn take_target_field(item: &mut ItemStruct) -> Result<(Ident, Type)> {
    let mut target: Option<(Ident, Type)> = None;
    for field in item.fields.iter_mut() {
        let before = field.attrs.len();
        field.attrs.retain(|attr| !attr.path().is_ident("id"));
        if field.attrs.len() == before {
            continue;
        }
        if before - field.attrs.len() > 1 {
            return Err(Error::new_spanned(&*field, "mark the field #[id] once"));
        }
        if let Some((first, _)) = &target {
            return Err(Error::new_spanned(
                &*field,
                format!("a command targets one aggregate; `{first}` is already its #[id]"),
            ));
        }
        let ident = field.ident.clone().expect("tuple fields were rejected");
        target = Some((ident, field.ty.clone()));
    }
    target.ok_or_else(|| {
        Error::new_spanned(
            &item.ident,
            "a command targets one aggregate; mark the field holding its id #[id], \
             such as `#[id] pub order: OrderId`",
        )
    })
}

// ---------------------------------------------------------------------------------------------
// #[oath::id]

/// `#[oath::value]` on a struct with named fields. Each field is checked by its own type; the
/// only rule is `check`, which gets the whole value, for rules across fields.
fn composite_value(mut item: ItemStruct, rules: Vec<Rule>) -> Result<TokenStream2> {
    if let Some(rule) = rules.iter().find(|rule| !matches!(rule.kind, RuleKind::Check(_))) {
        return Err(Error::new_spanned(
            &rule.key,
            format!(
                "`{}` checks a single value; give the field its own value type, such as \
                 `#[oath::value({})] struct Code(String);`, and use `check` for rules across fields",
                rule.key, rule.text
            ),
        ));
    }
    for field in &item.fields {
        if is_float(&field.ty) {
            return Err(Error::new_spanned(
                &field.ty,
                "a field can't be a bare float, which can be NaN; give it a float value type, \
                 such as `#[oath::value] struct Coord(f64);`, which is always finite",
            ));
        }
    }
    let name = item.ident.clone();
    let type_name = LitStr::new(&name.to_string(), name.span());
    let vis = std::mem::replace(&mut item.vis, Visibility::Inherited);
    item.vis = syn::parse2(vis_one_level_down(&vis))?;
    let module = format_ident!("__oath_{}", name);
    let fields: Vec<_> = item.fields.iter().cloned().collect();
    let names: Vec<_> = fields.iter().map(|field| field.ident.clone().expect("named")).collect();
    let types: Vec<_> = fields.iter().map(|field| field.ty.clone()).collect();
    let serde_attrs = |attrs: &[Attribute]| -> Vec<Attribute> {
        attrs.iter().filter(|attr| attr.path().is_ident("serde")).cloned().collect()
    };
    let struct_serde = serde_attrs(&item.attrs);
    let field_serde: Vec<_> = fields.iter().map(|field| serde_attrs(&field.attrs)).collect();
    let check = rules.first().map(|Rule { text, kind, .. }| {
        let RuleKind::Check(path) = kind else { unreachable!() };
        quote_spanned! {path.span()=>
            let check: fn(&#name) -> bool = #path;
            if !check(&value) {
                return ::core::result::Result::Err(::oath::Invalid { type_name: #type_name, rule: #text });
            }
        }
    });
    let serde = quote!(::oath::__private::serde);
    Ok(quote! {
        #vis use #module::#name;

        #[doc(hidden)]
        #[allow(non_snake_case)]
        mod #module {
            #[allow(unused_imports)]
            use super::*;

            #[derive(
                ::core::fmt::Debug,
                ::core::clone::Clone,
                ::core::cmp::PartialEq,
                ::core::cmp::Eq,
                ::core::hash::Hash,
                #serde::Serialize,
            )]
            #[serde(crate = "::oath::__private::serde")]
            #item

            #[allow(dead_code, clippy::too_many_arguments)]
            impl #name {
                /// The value, if its fields keep its rule; otherwise the rule it breaks.
                pub fn new(#( #names: #types ),*) -> ::core::result::Result<Self, ::oath::Invalid> {
                    let value = Self { #( #names ),* };
                    #check
                    ::core::result::Result::Ok(value)
                }

                #(
                    pub fn #names(&self) -> &#types {
                        &self.#names
                    }
                )*
            }

            /// The fields as read from the wire, before `new` checks them.
            #[derive(#serde::Deserialize)]
            #[serde(crate = "::oath::__private::serde")]
            #( #struct_serde )*
            struct Unchecked { #( #( #field_serde )* #names: #types ),* }

            // Deserializing runs the same rule as `new`.
            impl<'de> #serde::Deserialize<'de> for #name {
                fn deserialize<D: #serde::Deserializer<'de>>(
                    deserializer: D,
                ) -> ::core::result::Result<Self, D::Error> {
                    let Unchecked { #( #names ),* } = Unchecked::deserialize(deserializer)?;
                    Self::new(#( #names ),*).map_err(<D::Error as #serde::de::Error>::custom)
                }
            }
        }
    })
}

/// Whether `ty` is written `f32` or `f64`.
fn is_float(ty: &Type) -> bool {
    matches!(ty, Type::Path(path) if path.qself.is_none()
        && (path.path.is_ident("f32") || path.path.is_ident("f64")))
}

/// One rule of `#[oath::value(...)]` or `#[oath::id(...)]`.
struct Rule {
    key: Ident,
    /// The rule as written, for `Invalid::rule`, such as `prefix = "ord_"`.
    text: String,
    kind: RuleKind,
}

enum RuleKind {
    Range(Option<Expr>, Option<Expr>),
    Len(Option<Expr>, Option<Expr>),
    Prefix(LitStr),
    Chars(Vec<TokenStream2>),
    Check(Path),
}

struct Rules(Vec<Rule>);

impl Parse for Rules {
    fn parse(input: ParseStream) -> Result<Self> {
        let mut rules: Vec<Rule> = Vec::new();
        while !input.is_empty() {
            let key: Ident = input.parse()?;
            if rules.iter().any(|rule| rule.key == key) {
                return Err(Error::new_spanned(key, "rule given twice"));
            }
            input.parse::<Token![=]>()?;
            let (kind, text) = match key.to_string().as_str() {
                "range" | "len" => {
                    let (lo, hi) = bounds(input)?;
                    let show = |e: &Option<Expr>| {
                        e.as_ref().map(|e| quote!(#e).to_string().replace(' ', "")).unwrap_or_default()
                    };
                    let eq = if hi.is_some() { "=" } else { "" };
                    let text = format!("{key} = {}..{eq}{}", show(&lo), show(&hi));
                    let kind = if key == "range" { RuleKind::Range(lo, hi) } else { RuleKind::Len(lo, hi) };
                    (kind, text)
                }
                "prefix" => {
                    let prefix: LitStr = input.parse()?;
                    if prefix.value().is_empty() {
                        return Err(Error::new_spanned(prefix, "the prefix can't be empty"));
                    }
                    let text = format!("prefix = {:?}", prefix.value());
                    (RuleKind::Prefix(prefix), text)
                }
                "chars" => {
                    let spec: LitStr = input.parse()?;
                    let text = format!("chars = {:?}", spec.value());
                    (RuleKind::Chars(char_patterns(&spec)?), text)
                }
                "check" => {
                    let path: Path = input.parse()?;
                    let text = format!("check = {}", quote!(#path).to_string().replace(' ', ""));
                    (RuleKind::Check(path), text)
                }
                _ => {
                    return Err(Error::new_spanned(
                        key,
                        "expected `range = a..=b`, `len = a..=b`, `prefix = \"...\"`, \
                         `chars = \"...\"`, or `check = function`",
                    ));
                }
            };
            rules.push(Rule { key, text, kind });
            if !input.is_empty() {
                input.parse::<Token![,]>()?;
            }
        }
        // The custom check is the last resort, so it runs after the built-in rules.
        rules.sort_by_key(|rule| matches!(rule.kind, RuleKind::Check(_)));
        Ok(Rules(rules))
    }
}

/// Inclusive bounds, written `a..=b`, `a..`, or `..=b`.
fn bounds(input: ParseStream) -> Result<(Option<Expr>, Option<Expr>)> {
    let expr: Expr = input.parse()?;
    let wrong = |expr: &Expr| {
        Error::new_spanned(expr, "write the bounds as an inclusive range: `a..=b`, `a..`, or `..=b`")
    };
    let Expr::Range(range) = &expr else { return Err(wrong(&expr)) };
    if matches!(range.limits, syn::RangeLimits::HalfOpen(_)) && range.end.is_some() {
        return Err(wrong(&expr));
    }
    if range.start.is_none() && range.end.is_none() {
        return Err(Error::new_spanned(&expr, "give at least one bound"));
    }
    Ok((range.start.as_deref().cloned(), range.end.as_deref().cloned()))
}

/// The match patterns for a `chars` spec such as `"a-z0-9_"`.
fn char_patterns(spec: &LitStr) -> Result<Vec<TokenStream2>> {
    let chars: Vec<char> = spec.value().chars().collect();
    if chars.is_empty() {
        return Err(Error::new_spanned(spec, "list at least one character, such as \"a-z0-9_\""));
    }
    let mut patterns = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if i + 2 < chars.len() && chars[i + 1] == '-' {
            let (from, to) = (chars[i], chars[i + 2]);
            if from > to {
                return Err(Error::new_spanned(spec, format!("`{from}-{to}` is backwards; write `{to}-{from}`")));
            }
            let (from, to) = (Literal::character(from), Literal::character(to));
            patterns.push(quote!(#from..=#to));
            i += 3;
        } else {
            let one = Literal::character(chars[i]);
            patterns.push(quote!(#one));
            i += 1;
        }
    }
    Ok(patterns)
}

fn value_impl(args: TokenStream2, item: TokenStream2, is_id: bool) -> Result<TokenStream2> {
    let attribute = if is_id { "#[oath::id]" } else { "#[oath::value]" };
    let Rules(rules) = syn::parse2(args)?;
    let mut item: ItemStruct = syn::parse2(item).map_err(|error| {
        Error::new(error.span(), format!("{attribute} goes on a struct around one value, such as `struct Percent(u8);`"))
    })?;
    reject_generics(&item.generics, attribute)?;
    const VALUE_DERIVES: [&str; 7] =
        ["Debug", "Clone", "PartialEq", "Eq", "Hash", "Serialize", "Deserialize"];
    for attr in item.attrs.iter().filter(|attr| attr.path().is_ident("derive")) {
        let paths = attr.parse_args_with(Punctuated::<Path, Token![,]>::parse_terminated)?;
        for path in paths {
            let last = path.segments.last().map(|s| s.ident.to_string()).unwrap_or_default();
            if VALUE_DERIVES.contains(&last.as_str()) {
                return Err(Error::new_spanned(
                    path,
                    format!("{attribute} already derives {}", VALUE_DERIVES.join(", ")),
                ));
            }
        }
    }
    for field in &item.fields {
        if !matches!(field.vis, Visibility::Inherited) {
            return Err(Error::new_spanned(
                &field.vis,
                "keep the field private: build the value with `new` and read it with its getter",
            ));
        }
    }
    let field = match &item.fields {
        Fields::Unnamed(fields) if fields.unnamed.len() == 1 => &fields.unnamed[0],
        Fields::Named(fields) if !is_id && !fields.named.is_empty() => {
            return composite_value(item, rules);
        }
        _ if is_id => {
            return Err(Error::new_spanned(
                &item.fields,
                "an id wraps exactly one value, such as `struct OrderId(String);`",
            ));
        }
        _ => {
            return Err(Error::new_spanned(
                &item.fields,
                "a value wraps one value, such as `struct Percent(u8);`, or has named fields, \
                 such as `struct DiscountedPrice { price: Money, discount: Money }`",
            ));
        }
    };
    let inner = field.ty.clone();
    let name = item.ident.clone();
    let type_name = LitStr::new(&name.to_string(), name.span());
    let vis = std::mem::replace(&mut item.vis, Visibility::Inherited);
    item.vis = syn::parse2(vis_one_level_down(&vis))?;
    let module = format_ident!("__oath_{}", name);
    // A float value is always finite: without NaN, equality is total, so Eq and Hash are sound.
    let float = is_float(&inner);

    let finite = float.then(|| quote! {
        if !value.is_finite() {
            return ::core::result::Result::Err(::oath::Invalid { type_name: #type_name, rule: "finite" });
        }
    });
    let checks = rules.iter().map(|Rule { key, text, kind }| {
        let fail = quote! {
            return ::core::result::Result::Err(::oath::Invalid { type_name: #type_name, rule: #text })
        };
        let opt = |e: &Option<Expr>| match e {
            Some(e) => quote!(::core::option::Option::Some(#e)),
            None => quote!(::core::option::Option::None),
        };
        match kind {
            // The bounds are constants of the inner type, so they never change: a stored value
            // that kept its rule when it was written keeps it on every replay.
            RuleKind::Range(lo, hi) => {
                let (lo, hi) = (opt(lo), opt(hi));
                quote_spanned! {key.span()=> {
                    const LOWEST: ::core::option::Option<#inner> = #lo;
                    const HIGHEST: ::core::option::Option<#inner> = #hi;
                    if !::oath::__private::in_range(value, LOWEST.as_ref(), HIGHEST.as_ref()) { #fail; }
                } }
            }
            RuleKind::Len(lo, hi) => {
                let (lo, hi) = (opt(lo), opt(hi));
                quote_spanned! {key.span()=>
                    if !::oath::__private::len_in(::oath::__private::text(value).chars().count(), #lo, #hi) { #fail; }
                }
            }
            RuleKind::Prefix(prefix) => quote_spanned! {key.span()=>
                if !::oath::__private::text(value).starts_with(#prefix) { #fail; }
            },
            RuleKind::Chars(patterns) => quote_spanned! {key.span()=>
                if !::oath::__private::text(value).chars().all(|c| matches!(c, #( #patterns )|*)) { #fail; }
            },
            RuleKind::Check(path) => quote_spanned! {path.span()=>
                let check: fn(&#inner) -> bool = #path;
                if !check(value) { #fail; }
            },
        }
    });
    let equality = if float {
        quote! {
            impl ::core::cmp::PartialEq for #name {
                fn eq(&self, other: &Self) -> bool {
                    self.0 == other.0
                }
            }
            impl ::core::cmp::Eq for #name {}
            impl ::core::hash::Hash for #name {
                fn hash<H: ::core::hash::Hasher>(&self, state: &mut H) {
                    // -0.0 == 0.0, so both hash as 0.0.
                    let value = if self.0 == 0.0 { 0.0 } else { self.0 };
                    value.to_bits().hash(state);
                }
            }
        }
    } else {
        quote!()
    };
    let derives = if float {
        quote!(::core::fmt::Debug, ::core::clone::Clone)
    } else {
        quote!(::core::fmt::Debug, ::core::clone::Clone, ::core::cmp::PartialEq, ::core::cmp::Eq, ::core::hash::Hash)
    };
    let serde = quote!(::oath::__private::serde);
    let id = if is_id { quote!(impl ::oath::Id for #name {}) } else { quote!() };
    Ok(quote! {
        #vis use #module::#name;

        #[doc(hidden)]
        #[allow(non_snake_case)]
        mod #module {
            #[allow(unused_imports)]
            use super::*;

            #[derive(#derives)]
            #item
            #equality

            #[allow(dead_code)]
            impl #name {
                /// The value, if it keeps every rule; otherwise the first rule it breaks.
                pub fn new(
                    value: impl ::core::convert::Into<#inner>,
                ) -> ::core::result::Result<Self, ::oath::Invalid> {
                    let value = value.into();
                    Self::__oath_check(&value)?;
                    ::core::result::Result::Ok(Self(value))
                }

                /// The inner value.
                pub fn get(&self) -> &#inner {
                    &self.0
                }

                fn __oath_check(value: &#inner) -> ::core::result::Result<(), ::oath::Invalid> {
                    #finite
                    #( #checks )*
                    ::core::result::Result::Ok(())
                }
            }

            impl #serde::Serialize for #name {
                fn serialize<S: #serde::Serializer>(
                    &self,
                    serializer: S,
                ) -> ::core::result::Result<S::Ok, S::Error> {
                    #serde::Serialize::serialize(&self.0, serializer)
                }
            }

            // Deserializing runs the same rules as `new`.
            impl<'de> #serde::Deserialize<'de> for #name {
                fn deserialize<D: #serde::Deserializer<'de>>(
                    deserializer: D,
                ) -> ::core::result::Result<Self, D::Error> {
                    let value = <#inner as #serde::Deserialize<'de>>::deserialize(deserializer)?;
                    Self::new(value).map_err(<D::Error as #serde::de::Error>::custom)
                }
            }

            #id
        }
    })
}

// ---------------------------------------------------------------------------------------------

/// The enum of one aggregate's commands or events: `Name(Name)` for each struct in `names`.
fn grouping_enum(
    vis: &Visibility,
    name: &Ident,
    names: &[Ident],
    kind: Kind,
    extra: TokenStream2,
) -> TokenStream2 {
    let trait_path = kind.trait_path();
    let doc = format!("The {}s of one aggregate. Defined by `#[oath::aggregate]`.", kind.noun());
    // Each variant goes over the wire under its struct's stable name, which only the struct's
    // `Named` impl knows, so the serde impls are written out rather than derived.
    let pairs = names.iter().enumerate().flat_map(|(i, a)| names[i + 1..].iter().map(move |b| (a, b)));
    let distinct = pairs.map(|(a, b)| {
        let message = format!("`{a}` and `{b}` have the same name in {name}; give one `name = \"...\"`");
        quote! {
            const _: () = ::core::assert!(
                !::oath::__private::same_name(
                    <#a as ::oath::__private::Named>::NAME,
                    <#b as ::oath::__private::Named>::NAME,
                ),
                #message,
            );
        }
    });
    let serde = quote!(::oath::__private::serde);
    quote! {
        #[doc = #doc]
        #[derive(::core::fmt::Debug, ::core::clone::Clone, ::core::cmp::PartialEq)]
        #vis enum #name { #( #names(#names), )* }
        #( #distinct )*
        impl ::oath::__private::Names for #name {
            const NAMES: &'static [&'static str] = &[ #( <#names as ::oath::__private::Named>::NAME ),* ];
        }
        impl #serde::Serialize for #name {
            fn serialize<S: #serde::Serializer>(
                &self,
                serializer: S,
            ) -> ::core::result::Result<S::Ok, S::Error> {
                match self {
                    #( Self::#names(inner) => ::oath::__private::serialize_tagged(
                        <#names as ::oath::__private::Named>::NAME, inner, serializer,
                    ), )*
                }
            }
        }
        impl<'de> #serde::Deserialize<'de> for #name {
            fn deserialize<D: #serde::Deserializer<'de>>(
                deserializer: D,
            ) -> ::core::result::Result<Self, D::Error> {
                let (kind, fields) = ::oath::__private::read_tagged(deserializer)?;
                #(
                    if kind == <#names as ::oath::__private::Named>::NAME {
                        return ::oath::__private::untagged(fields).map(Self::#names);
                    }
                )*
                ::core::result::Result::Err(<D::Error as #serde::de::Error>::unknown_variant(
                    &kind,
                    &[ #( <#names as ::oath::__private::Named>::NAME ),* ],
                ))
            }
        }
        impl #trait_path for #name {
            fn name(&self) -> &'static str {
                match self { #( Self::#names(inner) => <#names as #trait_path>::name(inner), )* }
            }
            #extra
        }
        #(
            impl ::core::convert::From<#names> for #name {
                fn from(inner: #names) -> Self { Self::#names(inner) }
            }
        )*
    }
}

/// Turns each bare variant `Name` into `Name(Name)` and returns the names.
fn wrap_variants(item: &mut ItemEnum, noun: &str) -> Result<Vec<Ident>> {
    if item.variants.is_empty() {
        return Err(Error::new_spanned(&item.ident, format!("list at least one {noun}")));
    }
    let mut names = Vec::new();
    for variant in &mut item.variants {
        if !matches!(variant.fields, Fields::Unit) || variant.discriminant.is_some() {
            return Err(Error::new_spanned(
                &*variant,
                format!(
                    "list each {noun} by its struct name alone, such as `{0},`; \
                     the macro expands it to `{0}({0})`",
                    variant.ident
                ),
            ));
        }
        let name = variant.ident.clone();
        variant.fields = Fields::Unnamed(parse_quote!((#name)));
        names.push(name);
    }
    Ok(names)
}

fn reject_standard_derives(attrs: &[Attribute], kind: Kind) -> Result<()> {
    for attr in attrs.iter().filter(|attr| attr.path().is_ident("derive")) {
        let paths = attr.parse_args_with(Punctuated::<Path, Token![,]>::parse_terminated)?;
        for path in paths {
            let last = path.segments.last().map(|s| s.ident.to_string()).unwrap_or_default();
            if STANDARD_DERIVES.contains(&last.as_str()) {
                return Err(Error::new_spanned(
                    path,
                    format!("{} already derives {}", kind.attribute(), STANDARD_DERIVES.join(", ")),
                ));
            }
        }
    }
    Ok(())
}

/// The field that carries a command's, event's, or rejection's name on the wire.
const TAG_FIELD: &str = "kind";

fn reject_tag_field(fields: &Fields) -> Result<()> {
    match fields.iter().find(|field| field.ident.as_ref().is_some_and(|ident| ident == TAG_FIELD)) {
        Some(field) => Err(Error::new_spanned(
            field,
            format!("`{TAG_FIELD}` is reserved: on the wire it holds the name of the type"),
        )),
        None => Ok(()),
    }
}

fn reject_generics(generics: &syn::Generics, attribute: &str) -> Result<()> {
    if generics.params.is_empty() && generics.where_clause.is_none() {
        Ok(())
    } else {
        Err(Error::new_spanned(generics, format!("{attribute} does not support generics on enums")))
    }
}

// ---------------------------------------------------------------------------------------------
// #[oath::rejection]

fn rejection_impl(args: TokenStream2, item: TokenStream2) -> Result<TokenStream2> {
    let kind = Kind::Rejection;
    if !args.is_empty() {
        return Err(Error::new_spanned(args, format!("{} takes no arguments", kind.attribute())));
    }
    let mut item: ItemEnum = syn::parse2(item).map_err(|error| {
        Error::new(error.span(), "#[oath::rejection] goes on an enum of rejection reasons")
    })?;
    reject_standard_derives(&item.attrs, kind)?;
    reject_generics(&item.generics, kind.attribute())?;
    if let Some(variant) = item.variants.iter().find(|variant| variant.ident == "NotAllowed") {
        return Err(Error::new_spanned(
            &variant.ident,
            "#[oath::rejection] adds the NotAllowed variant itself",
        ));
    }
    if let Some(variant) =
        item.variants.iter().find(|variant| matches!(variant.fields, Fields::Unnamed(_)))
    {
        return Err(Error::new_spanned(
            &variant.fields,
            "a rejection goes over the wire as a tagged struct, so give its fields names",
        ));
    }
    for variant in &item.variants {
        reject_tag_field(&variant.fields)?;
    }
    item.variants.push(parse_quote!(NotAllowed(::oath::NotAllowed)));

    let name = &item.ident;
    let reasons: Vec<_> = item.variants.iter().map(|variant| variant.ident.clone()).collect();
    let reason_strs: Vec<_> =
        reasons.iter().map(|ident| LitStr::new(&ident.to_string(), ident.span())).collect();
    let standard = standard_derives();
    Ok(quote! {
        #standard
        #[serde(tag = #TAG_FIELD)]
        #item
        impl ::oath::Rejection for #name {
            fn name(&self) -> &'static str {
                match self { #( Self::#reasons { .. } => #reason_strs, )* }
            }
        }
        impl ::core::convert::From<::oath::NotAllowed> for #name {
            fn from(not_allowed: ::oath::NotAllowed) -> Self { Self::NotAllowed(not_allowed) }
        }
    })
}

// ---------------------------------------------------------------------------------------------
// #[oath::aggregate]

struct AggregateArgs {
    id: Type,
    command: Ident,
    event: Ident,
    rejection: Type,
}

impl Parse for AggregateArgs {
    fn parse(input: ParseStream) -> Result<Self> {
        let (mut id, mut command, mut event, mut rejection) = (None, None, None, None);
        while !input.is_empty() {
            let key: Ident = input.parse()?;
            input.parse::<Token![=]>()?;
            match key.to_string().as_str() {
                "id" if id.is_none() => id = Some(input.parse()?),
                "command" if command.is_none() => command = Some(enum_name(input)?),
                "event" if event.is_none() => event = Some(enum_name(input)?),
                "rejection" if rejection.is_none() => rejection = Some(input.parse()?),
                "id" | "command" | "event" | "rejection" => {
                    return Err(Error::new_spanned(key, "given twice"));
                }
                _ => {
                    return Err(Error::new_spanned(
                        key,
                        "expected `id = ...`, `command = ...`, `event = ...`, or `rejection = ...`",
                    ));
                }
            }
            if !input.is_empty() {
                input.parse::<Token![,]>()?;
            }
        }
        let missing = |what: &str| {
            Error::new(
                Span::call_site(),
                format!("#[oath::aggregate] needs `{what} = ...`; expected \
                         #[oath::aggregate(id = ..., command = ..., event = ..., rejection = ...)]"),
            )
        };
        Ok(AggregateArgs {
            id: id.ok_or_else(|| missing("id"))?,
            command: command.ok_or_else(|| missing("command"))?,
            event: event.ok_or_else(|| missing("event"))?,
            rejection: rejection.ok_or_else(|| missing("rejection"))?,
        })
    }
}

/// The same visibility as `vis`, written for an item one module further down.
fn vis_one_level_down(vis: &Visibility) -> TokenStream2 {
    match vis {
        Visibility::Inherited => quote!(pub(super)),
        Visibility::Restricted(restricted) => {
            let path = &restricted.path;
            match path.segments.first().map(|segment| segment.ident.to_string()).as_deref() {
                Some("crate") => quote!(#vis),
                Some("self") => {
                    let rest = path.segments.iter().skip(1);
                    quote!(pub(in super #( :: #rest )*))
                }
                _ => quote!(pub(in super :: #path)),
            }
        }
        Visibility::Public(_) => quote!(#vis),
    }
}

/// The name of an enum `#[oath::aggregate]` defines: a plain name, not a path.
fn enum_name(input: ParseStream) -> Result<Ident> {
    let path: Path = input.parse()?;
    path.get_ident().cloned().ok_or_else(|| {
        Error::new_spanned(path, "name the enum to define here, such as `OrderCommand`, not a path")
    })
}

#[derive(Default)]
struct StateSpec {
    accepts: Vec<Ident>,
    on: Vec<(Ident, Ident)>,
}

impl Parse for StateSpec {
    fn parse(input: ParseStream) -> Result<Self> {
        let mut spec = StateSpec::default();
        let (mut seen_accepts, mut seen_on) = (false, false);
        while !input.is_empty() {
            let key: Ident = input.parse()?;
            let content;
            parenthesized!(content in input);
            match key.to_string().as_str() {
                "accepts" if !seen_accepts => {
                    seen_accepts = true;
                    for command in Punctuated::<Ident, Token![,]>::parse_terminated(&content)? {
                        if spec.accepts.contains(&command) {
                            return Err(Error::new_spanned(command, "command listed twice"));
                        }
                        spec.accepts.push(command);
                    }
                }
                "on" if !seen_on => {
                    seen_on = true;
                    while !content.is_empty() {
                        let event: Ident = content.parse()?;
                        content.parse::<Token![=>]>()?;
                        let to: Ident = content.parse()?;
                        if spec.on.iter().any(|(seen, _)| *seen == event) {
                            return Err(Error::new_spanned(event, "event listed twice"));
                        }
                        spec.on.push((event, to));
                        if !content.is_empty() {
                            content.parse::<Token![,]>()?;
                        }
                    }
                }
                "accepts" | "on" => return Err(Error::new_spanned(key, "given twice")),
                _ => {
                    return Err(Error::new_spanned(
                        key,
                        "expected `accepts(Command, ...)` or `on(Event => State, ...)`",
                    ));
                }
            }
            if !input.is_empty() {
                input.parse::<Token![,]>()?;
            }
        }
        Ok(spec)
    }
}

fn aggregate_impl(args: TokenStream2, item: TokenStream2) -> Result<TokenStream2> {
    let AggregateArgs { id, command, event, rejection } = syn::parse2(args)?;
    let mut item: ItemEnum = syn::parse2(item).map_err(|error| {
        Error::new(error.span(), "#[oath::aggregate] goes on an enum of state names")
    })?;
    reject_generics(&item.generics, "#[oath::aggregate]")?;

    let mut initial: Option<Ident> = None;
    let mut specs = Vec::new();
    for variant in &mut item.variants {
        let mut spec: Option<StateSpec> = None;
        let mut attrs = Vec::new();
        for attr in variant.attrs.drain(..) {
            if attr.path().is_ident("initial") {
                attr.meta.require_path_only()?;
                if let Some(first) = &initial {
                    return Err(Error::new_spanned(
                        attr,
                        format!("only one state can be #[initial]; {first} already is"),
                    ));
                }
                initial = Some(variant.ident.clone());
            } else if attr.path().is_ident("state") {
                if spec.is_some() {
                    return Err(Error::new_spanned(attr, "a state takes one #[state(...)]"));
                }
                spec = Some(attr.parse_args()?);
            } else {
                attrs.push(attr);
            }
        }
        variant.attrs = attrs;
        specs.push(spec.unwrap_or_default());
    }
    let states = wrap_variants(&mut item, "state")?;
    let Some(initial) = initial else {
        return Err(Error::new_spanned(&item.ident, "mark one state #[initial]"));
    };
    for spec in &specs {
        for (_, to) in &spec.on {
            if !states.contains(to) {
                return Err(Error::new_spanned(to, format!("`{to}` is not a state of {}", item.ident)));
            }
        }
    }
    // Every state must be reachable from the initial state through the transitions; a state
    // no history can reach is dead code that reads like part of the lifecycle.
    let mut reached = vec![initial.clone()];
    let mut next = 0;
    while next < reached.len() {
        let from = states.iter().position(|state| *state == reached[next]).expect("a state");
        for (_, to) in &specs[from].on {
            if !reached.contains(to) {
                reached.push(to.clone());
            }
        }
        next += 1;
    }
    if let Some(unreached) = states.iter().find(|state| !reached.contains(state)) {
        return Err(Error::new_spanned(
            unreached,
            format!(
                "no history reaches `{unreached}` from the initial state `{initial}`; add a transition \
                 to it in another state's #[state(on(...))], or remove it"
            ),
        ));
    }

    // Each command and event appears once, in the order the states first list it.
    let (mut commands, mut events) = (Vec::<Ident>::new(), Vec::<Ident>::new());
    for spec in &specs {
        for accepted in &spec.accepts {
            if !commands.contains(accepted) {
                commands.push(accepted.clone());
            }
        }
        for (on, _) in &spec.on {
            if !events.contains(on) {
                events.push(on.clone());
            }
        }
    }
    if commands.is_empty() {
        return Err(Error::new_spanned(&item.ident, "no state accepts a command; list one in #[state(accepts(...))]"));
    }
    if events.is_empty() {
        return Err(Error::new_spanned(&item.ident, "no state has a transition; list one in #[state(on(Event => State))]"));
    }
    // Each arm carries its command's span, so a command whose #[id] has another aggregate's
    // id type is reported at that command in `accepts(...)`.
    let target_arms = commands.iter().map(|command| {
        quote_spanned! {command.span()=>
            Self::#command(inner) => {
                let target: &#id = ::oath::Command::target(inner);
                target
            }
        }
    });
    let target = quote! {
        type Id = #id;
        fn target(&self) -> &#id {
            match self { #( #target_arms )* }
        }
    };
    let command_enum = grouping_enum(&item.vis, &command, &commands, Kind::Command, target);
    let event_enum = grouping_enum(&item.vis, &event, &events, Kind::Event, TokenStream2::new());
    // Each command and event belongs to this aggregate alone. A second aggregate that lists
    // the same struct implements the trait again, which doesn't compile.
    // The initial state means no event has happened yet, so it holds no data: any data it
    // had would come from no event. It is built as `Initial {}`, which has no fields to fill.
    let initial_value = quote_spanned! {initial.span()=> #initial {} };
    let empty_initial = {
        let message = format!(
            "the initial state `{initial}` must hold no data, since no event has happened yet; \
             write `struct {initial};` and let the first event bring the data into the next state"
        );
        quote_spanned! {initial.span()=>
            const _: () = ::core::assert!(::core::mem::size_of::<#initial>() == 0, #message);
        }
    };
    let aggregate_name = item.ident.clone();
    let owned = commands.iter().chain(&events).map(|owned| {
        quote_spanned! {owned.span()=>
            impl ::oath::BelongsToAggregate for #owned { type Aggregate = #aggregate_name; }
        }
    });
    let owned = quote!( #( #owned )* );

    // The aggregate is a struct with a private field holding the state enum, both defined in a
    // private module, so no code outside the generated impls can build a state without history.
    let name = item.ident.clone();
    let vis = std::mem::replace(&mut item.vis, Visibility::Inherited);
    let inner_vis = vis_one_level_down(&vis);
    let inner = format_ident!("__{}State", name);
    let module = format_ident!("__oath_{}", name);
    let (id_alias, rejection_alias) = (format_ident!("__{}Id", name), format_ident!("__{}Rejection", name));
    item.ident = inner.clone();
    let struct_attrs = std::mem::take(&mut item.attrs);
    item.attrs = struct_attrs.iter().filter(|attr| !attr.path().is_ident("doc")).cloned().collect();
    // A final state's data is read by no generated code, only by the user's own impls.
    item.attrs.push(parse_quote!(#[allow(dead_code)]));
    let lit = |ident: &Ident| LitStr::new(&ident.to_string(), ident.span());
    let state_strs: Vec<_> = states.iter().map(lit).collect();

    let mut decide_arms = Vec::new();
    let mut evolve_arms = Vec::new();
    let mut edges = Vec::new();
    // The transition table as marker impls. `Handle` and `Transition` require them, so an impl
    // the table doesn't list doesn't compile, and `emit`/`then` check each event against them.
    let mut markers = Vec::new();
    for (state, spec) in states.iter().zip(&specs) {
        for accepted in &spec.accepts {
            decide_arms.push(quote! {
                (#inner::#state(state), #command::#accepted(command)) =>
                    <#state as ::oath::Handle<#accepted>>::decide(state, command)
                        .map(::oath::Events::into_vec),
            });
            markers.push(quote! { impl ::oath::Accepts<#accepted> for #state {} });
        }
        if !spec.on.is_empty() {
            markers.push(quote! {
                impl #state {
                    /// Starts the events this state records. Compiles only when this state has
                    /// a transition for `E`.
                    #[allow(dead_code)]
                    pub(crate) fn emit<E>(
                        &self,
                        event: E,
                    ) -> ::oath::Emitted<Self, <Self as ::oath::On<E>>::To>
                    where
                        Self: ::oath::On<E>,
                        E: ::core::convert::Into<::oath::EventOf<Self>>,
                    {
                        ::oath::__private::emit(event)
                    }
                }
            });
        }
        for (on, to) in &spec.on {
            markers.push(quote! { impl ::oath::On<#on> for #state { type To = #to; } });
            evolve_arms.push(quote! {
                (#inner::#state(state), #event::#on(event)) =>
                    Self(#inner::#to(<#state as ::oath::Transition<#on>>::evolve(state, event))),
            });
            let (from, to) = (lit(state), lit(to));
            edges.push(quote! {
                ::oath::Edge { from: #from, event: <#on as ::oath::__private::Named>::NAME, to: #to }
            });
        }
    }

    Ok(quote! {
        #command_enum
        #event_enum

        #[allow(non_camel_case_types)]
        type #id_alias = #id;
        #[allow(non_camel_case_types)]
        type #rejection_alias = #rejection;

        #vis use #module::#name;
        #owned
        #empty_initial

        #[doc(hidden)]
        #[allow(non_snake_case)]
        mod #module {
            use super::*;

            #( #struct_attrs )*
            #inner_vis struct #name(#inner);

            #item

            impl ::oath::__private::Sealed for #name {}

            impl ::core::default::Default for #name {
                fn default() -> Self {
                    Self(#inner::#initial(#initial_value))
                }
            }

            impl ::oath::Aggregate for #name {
                type Id = #id_alias;
                type Command = #command;
                type Event = #event;
                type Rejection = #rejection_alias;

                const TRANSITIONS: &'static [::oath::Edge] = &[ #( #edges, )* ];

                fn state(&self) -> &'static str {
                    match &self.0 { #( #inner::#states(_) => #state_strs, )* }
                }

                #[allow(unreachable_patterns)]
                fn decide(
                    &self,
                    command: &#command,
                ) -> ::core::result::Result<::std::vec::Vec<#event>, #rejection_alias> {
                    match (&self.0, command) {
                        #( #decide_arms )*
                        (_, command) => ::core::result::Result::Err(::core::convert::From::from(
                            ::oath::NotAllowed {
                                state: ::std::string::String::from(::oath::Aggregate::state(self)),
                                command: ::std::string::String::from(::oath::Command::name(command)),
                            },
                        )),
                    }
                }

                #[allow(unreachable_patterns)]
                fn evolve(self, event: &#event) -> Self {
                    let from = ::oath::Aggregate::state(&self);
                    match (self.0, event) {
                        #( #evolve_arms )*
                        (_, event) => ::core::panic!(
                            "event {} has no transition from state {}; the history and the model disagree",
                            ::oath::Event::name(event),
                            from,
                        ),
                    }
                }
            }
        }

        #( impl ::oath::State for #states { type Aggregate = #name; } )*
        #( #markers )*

    })
}

// ---------------------------------------------------------------------------------------------
// #[oath::context]

struct ContextArgs {
    command: Ident,
    event: Ident,
    rejection: Ident,
}

impl Parse for ContextArgs {
    fn parse(input: ParseStream) -> Result<Self> {
        let (mut command, mut event, mut rejection) = (None, None, None);
        while !input.is_empty() {
            let key: Ident = input.parse()?;
            input.parse::<Token![=]>()?;
            let slot = match key.to_string().as_str() {
                "command" => &mut command,
                "event" => &mut event,
                "rejection" => &mut rejection,
                _ => {
                    return Err(Error::new_spanned(
                        key,
                        "expected `command = ...`, `event = ...`, or `rejection = ...`",
                    ));
                }
            };
            if slot.is_some() {
                return Err(Error::new_spanned(key, "given twice"));
            }
            *slot = Some(enum_name(input)?);
            if !input.is_empty() {
                input.parse::<Token![,]>()?;
            }
        }
        let missing = |what: &str| {
            Error::new(
                Span::call_site(),
                format!("#[oath::context] needs `{what} = ...`; expected \
                         #[oath::context(command = ..., event = ..., rejection = ...)]"),
            )
        };
        Ok(ContextArgs {
            command: command.ok_or_else(|| missing("command"))?,
            event: event.ok_or_else(|| missing("event"))?,
            rejection: rejection.ok_or_else(|| missing("rejection"))?,
        })
    }
}

fn context_impl(args: TokenStream2, item: TokenStream2) -> Result<TokenStream2> {
    let ContextArgs { command, event, rejection } = syn::parse2(args)?;
    let mut item: ItemEnum = syn::parse2(item).map_err(|error| {
        Error::new(error.span(), "#[oath::context] goes on an enum of aggregate names")
    })?;
    reject_generics(&item.generics, "#[oath::context]")?;
    if let Some(attr) = item.variants.iter().flat_map(|variant| &variant.attrs).next() {
        return Err(Error::new_spanned(attr, "list each aggregate by its name alone"));
    }
    let aggregates = wrap_variants(&mut item, "aggregate")?;
    for (i, aggregate) in aggregates.iter().enumerate() {
        if aggregates[..i].contains(aggregate) {
            return Err(Error::new_spanned(aggregate, "aggregate listed twice"));
        }
    }
    let name = item.ident.clone();
    let vis = item.vis.clone();
    let inner_vis = vis_one_level_down(&vis);
    let module = format_ident!("__oath_{}", name);
    let serde = quote!(::oath::__private::serde);
    let commands_of = |aggregate: &Ident| quote!(<#aggregate as ::oath::Aggregate>::Command);
    let command_types: Vec<_> = aggregates.iter().map(commands_of).collect();
    let event_types = aggregates.iter().map(|a| quote!(<#a as ::oath::Aggregate>::Event));
    let rejection_types = aggregates.iter().map(|a| quote!(<#a as ::oath::Aggregate>::Rejection));
    let store_trait = format_ident!("__{}Store", name);
    let stores = quote!( #( ::oath::Store<#aggregates> + )* ::core::marker::Send + ::core::marker::Sync );
    let new_doc = format!(
        "A {name} that keeps the events of its aggregates in `store`, which implements \
         [`Store`](::oath::Store) for each of them."
    );

    // A command reaches the context by its name alone, so no two aggregates' commands share one.
    let pairs = aggregates.iter().enumerate().flat_map(|(i, a)| aggregates[i + 1..].iter().map(move |b| (a, b)));
    let distinct = pairs.map(|(a, b)| {
        let message = format!(
            "`{a}` and `{b}` both have a command with the same name, and {name} routes commands by \
             name; give one `name = \"...\"`"
        );
        let (a_commands, b_commands) = (commands_of(a), commands_of(b));
        quote_spanned! {b.span()=>
            const _: () = ::core::assert!(
                ::oath::__private::disjoint(
                    <#a_commands as ::oath::__private::Names>::NAMES,
                    <#b_commands as ::oath::__private::Names>::NAMES,
                ),
                #message,
            );
        }
    });
    let unknown = format!("no aggregate in {name} has a command named `{{}}`");
    let (command_doc, event_doc, rejection_doc) = (
        format!("The commands of every aggregate in {name}. Defined by `#[oath::context]`."),
        format!("The events of every aggregate in {name}. Defined by `#[oath::context]`."),
        format!("The rejections of every aggregate in {name}. Defined by `#[oath::context]`."),
    );
    let struct_attrs = &item.attrs;

    Ok(quote! {
        #[doc = #command_doc]
        #[derive(::core::fmt::Debug, ::core::clone::Clone, ::core::cmp::PartialEq)]
        #vis enum #command { #( #aggregates(#command_types), )* }
        #( #distinct )*
        #(
            impl ::core::convert::From<#command_types> for #command {
                fn from(inner: #command_types) -> Self { Self::#aggregates(inner) }
            }
        )*
        impl<C> ::core::convert::From<C> for #command
        where
            C: ::oath::BelongsToAggregate
                + ::core::convert::Into<<C::Aggregate as ::oath::Aggregate>::Command>,
            C::Aggregate: ::oath::BelongsToContext<Context = #name>,
            #command: ::core::convert::From<<C::Aggregate as ::oath::Aggregate>::Command>,
        {
            fn from(inner: C) -> Self {
                let inner: <C::Aggregate as ::oath::Aggregate>::Command = inner.into();
                ::core::convert::From::from(inner)
            }
        }
        // On the wire a command is its aggregate's command, unchanged: its name routes it.
        impl #serde::Serialize for #command {
            fn serialize<S: #serde::Serializer>(
                &self,
                serializer: S,
            ) -> ::core::result::Result<S::Ok, S::Error> {
                match self { #( Self::#aggregates(inner) => #serde::Serialize::serialize(inner, serializer), )* }
            }
        }
        impl<'de> #serde::Deserialize<'de> for #command {
            fn deserialize<D: #serde::Deserializer<'de>>(
                deserializer: D,
            ) -> ::core::result::Result<Self, D::Error> {
                let (kind, fields) = ::oath::__private::read_tagged(deserializer)?;
                #(
                    if <#command_types as ::oath::__private::Names>::NAMES.contains(&kind.as_str()) {
                        return ::oath::__private::retagged(kind, fields).map(Self::#aggregates);
                    }
                )*
                ::core::result::Result::Err(<D::Error as #serde::de::Error>::custom(
                    ::std::format!(#unknown, kind),
                ))
            }
        }

        #[doc = #event_doc]
        #[derive(
            ::core::fmt::Debug,
            ::core::clone::Clone,
            ::core::cmp::PartialEq,
            #serde::Serialize,
            #serde::Deserialize,
        )]
        #[serde(crate = "::oath::__private::serde")]
        #vis enum #event { #( #aggregates(#event_types), )* }

        #[doc = #rejection_doc]
        #[derive(
            ::core::fmt::Debug,
            ::core::clone::Clone,
            ::core::cmp::PartialEq,
            #serde::Serialize,
            #serde::Deserialize,
        )]
        #[serde(crate = "::oath::__private::serde")]
        #vis enum #rejection { #( #aggregates(#rejection_types), )* }

        #vis use #module::#name;

        // The context is a struct with a private field holding its store, defined in a private
        // module, so no code but `send` can append events.
        #[doc(hidden)]
        #[allow(non_snake_case)]
        mod #module {
            use super::*;

            trait #store_trait: #stores {}
            impl<S: #stores> #store_trait for S {}

            #( #struct_attrs )*
            #inner_vis struct #name(::std::boxed::Box<dyn #store_trait>);

            impl #name {
                #[doc = #new_doc]
                pub fn new(store: impl #stores + 'static) -> Self {
                    Self(::std::boxed::Box::new(store))
                }
            }

            impl ::oath::__private::Sealed for #name {}

            impl ::oath::Context for #name {
                type Command = #command;
                type Event = #event;
                type Rejection = #rejection;

                fn send(
                    &mut self,
                    command: impl ::core::convert::Into<#command>,
                ) -> ::core::result::Result<::std::vec::Vec<#event>, #rejection> {
                    match command.into() {
                        #(
                            #command::#aggregates(command) =>
                                ::oath::__private::send::<#aggregates, _>(&mut *self.0, &command)
                                    .map(|events| events.into_iter().map(#event::#aggregates).collect())
                                    .map_err(#rejection::#aggregates),
                        )*
                    }
                }
            }

            #(
                impl ::oath::BelongsToContext for #aggregates {
                    type Context = #name;
                }
            )*
        }
    })
}
