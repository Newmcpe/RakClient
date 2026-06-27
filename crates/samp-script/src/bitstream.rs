//! `bitStream` userdata — the RakSAMP native bitstream exposed to Luau.
//!
//! Wraps a [`RwBitStream`] in shared interior mutability so the engine can read a handler's edits
//! back after it runs. A `dirty` flag (set by any write / `setWriteOffset`) drives rewrite detection
//! at the `registerHandler` chokepoints. Senders push to the shared [`Outbox`] the driver flushes.

use std::cell::RefCell;
use std::rc::Rc;

use mlua::{Lua, UserData, UserDataMethods};
use samp_proto::{OutboundMsg, Outbox, RwBitStream, RELIABILITY_RELIABLE_ORDERED};

/// A `bitStream` instance shared between Luau and the host.
#[derive(Clone)]
pub struct BitStreamUserData {
    bs: Rc<RefCell<RwBitStream>>,
    outbox: Outbox,
    dirty: Rc<RefCell<bool>>,
}

impl BitStreamUserData {
    pub fn new(bs: RwBitStream, outbox: Outbox) -> Self {
        Self {
            bs: Rc::new(RefCell::new(bs)),
            outbox,
            dirty: Rc::new(RefCell::new(false)),
        }
    }

    /// Whether a handler wrote to / seeked the write cursor of this stream (rewrite detection).
    /// Used by the `registerHandler` chokepoints.
    pub fn is_dirty(&self) -> bool {
        *self.dirty.borrow()
    }

    /// The current bytes of the stream.
    pub fn bytes(&self) -> Vec<u8> {
        self.bs.borrow().as_bytes().to_vec()
    }

    fn mark_dirty(&self) {
        *self.dirty.borrow_mut() = true;
    }
}

/// Map a RakSAMP/sampfuncs reliability constant (`6..=10`) to the RakNet wire value (`0..=4`);
/// anything else defaults to reliable-ordered.
fn reliability_wire(value: i64) -> u8 {
    match value {
        6..=10 => (value - 6) as u8,
        _ => RELIABILITY_RELIABLE_ORDERED,
    }
}

impl UserData for BitStreamUserData {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        // --- writes (mark dirty) ---
        methods.add_method("writeBool", |_, this, v: bool| {
            this.bs.borrow_mut().write_bool(v);
            this.mark_dirty();
            Ok(())
        });
        methods.add_method("writeInt8", |_, this, v: i32| {
            this.bs.borrow_mut().write_i8(v as i8);
            this.mark_dirty();
            Ok(())
        });
        methods.add_method("writeUInt8", |_, this, v: u32| {
            this.bs.borrow_mut().write_u8(v as u8);
            this.mark_dirty();
            Ok(())
        });
        methods.add_method("writeInt16", |_, this, v: i32| {
            this.bs.borrow_mut().write_i16(v as i16);
            this.mark_dirty();
            Ok(())
        });
        methods.add_method("writeUInt16", |_, this, v: u32| {
            this.bs.borrow_mut().write_u16(v as u16);
            this.mark_dirty();
            Ok(())
        });
        methods.add_method("writeInt32", |_, this, v: i32| {
            this.bs.borrow_mut().write_i32(v);
            this.mark_dirty();
            Ok(())
        });
        methods.add_method("writeUInt32", |_, this, v: u32| {
            this.bs.borrow_mut().write_u32(v);
            this.mark_dirty();
            Ok(())
        });
        methods.add_method("writeFloat", |_, this, v: f32| {
            this.bs.borrow_mut().write_f32(v);
            this.mark_dirty();
            Ok(())
        });
        methods.add_method("writeString", |_, this, v: mlua::String| {
            this.bs.borrow_mut().write_bytes(&v.as_bytes());
            this.mark_dirty();
            Ok(())
        });

