#[cfg(target_arch = "wasm32")]
#[path = "../script_host.rs"]
mod script_host;
#[cfg(target_arch = "wasm32")]
#[path = "../worker.rs"]
mod session_worker;

#[cfg(target_arch = "wasm32")]
fn main() {}

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    eprintln!("smudgy-web-worker must be built for wasm32-unknown-unknown");
}
