# Oath

**A small, strict language for building applications that humans and agents can both understand.**

> **Status:** Oath is an idea in the design stage. There is no code yet. The full design lives in [IDEA.md](IDEA.md).

## The problem

Agents write code faster than anyone can review it. Writing code is no longer the bottleneck. Understanding it is.

General-purpose code lets an agent solve the same problem a dozen different ways. Business rules end up scattered, every feature brings its own patterns, and every review starts from zero. The result is slop: code that works today but that nobody fully understands, and that the next agent builds on anyway.

## The bet

Give humans and agents a small set of familiar primitives, one way to do each thing, and strict guardrails. A reviewer then reads a change in domain terms, such as a new command, a new event, or a new rule, instead of reading arbitrary code. "When an order is placed, request payment" is both the requirement and the code.

## How it works

Oath is a framework for domain-driven design, event sourcing, and state machines. An application is a model of its business domain, built from the vocabulary DDD teams already use:

- **Commands** request business actions.
- **Aggregates** decide whether to accept them. Each one is an explicit state machine.
- **Events** record what happened. They are the only way state changes, and history is never rewritten.
- **Policies** and **sagas** decide what happens next, from one-sentence rules to long-running processes with explicit failure handling.
- **Projections** turn events into the views people see.
- **Types** make invalid values impossible to express.

Supporting primitives cover access control, the outside world, bounded contexts, changes to old events, and live values that don't need to last. Each primitive has a narrow job and hard limits. The limits are the point.

## What you get

- **Multiplayer by default.** The same model runs on the client and the server. Clients update instantly, the server has the final say, and rollbacks are mechanical and never silent. Collaborative text merges instead of conflicting.
- **Runs anywhere.** The model knows nothing about databases, payment providers, or platforms. Each environment supplies its own, and tests can fake them all.
- **Verifiable.** Because the model is small and constrained, rules like "an order is never shipped before it's paid" can be checked across every reachable state, and every change is checked against the history that already exists.
- **Observable.** Every state change has a recorded cause. Watch the system live, ask why anything happened, and travel back in time. Humans and agents see the same thing, in the same words.
- **Authorization your way.** Roles, relationships, or custom roles that admins create at runtime are all ordinary domain data: auditable, replayable, and always current.

## What it isn't

Oath is not a general-purpose programming environment. It limits what you can express on purpose, because flexibility that makes review harder is a cost, not a feature.

## Learn more

[IDEA.md](IDEA.md) covers the principles, every primitive and its limits, and how Oath handles multiplayer, verification, observability, and change over time.
