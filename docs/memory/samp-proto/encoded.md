# samp-proto/encoded.rs

## Module — Huffman codec provenance & verification
<anchor: module>

SA-MP "encoded string" Huffman codec (`readEncoded`/`writeEncoded`).

Used for the body text of `ShowDialog` and `Create3DTextLabel`. This is a port of RakNet's
`StringCompressor` + `HuffmanEncodingTree` as shipped in SA-MP:
- the wire layout is `WriteCompressed(u16 bitLength)` followed by the Huffman-coded bits
  (matching `StringCompressor::EncodeString`); decode reads the compressed length then walks the
  tree for exactly that many bits (`StringCompressor::DecodeString` / `DecodeArray`).
- the tree is built from the fixed 256-entry `FREQUENCY_TABLE` using RakNet's
  `GenerateFromFrequencyTable` algorithm (zero weights are bumped to 1; the two lowest-weight
  nodes are repeatedly merged; ties insert the newer node *before* equal-weight nodes).

Verification status: `FREQUENCY_TABLE` is byte-identical to RakNet's canonical
`englishCharacterFrequencies` (`Source/StringCompressor.cpp`) and the tree construction mirrors
`Source/DS_HuffmanEncodingTree.cpp` exactly — insert-before-equal ties, `lesser`=left/`greater`=right
on merge, left=0/right=1 root-to-leaf codes. This matters: the `\r` (idx 13) weight is **2**, not
the 0→1 bump, and `\n` (idx 10) is **722**, not 723 — getting either wrong permutes the whole
weight-1 leaf cluster (all 0x80-0xFF) and yields correlated mojibake (a uniform +2 byte shift on
Cyrillic) even though `encode`→`decode` still round-trips self-consistently.

---
