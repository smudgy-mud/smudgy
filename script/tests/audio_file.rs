#![cfg(feature = "web-audio")]

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use deno_core::{ModuleSpecifier, OpState};
use smudgy_script::audio_file::AuthorizedAudioFile;
use smudgy_script::{
    Permissions, PermissionsContainer, PermissionsOptions, permission_descriptor_parser,
};

fn fixture() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let canonical = std::fs::canonicalize(dir.path()).unwrap();
    // Match the permission descriptor spelling on Windows and macOS runners.
    let text = canonical.to_string_lossy();
    let path = PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(&text));
    (dir, path)
}

fn state(allowed: Option<&Path>) -> OpState {
    let parser = permission_descriptor_parser();
    let permissions = Permissions::from_options(
        &*parser,
        &PermissionsOptions {
            allow_read: allowed.map(|path| vec![path.to_str().unwrap().to_owned()]),
            prompt: false,
            ..Default::default()
        },
    )
    .unwrap();
    let mut state = OpState::new(None);
    state.put(PermissionsContainer::new(parser, permissions));
    state
}

#[test]
fn missing_or_denied_isolate_authority_has_no_allow_all_fallback() {
    let (_dir, root) = fixture();
    let path = root.join("cue.wav");
    std::fs::write(&path, b"sample").unwrap();
    assert!(AuthorizedAudioFile::from_path(&OpState::new(None), &path).is_err());
    let denied = state(None);
    assert!(AuthorizedAudioFile::from_path(&denied, &path).is_err());
    assert!(AuthorizedAudioFile::from_path(&denied, &root.join("missing.wav")).is_err());
}

#[test]
fn script_file_factory_uses_runtime_read_grants_and_rejects_urls() {
    use smudgy_script::{ModulePolicy, ScriptRuntime, ScriptRuntimeOptions, WorkerMode};
    let (_dir, root) = fixture();
    let allowed = root.join("allowed");
    std::fs::create_dir(&allowed).unwrap();
    // One PCM frame is enough to exercise successful decode and EOF.
    let mut wav = Vec::from(&b"RIFF\x26\x00\x00\x00WAVEfmt \x10\x00\x00\x00\x01\x00\x01\x00\x80\xbb\x00\x00\x00\x77\x01\x00\x02\x00\x10\x00data\x02\x00\x00\x00"[..]);
    wav.extend_from_slice(&8192_i16.to_le_bytes());
    let cue = allowed.join("cue with space.wav");
    let denied = root.join("denied.wav");
    std::fs::write(&cue, &wav).unwrap();
    std::fs::write(&denied, &wav).unwrap();
    let permissions = state(Some(&allowed))
        .borrow::<PermissionsContainer>()
        .clone();
    let tokio = std::rc::Rc::new(
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap(),
    );
    let host = std::sync::Arc::new(deno_audio::AudioHost::default());
    let mut audio = deno_audio::deno_audio::init(
        deno_audio::AudioExtensionOptions::new(host.clone())
            .output_factory(std::sync::Arc::new(deno_audio::SilentAudioOutput::new())),
    );
    smudgy_script::prepare_deferred_web_audio_extension(&mut audio);
    let mut runtime = ScriptRuntime::new(ScriptRuntimeOptions {
        extensions: vec![audio],
        data_dir: root,
        webstorage_dir: None,
        module_policy: ModulePolicy::default(),
        inspector: None,
        tokio: tokio.clone(),
        package_provider: None,
        permissions: Some(permissions),
        broadcast_channel: None,
        workers: WorkerMode::Disabled,
        max_live_workers_override: None,
    })
    .unwrap();
    let cue_url = ModuleSpecifier::from_file_path(&cue).unwrap();
    let sources = serde_json::to_string(&[
        denied.to_str().unwrap(),
        "https://example.invalid/cue.wav",
        "data:audio/wav;base64,AA==",
    ])
    .unwrap();
    let script = format!(
        r#"(async () => {{
        const {{ createFileSource, Audio }} = await import("smudgy:media");
        const c = new AudioContext({{sinkId: "none"}});
        for (const path of {sources}) {{
            let refused = false;
            try {{ await createFileSource(c, path); }} catch {{ refused = true; }}
            if (!refused) throw new Error(`source accepted: ${{path}}`);
        }}
        const source = await createFileSource(c, new URL({url}));
        source.stop(); await c.close();
        for (const path of {sources}) {{
            const audio = new Audio(path);
            let refused = false;
            try {{ await audio.play(); }} catch {{ refused = true; }}
            await audio.close();
            if (!refused) throw new Error(`player source accepted: ${{path}}`);
        }}
        const audio = new Audio(new URL({url}));
        const ended = new Promise(resolve => {{ audio.onended = resolve; }});
        await audio.play(); await ended;
        // Every replay reauthorizes the file. A previous preload/play does not
        // grant permission after the isolate revokes its read access.
        Deno.permissions.revokeSync({{ name: 'read' }});
        let refused = false;
        try {{ await audio.play(); }} catch {{ refused = true; }}
        await audio.close();
        if (!refused) throw new Error('player replay ignored revoked read permission');
    }})()"#,
        url = serde_json::to_string(cue_url.as_str()).unwrap()
    );
    tokio.block_on(async {
        let value = runtime
            .deno_runtime()
            .execute_script("<audio-files>", script)
            .unwrap();
        let promise = runtime.deno_runtime().resolve(value);
        tokio::time::timeout(
            std::time::Duration::from_secs(15),
            runtime
                .deno_runtime()
                .with_event_loop_future(promise, deno_core::PollEventLoopOptions::default()),
        )
        .await
        .unwrap()
        .unwrap();
    });
    assert_eq!(host.usage().streaming_jobs(), 0);
}

