const KR_MIXING_STRING: &str = "y&errfV6GRS!e8JL";

const LINEAR_TABLE_4_BASIS: [u32; 8] = [
    0x180f_40cd,
    0x301e_8033,
    0x603c_a966,
    0xc078_fbcc,
    0x29f0_5f31,
    0x5249_be62,
    0xa492_d5c4,
    0xe18d_0321,
];

const LINEAR_TABLE_5_BASIS: [u32; 8] = [
    0xe19f_cf13,
    0x6b97_3726,
    0xd687_6e4c,
    0x05a7_dc98,
    0x0ae7_1199,
    0x1467_229b,
    0x28ce_449f,
    0x5035_8897,
];

#[derive(Clone)]
pub(crate) struct KeyProvider {
    state: [u32; 20],
    words: [u32; 16],
    word_position: usize,
}

impl KeyProvider {
    pub(crate) fn for_header(archive_name: &str) -> Self {
        Self::from_key(&header_key(archive_name))
    }

    pub(crate) fn for_table(archive_name: &str) -> Self {
        Self::from_key(&table_key(archive_name))
    }

    // This is a direct, state-by-state transcription of the two-round P5136
    // key schedule. Keeping the assignments together makes alias ordering
    // auditable against the original ref-parameter operations.
    #[allow(clippy::too_many_lines)]
    pub(crate) fn from_key(key: &[u8; 16]) -> Self {
        let mut state = [0_u32; 20];
        for i in 0..4 {
            let mut word = 0_u32;
            for j in 0..4 {
                // The original implementation casts byte -> sbyte -> int -> uint.
                // This explicit widening preserves that sign extension.
                let signed = i8::from_ne_bytes([key[i * 4 + j]]);
                let widened = i32::from(signed).cast_unsigned();
                word = word.wrapping_shl(8) | widened;
            }
            state[i] = word;
            state[i + 8] = word;
            state[i + 4] = !word;
            state[i + 12] = !word;
        }

        let mut temporary = [0_u32; 21];
        temporary[0] = state[0];
        temporary[1] = state[8];
        temporary[2] = state[1];
        temporary[3] = state[9];
        temporary[4] = state[2];
        temporary[5] = state[10];
        temporary[6] = state[3];
        temporary[7] = state[4];
        temporary[8] = state[5];
        temporary[9] = state[7];
        temporary[10] = state[14];
        temporary[11] = state[11];
        temporary[12] = state[6];
        temporary[13] = state[12];
        temporary[14] = state[15];
        temporary[15] = state[13];
        temporary[16] = 0;
        temporary[17] = state[13];
        temporary[18] = 0;
        temporary[19] = 2;

        for _ in 0..2 {
            let mut last_sum = temporary[16].wrapping_add(temporary[0]) ^ temporary[18];
            temporary[18] = temporary[18].wrapping_add(temporary[5]);

            temporary[14] = table(5, temporary[14])
                ^ table(4, temporary[7])
                ^ temporary[14].wrapping_shl(8)
                ^ (temporary[7] >> 8)
                ^ temporary[17]
                ^ last_sum;
            last_sum = temporary[14].wrapping_add(temporary[18]);

            let result = two_values(
                temporary[10],
                temporary[16],
                temporary[6],
                temporary[13],
                temporary[3],
                last_sum,
            );
            temporary[10] = result.0;
            temporary[16] = result.1;
            last_sum = result.2;

            let result = two_values(
                temporary[15],
                temporary[18],
                temporary[4],
                temporary[11],
                temporary[1],
                last_sum,
            );
            temporary[15] = result.0;
            temporary[17] = result.1;
            last_sum = result.2;

            let result = two_values(
                temporary[13],
                temporary[16],
                temporary[2],
                temporary[5],
                temporary[9],
                last_sum,
            );
            temporary[13] = result.0;
            state[18] = result.1;
            last_sum = result.2;

            let saved_state_18 = state[18];
            let result = two_values(
                temporary[11],
                temporary[17],
                temporary[0],
                temporary[3],
                temporary[12],
                last_sum,
            );
            temporary[18] = saved_state_18;
            temporary[11] = result.0;
            state[18] = result.1;
            last_sum = result.2;

            let result = two_values(
                temporary[5],
                temporary[18],
                temporary[14],
                temporary[1],
                temporary[8],
                last_sum,
            );
            temporary[5] = result.0;
            temporary[18] = result.1;
            last_sum = result.2;

            let old_temporary_18 = temporary[18];
            let result = two_values(
                temporary[3],
                state[18],
                temporary[10],
                temporary[9],
                temporary[7],
                last_sum,
            );
            state[18] = old_temporary_18;
            temporary[3] = result.0;
            temporary[18] = result.1;
            last_sum = result.2;

            let old_temporary_18 = temporary[18];
            let result = two_values(
                temporary[1],
                state[18],
                temporary[15],
                temporary[12],
                temporary[6],
                last_sum,
            );
            state[18] = old_temporary_18;
            temporary[1] = result.0;
            temporary[18] = result.1;
            last_sum = result.2;

            let old_temporary_18 = temporary[18];
            let result = two_values(
                temporary[9],
                state[18],
                temporary[13],
                temporary[8],
                temporary[4],
                last_sum,
            );
            state[18] = old_temporary_18;
            temporary[9] = result.0;
            temporary[18] = result.1;
            last_sum = result.2;

            let old_temporary_18 = temporary[18];
            let result = two_values(
                temporary[12],
                state[18],
                temporary[11],
                temporary[7],
                temporary[2],
                last_sum,
            );
            state[18] = old_temporary_18;
            temporary[12] = result.0;
            temporary[18] = result.1;
            last_sum = result.2;

            let old_temporary_18 = temporary[18];
            let result = two_values(
                temporary[8],
                state[18],
                temporary[5],
                temporary[6],
                temporary[0],
                last_sum,
            );
            state[18] = old_temporary_18;
            temporary[8] = result.0;
            temporary[18] = result.1;
            last_sum = result.2;

            let old_temporary_18 = temporary[18];
            let result = two_values(
                temporary[7],
                state[18],
                temporary[3],
                temporary[4],
                temporary[14],
                last_sum,
            );
            state[18] = old_temporary_18;
            temporary[7] = result.0;
            temporary[18] = result.1;
            last_sum = result.2;

            let old_temporary_18 = temporary[18];
            let result = two_values(
                temporary[6],
                state[18],
                temporary[1],
                temporary[2],
                temporary[10],
                last_sum,
            );
            state[18] = old_temporary_18;
            temporary[6] = result.0;
            temporary[18] = result.1;
            last_sum = result.2;

            let old_temporary_18 = temporary[18];
            let result = two_values(
                temporary[4],
                state[18],
                temporary[9],
                temporary[0],
                temporary[15],
                last_sum,
            );
            state[18] = old_temporary_18;
            temporary[4] = result.0;
            temporary[16] = result.1;
            last_sum = result.2;

            let result = two_values(
                temporary[2],
                state[18],
                temporary[12],
                temporary[14],
                temporary[13],
                last_sum,
            );
            temporary[2] = result.0;
            state[18] = result.1;
            last_sum = result.2;

            temporary[20] = table_03(temporary[16]) ^ last_sum;

            let result = two_values(
                temporary[0],
                temporary[16],
                temporary[8],
                temporary[10],
                temporary[11],
                last_sum,
            );
            temporary[0] = result.0;
            temporary[16] = result.1;

            temporary[18] = table_03(state[18]);
            temporary[19] = temporary[19].wrapping_sub(1);
            state[18] = temporary[16];
            temporary[17] = temporary[15];
        }

        state[0] = temporary[0];
        state[8] = temporary[1];
        state[1] = temporary[2];
        state[9] = temporary[3];
        state[2] = temporary[4];
        state[10] = temporary[5];
        state[3] = temporary[6];
        state[11] = temporary[11];
        state[4] = temporary[7];
        state[5] = temporary[8];
        state[6] = temporary[12];
        state[7] = temporary[9];
        state[16] = temporary[20];
        state[15] = temporary[14];
        state[13] = temporary[15];
        state[17] = temporary[16];
        state[14] = temporary[10];
        state[19] = temporary[18];
        state[12] = temporary[13];

        Self {
            state,
            words: [0; 16],
            word_position: 16,
        }
    }

