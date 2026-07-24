//! P5136's symmetric packet payload transform and checksum.

const KEY_XORS: [u32; 4] = [347_277_256, 2_361_332_396, 604_215_233, 4_089_260_480];

fn derived_keys(key: u32) -> [u32; 4] {
    KEY_XORS.map(|mask| key ^ mask)
}

fn read_word(block: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        block[offset],
        block[offset + 1],
        block[offset + 2],
        block[offset + 3],
    ])
}

fn write_word(block: &mut [u8], offset: usize, value: u32) {
    block[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn key_stream(keys: [u32; 4]) -> [u8; 16] {
    let mut stream = [0_u8; 16];
    for (index, key) in keys.into_iter().enumerate() {
        let offset = index * 4;
        stream[offset..offset + 4].copy_from_slice(&key.to_le_bytes());
    }
    stream
}

/// Encrypts a payload in place and returns its plaintext checksum.
pub fn encrypt_in_place(data: &mut [u8], key: u32) -> u32 {
    let keys = derived_keys(key);
    let full_length = data.len() / 16 * 16;
    let mut checksum = 0_u32;

    for block in data[..full_length].chunks_exact_mut(16) {
        for (word_index, key_word) in keys.into_iter().enumerate() {
            let offset = word_index * 4;
            let plaintext = read_word(block, offset);
            checksum ^= plaintext;
            write_word(block, offset, plaintext ^ key_word);
        }
    }

    let stream = key_stream(keys);
    for (index, byte) in data[full_length..].iter_mut().enumerate() {
        checksum ^= u32::from(*byte) << index;
        *byte ^= stream[index];
    }

    checksum
}

/// Decrypts a payload in place and returns its plaintext checksum.
pub fn decrypt_in_place(data: &mut [u8], key: u32) -> u32 {
    let keys = derived_keys(key);
    let full_length = data.len() / 16 * 16;
    let mut checksum = 0_u32;

    for block in data[..full_length].chunks_exact_mut(16) {
        for (word_index, key_word) in keys.into_iter().enumerate() {
            let offset = word_index * 4;
            let plaintext = read_word(block, offset) ^ key_word;
            write_word(block, offset, plaintext);
            checksum ^= plaintext;
        }
    }

    let stream = key_stream(keys);
    for (index, byte) in data[full_length..].iter_mut().enumerate() {
        *byte ^= stream[index];
        checksum ^= u32::from(*byte) << index;
    }

    checksum
}

#[cfg(test)]
mod tests {
    use super::{decrypt_in_place, encrypt_in_place};

    #[test]
    fn all_block_and_tail_lengths_round_trip() {
        for length in 0_u8..64 {
            let original = (0..length).collect::<Vec<_>>();
            let mut encrypted = original.clone();
            let encrypt_checksum = encrypt_in_place(&mut encrypted, 0xa1b7_1c9b);
            let decrypt_checksum = decrypt_in_place(&mut encrypted, 0xa1b7_1c9b);

            assert_eq!(encrypted, original);
            assert_eq!(decrypt_checksum, encrypt_checksum);
        }
    }
}
