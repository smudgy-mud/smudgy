//! Local media for kitty graphics transmits: `t=f` (a file), `t=t` (a
//! temporary file, deleted once read) and `t=s` (a POSIX shared-memory
//! object). The connection decides who may name them at all (a loopback peer
//! only, see `ChannelGraphicsHost`); this module decides which names may be
//! read and reads them, on the socket task, synchronously, as the protocol's
//! host contract asks.
//!
//! The rules, following the specification and kitty:
//! - A file name is an absolute UTF-8 path. It is resolved, symlinks
//!   followed, before anything is opened, and refused unless it names a
//!   regular file outside `/proc`, `/sys` and `/dev` (`/dev/shm`, a tmpfs of
//!   ordinary files the specification lists as a temporary directory,
//!   excepted); on Windows, unless it is a plain drive path — never a
//!   device, a pipe or a UNC share, though a network share mapped to a drive
//!   letter is a drive path like any other, as the standard library has no
//!   way to tell a remote drive (`DRIVE_REMOTE`) from a local one. The file
//!   is then opened (non-blocking on unix, so
//!   a FIFO swapped in meanwhile cannot hang the socket task, and never as a
//!   controlling terminal) and checked again: it must still be that regular
//!   file, and on Linux the kernel's own path for the open descriptor
//!   (`/proc/self/fd`) must still be the resolved one and outside the
//!   sensitive areas — nothing after the resolution refuses to follow a
//!   symlink, so a directory on the way that the peer can write could be
//!   swapped for one between the checks and the open.
//! - `size` bytes are read from `offset`, `size` zero meaning to the end. A
//!   file shorter than that, an empty read and anything over
//!   [`MAX_IMAGE_BYTES`] fail.
//! - A temporary file is deleted after a successful read, and only when its
//!   resolved path contains `tty-graphics-protocol` and lies under `/tmp`,
//!   `/dev/shm`, `$TMPDIR` or the platform's temporary directory, and only
//!   while that path still leads, with no symlink on the way, to the file
//!   that was read. Anything else stays where it is.
//! - Shared memory is read on Linux from `/dev/shm/<name>` (one leading `/`
//!   dropped, no other `/`, no symlink: what glibc's `shm_open` accepts) and
//!   unlinked once opened, as the specification asks. Elsewhere it fails:
//!   other unix systems need `shm_open`, which needs a `libc` dependency this
//!   crate does not have, and Windows' named mappings need `windows-sys`.
//!
//! Every failure is the one [`GraphicsError::bad_file`] reply, so a peer
//! learns nothing about files it could not read itself; the real reason goes
//! to the debug log, which only the local user sees.

use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};

use smudgy_protocol::graphics::{GraphicsError, MAX_IMAGE_BYTES, Medium};

/// What a temporary file's resolved path must contain to be deleted.
const TEMP_FILE_TOKEN: &str = "tty-graphics-protocol";

/// The longest name accepted, in bytes: `PATH_MAX` on Linux.
const MAX_NAME_BYTES: usize = 4096;

/// Why a read was refused or failed. Only ever logged.
type Refusal = String;

/// Read `size` bytes (zero: to the end) from `offset` of the file or
/// shared-memory object `name` names, under the rules in the module docs.
///
/// # Errors
/// [`GraphicsError::bad_file`] for every refusal and failure, with the
/// reason logged at debug level.
pub fn read_local_media(
    medium: Medium,
    name: &[u8],
    offset: u32,
    size: u32,
) -> Result<Vec<u8>, GraphicsError> {
    let result = match medium {
        Medium::File => read_file(name, offset, size, false),
        Medium::TempFile => read_file(name, offset, size, true),
        Medium::SharedMemory => read_shared_memory(name, offset, size),
        Medium::Direct => Err("direct data is not local media".to_owned()),
    };
    result.map_err(|reason| {
        log::debug!(
            "graphics {medium:?} read of {:?} refused: {reason}",
            String::from_utf8_lossy(name)
        );
        GraphicsError::bad_file()
    })
}

/// `t=f` and `t=t`.
fn read_file(name: &[u8], offset: u32, size: u32, temporary: bool) -> Result<Vec<u8>, Refusal> {
    let path = file_path(name)?;
    // Resolving touches directories and links only; nothing is opened before
    // the checks below, since opening can have side effects of its own.
    let canonical = fs::canonicalize(&path)
        .map_err(|err| format!("cannot resolve {}: {err}", path.display()))?;
    if let Some(area) = sensitive_area(&canonical) {
        return Err(format!("{} lies in {area}", canonical.display()));
    }
    let metadata = fs::metadata(&canonical)
        .map_err(|err| format!("cannot stat {}: {err}", canonical.display()))?;
    if !metadata.is_file() {
        return Err(format!("{} is not a regular file", canonical.display()));
    }
    let (file, opened) = open_checked(&canonical, &metadata)?;
    opened_path_matches(&file, &canonical)?;
    let data = read_span(file, offset, size)?;
    if temporary {
        delete_temp_file(&canonical, &opened);
    }
    Ok(data)
}

