#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodecLimits {
    pub max_pin_file_bytes: usize,
    pub max_encoded_block_bytes: usize,
    pub max_decoded_block_bytes: usize,
    pub max_string_code_units: usize,
    pub max_collection_items: usize,
    pub max_bml_depth: usize,
    pub max_bml_nodes: usize,
}

impl Default for CodecLimits {
    fn default() -> Self {
        Self {
            max_pin_file_bytes: 8 * 1024 * 1024,
            max_encoded_block_bytes: 8 * 1024 * 1024,
            max_decoded_block_bytes: 16 * 1024 * 1024,
            max_string_code_units: 32 * 1024,
            max_collection_items: 4 * 1024,
            max_bml_depth: 32,
            max_bml_nodes: 8 * 1024,
        }
    }
}
