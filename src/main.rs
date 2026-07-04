// Thin binary shim: the whole application lives in the library crate (see
// src/lib.rs) so that the fuzz targets under fuzz/ can link its internals.
fn main() {
    nim_proxy::run()
}
