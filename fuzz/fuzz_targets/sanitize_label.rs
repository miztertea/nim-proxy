// Client-controlled strings headed for Prometheus label positions: the
// sanitizer's output invariants (bounded, non-empty, safe charset) are the
// metric-injection defense.
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    nim_proxy::fuzz_proxy::sanitize_label(data);
});
