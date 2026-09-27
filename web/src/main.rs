#[cfg(target_arch = "wasm32")]
mod app;
#[cfg(target_arch = "wasm32")]
mod packages;
#[cfg(target_arch = "wasm32")]
mod storage;
#[cfg(target_arch = "wasm32")]
mod window_restore;

#[cfg(target_arch = "wasm32")]
fn main() -> iced::Result {
    app::run()
}

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    eprintln!("smudgy-web must be built for wasm32-unknown-unknown");
}
