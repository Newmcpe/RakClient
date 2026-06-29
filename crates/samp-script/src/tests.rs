use super::*;

#[test]
fn runtime_is_luau() {
    let engine = ScriptEngine::new().expect("vm");
    let version: String = engine.eval("return _VERSION").expect("eval _VERSION");
    assert!(
        version.starts_with("Luau"),
        "expected the Luau runtime, got {version:?}"
    );
}

/// Extract the bytes of a queued `Packet`, panicking on anything else — test helper.
fn packet_data(msg: &OutboundMsg) -> &[u8] {
    match msg {
        OutboundMsg::Packet { data, .. } => data,
        other => panic!("expected packet, got {other:?}"),
    }
}

#[test]
fn text_codec_translates_cyrillic_both_ways() {
    let engine = ScriptEngine::new().expect("vm");
    // "Привет" → cp1251 bytes, and back to the same UTF-8.
    let cp: mlua::String = engine.eval("return utf8ToCp1251('Привет')").unwrap();
    assert_eq!(
        cp.as_bytes().to_vec(),
        vec![0xCF, 0xF0, 0xE8, 0xE2, 0xE5, 0xF2]
    );
    let utf8: String = engine
        .eval("return cp1251ToUtf8(utf8ToCp1251('Привет'))")
        .unwrap();
    assert_eq!(utf8, "Привет");
}

#[test]
fn send_chat_encodes_utf8_to_cp1251() {
    let engine = ScriptEngine::new().expect("vm");
    engine
        .load_script("sampSendChat('Привет')", "c")
        .expect("load");
    assert_eq!(
        engine.drain_outgoing_chat(),
        vec![vec![0xCF, 0xF0, 0xE8, 0xE2, 0xE5, 0xF2]]
    );
}

#[test]
fn lifecycle_fires_and_queues_packet() {
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;

    let engine = ScriptEngine::new().expect("vm");
    let outbox: Outbox = Rc::new(RefCell::new(VecDeque::new()));
    engine.install_sender(outbox.clone()).expect("sender");
    engine
        .load_script(
            // CEF init shape: [220][18] then a u16 length etc. — we just check the prefix here.
            "function sampev.onConnect() sampSendPacket(string.char(220, 18)) end",
            "az",
        )
        .expect("load");
    engine.fire("onConnect", ());
    let msgs: Vec<_> = outbox.borrow_mut().drain(..).collect();
    assert_eq!(msgs.len(), 1);
    assert_eq!(packet_data(&msgs[0]), &[220, 18]);
}

#[test]
fn bitstream_builds_and_sends_packet() {
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;

    let engine = ScriptEngine::new().expect("vm");
    let outbox: Outbox = Rc::new(RefCell::new(VecDeque::new()));
    engine.install_sender(outbox.clone()).expect("sender");
    engine
        .load_script(
            "local bs = bitStream.new()\n\
                 bs:writeInt8(220) bs:writeInt8(18) bs:writeInt16(5)\n\
                 bs:sendPacket()",
            "bs",
        )
        .expect("load");
    let msgs: Vec<_> = outbox.borrow_mut().drain(..).collect();
    assert_eq!(packet_data(&msgs[0]), &[220, 18, 5, 0]);
}

#[test]
fn require_loads_ported_samp_events() {
    let engine = ScriptEngine::new().expect("vm");
    engine.install_sender(empty_outbox()).expect("sender");
    // Requiring the ported library (which transitively loads core/handlers/bitstream_io/utils/
    // extra_types/raknet/sampfuncs) must execute without error.
    engine
        .load_script(
            "local sampev = require('samp.events')\n\
                 local rn = require('samp.raknet')\n\
                 assert(type(sampev) == 'table', 'samp.events table')\n\
                 assert(rn.RPC.CLIENTJOIN ~= nil, 'raknet RPC table')",
            "req",
        )
        .expect("require samp.events");
}

#[test]
fn on_update_fires_registered_handlers() {
    let engine = ScriptEngine::new().expect("vm");
    engine.install_sender(empty_outbox()).expect("sender");
    engine
        .load_script(
            "ticks = 0\nregisterHandler('onUpdate', function() ticks = ticks + 1 end)",
            "u",
        )
        .expect("load");
    engine.dispatch_update();
    engine.dispatch_update();
    assert_eq!(engine.eval::<i64>("return ticks").unwrap(), 2);
}

