//! `smudgy_inspector` — a standalone `DevTools` window for a smudgy session's v8
//! inspector.
//!
//! Usage: `smudgy_inspector <inspector-http-addr>` e.g. `smudgy_inspector 127.0.0.1:9229`
//!
//! The main smudgy app constructs a session runtime with an inspector bound to a
//! localhost address (debug mode) and spawns this helper with that address. We:
//!   1. GET `http://<addr>/json` (deno's inspector target list),
//!   2. pull `webSocketDebuggerUrl` (`ws://host:port/ws/<uuid>`),
//!   3. open a webview at the **embedded** `DevTools` frontend wired to that ws.
//!
//! Why a separate process: a webview needs its own native event loop (tao), which
//! cannot coexist with the main app's iced/winit loop in one process. As a sidecar
//! it sidesteps that entirely and is crash-isolated from the MUD client.
//!
//! `DevTools` frontend: the frontend (HTML/JS/assets) is **vendored and embedded** in
//! this binary (`devtools-frontend/`, provenance in `DEVTOOLS-FRONTEND.md`) and
//! served to the webview over a `devtools://` custom protocol. So there is no
//! network dependency and no third-party host with CDP access to the isolate — a
//! debugger frontend can read all your source and `Runtime.evaluate` arbitrary code
//! over the ws, so it must be code we ship, not a page we fetch at runtime. The
//! webview loads `js_app.html` (the V8/JS-only `DevTools` — the same entry
//! `deno --inspect` uses), which reads its `ws=` query param to connect.
//!
//! Override hook: `SMUDGY_INSPECTOR_FRONTEND` — a full URL template whose `{ws}` is
//! replaced with the scheme-less ws target (`host:port/ws/<uuid>`); set it to point
//! at an external/hosted frontend instead of the embedded one.

use std::borrow::Cow;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use http::{header, Request, Response};
use rust_embed::RustEmbed;
use tao::{
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoop},
    window::WindowBuilder,
};
use wry::WebViewBuilder;

/// The vendored, prebuilt `DevTools` frontend (V8/JS-only build), embedded so the
/// debugger UI loads offline and no remote host ever gets CDP access to the session.
/// See `DEVTOOLS-FRONTEND.md` for provenance and how to refresh it.
#[derive(RustEmbed)]
#[folder = "devtools-frontend/"]
struct Frontend;

/// Custom-protocol scheme the embedded frontend is served under.
const DEVTOOLS_SCHEME: &str = "devtools";

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // `--print-url`: resolve + print the ws target and `DevTools` URL, then exit (no
    // window). Handy for scripting / headless checks. Note the default URL is an
    // internal custom-protocol address that only resolves inside this webview; the
    // ws target is the portable part you can point any CDP client at.
    let dry_run = args.iter().any(|a| a == "--print-url");
    let addr = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .cloned()
        .context("usage: smudgy_inspector [--print-url] <inspector-http-addr e.g. 127.0.0.1:9229>")?;

    let ws = resolve_ws_target(&addr)
        .with_context(|| format!("failed to read inspector targets from http://{addr}/json"))?;
    let frontend = frontend_url(&ws);

    // Print both so the user can see exactly what loaded (and override if blank).
    println!("smudgy_inspector: ws target  = {ws}");
    println!("smudgy_inspector: devtools   = {frontend}");
    println!("smudgy_inspector: (frontend is embedded; override with SMUDGY_INSPECTOR_FRONTEND)");

    if dry_run {
        return Ok(());
    }

    let event_loop = EventLoop::new();
    let window = WindowBuilder::new()
        .with_title("smudgy — script inspector")
        .build(&event_loop)
        .context("failed to create inspector window")?;

    let _webview = WebViewBuilder::new()
        .with_custom_protocol(DEVTOOLS_SCHEME.into(), |_id, request| serve_asset(&request))
        .with_url(&frontend)
        .build(&window)
        .context("failed to create webview (is the WebView2 runtime installed?)")?;

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        if let Event::WindowEvent {
            event: WindowEvent::CloseRequested,
            ..
        } = event
        {
            *control_flow = ControlFlow::Exit;
        }
    });
}

