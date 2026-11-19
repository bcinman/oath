//! Multiplayer: replicas predict, a host decides, and every replica converges on the server.

use oath::replica::{ClientId, Host, Read, Refusal, Replica, Session, ToClient, ToServer};
use oath::{Context, Decision, Handle, MemoryStore, Project, Transition};
use pollster::block_on;

#[oath::id(prefix = "ctr_")]
pub struct CounterId(String);

#[oath::command]
pub struct Add {
    #[id]
    pub counter: CounterId,
    pub by: u64,
}
#[oath::event]
pub struct Added {
    pub by: u64,
}
#[oath::rejection]
pub enum CounterRejection {
    Full { count: u64 },
}

#[oath::state]
pub struct Empty;
#[oath::state]
pub struct Counting {
    count: u64,
}

#[oath::aggregate(id = CounterId, rejection = CounterRejection)]
pub enum Counter {
    #[initial]
    #[state(accepts(Add), on(Added => Counting))]
    Empty,
    #[state(accepts(Add), on(Added => Counting))]
    Counting,
}

/// A counter never goes past this.
const LIMIT: u64 = 10;

impl Handle<Add> for Empty {
    fn decide(&self, add: &Add) -> Decision<Self> {
        if add.by > LIMIT {
            return Err(CounterRejection::Full { count: 0 });
        }
        Ok(self.emit(Added { by: add.by }).into())
    }
}
impl Transition<Added> for Empty {
    fn evolve(self, added: &Added) -> Counting {
        Counting { count: added.by }
    }
}
impl Handle<Add> for Counting {
    fn decide(&self, add: &Add) -> Decision<Self> {
        if self.count + add.by > LIMIT {
            return Err(CounterRejection::Full { count: self.count });
        }
        Ok(self.emit(Added { by: add.by }).into())
    }
}
impl Transition<Added> for Counting {
    fn evolve(self, added: &Added) -> Counting {
        Counting { count: self.count + added.by }
    }
}

/// A sum, so an effect applied twice shows.
#[oath::view(key = CounterId, on(Added))]
#[derive(Default)]
pub struct Total {
    pub count: u64,
}
impl Project<Added> for Total {
    fn key(counter: &CounterId, _: &Added) -> CounterId {
        counter.clone()
    }
    fn project(&mut self, added: &Added) {
        self.count += added.by;
    }
}

#[oath::context(aggregates = [Counter], views = [Total])]
pub struct Tally;

impl oath::replica::Authorize for Tally {
    type Principal = String;
    async fn may_send(&self, who: &String, _: &TallyCommand) -> bool {
        who != "mallory"
    }
    async fn may_read(&self, who: &String, _: Read<'_, Self>) -> bool {
        who != "mallory"
    }
}

fn ctr(id: &str) -> CounterId {
    CounterId::new(id).unwrap()
}

/// One client: its replica, its server session, and the messages on the way each way, which
/// go through JSON as they would over a WebSocket.
struct Client {
    replica: Replica<Tally>,
    session: Session<Tally>,
    up: Vec<String>,
    down: Vec<String>,
}

impl Client {
    fn new(host: &Host<Tally>, name: &str) -> Client {
        Client {
            replica: Replica::new(ClientId::new(name).unwrap()),
            session: host.session(name.to_string()),
            up: Vec::new(),
            down: Vec::new(),
        }
    }

    /// Moves the replica's outbox onto the wire.
    fn flush(&mut self) {
        for message in self.replica.outbox() {
            self.up.push(serde_json::to_string(&message).unwrap());
        }
    }

    /// The server takes in the client's next message, if any.
    fn deliver_up(&mut self) -> bool {
        self.flush();
        if self.up.is_empty() {
            return false;
        }
        let message: ToServer<Tally> = serde_json::from_str(&self.up.remove(0)).unwrap();
        if let Some(reply) = block_on(self.session.receive(message)).unwrap() {
            self.down.push(serde_json::to_string(&reply).unwrap());
        }
        true
    }

    /// The session tells the client what other clients changed.
    fn refresh(&mut self) {
        if let Some(update) = block_on(self.session.refresh()).unwrap() {
            self.down.push(serde_json::to_string(&update).unwrap());
        }
    }

