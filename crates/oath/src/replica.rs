//! Multiplayer: the same context on a server and on every client, as in a game whose server
//! has the final say.
//!
//! - The server keeps the [`Context`] and its store, and is the only one that records events.
//!   Wrap it in a [`Host`], and give each connected client a [`Session`]. The context must
//!   implement [`Authorize`], so a server without authorization doesn't compile.
//! - A client keeps a [`Replica`]: the server's state of the instances it sent commands to,
//!   the rows of the views it watches, and its own commands the server hasn't answered yet.
//!   It runs no store, no view of the whole log, and no saga.
//!
//! A command sent to a replica is decided at once, against what the replica knows: that is
//! the prediction its views show. It is also sent to the server, which decides it again,
//! against its own state, and answers with an [`Update`]. The replica never keeps an event it
//! predicted, only the commands: whenever an update arrives, it decides its unanswered
//! commands again on top of the server's state. So a command the server confirms, one another
//! client's command overtook, and one the server rejects are handled the same way, and a view
//! never shows an effect twice.
//!
//! On the wire, [`ToServer`] and [`ToClient`] go through serde over one ordered connection,
//! such as a WebSocket. The replica and the session are sans-IO: the transport calls them.
//!
//! ```text
//! // client
//! let mut shop = Replica::<Shop>::new(client);
//! shop.send(PlaceOrder { order: order.clone() });       // predicted now
//! let status = shop.watch::<Status>(order.clone());
//! shop.view(&status).text;                              // "placed"
//! shop.connected();
//! for message in shop.outbox() { /* write it to the server */ }
//! let refused = shop.receive(message_from_server)?;     // confirm, rebase, roll back
//!
//! // server
//! let host = Host::new(Shop::new(store));               // Shop: Authorize
//! let mut session = host.session(user);
//! let reply = session.receive(message).await?;          // to this client
//! let update = session.refresh().await?;                // after other clients' commands
//! ```

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_value::Value;

use crate::{CommandId, Context, Id, Invalid, Position, SendError, StoreError, Version, View};

/// Names one client, such as one browser tab's or one phone's install. Make it once, from a
/// random UUID, and keep it with the replica: two clients with the same id would have their
/// commands recorded as each other's. 1 to 64 characters of `A-Z`, `a-z`, `0-9`, `_`, and `-`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
pub struct ClientId(String);

impl ClientId {
    /// The id, if it is 1 to 64 characters of `A-Z`, `a-z`, `0-9`, `_`, and `-`.
    pub fn new(text: impl Into<String>) -> Result<Self, Invalid> {
        let text = text.into();
        if !(1..=64).contains(&text.chars().count()) {
            return Err(Invalid { type_name: "ClientId", rule: "len = 1..=64" });
        }
        if !text.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
            return Err(Invalid { type_name: "ClientId", rule: "chars = \"A-Za-z0-9_-\"" });
        }
        Ok(ClientId(text))
    }

    /// The id as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The id of this client's command `seq`. A saga's ids have no `/`, so the two never meet.
    fn command(&self, seq: u64) -> CommandId {
        CommandId(format!("client/{}:{seq}", self.0))
    }
}

impl<'de> Deserialize<'de> for ClientId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        ClientId::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// An id a [`Replica`] can make for an instance it creates, before the server has seen it,
/// with [`Replica::fresh`]. Implemented by `#[oath::id]` for an id around a `String` whose only
/// rule is `prefix`, since a made-up id could break any other rule.
#[diagnostic::on_unimplemented(
    message = "a replica can't make a `{Self}`",
    label = "`{Self}` has rules a made-up id could break",
    note = "a replica makes ids only for #[oath::id] around a String whose only rule is `prefix`"
)]
pub trait Fresh: Id {
    #[doc(hidden)]
    fn fresh(suffix: &str) -> Self;
}

/// What a server checks before it decides a client's command or tells it anything. Implement
/// it for a context to serve it with a [`Host`].
#[diagnostic::on_unimplemented(
    message = "`{Self}` doesn't say who may send and read what",
    label = "`{Self}` needs `impl oath::replica::Authorize`",
    note = "a Host serves only a context that implements oath::replica::Authorize"
)]
pub trait Authorize: Context {
    /// Who a session acts for, such as the signed-in user's id.
    type Principal: Send + Sync + 'static;

    /// Whether `who` may send `command`. A command `who` may not send is refused with
    /// [`Refusal::Forbidden`] and never decided. A command that records who did it carries
    /// them as a field; check here that it is `who`.
    fn may_send(&self, who: &Self::Principal, command: &Self::Command) -> impl Future<Output = bool> + Send;