    pub(crate) fn next_word(&mut self) -> u32 {
        if self.word_position == self.words.len() {
            self.refresh();
        }
        let word = self.words[self.word_position];
        self.word_position += 1;
        word
    }

    fn refresh(&mut self) {
        for i in 0..16 {
            let index_5 = ((0x40 + 0x3c - (i << 2)) & 0x3f) >> 2;
            let index_4 = ((0x40 + 0x10 - (i << 2)) & 0x3f) >> 2;
            let index_c = ((0x40 + 0x34 - (i << 2)) & 0x3f) >> 2;
            let index_b = ((0x40 + 0x28 - (i << 2)) & 0x3f) >> 2;
            let index_other = ((0x40 + 0x38 - (i << 2)) & 0x3f) >> 2;

            let transformed = table(5, self.state[index_5])
                ^ table(4, self.state[index_4])
                ^ self.state[index_5].wrapping_shl(8)
                ^ self.state[index_c]
                ^ (self.state[index_4] >> 8);
            let added = self.state[19].wrapping_add(self.state[index_b]);
            let substituted = table_03(self.state[18]);
            let output = transformed.wrapping_add(added) ^ substituted ^ self.state[index_other];

            self.state[index_5] = transformed;
            self.state[18] = added;
            self.state[19] = substituted;
            self.words[i] = output;
        }
        self.word_position = 0;
    }
}

