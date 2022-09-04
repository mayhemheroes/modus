#![no_main]
use libfuzzer_sys::fuzz_target;
use std::str::FromStr;

fuzz_target!(|data: &str| {
    _ = modus_lib::dockerfile::Dockerfile::from_str(data);
});
