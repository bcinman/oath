# Oath

Oath is a framework for building applications strategically with domain-driven design, event sourcing, and state machines. It gives humans and agents a small, shared language to **model**, **verify**, **understand**, and **observe** an application.

An Oath application is a model of a business domain: the things that can be requested, the things that can happen, the rules that decide between them, and the views people need to see. The model is the source of truth. Everything else, including storage, networking, and third-party services, serves the model and can be swapped out.

## The Problem

Agents can write code extremely quickly. Writing code is no longer the bottleneck. Understanding and reviewing it is.

Today an agent can solve the same problem a dozen different ways, and in a typical codebase it usually does. Business rules end up scattered across handlers, services, hooks, and database triggers. State changes happen in places nobody thought to look. Every feature brings its own patterns, so every review starts from zero. The result is a large and growing "slop surface area": code that works today but that no human fully understands, and that the next agent will build on without understanding it either.

The problem is not that agents write bad code. The problem is that general-purpose code gives them too much freedom, and freedom is what makes code expensive to review.

## The Bet

If humans and agents share a small set of well-defined primitives, there is one way to do each thing, and the guardrails are strict, then:

- **Review gets faster**, because a reviewer reads a change in terms of the domain (a new command, a new event, a new rule) instead of reading arbitrary code.
- **Slop has nowhere to hide**, because each primitive has a narrow job and is forbidden from doing anyone else's.
- **Conversation gets precise**, because humans and agents discuss features in the same vocabulary the system is built from. "When an order is placed, request payment" is both a requirement and a description of the code.
- **Correctness becomes checkable**, because a model built from constrained primitives can be analyzed, simulated, and verified in ways that arbitrary code cannot.

The vocabulary is deliberately familiar. Commands, events, aggregates, and projections already appear in many DDD codebases, so neither humans nor agents need to learn new concepts, only a stricter discipline for the ones they know.

## Principles

1. **One way to do a thing.** Each kind of behavior has exactly one home. If something could be written two ways, Oath picks one.
2. **The model is the source of truth.** What the application does is fully described by its model. Anything outside the model is infrastructure.
3. **Events are the record of truth.** State is never changed directly. It is derived from an append-only history of things that happened. If it isn't in the history, it didn't happen.
4. **Invalid states are unrepresentable.** Types, state machines, and invariants make illegal values and illegal transitions impossible to express, not merely unlikely.
5. **Decisions are pure; effects live at the edges.** Deciding what should happen never touches the outside world. Talking to the outside world never makes business decisions. The same inputs always produce the same decisions.
6. **Everything is explicit.** Every state change has a named cause. Every reaction has a named trigger. Nothing happens implicitly.
7. **Built for both readers.** The model should be equally legible to a human skimming it and an agent reasoning about it. If a concept is hard to explain, it is modeled wrong.

## Primitives

Oath has seven core primitives and five supporting ones. The core primitives are the familiar building blocks of DDD and event sourcing, and they describe what a feature does. The supporting primitives handle what surrounds a feature: who may act, the outside world, the size of the domain, change over time, and moments that don't need to last.

**Core**

| Primitive | Responsibility |
| --- | --- |
| **Types** | Define the shape and constraints of domain values. |
| **Commands** | Request a specific business action. |
| **Events** | Record something meaningful that happened. |
| **Aggregates** | Enforce consistency by deciding which commands are accepted. |
| **Policies** | React to events by issuing commands. |
| **Sagas** | Coordinate long-running processes across aggregates and external systems. |
| **Projections** | Build views from events. |

**Supporting**

| Primitive | Responsibility |
| --- | --- |
| **Access** | Decide who may issue each command and see each piece of data. |
| **Ports** | Name the boundaries where the model meets the outside world. |
| **Contexts** | Group a coherent part of the model with its own language. |
| **Upcasters** | Read events recorded in an older shape as the current shape. |
| **Transients** | Share ephemeral values with subscribers without persisting them. |

### How they fit together

```
┌───────┐   ┌─────────┐   ┌────────┐   ┌───────────┐   ┌────────┐
│ Actor │──▶│ Command │──▶│ Access │──▶│ Aggregate │──▶│ Events │
└───────┘   └─────────┘   └────────┘   └───────────┘   └───┬────┘
                 ▲                                         │
                 │          ┌───────────────┐              │
                 └──────────│ Policy / Saga │◀─────────────┤
                            └───────┬───────┘              │
                                    ▼                      ▼
                               ┌─────────┐          ┌────────────┐
                               │  Ports  │          │ Projection │
                               └─────────┘          └─────┬──────┘
                                                          ▼
                                    ┌───────┐       ┌────────┐
                                    │ Actor │◀──────│ Access │
                                    └───────┘       └────────┘
```

