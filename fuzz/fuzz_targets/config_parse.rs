#![no_main]

use libfuzzer_sys::fuzz_target;
use missive_core::MissiveConfig;

const MAX_INPUT_BYTES: usize = 64 * 1024;

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT_BYTES {
        return;
    }

    let Ok(input) = std::str::from_utf8(data) else {
        return;
    };

    if let Ok(config) = MissiveConfig::from_toml_str(input) {
        let _ = config.validate();
        let _ = config.profile(&config.default_profile);
        let _ = config.to_redacted_json();
        let _ = config.to_redacted_pretty_json();
    }
});
