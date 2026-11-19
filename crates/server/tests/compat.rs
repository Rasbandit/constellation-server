//! End-to-end scenarios for the Archipelago-compatible edge (`just e2e`): the real binary, the real fixture
//! seed, real WebSocket connections on loopback. Each test owns its server, so they run in parallel.
//! Expected values come from the fixture bundle (the importer's output) and the protocol
//! document's field tables.
#![allow(
    unused_crate_dependencies,
    reason = "an integration test sees every dependency of the e2e helper library"
)]

#[cfg(test)]
mod scenarios {
    use e2e::{Client, Server, connect_packet, fixture};
    use std::path::Path;

    /// Built by cargo for this package's integration tests.
    const BIN: &str = env!("CARGO_BIN_EXE_constellation-server");

    fn start(extra: &[&str]) -> e2e::Res<Server> {
        Server::start(Path::new(BIN), extra, &[])
    }

    fn start_env(extra: &[&str], env: &[(&str, &str)]) -> e2e::Res<Server> {
        Server::start(Path::new(BIN), extra, env)
    }
    use serde_json::{Value, json};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const GAME: &str = "Archipela-Go 2: Electric Boogaloo";

    /// Alice's (slot 1) locations whose item goes to `receiver`, from the fixture.
    fn alice_locations_for(receiver: u32) -> Vec<(i64, i64, u8)> {
        e2e::locations_for(1, receiver).unwrap()
    }

    async fn joined(server: &Server, name: &str) -> (Client, Value, Vec<Value>) {
        e2e::join(&server.url(), name).await.unwrap()
    }

    async fn check(c: &mut Client, locations: &[i64]) {
        c.check(locations).await.unwrap();
    }

    #[tokio::test]
    async fn room_info_arrives_first_and_names_the_fixture_seed() {
        let server = start(&[]).unwrap();
        let mut c = Client::connect(&server.url()).await.unwrap();
        let info = c.next().await.unwrap();
        let b = fixture().unwrap();
        assert_eq!(info["cmd"], "RoomInfo");
        assert_eq!(info["seed_name"], b.seed_name);
        assert_eq!(info["password"], false);
        // The generic game names location -2 and player 0 of start items (protocol doc, "Data
        // Package Contents"); it is listed next to the seed's game.
        assert_eq!(info["games"], json!([GAME, "Archipelago"]));
        assert_eq!(info["datapackage_checksums"]["Archipelago"], b.games["Archipelago"].checksum);
        assert_eq!(info["datapackage_checksums"][GAME], b.games[GAME].checksum);
        assert_eq!(info["hint_cost"], 10);
    }

    #[tokio::test]
    async fn get_data_package_returns_the_game_names_and_ids() {
        let server = start(&[]).unwrap();
        let mut c = Client::connect(&server.url()).await.unwrap();
        c.next().await.unwrap();
        c.send(&json!([{"cmd": "GetDataPackage", "games": [GAME]}])).await.unwrap();
        let dp = c.next().await.unwrap();
        let b = fixture().unwrap();
        let game = &dp["data"]["games"][GAME];
        assert_eq!(dp["cmd"], "DataPackage");
        assert_eq!(game["checksum"], b.games[GAME].checksum);
        let ids = game["location_name_to_id"].as_object().unwrap();
        assert_eq!(ids.len(), b.games[GAME].location_name_to_id.len());
    }

    #[tokio::test]
    async fn connected_lists_the_slot_and_every_location_as_missing() {
        let server = start(&[]).unwrap();
        let (_c, connected, _) = joined(&server, "Alice").await;
        let b = fixture().unwrap();
        let mut want: Vec<i64> = b.slot(1).unwrap().locations.iter().map(|p| p.location).collect();
        want.sort_unstable();
        assert_eq!((connected["team"].clone(), connected["slot"].clone()), (json!(0), json!(1)));
        assert_eq!(connected["missing_locations"], json!(want));
        assert_eq!(connected["checked_locations"], json!([]));
        assert_eq!(connected["slot_info"]["2"]["name"], "Bob");
        assert_eq!(connected["slot_info"]["2"]["game"], GAME);
        assert_eq!(connected["slot_data"], b.slot(1).unwrap().slot_data);
    }