An actor issues a command. Access rules decide whether that actor may ask, and the aggregate decides whether the request is valid, recording the outcome as events. Events flow to projections, which build views that access rules filter before any actor sees them. They also flow to policies and sagas, which decide what should happen next and issue more commands. Sagas reach the outside world through ports, and the outside world's answers come back as events.

Types define every value in this flow. Two supporting primitives sit in the flow: access guards both ends, and ports are where it meets the outside world. The rest shape the flow rather than sitting in it: contexts divide it into parts with their own language, upcasters let it read old events, and transients travel beside it without touching it.

Each primitive has a narrow job and explicit limits. The limits are the point.

## Core Primitives

### Types

Types give domain concepts explicit meaning. Money is not a number, an order ID is not a string, and an order status is not free text. Types carry their own constraints (a quantity is positive, an email is well-formed, a discount never exceeds the price) so that a value that exists is a value that is valid.

- Domain values are never raw primitives.
- A type's constraints are checked when a value is created, so the rest of the model never re-checks them.
- A type can be marked as personal data, such as a name, an email address, or a home address. The model then knows exactly what it may be asked to forget.
- A type can be mergeable, such as text, counters, and sets. Concurrent changes to a mergeable value combine instead of conflicting.

### Commands

A command is an explicit request to perform one business action, such as `PlaceOrder` or `CancelSubscription`. It is named in the imperative, carries everything needed to decide, and targets exactly one aggregate.

- A command may be rejected. Rejection is a normal, typed outcome, not an exception.
- A command changes nothing by itself. Only the events it produces do.
- Every command records its actor, so its origin and authority are always known.

### Events