/// The path a file name spells: UTF-8, absolute, of sane length, with no
/// NUL (which the OS would cut the path at).
fn file_path(name: &[u8]) -> Result<PathBuf, Refusal> {
    if name.len() > MAX_NAME_BYTES {
        return Err(format!("a {}-byte path is too long", name.len()));
    }
    let name = std::str::from_utf8(name).map_err(|_| "the path is not UTF-8".to_owned())?;
    if name.contains('\0') {
        return Err("the path contains NUL".to_owned());
    }
    let path = PathBuf::from(name);
    if !path.is_absolute() {
        return Err("the path is not absolute".to_owned());
    }
    // Resolving a network path would already reach out to the host it
    // names (and, on Windows, offer it the user's credentials), so a path
    // that is not a plain drive path is refused before it is resolved. A
    // share mapped to a drive letter passes: telling it from a local drive
    // takes `GetDriveTypeW`, which the standard library does not expose.
    #[cfg(windows)]
    if !is_drive_path(&path) {
        return Err("the path is not a drive path".to_owned());
    }
    Ok(path)
}

/// The sensitive part of the filesystem a resolved path lies in, if any.
#[cfg(not(windows))]
fn sensitive_area(canonical: &Path) -> Option<&'static str> {
    if canonical.starts_with("/dev/shm") {
        return None;
    }
    ["/proc", "/sys", "/dev"]
        .into_iter()
        .find(|area| canonical.starts_with(area))
}

/// The sensitive part of the filesystem a resolved path lies in, if any:
/// anything but a drive path (devices, pipes, network shares).
#[cfg(windows)]
fn sensitive_area(canonical: &Path) -> Option<&'static str> {
    (!is_drive_path(canonical)).then_some("a device or network namespace")
}

/// Whether `path` is a plain drive path (`C:\…` or `\\?\C:\…`).
#[cfg(windows)]
fn is_drive_path(path: &Path) -> bool {
    use std::path::{Component, Prefix};
    matches!(
        path.components().next(),
        Some(Component::Prefix(prefix))
            if matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_))
    )
}

/// `O_NONBLOCK`, so opening a FIFO swapped in after the checks cannot block
/// the socket task (the check after opening then refuses it), and
/// `O_NOCTTY`, so a terminal device swapped in cannot become the process's
/// controlling terminal for the moment before that check. This crate has no
/// `libc` dependency; these are the kernels' ABI values, and a platform not
/// listed opens without the flags.
#[cfg(all(
    target_os = "linux",
    not(any(
        target_arch = "mips",
        target_arch = "mips64",
        target_arch = "sparc",
        target_arch = "sparc64"
    ))
))]
const OPEN_FLAGS: i32 = 0o4000 | 0o400;
#[cfg(all(target_os = "linux", any(target_arch = "mips", target_arch = "mips64")))]
const OPEN_FLAGS: i32 = 0x80 | 0x800;
#[cfg(all(
    target_os = "linux",
    any(target_arch = "sparc", target_arch = "sparc64")
))]
const OPEN_FLAGS: i32 = 0x4000 | 0x8000;
#[cfg(target_vendor = "apple")]
const OPEN_FLAGS: i32 = 0x4 | 0x20000;
#[cfg(any(
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd",
    target_os = "dragonfly"
))]
const OPEN_FLAGS: i32 = 0x4 | 0x8000;
#[cfg(all(
    unix,
    not(any(
        target_os = "linux",
        target_vendor = "apple",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd",
        target_os = "dragonfly"
    ))
))]
const OPEN_FLAGS: i32 = 0;

/// Open what was checked as `expected` and make sure it is still that
/// regular file: the name may have been pointed elsewhere in between. The
/// opened file's metadata comes back with it, for the checks after the read.
fn open_checked(path: &Path, expected: &Metadata) -> Result<(File, Metadata), Refusal> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(OPEN_FLAGS);
    }
    let file = options
        .open(path)
        .map_err(|err| format!("cannot open {}: {err}", path.display()))?;
    let opened = file
        .metadata()
        .map_err(|err| format!("cannot stat the opened {}: {err}", path.display()))?;
    if !opened.is_file() || !same_file(expected, &opened) {
        return Err(format!(
            "{} changed between its check and its opening",
            path.display()
        ));
    }
    Ok((file, opened))
}

