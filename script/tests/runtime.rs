use std::path::Path;
use std::rc::Rc;

use anyhow::Result;
use deno_core::{serde_v8, FastString, ModuleSpecifier, PollEventLoopOptions};
use smudgy_script::{ImportPolicy, InspectorConfig, ModulePolicy, ScriptRuntime, ScriptRuntimeOptions};

fn tokio_runtime() -> Rc<tokio::runtime::Runtime> {
    Rc::new(
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap(),
    )
}

fn script_runtime(data_dir: &Path) -> Result<(Rc<tokio::runtime::Runtime>, ScriptRuntime)> {
    let tokio = tokio_runtime();
    let runtime = ScriptRuntime::new(ScriptRuntimeOptions {
        extensions: Vec::new(),
        data_dir: data_dir.to_path_buf(),
        webstorage_dir: None,
        module_policy: ModulePolicy { allow_https: true, import_policy: ImportPolicy::Any },
        inspector: None,
        tokio: tokio.clone(),
        package_provider: None,
        permissions: None,
    })?;
    Ok((tokio, runtime))
}

fn inspector_script_runtime(
    data_dir: &Path,
) -> Result<(Rc<tokio::runtime::Runtime>, ScriptRuntime)> {
    let tokio = tokio_runtime();
    let runtime = ScriptRuntime::new(ScriptRuntimeOptions {
        extensions: Vec::new(),
        data_dir: data_dir.to_path_buf(),
        webstorage_dir: None,
        module_policy: ModulePolicy { allow_https: true, import_policy: ImportPolicy::Any },
        inspector: Some(InspectorConfig {
            address: "127.0.0.1:0".parse().unwrap(),
        }),
        tokio: tokio.clone(),
        package_provider: None,
        permissions: None,
    })?;
    Ok((tokio, runtime))
}

fn eval_bool(rt: &mut ScriptRuntime, source: &str) -> Result<bool> {
    let value = rt
        .deno_runtime()
        .execute_script("<test>", FastString::from(source.to_string()))?;
    deno_core::scope!(scope, rt.deno_runtime());
    let local = deno_core::v8::Local::new(scope, value);
    Ok(serde_v8::from_v8(scope, local)?)
}

fn eval_async_bool(
    tokio: &tokio::runtime::Runtime,
    rt: &mut ScriptRuntime,
    source: &str,
) -> Result<bool> {
    tokio.block_on(async {
        let value = rt
            .deno_runtime()
            .execute_script("<test>", FastString::from(source.to_string()))?;
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

fn eval_module_bool(
    tokio: &tokio::runtime::Runtime,
    rt: &mut ScriptRuntime,
    specifier: &ModuleSpecifier,
) -> Result<bool> {
    tokio.block_on(async {
        let module_id = rt.deno_runtime().load_main_es_module(specifier).await?;
        let receiver = rt.deno_runtime().mod_evaluate(module_id);
        rt.deno_runtime()
            .run_event_loop(PollEventLoopOptions::default())
            .await?;
        receiver.await?;

        let namespace = rt.deno_runtime().get_module_namespace(module_id)?;
        deno_core::scope!(scope, rt.deno_runtime());
        let namespace = namespace.open(scope);
        let key = deno_core::v8::String::new(scope, "ok").unwrap();
        let value = namespace.get(scope, key.into()).unwrap();
        Ok(serde_v8::from_v8(scope, value)?)
    })
}

#[test]
fn basic_eval_load_smoke() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (tokio, mut rt) = script_runtime(temp.path())?;
    assert!(eval_bool(&mut rt, "globalThis.__smoke = 2 + 2 === 4; __smoke")?);

    let module_path = temp.path().join("smoke.js");
    std::fs::write(&module_path, "export const ok = 40 + 2 === 42;")?;
    let specifier = ModuleSpecifier::from_file_path(module_path).unwrap();
    assert!(eval_module_bool(&tokio, &mut rt, &specifier)?);
    Ok(())
}

#[test]
fn inspector_attach_first_does_not_block_execution() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (tokio, mut rt) = inspector_script_runtime(temp.path())?;
    let address = rt.inspector_address().unwrap();
    assert_eq!(address.ip().to_string(), "127.0.0.1");
    assert_ne!(address.port(), 0);

    assert!(eval_bool(&mut rt, "1 + 1 === 2")?);

    let module_path = temp.path().join("inspector.js");
    std::fs::write(
        &module_path,
        "export const ok = await Promise.resolve(true);",
    )?;
    let specifier = ModuleSpecifier::from_file_path(module_path).unwrap();
    assert!(eval_module_bool(&tokio, &mut rt, &specifier)?);
    Ok(())
}

#[test]
fn web_platform() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (tokio, mut rt) = script_runtime(temp.path())?;
    let source = r#"
      (async () => {
        const timer = await new Promise((resolve) => setTimeout(() => resolve(7), 1));
        const encoded = new TextEncoder().encode("abc");
        const cloned = structuredClone({ value: 9 });
        const url = new URL("/x", "https://example.com/base");
        const digest = await crypto.subtle.digest("SHA-256", encoded);
        const hex = Array.from(new Uint8Array(digest)).map((b) => b.toString(16).padStart(2, "0")).join("");
        let microtask = false;
        queueMicrotask(() => microtask = true);
        await Promise.resolve();
        return timer === 7
          && encoded.length === 3
          && cloned.value === 9
          && url.href === "https://example.com/x"
          && hex === "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
          && atob(btoa("smudgy")) === "smudgy"
          && microtask;
      })()
    "#;
    assert!(eval_async_bool(&tokio, &mut rt, source)?);
    Ok(())
}

