use super::*;

#[test]
fn incoming_registry_decodes_by_id() {
    // onPlayerJoin (137): player_id u16=7, color i32=-1, is_npc bool8=false, nickname str8="Bo".
    let payload = vec![0x07, 0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0x00, 0x02, b'B', b'o'];
    let (event, values) = decode_incoming(137, &payload)
        .expect("known id")
        .expect("decodes");
    assert_eq!(event, "onPlayerJoin");
    assert_eq!(
        values,
        vec![
            FieldValue::U16(7),
            FieldValue::I32(-1),
            FieldValue::Bool(false),
            FieldValue::Bytes(b"Bo".to_vec()),
        ]
    );

    // Round-trips back to the same bytes through the registry encoder.
    let reencoded = encode_incoming(137, &values)
        .expect("known id")
        .expect("encodes");
    assert_eq!(reencoded, payload);
}

#[test]
fn unknown_id_is_none() {
    assert!(decode_incoming(254, &[]).is_none());
    assert!(encode_incoming(254, &[]).is_none());
}

#[test]
fn incoming_and_outgoing_share_no_table() {
    // id 26 is onPlayerEnterVehicle (incoming) and onSendEnterVehicle (outgoing) — different
    // events behind the same id, kept in separate registries.
    assert_eq!(
        decode_incoming(26, &[0; 8]).unwrap().unwrap().0,
        "onPlayerEnterVehicle"
    );
    assert_eq!(
        decode_outgoing(26, &[0; 8]).unwrap().unwrap().0,
        "onSendEnterVehicle"
    );
}

#[test]
fn rewrite_via_values_changes_payload() {
    let payload = vec![0x07, 0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0x00, 0x02, b'B', b'o'];
    let (_, mut values) = decode_incoming(137, &payload).unwrap().unwrap();
    values[0] = FieldValue::U16(42); // rewrite player_id
    let out = encode_incoming(137, &values).unwrap().unwrap();
    assert_eq!(&out[0..2], &[42, 0]);
}