    /// Whether `who` may read `read`: the state of an instance, or a row of a view. A client
    /// is told nothing about what it may not read.
    fn may_read(&self, who: &Self::Principal, read: Read<'_, Self>) -> impl Future<Output = bool> + Send;
}

/// What a client asks to read, for [`Authorize::may_read`].
pub enum Read<'a, C: Context> {
    /// The state of an aggregate instance, which the client follows after sending it a command.
    Instance(&'a C::Instance),
    /// A row of a view, which the client watches.
    Row(&'a C::Row),
}

/// Why the server refused a client's command.
#[derive(Serialize, Deserialize)]
#[serde(bound = "")]
pub enum Refusal<C: Context> {
    /// The aggregate rejected it.
    Rejected(C::Rejection),
    /// [`Authorize::may_send`] refused it.
    Forbidden,
}

impl<C: Context> Clone for Refusal<C> {
    fn clone(&self) -> Self {
        match self {
            Refusal::Rejected(rejection) => Refusal::Rejected(rejection.clone()),
            Refusal::Forbidden => Refusal::Forbidden,
        }
    }
}

impl<C: Context> core::fmt::Debug for Refusal<C> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Refusal::Rejected(rejection) => f.debug_tuple("Rejected").field(rejection).finish(),
            Refusal::Forbidden => f.write_str("Forbidden"),
        }
    }
}

impl<C: Context> PartialEq for Refusal<C> {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Refusal::Rejected(a), Refusal::Rejected(b)) => a == b,
            (Refusal::Forbidden, Refusal::Forbidden) => true,
            _ => false,
        }
    }
}

/// A message from a client to the server.
#[derive(Serialize, Deserialize)]
#[serde(bound = "")]
pub enum ToServer<C: Context> {
    /// The first message on each connection: who the client is, what it already knows, and its
    /// unanswered commands, which the server decides in order.
    Hello {
        /// The client's [`Context::FINGERPRINT`].
        fingerprint: u64,
        /// The client.
        client: ClientId,
        /// The instances the client follows, with the version it knows.
        instances: Vec<(C::Instance, Version)>,
        /// The rows the client watches.
        rows: Vec<C::Row>,
        /// The client's unanswered commands, by number, in the order sent.
        pending: Vec<(u64, C::Command)>,
    },
    /// A command, by its number on this client.
    Send {
        /// The command's number on this client.
        seq: u64,
        /// The command.
        command: C::Command,
    },
    /// Starts watching a row.
    Watch(C::Row),
    /// Stops watching a row.
    Unwatch(C::Row),
}

/// A message from the server to a client.
#[derive(Serialize, Deserialize)]
#[serde(bound = "")]
pub enum ToClient<C: Context> {
    /// What changed for the client.
    Update(Update<C>),
    /// The client's model differs from the server's; the app must update.
    Outdated,
}

/// What changed for a client, as one step: the server's state of instances it follows and rows
/// it watches, and the commands the server answered, whose effects those include.
#[derive(Serialize, Deserialize)]
#[serde(bound = "")]
pub struct Update<C: Context> {
    instances: Vec<(C::Instance, Version, Option<Value>)>,
    rows: Vec<(C::Row, Value)>,
    done: Vec<u64>,
    refused: Vec<(u64, Refusal<C>)>,
}

impl<C: Context> Update<C> {
    fn is_empty(&self) -> bool {
        self.instances.is_empty() && self.rows.is_empty() && self.done.is_empty() && self.refused.is_empty()
    }
}

impl<C: Context> Default for Update<C> {
    fn default() -> Self {
        Update { instances: Vec::new(), rows: Vec::new(), done: Vec::new(), refused: Vec::new() }
    }
}

/// The server sent [`ToClient::Outdated`]: the client's model differs from the server's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Outdated;

impl core::fmt::Display for Outdated {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("the server runs another version of the model; update the app")
    }
}

impl std::error::Error for Outdated {}

/// A watched row of view `V`, from [`Replica::watch`]. Read it with [`Replica::view`].
#[must_use = "read the row with `Replica::view`, and stop with `Replica::unwatch`"]
pub struct Watched<V: View> {
    key: V::Key,
}

/// A client's copy of context `C`: the server's state of what it uses, and its own commands
/// the server hasn't answered, applied as predictions. See the [module](self).
///
/// It goes through serde, so the app can save it, such as to IndexedDB or a file, and keep its
/// unanswered commands across restarts. Reading a saved replica fails when it was saved by
/// another version of the model.
pub struct Replica<C: Context> {
    saved: Saved<C>,
    connected: bool,
    outbox: Vec<ToServer<C>>,
}