#[test]
fn typescript_transpile() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (tokio, mut rt) = script_runtime(temp.path())?;
    let module_path = temp.path().join("mod.ts");
    std::fs::write(
        &module_path,
        "const value: number = 41; export const ok: boolean = value + 1 === 42;",
    )?;
    let specifier = ModuleSpecifier::from_file_path(module_path).unwrap();
    assert!(eval_module_bool(&tokio, &mut rt, &specifier)?);
    Ok(())
}

#[test]
fn fs_read_write_cwd() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (tokio, mut rt) = script_runtime(temp.path())?;
    let file = temp.path().join("fs.txt").to_string_lossy().replace('\\', "\\\\");
    let source = format!(
        r#"
        (async () => {{
          await Deno.writeTextFile("{file}", "hello");
          const text = await Deno.readTextFile("{file}");
          return text === "hello" && typeof Deno.cwd() === "string" && Deno.cwd().length > 0;
        }})()
        "#
    );
    assert!(eval_async_bool(&tokio, &mut rt, &source)?);
    Ok(())
}

#[test]
fn net_tcp_echo() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (tokio, mut rt) = script_runtime(temp.path())?;
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let addr = listener.local_addr()?;
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = [0_u8; 4];
        std::io::Read::read_exact(&mut stream, &mut buf).unwrap();
        std::io::Write::write_all(&mut stream, &buf).unwrap();
    });

    let source = format!(
        r#"
        (async () => {{
          const conn = await Deno.connect({{ hostname: "127.0.0.1", port: {} }});
          await conn.write(new TextEncoder().encode("ping"));
          const chunk = new Uint8Array(4);
          const n = await conn.read(chunk);
          conn.close();
          return n === 4 && new TextDecoder().decode(chunk) === "ping";
        }})()
        "#,
        addr.port()
    );
    assert!(eval_async_bool(&tokio, &mut rt, &source)?);
    Ok(())
}

// Regression for the rustls "no process-level CryptoProvider available" panic:
// Deno.connectTls -> deno_tls::create_client_config requires an installed provider.
// example.com:443 is a stable, publicly-trusted TLS endpoint. Network-gated.
#[ignore = "requires network"]
#[test]
fn tls_connect_handshake() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (tokio, mut rt) = script_runtime(temp.path())?;
    let source = r#"
        (async () => {
          const conn = await Deno.connectTls({ hostname: "example.com", port: 443 });
          conn.close();
          return true;
        })()
        "#;
    assert!(eval_async_bool(&tokio, &mut rt, source)?);
    Ok(())
}

// Regression for the "there is no reactor running" abort: JS that opens a TLS
// connection and never closes it leaves an Open `TlsStream` in the resource table,
// and rustls-tokio-stream's Drop spawns a graceful-shutdown task. Dropping the
// ScriptRuntime outside any `block_on` — exactly what `ScriptEngine::new` does with
// a package isolate whose load failed after a top-level `connectTls` — panicked the
// session thread until `Drop for ScriptRuntime` entered its own runtime. Network-gated.
#[ignore = "requires network"]
#[test]
fn drop_with_open_tls_connection_outside_runtime_context() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (tokio, mut rt) = script_runtime(temp.path())?;
    let source = r#"
        (async () => {
          globalThis.conn = await Deno.connectTls({ hostname: "example.com", port: 443 });
          return true;
        })()
        "#;
    assert!(eval_async_bool(&tokio, &mut rt, source)?);
    // The connection is still open; drop the runtime on this thread with no tokio
    // context entered. Must not panic.
    drop(rt);
    Ok(())
}