/// Build the `DevTools` frontend URL for a scheme-less ws target (`host:port/path`).
///
/// Default points at the embedded frontend's custom protocol. wry maps that to
/// `http://<scheme>.localhost/` on Windows/Android (`WebView2`) and
/// `<scheme>://localhost/` on macOS/Linux; `*.localhost` is a secure context in
/// Chromium, which `DevTools` requires.
fn frontend_url(ws: &str) -> String {
    if let Ok(template) = std::env::var("SMUDGY_INSPECTOR_FRONTEND") {
        return template.replace("{ws}", ws);
    }
    #[cfg(any(windows, target_os = "android"))]
    let base = format!("http://{DEVTOOLS_SCHEME}.localhost");
    #[cfg(not(any(windows, target_os = "android")))]
    let base = format!("{DEVTOOLS_SCHEME}://localhost");
    format!("{base}/js_app.html?ws={ws}&experiments=true&v8only=true")
}

/// Serve a file from the embedded `DevTools` frontend over the custom protocol. The
/// requested path is taken from the URI (query string — e.g. `?ws=…` — is ignored
/// here; the page reads it from `location.search`). A bare `/` serves `js_app.html`.
fn serve_asset(request: &Request<Vec<u8>>) -> Response<Cow<'static, [u8]>> {
    let path = request.uri().path().trim_start_matches('/');
    let path = if path.is_empty() { "js_app.html" } else { path };
    match Frontend::get(path) {
        Some(file) => Response::builder()
            .header(header::CONTENT_TYPE, mime_for(path))
            .body(file.data)
            .expect("static content-type header is valid"),
        None => not_found(),
    }
}

fn not_found() -> Response<Cow<'static, [u8]>> {
    Response::builder()
        .status(404)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(Cow::Borrowed(&b"Not Found"[..]))
        .expect("static 404 response is valid")
}

/// Map a file extension to a content type. The embedded frontend only ships
/// `.js/.html/.json/.svg/.png/.avif/.md`; the rest are belt-and-suspenders for a
/// future refresh. Correct MIME matters: `WebView2` refuses to run an ES module
/// served as `application/octet-stream`.
fn mime_for(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "avif" => "image/avif",
        "md" => "text/markdown; charset=utf-8",
        "wasm" => "application/wasm",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

/// GET `http://<addr>/json`, parse it, and return the scheme-less ws target
/// (`host:port/ws/<uuid>`) of the first inspector target. Retries briefly so a
/// just-spawned helper doesn't race the inspector server coming up.
fn resolve_ws_target(addr: &str) -> Result<String> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match http_get_json(addr, "/json") {
            Ok(body) => {
                let targets: serde_json::Value =
                    serde_json::from_str(&body).context("inspector /json was not valid JSON")?;
                let url = targets
                    .as_array()
                    .and_then(|a| a.first())
                    .and_then(|t| t.get("webSocketDebuggerUrl"))
                    .and_then(|u| u.as_str())
                    .ok_or_else(|| anyhow!("no webSocketDebuggerUrl in inspector /json: {body}"))?;
                // Strip the ws:// scheme; the `DevTools` frontend wants host:port/path.
                return Ok(url
                    .strip_prefix("ws://")
                    .or_else(|| url.strip_prefix("wss://"))
                    .unwrap_or(url)
                    .to_string());
            }
            // Retry briefly: the helper may be spawned before the inspector's HTTP
            // server is accepting connections.
            Err(e) if Instant::now() >= deadline => return Err(e),
            Err(_) => std::thread::sleep(Duration::from_millis(150)),
        }
    }
}

