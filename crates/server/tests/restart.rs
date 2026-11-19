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

    /// The fixture's start items for `slot` as (location -2, player 0) pairs.
    fn start_items(slot: u32) -> Vec<(i64, i64)> {
        e2e::start_items(slot).unwrap().iter().map(|_| (-2, 0)).collect()
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
        // Two writes to the data storage; the reply to the second means both are durable.
        for v in [2, 3] {
            let set = json!([{"cmd": "Set", "key": "saved", "want_reply": true,
                "operations": [{"operation": "add", "value": v}]}]);
            alice.send(&set).await.unwrap();
            alice.next_cmd("SetReply").await.unwrap();
        }
        alice.send(&json!([{"cmd": "StatusUpdate", "status": 30}])).await.unwrap();
        wait_goal_print(&mut alice).await;
        // Bob asks for his starting inventory (bit 2), so it leads his list.
        let mut bob_items = start_items(2);
        bob_items.extend(alice_checked.iter().filter(|l| a_to_b.contains(l)).map(|&l| (l, 1)));
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
        bob.send(&json!([{"cmd": "Get", "keys": ["_read_client_status_0_1", "saved"]}]))
            .await
            .unwrap();
        let (got, _) = bob.next_cmd("Retrieved").await.unwrap();
        assert_eq!(got["keys"]["_read_client_status_0_1"], 30, "the goal was lost");
        assert_eq!(got["keys"]["saved"], 5, "the stored value was lost");
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
        assert_kept(&server, &played).await;
        let loaded = server.wait_for_log("room loaded").unwrap();
        // Six checks, each a Checked and an Item event, two key writes and one Goal.
        assert!(loaded.contains("events=15"), "{loaded}");
    }

    /// Hints and hint points are log events too: a free scout hint, a paid `!hint` and a
    /// status change all survive `kill -9`, and the points spent stay spent.
    #[tokio::test]
    async fn hints_and_points_survive_kill_9() {
        let db = database().await;
        let server = start(&db);
        let (mut alice, _, _) = join(&server.url(), "Alice").await.unwrap();
        let (mut bob, _, _) = join(&server.url(), "Bob").await.unwrap();
        // The fixture: one point per check, a hint costs Bob max(1, int(10% of 15)) = 1.
        bob.check(&[locations_for(2, 1).unwrap()[0].0]).await.unwrap();
        let (u, _) = bob.next_cmd("RoomUpdate").await.unwrap();
        assert_eq!(u["hint_points"], 1);
        // A free hint: Alice scouts a location of hers that holds an item for Bob.
        let scouted = locations_for(1, 2).unwrap()[0].0;
        let scout = json!([{"cmd": "LocationScouts", "locations": [scouted], "create_as_hint": 2}]);
        alice.send(&scout).await.unwrap();
        alice.next_cmd("LocationInfo").await.unwrap();
        // A paid hint: Bob asks for an item by name.
        let b = e2e::fixture().unwrap();
        let (_, item, _) = locations_for(1, 2).unwrap()[3];
        let game = &b.games[&b.slot(2).unwrap().game];
        let name = game.item_name_to_id.iter().find(|(_, id)| **id == item).unwrap().0;
        bob.send(&json!([{"cmd": "Say", "text": format!("!hint {name}")}])).await.unwrap();
        loop {
            let (u, _) = bob.next_cmd("RoomUpdate").await.unwrap();
            if u["hint_points"] == 0 {
                break;
            }
        }
        // A status change, then a read: the session answers the read only after the change
        // is durable.
        let key = "_read_hints_0_2";
        let up = json!([{"cmd": "UpdateHint", "player": 1, "location": scouted, "status": 10},
            {"cmd": "Get", "keys": [key]}]);
        bob.send(&up).await.unwrap();
        let (before, _) = bob.next_cmd("Retrieved").await.unwrap();
        let status = server.kill().unwrap();
        assert!(!status.success(), "{status}");
        let server = start(&db);
        let loaded = server.wait_for_log("room loaded").unwrap();
        // One check (two events), two new hints and one status change.
        assert!(loaded.contains("events=5"), "{loaded}");
        let (mut bob, connected, _) = join(&server.url(), "Bob").await.unwrap();
        assert_eq!(connected["hint_points"], 0, "the paid hint stays paid");
        bob.send(&json!([{"cmd": "Get", "keys": [key]}])).await.unwrap();
        let (after, _) = bob.next_cmd("Retrieved").await.unwrap();
        assert_eq!(after["keys"][key], before["keys"][key]);
        let hints = after["keys"][key].as_array().unwrap();
        assert_eq!(hints.len(), 2, "{hints:?}");
        let at = |l: i64| hints.iter().find(|h| h["location"] == l && h["finding_player"] == 1);
        assert_eq!(at(scouted).unwrap()["status"], 10);
        assert!(
            hints.iter().any(|h| h["item"] == item && h["status"] == 30),
            "the paid hint is kept: {hints:?}"
        );
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
        let mut want = start_items(2);
        want.extend(all.iter().take(n).filter(|p| p.1 == 2).map(|p| (p.0, 1)));
        let (inv, _) = bob.next_cmd("ReceivedItems").await.unwrap();
        assert_eq!((inv["index"].clone(), items(&inv)), (json!(0), want));
    }

    /// Run one statement in the test's schema (fault injection and fence reads).
    async fn sql(db: &str, q: &'static str) {
        use sqlx::Connection;
        let mut c = sqlx::PgConnection::connect(db).await.unwrap();
        sqlx::raw_sql(q).execute(&mut c).await.unwrap();
    }

    async fn epoch(db: &str) -> i64 {
        use sqlx::Connection;
        let mut c = sqlx::PgConnection::connect(db).await.unwrap();
        sqlx::query_scalar("SELECT epoch FROM room_owner").fetch_one(&mut c).await.unwrap()
    }

    /// Liveness: a room whose append fails stops, and so does the process (non-zero), so the
    /// supervisor restarts it; the restart rebuilds from the log, nothing acknowledged is lost
    /// and the refused check can be made again.
    #[tokio::test]
    async fn a_failed_append_exits_the_process_and_a_restart_recovers() {
        let db = database().await;
        let server = start(&db);
        let own: Vec<i64> = locations_for(1, 1).unwrap().iter().take(2).map(|p| p.0).collect();
        let (mut alice, _, _) = join(&server.url(), "Alice").await.unwrap();
        alice.check(&own[..1]).await.unwrap();
        alice.next_cmd("RoomUpdate").await.unwrap();
        // The log holds seq 1 and 2 (a Checked and its Item); the next append must fail.
        sql(&db, "ALTER TABLE events ADD CONSTRAINT fail_next CHECK (seq <= 2)").await;
        alice.check(&own[1..]).await.unwrap();
        let status = server.wait_exit().unwrap();
        // 1 is the generic failure: restart me (3 is reserved for "fenced", below).
        assert_eq!(status.code(), Some(1), "a stopped room must stop the process: {status}");
        sql(&db, "ALTER TABLE events DROP CONSTRAINT fail_next").await;
        let server = start(&db);
        let (mut alice, connected, _) = join(&server.url(), "Alice").await.unwrap();
        assert_eq!(locations(&connected["checked_locations"]), own[..1]);
        let (inv, _) = alice.next_cmd("ReceivedItems").await.unwrap();
        assert_eq!(items(&inv), [(own[0], 1)]);
        alice.check(&own[1..]).await.unwrap();
        let (u, _) = alice.next_cmd("RoomUpdate").await.unwrap();
        assert_eq!(locations(&u["checked_locations"]), own[1..]);
    }

    /// A mistaken second instance (same database, port already taken) must fail before it
    /// claims the room: the epoch stays put and the live room keeps serving.
    #[tokio::test]
    async fn a_second_instance_on_a_taken_port_exits_without_fencing_the_live_room() {
        let db = database().await;
        let live = start(&db);
        assert_eq!(epoch(&db).await, 1);
        let port = live.addr.port().to_string();
        let second = Server::start(
            Path::new(BIN),
            &["--port", &port],
            &[("CONSTELLATION_DATABASE_URL", &db)],
        );
        let err = second.unwrap_err();
        assert!(err.contains("bind:"), "the second instance must fail on the port: {err}");
        assert_eq!(epoch(&db).await, 1, "the second instance claimed the room");
        let (mut alice, _, _) = join(&live.url(), "Alice").await.unwrap();
        let loc = locations_for(1, 1).unwrap()[0].0;
        alice.check(&[loc]).await.unwrap();
        let (u, _) = alice.next_cmd("RoomUpdate").await.unwrap();
        assert_eq!(locations(&u["checked_locations"]), [loc]);
    }

    /// A newer owner took the room: the old process exits with its own code (3, the
    /// documented do-not-restart status) and says so, so a supervisor does not bring it back
    /// to fence the new owner in turn.
    #[tokio::test]
    async fn a_fenced_server_exits_with_the_fenced_code_and_says_why() {
        let db = database().await;
        let server = start(&db);
        let (mut alice, _, _) = join(&server.url(), "Alice").await.unwrap();
        // What a second instance's claim does.
        sql(&db, "UPDATE room_owner SET epoch = epoch + 1").await;
        alice.check(&[locations_for(1, 1).unwrap()[0].0]).await.unwrap();
        let line = server.wait_for_log("fenced by a newer owner").unwrap();
        assert!(line.contains("current=2"), "{line}");
        let status = server.wait_exit().unwrap();
        assert_eq!(status.code(), Some(3), "{status}");
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

    /// The key `room ready` logged for `seed`.
    fn key_of(server: &Server, seed: &str) -> String {
        let line = server
            .log()
            .into_iter()
            .find(|l| l.contains("room ready") && l.contains(&format!("seed=\"{seed}\"")))
            .unwrap_or_else(|| panic!("no room ready line for {seed}: {:?}", server.log()));
        line.split("key=").nth(1).unwrap().split_whitespace().next().unwrap().to_owned()
    }

    #[tokio::test]
    async fn rooms_keep_their_random_keys_and_their_items_across_kill_9() {
        let db = database().await;
        let dir = std::env::temp_dir().join(format!("e2e-pg-rooms-{}", std::process::id()));
        drop(std::fs::remove_dir_all(&dir));
        std::fs::create_dir_all(&dir).unwrap();
        let text = std::fs::read_to_string(e2e::fixture_path()).unwrap();
        for seed in ["alpha", "beta"] {
            let mut v: Value = serde_json::from_str(&text).unwrap();
            v["seed_name"] = json!(seed);
            std::fs::write(dir.join(format!("{seed}.json")), v.to_string()).unwrap();
        }
        let env = [("CONSTELLATION_DATABASE_URL", db.as_str())];
        let server = Server::start_rooms(Path::new(BIN), &dir, &[], &env).unwrap();
        let (a, b) = (key_of(&server, "alpha"), key_of(&server, "beta"));
        assert_ne!(a, b);
        let url = |s: &Server, key: &str| format!("ws://{key}.example:{}", s.addr.port());
        let loc = locations_for(1, 2).unwrap()[0].0;
        let (mut alice, _, _) =
            e2e::join_via(server.addr, &url(&server, &a), "Alice").await.unwrap();
        alice.check(&[loc]).await.unwrap();
        alice.next_cmd("RoomUpdate").await.unwrap();
        assert!(!server.kill().unwrap().success());

        let server = Server::start_rooms(Path::new(BIN), &dir, &[], &env).unwrap();
        assert_eq!((key_of(&server, "alpha"), key_of(&server, "beta")), (a.clone(), b.clone()));
        let (_, room_a, _) = e2e::join_via(server.addr, &url(&server, &a), "Alice").await.unwrap();
        assert_eq!(locations(&room_a["checked_locations"]), [loc]);
        let (_, room_b, _) = e2e::join_via(server.addr, &url(&server, &b), "Alice").await.unwrap();
        assert!(locations(&room_b["checked_locations"]).is_empty());
        // Two rooms of one game: its packages once (the game and the generic one), a seed each.
        assert_eq!(count(&db, "SELECT count(*) FROM game_packages").await, 2);
        assert_eq!(count(&db, "SELECT count(*) FROM room_seeds").await, 2);
        assert_eq!(count(&db, "SELECT count(*) FROM rooms WHERE bundle IS NOT NULL").await, 0);
        drop(std::fs::remove_dir_all(&dir));
    }

    async fn count(db: &str, query: &'static str) -> i64 {
        use sqlx::Connection;
        let mut c = sqlx::PgConnection::connect(db).await.unwrap();
        sqlx::query_scalar(query).fetch_one(&mut c).await.unwrap()
    }

    /// A package row whose bytes do not hash to its key is refused when the room loads after a
    /// restart: the room does not open, nothing wrong is served, and the client gets a generic
    /// 503 without the error text (#63).
    #[tokio::test]
    async fn a_tampered_package_row_is_refused_when_the_room_loads() {
        let db = database().await;
        let dir = rooms_dir("tampered", &["alpha"]);
        let env = [("CONSTELLATION_DATABASE_URL", db.as_str())];
        let server = Server::start_rooms(Path::new(BIN), &dir, &[], &env).unwrap();
        let a = key_of(&server, "alpha");
        assert!(server.stop().unwrap().success());
        std::fs::remove_file(dir.join("alpha.json")).unwrap();
        sql(&db, "UPDATE game_packages SET bytes = 'tampered'").await;
        let server = Server::start_rooms(Path::new(BIN), &dir, &[], &env).unwrap();
        let url = format!("ws://{a}.example:{}", server.addr.port());
        assert!(e2e::join_via(server.addr, &url, "Alice").await.is_err());
        server.wait_for_log("does not match its key").unwrap();
        // The client is told only that the room did not load; the detail stays in the log.
        let reply = e2e::http_get(server.addr, &format!("{a}.example"), "/", true).await;
        assert!(reply.starts_with("HTTP/1.1 503"), "{reply}");
        assert!(reply.ends_with("\r\n\r\nthe room could not be loaded; try again"), "{reply}");
        drop(std::fs::remove_dir_all(&dir));
    }

    /// A room stored before seeds were kept (its v1 file in rooms.bundle, no seed row) keeps
    /// its own world when another bundle with its seed name comes in: the import is refused
    /// and writes no seed for it.
    #[tokio::test]
    async fn a_legacy_room_keeps_its_world_when_another_bundle_takes_its_name() {
        use sqlx::Connection;
        let db = database().await;
        let dir = rooms_dir("legacy", &["alpha"]);
        let env = [("CONSTELLATION_DATABASE_URL", db.as_str())];
        let server = Server::start_rooms(Path::new(BIN), &dir, &[], &env).unwrap();
        let a = key_of(&server, "alpha");
        assert!(server.stop().unwrap().success());
        let file = std::fs::read(dir.join("alpha.json")).unwrap();
        let mut c = sqlx::PgConnection::connect(&db).await.unwrap();
        sqlx::query("UPDATE rooms SET bundle = $1").bind(&file).execute(&mut c).await.unwrap();
        sql(&db, "DELETE FROM room_seeds").await;
        let mut other: Value = serde_json::from_slice(&file).unwrap();
        for g in other["games"].as_object_mut().unwrap().values_mut() {
            g["checksum"] = json!(format!("other-{}", g["checksum"]));
        }
        std::fs::write(dir.join("alpha.json"), other.to_string()).unwrap();
        let server = Server::start_rooms(Path::new(BIN), &dir, &[], &env).unwrap();
        server.wait_for_log("different bundle").unwrap();
        let seeds: i64 =
            sqlx::query_scalar("SELECT count(*) FROM room_seeds").fetch_one(&mut c).await.unwrap();
        assert_eq!(seeds, 0, "no seed of the other bundle was written");
        let url = format!("ws://{a}.example:{}", server.addr.port());
        e2e::join_via(server.addr, &url, "Alice").await.unwrap();
        drop(std::fs::remove_dir_all(&dir));
    }

    /// A rooms folder holding the fixture under each seed name.
    fn rooms_dir(name: &str, seeds: &[&str]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("e2e-pg-{name}-{}", std::process::id()));
        drop(std::fs::remove_dir_all(&dir));
        std::fs::create_dir_all(&dir).unwrap();
        let text = std::fs::read_to_string(e2e::fixture_path()).unwrap();
        for seed in seeds {
            let mut v: Value = serde_json::from_str(&text).unwrap();
            v["seed_name"] = json!(seed);
            std::fs::write(dir.join(format!("{seed}.json")), v.to_string()).unwrap();
        }
        dir
    }

    /// Rooms come from the database: a restart serves every stored room even when the folder
    /// no longer has its file.
    #[tokio::test]
    async fn a_restart_serves_the_stored_rooms_from_the_database_with_an_empty_folder() {
        let db = database().await;
        let dir = rooms_dir("directory", &["alpha"]);
        let env = [("CONSTELLATION_DATABASE_URL", db.as_str())];
        let server = Server::start_rooms(Path::new(BIN), &dir, &[], &env).unwrap();
        let a = key_of(&server, "alpha");
        let url = |s: &Server| format!("ws://{a}.example:{}", s.addr.port());
        let loc = locations_for(1, 2).unwrap()[0].0;
        let (mut alice, _, _) = e2e::join_via(server.addr, &url(&server), "Alice").await.unwrap();
        alice.check(&[loc]).await.unwrap();
        alice.next_cmd("RoomUpdate").await.unwrap();
        assert!(server.stop().unwrap().success());
        std::fs::remove_file(dir.join("alpha.json")).unwrap();

        // Nothing of the world is in a file now: the room wakes from its stored seed.
        let server = Server::start_rooms(Path::new(BIN), &dir, &[], &env).unwrap();
        assert_eq!(key_of(&server, "alpha"), a);
        let (_, room, _) = e2e::join_via(server.addr, &url(&server), "Alice").await.unwrap();
        assert_eq!(locations(&room["checked_locations"]), [loc]);
        assert_eq!(count(&db, "SELECT count(*) FROM room_seeds").await, 1);
        drop(std::fs::remove_dir_all(&dir));
    }

    /// D-30: an abandoned room is marked finished in the database, keeps its data, and is not
    /// served again after a restart, even though its file is still in the folder.
    #[tokio::test]
    async fn an_abandoned_room_stays_ended_across_a_restart() {
        let db = database().await;
        let dir = rooms_dir("abandon", &["alpha"]);
        let fast = [
            ("CONSTELLATION_DATABASE_URL", db.as_str()),
            ("CONSTELLATION_ROOMS_SCAN_MS", "100"),
            ("CONSTELLATION_ROOM_ABANDON_SECS", "1"),
        ];
        let server = Server::start_rooms(Path::new(BIN), &dir, &[], &fast).unwrap();
        let a = key_of(&server, "alpha");
        server.wait_for_log("room abandoned").unwrap();
        assert!(server.stop().unwrap().success());
        {
            use sqlx::Connection;
            let mut c = sqlx::PgConnection::connect(&db).await.unwrap();
            let reason: Option<String> = sqlx::query_scalar("SELECT finished_reason FROM rooms")
                .fetch_one(&mut c)
                .await
                .unwrap();
            assert_eq!(reason.as_deref(), Some("abandoned"));
        }

        let env = [("CONSTELLATION_DATABASE_URL", db.as_str())];
        let server = Server::start_rooms(Path::new(BIN), &dir, &[], &env).unwrap();
        server.wait_for_log("has finished (abandoned)").unwrap();
        let url = format!("ws://{a}.example:{}", server.addr.port());
        assert!(e2e::Client::connect_via(server.addr, &url).await.unwrap_err().contains("404"));
        drop(std::fs::remove_dir_all(&dir));
    }
}
