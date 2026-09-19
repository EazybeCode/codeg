pub(crate) mod issuer;
pub(crate) mod routes;
pub(crate) mod transport;
mod wire;
mod wire_types;
#[cfg(test)]
mod issuer_tests;
#[cfg(test)]
mod routes_tests;

#[allow(unused_imports)]
pub(crate) use wire::{
    decode_request, decode_response, encode_request, encode_response, WireError,
};
#[allow(unused_imports)]
pub(crate) use wire_types::{Operation, Request, Response};
pub(crate) use wire_types::{Email as GatewayEmail, Name as GatewayName};

#[cfg(test)]
mod wire_tests;
