//! Prints the protocol identity of this checkout. `build-web.sh` bakes it into `index.html` so the
//! page can compare itself to the server before downloading the wasm.

fn main() {
    println!("{}", endif_sim::protocol_id());
}
