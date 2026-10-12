//! What a plugin holds on to: the host table, with a safe face.

use core::ffi::CStr;
use core::fmt;

use crate::abi::{
    ParsecOrbitHost, ParsecOrbitInterfaceHeader, ABI_VERSION, ERR_EXISTS, ERR_INVALID,
    ERR_NOT_FOUND, ERR_NOT_READY, HOST_REVISION_1_SIZE, LOG_DEBUG, LOG_ERROR, LOG_INFO, LOG_TRACE,
    LOG_WARN,
};
use crate::text::StackBuf;

/// Log level, in the order of the ABI's `LOG_*` values.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl Level {
    /// The ABI value for this level.
    pub const fn to_abi(self) -> u32 {
        match self {
            Level::Trace => LOG_TRACE,
            Level::Debug => LOG_DEBUG,
            Level::Info => LOG_INFO,
            Level::Warn => LOG_WARN,
            Level::Error => LOG_ERROR,
        }
    }
}

/// A failure reported to the loader: a non-zero code and, optionally, a
/// static explanation that is written to the log.
///
/// `init` returning `Err` makes the loader mark the plugin as failed with
/// `init returned <code>`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Error {
    code: i32,
    message: Option<&'static str>,
}

impl Error {
    /// An error with `code`. A code of `0` is replaced by `1` because `0`
    /// means success to the loader.
    pub const fn new(code: i32) -> Self {
        Error {
            code: if code == 0 { 1 } else { code },
            message: None,
        }
    }

    /// An error with a code and an explanation.
    pub const fn with_message(code: i32, message: &'static str) -> Self {
        Error {
            code: if code == 0 { 1 } else { code },
            message: Some(message),
        }
    }

    pub const fn code(&self) -> i32 {
        self.code
    }

    pub const fn message(&self) -> Option<&'static str> {
        self.message
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.message {
            Some(message) => write!(f, "{message} (code {})", self.code),
            None => write!(f, "error code {}", self.code),
        }
    }
}

impl<E: core::error::Error> From<E> for Error {
    /// Lets `?` turn any error into an init failure. The details are not
    /// kept (an [`Error`] is a code and a static string), so log them first.
    fn from(_: E) -> Self {
        Error::with_message(1, "operation failed")
    }
}

/// `Result` with the SDK's [`Error`].
pub type Result<T = (), E = Error> = core::result::Result<T, E>;

/// A host service: a `repr(C)` function table that starts with a
/// [`ParsecOrbitInterfaceHeader`], registered under a name.
///
/// `MAJOR` must match exactly; a table at least `MIN_SIZE` bytes long carries
/// every function the consumer needs. See the services specification.
pub trait Interface {
    /// Registry name, NUL terminated.
    const NAME: &'static CStr;
    /// Major version this binding was written for.
    const MAJOR: u16;
    /// Smallest `header.size` that holds every function the binding calls.
    const MIN_SIZE: usize = core::mem::size_of::<Self::Table>();
    /// The table type.
    type Table;

    /// The header at the start of `table`.
    fn header(table: &Self::Table) -> &ParsecOrbitInterfaceHeader;
}

/// The loader as seen from one plugin: a `Copy` handle to the plugin's own
/// host table, which lives for the rest of the process.
#[derive(Clone, Copy)]
pub struct Context {
    host: &'static ParsecOrbitHost,
}

impl fmt::Debug for Context {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Context")
            .field("plugin_id", &self.plugin_id())
            .finish()
    }
}

// Safety: the host table is immutable and its callbacks are documented as
// callable from any thread.
unsafe impl Send for Context {}
unsafe impl Sync for Context {}

impl Context {
    /// Wraps a host table.
    ///
    /// # Safety
    /// `host` points to a table the loader keeps alive for the rest of the
    /// process, as the ABI promises for every table passed to `init`.
    pub unsafe fn from_host(host: *const ParsecOrbitHost) -> Option<Context> {
        let host: &'static ParsecOrbitHost = unsafe { host.as_ref() }?;
        (host.struct_size as usize >= HOST_REVISION_1_SIZE).then_some(Context { host })
    }

