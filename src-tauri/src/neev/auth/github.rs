// The existing GitHub App's verified-email permission has not been established.
// Keep this provider explicitly unavailable and unmounted until that external
// capability is positively verified; repository grants are a separate system.
pub(crate) fn available() -> bool {
    false
}