#[derive(Serialize, Deserialize)]
#[serde(bound = "")]
struct Saved<C: Context> {
    fingerprint: u64,
    client: ClientId,
    next: u64,
    #[serde(with = "pairs")]
    instances: HashMap<C::Instance, (Version, Option<Value>)>,
    #[serde(with = "pairs")]
    rows: HashMap<C::Row, Option<Value>>,
    pending: Vec<(u64, C::Command)>,
}

impl<C: Context> Serialize for Replica<C> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.saved.serialize(serializer)
    }
}

impl<'de, C: Context> Deserialize<'de> for Replica<C> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let saved = Saved::<C>::deserialize(deserializer)?;
        if saved.fingerprint != C::FINGERPRINT {
            return Err(serde::de::Error::custom("the replica was saved by another version of the model"));
        }
        Ok(Replica { saved, connected: false, outbox: Vec::new() })
    }
}

impl<C: Context> Replica<C> {
    /// A replica for `client` that knows nothing yet.
    pub fn new(client: ClientId) -> Self {
        let saved = Saved {
            fingerprint: C::FINGERPRINT,
            client,
            next: 1,
            instances: HashMap::new(),
            rows: HashMap::new(),
            pending: Vec::new(),
        };
        Replica { saved, connected: false, outbox: Vec::new() }
    }

    /// Sends `command`: applies it at once as a prediction, and to the server. It goes to the
    /// server even when the replica predicts a rejection, since only the server's state counts;
    /// the prediction then shows no effect. The replica follows the instance it targets.
    pub fn send(&mut self, command: impl Into<C::Command>) -> CommandId {
        let command = command.into();
        let seq = self.next();
        self.saved.instances.entry(C::target(&command)).or_insert((Version(0), None));
        self.saved.pending.push((seq, command.clone()));
        if self.connected {
            self.outbox.push(ToServer::Send { seq, command });
        }
        self.saved.client.command(seq)
    }

    /// A new id for an instance this client creates, unique across clients.
    pub fn fresh<I: Fresh>(&mut self) -> I {
        let seq = self.next();
        I::fresh(&format!("{}-{seq}", self.saved.client.0))
    }

    /// Starts watching row `key` of view `V`. Until the server sends the row, it reads as the
    /// view's `Default` with the predictions applied.
    pub fn watch<V: View>(&mut self, key: V::Key) -> Watched<V>
    where
        C: crate::__private::Holds<V>,
    {
        let row = C::row_of(key.clone());
        if !self.saved.rows.contains_key(&row) {
            self.saved.rows.insert(row.clone(), None);
            if self.connected {
                self.outbox.push(ToServer::Watch(row));
            }
        }
        Watched { key }
    }

    /// Stops watching a row.
    pub fn unwatch<V: View>(&mut self, watched: Watched<V>)
    where
        C: crate::__private::Holds<V>,
    {
        let row = C::row_of(watched.key);
        if self.saved.rows.remove(&row).is_some() && self.connected {
            self.outbox.push(ToServer::Unwatch(row));
        }
    }

    /// The row: the server's row with this client's unanswered commands applied.
    pub fn view<V: View>(&self, watched: &Watched<V>) -> V
    where
        C: crate::__private::Holds<V>,
    {
        let row = C::row_of(watched.key.clone());
        let mut value = self.saved.rows.get(&row).cloned().flatten();
        for (instance, event) in self.predicted() {
            if let Some(next) = C::project_in(&row, value.as_ref(), &instance, &event) {
                value = Some(next);
            }
        }
        value.and_then(|value| value.deserialize_into().ok()).unwrap_or_default()
    }

    /// How many commands the server hasn't answered yet.
    pub fn unanswered(&self) -> usize {
        self.saved.pending.len()
    }

    /// Call when the connection to the server opens: the outbox then starts with a hello,
    /// which carries every unanswered command.
    pub fn connected(&mut self) {
        let saved = &self.saved;
        self.connected = true;
        self.outbox = vec![ToServer::Hello {
            fingerprint: C::FINGERPRINT,
            client: saved.client.clone(),
            instances: saved.instances.iter().map(|(instance, (version, _))| (instance.clone(), *version)).collect(),
            rows: saved.rows.keys().cloned().collect(),
            pending: saved.pending.clone(),
        }];
    }

    /// Call when the connection closes. Commands sent until the next [`connected`](Self::connected)
    /// wait in the replica, as predictions.
    pub fn disconnected(&mut self) {
        self.connected = false;
        self.outbox.clear();
    }

