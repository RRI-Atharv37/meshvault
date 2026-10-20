use meshvault_core::chunk::{chunk_list_hash, chunk_stream, ChunkRef, CHUNK_MAX, CHUNK_MIN};
use meshvault_core::hash::{hash_bytes, Hash, HashWriter};
use proptest::prelude::*;
use std::collections::HashSet;

fn pseudo_random(seed: u64, len: usize) -> Vec<u8> {
    let mut state = seed | 1;
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out.extend_from_slice(&state.to_le_bytes());
    }
    out.truncate(len);
    out
}

fn chunks_of(data: &[u8]) -> Vec<(ChunkRef, Vec<u8>)> {
    chunk_stream(data).collect::<Result<_, _>>().unwrap()
}

fn hash_set(chunks: &[(ChunkRef, Vec<u8>)]) -> HashSet<Hash> {
    chunks.iter().map(|(c, _)| c.hash).collect()
}

fn unchanged_fraction(before: &[(ChunkRef, Vec<u8>)], after: &[(ChunkRef, Vec<u8>)]) -> f64 {
    let after_set = hash_set(after);
    let kept = before
        .iter()
        .filter(|(c, _)| after_set.contains(&c.hash))
        .count();
    kept as f64 / before.len() as f64
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn chunk_round_trip(seed in any::<u64>(), len in 0usize..1_500_000) {
        let data = pseudo_random(seed, len);
        let chunks = chunks_of(&data);
        let joined: Vec<u8> = chunks.iter().flat_map(|(_, d)| d.iter().copied()).collect();
        prop_assert_eq!(joined, data);
        let mut offset = 0u64;
        for (c, d) in &chunks {
            prop_assert_eq!(c.offset, offset);
            prop_assert_eq!(c.len as usize, d.len());
            offset += u64::from(c.len);
        }
    }

    #[test]
    fn chunk_sizes_within_bounds(seed in any::<u64>(), len in 0usize..1_500_000) {
        let chunks = chunks_of(&pseudo_random(seed, len));
        let last = chunks.len().saturating_sub(1);
        for (i, (c, _)) in chunks.iter().enumerate() {
            prop_assert!(c.len <= CHUNK_MAX);
            if i != last {
                prop_assert!(c.len >= CHUNK_MIN);
            }
        }
    }

    #[test]
    fn chunk_hashes_match_data(seed in any::<u64>(), len in 0usize..1_500_000) {
        for (c, d) in chunks_of(&pseudo_random(seed, len)) {
            prop_assert_eq!(c.hash, hash_bytes(&d));
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]

    #[test]
    fn chunk_boundaries_stable_after_insert(seed in any::<u64>(), at in 4_000_000usize..12_000_000, n in 1usize..=1024) {
        let data = pseudo_random(seed, 16 * 1024 * 1024);
        let mut edited = data.clone();
        let insert = pseudo_random(seed ^ 0xdead_beef, n);
        edited.splice(at..at, insert);
        let (before, after) = (chunks_of(&data), chunks_of(&edited));
        prop_assert!(unchanged_fraction(&before, &after) >= 0.8);
    }

    #[test]
    fn chunk_boundaries_stable_after_delete(seed in any::<u64>(), at in 4_000_000usize..12_000_000, n in 1usize..=1024) {
        let data = pseudo_random(seed, 16 * 1024 * 1024);
        let mut edited = data.clone();
        edited.drain(at..at + n);
        let (before, after) = (chunks_of(&data), chunks_of(&edited));
        prop_assert!(unchanged_fraction(&before, &after) >= 0.8);
    }
}

#[test]
fn chunk_empty_input_yields_nothing() {
    assert!(chunks_of(&[]).is_empty());
}

#[test]
fn chunk_small_input_one_chunk() {
    let data = pseudo_random(7, 10_000);
    let chunks = chunks_of(&data);
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].0.offset, 0);
    assert_eq!(chunks[0].0.len, 10_000);
    assert_eq!(chunks[0].0.hash, hash_bytes(&data));
}

fn sample_chunks() -> [ChunkRef; 2] {
    [
        ChunkRef {
            offset: 0,
            len: 300_000,
            hash: hash_bytes(b"first"),
        },
        ChunkRef {
            offset: 300_000,
            len: 5,
            hash: hash_bytes(b"second"),
        },
    ]
}

#[test]
fn chunk_list_hash_changes_with_order() {
    let [a, b] = sample_chunks();
    assert_ne!(chunk_list_hash(&[a, b]), chunk_list_hash(&[b, a]));
}

#[test]
fn chunk_list_hash_golden() {
    let chunks = sample_chunks();
    let mut layout = Vec::new();
    for c in &chunks {
        layout.extend_from_slice(&c.offset.to_le_bytes());
        layout.extend_from_slice(&c.len.to_le_bytes());
        layout.extend_from_slice(&c.hash.0);
    }
    assert_eq!(chunk_list_hash(&chunks), hash_bytes(&layout));
    assert_eq!(
        chunk_list_hash(&chunks).to_string(),
        "364ffb4efe218b2ebd3c984b97a49eeaee566931eb900b0f1e3e1801befe4f53"
    );
}

#[test]
fn hash_display_is_lowercase_hex() {
    let mut bytes = [0u8; 32];
    bytes[0] = 0xAB;
    bytes[31] = 0x0F;
    let shown = Hash(bytes).to_string();
    assert_eq!(shown.len(), 64);
    assert!(shown.starts_with("ab"));
    assert!(shown.ends_with("0f"));
    assert_eq!(shown, shown.to_lowercase());
    assert_eq!(format!("{:?}", Hash::ZERO), "0".repeat(64));
}

#[test]
fn hash_golden_blake3() {
    assert_eq!(
        hash_bytes(b"").to_string(),
        "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
    );
    assert_eq!(
        hash_bytes(&[0]).to_string(),
        "2d3adedff11b61f14c886e35afa036736dcd87a74d27b5c1510225d0f592e213"
    );
    let mut w = HashWriter::new();
    w.update(&[]);
    assert_eq!(w.finish(), hash_bytes(b""));
    let data = pseudo_random(3, 100_000);
    let mut w = HashWriter::new();
    for part in data.chunks(777) {
        w.update(part);
    }
    assert_eq!(w.finish(), hash_bytes(&data));
}
