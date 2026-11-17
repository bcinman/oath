# Verification

This document guides the design of Oath's language so that Oath models can be verified. It states what verification must give users and what the language must guarantee to make that possible. It does not prescribe how the checker or the engine is built.

[IDEA.md](IDEA.md) promises that rules like "an order is never shipped before it's paid" are checked across every reachable state, and that every change is checked against the history that already exists. This document says what the language needs to keep that promise without adding nuance to building applications.

## Two audiences

Verification serves two different things, and each has its own best fit:

- **Oath programs that users write** are checked exhaustively: every reachable state, not a sample of them. Oath models are small, pure, deterministic state machines whose choice points are all explicit, which is exactly the shape that can be explored completely.
- **The engine that runs those programs** is tested by deterministic simulation: whole systems of servers, clients, and storage run under injected failures, driven by a seed, so any failure can be replayed exactly. The engine is a distributed system, and its bugs live in the timing of messages, crashes, and retries.

Example-based tests, randomized testing, and formal specification still help in both, but in supporting roles described below.

## Oath programs

### One check, one verdict

Oath's goal is to remove nuance from building applications, and verification must not add it back. Checking a model has exactly two outcomes:

- **Pass.** Every rule holds in every reachable state, all recorded history still replays, and every process can always finish.
- **A counterexample.** A short history, in the model's own vocabulary, that breaks a rule. For example: an order is placed, payment is requested, payment times out, and shipping is accepted anyway.

Users never choose how much to check, never read different kinds of "pass," never pick a checking technique, and never annotate their model for the checker's benefit. They also never write proofs: a proof is not something a reviewer can read in domain terms.

When a model cannot be checked completely, that is reported like a compile error, naming the construct and what to write instead. It is never reported as a weaker pass.

### The restriction test

Keeping one complete verdict means the language must forbid some things. Every restriction must pass this test:

1. **Ban something only when it is hard to explain or review in domain terms.** Being hard for the checker is not, by itself, a reason.
2. **Every ban names what the user writes instead, in domain words.** A ban with no domain alternative is a gap in Oath, not a mistake in the user's model.
3. **When something is easy to explain but hard to check, the tool handles it.** Multiplying a price by a quantity is easy to explain, so the checker must cope with it.

Restrictions that pass this test:

| Restriction | What the user writes instead |
| --- | --- |
| Processes that may retry forever | "Try up to five times, then escalate to support." |
| Numbers without limits | A range for each kind of number, such as "a quantity is at most 10,000," with sensible defaults so it is rarely extra work. |
| Aggregates that affect each other directly | A saga or policy that names the process connecting them. |
| Decision logic written in a general-purpose language | The same logic in Oath, where it reads as domain rules. |

A restriction that fails it, and is ruled out:

| Restriction | Why it fails |
| --- | --- |
| Arithmetic that never multiplies or divides two varying values | It exists only for the checker, and it would ban everyday calculations in many domains: price × quantity, a dose per kilogram of body weight, a rate × a duration, scaling a recipe, and averages. |

### What the language must guarantee

- **All decision logic is in the language.** Deciding commands, applying events, access rules, policies, invariants, and upcasters are written in Oath, never in a general-purpose language. Logic the checker cannot see is logic it cannot verify.
- **Every choice point is explicit.** The places where the model allows more than one next step are named: which actor sends which command, what a port answers, when time passes, in what order concurrent commands arrive, and how far behind a view is when something reads it. Nothing else can vary.
- **Models are finite by construction.** Complete checking needs a finite number of states, and the language provides that without asking users to set limits:
  - Every number has a range.
  - Every process always ends. Each step of a long-running process states what happens when it fails or doesn't respond in time, and retries are capped.
  - Parts of the model interact only in ways that let a small number of instances stand for any number of them. This must be proven for Oath's final rules, not assumed.
- **There is one way to state a rule.** A rule is a statement that something is never or always true. The tool works out what kind of rule it is. Examples of specific cases ("given these events, when this command, then this outcome") are useful records, but they never stand in for a rule.
- **Every port answers the same way.** An answer is a success, a typed failure, or no answer in time. Duplicate answers are the engine's problem, not the model's.
- **Common arithmetic pitfalls are built in.** Operations that are easy to explain but easy to get wrong, such as rounding by a stated rule or dividing a number into parts that add up exactly, are provided by the language for any number type rather than written by hand. The language has no built-in domain concepts such as money; a domain that needs money defines it as a type, like any other domain value, and uses these operations.
- **History is part of the check.** A change fails if anything already recorded no longer reads, applies, or leaves the model in a valid state, including a process left partway through with no valid next step.
- **Forgettable data never decides anything.** No rule or decision may depend on a value that is marked as personal data, so history still replays after that data is forgotten.
- **Behavior never depends on where the model runs.** The same model and history produce the same result on every platform, so anything whose result can vary by platform, such as unordered iteration or imprecise number formats, is not part of the language.

### Where other techniques help

- **Examples of specific cases** document intended behavior and pin down outcomes that are not rules, such as which rejection reason a customer sees. Most should come from saving a counterexample or from real history.
- **Replaying recorded history** is folded into the single verdict rather than reported separately.
- **Checks on the model's structure** catch missing pieces before any exploration: a command without an access rule, a process step without a failure path, or an event shape that changed without an upcaster.
- **Sampling and other techniques** may be used inside the tool to reach the verdict faster, but they never produce a separate kind of result.

## The engine

The engine has to keep IDEA.md's promises about multiplayer and delivery: no event lost or handled twice, causes before effects, rollbacks that are mechanical and never silent, revocation with no window, offline clients that reconcile by the same rules as any other delay, and the same behavior on every platform. Each of these can fail only under particular timings of messages, crashes, and retries.

Guidance for the engine:

- **Test it by deterministic simulation from the start.** Run whole systems under injected failures, driven by a seed, and check the promises above after every run. This only works if everything that can vary, such as time, networking, storage, and randomness, reaches the engine through boundaries that a simulation can replace. That decision has to be made before the engine is written.
- **State every promise as something that can be checked.** Phrases such as "exactly once in effect," "no window," and "within a second or two" need precise meanings: what is observed, by whom, and under what conditions.
- **Keep a simple reference.** A small, obviously correct implementation of Oath's semantics serves as the standard the real engine is compared against.
- **Specify the hardest protocols before building them.** Sync, delivery, and revocation are cheapest to get right on paper.
- **Use randomized and cross-platform testing where input is untrusted or platforms differ.** This covers reading old or damaged events, messages between clients and servers, and checking that every platform makes identical decisions.
- **Share one simulation.** The simulation that tests the engine under network failures is the same one that checks a user's model under failing ports, so "your model is verified" and "the engine is tested" come from the same source.

## Open questions

- **Can a small number of instances provably stand for any number?** The single complete verdict depends on it.
- **What are the default ranges for each kind of number?** They decide how often users have to write a limit.
- **Must any real process retry forever?** Capped retries are what make "every process ends" a rule the language can enforce.
- **Which platforms must make identical decisions?** This decides how strict the language's rules about numbers and collections must be.