/// The path the kernel opened `file` at must be `canonical`, the resolved
/// path that was checked, and must still lie outside the sensitive areas:
/// a directory on the way swapped for a symlink after the resolution
/// would have led the open elsewhere (into `/proc`, say) with the
/// resolved path none the wiser. Read from `/proc/self/fd`, which names
/// the opened file as the kernel sees it; without `/proc` (a bare
/// container) every file is refused, which is the safe way round.
#[cfg(target_os = "linux")]
fn opened_path_matches(file: &File, canonical: &Path) -> Result<(), Refusal> {
    use std::os::fd::AsRawFd as _;
    let link = PathBuf::from(format!("/proc/self/fd/{}", file.as_raw_fd()));
    let actual =
        fs::read_link(&link).map_err(|err| format!("cannot read {}: {err}", link.display()))?;
    if actual != canonical {
        return Err(format!(
            "{} was opened as {}",
            canonical.display(),
            actual.display()
        ));
    }
    if let Some(area) = sensitive_area(&actual) {
        return Err(format!("{} lies in {area}", actual.display()));
    }
    Ok(())
}

/// Elsewhere the opened file is known by its identity alone
/// ([`open_checked`]).
#[cfg(not(target_os = "linux"))]
fn opened_path_matches(_file: &File, _canonical: &Path) -> Result<(), Refusal> {
    Ok(())
}

#[cfg(unix)]
fn same_file(a: &Metadata, b: &Metadata) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    a.dev() == b.dev() && a.ino() == b.ino()
}

/// Windows' file identity in std is unstable; the opened handle's own
/// metadata (a regular file) is what is checked there.
#[cfg(not(unix))]
fn same_file(_a: &Metadata, _b: &Metadata) -> bool {
    true
}

/// Read `size` bytes (zero: to the end) from `offset` of `file`.
fn read_span(mut file: File, offset: u32, size: u32) -> Result<Vec<u8>, Refusal> {
    let len = file
        .metadata()
        .map_err(|err| format!("cannot stat: {err}"))?
        .len();
    let want = span_len(len, offset, size)?;
    file.seek(SeekFrom::Start(u64::from(offset)))
        .map_err(|err| format!("cannot seek to {offset}: {err}"))?;
    let mut data = Vec::with_capacity(want);
    file.take(want as u64)
        .read_to_end(&mut data)
        .map_err(|err| format!("read failed: {err}"))?;
    if data.len() != want {
        // Truncated while being read.
        return Err(format!("read {} of {want} bytes", data.len()));
    }
    Ok(data)
}

/// How many bytes a read of `size` (zero: to the end) from `offset` of a
/// `len`-byte file takes, if it can.
fn span_len(len: u64, offset: u32, size: u32) -> Result<usize, Refusal> {
    let available = len.saturating_sub(u64::from(offset));
    let want = if size == 0 {
        available
    } else {
        u64::from(size)
    };
    if want == 0 {
        return Err(format!(
            "nothing to read past offset {offset} of {len} bytes"
        ));
    }
    if want > MAX_IMAGE_BYTES {
        return Err(format!(
            "{want} bytes exceed the {MAX_IMAGE_BYTES}-byte limit"
        ));
    }
    if want > available {
        return Err(format!(
            "{want} bytes asked for, {available} past offset {offset}"
        ));
    }
    usize::try_from(want).map_err(|_| format!("{want} bytes do not fit in memory"))
}

/// Delete a temporary file just read, if its resolved path says it is one
/// and the path still leads to the file that was read (`opened`): unlinking
/// follows the path as it is now, not as it was resolved, and a component
/// swapped for a symlink meanwhile would have it delete something else.
fn delete_temp_file(canonical: &Path, opened: &Metadata) {
    if !is_deletable_temp_file(canonical) {
        log::debug!(
            "graphics temporary file {} kept: not a {TEMP_FILE_TOKEN} file in a temporary directory",
            canonical.display()
        );
        return;
    }
    if !still_the_file_read(canonical, opened) {
        log::debug!(
            "graphics temporary file {} kept: the path no longer leads to the file that was read",
            canonical.display()
        );
        return;
    }
    if let Err(err) = fs::remove_file(canonical) {
        log::debug!(
            "graphics temporary file {} not deleted: {err}",
            canonical.display()
        );
    }
}