#[test]
fn addon_tasks_run_on_update() {
    let engine = ScriptEngine::new().expect("vm");
    engine.install_sender(empty_outbox()).expect("sender");
    // addon.lua provides newTask/wait + its own registerHandler('onUpdate'). A waiting task
    // resumes on each tick.
    engine
        .load_script(
            "require('addon')\n\
                 count = 0\n\
                 newTask(function() while true do count = count + 1 wait(0) end end)",
            "t",
        )
        .expect("load");
    let after_create: i64 = engine.eval("return count").unwrap();
    engine.dispatch_update();
    let after_tick: i64 = engine.eval("return count").unwrap();
    assert!(after_tick > after_create, "task resumed on update");
}

#[test]
fn classic_arizona_launcher_runs() {
    assert_arizona_launcher("../../example_scripts/arizona_launcher_emulation_classic.luau");
}

#[test]
fn sugar_arizona_launcher_runs() {
    assert_arizona_launcher("../../example_scripts/arizona_launcher_emulation.luau");
}

/// Load an Arizona launcher example and assert its `onSendClientJoin` rewrites RPC 25 to the
/// Arizona 7-field variant and queues the CEF init packets. The classic (raw-byte) and sugar
/// (typed builder) examples must produce the same observable behaviour.
fn assert_arizona_launcher(rel_path: &str) {
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;

    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(rel_path);
    let source = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {rel_path}: {e}"));

    let engine = ScriptEngine::new().expect("vm");
    let outbox: Outbox = Rc::new(RefCell::new(VecDeque::new()));
    engine.install_sender(outbox.clone()).expect("sender");
    engine
        .install_bindings(samp_client::LocalPlayer::shared(
            "Bot".to_string(),
            "127.0.0.1:7777".parse().expect("addr"),
        ))
        .expect("bindings");
    engine.load_script(&source, rel_path).expect("load");

    // A 7-field ClientJoin (RPC 25) carrying a sample auth key. onSendClientJoin should rewrite it to
    // the Arizona variant (modded=1, "Arizona PC") and PASS THE DRIVER'S auth key (`joinAuthKey`)
    // THROUGH — the launcher examples no longer hardcode one (it is the random `generate_gpci`). It
    // also queues the two CEF packets via sendCef → bitStream:sendPacket.
    let auth = b"263083C359F5AE44AD3AFC8551F4208E6C84A36F6CC";
    let mut join = vec![
        0xD9, 0x0F, 0x00, 0x00, 0x00, 0x03, b'B', b'o', b't', 0x44, 0x33, 0x22, 0x11,
    ];
    join.push(auth.len() as u8);
    join.extend_from_slice(auth);
    join.extend_from_slice(&[0x01, b'x', 0x44, 0x33, 0x22, 0x11]);
    match engine.dispatch_chokepoint("onSendRPC", 25, &join) {
        Verdict::Rewrite(bytes) => {
            assert_eq!(bytes[4], 1, "modded");
            let has = |n: &[u8]| bytes.windows(n.len()).any(|w| w == n);
            assert!(has(b"Arizona PC"), "client version");
            assert!(has(auth), "auth key passed through from joinAuthKey");
        }
        other => panic!("expected rewrite, got {other:?}"),
    }
    let queued = outbox.borrow().len();
    assert!(queued >= 2, "expected 2 CEF packets queued, got {queued}");
}

