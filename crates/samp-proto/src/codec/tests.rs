use super::*;
use crate::SAMP_VERSION_0_3_7;

#[test]
fn client_join_golden_vector() {
    let join = ClientJoin {
        version: SAMP_VERSION_0_3_7,
        modded: true,
        nick: "Bot",
        challenge_response: 0x1234_5678,
        auth: "AUTH",
        client_version: "0.3.7",
        duplicate_challenge_response: false,
    };
    let bytes = join.encode();
    let expected = vec![
        0xD9, 0x0F, 0x00, 0x00, // version 4057, little-endian
        0x01, // modded = 1
        0x03, b'B', b'o', b't', // nick str8
        0x78, 0x56, 0x34, 0x12, // challenge_response, little-endian
        0x04, b'A', b'U', b'T', b'H', // auth str8
        0x05, b'0', b'.', b'3', b'.', b'7', // client_version str8
    ];
    assert_eq!(bytes, expected);
}

#[test]
fn client_join_unmodded_byte() {
    let join = ClientJoin {
        version: SAMP_VERSION_0_3_7,
        modded: false,
        nick: "A",
        challenge_response: 0,
        auth: "",
        client_version: "",
        duplicate_challenge_response: false,
    };
    let bytes = join.encode();
    assert_eq!(bytes[4], 0x00); // modded byte
}

#[test]
fn client_join_arizona_appends_trailing_challenge() {
    let base = ClientJoin {
        version: SAMP_VERSION_0_3_7,
        modded: false,
        nick: "Bot",
        challenge_response: 0x1234_5678,
        auth: "",
        client_version: "0.3.7-R3",
        duplicate_challenge_response: false,
    };
    let vanilla = base.encode();
    let arizona = ClientJoin {
        duplicate_challenge_response: true,
        ..base.clone()
    }
    .encode();
    // Arizona form == vanilla form + a trailing little-endian copy of challenge_response.
    assert_eq!(&arizona[..vanilla.len()], vanilla.as_slice());
    assert_eq!(&arizona[vanilla.len()..], &[0x78, 0x56, 0x34, 0x12]);
}

#[test]
fn init_game_roundtrip() {
    let mut w = BitStreamWriter::new();
    w.write_zero_bits(INIT_GAME_BITS_BEFORE_PLAYER_ID);
    w.write_u16(0x1234);
    w.write_zero_bits(INIT_GAME_BITS_BETWEEN_PLAYER_ID_AND_HOSTNAME);
    w.write_str8("Los Santos Roleplay");
    let payload = w.into_bytes();

    let game = InitGame::decode(&payload).unwrap();
    assert_eq!(game.local_player_id, PlayerId(0x1234));
    assert_eq!(game.host_name, "Los Santos Roleplay");
}

#[test]
fn init_game_truncated_errs() {
    assert!(InitGame::decode(&[]).is_err());
    assert!(InitGame::decode(&[0u8; 13]).is_err()); // enough to skip but not read id
}

#[test]
fn request_class_roundtrip() {
    let bytes = RequestClass { class: ClassId(7) }.encode();
    assert_eq!(bytes, vec![0x07, 0x00, 0x00, 0x00]);

    let bytes = RequestClass { class: ClassId(-1) }.encode();
    assert_eq!(bytes, vec![0xFF, 0xFF, 0xFF, 0xFF]);
}

#[test]
fn request_class_response_allowed() {
    let mut info = vec![0u8; SPAWN_INFO_LEN];
    info[SPAWN_INFO_SKIN_OFFSET] = 0x2A; // skin low byte = 42
    info[SPAWN_INFO_POS_OFFSET..SPAWN_INFO_POS_OFFSET + 4].copy_from_slice(&1.5f32.to_le_bytes());
    info[SPAWN_INFO_POS_OFFSET + 4..SPAWN_INFO_POS_OFFSET + 8]
        .copy_from_slice(&(-2.0f32).to_le_bytes());
    info[SPAWN_INFO_POS_OFFSET + 8..SPAWN_INFO_POS_OFFSET + 12]
        .copy_from_slice(&3.25f32.to_le_bytes());

    let mut payload = vec![0x01u8]; // allow
    payload.extend_from_slice(&info);

    let resp = RequestClassResponse::decode(&payload).unwrap();
    assert!(resp.allowed);
    assert_eq!(resp.skin, Skin(42));
    assert_eq!(
        resp.spawn_position,
        Vector3 {
            x: 1.5,
            y: -2.0,
            z: 3.25
        }
    );
}

#[test]
fn request_class_response_denied_has_no_spawn_info() {
    let resp = RequestClassResponse::decode(&[0x00]).unwrap();
    assert!(!resp.allowed);
}

#[test]
fn request_class_response_truncated_errs() {
    assert!(RequestClassResponse::decode(&[]).is_err());
    assert!(RequestClassResponse::decode(&[1, 2, 3]).is_err());
}

#[test]
fn spawn_response_roundtrip() {
    assert_eq!(RequestSpawnResponse::decode(&[2]).unwrap().allow, 2);
    assert!(RequestSpawnResponse::decode(&[]).is_err());
}

#[test]
fn empty_bodies() {
    assert!(RequestSpawn.encode().is_empty());
    assert!(Spawn.encode().is_empty());
}

