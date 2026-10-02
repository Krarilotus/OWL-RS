#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| nrese_fuzz::Target::ResultsXml.run(data));