    /// The client takes in the server's next message, if any, and returns what it refused.
    fn deliver_down(&mut self) -> Option<Vec<Refusal<Tally>>> {
        if self.down.is_empty() {
            return None;
        }
        let message: ToClient<Tally> = serde_json::from_str(&self.down.remove(0)).unwrap();
        let refused = self.replica.receive(message).unwrap();
        Some(refused.into_iter().map(|(_, refusal)| refusal).collect())
    }

    /// The connection drops: messages in flight are lost.
    fn drop_connection(&mut self) {
        self.replica.disconnected();
        self.up.clear();
        self.down.clear();
    }
}

fn total(client: &mut Client, counter: &str) -> u64 {
    let watched = client.replica.watch::<Total>(ctr(counter));
    client.replica.view(&watched).count
}

fn server_total(host: &Host<Tally>, counter: &str) -> u64 {
    block_on(host.context().view::<Total>(&ctr(counter))).unwrap().count
}

fn connected(host: &Host<Tally>, name: &str) -> Client {
    let mut client = Client::new(host, name);
    let _ = client.replica.watch::<Total>(ctr("ctr_a"));
    client.replica.connected();
    while client.deliver_up() {}
    while client.deliver_down().is_some() {}
    client
}

#[test]
fn a_command_shows_at_once_and_once_when_confirmed() {
    let host = Host::new(Tally::new(MemoryStore::default()));
    let mut alice = connected(&host, "alice");

    alice.replica.send(Add { counter: ctr("ctr_a"), by: 2 });
    assert_eq!(total(&mut alice, "ctr_a"), 2, "predicted before the server has it");
    assert!(alice.deliver_up());
    assert_eq!(server_total(&host, "ctr_a"), 2);
    assert_eq!(total(&mut alice, "ctr_a"), 2, "the reply is still on its way");
    assert_eq!(alice.deliver_down(), Some(vec![]));
    assert_eq!(total(&mut alice, "ctr_a"), 2, "confirmed, and not counted twice");
    assert_eq!(alice.replica.unanswered(), 0);
}

#[test]
fn a_command_another_client_overtook_is_rebased() {
    let host = Host::new(Tally::new(MemoryStore::default()));
    let (mut alice, mut bob) = (connected(&host, "alice"), connected(&host, "bob"));

    alice.replica.send(Add { counter: ctr("ctr_a"), by: 2 });
    bob.replica.send(Add { counter: ctr("ctr_a"), by: 3 });
    assert!(bob.deliver_up());
    bob.deliver_down();
    // Alice hears of Bob's 3 before the server has her 2.
    alice.refresh();
    alice.deliver_down();
    assert_eq!(total(&mut alice, "ctr_a"), 5, "Bob's 3 from the server, and her 2 on top");
    assert!(alice.deliver_up());
    alice.deliver_down();
    assert_eq!(total(&mut alice, "ctr_a"), 5);
    bob.refresh();
    bob.deliver_down();
    assert_eq!(total(&mut bob, "ctr_a"), 5);
    assert_eq!(server_total(&host, "ctr_a"), 5);
}

#[test]
fn a_command_the_server_rejects_is_rolled_back() {
    let host = Host::new(Tally::new(MemoryStore::default()));
    let (mut alice, mut bob) = (connected(&host, "alice"), connected(&host, "bob"));
    alice.replica.send(Add { counter: ctr("ctr_a"), by: 7 });
    alice.deliver_up();
    alice.deliver_down();

    // Alice predicts 7 + 3 = 10, within the limit; Bob's 2 gets there first.
    bob.refresh();
    bob.deliver_down();
    alice.replica.send(Add { counter: ctr("ctr_a"), by: 3 });
    bob.replica.send(Add { counter: ctr("ctr_a"), by: 2 });
    assert_eq!(total(&mut alice, "ctr_a"), 10);
    bob.deliver_up();
    alice.deliver_up();
    let refused = alice.deliver_down().unwrap();
    assert_eq!(refused, vec![Refusal::Rejected(TallyRejection::Counter(CounterRejection::Full { count: 9 }))]);
    assert_eq!(total(&mut alice, "ctr_a"), 9, "her 3 is gone, Bob's 2 is in");
    assert_eq!(alice.replica.unanswered(), 0);
}

