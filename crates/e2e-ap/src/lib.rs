//! Evidence that a real Archipelago client library plays a real seed against our server:
//! `archipelago_rs` 3.0.1 (MIT, the owner's vendored copy with soketto and permessage-deflate)
//! connects to the real `constellation-server` binary, checks, receives, reconnects and
//! reaches its goal. Expected values come from the fixture bundle.
#![forbid(unsafe_code)]

#[cfg(test)]
#[cfg(feature = "ap-interop")]
mod tests {
    use flate2 as _; // only pins the version (see Cargo.toml)

    use archipelago_rs::{
        Cache, Client, ClientStatus, ConnectionOptions, CreateAsHint, DataStorageOperation, Event,
        HintStatus, ItemHandling, Print,
    };
    use e2e::{GUARD, Server, fixture};
    use std::path::Path;

    /// The server binary, built by `just e2e-ap` in the main workspace (this crate is its own
    /// workspace, so cargo cannot hand it the main workspace's binary).
    const BIN: &str = env!("CONSTELLATION_SERVER_BIN");

    fn start() -> Server {
        Server::start(Path::new(BIN), &[], &[]).unwrap()
    }
    use std::path::PathBuf;
    use std::time::Instant;

    const GAME: &str = "Archipela-Go 2: Electric Boogaloo";

    type Ap = Client<serde_json::Value>;

