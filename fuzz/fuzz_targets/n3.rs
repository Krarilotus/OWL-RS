#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| nrese_fuzz::Target::N3.run(data));
