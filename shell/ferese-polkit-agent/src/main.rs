mod agent;
mod authority;
mod prompt;
mod session;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // SAFETY: PR_SET_DUMPABLE takes an integer value and does not dereference caller memory.
    unsafe {
        libc::prctl(libc::PR_SET_DUMPABLE, 0);
    }

    match std::env::args().nth(1).as_deref() {
        None => tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?
            .block_on(agent::run()),
        Some("--prompt") => prompt::run(),
        Some("--check") => session::check().map_err(Into::into),
        _ => Err("Usage: ferese-polkit-agent [--check]".into()),
    }
}
