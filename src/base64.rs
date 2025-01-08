use base64::{
    alphabet,
    engine::{self, general_purpose, DecodePaddingMode},
    Engine as _,
};

const BASE_64_ENGINE: engine::GeneralPurpose = engine::GeneralPurpose::new(
    &alphabet::STANDARD,
    general_purpose::PAD
        .with_encode_padding(true)
        .with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

pub fn base_64_encode_bytes_to_string(input: &[u8]) -> String {
    BASE_64_ENGINE.encode(input)
}

pub fn base_64_decode_string_to_bytes(input: &str) -> Vec<u8> {
    BASE_64_ENGINE.decode(input).unwrap()
}
