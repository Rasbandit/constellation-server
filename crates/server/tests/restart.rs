//! Restart durability with Postgres (gate G-DB, D-98; doc 05 section 20 gate 0 (c)): the real
//! binary on a real database, two slots playing, then `kill -9` (or SIGTERM) and a restart on
//! the same database. Built only with the `pg-tests` feature (`required-features` in
//! `Cargo.toml`); with the feature on and no
//! `CONSTELLATION_DATABASE_URL`, every test here fails (docs/TESTING.md rule 5: no silent skip).
//! Each test owns a fresh schema, so they run in parallel. Expected values come from the
//! fixture bundle.
#![allow(
    unused_crate_dependencies,
    reason = "an integration test sees every dependency of the server package"
)]

#[cfg(test)]
mod restart {
    use e2e::{Client, Server, join, locations_for};
    use serde_json::{Value, json};
    use std::collections::BTreeSet;
    use std::path::Path;

    const BIN: &str = env!("CARGO_BIN_EXE_constellation-server");

    async fn database() -> String {
        let base = config::load_secrets().unwrap().database_url.expect(
            "pg-tests needs CONSTELLATION_DATABASE_URL (docs/TESTING.md rule 5: no silent skip)",
        );
        log::testing::fresh_schema(&base).await.unwrap()
    }

    fn start(db: &str) -> Server {
        let s = Server::start(Path::new(BIN), &[], &[("CONSTELLATION_DATABASE_URL", db)]).unwrap();
        s.wait_for_log("log store: postgres").unwrap();
        s
    }

    fn locations(v: &Value) -> Vec<i64> {
        v.as_array().unwrap().iter().map(|l| l.as_i64().unwrap()).collect()
    }

