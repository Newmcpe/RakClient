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

    // A 7-field ClientJoin (RPC 25). onSendClientJoin should rewrite it to the Arizona variant
    // and queue the two CEF packets via sendCef → bitStream:sendPacket.
    let join = vec![
        0xD9, 0x0F, 0x00, 0x00, 0x00, 0x03, b'B', b'o', b't', 0x44, 0x33, 0x22, 0x11, 0x00, 0x01,
        b'x', 0x44, 0x33, 0x22, 0x11,
    ];
    match engine.dispatch_chokepoint("onSendRPC", 25, &join) {
        Verdict::Rewrite(bytes) => {
            assert_eq!(bytes[4], 1, "modded");
            let has = |n: &[u8]| bytes.windows(n.len()).any(|w| w == n);
            assert!(has(b"Arizona PC"), "client version");
            assert!(
                has(b"263083C359F5AE44AD3AFC8551F4208E6C84A36F6CC"),
                "auth key"
            );
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