#[test]
fn a_command_resent_after_a_lost_reply_is_recorded_once() {
    let host = Host::new(Tally::new(MemoryStore::default()));
    let mut alice = connected(&host, "alice");
    alice.drop_connection();

    alice.replica.send(Add { counter: ctr("ctr_a"), by: 1 });
    alice.replica.send(Add { counter: ctr("ctr_a"), by: 4 });
    assert_eq!(total(&mut alice, "ctr_a"), 5, "offline, still predicted");
    // The hello carries both commands; the server records them, and the reply is lost.
    alice.replica.connected();
    assert!(alice.deliver_up());
    assert_eq!(server_total(&host, "ctr_a"), 5);
    alice.drop_connection();
    assert_eq!(total(&mut alice, "ctr_a"), 5);

    alice.replica.connected();
    assert!(alice.deliver_up());
    assert_eq!(server_total(&host, "ctr_a"), 5, "sent again, recorded once");
    assert_eq!(alice.deliver_down(), Some(vec![]));
    assert_eq!(total(&mut alice, "ctr_a"), 5);
    assert_eq!(alice.replica.unanswered(), 0);
}

#[test]
fn a_recorded_command_sent_again_is_done_even_when_it_would_now_be_rejected() {
    let host = Host::new(Tally::new(MemoryStore::default()));
    let (mut alice, mut bob) = (connected(&host, "alice"), connected(&host, "bob"));
    alice.replica.send(Add { counter: ctr("ctr_a"), by: 6 });
    assert!(alice.deliver_up());
    alice.drop_connection();
    // Bob fills the counter, so deciding Alice's 6 again would reject it.
    bob.replica.send(Add { counter: ctr("ctr_a"), by: 4 });
    bob.deliver_up();
    alice.replica.connected();
    assert!(alice.deliver_up());
    assert_eq!(alice.deliver_down(), Some(vec![]), "her 6 was recorded, not refused");
    assert_eq!(total(&mut alice, "ctr_a"), 10);
    assert_eq!(server_total(&host, "ctr_a"), 10);
}

#[test]
fn a_command_predicted_to_be_rejected_shows_no_effect_and_still_goes_to_the_server() {
    let host = Host::new(Tally::new(MemoryStore::default()));
    let mut alice = connected(&host, "alice");
    alice.replica.send(Add { counter: ctr("ctr_a"), by: 8 });
    alice.deliver_up();
    alice.deliver_down();

    // The replica knows the counter is at 8 from the server, so it predicts 8 + 5 is rejected.
    alice.replica.send(Add { counter: ctr("ctr_a"), by: 5 });
    alice.replica.send(Add { counter: ctr("ctr_a"), by: 1 });
    assert_eq!(total(&mut alice, "ctr_a"), 9, "the 5 shows no effect; the 1 after it does");
    assert!(alice.deliver_up() && alice.deliver_up(), "both go to the server");
    let refused = [alice.deliver_down().unwrap(), alice.deliver_down().unwrap()].concat();
    assert_eq!(refused, vec![Refusal::Rejected(TallyRejection::Counter(CounterRejection::Full { count: 8 }))]);
    assert_eq!(total(&mut alice, "ctr_a"), 9);
}

#[test]
fn a_command_the_principal_may_not_send_is_forbidden_and_never_decided() {
    let host = Host::new(Tally::new(MemoryStore::default()));
    let mut mallory = connected(&host, "mallory");
    mallory.replica.send(Add { counter: ctr("ctr_a"), by: 2 });
    assert_eq!(total(&mut mallory, "ctr_a"), 2);
    mallory.deliver_up();
    assert_eq!(mallory.deliver_down(), Some(vec![Refusal::Forbidden]));
    assert_eq!(total(&mut mallory, "ctr_a"), 0);
    assert_eq!(server_total(&host, "ctr_a"), 0);
}