/// Whether `canonical` still resolves to itself (no symlink on the way) and
/// its last component, read without following a link, is the regular file
/// `opened` describes.
fn still_the_file_read(canonical: &Path, opened: &Metadata) -> bool {
    fs::canonicalize(canonical).is_ok_and(|now| now == canonical)
        && fs::symlink_metadata(canonical).is_ok_and(|now| now.is_file() && same_file(opened, &now))
}

/// Whether a resolved path names a file `t=t` may delete: the token in its
/// path, under a temporary directory.
fn is_deletable_temp_file(canonical: &Path) -> bool {
    canonical
        .to_str()
        .is_some_and(|path| path.contains(TEMP_FILE_TOKEN))
        && is_under_temp_dir(canonical)
}

/// Whether `path` lies under one of the temporary directories, each taken
/// as spelled and resolved (`/tmp` is `/private/tmp` on macOS). A relative
/// `$TMPDIR` or a filesystem root counts as none.
fn is_under_temp_dir(path: &Path) -> bool {
    let mut dirs = vec![
        PathBuf::from("/tmp"),
        PathBuf::from("/dev/shm"),
        std::env::temp_dir(),
    ];
    if let Some(dir) = std::env::var_os("TMPDIR") {
        dirs.push(PathBuf::from(dir));
    }
    let resolved: Vec<PathBuf> = dirs
        .iter()
        .filter_map(|dir| fs::canonicalize(dir).ok())
        .collect();
    dirs.into_iter()
        .chain(resolved)
        .filter(|dir| dir.is_absolute() && dir.parent().is_some())
        .any(|dir| path.starts_with(dir))
}

