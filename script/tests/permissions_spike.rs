//! Proves that a **restricted** `PermissionsContainer` (built via
//! `Permissions::from_options` + `PermissionsContainer::new`, *not* `allow_all`) actually
//! enforces deno-native permissions when handed to a [`ScriptRuntime`] — the mechanism by
//! which the per-package isolate factory sandboxes a package's net/fs/env (see
//! `script/PACKAGE-ISOLATES.md`).
//!
//! These tests cover the deno-native permission layer only; the `SmudgyGrants`
//! op-capability layer, which depends on the per-isolate `OpState`, is not exercised here.
//!
//! Each test is hermetic — no real network. The "allowed" host is a local one-shot HTTP
//! server bound to `127.0.0.1`; the "denied" host is reserved (`*.example`, RFC 2606) or
//! a port with nothing listening, and is rejected at the permission gate *before* any
//! connection is attempted, so there is no prompt and no hang.
//!
//! Tests 2 and 3 additionally pin two finer points of the deno-native model: `net`
//! host-vs-`host:port` granularity, and `allow_read` path-subtree semantics.

use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener};
use std::path::Path;
use std::rc::Rc;
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, Result};
use deno_core::{serde_v8, FastString, PollEventLoopOptions};
use serde_json::Value;
use smudgy_script::{
    permission_descriptor_parser, ModulePolicy, Permissions, PermissionsContainer,
    PermissionsOptions, ScriptRuntime, ScriptRuntimeOptions,
};

fn tokio_runtime() -> Rc<tokio::runtime::Runtime> {
    Rc::new(
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap(),
    )
}

/// Build a [`ScriptRuntime`] whose worker isolate is governed by a **restricted**
/// container constructed from `opts` — the same recipe the per-package isolate factory
/// uses to sandbox a package, applied to the `allow_all` construction site in
/// `script/src/lib.rs`.
fn restricted_runtime(
    data_dir: &Path,
    opts: &PermissionsOptions,
) -> Result<(Rc<tokio::runtime::Runtime>, ScriptRuntime)> {
    // `from_options` turns the manifest-shaped options into a `Permissions`;
    // `PermissionsContainer::new` wraps it. `None` for a field ⇒ that permission is denied.
    let parser = permission_descriptor_parser();
    let perms = Permissions::from_options(&*parser, opts)
        .context("Permissions::from_options should accept the restricted options")?;
    let container = PermissionsContainer::new(parser, perms);

    let tokio = tokio_runtime();
    let runtime = ScriptRuntime::new(ScriptRuntimeOptions {
        extensions: Vec::new(),
        data_dir: data_dir.to_path_buf(),
        webstorage_dir: None,
        module_policy: ModulePolicy { allow_https: true, ..Default::default() },
        inspector: None,
        tokio: tokio.clone(),
        package_provider: None,
        permissions: Some(container),
    })?;
    Ok((tokio, runtime))
}

/// Evaluate an async IIFE and deserialize its resolved value as JSON. Mirrors
/// `tests/runtime.rs::eval_async_bool`, but returns the whole result object so a single
/// script can report several outcomes (a permission denial is captured in-script, not
/// thrown, so the promise resolves and we can assert on the error's name/message).
fn eval_async_json(
    tokio: &tokio::runtime::Runtime,
    rt: &mut ScriptRuntime,
    source: &str,
) -> Result<Value> {
    tokio.block_on(async {
        let value = rt
            .deno_runtime()
            .execute_script("<permissions-spike>", FastString::from(source.to_string()))?;
        let promise = rt.deno_runtime().resolve(value);
        let value = rt
            .deno_runtime()
            .with_event_loop_future(promise, PollEventLoopOptions::default())
            .await?;
        deno_core::scope!(scope, rt.deno_runtime());
        let local = deno_core::v8::Local::new(scope, value);
        Ok(serde_v8::from_v8(scope, local)?)
    })
}

/// Escape a filesystem path for embedding in a double-quoted JS string literal
/// (Windows backslashes would otherwise be interpreted as escapes).
fn js_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "\\\\")
}

/// Spawn a one-shot HTTP/1.1 server on `127.0.0.1:0` that answers a single GET with
/// `200 OK` / body `ok`, then exits. Returns the bound port and the server thread's
/// handle (the thread owns the listener, keeping the port bound until the request is
/// served). The request headers are fully drained before responding so that closing the
/// socket cannot RST-truncate the response.
fn spawn_http_200_server() -> Result<(u16, JoinHandle<()>)> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let handle = std::thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));

        // Drain the request up to the end-of-headers marker.
        let mut request = Vec::new();
        let mut buf = [0u8; 512];
        loop {
            match stream.read(&mut buf) {
                // EOF or a read error: stop. (Merged: pedantic `match_same_arms`.)
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    request.extend_from_slice(&buf[..n]);
                    if request.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
            }
        }

        let body = b"ok";
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let _ = stream.write_all(response.as_bytes());
        let _ = stream.write_all(body);
        let _ = stream.flush();
        let _ = stream.shutdown(Shutdown::Both);
    });
    Ok((port, handle))
}