    #[tokio::test]
    async fn a_check_of_another_slots_item_is_delivered_to_that_slot_with_its_index() {
        let server = start(&[]).unwrap();
        let (mut alice, _, _) = joined(&server, "Alice").await;
        let (mut bob, _, _) = joined(&server, "Bob").await;
        let (loc, item, flags) = alice_locations_for(2)[0];
        // Bob's starting inventory comes first, at index 0, as he asked for it (bit 2).
        let start = e2e::start_items(2).unwrap();
        let (got, _) = bob.next_cmd("ReceivedItems").await.unwrap();
        assert_eq!(got, json!({"cmd": "ReceivedItems", "index": 0, "items": start}));
        check(&mut alice, &[loc]).await;
        let (got, _) = bob.next_cmd("ReceivedItems").await.unwrap();
        assert_eq!(
            got,
            json!({"cmd": "ReceivedItems", "index": start.len(),
            "items": [{"class": "NetworkItem", "item": item, "location": loc, "player": 1, "flags": flags}]})
        );
        let (update, _) = alice.next_cmd("RoomUpdate").await.unwrap();
        assert_eq!(update["checked_locations"], json!([loc]));
    }

    #[tokio::test]
    async fn a_check_of_an_own_item_comes_back_to_the_sender() {
        let server = start(&[]).unwrap();
        let (mut alice, _, _) = joined(&server, "Alice").await;
        let (loc, item, flags) = alice_locations_for(1)[0];
        check(&mut alice, &[loc]).await;
        let (got, _) = alice.next_cmd("ReceivedItems").await.unwrap();
        assert_eq!(got["index"], 0);
        assert_eq!(
            got["items"],
            json!([{"class": "NetworkItem", "item": item, "location": loc, "player": 1, "flags": flags}])
        );
    }

    #[tokio::test]
    async fn a_repeated_check_delivers_nothing_a_second_time() {
        let server = start(&[]).unwrap();
        let (mut alice, _, _) = joined(&server, "Alice").await;
        let (mut bob, _, _) = joined(&server, "Bob").await;
        let to_bob = alice_locations_for(2);
        let start = e2e::start_items(2).unwrap().len();
        bob.next_cmd("ReceivedItems").await.unwrap(); // the starting inventory
        check(&mut alice, &[to_bob[0].0, to_bob[0].0]).await;
        check(&mut alice, &[to_bob[0].0]).await;
        // The signal: a fresh check after the repeats. Bob's next item must be that one, right
        // after the first.
        check(&mut alice, &[to_bob[1].0]).await;
        let (first, _) = bob.next_cmd("ReceivedItems").await.unwrap();
        let (second, _) = bob.next_cmd("ReceivedItems").await.unwrap();
        assert_eq!(first["index"], start);
        assert_eq!(
            (second["index"].clone(), second["items"][0]["location"].clone()),
            (json!(start + 1), json!(to_bob[1].0))
        );
    }

    #[tokio::test]
    async fn a_reconnect_resends_every_item_from_index_zero_and_sync_agrees() {
        let server = start(&[]).unwrap();
        let (mut alice, _, _) = joined(&server, "Alice").await;
        let to_bob: Vec<i64> = alice_locations_for(2).iter().take(3).map(|p| p.0).collect();
        check(&mut alice, &to_bob).await;
        // The signal that the checks are durable, so Bob's first list holds them: his start
        // items always make a first list now, so it must not race the checks.
        alice.next_cmd("RoomUpdate").await.unwrap();
        let (mut bob, _, _) = joined(&server, "Bob").await;
        let start = e2e::start_items(2).unwrap();
        let (first, _) = bob.next_cmd("ReceivedItems").await.unwrap();
        assert_eq!(first["index"], 0);
        assert_eq!(first["items"].as_array().unwrap().len(), start.len() + 3);
        bob.close().await.unwrap();
        let extra = alice_locations_for(2)[3].0;
        check(&mut alice, &[extra]).await;
        alice.next_cmd("RoomUpdate").await.unwrap();
        let (mut bob, connected, _) = joined(&server, "Bob").await;
        assert_eq!(connected["slot"], 2);
        let (again, _) = bob.next_cmd("ReceivedItems").await.unwrap();
        assert_eq!(again["index"], 0, "index 0 means: this is the whole inventory");
        let locations: Vec<Value> =
            again["items"].as_array().unwrap().iter().map(|i| i["location"].clone()).collect();
        let mut want: Vec<Value> = start.iter().map(|_| json!(-2)).collect();
        want.extend(to_bob.iter().map(|l| json!(l)));
        want.push(json!(extra));
        assert_eq!(locations, want);
        bob.send(&json!([{"cmd": "Sync"}])).await.unwrap();
        let (synced, _) = bob.next_cmd("ReceivedItems").await.unwrap();
        assert_eq!(synced, again);
        let (_, alice_view, _) = joined(&server, "Alice").await;
        assert_eq!(alice_view["checked_locations"].as_array().unwrap().len(), 4);
    }