    /// The messages to send the server, in order. Save the replica first, so a command the
    /// server may record is never lost on the client.
    pub fn outbox(&mut self) -> Vec<ToServer<C>> {
        std::mem::take(&mut self.outbox)
    }

    /// Takes in a message from the server, and returns the commands it refused.
    pub fn receive(&mut self, message: ToClient<C>) -> Result<Vec<(CommandId, Refusal<C>)>, Outdated> {
        let update = match message {
            ToClient::Update(update) => update,
            ToClient::Outdated => return Err(Outdated),
        };
        let saved = &mut self.saved;
        for (instance, version, state) in update.instances {
            let known = saved.instances.entry(instance).or_insert((Version(0), None));
            if version >= known.0 {
                *known = (version, state);
            }
        }
        for (row, value) in update.rows {
            if let Some(known) = saved.rows.get_mut(&row) {
                *known = Some(value);
            }
        }
        saved.pending.retain(|(seq, _)| !update.done.contains(seq));
        let mut refused = Vec::new();
        for (seq, refusal) in update.refused {
            // The server answers a command sent twice twice; the first answer counts.
            let Some(at) = saved.pending.iter().position(|(pending, _)| *pending == seq) else { continue };
            saved.pending.remove(at);
            refused.push((saved.client.command(seq), refusal));
        }
        Ok(refused)
    }

    fn next(&mut self) -> u64 {
        let seq = self.saved.next;
        self.saved.next += 1;
        seq
    }

    /// The events of the unanswered commands, each decided in order on top of the server's
    /// state and the commands before it. A command predicted to be rejected adds none.
    fn predicted(&self) -> Vec<(C::Instance, C::Event)> {
        let mut states: HashMap<C::Instance, Option<Value>> = HashMap::new();
        let mut events = Vec::new();
        'commands: for (_, command) in &self.saved.pending {
            let target = C::target(command);
            let mut state = match states.get(&target) {
                Some(state) => state.clone(),
                None => self.saved.instances.get(&target).and_then(|(_, state)| state.clone()),
            };
            let Ok(decided) = C::decide_in(state.as_ref(), command) else { continue };
            for event in &decided {
                match C::evolve_in(state.as_ref(), event) {
                    Some(next) => state = Some(next),
                    None => continue 'commands,
                }
            }
            states.insert(target.clone(), state);
            events.extend(decided.into_iter().map(|event| (target.clone(), event)));
        }
        events
    }
}

/// Context `C` served to clients. Clone it to share it between connections.
pub struct Host<C: Authorize> {
    context: Arc<C>,
}

impl<C: Authorize> Clone for Host<C> {
    fn clone(&self) -> Self {
        Host { context: self.context.clone() }
    }
}

impl<C: Authorize> Host<C> {
    /// Serves `context`.
    pub fn new(context: C) -> Self {
        Host { context: Arc::new(context) }
    }

    /// The context, to send it commands or read its views on the server itself.
    pub fn context(&self) -> &C {
        &self.context
    }

    /// A session for a client that connected for `who`.
    pub fn session(&self, who: C::Principal) -> Session<C> {
        Session {
            host: self.clone(),
            who,
            client: None,
            position: Position(0),
            instances: HashMap::new(),
            rows: HashSet::new(),
            stale: HashSet::new(),
            stale_rows: HashSet::new(),
        }
    }
}

/// One connected client, on the server. Pass it each message the client sends with
/// [`receive`](Self::receive), and call [`refresh`](Self::refresh) when other commands may have
/// changed what it follows, such as after each command any session sends. Send the client
/// what either returns.
pub struct Session<C: Authorize> {
    host: Host<C>,
    who: C::Principal,
    client: Option<ClientId>,
    /// How far in the log the client has been told about.
    position: Position,
    /// The instances the client follows, with the version it has.
    instances: HashMap<C::Instance, Version>,
    rows: HashSet<C::Row>,
    /// Instances and rows to send whether or not the log changed them.
    stale: HashSet<C::Instance>,
    stale_rows: HashSet<C::Row>,
}

