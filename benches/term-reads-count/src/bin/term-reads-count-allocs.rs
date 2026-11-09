//! The allocation pass, `term-reads-count --count allocs` (see the library): mimalloc,
//! counted.

#[global_allocator]
static GLOBAL: term_reads_count::Counting<mimalloc::MiMalloc> =
    term_reads_count::Counting(mimalloc::MiMalloc);

fn main() -> std::process::ExitCode {
    term_reads_count::main(true)
}