An event records something meaningful that happened, such as `OrderPlaced` or `PaymentFailed`. It is named in the past tense and is a fact: it cannot be rejected, edited, or deleted. The only exception is forgetting personal data (see [Evolution](#evolution)), and even that is recorded as an event.

- Events are the only way state changes.
- Events describe the domain, not the database. `ItemAddedToCart`, not `CartRowUpdated`.
- Every event records what caused it, so any state can be traced back to the commands and events that produced it.
- No event is lost or handled twice. Every policy, saga, and projection that reacts to an event receives it exactly once in effect.
- Causes come before effects. No one sees an event before the events that caused it.

### Aggregates

An aggregate is the only consistency boundary. It owns a piece of state, enforces its invariants, and decides whether a command is accepted. Every aggregate is a state machine: its states and the transitions between them are explicit, and a command that is not valid in the current state cannot succeed.

- Deciding is pure: given the current state and a command, an aggregate returns events or a rejection, and does nothing else.
- An aggregate never reads another aggregate's state and never talks to the outside world.
- The application reads an aggregate's state only while deciding a command addressed to it, including the access check for that command. All other reads go through projections, so an aggregate can change shape without breaking any reader. Observability can still inspect it for debugging.
- Nothing spans aggregates atomically. There are no transactions or locks across aggregates. Keeping them consistent with each other is a process, run by a policy or saga.
- An aggregate's invariants hold after every event, always.

### Policies

A policy expresses a business rule about what should happen next: "when an order is placed, request payment." It listens for an event and issues a command.

- Policies are stateless. If a rule needs to remember anything between events, it is a saga.
- Policies only issue commands. They never talk to the outside world.
- A policy reads as a single sentence: *when X happens, do Y*.

### Sagas

A saga coordinates a long-running business process that spans several aggregates, external systems, or periods of time, such as fulfilling an order from payment through shipping. A saga is a state machine that tracks progress through the process.

- A saga reacts to events, issues commands, and calls ports. It is the only primitive allowed to wait on the outside world.
- Every step defines what happens when it fails or doesn't respond in time: retry, compensate, or escalate. A process with an unhandled failure is an incomplete model.
- Time is an explicit input. Deadlines and timeouts are events like any other, not hidden timers.
- Rules that span aggregates are upheld by sagas and proven by verification. "An order is never shipped before it is paid" holds because the fulfillment saga cannot reach *shipped* without passing through *paid*.

### Projections

A projection turns events into a view shaped for a particular question or interface, such as an order summary, an account balance, or a search index.

- Projections are the only way the application reads state outside of deciding a command.
- Projections only read events. They never issue commands or affect decisions.
- A projection can always be thrown away and rebuilt by replaying history.
- Many projections can exist over the same events, each shaped for its own purpose.

## Supporting Primitives

### Access

Access rules decide who may do what: whether an actor may issue a command, and whether an actor may see an event, a view, or a transient. Oath does not prescribe an authorization strategy. Roles, relationships, attributes, or a scheme unique to the business are all expressed the same way. An access rule is a pure decision about an actor and a request.

- Every command and every read has an actor, such as a user, a service, a policy, or a saga. When a policy or saga issues a command, it states whose authority it acts with: its own, or that of the actor whose event triggered it.
- Access is separate from domain logic. An access rule answers "may this actor ask?" and the aggregate answers "is this valid right now?" A denial and a rejection are different outcomes, and each one says why.
- Access rules are pure. They read only the model's own state, never the outside world, and the same inputs always produce the same decision.
- Nothing is allowed or visible by default. A command or view without an access rule is an incomplete model.
- Access is always current on the server. A grant or revocation applies to every command and every read that comes after it, with no window where revoked access still works.
- Authority is checked on every command, not once when a process starts. If a saga acts for someone whose access is revoked partway through, its next command is denied, and the saga handles that like any other failed step.

**Strategy is data.** The model defines the vocabulary of access: which commands and views exist, and how rules interpret grants. Who holds which grants is ordinary domain state. When an admin creates a custom role, assigns it to someone, or shares a document, those are commands and events like any other. They usually live in an access context of their own, and projections turn them into what the rules consult. Dynamic authorization therefore needs no special machinery and inherits everything else in Oath:

- **It is bounded by the model.** Admins can compose and assign access at runtime, but only from the commands and views the model defines. Configuration can never create a capability the model doesn't have.
- **It is auditable.** Every change in who may do what has a recorded actor and cause.
- **It travels in time.** "Who could see this invoice on March 3, and why?" is answered by the same history as everything else.
- **It is verifiable.** Invariants can cover access itself, such as "an organization always has at least one owner" or "no one can grant a permission they don't hold," and can be checked across every possible configuration, not just the one in production.

### Ports

A port is a named, typed boundary between the model and the outside world: a payment provider, an email sender, a clock, an LLM. The model declares what it needs from a port. The environment decides how to provide it.

- Ports are the only way the model touches the outside world.
- A port's results enter the model as events, so the outside world's answers become part of the history like everything else.
- Because every external dependency is a port, any of them can be replaced, faked, or simulated.

### Contexts

A context is a bounded part of the domain with its own consistent language, such as Ordering, Billing, or Shipping. The same word may mean different things in different contexts, and that is allowed.

- A context is a boundary of language and ownership, not of consistency. Crossing contexts carries the same guarantees as crossing aggregates, no more and no less.
- Contexts communicate only through events and commands, never by sharing state.
- A context is the natural unit of review and verification.

### Upcasters

An upcaster lets the model read an event recorded in an older shape as if it had the current shape. For example, when `OrderPlaced` gains a currency, an upcaster reads every earlier `OrderPlaced` as having been placed in the currency the business used at the time.

- Upcasters are the only way an event's shape changes. History itself is never edited; the stored event stays exactly as it was recorded.
- An upcaster changes shape, never meaning. If the meaning changes, it is a new event.
- Upcasters are pure, and each one covers a single step from one version of an event to the next. Reading a very old event runs the steps in order.
- Upcasters are part of the model, so they are reviewed and verified like everything else.

### Transients

A transient is an ephemeral value shared with subscribed clients but never persisted, such as tokens streaming from an agent, a cursor position, or "user is typing."

- Transients never affect authoritative state and never trigger commands.
- Losing a transient is always acceptable. If losing it would matter, it should be an event.

## Multiplayer

Multiplayer sync is a first-class concern, not an add-on. An Oath model runs on both the client and the server, much like a multiplayer game:

- **The client is optimistic.** It decides commands locally and applies the resulting events to its own views at once, so you always see your own changes immediately, without waiting on the network. It can also check access rules locally, for example to hide actions its user can't take.
- **The server is authoritative.** A client sends the server its commands, not the events it produced locally, and the server decides each one again against the authoritative state. The server's history is the truth, and only the server's decisions count.
- **The server always wins.** When the server rejects a command or orders events differently, the client rolls back its optimistic changes and replays from the authoritative history. Because decisions are deterministic, reconciling is mechanical rather than ad hoc. Most conflicts aren't conflicts at all. Two people adding items to the same cart both succeed. When two people buy the last ticket, the second is rejected, which is the correct business outcome, not a sync failure.
- **A rollback is never silent.** When an optimistic change is undone, the actor sees that it was undone and the typed reason why.
- **Mergeable values never conflict.** Concurrent edits to a mergeable value, such as collaborative text, combine on the server instead of being rejected. The server still decides and still wins; for these values, its decision includes everyone's edits.
- **Everyone else sees your changes within a second or two.** Other clients' views are eventually consistent, but only briefly: under normal conditions, a change recorded on the server reaches every connected client that may see it within a second or two. Lag is measured and observable, so a slow view is a visible fault, not a silent one.
- **Not every client sees everything.** Each client holds only the slice of the model that access rules let its actor see and that it needs. When access changes, the slice changes with it. After a revocation, the server sends the client nothing new and removes what it already holds.
- **Offline is a longer delay.** A client that loses its connection keeps working optimistically and reconciles when it returns, using the same rules as any other delay.

Transients ride alongside this flow for the moments that need to be live but not durable.

## Portability

The model is independent of where it runs. The same model, with the same behavior, should run in a browser, on a phone, on a server, or inside an agent's tooling.

What changes between environments is the implementation of each port. A web client might keep its durable state locally in the browser, while the authoritative server uses a production database and a real payment provider, and a test harness uses in-memory fakes. The model does not know or care which.

## Verifiability

The rules of the business must be checkable, not just tested.

- **Types** rule out invalid values.
- **State machines** in aggregates and sagas rule out invalid transitions and make every reachable state enumerable.
- **Invariants** state what must always be true, within an aggregate or across the whole system, such as "an order is never shipped before it is paid" or "an account balance never goes negative."
- **Access rules** make who may do what checkable for every possible configuration of grants.
- **Determinism** means the same history always produces the same state, so any scenario can be replayed exactly.
- **History** is a test suite that already exists. Every change to the model is checked against the events already recorded, not only against new scenarios.

Together these make the model a candidate for formal verification. Because the model is built from a small number of constrained primitives, it should be possible to explore its reachable states, check invariants against them, simulate sagas against failing or slow ports, and find counterexamples before they happen in production. Verification should be a normal part of reviewing a change, not a specialist activity.

## Observability

A running Oath application is observable in real time. Because every state change is an event with a recorded cause, the history *is* the trace.

- **See what's happening.** Watch commands, events, rejections, denials, and transients flow through the system as they happen, on the client and, when access rules allow, on the server.
- **Ask why.** Follow any state, rejection, or denial back through the chain of events and commands that produced it, across policies, sagas, and contexts.
- **Travel in time.** Rewind to any point in history, inspect the state of any aggregate, saga, or projection at that moment, and replay forward. Forgotten personal data stays forgotten.
- **Share the view.** Humans and agents observe the same system through the same vocabulary. An agent debugging a production issue sees what a human would see, and can explain it in the same terms.

## Evolution

Applications change, but the past is read-only. Events recorded years ago must still mean something today, and the model must always be able to read all of them. History is never migrated. Every change to the model falls into one of three groups:

- **Always safe.** Adding things: new types, commands, events, states, policies, sagas, and projections. Changing how future commands are decided. Rebuilding projections in any shape.
- **Needs an upcaster.** Changing an event's shape.
- **Forbidden.** Changing what a recorded event means, removing an event type that appears in history, or any change that would leave existing history in an invalid state, such as a saga partway through a process that no longer has a valid next step.

**Forgetting is an event.** Permanent history must coexist with laws that require erasing a person's data on request. When personal data must be forgotten, the facts stay and the personal data inside them is erased everywhere it appears: in events, in views, and on clients. The erasure itself is recorded as an event, with an actor and a cause like any other change.

- Only values whose types are marked as personal data can be forgotten. Everything else is permanent.
- History still replays after forgetting, because verification checks that no invariant or decision depends on a value that may be forgotten.

## Non-goals

- **Being a general-purpose programming environment.** Oath deliberately constrains what can be expressed. Flexibility that makes review harder is a cost, not a feature.
- **Hiding distributed systems.** Latency, failure, and disagreement are modeled explicitly, not abstracted away.
- **Owning every layer.** Rendering, styling, and infrastructure are outside the model. Oath defines what the application *is*, not how it looks or where it is hosted.
