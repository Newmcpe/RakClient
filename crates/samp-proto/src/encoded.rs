//! SA-MP "encoded string" Huffman codec (`readEncoded`/`writeEncoded`).
//!
//! Used for the body text of `ShowDialog` and `Create3DTextLabel`. This is a port of RakNet's
//! `StringCompressor` + `HuffmanEncodingTree` as shipped in SA-MP:
//! - the wire layout is `WriteCompressed(u16 bitLength)` followed by the Huffman-coded bits
//!   (matching `StringCompressor::EncodeString`); decode reads the compressed length then walks the
//!   tree for exactly that many bits (`StringCompressor::DecodeString` / `DecodeArray`).
//! - the tree is built from the fixed 256-entry [`FREQUENCY_TABLE`] using RakNet's
//!   `GenerateFromFrequencyTable` algorithm (zero weights are bumped to 1; the two lowest-weight
//!   nodes are repeatedly merged; ties insert the newer node *before* equal-weight nodes).
//!
//! Verification status: [`FREQUENCY_TABLE`] is byte-identical to RakNet's canonical
//! `englishCharacterFrequencies` (`Source/StringCompressor.cpp`) and the tree construction mirrors
//! `Source/DS_HuffmanEncodingTree.cpp` exactly — insert-before-equal ties, `lesser`=left/`greater`=right
//! on merge, left=0/right=1 root-to-leaf codes. This matters: the `\r` (idx 13) weight is **2**, not
//! the 0→1 bump, and `\n` (idx 10) is **722**, not 723 — getting either wrong permutes the whole
//! weight-1 leaf cluster (all 0x80-0xFF) and yields correlated mojibake (a uniform +2 byte shift on
//! Cyrillic) even though `encode`→`decode` still round-trips self-consistently.

use crate::bitstream::{BitStreamReader, BitStreamWriter};

/// RakNet/SA-MP `englishCharacterFrequencies` — the fixed 256-entry byte frequency table the
/// Huffman tree is generated from.
#[rustfmt::skip]
pub const FREQUENCY_TABLE: [u32; 256] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 722, 0, 0, 2, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    11084, 58, 63, 1, 0, 31, 0, 317, 64, 64, 44, 0, 695, 62, 980, 266,
    69, 67, 56, 7, 73, 3, 14, 2, 69, 1, 167, 9, 1, 2, 25, 94,
    0, 195, 139, 34, 96, 48, 103, 56, 125, 653, 21, 5, 23, 64, 85, 44,
    34, 7, 92, 76, 147, 12, 14, 57, 15, 39, 15, 1, 1, 1, 2, 3,
    0, 3611, 845, 1077, 1884, 5870, 841, 1057, 2501, 3212, 164, 531, 2019, 1330, 3056, 4037,
    848, 47, 2586, 2919, 4771, 1707, 535, 1106, 152, 1243, 100, 0, 2, 0, 10, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
];

enum Node {
    Leaf(u8),
    Internal { left: Box<Node>, right: Box<Node> },
}

struct WeightedNode {
    weight: u64,
    node: Node,
}

/// The decoded Huffman tree plus the per-byte encodings (root-to-leaf bit paths).
struct HuffmanTree {
    root: Node,
    codes: Vec<Vec<bool>>,
}

impl HuffmanTree {
    fn build() -> Self {
        // Mirror RakNet's sorted-list construction: zero weights become 1, then repeatedly merge the
        // two lowest-weight nodes. Insertion keeps ascending weight; equal weights insert before
        // existing equal-weight nodes (RakNet `InsertNodeIntoSortedList`).
        let mut list: Vec<WeightedNode> = Vec::with_capacity(256);
        for (byte, &freq) in FREQUENCY_TABLE.iter().enumerate() {
            let weight = if freq == 0 { 1 } else { freq as u64 };
            insert_sorted(
                &mut list,
                WeightedNode {
                    weight,
                    node: Node::Leaf(byte as u8),
                },
            );
        }
        while list.len() > 1 {
            let left = list.remove(0);
            let right = list.remove(0);
            let merged = WeightedNode {
                weight: left.weight + right.weight,
                node: Node::Internal {
                    left: Box::new(left.node),
                    right: Box::new(right.node),
                },
            };
            insert_sorted(&mut list, merged);
        }
        // `FREQUENCY_TABLE` has 256 entries, so the reduction always leaves exactly one node; the
        // empty arm is unreachable and yields a harmless degenerate tree rather than panicking.
        let root = match list.pop() {
            Some(node) => node.node,
            None => Node::Leaf(0),
        };

        let mut codes = vec![Vec::new(); 256];
        let mut path = Vec::new();
        assign_codes(&root, &mut path, &mut codes);
        HuffmanTree { root, codes }
    }
}

