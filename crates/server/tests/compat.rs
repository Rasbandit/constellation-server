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
            "items": [{"item": item, "location": loc, "player": 1, "flags": flags}]})
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
            json!([{"item": item, "location": loc, "player": 1, "flags": flags}])
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
        assert_eq!(
            got["keys"],
            json!({"_read_client_status_0_1": 30, "_read_client_status_0_2": 0})
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
}