impl<C: Authorize> Session<C> {
    /// Takes in a message from the client, and returns the answer to send it. Messages before
    /// the client's hello are ignored. A store error ends the connection; the client resends
    /// its unanswered commands with its next hello.
    pub async fn receive(&mut self, message: ToServer<C>) -> Result<Option<ToClient<C>>, StoreError> {
        let mut update = Update::default();
        match message {
            ToServer::Hello { fingerprint, client, instances, rows, pending } => {
                if fingerprint != C::FINGERPRINT {
                    return Ok(Some(ToClient::Outdated));
                }
                self.client = Some(client);
                self.position = crate::__private::Log::head(self.context()).await?;
                self.instances.clear();
                self.rows.clear();
                for (instance, version) in instances {
                    if self.context().may_read(&self.who, Read::Instance(&instance)).await {
                        self.stale.insert(instance.clone());
                        self.instances.insert(instance, version);
                    }
                }
                for row in rows {
                    self.watch(row).await;
                }
                for (seq, command) in pending {
                    self.decide(seq, command, &mut update).await?;
                }
            }
            _ if self.client.is_none() => return Ok(None),
            ToServer::Send { seq, command } => self.decide(seq, command, &mut update).await?,
            ToServer::Watch(row) => self.watch(row).await,
            ToServer::Unwatch(row) => {
                self.rows.remove(&row);
                self.stale_rows.remove(&row);
            }
        }
        self.collect(update).await
    }

    /// What changed for the client since it was last told, if anything did.
    pub async fn refresh(&mut self) -> Result<Option<ToClient<C>>, StoreError> {
        if self.client.is_none() {
            return Ok(None);
        }
        self.collect(Update::default()).await
    }

    fn context(&self) -> &C {
        &self.host.context
    }

    async fn watch(&mut self, row: C::Row) {
        if self.context().may_read(&self.who, Read::Row(&row)).await {
            self.stale_rows.insert(row.clone());
            self.rows.insert(row);
        }
    }

    /// Decides the client's command `seq`, and adds the answer to `update`. A command that
    /// conflicted every time it was decided gets no answer; the client resends it with its next
    /// hello.
    async fn decide(&mut self, seq: u64, command: C::Command, update: &mut Update<C>) -> Result<(), StoreError> {
        let client = self.client.as_ref().expect("a command comes after the hello");
        if !self.host.context.may_send(&self.who, &command).await {
            update.refused.push((seq, Refusal::Forbidden));
            return Ok(());
        }
        let target = C::target(&command);
        match self.host.context.send_as(command, Some(&client.command(seq))).await {
            // A command recorded before, sent again, is done too.
            Ok(_) | Err(SendError::Runaway) => update.done.push(seq),
            Err(SendError::Rejected(rejection)) => update.refused.push((seq, Refusal::Rejected(rejection))),
            Err(SendError::Conflict) => return Ok(()),
            Err(SendError::Store(error)) => return Err(error),
            Err(SendError::Unreplayable(error)) => return Err(Box::new(error)),
        }
        if !self.instances.contains_key(&target)
            && self.context().may_read(&self.who, Read::Instance(&target)).await
        {
            self.instances.insert(target.clone(), Version(0));
            self.stale.insert(target);
        }
        Ok(())
    }

    /// Adds to `update` the state of each instance and row the log changed since the client
    /// was last told, or that is stale, and returns it unless it is empty.
    async fn collect(&mut self, mut update: Update<C>) -> Result<Option<ToClient<C>>, StoreError> {
        loop {
            let (changes, upto) = self.context().changes(self.position).await?;
            if upto <= self.position {
                break;
            }
            for change in &changes {
                if self.instances.get(&change.instance).is_some_and(|known| change.version > *known) {
                    self.stale.insert(change.instance.clone());
                }
                for row in &self.rows {
                    if C::project_in(row, None, &change.instance, &change.event).is_some() {
                        self.stale_rows.insert(row.clone());
                    }
                }
            }
            self.position = upto;
        }
        for instance in std::mem::take(&mut self.stale) {
            let Some(&known) = self.instances.get(&instance) else { continue };
            let (version, state) = self.context().state_of(&instance).await?;
            if version > known {
                self.instances.insert(instance.clone(), version);
                update.instances.push((instance, version, state));
            }
        }
        for row in std::mem::take(&mut self.stale_rows) {
            if self.rows.contains(&row) {
                let value = self.context().current(&row).await?;
                update.rows.push((row, value));
            }
        }
        Ok((!update.is_empty()).then_some(ToClient::Update(update)))
    }
}

/// A map through serde as a list of pairs, since its keys are enums, which JSON keys can't be.
mod pairs {
    use std::collections::HashMap;
    use std::hash::Hash;

    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<K: Serialize, V: Serialize, S: Serializer>(map: &HashMap<K, V>, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(map.iter())
    }

    pub fn deserialize<'de, K, V, D>(deserializer: D) -> Result<HashMap<K, V>, D::Error>
    where
        K: Deserialize<'de> + Eq + Hash,
        V: Deserialize<'de>,
        D: Deserializer<'de>,
    {
        Ok(Vec::<(K, V)>::deserialize(deserializer)?.into_iter().collect())
    }
}
