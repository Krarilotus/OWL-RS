#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| nrese_fuzz::Target::TriG.run(data));
