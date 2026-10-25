#![no_main]

use std::collections::HashMap;

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(text) = std::str::from_utf8(data) {
        let _ =
            noos_package_manager::backend::pacman::parse_pacman_ss_output(text, &HashMap::new());
    }
});
