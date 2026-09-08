//! Native file authority for streaming audio playback.
//!
//! The file-source resolver uses the calling isolate's Deno
//! read permissions and returns a one-use opener for `AudioFileStream::open`.
//! Source authorization is separate from playback admission and decoder limits.

use std::borrow::Cow;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

use deno_core::{ModuleSpecifier, OpState};
use deno_error::JsErrorBox;
use deno_permissions::{OpenAccessKind, PermissionsContainer};

const API_NAME: &str = "Smudgy audio file";

/// File-source resolver installed only in Smudgy runtimes with Web Audio enabled.
pub(crate) struct SmudgyAudioFileResolver;

impl deno_audio::AudioFileResolver for SmudgyAudioFileResolver {
    fn resolve(
        &self,
        state: &OpState,
        source: &str,
    ) -> Result<deno_audio::AudioFileOpener, JsErrorBox> {
        let request = if Path::new(source).is_absolute() {
            AuthorizedAudioFile::from_path(state, Path::new(source))?
        } else if let Ok(url) = ModuleSpecifier::parse(source) {
            AuthorizedAudioFile::from_file_url(state, &url)?
        } else {
            AuthorizedAudioFile::from_path(state, Path::new(source))?
        };
        Ok(Box::new(request.into_opener()))
    }
}

/// A regular-file request authorized by the originating isolate.
///
/// Preparation checks read permission and resolves the path without reading file
/// contents. The retained stream descriptor is opened on the decoder worker.
/// The decoder worker rechecks the same live permission container before opening.
/// Revocation before that check denies the open; as with Deno file handles,
/// revocation after opening does not revoke an existing descriptor.
///
/// Native callers must pass the opener to audio admission in that same isolate.
/// This type is not a transferable JavaScript capability or a decoder memory limit.
pub struct AuthorizedAudioFile {
    path: PathBuf,
    permissions: PermissionsContainer,
}

impl AuthorizedAudioFile {
    /// Authorizes a filesystem path using the permission container in `state`.
    /// Relative paths resolve against the process directory at preparation time.
    /// The canonical target is also checked and retained instead of reopening
    /// the input spelling; later directory or alias changes cannot redirect it.
    ///
    /// # Errors
    /// Returns an error if permissions are absent or deny access, the path cannot
    /// be resolved, or it denotes a Windows device/pipe namespace. No allow-all
    /// fallback is used.
    pub fn from_path(state: &OpState, path: &Path) -> Result<Self, JsErrorBox> {
        let permissions = state.try_borrow::<PermissionsContainer>().ok_or_else(|| {
            JsErrorBox::generic("audio file access requires isolate read permissions")
        })?;
        let path = std::path::absolute(path).map_err(JsErrorBox::from_err)?;
        validate_local_path(&path)?;
        let checked = permissions
            .check_open(Cow::Owned(path), OpenAccessKind::Read, Some(API_NAME))
            .map_err(JsErrorBox::from_err)?;
        validate_local_path(&checked)?;
        let resolved = std::fs::canonicalize(&checked).map_err(JsErrorBox::from_err)?;
        validate_local_path(&resolved)?;
        let checked = permissions
            .check_open(Cow::Owned(resolved), OpenAccessKind::Read, Some(API_NAME))
            .map_err(JsErrorBox::from_err)?;
        Ok(Self {
            path: checked.into_owned_path(),
            permissions: permissions.clone(),
        })
    }

    /// Authorizes a local `file:` URL. Callers can resolve it against their module
    /// URL before calling; this method does not infer a package or module root.
    ///
    /// # Errors
    /// Rejects other schemes, remote hosts, queries/fragments, invalid file URLs,
    /// and paths denied by the calling isolate's read permissions.
    pub fn from_file_url(state: &OpState, url: &ModuleSpecifier) -> Result<Self, JsErrorBox> {
        if url.scheme() != "file"
            || url
                .host_str()
                .is_some_and(|host| !host.is_empty() && host != "localhost")
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(JsErrorBox::type_error(
                "audio source must be a local file URL without a query or fragment",
            ));
        }
        let path = url
            .to_file_path()
            .map_err(|()| JsErrorBox::type_error("audio source is not a valid local file URL"))?;
        Self::from_path(state, &path)
    }

    /// Consumes the request and returns an opener suitable for `AudioFileStream::open`.
    /// Invoke it only on the admitted decoder worker: permission resolution,
    /// metadata checks, and opening can block. No file contents are buffered here.
    pub fn into_opener(self) -> impl FnOnce() -> io::Result<File> + Send + 'static {
        move || self.open()
    }

    fn open(self) -> io::Result<File> {
        let checked = self
            .permissions
            .check_open(Cow::Owned(self.path), OpenAccessKind::Read, Some(API_NAME))
            .map_err(|error| io::Error::new(io::ErrorKind::PermissionDenied, error.to_string()))?;
        validate_local_path(&checked).map_err(|error| io::Error::other(error.to_string()))?;
        // Deno may skip canonicalization for allow-all containers. Resolve those
        // aliases too, then recheck the resolved target before a no-follow open.
        let resolved = std::fs::canonicalize(&checked)?;
        let checked = self
            .permissions
            .check_open(Cow::Owned(resolved), OpenAccessKind::Read, Some(API_NAME))
            .map_err(|error| io::Error::new(io::ErrorKind::PermissionDenied, error.to_string()))?;
        validate_local_path(&checked).map_err(|error| io::Error::other(error.to_string()))?;
        if !std::fs::metadata(&checked)?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "audio source must be a regular file",
            ));
        }
        // Preserve Deno's checked-path opening flags, including Unix O_NOFOLLOW.
        let mut options =
            deno_fs::open_options_for_checked_path(deno_fs::OpenOptions::read(), &checked);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            // A path swapped to a FIFO after metadata must not park the worker.
            // O_NONBLOCK has no effect on ordinary file reads.
            options.custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            // Open a raced final-component reparse point itself, never its target.
            // Regular files need neither directory backup semantics nor device flags.
            const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
            options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        }
        let file = options.open(&checked)?;
        let metadata = file.metadata()?;
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
            if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "audio source cannot be a reparse point",
                ));
            }
        }
        if !metadata.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "audio source must be a regular file",
            ));
        }
        Ok(file)
    }
}

fn validate_local_path(path: &Path) -> Result<(), JsErrorBox> {
    if !path.is_absolute() {
        return Err(JsErrorBox::type_error(
            "audio source requires an absolute local path",
        ));
    }
    #[cfg(windows)]
    {
        use std::path::{Component, Prefix};
        // UNC shares and device/pipe namespaces are outside this local-file API.
        if !matches!(path.components().next(), Some(Component::Prefix(prefix))
            if matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)))
        {
            return Err(JsErrorBox::type_error(
                "audio source must be a local disk file",
            ));
        }
    }
    Ok(())
}
