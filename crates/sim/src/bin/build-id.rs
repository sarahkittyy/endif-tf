//! Prints the build identity of this checkout (the git commit). `build-web.sh` bakes it into
//! `index.html` so the page can compare itself to the server's `GET /build` before downloading
//! the wasm.

fn main() {
    println!("{}", endif_sim::BUILD_ID);
}