#[test]
fn a_client_of_another_model_is_told_to_update() {
    let host = Host::new(Tally::new(MemoryStore::default()));
    let mut session = host.session("alice".to_string());
    let mut replica = Replica::<Tally>::new(ClientId::new("alice").unwrap());
    replica.connected();
    let mut hello = serde_json::to_value(&replica.outbox()[0]).unwrap();
    hello["Hello"]["fingerprint"] = (Tally::FINGERPRINT ^ 1).into();
    let reply = block_on(session.receive(serde_json::from_value(hello).unwrap())).unwrap().unwrap();
    assert!(matches!(reply, ToClient::Outdated));
    assert!(replica.receive(reply).is_err());
}

#[test]
fn a_saved_replica_keeps_its_commands_and_refuses_another_model() {
    let mut replica = Replica::<Tally>::new(ClientId::new("alice").unwrap());
    replica.send(Add { counter: ctr("ctr_a"), by: 3 });
    let saved = serde_json::to_value(&replica).unwrap();

    let mut restored: Replica<Tally> = serde_json::from_value(saved.clone()).unwrap();
    assert_eq!(restored.unanswered(), 1);
    let watched = restored.watch::<Total>(ctr("ctr_a"));
    assert_eq!(restored.view(&watched).count, 3);

    let mut other = saved;
    other["fingerprint"] = (Tally::FINGERPRINT ^ 1).into();
    assert!(serde_json::from_value::<Replica<Tally>>(other).is_err());
}

#[test]
fn fresh_ids_keep_their_rules_and_differ_across_clients() {
    let mut alice = Replica::<Tally>::new(ClientId::new("alice").unwrap());
    let mut bob = Replica::<Tally>::new(ClientId::new("bob").unwrap());
    let (a1, a2, b1): (CounterId, CounterId, CounterId) = (alice.fresh(), alice.fresh(), bob.fresh());
    assert_eq!(CounterId::new(a1.get().clone()), Ok(a1.clone()));
    assert!(a1.get().starts_with("ctr_"));
    assert!(a1 != a2 && a1 != b1 && a2 != b1);
}

/// Three clients send to two counters while messages are delayed, interleaved, and lost to
/// dropped connections. Once every message is through, each replica shows the server's totals,
/// no command is left unanswered, and every recorded command is counted once.
#[test]
fn replicas_converge_on_the_server_under_a_flaky_network() {
    for seed in 0..200u64 {
        let host = Host::new(Tally::new(MemoryStore::default()));
        let mut clients: Vec<Client> = ["alice", "bob", "carol"].iter().map(|name| connected(&host, name)).collect();
        for client in &mut clients {
            let _ = client.replica.watch::<Total>(ctr("ctr_b"));
        }
        let mut random = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let mut next = |n: u64| {
            random = random.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (random >> 33) % n
        };
        let mut sent = 0;
        let mut refused = 0;
        for _ in 0..60 {
            let client = &mut clients[next(3) as usize];
            match next(10) {
                0..=2 => {
                    let counter = if next(2) == 0 { "ctr_a" } else { "ctr_b" };
                    client.replica.send(Add { counter: ctr(counter), by: 1 + next(4) });
                    sent += 1;
                }
                3..=4 => {
                    client.deliver_up();
                }
                5..=6 => {
                    refused += client.deliver_down().map_or(0, |refusals| refusals.len());
                }
                7 => client.refresh(),
                8 => client.drop_connection(),
                _ => {
                    client.drop_connection();
                    client.replica.connected();
                }
            }
        }
        // Quiet: every client connects and every message gets through.
        for client in &mut clients {
            client.drop_connection();
            client.replica.connected();
            while client.deliver_up() {}
            while let Some(refusals) = client.deliver_down() {
                refused += refusals.len();
            }
        }
        for client in &mut clients {
            client.refresh();
            while let Some(refusals) = client.deliver_down() {
                refused += refusals.len();
            }
        }
        let (a, b) = (server_total(&host, "ctr_a"), server_total(&host, "ctr_b"));
        assert!(a <= LIMIT && b <= LIMIT, "seed {seed}");
        for client in &mut clients {
            assert_eq!(client.replica.unanswered(), 0, "seed {seed}");
            assert_eq!((total(client, "ctr_a"), total(client, "ctr_b")), (a, b), "seed {seed}");
        }
        // Every command was recorded once or refused once.
        let recorded = block_on(host.context().changes(oath::Position(0))).unwrap().0.len();
        assert_eq!(recorded + refused, sent, "seed {seed}");
    }
}
