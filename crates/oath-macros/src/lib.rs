//! Attribute macros for the `oath` crate. Use them through `oath`: `#[oath::command]`,
//! `#[oath::event]`, `#[oath::rejection]`, and `#[oath::aggregate(...)]`.

use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::{
    Attribute, Error, Fields, Ident, Item, ItemEnum, ItemStruct, LitStr, Path, Result, Token, Type,
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

fn message(args: TokenStream2, item: TokenStream2, kind: Kind) -> Result<TokenStream2> {
    if !args.is_empty() {
        return Err(Error::new_spanned(args, format!("{} takes no arguments", kind.attribute())));
    }
    match syn::parse2::<Item>(item)? {
        Item::Struct(item) => message_struct(item, kind),
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

fn message_struct(item: ItemStruct, kind: Kind) -> Result<TokenStream2> {
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
    let name = &item.ident;
    let name_str = LitStr::new(&name.to_string(), name.span());
    let trait_path = kind.trait_path();
    let (impl_generics, type_generics, where_clause) = item.generics.split_for_impl();
    let standard = standard_derives();
    Ok(quote! {
        #standard
        #item
        impl #impl_generics #trait_path for #name #type_generics #where_clause {
            fn name(&self) -> &'static str { #name_str }
        }
    })
}

/// The enum of one aggregate's commands or events: `Name(Name)` for each struct in `names`.
fn grouping_enum(vis: &Visibility, name: &Ident, names: &[Ident], kind: Kind) -> TokenStream2 {
    let trait_path = kind.trait_path();
    let standard = standard_derives();
    let doc = format!("The {}s of one aggregate. Defined by `#[oath::aggregate]`.", kind.noun());
    quote! {
        #[doc = #doc]
        #standard
        #[serde(tag = #TAG_FIELD)]
        #vis enum #name { #( #names(#names), )* }
        impl #trait_path for #name {
            fn name(&self) -> &'static str {
                match self { #( Self::#names(inner) => <#names as #trait_path>::name(inner), )* }
            }
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
    command: Ident,
    event: Ident,
    rejection: Type,
}

impl Parse for AggregateArgs {
    fn parse(input: ParseStream) -> Result<Self> {
        let (mut command, mut event, mut rejection) = (None, None, None);
        while !input.is_empty() {
            let key: Ident = input.parse()?;
            input.parse::<Token![=]>()?;
            match key.to_string().as_str() {
                "command" if command.is_none() => command = Some(enum_name(input)?),
                "event" if event.is_none() => event = Some(enum_name(input)?),
                "rejection" if rejection.is_none() => rejection = Some(input.parse()?),
                "command" | "event" | "rejection" => {
                    return Err(Error::new_spanned(key, "given twice"));
                }
                _ => {
                    return Err(Error::new_spanned(
                        key,
                        "expected `command = ...`, `event = ...`, or `rejection = ...`",
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
                         #[oath::aggregate(command = ..., event = ..., rejection = ...)]"),
            )
        };
        Ok(AggregateArgs {
            command: command.ok_or_else(|| missing("command"))?,
            event: event.ok_or_else(|| missing("event"))?,
            rejection: rejection.ok_or_else(|| missing("rejection"))?,
        })
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
    let AggregateArgs { command, event, rejection } = syn::parse2(args)?;
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
    let command_enum = grouping_enum(&item.vis, &command, &commands, Kind::Command);
    let event_enum = grouping_enum(&item.vis, &event, &events, Kind::Event);

    let name = &item.ident;
    let lit = |ident: &Ident| LitStr::new(&ident.to_string(), ident.span());
    let state_strs: Vec<_> = states.iter().map(lit).collect();

    let mut decide_arms = Vec::new();
    let mut evolve_arms = Vec::new();
    let mut edges = Vec::new();
    for (state, spec) in states.iter().zip(&specs) {
        for accepted in &spec.accepts {
            decide_arms.push(quote! {
                (Self::#state(state), #command::#accepted(command)) =>
                    <#state as ::oath::Handle<#accepted>>::decide(state, command),
            });
        }
        for (on, to) in &spec.on {
            evolve_arms.push(quote! {
                (Self::#state(state), #event::#on(event)) =>
                    Self::#to(<#state as ::oath::Transition<#on>>::evolve(state, event)),
            });
            let (from, on, to) = (lit(state), lit(on), lit(to));
            edges.push(quote! { ::oath::Edge { from: #from, event: #on, to: #to } });
        }
    }

    Ok(quote! {
        #item
        #command_enum
        #event_enum

        impl ::oath::__private::Sealed for #name {}

        impl ::core::default::Default for #name {
            fn default() -> Self {
                Self::#initial(<#initial as ::core::default::Default>::default())
            }
        }

        #( impl ::oath::State for #states { type Aggregate = #name; } )*

        impl ::oath::Aggregate for #name {
            type Command = #command;
            type Event = #event;
            type Rejection = #rejection;

            const TRANSITIONS: &'static [::oath::Edge] = &[ #( #edges, )* ];

            fn state(&self) -> &'static str {
                match self { #( Self::#states(_) => #state_strs, )* }
            }

            #[allow(unreachable_patterns)]
            fn decide(
                &self,
                command: &#command,
            ) -> ::core::result::Result<::std::vec::Vec<#event>, #rejection> {
                match (self, command) {
                    #( #decide_arms )*
                    (state, command) => ::core::result::Result::Err(::core::convert::From::from(
                        ::oath::NotAllowed {
                            state: ::std::string::String::from(::oath::Aggregate::state(state)),
                            command: ::std::string::String::from(::oath::Command::name(command)),
                        },
                    )),
                }
            }

            #[allow(unreachable_patterns)]
            fn evolve(self, event: &#event) -> Self {
                match (self, event) {
                    #( #evolve_arms )*
                    (state, event) => ::core::panic!(
                        "event {} has no transition from state {}; the history and the model disagree",
                        ::oath::Event::name(event),
                        ::oath::Aggregate::state(&state),
                    ),
                }
            }
        }
    })
}