/// `t=s` on Linux: the object glibc's `shm_open` would open, read through
/// `/dev/shm` without following a symlink, then unlinked.
#[cfg(target_os = "linux")]
fn read_shared_memory(name: &[u8], offset: u32, size: u32) -> Result<Vec<u8>, Refusal> {
    let object = shm_object_name(name)?;
    let path = Path::new("/dev/shm").join(object);
    let metadata = fs::symlink_metadata(&path)
        .map_err(|err| format!("cannot stat {}: {err}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!("{} is not a regular object", path.display()));
    }
    let (file, _) = open_checked(&path, &metadata)?;
    // Once opened the object is unlinked whatever the read makes of it, so
    // a refused one is not left behind holding memory.
    let data = read_span(file, offset, size);
    if let Err(err) = fs::remove_file(&path) {
        log::debug!(
            "graphics shared memory {} not unlinked: {err}",
            path.display()
        );
    }
    data
}

/// The object a shared-memory name names under `/dev/shm`: leading slashes
/// dropped, as glibc does, and nothing else that could leave the directory.
#[cfg(target_os = "linux")]
fn shm_object_name(name: &[u8]) -> Result<&str, Refusal> {
    // NAME_MAX, plus the leading slash.
    if name.len() > 256 {
        return Err(format!("a {}-byte name is too long", name.len()));
    }
    let name = std::str::from_utf8(name).map_err(|_| "the name is not UTF-8".to_owned())?;
    let object = name.trim_start_matches('/');
    if object.is_empty() || object == "." || object == ".." || object.contains(['/', '\0']) {
        return Err(format!("{name:?} is not a shared-memory name"));
    }
    Ok(object)
}

/// `t=s` elsewhere: other unix systems keep shared memory out of the
/// filesystem (it needs `shm_open`, which needs `libc`), and Windows' named
/// mappings need `windows-sys`; neither is a dependency of this crate.
#[cfg(not(target_os = "linux"))]
fn read_shared_memory(_name: &[u8], _offset: u32, _size: u32) -> Result<Vec<u8>, Refusal> {
    Err("shared-memory media are only read on Linux".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use smudgy_protocol::graphics::ErrorCode;

    fn read(medium: Medium, path: &Path, offset: u32, size: u32) -> Result<Vec<u8>, GraphicsError> {
        read_local_media(medium, path.to_str().unwrap().as_bytes(), offset, size)
    }

    #[track_caller]
    fn assert_bad_file(result: Result<Vec<u8>, GraphicsError>) {
        let err = result.expect_err("the read must fail");
        assert_eq!(err.code, ErrorCode::BadFile);
        assert_eq!(err.to_string(), "EBADF:Failed to read image file");
    }

    /// A directory that is not a temporary one, for files that must
    /// survive: beside the test binary, unless the build itself lives under
    /// a temporary directory.
    fn non_temp_dir() -> Option<tempfile::TempDir> {
        let exe = std::env::current_exe().ok()?;
        let parent = fs::canonicalize(exe.parent()?).ok()?;
        if is_under_temp_dir(&parent) {
            eprintln!("skipped: the build directory is a temporary directory");
            return None;
        }
        tempfile::Builder::new()
            .prefix("graphics-media-")
            .tempdir_in(parent)
            .ok()
    }

    #[test]
    fn a_regular_file_reads_from_its_offset() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pixels.bin");
        fs::write(&path, b"0123456789").unwrap();
        assert_eq!(read(Medium::File, &path, 2, 4).unwrap(), b"2345");
        assert_eq!(read(Medium::File, &path, 0, 10).unwrap(), b"0123456789");
        assert!(path.exists(), "t=f never deletes");
    }

    #[test]
    fn size_zero_reads_to_the_end() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pixels.bin");
        fs::write(&path, b"0123456789").unwrap();
        assert_eq!(read(Medium::File, &path, 0, 0).unwrap(), b"0123456789");
        assert_eq!(read(Medium::File, &path, 7, 0).unwrap(), b"789");
    }

    #[test]
    fn reads_past_the_end_or_over_the_limit_fail() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pixels.bin");
        fs::write(&path, b"0123456789").unwrap();
        // Shorter than claimed, nothing past the offset, an empty file.
        assert_bad_file(read(Medium::File, &path, 8, 3));
        assert_bad_file(read(Medium::File, &path, 10, 0));
        assert_bad_file(read(Medium::File, &path, 11, 1));
        let empty = dir.path().join("empty.bin");
        fs::write(&empty, b"").unwrap();
        assert_bad_file(read(Medium::File, &empty, 0, 0));
        assert_eq!(
            span_len(u64::MAX, 0, 0),
            Err(format!(
                "{} bytes exceed the {MAX_IMAGE_BYTES}-byte limit",
                u64::MAX
            ))
        );
        assert!(span_len(MAX_IMAGE_BYTES, 0, 0).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_to_a_regular_file_is_followed() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("pixels.bin");
        fs::write(&target, b"abcdef").unwrap();
        let link = dir.path().join("link.bin");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert_eq!(read(Medium::File, &link, 1, 2).unwrap(), b"bc");
    }

    #[test]
    fn relative_missing_directory_and_non_utf8_names_fail() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("pixels.bin"), b"abc").unwrap();
        assert_bad_file(read_local_media(Medium::File, b"pixels.bin", 0, 0));
        assert_bad_file(read_local_media(Medium::File, b"./pixels.bin", 0, 0));
        assert_bad_file(read(Medium::File, &dir.path().join("missing.bin"), 0, 0));
        assert_bad_file(read(Medium::File, dir.path(), 0, 0));
        assert_bad_file(read_local_media(Medium::File, b"/tmp/\xff\xfe.bin", 0, 0));
        assert_bad_file(read_local_media(Medium::File, b"", 0, 0));
        let mut nul = dir
            .path()
            .join("pixels.bin")
            .to_str()
            .unwrap()
            .as_bytes()
            .to_vec();
        nul.extend_from_slice(b"\0.png");
        assert_bad_file(read_local_media(Medium::File, &nul, 0, 0));
        assert_bad_file(read_local_media(
            Medium::File,
            &vec![b'/'; MAX_NAME_BYTES + 1],
            0,
            0,
        ));
        assert_bad_file(read_local_media(Medium::Direct, b"/tmp", 0, 0));
    }

    #[cfg(unix)]
    #[test]
    fn sensitive_and_special_files_fail() {
        // Regular files by their metadata (or devices), all in places no
        // image comes from.
        for path in [
            "/proc/self/status",
            "/proc/version",
            "/sys/kernel/uevent_seqnum",
            "/dev/null",
            "/dev/zero",
        ] {
            if Path::new(path).exists() {
                assert_bad_file(read(Medium::File, Path::new(path), 0, 16));
            }
        }
        // A symlink into /proc resolves there and is refused there.
        if Path::new("/proc/self/status").exists() {
            let dir = tempfile::tempdir().unwrap();
            let link = dir.path().join("status.png");
            std::os::unix::fs::symlink("/proc/self/status", &link).unwrap();
            assert_bad_file(read(Medium::File, &link, 0, 16));
        }
        assert_eq!(sensitive_area(Path::new("/proc/1/environ")), Some("/proc"));
        assert_eq!(sensitive_area(Path::new("/sys/power/state")), Some("/sys"));
        assert_eq!(sensitive_area(Path::new("/dev/sda")), Some("/dev"));
        assert_eq!(
            sensitive_area(Path::new("/dev/shm/tty-graphics-protocol.rgb")),
            None
        );
        assert_eq!(sensitive_area(Path::new("/devices/x")), None);
        assert_eq!(sensitive_area(Path::new("/tmp/x")), None);
    }

    /// A FIFO is refused on its metadata, never opened: opening one for
    /// reading would block until a writer came.
    #[cfg(unix)]
    #[test]
    fn a_fifo_fails_without_blocking() {
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("pipe.png");
        let made = std::process::Command::new("mkfifo").arg(&fifo).status();
        if !made.is_ok_and(|status| status.success()) {
            eprintln!("skipped: mkfifo is not available");
            return;
        }
        let started = std::time::Instant::now();
        assert_bad_file(read(Medium::File, &fifo, 0, 0));
        assert_bad_file(read(Medium::TempFile, &fifo, 0, 0));
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        assert!(fifo.exists());
    }

    /// The non-blocking open itself: a FIFO opened through `open_checked`
    /// (as one swapped in after the checks would be) returns at once and is
    /// refused as no longer the checked regular file.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_fifo_swapped_in_after_the_checks_is_refused_without_blocking() {
        let dir = tempfile::tempdir().unwrap();
        let regular = dir.path().join("regular.bin");
        fs::write(&regular, b"abc").unwrap();
        let checked = fs::metadata(&regular).unwrap();
        let fifo = dir.path().join("pipe.png");
        let made = std::process::Command::new("mkfifo").arg(&fifo).status();
        if !made.is_ok_and(|status| status.success()) {
            eprintln!("skipped: mkfifo is not available");
            return;
        }
        let refusal = open_checked(&fifo, &checked).expect_err("a FIFO is refused");
        assert!(refusal.contains("changed"), "{refusal}");
        // A regular file replaced by another between check and open.
        let other = dir.path().join("other.bin");
        fs::write(&other, b"abc").unwrap();
        assert!(open_checked(&other, &checked).is_err());
        assert!(open_checked(&regular, &checked).is_ok());
    }

    /// The kernel's own path for the opened descriptor must be the
    /// resolved one: a file opened through a symlink is known by its
    /// target, and a descriptor that turns out to be somewhere else (or in
    /// a sensitive area) is refused.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_opened_descriptor_must_be_at_the_resolved_path() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("pixels.bin");
        fs::write(&target, b"abc").unwrap();
        let canonical = fs::canonicalize(&target).unwrap();
        let metadata = fs::metadata(&canonical).unwrap();
        let (file, _) = open_checked(&canonical, &metadata).unwrap();
        assert_eq!(opened_path_matches(&file, &canonical), Ok(()));
        let link = dir.path().join("link.bin");
        std::os::unix::fs::symlink(&canonical, &link).unwrap();
        let (through_link, _) = open_checked(&link, &metadata).unwrap();
        assert_eq!(opened_path_matches(&through_link, &canonical), Ok(()));
        let other = dir.path().join("other.bin");
        fs::write(&other, b"abc").unwrap();
        let refusal = opened_path_matches(&file, &fs::canonicalize(&other).unwrap())
            .expect_err("another path");
        assert!(refusal.contains("was opened as"), "{refusal}");
        // A descriptor that is where it says, but in a sensitive area.
        let status = PathBuf::from(format!("/proc/{}/status", std::process::id()));
        if let Ok(file) = File::open(&status) {
            let refusal = opened_path_matches(&file, &status).expect_err("in /proc");
            assert!(refusal.contains("lies in /proc"), "{refusal}");
            let refusal = opened_path_matches(&file, Path::new("/proc/self/status"))
                .expect_err("not the kernel's spelling");
            assert!(refusal.contains("was opened as"), "{refusal}");
        }
    }

    /// A temporary file is unlinked only while its path still leads to the
    /// file that was read: once the path is a symlink, is gone, or names
    /// something else, nothing is deleted — not the link's target either.
    #[cfg(unix)]
    #[test]
    fn a_temporary_file_is_not_deleted_once_its_path_leads_elsewhere() {
        let dir = tempfile::tempdir().unwrap();
        let temp = fs::canonicalize(dir.path())
            .unwrap()
            .join("tty-graphics-protocol-swap.rgb");
        fs::write(&temp, b"abcdef").unwrap();
        let opened = File::open(&temp).unwrap().metadata().unwrap();
        assert!(still_the_file_read(&temp, &opened));

        // Swapped for a symlink to another file: kept, the target with it.
        let elsewhere = dir.path().join("precious.rgb");
        fs::write(&elsewhere, b"keep").unwrap();
        fs::remove_file(&temp).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &temp).unwrap();
        assert!(!still_the_file_read(&temp, &opened));
        delete_temp_file(&temp, &opened);
        assert!(fs::symlink_metadata(&temp).is_ok(), "the link stays");
        assert_eq!(fs::read(&elsewhere).unwrap(), b"keep");

        // Replaced by another regular file (which the filesystem may give
        // the freed inode number, and then it passes for the one read: the
        // check is by identity), or by a directory, or gone.
        fs::remove_file(&temp).unwrap();
        fs::write(&temp, b"other").unwrap();
        let replaced = fs::metadata(&temp).unwrap();
        if !same_file(&opened, &replaced) {
            assert!(!still_the_file_read(&temp, &opened));
        }
        fs::remove_file(&temp).unwrap();
        fs::create_dir(&temp).unwrap();
        assert!(!still_the_file_read(&temp, &opened));
        delete_temp_file(&temp, &opened);
        assert!(temp.is_dir(), "a directory is never removed");
        fs::remove_dir(&temp).unwrap();
        assert!(!still_the_file_read(&temp, &opened));

        // The file itself, still there: deleted.
        fs::write(&temp, b"abcdef").unwrap();
        let opened = File::open(&temp).unwrap().metadata().unwrap();
        delete_temp_file(&temp, &opened);
        assert!(!temp.exists());
    }

    #[test]
    fn a_temporary_file_is_deleted_only_when_named_and_placed_as_one() {
        // In a temporary directory with the token: read, then deleted.
        let dir = tempfile::tempdir().unwrap();
        assert!(is_under_temp_dir(&fs::canonicalize(dir.path()).unwrap()));
        let named = dir.path().join("tty-graphics-protocol-1.rgb");
        fs::write(&named, b"abcdef").unwrap();
        assert_eq!(read(Medium::TempFile, &named, 0, 0).unwrap(), b"abcdef");
        assert!(!named.exists(), "a token-named temporary file is deleted");

        // In a temporary directory without the token: read, kept.
        let plain = dir.path().join("pixels.rgb");
        fs::write(&plain, b"abcdef").unwrap();
        assert_eq!(read(Medium::TempFile, &plain, 0, 3).unwrap(), b"abc");
        assert!(plain.exists(), "a file without the token is kept");

        // The token in a directory name counts: it is in the full path.
        let token_dir = dir.path().join("tty-graphics-protocol-dir");
        fs::create_dir(&token_dir).unwrap();
        let nested = token_dir.join("pixels.rgb");
        fs::write(&nested, b"abcdef").unwrap();
        assert_eq!(read(Medium::TempFile, &nested, 0, 0).unwrap(), b"abcdef");
        assert!(!nested.exists());

        // A failed read deletes nothing.
        let short = dir.path().join("tty-graphics-protocol-2.rgb");
        fs::write(&short, b"ab").unwrap();
        assert_bad_file(read(Medium::TempFile, &short, 0, 3));
        assert!(short.exists(), "a failed read keeps the file");

        // With the token but outside every temporary directory: kept.
        let Some(outside) = non_temp_dir() else {
            return;
        };
        let elsewhere = outside.path().join("tty-graphics-protocol-3.rgb");
        fs::write(&elsewhere, b"abcdef").unwrap();
        assert_eq!(read(Medium::TempFile, &elsewhere, 0, 0).unwrap(), b"abcdef");
        assert!(
            elsewhere.exists(),
            "outside a temporary directory nothing is deleted"
        );

        // A token-named link in a temporary directory resolves to its
        // target, whose path decides: the target is kept.
        #[cfg(unix)]
        {
            let link = dir.path().join("tty-graphics-protocol-link.rgb");
            std::os::unix::fs::symlink(&elsewhere, &link).unwrap();
            assert_eq!(read(Medium::TempFile, &link, 0, 0).unwrap(), b"abcdef");
            assert!(
                elsewhere.exists(),
                "the link's target is not a temporary file"
            );
        }
    }

    #[test]
    fn filesystem_roots_and_relative_dirs_are_not_temporary_directories() {
        // The platform's own temporary directory, wherever it is.
        let temp = std::env::temp_dir();
        assert!(is_under_temp_dir(&temp.join("x")));
        assert!(is_deletable_temp_file(&temp.join("tty-graphics-protocol")));
        assert!(!is_deletable_temp_file(&temp.join("pixels.rgb")));
        // A directory whose name merely starts like one is not under it.
        let mut lookalike = temp.file_name().unwrap().to_os_string();
        lookalike.push("foo");
        let lookalike = temp.parent().unwrap().join(lookalike).join("x");
        assert!(!is_under_temp_dir(&lookalike), "{}", lookalike.display());
        // `/tmp` and `/dev/shm` are temporary directories on unix alone:
        // on Windows they are relative paths, under nothing.
        #[cfg(unix)]
        {
            assert!(is_under_temp_dir(Path::new("/tmp/x")));
            assert!(!is_under_temp_dir(Path::new("/tmpfoo/x")));
            assert!(!is_under_temp_dir(Path::new("/home/user/x")));
            assert!(!is_deletable_temp_file(Path::new(
                "/home/tty-graphics-protocol"
            )));
            assert!(is_deletable_temp_file(Path::new(
                "/tmp/tty-graphics-protocol"
            )));
            assert!(!is_deletable_temp_file(Path::new("/tmp/pixels.rgb")));
        }
        #[cfg(windows)]
        {
            assert!(!is_under_temp_dir(Path::new("/tmp/x")));
            assert!(!is_under_temp_dir(Path::new(r"C:\Users\x")));
        }
    }

    /// A shared-memory object in `/dev/shm`, removed on drop if the test
    /// left it there.
    #[cfg(target_os = "linux")]
    struct ShmObject(PathBuf);

    #[cfg(target_os = "linux")]
    impl ShmObject {
        fn create(tag: &str, bytes: &[u8]) -> Option<Self> {
            let path = PathBuf::from(format!(
                "/dev/shm/smudgy-graphics-media-{}-{tag}",
                std::process::id()
            ));
            match fs::write(&path, bytes) {
                Ok(()) => Some(Self(path)),
                Err(err) => {
                    eprintln!("skipped: /dev/shm is not writable: {err}");
                    None
                }
            }
        }

        fn name(&self) -> String {
            self.0.file_name().unwrap().to_str().unwrap().to_owned()
        }
    }

    #[cfg(target_os = "linux")]
    impl Drop for ShmObject {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn shared_memory_is_read_from_dev_shm_and_unlinked() {
        let Some(object) = ShmObject::create("read", b"0123456789") else {
            return;
        };
        let name = format!("/{}", object.name());
        assert_eq!(
            read_local_media(Medium::SharedMemory, name.as_bytes(), 3, 4).unwrap(),
            b"3456"
        );
        assert!(!object.0.exists(), "the object is unlinked after reading");

        // A name without the leading slash is passed through as it is.
        let Some(object) = ShmObject::create("bare", b"abc") else {
            return;
        };
        assert_eq!(
            read_local_media(Medium::SharedMemory, object.name().as_bytes(), 0, 0).unwrap(),
            b"abc"
        );
        assert!(!object.0.exists());

        // A read the object is too short for fails, and still unlinks it.
        let Some(object) = ShmObject::create("short", b"abc") else {
            return;
        };
        assert_bad_file(read_local_media(
            Medium::SharedMemory,
            object.name().as_bytes(),
            0,
            80,
        ));
        assert!(!object.0.exists());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn shared_memory_names_cannot_leave_dev_shm() {
        for name in [
            &b""[..],
            b"/",
            b"//",
            b".",
            b"/..",
            b"/../etc/passwd",
            b"a/b",
            b"/a\0b",
            b"\xff",
        ] {
            assert_bad_file(read_local_media(Medium::SharedMemory, name, 0, 0));
        }
        assert_bad_file(read_local_media(Medium::SharedMemory, &[b'a'; 300], 0, 0));
        // A symlink in /dev/shm is not followed (glibc opens with
        // O_NOFOLLOW), and is left alone.
        let Some(target) = ShmObject::create("target", b"abc") else {
            return;
        };
        let link = ShmObject(PathBuf::from(format!(
            "/dev/shm/smudgy-graphics-media-{}-link",
            std::process::id()
        )));
        std::os::unix::fs::symlink(&target.0, &link.0).unwrap();
        assert_bad_file(read_local_media(
            Medium::SharedMemory,
            link.name().as_bytes(),
            0,
            0,
        ));
        assert!(fs::symlink_metadata(&link.0).is_ok(), "the link stays");
        assert!(target.0.exists(), "its target stays");
    }

    /// `/dev/shm` holds ordinary files: `t=f` and `t=t` read them, and a
    /// token-named temporary file there is deleted.
    #[cfg(target_os = "linux")]
    #[test]
    fn files_in_dev_shm_are_readable_as_files() {
        let Some(object) = ShmObject::create("tty-graphics-protocol", b"abcdef") else {
            return;
        };
        assert_eq!(read(Medium::File, &object.0, 0, 0).unwrap(), b"abcdef");
        assert!(object.0.exists());
        assert_eq!(read(Medium::TempFile, &object.0, 0, 0).unwrap(), b"abcdef");
        assert!(
            !object.0.exists(),
            "a token-named file in /dev/shm is temporary"
        );
    }
}
