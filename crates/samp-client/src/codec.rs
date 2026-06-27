//! Protocol-codec seam between the FSM and [`samp_proto`].
//!
//! Wrapping the `samp_proto` free functions behind a trait lets the FSM run against a fake codec in
//! unit tests (where `samp_proto` is still a stub) while production forwards straight through.

use samp_proto::{
    decode_client_message, decode_init_game, decode_player_chat, decode_request_class_response,
    decode_request_spawn_response, encode_chat, encode_client_join, encode_on_foot_sync,
    encode_request_class, encode_request_spawn, encode_spawn, generate_gpci, BitStreamReader,
    ChatMessage, ClassId, ClientJoin, InitGame, OnFootSync, PlayerId, RequestClassResponse,
    RequestSpawnResponse, Result as ProtoResult, ServerCookie, ServerMessage,
};

pub(crate) trait Codec {
    /// Read the assigned player id + server cookie from a `CONNECTION_REQUEST_ACCEPTED` body.
    fn parse_connect(&self, body: &[u8]) -> ProtoResult<(PlayerId, ServerCookie)>;
    fn encode_client_join(&self, join: &ClientJoin<'_>) -> Vec<u8>;
    fn decode_init_game(&self, payload: &[u8]) -> ProtoResult<InitGame>;
    fn encode_request_class(&self, class: ClassId) -> Vec<u8>;
    fn decode_request_class_response(&self, payload: &[u8]) -> ProtoResult<RequestClassResponse>;
    fn encode_request_spawn(&self) -> Vec<u8>;
    fn decode_request_spawn_response(&self, payload: &[u8]) -> ProtoResult<RequestSpawnResponse>;
    fn encode_spawn(&self) -> Vec<u8>;
    fn encode_on_foot_sync(&self, sync: &OnFootSync) -> Vec<u8>;
    fn decode_client_message(&self, payload: &[u8]) -> ProtoResult<ServerMessage>;
    fn decode_player_chat(&self, payload: &[u8]) -> ProtoResult<ChatMessage>;
    fn encode_chat(&self, text: &[u8]) -> Vec<u8>;
    fn generate_gpci(&self) -> String;
}

pub(crate) struct SampProtoCodec;

impl Codec for SampProtoCodec {
    fn parse_connect(&self, body: &[u8]) -> ProtoResult<(PlayerId, ServerCookie)> {
        // Verified against samp.dll sub_1000AA20: the CONNECTION_REQUEST_ACCEPTED body (after the
        // RakNet id byte) is [u32 external IP][u16 port][u16 systemIndex][u32 cookie]. The
        // systemIndex is the assigned local player id; the cookie XORed with CHALLENGE_XOR (0xFD9)
        // becomes the ClientJoin challenge response.
        let mut reader = BitStreamReader::new(body);
        let _ip = reader.read_u32()?;
        let _port = reader.read_u16()?;
        let player_id = PlayerId(reader.read_u16()?);
        let cookie = ServerCookie(reader.read_u32()?);
        Ok((player_id, cookie))
    }

    fn encode_client_join(&self, join: &ClientJoin<'_>) -> Vec<u8> {
        encode_client_join(join)
    }

    fn decode_init_game(&self, payload: &[u8]) -> ProtoResult<InitGame> {
        decode_init_game(payload)
    }

    fn encode_request_class(&self, class: ClassId) -> Vec<u8> {
        encode_request_class(class)
    }

    fn decode_request_class_response(&self, payload: &[u8]) -> ProtoResult<RequestClassResponse> {
        decode_request_class_response(payload)
    }

    fn encode_request_spawn(&self) -> Vec<u8> {
        encode_request_spawn()
    }

    fn decode_request_spawn_response(&self, payload: &[u8]) -> ProtoResult<RequestSpawnResponse> {
        decode_request_spawn_response(payload)
    }

    fn encode_spawn(&self) -> Vec<u8> {
        encode_spawn()
    }

    fn encode_on_foot_sync(&self, sync: &OnFootSync) -> Vec<u8> {
        encode_on_foot_sync(sync)
    }

    fn decode_client_message(&self, payload: &[u8]) -> ProtoResult<ServerMessage> {
        decode_client_message(payload)
    }

    fn decode_player_chat(&self, payload: &[u8]) -> ProtoResult<ChatMessage> {
        decode_player_chat(payload)
    }

    fn encode_chat(&self, text: &[u8]) -> Vec<u8> {
        encode_chat(text)
    }

    fn generate_gpci(&self) -> String {
        generate_gpci()
    }
}