    /// The raw table, for the parts the SDK has no wrapper for.
    pub fn host(&self) -> &'static ParsecOrbitHost {
        self.host
    }

    /// ABI version the loader implements.
    pub fn abi_version(&self) -> u32 {
        self.host.abi_version
    }

    /// Whether the loader provides the host revision `revision` (1 or 2).
    pub fn has_revision(&self, revision: u32) -> bool {
        match revision {
            0 | 1 => true,
            2 => self.host.has_revision_2(),
            _ => false,
        }
    }

    /// Version of the loader, for example `0.1.0`.
    pub fn loader_version(&self) -> &'static str {
        self.host.loader_version().unwrap_or("unknown")
    }

    /// Id of this plugin; empty on a revision 1 loader.
    pub fn plugin_id(&self) -> &'static str {
        self.host.plugin_id().unwrap_or("")
    }

    /// Directory this plugin was loaded from (revision 2).
    pub fn plugin_dir(&self) -> Option<&'static str> {
        self.host.plugin_dir()
    }

    /// Directory this plugin may keep data in (revision 2). It may not exist
    /// yet.
    pub fn data_dir(&self) -> Option<&'static str> {
        self.host.data_dir()
    }

    /// Writes one line to the loader log, tagged with the plugin id when the
    /// loader supports it. The text is formatted in a 1 KiB stack buffer and
    /// ends in `...` when it does not fit; nothing is allocated.
    pub fn log(&self, level: Level, args: fmt::Arguments<'_>) {
        let mut buffer = StackBuf::<1024>::new();
        let _ = fmt::Write::write_fmt(&mut buffer, args);
        self.host
            .log_with_target(level.to_abi(), self.plugin_id(), buffer.as_str());
    }

    /// Looks up the service `I`. `None` when nobody registered it, when its
    /// major version differs or when its table is too small for this
    /// binding. Always `None` on a revision 1 loader.
    pub fn service<I: Interface>(&self) -> Option<&'static I::Table> {
        if !self.host.has_revision_2() {
            return None;
        }
        // Safety: the registry hands out tables that start with a header and
        // live for the whole process.
        let header = unsafe { self.host.interface(I::NAME) }?;
        if (header as *const _ as usize) % core::mem::align_of::<I::Table>() != 0
            || header.major() != I::MAJOR
            || !header.covers(I::MIN_SIZE)
        {
            return None;
        }
        Some(unsafe { &*(header as *const ParsecOrbitInterfaceHeader as *const I::Table) })
    }

    /// Registers `table` as the service `I` for other plugins.
    pub fn register<I: Interface>(&self, table: &'static I::Table) -> Result {
        self.register_as(I::NAME, I::header(table))
    }

    /// Registers a table under an arbitrary name.
    pub fn register_as(&self, name: &CStr, header: &'static ParsecOrbitInterfaceHeader) -> Result {
        match self.host.register(name, header) {
            0 => Ok(()),
            ERR_EXISTS => Err(Error::with_message(
                ERR_EXISTS,
                "interface already registered",
            )),
            ERR_INVALID => Err(Error::with_message(ERR_INVALID, "interface refused")),
            ERR_NOT_READY => Err(Error::with_message(
                ERR_NOT_READY,
                "this loader has no interface registry",
            )),
            code => Err(Error::new(code)),
        }
    }

    /// Removes a service this plugin registered.
    pub fn unregister_as(&self, name: &CStr) -> Result {
        if !self.host.has_revision_2() {
            return Err(Error::new(ERR_NOT_READY));
        }
        let Some(unregister) = self.host.unregister_interface else {
            return Err(Error::new(ERR_NOT_READY));
        };
        // Safety: `self.host` is this plugin's own table.
        match unsafe { unregister(self.host, name.as_ptr() as *const u8) } {
            0 => Ok(()),
            ERR_NOT_FOUND => Err(Error::with_message(
                ERR_NOT_FOUND,
                "interface not registered",
            )),
            code => Err(Error::new(code)),
        }
    }

    /// Makes this plugin the handler of the plugin kind `kind` (for example
    /// `lua` for plugins with `kind = "lua"`): registers
    /// `parsec-orbit.kind.<kind>`.
    pub fn register_kind_handler(
        &self,
        kind: &str,
        handler: &'static crate::abi::ParsecOrbitKindHandlerV1,
    ) -> Result {
        let mut name = StackBuf::<128>::new();
        let written = fmt::Write::write_fmt(
            &mut name,
            format_args!("{}{}", crate::abi::INTERFACE_KIND_PREFIX, kind),
        );
        if written.is_err() || name.truncated() || kind.contains('\0') {
            return Err(Error::with_message(ERR_INVALID, "kind name too long"));
        }
        let name = name.as_c_str().ok_or(Error::new(ERR_INVALID))?;
        self.register_as(name, &handler.header)
    }

    /// Whether the loader's ABI is the one this SDK was built for.
    pub fn abi_matches(&self) -> bool {
        self.host.abi_version == ABI_VERSION
    }
}