    #[tokio::test]
    async fn a_goal_status_update_is_announced_and_readable() {
        let server = start(&[]).unwrap();
        let (mut alice, _, _) = joined(&server, "Alice").await;
        let (mut bob, _, _) = joined(&server, "Bob").await;
        alice.send(&json!([{"cmd": "StatusUpdate", "status": 30}])).await.unwrap();
        let goal = loop {
            let (p, _) = bob.next_cmd("PrintJSON").await.unwrap();
            if p["type"] == "Goal" {
                break p;
            }
        };
        assert_eq!((goal["team"].clone(), goal["slot"].clone()), (json!(0), json!(1)));
        bob.send(&json!([{"cmd": "Get", "keys": ["_read_client_status_0_1", "_read_client_status_0_2"]}]))
            .await
            .unwrap();
        let (got, _) = bob.next_cmd("Retrieved").await.unwrap();
        // Bob is connected (5) and has not reached his goal.
        assert_eq!(
            got["keys"],
            json!({"_read_client_status_0_1": 30, "_read_client_status_0_2": 5})
        );
    }

    #[tokio::test]
    async fn a_bad_password_is_refused_with_invalid_password() {
        let server = start_env(&[], &[("CONSTELLATION_ROOM_PASSWORD", "secret")]).unwrap();
        let mut c = Client::connect(&server.url()).await.unwrap();
        assert_eq!(c.next().await.unwrap()["password"], true);
        c.send(&connect_packet("Alice", Some("wrong"))).await.unwrap();
        assert_eq!(
            c.next().await.unwrap(),
            json!({"cmd": "ConnectionRefused", "errors": ["InvalidPassword"]})
        );
        c.send(&connect_packet("Alice", Some("secret"))).await.unwrap();
        assert_eq!(c.next_cmd("Connected").await.unwrap().0["slot"], 1);
    }

    #[tokio::test]
    async fn an_unknown_slot_name_is_refused_with_invalid_slot() {
        let server = start(&[]).unwrap();
        let mut c = Client::connect(&server.url()).await.unwrap();
        c.next().await.unwrap();
        c.send(&connect_packet("Mallory", None)).await.unwrap();
        assert_eq!(
            c.next().await.unwrap(),
            json!({"cmd": "ConnectionRefused", "errors": ["InvalidSlot"]})
        );
    }

    #[tokio::test]
    async fn garbage_gets_invalid_packet_and_the_connection_keeps_working() {
        let server = start(&[]).unwrap();
        let mut c = Client::connect(&server.url()).await.unwrap();
        c.next().await.unwrap();
        for text in ["{not json", "{\"cmd\":\"Connect\"}", "[{\"cmd\":\"Fly\"}]"] {
            c.send_text(text).await.unwrap();
            let p = c.next().await.unwrap();
            assert_eq!(
                (p["cmd"].clone(), p["type"].clone()),
                (json!("InvalidPacket"), json!("cmd")),
                "{text}"
            );
        }
        c.send(&connect_packet("Alice", None)).await.unwrap();
        assert_eq!(c.next_cmd("Connected").await.unwrap().0["slot"], 1);
    }