#[test]
fn webstorage_local_storage() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (_tokio, mut rt) = script_runtime(temp.path())?;
    assert!(eval_bool(
        &mut rt,
        r#"
        localStorage.setItem("smudgy", "ok");
        localStorage.getItem("smudgy") === "ok"
        "#,
    )?);
    Ok(())
}

#[test]
fn node_builtins() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (tokio, mut rt) = script_runtime(temp.path())?;
    let module_path = temp.path().join("node.js");
    std::fs::write(
        &module_path,
        r#"
        import { EventEmitter } from "node:events";
        import { createHash } from "node:crypto";
        import { DatabaseSync } from "node:sqlite";
        const events = new EventEmitter();
        let seen = "";
        events.on("value", (value) => seen = value);
        events.emit("value", "abc");
        const hash = createHash("sha3-512").update("abc").digest("base64url");
        const db = new DatabaseSync(":memory:");
        db.exec("CREATE TABLE kills (mob TEXT, count INTEGER)");
        db.prepare("INSERT INTO kills VALUES (?, ?)").run("goblin", 3);
        const row = db.prepare("SELECT count FROM kills WHERE mob = ?").get("goblin");
        db.close();
        export const ok = seen === "abc"
          && hash === "t1GFCxpXFopWk82SS2sJbgj2IYJ0RPcNiE9dAkDScS4Q4RbpGSrzyRp-xXZH45NAVzQLTPQI1aVlkvgnTuxT8A"
          && row.count === 3;
        "#,
    )?;
    let specifier = ModuleSpecifier::from_file_path(module_path).unwrap();
    assert!(eval_module_bool(&tokio, &mut rt, &specifier)?);
    Ok(())
}

#[ignore = "requires network"]
#[test]
fn jsr_import() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (tokio, mut rt) = script_runtime(temp.path())?;
    let module_path = temp.path().join("jsr.js");
    std::fs::write(
        &module_path,
        r#"
        import { encodeBase64 } from "jsr:@std/encoding@1/base64";
        export const ok = encodeBase64(new TextEncoder().encode("hello")) === "aGVsbG8=";
        "#,
    )?;
    let specifier = ModuleSpecifier::from_file_path(module_path).unwrap();
    assert!(eval_module_bool(&tokio, &mut rt, &specifier)?);
    Ok(())
}

#[ignore = "requires network"]
#[test]
fn npm_import() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (tokio, mut rt) = script_runtime(temp.path())?;
    // Use a real file:// module so the `npm:` specifier resolves against a valid
    // base referrer; a dynamic import() from a "<test>" script has no base URL.
    let module_path = temp.path().join("npm_test.js");
    std::fs::write(
        &module_path,
        "import ms from \"npm:ms@2.1.3\";\nexport const ok = ms(\"2h\") === 7200000;\n",
    )?;
    let specifier = ModuleSpecifier::from_file_path(module_path).unwrap();
    assert!(eval_module_bool(&tokio, &mut rt, &specifier)?);
    Ok(())
}

/// An npm package with a real dependency tree (the scriptref start page's npm
/// example). Guards the two halves of npm interop beyond dep-free `ms` above:
///
/// * transitive dependencies at require time — discord.js `require()`s
///   `@discordjs/util` etc. out of the global npm cache, which only works while
///   `has_node_modules_dir` stays false (see lib.rs) so deno_node keeps its
///   global-cache lookup;
/// * CJS named-export interop — `import { Client }` from a CommonJS package
///   needs the deno_ast export analysis wired in npm_resolver.rs (the default
///   import works even without it).
#[ignore = "requires network"]
#[test]
fn npm_discord_import() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (tokio, mut rt) = script_runtime(temp.path())?;
    let module_path = temp.path().join("discord_test.js");
    std::fs::write(
        &module_path,
        "import discord, { Client, GatewayIntentBits } from \"npm:discord.js\";\n\
         const client = new Client({ intents: [GatewayIntentBits.Guilds] });\n\
         export const ok = typeof client.login === \"function\"\n\
             && discord.Client === Client\n\
             && new discord.Client({ intents: [discord.GatewayIntentBits.Guilds] }) instanceof Client;\n",
    )?;
    let specifier = ModuleSpecifier::from_file_path(module_path).unwrap();
    assert!(eval_module_bool(&tokio, &mut rt, &specifier)?);
    Ok(())
}