    /// A data package cache of its own per test, so nothing touches the user's cache.
    fn cache_dir(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("e2e-ap-{}-{test}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn connect(server: &Server, name: &str, test: &str) -> Ap {
        let options = ConnectionOptions::new()
            .receive_items(ItemHandling::OtherWorlds { own_world: true, starting_inventory: true })
            .cache(Cache::path(cache_dir(test)));
        smol::block_on(Client::connect(server.url(), name, Some(GAME), options)).unwrap()
    }

    /// Update until `done` holds, sleeping only inside the socket until data arrives. Returns
    /// every event seen; fails on a client error or after the guard timeout.
    fn pump(c: &mut Ap, mut done: impl FnMut(&Ap, &[Event]) -> bool) -> Vec<Event> {
        let deadline = Instant::now().checked_add(GUARD).unwrap();
        let mut seen = Vec::new();
        loop {
            let events = c.update();
            for e in &events {
                if let Event::Error(err) = e {
                    panic!("client error: {err:?}");
                }
            }
            seen.extend(events);
            if done(c, &seen) {
                return seen;
            }
            let left = deadline.checked_duration_since(Instant::now()).expect("guard timeout");
            c.readiness().wait(left);
        }
    }

    fn prints(events: &[Event]) -> Vec<String> {
        events
            .iter()
            .filter_map(|e| if let Event::Print(p) = e { Some(p.to_string()) } else { None })
            .collect()
    }

    /// The Archipelago server warns clients that do not compress; ours must never send such
    /// a print, and every connection it accepted must have negotiated permessage-deflate.
    fn assert_compressed(server: &Server, events: &[Event]) {
        for p in prints(events) {
            assert!(!p.to_lowercase().contains("compress"), "compression warning: {p}");
        }
        let accepted: Vec<_> =
            server.log().into_iter().filter(|l| l.contains("websocket accepted")).collect();
        assert_ne!(accepted.len(), 0, "no accepted socket was logged");
        for line in accepted {
            assert!(line.contains("deflate=true"), "{line}");
        }
    }

    #[test]
    fn archipelago_rs_connects_checks_receives_reconnects_and_reaches_its_goal() {
        let b = fixture().unwrap();
        let alice_world = &b.slot(1).unwrap().locations;
        let to_bob: Vec<_> = alice_world.iter().filter(|p| p.receiver == 2).collect();
        // Bob asks for his starting inventory, so it leads his item list (protocol doc,
        // "items_handling flags" bit 2).
        let start_items = &b.slot(2).unwrap().start_inventory;
        let n = start_items.len();
        let own = alice_world.iter().find(|p| p.receiver == 1).unwrap();
        let server = start();
        let test = "core";

        let mut alice = connect(&server, "Alice", test);
        let mut bob = connect(&server, "Bob", test);
        assert_eq!(alice.this_player().slot(), 1);
        assert_eq!(alice.seed_name(), b.seed_name);
        assert_eq!(alice.unchecked_locations().count(), alice_world.len());
        assert_eq!(alice.slot_data(), &b.slot(1).unwrap().slot_data);
        let mut log = pump(&mut alice, |_, _| true);

        log.extend(pump(&mut bob, |c, _| c.received_items().len() == n));
        let got: Vec<_> = bob
            .received_items()
            .iter()
            .map(|i| (i.index(), i.item().id(), i.location().id(), i.sender().slot()))
            .collect();
        let want: Vec<_> =
            start_items.iter().enumerate().map(|(i, &item)| (i, item, -2, 0)).collect();
        assert_eq!(got, want, "start items: location -2 from player 0, the server");

        // Cross-slot: Alice's check lands in Bob's inventory with Alice as the sender.
        alice.mark_checked([to_bob[0].location]).unwrap();
        log.extend(pump(&mut bob, |c, _| c.received_items().len() == n + 1));
        let got = &bob.received_items()[n];
        assert_eq!(
            (got.index(), got.item().id(), got.location().id()),
            (n, to_bob[0].item, to_bob[0].location)
        );
        assert_eq!(got.sender().slot(), 1);
        assert_eq!(got.is_trap(), to_bob[0].flags & 4 != 0);

        // Own item: comes back to Alice; the checked set follows the server.
        alice.mark_checked([own.location]).unwrap();
        log.extend(pump(&mut alice, |c, _| c.received_items().len() == 1));
        assert_eq!(alice.received_items()[0].item().id(), own.item);
        assert!(alice.is_local_location_checked(own.location));

        // Reconnect: a fresh client for Bob gets the whole inventory again, then a new item.
        drop(bob);
        alice.mark_checked([to_bob[1].location]).unwrap();
        log.extend(pump(&mut alice, |c, _| c.checked_locations().count() == 3));
        let mut bob = connect(&server, "Bob", test);
        log.extend(pump(&mut bob, |c, _| c.received_items().len() == n + 2));
        let ids: Vec<_> =
            bob.received_items().iter().skip(n).map(|i| (i.index(), i.location().id())).collect();
        assert_eq!(ids, [(n, to_bob[0].location), (n + 1, to_bob[1].location)]);
        alice.mark_checked([to_bob[2].location]).unwrap();
        log.extend(pump(&mut bob, |c, _| c.received_items().len() == n + 3));
        assert_eq!(bob.received_items()[n + 2].index(), n + 2);

        // A repeated check after reconnecting (as the protocol asks clients to do) adds nothing.
        alice.mark_checked([to_bob[0].location, to_bob[2].location]).unwrap();
        bob.sync().unwrap();
        log.extend(pump(&mut bob, |_, evs| {
            evs.iter().any(|e| matches!(e, Event::ReceivedItems(0)))
        }));
        assert_eq!(bob.received_items().len(), n + 3);

        // Goal: Bob hears about it through a Goal print naming Alice.
        alice.set_status(ClientStatus::Goal).unwrap();
        let heard = pump(&mut bob, |_, evs| {
            evs.iter().any(
                |e| matches!(e, Event::Print(Print::Goal { player, .. }) if player.slot() == 1),
            )
        });
        log.extend(heard);
        log.extend(pump(&mut alice, |_, _| true));
        assert_compressed(&server, &log);
    }

    /// Data storage through the library's own calls: `watch` (`SetNotify`), `change` (`Set`
    /// with operations and a reply) and `get`; client status changes reach a watcher.
    #[test]
    fn archipelago_rs_stores_watches_and_reads_data_storage() {
        let server = start();
        let test = "storage";
        let mut alice = connect(&server, "Alice", test);
        let mut bob = connect(&server, "Bob", test);
        let key = "e2e-ap-shared";
        alice.watch([key, "_read_client_status_0_2"]).unwrap();
        let changed = |key: &'static str| {
            move |_: &Ap, evs: &[Event]| {
                evs.iter().any(|e| matches!(e, Event::KeyChanged { key: k, .. } if k == key))
            }
        };
        let ops = [
            DataStorageOperation::Union(vec![serde_json::json!("a"), serde_json::json!("b")]),
            DataStorageOperation::Remove(serde_json::json!("a")),
        ];
        bob.change(key, serde_json::json!([]), ops, true).unwrap();
        for c in [&mut bob, &mut alice] {
            let seen = pump(c, changed(key));
            let (old, new, by) = seen
                .iter()
                .find_map(|e| match e {
                    Event::KeyChanged { key: k, old_value, new_value, player } if k == key => Some(
                        (old_value.clone(), new_value.clone(), player.as_ref().map(|p| p.slot())),
                    ),
                    _ => None,
                })
                .unwrap();
            assert_eq!(
                (old, new, by),
                (Some(serde_json::json!([])), serde_json::json!(["b"]), Some(2))
            );
        }
        // Arithmetic as the library sends it (floats), then floor without a value.
        let ops = [DataStorageOperation::Add(2.5), DataStorageOperation::Floor];
        bob.change("e2e-ap-n", serde_json::json!(1), ops, false).unwrap();
        let groups = format!("_read_item_name_groups_{GAME}");
        let rx = alice.get([key.to_owned(), "e2e-ap-n".to_owned(), groups.clone()]);
        let mut got = None;
        pump(&mut alice, |_, _| {
            got = rx.try_recv().ok();
            got.is_some()
        });
        let got = got.unwrap().unwrap();
        assert_eq!(got[key], serde_json::json!(["b"]));
        assert_eq!(got["e2e-ap-n"], serde_json::json!(3));
        let want = serde_json::to_value(&fixture().unwrap().games[GAME].item_name_groups).unwrap();
        assert_eq!(got[&groups], want);
        // Bob leaves: his status goes to 0 for the watcher.
        drop(bob);
        let seen = pump(&mut alice, changed("_read_client_status_0_2"));
        assert!(
            seen.iter().any(|e| matches!(e, Event::KeyChanged { new_value, player: None, .. }
                if *new_value == serde_json::json!(0))),
            "no status 0 after Bob left"
        );
    }