    /// The raw upgrade response to a handshake that offers `extensions` (or none).
    async fn handshake_headers(server: &Server, extensions: Option<&str>) -> String {
        let mut tcp = tokio::net::TcpStream::connect(server.addr).await.unwrap();
        let offer =
            extensions.map(|e| format!("Sec-WebSocket-Extensions: {e}\r\n")).unwrap_or_default();
        let req = format!(
            "GET / HTTP/1.1\r\nHost: {}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
             Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n{offer}\r\n",
            server.addr
        );
        tcp.write_all(req.as_bytes()).await.unwrap();
        let mut buf = Vec::new();
        while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
            let mut chunk = [0_u8; 512];
            let n = tokio::time::timeout(e2e::GUARD, tcp.read(&mut chunk)).await.unwrap().unwrap();
            assert_ne!(n, 0, "closed before the headers ended");
            buf.extend_from_slice(&chunk[..n]);
        }
        let text = String::from_utf8_lossy(&buf).to_lowercase();
        text.split("\r\n\r\n").next().unwrap().to_owned()
    }

    #[tokio::test]
    async fn compression_is_negotiated_when_offered_and_not_otherwise() {
        let server = start(&[]).unwrap();
        let head =
            handshake_headers(&server, Some("permessage-deflate; client_max_window_bits")).await;
        // RFC 6455 section 1.3's sample key and its accept value.
        assert!(head.starts_with("http/1.1 101"), "{head}");
        assert!(head.contains("sec-websocket-accept: s3pplmbitxaq9kygzzhzrbk+xoo="), "{head}");
        let ext = head.lines().find(|l| l.starts_with("sec-websocket-extensions:")).unwrap_or("");
        assert!(ext.contains("permessage-deflate"), "{head}");
        assert!(ext.contains("server_no_context_takeover"), "{head}");
        // A repeated offer whose first entry the server cannot honour still gets deflate.
        let head = handshake_headers(&server, Some("x-unknown, permessage-deflate")).await;
        assert!(head.contains("permessage-deflate"), "{head}");
        let plain = handshake_headers(&server, None).await;
        assert!(!plain.contains("sec-websocket-extensions"), "{plain}");
    }

    /// RFC 7692 section 7.1.1.1: a server may answer `client_no_context_takeover` to an offer
    /// without it, and a client must then reset its compressor per message. The server then
    /// keeps no inflate state per connection between messages (it returns to the shared pool).
    #[tokio::test]
    async fn the_server_asks_clients_to_compress_each_message_alone() {
        let server = start(&[]).unwrap();
        for offer in ["permessage-deflate", "permessage-deflate; client_max_window_bits"] {
            let head = handshake_headers(&server, Some(offer)).await;
            let ext = head.lines().find(|l| l.starts_with("sec-websocket-extensions:"));
            assert!(ext.unwrap_or("").contains("client_no_context_takeover"), "{offer}: {head}");
        }
        // And a compressing client (yawc, which honours it) still plays: connect and sync.
        let (mut c, connected, _) = joined(&server, "Alice").await;
        assert_eq!(connected["cmd"], "Connected");
        c.send(&json!([{"cmd": "Sync"}, {"cmd": "Get", "keys": ["a"]}])).await.unwrap();
        c.next_cmd("Retrieved").await.unwrap();
    }

    #[tokio::test]
    async fn healthz_answers_ok() {
        let server = start(&[]).unwrap();
        let mut tcp = tokio::net::TcpStream::connect(server.addr).await.unwrap();
        tcp.write_all(b"GET /healthz HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut body = String::new();
        tokio::time::timeout(e2e::GUARD, tcp.read_to_string(&mut body)).await.unwrap().unwrap();
        assert!(body.starts_with("HTTP/1.1 200"), "{body}");
        assert!(body.ends_with("ok"), "{body}");
    }

    /// A rooms folder with the fixture under each seed name, and the server on it.
    fn rooms_server(name: &str, seeds: &[&str]) -> (Server, std::path::PathBuf) {
        rooms_server_env(name, seeds, &[])
    }

    fn rooms_server_env(
        name: &str,
        seeds: &[&str],
        env: &[(&str, &str)],
    ) -> (Server, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("e2e-rooms-{name}-{}", std::process::id()));
        drop(std::fs::remove_dir_all(&dir));
        std::fs::create_dir_all(&dir).unwrap();
        for seed in seeds {
            add_bundle(&dir, seed);
        }
        (Server::start_rooms(Path::new(BIN), &dir, &[], env).unwrap(), dir)
    }

    /// Put the fixture under `seed` into the folder the way an operator should: written under
    /// another name, then renamed into place, so a scan never reads half a file.
    fn add_bundle(dir: &Path, seed: &str) {
        let mut v: Value =
            serde_json::from_str(&std::fs::read_to_string(e2e::fixture_path()).unwrap()).unwrap();
        v["seed_name"] = json!(seed);
        let part = dir.join(format!("{seed}.json.part"));
        std::fs::write(&part, v.to_string()).unwrap();
        std::fs::rename(part, dir.join(format!("{seed}.json"))).unwrap();
    }

    /// The room key the server logged for `seed`.
    fn key_of(server: &Server, seed: &str) -> String {
        let line = server
            .log()
            .into_iter()
            .find(|l| l.contains("room ready") && l.contains(&format!("seed=\"{seed}\"")))
            .unwrap_or_else(|| panic!("no room ready line for {seed}: {:?}", server.log()));
        line.split("key=").nth(1).unwrap().split_whitespace().next().unwrap().to_owned()
    }

    /// A WebSocket URL naming `key` in the host; the TCP connection goes to the server.
    fn room_url(server: &Server, key: &str) -> String {
        format!("ws://{key}.example:{}", server.addr.port())
    }

    #[tokio::test]
    async fn a_rooms_folder_starts_one_room_per_bundle_each_with_a_random_key() {
        let (server, dir) = rooms_server("start", &["alpha", "beta"]);
        let (a, b) = (key_of(&server, "alpha"), key_of(&server, "beta"));
        assert_ne!(a, b);
        for key in [&a, &b] {
            assert_eq!(key.len(), 10, "{key}");
            assert!(key.bytes().all(|c| matches!(c, b'a'..=b'z' | b'2'..=b'7')), "{key}");
        }
        assert!(server.log().iter().any(|l| l.contains("listening") && l.contains("rooms=2")));
        let health = e2e::http_get(server.addr, "x", "/healthz", false).await;
        assert!(health.starts_with("HTTP/1.1 200"), "{health}");
        // In memory, a restart draws new keys (the rooms are lost anyway).
        drop(server);
        let again = Server::start_rooms(Path::new(BIN), &dir, &[], &[]).unwrap();
        assert_ne!(key_of(&again, "alpha"), a);
        drop(std::fs::remove_dir_all(&dir));
    }

    #[tokio::test]
    async fn each_key_in_the_host_reaches_its_own_room() {
        let (server, dir) = rooms_server("route", &["alpha", "beta"]);
        let (a, b) = (key_of(&server, "alpha"), key_of(&server, "beta"));
        let (loc, item, _) = alice_locations_for(2)[0];
        // Alice checks a location in room A; Bob in room A gets the item.
        let (mut alice_a, _, _) =
            e2e::join_via(server.addr, &room_url(&server, &a), "Alice").await.unwrap();
        let (mut bob_a, _, _) =
            e2e::join_via(server.addr, &room_url(&server, &a), "Bob").await.unwrap();
        bob_a.next_cmd("ReceivedItems").await.unwrap(); // starting inventory
        check(&mut alice_a, &[loc]).await;
        let (got, _) = bob_a.next_cmd("ReceivedItems").await.unwrap();
        assert_eq!(got["items"][0]["item"], item);
        // Room B never saw that check; room A did. Upper case in the host is the same key.
        let (_, connected_b, _) =
            e2e::join_via(server.addr, &room_url(&server, &b.to_ascii_uppercase()), "Alice")
                .await
                .unwrap();
        assert_eq!(connected_b["checked_locations"], json!([]));
        let (_, connected_a, _) =
            e2e::join_via(server.addr, &room_url(&server, &a), "Alice").await.unwrap();
        assert_eq!(connected_a["checked_locations"], json!([loc]));
        assert!(server.log().iter().any(|l| l.contains("websocket accepted") && l.contains(&a)));
        drop(std::fs::remove_dir_all(&dir));
    }

    #[tokio::test]
    async fn an_unknown_or_missing_key_is_refused_with_404() {
        let (server, dir) = rooms_server("unknown", &["alpha", "beta"]);
        let unknown = e2e::Client::connect_via(server.addr, &room_url(&server, "zzzzzzzzzz")).await;
        assert!(unknown.unwrap_err().contains("404"));
        // No key in the host at all: with two rooms the server does not guess.
        let none = e2e::Client::connect(&server.url()).await;
        assert!(none.unwrap_err().contains("404"));
        let ws = e2e::http_get(server.addr, "x", "/", true).await;
        assert!(ws.contains("this server runs 2 rooms"), "{ws}");
        drop(std::fs::remove_dir_all(&dir));
    }

    #[tokio::test]
    async fn a_bundle_added_to_the_folder_becomes_a_room_while_the_server_runs() {
        let fast = [("CONSTELLATION_ROOMS_SCAN_MS", "100")];
        let (server, dir) = rooms_server_env("live-add", &["alpha"], &fast);
        let alpha = key_of(&server, "alpha");
        let (mut alice_a, _, _) =
            e2e::join_via(server.addr, &room_url(&server, &alpha), "Alice").await.unwrap();
        add_bundle(&dir, "beta");
        server.wait_for_log("seed=\"beta\"").unwrap();
        let beta = key_of(&server, "beta");
        assert_ne!(alpha, beta);
        e2e::join_via(server.addr, &room_url(&server, &beta), "Alice").await.unwrap();
        // The open connection to alpha did not notice.
        alice_a.send(&json!([{"cmd": "Get", "keys": []}])).await.unwrap();
        assert_eq!(alice_a.next_cmd("Retrieved").await.unwrap().0["keys"], json!({}));
        drop(std::fs::remove_dir_all(&dir));
    }

    #[tokio::test]
    async fn an_idle_room_leaves_memory_and_its_next_connect_loads_it_with_its_checks() {
        let env = [("CONSTELLATION_ROOMS_SCAN_MS", "100"), ("CONSTELLATION_ROOM_IDLE_SECS", "1")];
        let (server, dir) = rooms_server_env("idle", &["alpha", "beta"], &env);
        let alpha = key_of(&server, "alpha");
        let (loc, _, _) = alice_locations_for(2)[0];
        let (mut alice, _, _) =
            e2e::join_via(server.addr, &room_url(&server, &alpha), "Alice").await.unwrap();
        check(&mut alice, &[loc]).await;
        alice.next_cmd("RoomUpdate").await.unwrap();
        alice.close().await.unwrap();
        server.wait_for_log("room idle; left memory").unwrap();
        let (_, connected, _) = e2e::join_via(server.addr, &room_url(&server, &alpha), "Alice")
            .await
            .unwrap_or_else(|e| panic!("{e}; log:\n{}", server.log().join("\n")));
        assert_eq!(connected["checked_locations"], json!([loc]));
        let loads = server.log().iter().filter(|l| l.contains("room in memory")).count();
        assert_eq!(loads, 2, "loaded on the first connect and again after it idled");
        drop(std::fs::remove_dir_all(&dir));
    }

    #[tokio::test]
    async fn an_abandoned_room_ends_and_is_refused() {
        let env =
            [("CONSTELLATION_ROOMS_SCAN_MS", "100"), ("CONSTELLATION_ROOM_ABANDON_SECS", "1")];
        let (server, dir) = rooms_server_env("abandon", &["alpha"], &env);
        let alpha = key_of(&server, "alpha");
        server.wait_for_log("room abandoned").unwrap();
        let again = e2e::Client::connect_via(server.addr, &room_url(&server, &alpha)).await;
        assert!(again.unwrap_err().contains("404"));
        assert!(dir.join("alpha.json").exists(), "the file is left alone");
        drop(std::fs::remove_dir_all(&dir));
    }

    #[tokio::test]
    async fn one_bundle_takes_every_host() {
        let server = start(&[]).unwrap();
        // A host whose first label only looks like a key still reaches the one room.
        e2e::join_via(server.addr, &room_url(&server, "playground"), "Alice").await.unwrap();
        e2e::join(&server.url(), "Bob").await.unwrap();
    }

    /// The number of listed rooms changes while a rooms folder runs (a scan adds one, D-30
    /// ends one), so routing never depends on it: an old link to another room must not land
    /// in the only room left.
    #[tokio::test]
    async fn a_rooms_folder_with_one_room_refuses_another_key_and_no_key() {
        let (server, dir) = rooms_server("one", &["alpha"]);
        let alpha = key_of(&server, "alpha");
        let other = e2e::Client::connect_via(server.addr, &room_url(&server, "zzzzzzzzzz")).await;
        assert!(other.unwrap_err().contains("404"));
        assert!(e2e::Client::connect(&server.url()).await.unwrap_err().contains("404"));
        e2e::join_via(server.addr, &room_url(&server, &alpha), "Alice").await.unwrap();
        drop(std::fs::remove_dir_all(&dir));
    }

    #[tokio::test]
    async fn the_password_can_come_from_a_file_and_never_from_the_command_line() {
        let dir = std::env::temp_dir().join(format!("e2e-pw-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("pw");
        std::fs::write(&path, "from-file\n").unwrap();
        let server =
            start_env(&[], &[("CONSTELLATION_ROOM_PASSWORD_FILE", path.to_str().unwrap())])
                .unwrap();
        let mut c = Client::connect(&server.url()).await.unwrap();
        c.next().await.unwrap();
        c.send(&connect_packet("Alice", Some("from-file"))).await.unwrap();
        assert_eq!(c.next_cmd("Connected").await.unwrap().0["slot"], 1);
        assert!(start(&["--password", "x"]).is_err(), "the flag is gone");
    }

    #[tokio::test]
    async fn connections_over_the_cap_get_503_and_a_freed_slot_is_reused() {
        let server = start_env(&[], &[("CONSTELLATION_MAX_CONNECTIONS", "1")]).unwrap();
        let mut first = Client::connect(&server.url()).await.unwrap();
        first.next().await.unwrap();
        let head = handshake_headers(&server, None).await;
        assert!(head.starts_with("http/1.1 503"), "{head}");
        first.close().await.unwrap();
        server.wait_for_log("connection closed").unwrap();
        let mut again = Client::connect(&server.url()).await.unwrap();
        assert_eq!(again.next().await.unwrap()["cmd"], "RoomInfo");
    }

    #[tokio::test]
    async fn a_socket_that_never_connects_to_a_slot_is_closed_and_a_connected_one_is_not() {
        // 2 s is generous for a loopback Connect, so the timely client never races the timer.
        let server = start_env(&[], &[("CONSTELLATION_CONNECT_TIMEOUT_MS", "2000")]).unwrap();
        let mut idle = Client::connect(&server.url()).await.unwrap();
        let mut ok = Client::connect(&server.url()).await.unwrap();
        assert_eq!(idle.next().await.unwrap()["cmd"], "RoomInfo");
        ok.next().await.unwrap();
        ok.send(&connect_packet("Alice", None)).await.unwrap();
        assert_eq!(ok.next_cmd("Connected").await.unwrap().0["slot"], 1);
        let end = idle.next().await.unwrap_err();
        assert!(end.contains("closed") || end.contains("socket"), "{end}");
        server.wait_for_log("connect timeout").unwrap();
        // The timer has fired for the other socket; the connected one still answers.
        ok.send(&json!([{"cmd": "Get", "keys": []}])).await.unwrap();
        assert_eq!(ok.next_cmd("Retrieved").await.unwrap().0["keys"], json!({}));
    }

    #[tokio::test]
    async fn a_quiet_client_that_answers_pings_stays_open_past_the_idle_timeout() {
        let env =
            [("CONSTELLATION_IDLE_TIMEOUT_MS", "300"), ("CONSTELLATION_PING_INTERVAL_MS", "50")];
        let server = start_env(&[], &env).unwrap();
        let mut c = Client::connect(&server.url()).await.unwrap();
        c.next().await.unwrap();
        c.send(&connect_packet("Alice", None)).await.unwrap();
        c.next_cmd("Connected").await.unwrap();
        // The server's own pings are the clock: 12 of them, 50 ms apart, span well past 300 ms,
        // and the client only answers them (yawc replies to pings while reading).
        c.wait_pings(12).await.unwrap();
        c.send(&json!([{"cmd": "Get", "keys": []}])).await.unwrap();
        assert_eq!(c.next_cmd("Retrieved").await.unwrap().0["cmd"], "Retrieved");
        assert!(!server.log().iter().any(|l| l.contains("idle timeout")), "closed as idle");
    }

    #[tokio::test]
    async fn a_peer_that_stops_answering_pings_is_closed_as_idle() {
        let env =
            [("CONSTELLATION_IDLE_TIMEOUT_MS", "300"), ("CONSTELLATION_PING_INTERVAL_MS", "100")];
        let server = start_env(&[], &env).unwrap();
        // A raw socket: upgraded, then silent; it never answers the server's pings.
        let mut tcp = tokio::net::TcpStream::connect(server.addr).await.unwrap();
        let req = format!(
            "GET / HTTP/1.1\r\nHost: {}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
             Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n",
            server.addr
        );
        tcp.write_all(req.as_bytes()).await.unwrap();
        let mut sink = Vec::new();
        tokio::time::timeout(e2e::GUARD, tcp.read_to_end(&mut sink)).await.unwrap().unwrap();
        server.wait_for_log("idle timeout").unwrap();
    }

    #[test]
    fn the_server_stops_cleanly_on_sigterm() {
        let server = start(&[]).unwrap();
        let status = server.stop().unwrap();
        assert!(status.success(), "{status}");
    }

    /// A supervisor may stop the server before it listens. Startup is held at its database
    /// connection (a socket that accepts and never answers); SIGTERM there is a clean stop,
    /// not a death by signal and not a wait for the database.
    #[tokio::test]
    async fn sigterm_during_startup_is_a_clean_stop() {
        let db = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("postgres://u:p@{}/d", db.local_addr().unwrap());
        let cmd = e2e::command(Path::new(BIN), &[], &[("CONSTELLATION_DATABASE_URL", &url)]);
        let child = tokio::process::Command::from(cmd).kill_on_drop(true).spawn().unwrap();
        let _held = tokio::time::timeout(e2e::GUARD, db.accept()).await.unwrap().unwrap();
        e2e::sigterm(child.id().unwrap()).unwrap();
        let out =
            tokio::time::timeout(e2e::GUARD, child.wait_with_output()).await.unwrap().unwrap();
        let log = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{}; log:\n{log}", out.status);
        assert!(log.contains("SIGTERM"), "{log}");
    }

    /// Data storage over real sockets (protocol doc, "Get", "Set", "`SetNotify`", "`SetReply`"):
    /// a watcher hears a write with the writer's extra fields, the writer gets its one reply,
    /// and `Get` reads the stored value and the fixture's name groups.
    #[tokio::test]
    async fn data_storage_round_trips_between_two_real_clients() {
        let server = start(&[]).unwrap();
        let (mut alice, _, _) = joined(&server, "Alice").await;
        let (mut bob, _, _) = joined(&server, "Bob").await;
        bob.send(&json!([{"cmd": "SetNotify", "keys": ["shared"]}])).await.unwrap();
        // SetNotify answers nothing; this Get is the signal that it was handled.
        bob.send(&json!([{"cmd": "Get", "keys": ["shared"]}])).await.unwrap();
        let (got, _) = bob.next_cmd("Retrieved").await.unwrap();
        assert_eq!(got["keys"], json!({"shared": null}));
        let set = json!([{"cmd": "Set", "key": "shared", "default": [], "want_reply": true,
            "operations": [{"operation": "update", "value": ["a", "b"]},
                           {"operation": "remove", "value": "a"}], "tag": "e2e"}]);
        alice.send(&set).await.unwrap();
        let want = json!({"cmd": "SetReply", "key": "shared", "value": ["b"], "original_value": [],
            "slot": 1, "default": [], "want_reply": true, "tag": "e2e",
            "operations": [{"operation": "update", "value": ["a", "b"]},
                           {"operation": "remove", "value": "a"}]});
        assert_eq!(alice.next_cmd("SetReply").await.unwrap().0, want);
        assert_eq!(bob.next_cmd("SetReply").await.unwrap().0, want);
        let b = fixture().unwrap();
        let groups = format!("_read_item_name_groups_{GAME}");
        bob.send(&json!([{"cmd": "Get", "keys": ["shared", groups]}])).await.unwrap();
        let (got, _) = bob.next_cmd("Retrieved").await.unwrap();
        assert_eq!(got["keys"]["shared"], json!(["b"]));
        assert_eq!(got["keys"][&groups], json!(b.games[GAME].item_name_groups));
        assert!(!b.games[GAME].item_name_groups.is_empty(), "the fixture has item groups");
    }

    /// A watcher of a slot's client status hears it connect (5) and its last connection leave
    /// (0) (reference `MultiServer.py:1043-1046, 1071-1074`).
    #[tokio::test]
    async fn a_slots_client_status_follows_its_connections() {
        let server = start(&[]).unwrap();
        let (mut alice, _, _) = joined(&server, "Alice").await;
        let key = "_read_client_status_0_2";
        alice.send(&json!([{"cmd": "SetNotify", "keys": [key]}])).await.unwrap();
        alice.send(&json!([{"cmd": "Get", "keys": [key]}])).await.unwrap();
        assert_eq!(alice.next_cmd("Retrieved").await.unwrap().0["keys"][key], 0);
        let (bob, _, _) = joined(&server, "Bob").await;
        let reply = |v: i64| json!({"cmd": "SetReply", "key": key, "value": v});
        assert_eq!(alice.next_cmd("SetReply").await.unwrap().0, reply(5));
        bob.close().await.unwrap();
        assert_eq!(alice.next_cmd("SetReply").await.unwrap().0, reply(0));
    }
}
