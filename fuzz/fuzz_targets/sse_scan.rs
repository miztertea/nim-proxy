// Upstream SSE bytes arrive fragmented however the network pleases; the
// scanner must never panic and must keep its buffer bounded.
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    nim_proxy::fuzz_proxy::sse_scan(data);
});
