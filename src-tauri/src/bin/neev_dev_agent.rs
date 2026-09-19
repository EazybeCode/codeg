//! Loopback, test-only dev-agent server (see `neev::dev_agent`). Built only with
//! `--features dev-agent`; never part of a production image.
fn main() -> std::process::ExitCode {
    codeg_lib::neev::dev_agent::run()
}
