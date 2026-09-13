mod wire;
mod wire_types;

#[allow(unused_imports)]
pub(crate) use wire::{
    decode_request, decode_response, encode_request, encode_response, WireError,
};
#[allow(unused_imports)]
pub(crate) use wire_types::{Operation, Request, Response};

#[cfg(test)]
mod wire_tests;