#[test]
fn media_import_cannot_enable_audio_in_a_disabled_runtime() {
    use smudgy_script::{ModulePolicy, ScriptRuntime, ScriptRuntimeOptions, WorkerMode};
    let (_dir, root) = fixture();
    let tokio = std::rc::Rc::new(
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap(),
    );
    let mut runtime = ScriptRuntime::new(ScriptRuntimeOptions {
        extensions: Vec::new(),
        data_dir: root,
        webstorage_dir: None,
        module_policy: ModulePolicy::default(),
        inspector: None,
        tokio: tokio.clone(),
        package_provider: None,
        permissions: Some(state(None).borrow::<PermissionsContainer>().clone()),
        broadcast_channel: None,
        workers: WorkerMode::Disabled,
        max_live_workers_override: None,
    })
    .unwrap();
    tokio.block_on(async {
        let value = runtime.deno_runtime().execute_script("<no-audio>", r#"(async () => {
            for (const specifier of ['smudgy:media', 'smudgy-media:main']) {
                let denied = false;
                try { await import(specifier); } catch { denied = true; }
                if (!denied) throw new Error('media import enabled audio');
            }
            if ('AudioContext' in globalThis || 'AudioFileSourceNode' in globalThis || 'Audio' in globalThis) throw new Error('audio global leaked');
        })()"#).unwrap();
        let promise = runtime.deno_runtime().resolve(value);
        runtime.deno_runtime().with_event_loop_future(promise, deno_core::PollEventLoopOptions::default()).await.unwrap();
    });
}

#[test]
fn opener_is_deferred_read_only_and_uses_the_captured_permission_container() {
    let (_dir, root) = fixture();
    let path = root.join("cue.wav");
    std::fs::write(&path, b"first").unwrap();
    let isolate = state(Some(&root));
    let opener = AuthorizedAudioFile::from_path(&isolate, &path)
        .unwrap()
        .into_opener();
    // Preparation retained neither a stream descriptor nor an encoded snapshot.
    std::fs::write(&path, b"later").unwrap();
    drop(isolate);
    let mut file = std::thread::spawn(opener).join().unwrap().unwrap();
    let mut contents = String::new();
    file.read_to_string(&mut contents).unwrap();
    assert_eq!(contents, "later");
    assert!(file.write_all(b"must not write").is_err());
    drop(file);
    let isolate = state(Some(&root));
    let opener = AuthorizedAudioFile::from_path(&isolate, &path)
        .unwrap()
        .into_opener();
    std::fs::remove_file(path).unwrap();
    assert_eq!(opener().unwrap_err().kind(), std::io::ErrorKind::NotFound);
}

#[test]
fn queued_source_observes_revocation_before_open() {
    let (_dir, root) = fixture();
    let path = root.join("speech.wav");
    std::fs::write(&path, b"speech").unwrap();
    let isolate = state(Some(&root));
    let opener = AuthorizedAudioFile::from_path(&isolate, &path)
        .unwrap()
        .into_opener();
    isolate
        .borrow::<PermissionsContainer>()
        .revoke_read(None)
        .unwrap();
    assert_eq!(
        std::thread::spawn(opener)
            .join()
            .unwrap()
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::PermissionDenied
    );
    assert!(AuthorizedAudioFile::from_path(&isolate, &path).is_err());
}

