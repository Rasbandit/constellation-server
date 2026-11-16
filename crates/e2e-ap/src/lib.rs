//! Evidence that a real Archipelago client library plays a real seed against our server:
//! `archipelago_rs` 3.0.1 (MIT, the owner's vendored copy with soketto and permessage-deflate)
//! connects to the real `constellation-server` binary, checks, receives, reconnects and
//! reaches its goal. Expected values come from the fixture bundle.
#![forbid(unsafe_code)]

#[cfg(all(test, feature = "ap-interop"))]
mod tests {
    use archipelago_rs::{
        Cache, Client, ClientStatus, ConnectionOptions, Event, ItemHandling, Print,
    };
    use e2e::{GUARD, Server, fixture};
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
        let deadline = Instant::now() + GUARD;
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
        assert!(!accepted.is_empty());
        for line in accepted {
            assert!(line.contains("deflate=true"), "{line}");
        }
    }

    #[test]
    fn archipelago_rs_connects_checks_receives_reconnects_and_reaches_its_goal() {
        let b = fixture().unwrap();
        let alice_world = &b.slot(1).unwrap().locations;
        let to_bob: Vec<_> = alice_world.iter().filter(|p| p.receiver == 2).collect();
        let own = alice_world.iter().find(|p| p.receiver == 1).unwrap();
        let server = Server::start(&[]).unwrap();
        let test = "core";

        let mut alice = connect(&server, "Alice", test);
        let mut bob = connect(&server, "Bob", test);
        assert_eq!(alice.this_player().slot(), 1);
        assert_eq!(alice.seed_name(), b.seed_name);
        assert_eq!(alice.unchecked_locations().count(), alice_world.len());
        assert_eq!(alice.slot_data(), &b.slot(1).unwrap().slot_data);
        let mut log = pump(&mut alice, |_, _| true);

        // Cross-slot: Alice's check lands in Bob's inventory with Alice as the sender.
        alice.mark_checked([to_bob[0].location]).unwrap();
        log.extend(pump(&mut bob, |c, _| c.received_items().len() == 1));
        let got = &bob.received_items()[0];
        assert_eq!(
            (got.index(), got.item().id(), got.location().id()),
            (0, to_bob[0].item, to_bob[0].location)
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
        log.extend(pump(&mut bob, |c, _| c.received_items().len() == 2));
        let ids: Vec<_> =
            bob.received_items().iter().map(|i| (i.index(), i.location().id())).collect();
        assert_eq!(ids, [(0, to_bob[0].location), (1, to_bob[1].location)]);
        alice.mark_checked([to_bob[2].location]).unwrap();
        log.extend(pump(&mut bob, |c, _| c.received_items().len() == 3));
        assert_eq!(bob.received_items()[2].index(), 2);

        // A repeated check after reconnecting (as the protocol asks clients to do) adds nothing.
        alice.mark_checked([to_bob[0].location, to_bob[2].location]).unwrap();
        bob.sync().unwrap();
        log.extend(pump(&mut bob, |_, evs| {
            evs.iter().any(|e| matches!(e, Event::ReceivedItems(0)))
        }));
        assert_eq!(bob.received_items().len(), 3);

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

    #[test]
    fn archipelago_rs_is_refused_for_an_unknown_slot() {
        let server = Server::start(&[]).unwrap();
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