#[test]
fn arizona_login_answers_auth_dialog() {
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;

    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../example_scripts/arizona_login.luau");
    let source = std::fs::read_to_string(&path).expect("read arizona_login.luau");

    let engine = ScriptEngine::new().expect("vm");
    let outbox: Outbox = Rc::new(RefCell::new(VecDeque::new()));
    engine.install_sender(outbox.clone()).expect("sender");
    engine.load_script(&source, "arizona_login").expect("load");

    // The host hands scripts UTF-8 titles → the login dialog is answered with a DialogResponse (62).
    engine
        .load_script(
            "require('samp.events').onShowDialog(7, 3, 'Авторизация', '', '', '')",
            "auth",
        )
        .expect("auth dialog");
    let msgs: Vec<_> = outbox.borrow_mut().drain(..).collect();
    assert_eq!(msgs.len(), 1, "login dialog should be answered");
    match &msgs[0] {
        OutboundMsg::Rpc { id, payload } => {
            assert_eq!(*id, 62, "RPC_DialogResponse");
            assert_eq!(&payload[0..2], &[7, 0], "echoes dialog id");
            assert_eq!(payload[2], 1, "confirm button");
            assert!(payload.ends_with(b"CHANGE_ME"), "sends the password");
        }
        other => panic!("expected an RPC, got {other:?}"),
    }

    // A different dialog is left alone.
    engine
        .load_script(
            "require('samp.events').onShowDialog(8, 0, 'Info', 'Ok', '', '')",
            "other",
        )
        .expect("other dialog");
    assert!(
        outbox.borrow().is_empty(),
        "non-login dialogs are not answered"
    );
}

#[test]
fn samp_events_rewrites_client_join() {
    let engine = ScriptEngine::new().expect("vm");
    engine.install_sender(empty_outbox()).expect("sender");
    // The new_launcher pattern: samp.events onSendClientJoin returns the Arizona variant.
    engine
        .load_script(
            "local sampev = require('samp.events')\n\
                 function sampev.onSendClientJoin(version, mod, nickname, cr, authKey, ver, cr2)\n\
                   return {version, 1, nickname, cr, 'AUTH', 'Arizona PC', cr2}\n\
                 end",
            "launcher",
        )
        .expect("load");
    // A vanilla-ish 7-field ClientJoin (RPC 25): version=4057, mod=0, nick="Bot",
    // cr=0x11223344, authKey="", ver="x", cr2=0x11223344.
    let join = vec![
        0xD9, 0x0F, 0x00, 0x00, // version int32
        0x00, // mod u8
        0x03, b'B', b'o', b't', // nick str8
        0x44, 0x33, 0x22, 0x11, // challengeResponse int32
        0x00, // joinAuthKey str8 ""
        0x01, b'x', // clientVer str8 "x"
        0x44, 0x33, 0x22, 0x11, // challengeResponse2 int32
    ];
    match engine.dispatch_chokepoint("onSendRPC", 25, &join) {
        Verdict::Rewrite(bytes) => {
            assert_eq!(bytes[4], 1, "mod byte set to 1");
            let contains = |needle: &[u8]| bytes.windows(needle.len()).any(|w| w == needle);
            assert!(contains(b"AUTH"), "auth key written");
            assert!(contains(b"Arizona PC"), "client version written");
        }
        other => panic!("expected rewrite, got {other:?}"),
    }
}

#[test]
fn bot_bindings_read_write_state() {
    let engine = ScriptEngine::new().expect("vm");
    let state = samp_client::LocalPlayer::shared(
        "Tester".to_string(),
        "127.0.0.1:7777".parse().expect("addr"),
    );
    engine.install_bindings(state.clone()).expect("bindings");
    assert_eq!(
        engine.eval::<String>("return getBotNick()").unwrap(),
        "Tester"
    );
    assert_eq!(
        engine.eval::<String>("return getServerAddress()").unwrap(),
        "127.0.0.1:7777"
    );
    engine
        .load_script("setBotPosition(1.5, 2.5, 3.5)", "p")
        .expect("load");
    assert_eq!(state.borrow().on_foot.position.x, 1.5);
    assert_eq!(state.borrow().on_foot.position.z, 3.5);
    engine.load_script("updateSync()", "u").expect("load");
    assert!(state.borrow().force_sync);
}