/// A port with nothing listening on it (bound then immediately released). Used as a
/// "denied" target: the permission gate rejects it before any connection, so the fact
/// that it is closed never matters — but if the gate were ever buggy and let it through,
/// the resulting connection error would differ from a permission error and fail the test.
fn unused_port() -> Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?.port())
}

fn name(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .unwrap_or("<missing>")
        .to_string()
}

/// A restricted container with `allow_net` for exactly one host:port (everything else
/// `None` ⇒ denied) enforces all three of:
///   (2a) `fetch` to the allowed host **succeeds**,
///   (2b) `fetch` to a denied host **rejects** with a permission error (no prompt/hang),
///   (3)  `Deno.readFile` outside any `allow_read` path **rejects**.
#[test]
fn restricted_container_enforces_deno_native_permissions() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (allowed_port, _server) = spawn_http_200_server()?;

    // net to the allowed host:port only; everything else None ⇒ denied
    // (no read/write/env/run/ffi/sys/import). prompt: false ⇒ a denied access fails
    // immediately rather than prompting.
    let opts = PermissionsOptions {
        allow_net: Some(vec![format!("127.0.0.1:{allowed_port}")]),
        prompt: false,
        ..Default::default()
    };
    let (tokio, mut rt) = restricted_runtime(temp.path(), &opts)?;

    // A real file the script will try (and fail) to read — outside every allow_read path
    // (there are none). Created host-side, so the write itself is not permission-gated.
    let secret = temp.path().join("secret.txt");
    std::fs::write(&secret, "top secret")?;

    let source = format!(
        r#"
        (async () => {{
          const out = {{}};

          // (2a) allowed host:port — passes check_net, then a real 200 from the local server.
          try {{
            const res = await fetch("http://127.0.0.1:{allowed_port}/");
            out.allowedStatus = res.status;
            out.allowedBody = await res.text();
            out.allowedError = null;
          }} catch (e) {{
            out.allowedStatus = 0;
            out.allowedError = (e?.name ?? "") + ": " + (e?.message ?? String(e));
          }}

          // (2b) denied host — rejected at the permission gate before any DNS/connect.
          try {{
            await fetch("http://denied.example/");
            out.deniedName = "NO_ERROR";
            out.deniedMessage = "";
          }} catch (e) {{
            out.deniedName = e?.name ?? String(e);
            out.deniedMessage = e?.message ?? String(e);
          }}

          // (3) read outside any allow_read path — must reject.
          try {{
            await Deno.readTextFile("{secret}");
            out.readName = "NO_ERROR";
            out.readMessage = "";
          }} catch (e) {{
            out.readName = e?.name ?? String(e);
            out.readMessage = e?.message ?? String(e);
          }}

          return out;
        }})()
        "#,
        secret = js_path(&secret),
    );

    let out = eval_async_json(&tokio, &mut rt, &source)?;

    // (2a) the allowed fetch passed the gate and actually completed.
    assert!(
        out.get("allowedError").is_some_and(Value::is_null),
        "allowed fetch should succeed, got error: {:?}",
        out.get("allowedError"),
    );
    assert_eq!(
        out.get("allowedStatus").and_then(Value::as_u64),
        Some(200),
        "allowed fetch should return HTTP 200: {out}",
    );
    assert_eq!(out.get("allowedBody").and_then(Value::as_str), Some("ok"));

    // (2b) the denied fetch rejected with a deno permission error (NotCapable), naming
    // net access to the denied host — not a network/DNS error.
    assert_eq!(
        name(&out, "deniedName"),
        "NotCapable",
        "denied fetch should be a permission error: {out}",
    );
    let denied_msg = name(&out, "deniedMessage");
    assert!(
        denied_msg.contains("net access"),
        "denied fetch should be a net-permission error: {denied_msg}",
    );
    assert!(
        denied_msg.contains("denied.example"),
        "denied fetch message should name the host: {denied_msg}",
    );

    // (3) the out-of-scope read rejected with a deno permission error naming read access.
    assert_eq!(
        name(&out, "readName"),
        "NotCapable",
        "read should be a permission error: {out}",
    );
    assert!(
        name(&out, "readMessage").contains("read access"),
        "read should be a read-permission error: {}",
        name(&out, "readMessage"),
    );

    Ok(())
}

