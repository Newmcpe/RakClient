# samp-proto/rwbitstream.rs

## Module — seekable read+write bitstream
<anchor: module>

Seekable read+write bitstream backing the Lua `bitStream` userdata.

Unlike the append-only `crate::BitStreamWriter` / forward-only `crate::BitStreamReader`
(kept untouched — 150+ codecs depend on them), this type owns one buffer with independent read
and write cursors and supports `setReadOffset`/`setWriteOffset`, so a script can read an incoming
packet and overwrite fields in place. Bit packing is MSB-first per byte, matching RakNet (and the
other two types); it is implemented bit-by-bit for obviously-correct overwrite semantics — packet
bodies are tiny, so the cost is irrelevant.

---
