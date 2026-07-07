# samp-proto/bitstream.rs

## Module — bit order
<anchor: bit-order>

RakNet-compatible bit stream, ported from `BitStream_WriteBits` (0x402180) and
`BitStream_ReadBits` (0x4022B0).

Bit order: each source byte is packed most-significant-bit first into the stream. Multi-byte
integers are first laid out little-endian, then bit-packed, so a fully byte-aligned stream is
identical to a plain little-endian buffer.

---

## BitStreamWriter::write_compressed
<anchor: write-compressed>

RakNet `BitStream::WriteCompressed` for an unsigned little-endian value: high zero bytes are
each encoded as a single `1` bit; the first non-zero byte is preceded by a `0` bit followed by
every byte from that point down to the lowest; the lowest byte is encoded as `1`+low-nibble
when its high nibble is zero, otherwise `0`+full-byte.

---