    /// Hints through the library's own calls: a scout that creates hints, the receiver's
    /// `Hint` print and watched `_read_hints_` key, and `update_hint`.
    #[test]
    fn archipelago_rs_creates_watches_and_updates_hints() {
        let b = fixture().unwrap();
        let to_bob = b.slot(1).unwrap().locations.iter().find(|p| p.receiver == 2).unwrap();
        let server = start();
        let test = "hints";
        let mut alice = connect(&server, "Alice", test);
        let mut bob = connect(&server, "Bob", test);
        let key = "_read_hints_0_2";
        bob.watch([key]).unwrap();
        pump(&mut bob, |_, _| true);
        let rx = alice.scout_locations([to_bob.location], CreateAsHint::New);
        let mut scouted = None;
        pump(&mut alice, |_, _| {
            scouted = rx.try_recv().ok();
            scouted.is_some()
        });
        assert_eq!(scouted.unwrap().unwrap().len(), 1);
        let mut printed = false;
        let mut listed = None;
        pump(&mut bob, |_, evs| {
            for e in evs {
                match e {
                    Event::Print(Print::Hint { item, found, .. }) => {
                        assert_eq!(item.location().id(), to_bob.location);
                        assert_eq!((item.sender().slot(), item.receiver().slot()), (1, 2));
                        assert!(!found);
                        printed = true;
                    }
                    Event::KeyChanged { key: k, new_value, player, .. } if k == key => {
                        assert!(player.is_none(), "a read-only key has no writer");
                        listed = Some(new_value.clone());
                    }
                    _ => {}
                }
            }
            printed && listed.is_some()
        });
        let list = listed.unwrap();
        assert_eq!(list[0]["location"], to_bob.location);
        assert_eq!((list[0]["status"].clone(), list[0]["class"].clone()), (0.into(), "Hint".into()));
        // Bob receives it, so Bob may mark it.
        bob.update_hint(1, to_bob.location, HintStatus::Avoid).unwrap();
        pump(&mut bob, |_, evs| {
            evs.iter().any(|e| matches!(e, Event::KeyChanged { key: k, new_value, .. }
                if k == key && new_value[0]["status"] == 20))
        });
    }

    #[test]
    fn archipelago_rs_is_refused_for_an_unknown_slot() {
        let server = start();
        let options = ConnectionOptions::new().cache(Cache::path(cache_dir("refused")));
        let err = smol::block_on(Client::<serde_json::Value>::connect(
            server.url(),
            "Mallory",
            Some(GAME),
            options,
        ))
        .err()
        .unwrap();
        assert!(format!("{err:?}").contains("InvalidSlot"), "{err:?}");
    }
}
