//! The instruction pass (see the library): plain mimalloc, as the server ships.

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() -> std::process::ExitCode {
    term_reads_count::main(false)
}