#[test]
fn register_handler_drop_rewrite_pass() {
    let engine = ScriptEngine::new().expect("vm");
    engine.install_sender(empty_outbox()).expect("sender");
    engine
        .load_script(
            "registerHandler('onReceiveRPC', function(id, bs)\n\
                   if id == 99 then return false end\n\
                   if id == 101 then bs:setWriteOffset(0) bs:writeUInt8(0xAA) end\n\
                 end)",
            "h",
        )
        .expect("load");
    assert_eq!(
        engine.dispatch_chokepoint("onReceiveRPC", 99, &[1, 2, 3]),
        Verdict::Drop
    );
    match engine.dispatch_chokepoint("onReceiveRPC", 101, &[0x11, 0x22]) {
        Verdict::Rewrite(bytes) => assert_eq!(bytes[0], 0xAA),
        other => panic!("expected rewrite, got {other:?}"),
    }
    assert_eq!(
        engine.dispatch_chokepoint("onReceiveRPC", 5, &[1, 2]),
        Verdict::Pass
    );
    // No handler registered for this chokepoint.
    assert_eq!(
        engine.dispatch_chokepoint("onSendRPC", 1, &[1]),
        Verdict::Pass
    );
}

#[test]
fn bitstream_encoded_string_roundtrip() {
    let engine = ScriptEngine::new().expect("vm");
    engine.install_sender(empty_outbox()).expect("sender");
    let text: String = engine
        .eval(
            "local bs = bitStream.new()\n\
                 bs:writeUInt8(7)\n\
                 bs:writeEncoded('Login to your account')\n\
                 bs:setReadOffset(0)\n\
                 bs:readUInt8()\n\
                 return bs:readEncoded(256)",
        )
        .expect("eval");
    assert_eq!(text, "Login to your account");
}

#[test]
fn bitstream_reads_back_what_it_wrote() {
    let engine = ScriptEngine::new().expect("vm");
    engine.install_sender(empty_outbox()).expect("sender");
    let value: i64 = engine
        .eval(
            "local bs = bitStream.new()\n\
                 bs:writeInt32(1337) bs:setReadOffset(0)\n\
                 return bs:readInt32()",
        )
        .expect("eval");
    assert_eq!(value, 1337);
}

fn empty_outbox() -> Outbox {
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;
    Rc::new(RefCell::new(VecDeque::new()))
}

#[test]
fn script_queues_outgoing_chat() {
    let engine = ScriptEngine::new().expect("vm");
    engine
        .load_script("sampSendChat('hi')", "chat")
        .expect("load");
    assert_eq!(engine.drain_outgoing_chat(), vec![b"hi".to_vec()]);
    assert!(engine.drain_outgoing_chat().is_empty(), "queue drained");
}

mod type3 {
    use super::*;
    use hmac::{Hmac, Mac};
    use sha2::{Digest, Sha256};
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;

    const RES_100: &[u8] = include_bytes!("type3_res/res_100.bin");
    const RES_101: &[u8] = include_bytes!("type3_res/res_101.bin");
    const RES_57024: &[u8] = include_bytes!("type3_res/res_57024.bin");