        // --- reads (RakNet-lenient: exhaustion yields 0 / empty) ---
        methods.add_method("readBool", |_, this, ()| {
            Ok(this.bs.borrow_mut().read_bool().unwrap_or(false))
        });
        methods.add_method("readInt8", |_, this, ()| {
            Ok(this.bs.borrow_mut().read_i8().unwrap_or(0) as i32)
        });
        methods.add_method("readUInt8", |_, this, ()| {
            Ok(this.bs.borrow_mut().read_u8().unwrap_or(0) as u32)
        });
        methods.add_method("readInt16", |_, this, ()| {
            Ok(this.bs.borrow_mut().read_i16().unwrap_or(0) as i32)
        });
        methods.add_method("readUInt16", |_, this, ()| {
            Ok(this.bs.borrow_mut().read_u16().unwrap_or(0) as u32)
        });
        methods.add_method("readInt32", |_, this, ()| {
            Ok(this.bs.borrow_mut().read_i32().unwrap_or(0))
        });
        methods.add_method("readUInt32", |_, this, ()| {
            Ok(this.bs.borrow_mut().read_u32().unwrap_or(0))
        });
        methods.add_method("readFloat", |_, this, ()| {
            Ok(this.bs.borrow_mut().read_f32().unwrap_or(0.0))
        });
        methods.add_method("readString", |lua, this, len: usize| {
            let bytes = this.bs.borrow_mut().read_bytes(len).unwrap_or_default();
            lua.create_string(&bytes)
        });
        methods.add_method("writeEncoded", |_, this, s: mlua::String| {
            this.bs.borrow_mut().write_encoded(&s.as_bytes());
            this.mark_dirty();
            Ok(())
        });
        methods.add_method("readEncoded", |lua, this, max_len: usize| {
            let bytes = this.bs.borrow_mut().read_encoded(max_len);
            lua.create_string(&bytes)
        });

        // --- cursors / state ---
        methods.add_method("setReadOffset", |_, this, bits: usize| {
            this.bs.borrow_mut().set_read_offset(bits);
            Ok(())
        });
        methods.add_method("setWriteOffset", |_, this, bits: usize| {
            this.bs.borrow_mut().set_write_offset(bits);
            this.mark_dirty();
            Ok(())
        });
        methods.add_method("getNumberOfUnreadBits", |_, this, ()| {
            Ok(this.bs.borrow().num_unread_bits())
        });
        methods.add_method("getNumberOfUnreadBytes", |_, this, ()| {
            Ok(this.bs.borrow().num_unread_bytes())
        });
        methods.add_method("ignoreBits", |_, this, bits: usize| {
            this.bs.borrow_mut().ignore_bits(bits);
            Ok(())
        });
        methods.add_method("reset", |_, this, ()| {
            this.bs.borrow_mut().reset();
            *this.dirty.borrow_mut() = false;
            Ok(())
        });

        // --- senders ---
        methods.add_method("sendPacket", |_, this, ()| {
            this.outbox.borrow_mut().push_back(OutboundMsg::Packet {
                data: this.bytes(),
                reliability: RELIABILITY_RELIABLE_ORDERED,
                channel: 0,
            });
            Ok(())
        });
        methods.add_method(
            "sendPacketEx",
            |_, this, (_priority, reliability, channel): (i64, i64, u8)| {
                this.outbox.borrow_mut().push_back(OutboundMsg::Packet {
                    data: this.bytes(),
                    reliability: reliability_wire(reliability),
                    channel,
                });
                Ok(())
            },
        );
        methods.add_method("sendRPC", |_, this, id: u8| {
            this.outbox.borrow_mut().push_back(OutboundMsg::Rpc {
                id,
                payload: this.bytes(),
            });
            Ok(())
        });

        // Fall through to the global `bitStream` table for methods `addon.lua` adds in Lua
        // (`readString8`, `writeBool8`, `writeVector3`, …): `function bitStream:foo()` registers on
        // that table, and instances resolve missing methods through here.
        methods.add_meta_method(mlua::MetaMethod::Index, |lua, _this, key: mlua::String| {
            let table: mlua::Table = lua.globals().get("bitStream")?;
            table.get::<mlua::Value>(&key)
        });
    }
}

/// Install the `bitStream` global with `bitStream.new()` bound to `outbox`.
pub fn install_bitstream(lua: &Lua, outbox: Outbox) -> mlua::Result<()> {
    let table = lua.create_table()?;
    let new = lua.create_function(move |_, ()| {
        Ok(BitStreamUserData::new(RwBitStream::new(), outbox.clone()))
    })?;
    table.set("new", new)?;
    lua.globals().set("bitStream", table)
}
