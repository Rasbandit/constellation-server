# Compatibility status: the Archipelago-compatible edge

What `edge-compat` supports today, checked against Archipelago's protocol document (`docs/network protocol.md` in the Archipelago repository, read at the 0.6.8 checkout pinned by Archipela-Go, 2026-11-16). Internal note; anything external needs the review in the design repo's `CLAUDE.md`.

Test names: `e2e::` is `crates/server/tests/compat.rs` (real binary, yawc client), `ap::` is `crates/e2e-ap/src/lib.rs` (the real binary driven by `archipelago_rs` 3.0.1), `unit::` is `crates/edge-compat/src/tests.rs` (a session over a real room task).

## Client to server

| Packet | Status | Proven by |
|---|---|---|
| `Connect` | Supported. Refusals: `InvalidSlot`, `InvalidGame`, `IncompatibleVersion` (below the slot's minimum client version; a client with no game is held to the server-wide 0.5.0), `InvalidPassword`, `InvalidItemsHandling` (not an integer or boolean, or bits 1 or 2 without bit 0; read as a bit set like the reference server, so bits above the three flags are ignored, -1 is every flag and `true` is 1). A refused socket may try again. Trackers (`Tracker`, `TextOnly`, `HintGame` tags) may send an empty game. A re-Connect to another slot announces `Part` then `Join`; to the same slot, nothing. | `e2e::connected_lists_the_slot_and_every_location_as_missing`, `e2e::a_bad_password_is_refused_with_invalid_password`, `e2e::an_unknown_slot_name_is_refused_with_invalid_slot`, `unit::each_bad_connect_is_refused_with_its_error_and_the_socket_can_try_again`, `unit::a_tracker_may_connect_without_a_game_but_may_not_check`, `unit::trackers_meet_the_global_minimum_version_and_games_meet_their_slots`, `unit::items_handling_bits_above_the_three_flags_are_ignored`, `unit::reconnecting_to_another_slot_announces_part_then_join_and_the_same_slot_nothing`, `ap::archipelago_rs_is_refused_for_an_unknown_slot` |
| `GetDataPackage` | Supported, whole or per game. Each game's package is encoded once at start. Rate limited per session (burst 4, then one per 10 s). | `e2e::get_data_package_returns_the_game_names_and_ids`, `unit::the_data_package_is_sent_whole_or_per_game`, `unit::get_data_package_is_rate_limited` |
| `LocationChecks` | Supported. Repeats and unknown ids are ignored; trackers are refused with `InvalidPacket`. | `e2e::a_check_of_another_slots_item_is_delivered_to_that_slot_with_its_index`, `e2e::a_check_of_an_own_item_comes_back_to_the_sender`, `e2e::a_repeated_check_delivers_nothing_a_second_time`, `unit::two_hundred_checks_in_one_packet_reach_a_bystander_that_stays_connected`, `ap::archipelago_rs_connects_checks_receives_reconnects_and_reaches_its_goal` |
| `Sync` | Supported: `ReceivedItems` from index 0 (nothing when there are no items or the client takes none). | `e2e::a_reconnect_resends_every_item_from_index_zero_and_sync_agrees`, `unit::no_empty_received_items_and_none_for_clients_that_take_no_items` |
| `StatusUpdate` | Goal (30) supported and durable; refused (`InvalidPacket`) from trackers and text clients. Other values are accepted and not stored yet. | `e2e::a_goal_status_update_is_announced_and_readable`, `ap::archipelago_rs_connects_checks_receives_reconnects_and_reaches_its_goal`, `unit::a_tracker_cannot_declare_a_goal` |
| `LocationScouts` | Supported: one answer per requested id, in request order; an id outside the slot's world gets `InvalidPacket`. `create_as_hint` is ignored (no hints yet). | `unit::scouts_answer_with_the_receiver_of_each_item`, `unit::scouts_answer_in_request_order_and_refuse_unknown_ids` |
| `ConnectUpdate` | Supported for `items_handling` (resends from index 0) and `tags`; a bad value changes nothing. No `TagsChanged` print yet. | `unit::connect_update_changes_items_handling_and_resends_items`, `unit::a_bad_connect_update_changes_nothing_not_even_the_tags` |
| `Say` | Chat to everyone. `!` commands get a `CommandResult` saying they are not supported yet. Text at most 2 KiB; shares a per-session rate limit with `Bounce` (burst 10, 2 per second). | `unit::chat_reaches_everyone_and_no_text_clients_skip_prints`, `unit::chat_and_bounce_share_a_rate_limit_and_the_excess_is_refused`, `unit::oversized_chat_and_bounce_are_refused_before_they_cost_a_token` |
| `Bounce` | Supported with `or`, `and` and `legacy` targeting; a missing team list means the sender's team. Packet at most 16 KiB; rate limited with `Say`. | `unit::a_bounce_reaches_only_its_targets` |
| `Get` | `_read_client_status_<team>_<slot>` (30 after the goal, else 0), `_read_slot_data_<slot>`, `_read_hints_*` (an empty list: no hints yet), `_read_race_mode` (the seed's); every other key is `null` (no data storage yet). Extra request fields are passed back. | `e2e::a_goal_status_update_is_announced_and_readable`, `unit::a_goal_is_recorded_once_announced_and_readable`, `unit::hints_read_as_empty_and_race_mode_as_the_rooms_value` |
| `Set`, `SetNotify` | Not yet: `InvalidPacket` saying so. | `unit::unsupported_commands_say_so` |
| `CreateHints`, `UpdateHint` | Not yet: `InvalidPacket` saying so. | `unit::unsupported_commands_say_so` |
| Unknown command, bad JSON, bad arguments | `InvalidPacket` (`cmd` or `arguments`), and the connection stays open. | `e2e::garbage_gets_invalid_packet_and_the_connection_keeps_working`, `unit::hostile_frames_get_invalid_packet_and_the_session_survives` |

## Server to client

| Packet | Status | Proven by |
|---|---|---|
| `RoomInfo` | Sent first. `version` is 0.6.8, the protocol level we implement. `permissions` are all disabled because release, collect and remaining are not implemented, whatever the seed chose. `tags` is empty. `games` lists the seed's games only, not the generic `Archipelago` game. | `e2e::room_info_arrives_first_and_names_the_fixture_seed`, `unit::the_first_frame_is_room_info_with_checksums_and_no_password` |
| `ConnectionRefused` | All five documented errors. | see `Connect` |
| `Connected` | `team`, `slot`, `players`, `missing_locations`, `checked_locations`, `slot_data` (unless the client asked for none), `slot_info`, `hint_points`. | `e2e::connected_lists_the_slot_and_every_location_as_missing`, `unit::slot_data_is_left_out_when_the_client_does_not_ask` |
| `ReceivedItems` | Index semantics per the document: index 0 on connect, `Sync` and `ConnectUpdate` is the whole inventory; live items carry the next index, one packet per room batch. Own-world items only with `items_handling` bit 1. A session that falls behind the room resyncs from its cursor and gets the missed items with the right indexes, without disconnecting. | `e2e::a_reconnect_resends_every_item_from_index_zero_and_sync_agrees`, `unit::an_own_item_comes_back_only_to_a_client_that_wants_own_items`, `unit::a_slow_session_resyncs_from_its_cursor_and_gets_every_item_once_in_order` |
| `LocationInfo` | Supported. | see `LocationScouts` |
| `RoomUpdate` | `checked_locations` (new ones) and `hint_points` after a check. Nothing else changes yet. | `e2e::a_check_of_another_slots_item_is_delivered_to_that_slot_with_its_index` |
| `PrintJSON` | `ItemSend` ("sent ... to ..." or "found their ..."), `Join`, `Part`, `Chat`, `Goal`, `CommandResult`, with typed parts (`player_id`, `item_id`, `location_id`). Skipped for `NoText` clients; chat is dropped for a session whose queue is full. `archipelago_rs` hydrates them without errors. | `unit::a_check_sends_the_item_to_its_receiver_with_the_next_index`, `unit::a_self_found_item_reads_found_their`, `ap::archipelago_rs_connects_checks_receives_reconnects_and_reaches_its_goal` |
| `DataPackage`, `Bounced`, `InvalidPacket`, `Retrieved` | Supported. | see the client packets |
| `SetReply` | Not yet. | |

## Transport

| Item | Status | Proven by |
|---|---|---|
| permessage-deflate | Negotiated when offered, with `server_no_context_takeover` and a shared compressor pool (D-97); not sent when not offered. Window bits are left at 15 (doc 11 asks for small windows; deferred to gate G-WS). | `e2e::compression_is_negotiated_when_offered_and_not_otherwise` |
| No compression warning | The server never prints a compression warning, and every socket `archipelago_rs` opened negotiated deflate. | `ap::archipelago_rs_connects_checks_receives_reconnects_and_reaches_its_goal` |
| Caps | Incoming frame and message caps 1 MiB each, read and write buffers 4 KiB, all from `config` (design values). | `config::tests::socket_buffers_and_caps_are_pinned` |
| Connection cap, timeouts | Over `max_connections`, HTTP 503. No slot connected within 30 s: closed. Pinged every 20 s; nothing heard for 60 s: closed. All from `config`. | `e2e::connections_over_the_cap_get_503_and_a_freed_slot_is_reused`, `e2e::a_socket_that_never_connects_to_a_slot_is_closed`, `e2e::a_peer_that_stops_answering_pings_is_closed_as_idle` |
| Room password | From `CONSTELLATION_ROOM_PASSWORD` or a file, never a flag. | `e2e::a_bad_password_is_refused_with_invalid_password`, `e2e::the_password_can_come_from_a_file_and_never_from_the_command_line` |
| `/healthz`, SIGTERM | `ok`; a clean exit. | `e2e::healthz_answers_ok`, `e2e::the_server_stops_cleanly_on_sigterm` |

## Not implemented yet

Release, collect and remaining (and their `!` commands); hints and hint points spending; data storage (`Set`, `SetNotify`, `SetReply`); starting inventory and group slots (item links): the importer and the bundle loader refuse such seeds with a clear message; client statuses other than the goal; `TagsChanged` prints; TLS (put a terminator in front).

## Known interop finding (not a defect of our server)

`archipelago_rs` 3.0.1 as vendored uses `soketto` 0.8.1, which inflates each compressed message by finishing a deflate stream that has no final block. `flate2` 1.1.11 rejects that ("incomplete deflate stream"), so with `flate2` 1.1.11 every compressed frame fails, against any compressing server. `crates/e2e-ap` pins `flate2 = "=1.1.10"`, the version the Archipela-Go app locks. Inferred from `flate2` 1.1.11's own tests and reproduced here; not checked against Archipelago's server.