fn insert_sorted(list: &mut Vec<WeightedNode>, node: WeightedNode) {
    let pos = list
        .iter()
        .position(|existing| existing.weight >= node.weight)
        .unwrap_or(list.len());
    list.insert(pos, node);
}

fn assign_codes(node: &Node, path: &mut Vec<bool>, codes: &mut [Vec<bool>]) {
    match node {
        Node::Leaf(byte) => {
            // A degenerate single-symbol tree would leave an empty path; the SA-MP table never
            // produces one, but keep the encoding non-empty so decode terminates.
            codes[*byte as usize] = if path.is_empty() {
                vec![false]
            } else {
                path.clone()
            };
        }
        Node::Internal { left, right } => {
            path.push(false);
            assign_codes(left, path, codes);
            path.pop();
            path.push(true);
            assign_codes(right, path, codes);
            path.pop();
        }
    }
}

thread_local! {
    static TREE: HuffmanTree = HuffmanTree::build();
}

/// Encode `input` to SA-MP's encoded-string wire form: a compressed `u16` bit length followed by the
/// Huffman-coded bits. Matches `StringCompressor::EncodeString` / `bs:writeEncoded`.
pub fn encode_string(input: &[u8]) -> Vec<u8> {
    TREE.with(|tree| {
        let mut bits: Vec<bool> = Vec::new();
        for &byte in input {
            bits.extend_from_slice(&tree.codes[byte as usize]);
        }
        let mut out = BitStreamWriter::new();
        out.write_compressed_u16(bits.len() as u16);
        for bit in bits {
            out.write_bit(bit);
        }
        out.into_bytes()
    })
}

/// Decode a SA-MP encoded string: read the compressed `u16` bit length, then walk the Huffman tree
/// for that many bits, emitting at most `max_len` bytes. Matches `StringCompressor::DecodeString` /
/// `bs:readEncoded(max_len)`. A truncated/exhausted stream stops early rather than erroring.
pub fn decode_string(data: &[u8], max_len: usize) -> Vec<u8> {
    TREE.with(|tree| {
        let mut reader = BitStreamReader::new(data);
        let bit_len = match reader.read_compressed_u16() {
            Ok(len) => len as usize,
            Err(_) => return Vec::new(),
        };
        let mut out = Vec::new();
        let mut node = &tree.root;
        for _ in 0..bit_len {
            let bit = match reader.read_bit() {
                Ok(b) => b,
                Err(_) => break,
            };
            node = match node {
                Node::Internal { left, right } => {
                    if bit {
                        right
                    } else {
                        left
                    }
                }
                Node::Leaf(_) => &tree.root,
            };
            if let Node::Leaf(byte) = node {
                if out.len() < max_len {
                    out.push(*byte);
                }
                node = &tree.root;
            }
        }
        out
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(input: &[u8]) {
        let encoded = encode_string(input);
        let decoded = decode_string(&encoded, input.len() + 16);
        assert_eq!(decoded, input, "round-trip mismatch for {input:?}");
    }

    #[test]
    fn frequency_table_is_256_entries() {
        assert_eq!(FREQUENCY_TABLE.len(), 256);
    }

    #[test]
    fn roundtrip_ascii() {
        roundtrip(b"");
        roundtrip(b"a");
        roundtrip(b"Hello, world!");
        roundtrip(b"Login to your account.");
        roundtrip(b"The quick brown fox jumps over the lazy dog 0123456789");
    }

    #[test]
    fn roundtrip_all_byte_values() {
        let all: Vec<u8> = (0..=255u8).collect();
        roundtrip(&all);
    }

    #[test]
    fn roundtrip_high_bytes() {
        roundtrip(&[0x00, 0xFF, 0x80, 0x7F, 0xC0, 0xCF, 0xF0]);
    }

    #[test]
    fn empty_input_encodes_to_zero_length() {
        let encoded = encode_string(b"");
        let mut r = BitStreamReader::new(&encoded);
        assert_eq!(r.read_compressed_u16().unwrap(), 0);
        assert_eq!(decode_string(&encoded, 100), b"");
    }

    #[test]
    fn max_len_truncates_output() {
        let encoded = encode_string(b"abcdef");
        assert_eq!(decode_string(&encoded, 3), b"abc");
    }
}