    fn resource(flag: u8) -> &'static [u8] {
        match flag {
            0x64 => RES_100,
            0x65 => RES_101,
            0xC0 => RES_57024,
            other => panic!("unknown resource flag {other:#x}"),
        }
    }

    fn hx(s: &str) -> Vec<u8> {
        s.split_whitespace()
            .map(|b| u8::from_str_radix(b, 16).expect("hex"))
            .collect()
    }

    /// Oracle: reproduce the genuine client's RPC 187 body for a challenge with a given trailer, using
    /// the real `core.asi` resources. Mirrors the responder's HMAC composition exactly.
    fn expected_response(challenge: &[u8], trailer: &[u8]) -> Vec<u8> {
        let key = &challenge[1..17];
        let nonce = &challenge[17..33];
        let n = challenge[33] as usize;
        let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("key");
        mac.update(nonce);
        let mut p = 34;
        for _ in 0..n {
            let flag = challenge[p + 1];
            let offset = u32::from_le_bytes(challenge[p + 2..p + 6].try_into().unwrap()) as usize;
            let size = u16::from_le_bytes(challenge[p + 6..p + 8].try_into().unwrap()) as usize;
            p += 8;
            mac.update(&Sha256::digest(&resource(flag)[offset..offset + size]));
        }
        mac.update(trailer);
        let mut resp = vec![3];
        resp.extend_from_slice(nonce);
        resp.extend_from_slice(&mac.finalize().into_bytes());
        resp.extend_from_slice(&(trailer.len() as u16).to_le_bytes());
        resp.extend_from_slice(trailer);
        resp
    }

    /// The real captured Bumble Bee challenge (RPC 186 body) and its genuine response (RPC 187 body).
    /// trailerFlags `0x44` selects a 6-byte trailer; the real client's was `01 00 00 00 5C 01`.
    fn real_capture() -> (Vec<u8>, Vec<u8>) {
        let challenge = hx("03 32 A6 EF 02 A1 F6 D7 FD D2 B7 58 16 53 7E 74 98 \
             C6 0D AE 70 0A 5B 82 24 42 51 D8 73 1B 3A FA 20 \
             03 04 64 00 00 00 00 20 00 04 C0 00 00 00 00 40 00 04 65 00 00 00 00 00 01 44");
        let response = hx(
            "03 C6 0D AE 70 0A 5B 82 24 42 51 D8 73 1B 3A FA 20 \
             F8 DC 70 F0 8F 17 26 4A 3F 4D 89 19 7D 67 CB 1A 5F 34 96 94 CF 08 1A AA 8D 60 69 4C E7 22 44 69 \
             06 00 01 00 00 00 5C 01",
        );
        (challenge, response)
    }

    /// Anchors the oracle to the genuine capture: fed the real trailer, it reproduces the captured
    /// RPC 187 byte-for-byte. (The host `sha256`/`hmacSha256` are proven equivalent to this oracle by
    /// `handler_answers_real_challenge`.)
    #[test]
    fn oracle_matches_real_capture() {
        let (challenge, response) = real_capture();
        let real_trailer = hx("01 00 00 00 5C 01");
        assert_eq!(expected_response(&challenge, &real_trailer), response);
    }

    /// The Luau handler (driven through the host crypto bindings) answers the real RPC 186 with an
    /// RPC 187 carrying the clean-trailer HMAC the oracle computes — proving the port reproduces the
    /// genuine attestation math.
    #[test]
    fn handler_answers_real_challenge() {
        let engine = ScriptEngine::new().expect("vm");
        let outbox: Outbox = Rc::new(RefCell::new(VecDeque::new()));
        engine.install_sender(outbox.clone()).expect("sender");
        engine
            .load_script("require('arizona')", "load arizona")
            .expect("load");

        let (challenge, _) = real_capture();
        assert_eq!(
            engine.dispatch_chokepoint("onReceiveRPC", 186, &challenge),
            Verdict::Pass,
            "reading the challenge must not rewrite or drop it"
        );

        // flags 0x44 → bits 0x04 (4 B) + 0x40 (2 B) → a 6-byte all-zero clean trailer.
        let expected = expected_response(&challenge, &[0u8; 6]);
        let msgs: Vec<_> = outbox.borrow_mut().drain(..).collect();
        assert_eq!(msgs.len(), 1, "exactly one RPC 187 reply");
        match &msgs[0] {
            OutboundMsg::Rpc { id, payload } => {
                assert_eq!(*id, 187, "RPC_TYPE3_RESPONSE");
                assert_eq!(payload, &expected);
            }
            other => panic!("expected an RPC, got {other:?}"),
        }
    }

    /// A live-memory field (type 0, not a static resource) can't be answered statically, so the
    /// handler declines: nothing is queued.
    #[test]
    fn handler_declines_live_memory_challenge() {
        let engine = ScriptEngine::new().expect("vm");
        let outbox: Outbox = Rc::new(RefCell::new(VecDeque::new()));
        engine.install_sender(outbox.clone()).expect("sender");
        engine
            .load_script("require('arizona')", "load arizona")
            .expect("load");

        let challenge = hx("03 \
             00 11 22 33 44 55 66 77 88 99 AA BB CC DD EE FF \
             00 11 22 33 44 55 66 77 88 99 AA BB CC DD EE FF \
             01 00 64 00 00 00 00 20 00 44");
        assert_eq!(
            engine.dispatch_chokepoint("onReceiveRPC", 186, &challenge),
            Verdict::Pass
        );
        assert!(outbox.borrow().is_empty(), "declined: no RPC 187 queued");
    }
}