    /// (location, sending slot) of every item in a `ReceivedItems`.
    fn items(p: &Value) -> Vec<(i64, i64)> {
        p["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| (i["location"].as_i64().unwrap(), i["player"].as_i64().unwrap()))
            .collect()
    }

    async fn wait_goal_print(c: &mut Client) {
        loop {
            let (p, _) = c.next_cmd("PrintJSON").await.unwrap();
            if p["type"] == "Goal" {
                return;
            }
        }
    }

    /// What `play` did, for the checks after the restart.
    struct Played {
        alice_checked: Vec<i64>,
        bob_checked: Vec<i64>,
        alice_items: Vec<(i64, i64)>,
        bob_items: Vec<(i64, i64)>,
    }

    /// Alice checks three of Bob's items and one of her own, Bob checks two of Alice's, and
    /// Alice reaches her goal. Returns once each result reached its client, so (hold rule) all
    /// of it is durable.
    async fn play(server: &Server) -> Played {
        let a_to_b: Vec<i64> = locations_for(1, 2).unwrap().iter().take(3).map(|p| p.0).collect();
        let a_own = locations_for(1, 1).unwrap()[0].0;
        let mut b_to_a: Vec<i64> =
            locations_for(2, 1).unwrap().iter().take(2).map(|p| p.0).collect();
        let (mut alice, _, _) = join(&server.url(), "Alice").await.unwrap();
        let (mut bob, _, _) = join(&server.url(), "Bob").await.unwrap();
        let mut alice_checked = a_to_b.clone();
        alice_checked.push(a_own);
        // The server applies one packet's locations in ascending order.
        alice_checked.sort_unstable();
        b_to_a.sort_unstable();
        alice.check(&alice_checked).await.unwrap();
        let (u, _) = alice.next_cmd("RoomUpdate").await.unwrap();
        assert_eq!(locations(&u["checked_locations"]), alice_checked);
        bob.check(&b_to_a).await.unwrap();
        let (u, _) = bob.next_cmd("RoomUpdate").await.unwrap();
        assert_eq!(locations(&u["checked_locations"]), b_to_a);
        alice.send(&json!([{"cmd": "StatusUpdate", "status": 30}])).await.unwrap();
        wait_goal_print(&mut alice).await;
        let bob_items: Vec<(i64, i64)> =
            alice_checked.iter().filter(|l| a_to_b.contains(l)).map(|&l| (l, 1)).collect();
        let mut alice_items = vec![(a_own, 1)];
        alice_items.extend(b_to_a.iter().map(|&l| (l, 2)));
        Played { alice_checked, bob_checked: b_to_a, alice_items, bob_items }
    }

    /// After the restart: each slot's checks, whole inventory from index 0 exactly once, the
    /// goal, then a repeat that delivers nothing and a new check that continues the index.
    async fn assert_kept(server: &Server, p: &Played) {
        let (mut alice, connected, _) = join(&server.url(), "Alice").await.unwrap();
        let got: BTreeSet<i64> = locations(&connected["checked_locations"]).into_iter().collect();
        assert_eq!(got, p.alice_checked.iter().copied().collect());
        let (inv, _) = alice.next_cmd("ReceivedItems").await.unwrap();
        assert_eq!((inv["index"].clone(), items(&inv)), (json!(0), p.alice_items.clone()));
        let (mut bob, connected, _) = join(&server.url(), "Bob").await.unwrap();
        let got: BTreeSet<i64> = locations(&connected["checked_locations"]).into_iter().collect();
        assert_eq!(got, p.bob_checked.iter().copied().collect());
        let (inv, _) = bob.next_cmd("ReceivedItems").await.unwrap();
        assert_eq!((inv["index"].clone(), items(&inv)), (json!(0), p.bob_items.clone()));
        bob.send(&json!([{"cmd": "Get", "keys": ["_read_client_status_0_1"]}])).await.unwrap();
        let (status, _) = bob.next_cmd("Retrieved").await.unwrap();
        assert_eq!(status["keys"]["_read_client_status_0_1"], 30, "the goal was lost");
        // A check from before the crash is still deduplicated; the new one gets the next index.
        let next = locations_for(1, 2).unwrap()[3].0;
        alice.check(&[p.alice_checked[0], next]).await.unwrap();
        let (live, _) = bob.next_cmd("ReceivedItems").await.unwrap();
        assert_eq!(
            (live["index"].clone(), items(&live)),
            (json!(p.bob_items.len()), vec![(next, 1)])
        );
    }

    #[tokio::test]
    async fn a_room_survives_kill_9_with_every_item_once_and_its_goal() {
        let db = database().await;
        let server = start(&db);
        let played = play(&server).await;
        let status = server.kill().unwrap();
        assert!(!status.success(), "SIGKILL must not look like a clean exit: {status}");
        let server = start(&db);
        let loaded = server.wait_for_log("room loaded").unwrap();
        // Six checks, each a Checked and an Item event, and one Goal.
        assert!(loaded.contains("events=13"), "{loaded}");
        assert_kept(&server, &played).await;
    }

    #[tokio::test]
    async fn a_room_survives_a_graceful_sigterm_restart() {
        let db = database().await;
        let server = start(&db);
        let played = play(&server).await;
        assert!(server.stop().unwrap().success());
        let server = start(&db);
        assert_kept(&server, &played).await;
    }

    /// The hold rule across a real crash: a check whose `RoomUpdate` reached the client was
    /// durable, so after `kill -9` mid-stream it is still there, and the log stays consistent:
    /// the checks that survived are a prefix of the ones sent (one client's packets apply in
    /// order) and Bob holds exactly the items of the surviving checks, once each.
    #[tokio::test]
    async fn a_check_acknowledged_before_kill_9_is_never_lost() {
        let db = database().await;
        let server = start(&db);
        let all: Vec<(i64, u32)> = {
            let b = e2e::fixture().unwrap();
            b.slot(1).unwrap().locations.iter().map(|p| (p.location, p.receiver)).collect()
        };
        let (mut alice, _, _) = join(&server.url(), "Alice").await.unwrap();
        for (l, _) in &all {
            alice.check(&[*l]).await.unwrap();
        }
        let mut acked = Vec::new();
        while acked.len() < 5 {
            let (u, _) = alice.next_cmd("RoomUpdate").await.unwrap();
            acked.extend(locations(&u["checked_locations"]));
        }
        // Killed while the rest of the 15 checks are still in flight.
        server.kill().unwrap();
        let server = start(&db);
        let (_alice, connected, _) = join(&server.url(), "Alice").await.unwrap();
        let kept: BTreeSet<i64> = locations(&connected["checked_locations"]).into_iter().collect();
        for l in &acked {
            assert!(kept.contains(l), "acknowledged check {l} lost; kept {kept:?}");
        }
        let n = kept.len();
        let prefix: BTreeSet<i64> = all.iter().take(n).map(|p| p.0).collect();
        assert_eq!(kept, prefix, "the surviving checks are not a prefix of those sent");
        let (mut bob, _, _) = join(&server.url(), "Bob").await.unwrap();
        let want: Vec<(i64, i64)> =
            all.iter().take(n).filter(|p| p.1 == 2).map(|p| (p.0, 1)).collect();
        if want.is_empty() {
            return;
        }
        let (inv, _) = bob.next_cmd("ReceivedItems").await.unwrap();
        assert_eq!((inv["index"].clone(), items(&inv)), (json!(0), want));
    }

    #[tokio::test]
    async fn a_restart_with_a_different_bundle_for_the_same_room_is_refused() {
        let db = database().await;
        let server = start(&db);
        assert!(server.stop().unwrap().success());
        let mut b: Value =
            serde_json::from_str(&std::fs::read_to_string(e2e::fixture_path()).unwrap()).unwrap();
        b["race_mode"] = json!(1 - b["race_mode"].as_i64().unwrap_or(0));
        let dir = std::env::temp_dir().join(format!("e2e-bundle-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("changed.json");
        std::fs::write(&path, b.to_string()).unwrap();
        let err = Server::start(
            Path::new(BIN),
            &["--bundle", path.to_str().unwrap()],
            &[("CONSTELLATION_DATABASE_URL", &db)],
        )
        .unwrap_err();
        assert!(err.contains("different bundle"), "{err}");
    }
}