#[test]
fn on_foot_sync_layout() {
    let sync = OnFootSync {
        keys: 0xBEEF,
        position: Vector3 {
            x: 1.0,
            y: 2.0,
            z: 3.0,
        },
        quaternion: Quaternion {
            x: 0.1,
            y: 0.2,
            z: 0.3,
            w: 0.4,
        },
        health: 100,
        armour: 50,
        weapon: WeaponId(24),
        special_action: 5,
    };
    let body = sync.encode();
    assert_eq!(body.len(), ON_FOOT_SYNC_LEN);

    let mut r = BitStreamReader::new(&body);
    assert_eq!(r.read_u16().unwrap(), 0); // lrAnalog
    assert_eq!(r.read_u16().unwrap(), 0); // udAnalog
    assert_eq!(r.read_u16().unwrap(), 0xBEEF); // keys
    assert_eq!(r.read_f32().unwrap(), 1.0);
    assert_eq!(r.read_f32().unwrap(), 2.0);
    assert_eq!(r.read_f32().unwrap(), 3.0);
    assert_eq!(r.read_f32().unwrap(), 0.1);
    assert_eq!(r.read_f32().unwrap(), 0.2);
    assert_eq!(r.read_f32().unwrap(), 0.3);
    assert_eq!(r.read_f32().unwrap(), 0.4);
    assert_eq!(r.read_u8().unwrap(), 100); // health
    assert_eq!(r.read_u8().unwrap(), 50); // armour
    assert_eq!(r.read_u8().unwrap(), 24); // weapon
    assert_eq!(r.read_u8().unwrap(), 5); // special action
}

#[test]
fn gpci_is_deterministic_and_valid() {
    let a = generate_gpci_seeded(0xDEAD_BEEF);
    let b = generate_gpci_seeded(0xDEAD_BEEF);
    assert_eq!(a, b);
    assert!(!a.is_empty() && a.len() <= 48);
    assert!(a
        .bytes()
        .all(|c| c.is_ascii_digit() || (b'A'..=b'F').contains(&c)));

    // Different seeds should generally differ.
    assert_ne!(generate_gpci_seeded(1), generate_gpci_seeded(2));
}

#[test]
fn generate_gpci_runs() {
    let token = generate_gpci();
    assert!(!token.is_empty() && token.len() <= 48);
}

#[test]
fn client_message_decodes_color_and_text() {
    // White, length 1, " " — the exact shape captured live from Arizona Bumble Bee.
    let msg = ServerMessage::decode(&[0xff, 0xff, 0xff, 0xff, 0x01, 0, 0, 0, 0x20]).unwrap();
    assert_eq!(msg.color, 0xffff_ffff);
    assert_eq!(msg.text, b" ");
}

#[test]
fn client_message_truncated_errs() {
    assert!(ServerMessage::decode(&[0xff, 0xff]).is_err());
    // colour + length present but text shorter than the declared length.
    assert!(ServerMessage::decode(&[0, 0, 0, 0, 0x05, 0, 0, 0, b'h', b'i']).is_err());
}

#[test]
fn player_chat_decodes_id_and_text() {
    let msg = ChatMessage::decode(&[0x2a, 0x00, 0x03, b'y', b'o', b'!']).unwrap();
    assert_eq!(msg.player_id.0, 42);
    assert_eq!(msg.text, b"yo!");
}

#[test]
fn player_chat_truncated_errs() {
    assert!(ChatMessage::decode(&[0x05]).is_err());
    assert!(ChatMessage::decode(&[0x05, 0x00, 0x04, b'h']).is_err());
}

#[test]
fn chat_encodes_length_prefixed() {
    assert_eq!(
        ChatOutgoing { text: b"hello" }.encode(),
        vec![5, b'h', b'e', b'l', b'l', b'o']
    );
    assert_eq!(ChatOutgoing { text: b"" }.encode(), vec![0]);
}

#[test]
fn chat_encode_truncates_over_255_bytes() {
    let long = vec![b'a'; 300];
    let encoded = ChatOutgoing { text: &long }.encode();
    assert_eq!(encoded[0], 255);
    assert_eq!(encoded.len(), 256);
}

#[test]
fn show_dialog_decodes_structural_head() {
    // [u16 id=2][u8 style=3][str8 "Авторизация"-stand-in "Login"][str8 "OK"][str8 "Cancel"][body]
    let mut p = vec![0x02, 0x00, 0x03];
    p.push(5);
    p.extend_from_slice(b"Login");
    p.push(2);
    p.extend_from_slice(b"OK");
    p.push(6);
    p.extend_from_slice(b"Cancel");
    p.extend_from_slice(b"...body...");
    let d = ShowDialog::decode(&p).unwrap();
    assert_eq!(d.dialog_id, 2);
    assert_eq!(d.style, 3);
    assert_eq!(d.title, b"Login");
    assert_eq!(d.button1, b"OK");
    assert_eq!(d.button2, b"Cancel");
}

#[test]
fn show_dialog_truncated_errs() {
    assert!(ShowDialog::decode(&[0x02, 0x00]).is_err());
    assert!(ShowDialog::decode(&[0x02, 0x00, 0x03, 0x05, b'h', b'i']).is_err());
}

#[test]
fn dialog_response_layout() {
    // dialogId=2, button=1, listItem=0xFFFF, input="secret"
    let bytes = DialogResponse {
        dialog_id: 2,
        button: 1,
        list_item: 0xFFFF,
        input: b"secret",
    }
    .encode();
    assert_eq!(
        bytes,
        vec![0x02, 0x00, 0x01, 0xFF, 0xFF, 0x06, b's', b'e', b'c', b'r', b'e', b't']
    );
}

#[test]
fn cp1251_roundtrips_cyrillic_and_ascii() {
    // "Привет" in cp1251 bytes.
    let bytes = [0xCF, 0xF0, 0xE8, 0xE2, 0xE5, 0xF2];
    assert_eq!(crate::decode_cp1251(&bytes), "Привет");
    assert_eq!(crate::decode_cp1251(b"hi 123"), "hi 123");
}