pub(crate) fn packed_file_key(checksum: &[u8; 16], path: &str) -> [u8; 16] {
    let u1 = file_key_u1(KR_MIXING_STRING);
    let digits: Vec<u8> = u1.to_string().bytes().map(|byte| byte - b'0').collect();
    let utf16: Vec<u16> = path.encode_utf16().collect();
    debug_assert!(!utf16.is_empty());

    let mut output = [0_u8; 16];
    for (i, output_byte) in output.iter_mut().enumerate() {
        let i_u8 = u8::try_from(i).expect("packed key has exactly 16 bytes");
        let i_i32 = i32::from(i_u8);
        let a = i32::from(digits[i % digits.len()] & 1);
        let b = i32::from(digits[(i + 1) % digits.len()]);
        let c = usize::from(digits[(i + 2) % digits.len()].wrapping_add(i_u8) & 0x0f);
        let mut value = (b + i_i32) % 5;
        value = i32::from((value + i32::from(checksum[c]) + a).to_le_bytes()[0].cast_signed());
        let file_byte = utf16[i % utf16.len()].to_le_bytes()[0].cast_signed();
        value = value.wrapping_mul(i32::from(file_byte)).wrapping_add(i_i32);
        *output_byte = value.to_le_bytes()[0];
    }
    output
}

pub(crate) fn decrypt_in_place(data: &mut [u8], key: &[u8; 16]) {
    let mut provider = KeyProvider::from_key(key);
    for chunk in data.chunks_mut(4) {
        let mut bytes = [0_u8; 4];
        bytes[..chunk.len()].copy_from_slice(chunk);
        let plaintext = u32::from_le_bytes(bytes).wrapping_sub(provider.next_word());
        chunk.copy_from_slice(&plaintext.to_le_bytes()[..chunk.len()]);
    }
}

#[cfg(test)]
pub(crate) fn encrypt_in_place(data: &mut [u8], key: &[u8; 16]) {
    let mut provider = KeyProvider::from_key(key);
    for chunk in data.chunks_mut(4) {
        let mut bytes = [0_u8; 4];
        bytes[..chunk.len()].copy_from_slice(chunk);
        let ciphertext = u32::from_le_bytes(bytes).wrapping_add(provider.next_word());
        chunk.copy_from_slice(&ciphertext.to_le_bytes()[..chunk.len()]);
    }
}

fn header_key(archive_name: &str) -> [u8; 16] {
    let combined: Vec<u16> = format!("{}{}", archive_name.to_ascii_lowercase(), KR_MIXING_STRING)
        .encode_utf16()
        .collect();
    std::array::from_fn(|i| {
        combined[i % combined.len()].to_le_bytes()[0]
            .wrapping_add(u8::try_from(i).expect("header key has exactly 16 bytes"))
    })
}

fn table_key(archive_name: &str) -> [u8; 16] {
    let combined: Vec<u16> = format!("{}{}", archive_name.to_ascii_lowercase(), KR_MIXING_STRING)
        .encode_utf16()
        .collect();
    std::array::from_fn(|i| {
        let index = i % combined.len();
        let multiplier = u8::try_from(i % 3 + 2).expect("table multiplier is between two and four");
        combined[combined.len() - index - 1].to_le_bytes()[0]
            .wrapping_mul(multiplier)
            .wrapping_add(u8::try_from(i).expect("table key has exactly 16 bytes"))
    })
}

fn file_key_u1(value: &str) -> u32 {
    value.encode_utf16().fold(0x811c_9dc5_u32, |hash, unit| {
        (hash ^ u32::from(unit)).wrapping_mul(0x0100_0193)
    })
}