#[test]
fn package_read_grants_do_not_cross_isolates_or_parent_traversal() {
    let (_dir, root) = fixture();
    let first = root.join("first");
    let second = root.join("second");
    std::fs::create_dir_all(&first).unwrap();
    std::fs::create_dir_all(&second).unwrap();
    let cue = first.join("cue.wav");
    std::fs::write(&cue, b"cue").unwrap();
    assert!(AuthorizedAudioFile::from_path(&state(Some(&first)), &cue).is_ok());
    assert!(AuthorizedAudioFile::from_path(&state(Some(&second)), &cue).is_err());
    assert!(
        AuthorizedAudioFile::from_path(&state(Some(&second)), &second.join("../first/cue.wav"))
            .is_err()
    );
}

#[test]
fn local_file_urls_preserve_escaping_and_reject_other_source_kinds() {
    let (_dir, root) = fixture();
    let path = root.join("cue with space #1.wav");
    std::fs::write(&path, b"cue").unwrap();
    let isolate = state(Some(&root));
    let url = ModuleSpecifier::from_file_path(&path).unwrap();
    assert!(
        AuthorizedAudioFile::from_file_url(&isolate, &url)
            .unwrap()
            .into_opener()()
        .is_ok()
    );
    let mut query = url.clone();
    query.set_query(Some("ignored"));
    let mut fragment = url;
    fragment.set_fragment(Some("ignored"));
    for invalid in [
        query,
        fragment,
        ModuleSpecifier::parse("https://example.invalid/cue.wav").unwrap(),
        ModuleSpecifier::parse("file://remote.invalid/share/cue.wav").unwrap(),
        ModuleSpecifier::parse("data:audio/wav;base64,AAAA").unwrap(),
        ModuleSpecifier::parse("smudgy://author/package/cue.wav").unwrap(),
    ] {
        assert!(AuthorizedAudioFile::from_file_url(&isolate, &invalid).is_err());
    }
}

#[test]
fn directories_are_rejected_before_decoding() {
    let (_dir, root) = fixture();
    let opener = AuthorizedAudioFile::from_path(&state(Some(&root)), &root)
        .unwrap()
        .into_opener();
    assert_eq!(
        opener().unwrap_err().kind(),
        std::io::ErrorKind::InvalidInput
    );
}

#[cfg(windows)]
#[test]
fn windows_pipe_and_device_namespaces_are_rejected_even_with_read_grants() {
    let mut isolate = OpState::new(None);
    isolate.put(PermissionsContainer::allow_all(
        permission_descriptor_parser(),
    ));
    for path in [
        r"\\.\pipe\smudgy-audio",
        r"\\?\pipe\smudgy-audio",
        r"\\.\NUL",
        r"\\server\share\cue.wav",
    ] {
        assert!(AuthorizedAudioFile::from_path(&isolate, Path::new(path)).is_err());
    }
}

#[cfg(unix)]
#[test]
fn fifo_is_rejected_without_waiting_for_a_writer() {
    use std::os::unix::ffi::OsStrExt;
    let (_dir, root) = fixture();
    let path = root.join("pipe");
    let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: name is a live NUL-terminated path; mkfifo retains no pointer.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    let opener = AuthorizedAudioFile::from_path(&state(Some(&root)), &path)
        .unwrap()
        .into_opener();
    assert_eq!(
        opener().unwrap_err().kind(),
        std::io::ErrorKind::InvalidInput
    );
}

#[cfg(unix)]
#[test]
fn checked_symlink_target_cannot_escape_the_read_grant() {
    let (_dir, root) = fixture();
    let allowed = root.join("allowed");
    std::fs::create_dir(&allowed).unwrap();
    let cue = allowed.join("cue.wav");
    let secret = root.join("denied.wav");
    std::fs::write(&cue, b"cue").unwrap();
    std::fs::write(&secret, b"denied").unwrap();
    let link = allowed.join("link.wav");
    use std::os::unix::fs::symlink as create_link;
    create_link(&secret, &link).unwrap();
    let isolate = state(Some(&allowed));
    assert!(AuthorizedAudioFile::from_path(&isolate, &link).is_err());
    std::fs::remove_file(&link).unwrap();
    create_link(&cue, &link).unwrap();
    let opener = AuthorizedAudioFile::from_path(&isolate, &link)
        .unwrap()
        .into_opener();
    std::fs::remove_file(&link).unwrap();
    create_link(&secret, &link).unwrap();
    let mut contents = String::new();
    opener().unwrap().read_to_string(&mut contents).unwrap();
    assert_eq!(contents, "cue");

    // Replacing the retained canonical target itself must be checked again.
    let opener = AuthorizedAudioFile::from_path(&isolate, &cue)
        .unwrap()
        .into_opener();
    std::fs::remove_file(&cue).unwrap();
    create_link(&secret, &cue).unwrap();
    assert_eq!(
        opener().unwrap_err().kind(),
        std::io::ErrorKind::PermissionDenied
    );
}