/// A `host:port` grant authorizes **only that port**. Grant `127.0.0.1:<allowed>`; the
/// same host on a *different* port is denied at the gate (proving the port is enforced,
/// not just the host).
#[test]
fn allow_net_grant_is_scoped_to_host_and_port() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (allowed_port, _server) = spawn_http_200_server()?;
    let denied_port = unused_port()?;

    let opts = PermissionsOptions {
        allow_net: Some(vec![format!("127.0.0.1:{allowed_port}")]),
        prompt: false,
        ..Default::default()
    };
    let (tokio, mut rt) = restricted_runtime(temp.path(), &opts)?;

    let source = format!(
        r#"
        (async () => {{
          const out = {{}};
          try {{
            const res = await fetch("http://127.0.0.1:{allowed_port}/");
            out.allowedStatus = res.status;
            await res.text();
            out.allowedError = null;
          }} catch (e) {{
            out.allowedStatus = 0;
            out.allowedError = (e?.name ?? "") + ": " + (e?.message ?? String(e));
          }}
          // Same host, different (un-granted) port — must be denied, not connection-refused.
          try {{
            await fetch("http://127.0.0.1:{denied_port}/");
            out.deniedName = "NO_ERROR";
            out.deniedMessage = "";
          }} catch (e) {{
            out.deniedName = e?.name ?? String(e);
            out.deniedMessage = e?.message ?? String(e);
          }}
          return out;
        }})()
        "#
    );

    let out = eval_async_json(&tokio, &mut rt, &source)?;

    assert!(
        out.get("allowedError").is_some_and(Value::is_null),
        "allowed port should succeed, got: {:?}",
        out.get("allowedError"),
    );
    assert_eq!(out.get("allowedStatus").and_then(Value::as_u64), Some(200));

    assert_eq!(
        name(&out, "deniedName"),
        "NotCapable",
        "a different port on the granted host must be a permission error, not a connection \
         error: {out}",
    );
    let denied_msg = name(&out, "deniedMessage");
    assert!(
        denied_msg.contains("net access") && denied_msg.contains(&denied_port.to_string()),
        "denied-port message should be a net-permission error naming the port {denied_port}: \
         {denied_msg}",
    );

    Ok(())
}

/// A directory grant authorizes its whole **subtree**, not a single path. Grant
/// `<temp>/granted`; a file nested under it reads successfully, while a sibling outside
/// it is denied.
#[test]
fn allow_read_grant_covers_subtree_not_siblings() -> Result<()> {
    let temp = tempfile::tempdir()?;

    let granted_dir = temp.path().join("granted");
    let nested_dir = granted_dir.join("nested");
    std::fs::create_dir_all(&nested_dir)?;
    let nested_file = nested_dir.join("deep.txt");
    std::fs::write(&nested_file, "deep-ok")?;

    // A sibling of the granted dir — outside the granted subtree.
    let sibling = temp.path().join("secret.txt");
    std::fs::write(&sibling, "secret")?;

    let opts = PermissionsOptions {
        allow_read: Some(vec![granted_dir.to_string_lossy().into_owned()]),
        prompt: false,
        ..Default::default()
    };
    let (tokio, mut rt) = restricted_runtime(temp.path(), &opts)?;

    let source = format!(
        r#"
        (async () => {{
          const out = {{}};
          // Nested under the granted directory — the subtree grant should cover it.
          try {{
            out.nested = await Deno.readTextFile("{nested}");
            out.nestedError = null;
          }} catch (e) {{
            out.nested = null;
            out.nestedError = (e?.name ?? "") + ": " + (e?.message ?? String(e));
          }}
          // Sibling outside the granted subtree — must be denied.
          try {{
            await Deno.readTextFile("{sibling}");
            out.siblingName = "NO_ERROR";
            out.siblingMessage = "";
          }} catch (e) {{
            out.siblingName = e?.name ?? String(e);
            out.siblingMessage = e?.message ?? String(e);
          }}
          return out;
        }})()
        "#,
        nested = js_path(&nested_file),
        sibling = js_path(&sibling),
    );

    let out = eval_async_json(&tokio, &mut rt, &source)?;

    // A directory grant covers nested files (subtree semantics).
    assert!(
        out.get("nestedError").is_some_and(Value::is_null),
        "a file nested under the granted dir should be readable, got: {:?}",
        out.get("nestedError"),
    );
    assert_eq!(out.get("nested").and_then(Value::as_str), Some("deep-ok"));

    // A sibling outside the granted subtree is denied.
    assert_eq!(
        name(&out, "siblingName"),
        "NotCapable",
        "a sibling outside the granted subtree must be denied: {out}",
    );
    assert!(
        name(&out, "siblingMessage").contains("read access"),
        "sibling denial should be a read-permission error: {}",
        name(&out, "siblingMessage"),
    );

    Ok(())
}