fn two_values(
    set_value: u32,
    first_source: u32,
    left_source: u32,
    constant: u32,
    previous: u32,
    last_sum: u32,
) -> (u32, u32, u32) {
    let first_round = table_03(first_source);
    let second_round = table(5, set_value)
        ^ table(4, left_source)
        ^ set_value.wrapping_shl(8)
        ^ (left_source >> 8)
        ^ constant
        ^ first_round
        ^ last_sum;
    let second_value = previous.wrapping_add(first_round);
    (
        second_round,
        second_value,
        second_round.wrapping_add(second_value),
    )
}

fn table_03(number: u32) -> u32 {
    table(0, number) ^ table(1, number) ^ table(2, number) ^ table(3, number)
}

fn table(index: usize, number: u32) -> u32 {
    let bytes = number.to_be_bytes();
    match index {
        0 => aes_table(bytes[0]),
        1 => aes_table(bytes[1]).rotate_right(8),
        2 => aes_table(bytes[2]).rotate_right(16),
        3 => aes_table(bytes[3]).rotate_right(24),
        4 => linear_table(&LINEAR_TABLE_4_BASIS, bytes[3]),
        5 => linear_table(&LINEAR_TABLE_5_BASIS, bytes[0]),
        _ => unreachable!("RHO5 key table index is fixed"),
    }
}

fn aes_table(input: u8) -> u32 {
    let substituted = aes_sbox(input);
    let doubled = gf_multiply(substituted, 2);
    let tripled = doubled ^ substituted;
    (u32::from(doubled) << 24)
        | (u32::from(tripled) << 16)
        | (u32::from(substituted) << 8)
        | u32::from(substituted)
}

fn aes_sbox(input: u8) -> u8 {
    let inverse = if input == 0 { 0 } else { gf_power(input, 254) };
    inverse
        ^ inverse.rotate_left(1)
        ^ inverse.rotate_left(2)
        ^ inverse.rotate_left(3)
        ^ inverse.rotate_left(4)
        ^ 0x63
}

fn gf_power(mut base: u8, mut exponent: u8) -> u8 {
    let mut result = 1_u8;
    while exponent != 0 {
        if exponent & 1 != 0 {
            result = gf_multiply(result, base);
        }
        base = gf_multiply(base, base);
        exponent >>= 1;
    }
    result
}

fn gf_multiply(mut left: u8, mut right: u8) -> u8 {
    let mut result = 0_u8;
    while right != 0 {
        if right & 1 != 0 {
            result ^= left;
        }
        left = if left & 0x80 == 0 {
            left << 1
        } else {
            (left << 1) ^ 0x1b
        };
        right >>= 1;
    }
    result
}

fn linear_table(basis: &[u32; 8], input: u8) -> u32 {
    basis
        .iter()
        .enumerate()
        .filter(|(bit, _)| input & (1 << bit) != 0)
        .fold(0, |value, (_, basis_value)| value ^ basis_value)
}

#[cfg(test)]
mod tests {
    use super::{KeyProvider, packed_file_key};

    const ARCHIVE: &str = "DataPack1_00000.rho5";
    const TARGET_PATH: &str = "etc_/emblem/emblem@kr.xml";
    const TARGET_MD5: [u8; 16] = [
        0x4e, 0xb1, 0xba, 0xe1, 0x05, 0x9a, 0x03, 0xba, 0xfc, 0xe8, 0x04, 0xe0, 0xe8, 0xf3, 0x92,
        0x77,
    ];

    #[test]
    fn kr_golden_first_words_match_each_key_layer() {
        let mut header = KeyProvider::for_header(ARCHIVE);
        assert_eq!(
            std::array::from_fn::<_, 4, _>(|_| header.next_word()),
            [0x47f2_7f16, 0x522c_20e8, 0x47fa_7758, 0xbf8e_afb8]
        );

        let mut table = KeyProvider::for_table(ARCHIVE);
        assert_eq!(
            std::array::from_fn::<_, 4, _>(|_| table.next_word()),
            [0x8bfb_166c, 0x766b_8060, 0x0f44_21a8, 0x1edf_e743]
        );

        let packed = packed_file_key(&TARGET_MD5, TARGET_PATH);
        assert_eq!(
            packed,
            [
                0xb7, 0x45, 0x54, 0xe1, 0x5d, 0x33, 0xd3, 0x09, 0xfc, 0xa8, 0x1e, 0xd9, 0x0b, 0xf1,
                0xca, 0x0f,
            ]
        );
        let mut data = KeyProvider::from_key(&packed);
        assert_eq!(
            std::array::from_fn::<_, 4, _>(|_| data.next_word()),
            [0xc938_d5b5, 0x4fb4_b297, 0xa189_bdcb, 0xa88e_b8bd]
        );
    }
}
