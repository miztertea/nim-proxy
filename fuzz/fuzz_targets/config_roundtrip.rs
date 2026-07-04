// config.json is operator-edited by design (lockout recovery is a volume
// edit): parsing must never panic, and save -> load must be a fixpoint.
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    nim_proxy::fuzz_config::config_roundtrip(data);
});
