#![no_main]
use libfuzzer_sys::fuzz_target;
use std::str::FromStr;

fuzz_target!(|data: &str| {
    _ = modus_lib::modusfile::Modusfile::from_str(data);
});