#[cfg(windows)]
#[test]
fn checked_junction_target_cannot_escape_the_read_grant() {
    use deno_fs::{FileSystem, FsFileType, RealFs};
    use smudgy_script::OpenAccessKind;
    use std::borrow::Cow;
    let create_junction = |target: &Path, link: &Path| {
        let permissions = PermissionsContainer::allow_all(permission_descriptor_parser());
        let target = permissions
            .check_open(Cow::Borrowed(target), OpenAccessKind::Read, None)
            .unwrap();
        let link = permissions
            .check_open(Cow::Borrowed(link), OpenAccessKind::WriteNoFollow, None)
            .unwrap();
        RealFs
            .symlink_sync(&target, &link, Some(FsFileType::Junction))
            .unwrap();
    };
    let (_dir, root) = fixture();
    let allowed = root.join("allowed");
    let data = allowed.join("data");
    let outside = root.join("outside");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(data.join("cue.wav"), b"cue").unwrap();
    std::fs::write(outside.join("cue.wav"), b"denied").unwrap();
    let link = allowed.join("alias");
    create_junction(&outside, &link);
    let isolate = state(Some(&allowed));
    assert!(AuthorizedAudioFile::from_path(&isolate, &link.join("cue.wav")).is_err());
    std::fs::remove_dir(&link).unwrap();
    create_junction(&data, &link);
    let opener = AuthorizedAudioFile::from_path(&isolate, &link.join("cue.wav"))
        .unwrap()
        .into_opener();
    std::fs::remove_dir(&link).unwrap();
    create_junction(&outside, &link);
    let mut contents = String::new();
    opener().unwrap().read_to_string(&mut contents).unwrap();
    assert_eq!(contents, "cue");
    let opener = AuthorizedAudioFile::from_path(&isolate, &data.join("cue.wav"))
        .unwrap()
        .into_opener();
    std::fs::remove_file(data.join("cue.wav")).unwrap();
    std::fs::remove_dir(&data).unwrap();
    create_junction(&outside, &data);
    assert_eq!(
        opener().unwrap_err().kind(),
        std::io::ErrorKind::PermissionDenied
    );
    std::fs::remove_dir(&data).unwrap();
    std::fs::remove_dir(&link).unwrap();
}

#[test]
fn real_runtime_permission_revocation_reaches_the_queued_native_opener() {
    use smudgy_script::{ModulePolicy, ScriptRuntime, ScriptRuntimeOptions, WorkerMode};
    let (_dir, root) = fixture();
    let cue = root.join("cue.wav");
    std::fs::write(&cue, b"cue").unwrap();
    let permissions = state(Some(&root)).borrow::<PermissionsContainer>().clone();
    let tokio = std::rc::Rc::new(
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap(),
    );
    let mut runtime = ScriptRuntime::new(ScriptRuntimeOptions {
        extensions: Vec::new(),
        data_dir: root,
        webstorage_dir: None,
        module_policy: ModulePolicy::default(),
        inspector: None,
        tokio,
        package_provider: None,
        permissions: Some(permissions),
        broadcast_channel: None,
        workers: WorkerMode::Disabled,
        max_live_workers_override: None,
    })
    .unwrap();
    let state = runtime.deno_runtime().op_state();
    let opener = AuthorizedAudioFile::from_path(&state.borrow(), &cue)
        .unwrap()
        .into_opener();
    runtime
        .deno_runtime()
        .execute_script(
            "revoke_audio_file.js",
            "Deno.permissions.revokeSync({ name: 'read' });",
        )
        .unwrap();
    assert_eq!(
        std::thread::spawn(opener)
            .join()
            .unwrap()
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::PermissionDenied
    );
    assert!(AuthorizedAudioFile::from_path(&state.borrow(), &cue).is_err());
}
