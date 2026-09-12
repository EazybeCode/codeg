pub(crate) mod access;
pub(crate) mod backup;
#[cfg(all(test, unix))]
mod backup_tests;
mod blob;
#[cfg(all(test, unix))]
mod blob_tests;
#[cfg(test)]
mod document_tests;
pub(crate) mod documents;
#[cfg(test)]
mod message_tests;
pub(crate) mod messages;
#[cfg(test)]
mod resource_tests;
pub(crate) mod resources;
#[cfg(test)]
mod test_support;
pub(crate) mod types;

pub(crate) use blob::{BlobInfo, BlobStore};