/// Minimal HTTP/1.0 GET for a localhost endpoint (the inspector serves plain HTTP).
/// Avoids pulling an HTTP client crate into this tiny sidecar.
fn http_get_json(addr: &str, path: &str) -> Result<String> {
    let mut stream =
        TcpStream::connect(addr).with_context(|| format!("connect to inspector at {addr}"))?;
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    write!(
        stream,
        "GET {path} HTTP/1.0\r\nHost: {addr}\r\nAccept: application/json\r\nConnection: close\r\n\r\n"
    )?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    let (head, body) = response
        .split_once("\r\n\r\n")
        .ok_or_else(|| anyhow!("malformed HTTP response from inspector"))?;
    if !head.lines().next().unwrap_or_default().contains("200") {
        bail!("inspector returned non-200: {}", head.lines().next().unwrap_or_default());
    }
    Ok(body.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    #[test]
    fn frontend_url_templating() {
        // Env override wins and substitutes {ws}.
        std::env::set_var("SMUDGY_INSPECTOR_FRONTEND", "https://x/devtools?ws={ws}");
        assert_eq!(
            frontend_url("127.0.0.1:9229/ws/abc"),
            "https://x/devtools?ws=127.0.0.1:9229/ws/abc"
        );
        std::env::remove_var("SMUDGY_INSPECTOR_FRONTEND");
        // Default form points at the embedded frontend and embeds the ws target.
        let url = frontend_url("127.0.0.1:9229/ws/abc");
        assert!(url.contains(DEVTOOLS_SCHEME));
        assert!(url.contains("/js_app.html?ws=127.0.0.1:9229/ws/abc"));
    }

    #[test]
    fn embedded_frontend_has_js_app_entry() {
        // The whole point of embedding: the V8 `DevTools` entry + its bootstrap module
        // are baked into the binary.
        assert!(Frontend::get("js_app.html").is_some());
        assert!(Frontend::get("entrypoints/js_app/js_app.js").is_some());
    }

    #[test]
    fn serves_embedded_asset_with_correct_mime() {
        let req = Request::builder()
            .uri("http://devtools.localhost/js_app.html?ws=127.0.0.1:9/ws/x")
            .body(Vec::new())
            .unwrap();
        let resp = serve_asset(&req);
        assert_eq!(resp.status(), 200);
        assert_eq!(
            resp.headers().get(header::CONTENT_TYPE).unwrap(),
            "text/html; charset=utf-8"
        );
        assert!(!resp.body().is_empty());
    }

    #[test]
    fn unknown_asset_is_404() {
        let req = Request::builder()
            .uri("http://devtools.localhost/does-not-exist.js")
            .body(Vec::new())
            .unwrap();
        assert_eq!(serve_asset(&req).status(), 404);
    }

    #[test]
    fn mime_for_known_types() {
        assert_eq!(mime_for("js_app.html"), "text/html; charset=utf-8");
        assert_eq!(
            mime_for("entrypoints/js_app/js_app.js"),
            "text/javascript; charset=utf-8"
        );
        assert_eq!(mime_for("Images/foo.svg"), "image/svg+xml");
        assert_eq!(mime_for("whatever.unknown"), "application/octet-stream");
    }

    #[test]
    fn resolves_ws_target_from_canned_json() {
        // Mock the inspector's /json on a real localhost socket and confirm we
        // extract + strip the ws:// scheme exactly as the helper will at runtime.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        // Detached loop-accept so the resolver's retry path never hits a dropped
        // listener; the thread dies with the test process.
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(mut s) = conn else { break };
                let mut buf = [0u8; 1024];
                let _ = s.read(&mut buf);
                let body =
                    r#"[{"webSocketDebuggerUrl":"ws://127.0.0.1:9999/ws/uuid-1","type":"node"}]"#;
                let resp = format!(
                    "HTTP/1.0 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = s.write_all(resp.as_bytes());
            }
        });
        let ws = resolve_ws_target(&addr.to_string()).unwrap();
        assert_eq!(ws, "127.0.0.1:9999/ws/uuid-1");
    }
}
