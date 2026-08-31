//! Bindings to BonDriver, the de-facto tuner interface on Windows.
//!
//! A BonDriver is a DLL exporting `CreateBonDriver()`, which returns a pointer
//! to a C++ object implementing `IBonDriver2`. The interface only ever existed
//! as a header passed around with TVTest, so there is nothing to bind against
//! and [`IBonDriver2Vtbl`] reproduces its virtual table by hand.
//!
//! This is Windows-only: the crate is empty everywhere else.

#![cfg(windows)]

use std::ffi::{CString, c_void};
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::ptr::null_mut;

use anyhow::{Context, bail};
use windows_sys::Win32::Foundation::{FreeLibrary, HMODULE, S_FALSE, S_OK};
use windows_sys::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
use windows_sys::Win32::System::LibraryLoader::{
    GetProcAddress, LOAD_WITH_ALTERED_SEARCH_PATH, LoadLibraryExW,
};
use windows_sys::Win32::System::Memory::{
    MEM_COMMIT, MEMORY_BASIC_INFORMATION, PAGE_EXECUTE_READ, PAGE_EXECUTE_READWRITE,
    PAGE_EXECUTE_WRITECOPY, PAGE_GUARD, PAGE_READONLY, PAGE_READWRITE, PAGE_WRITECOPY,
    VirtualQuery,
};

/// The Win32 `BOOL` the interface returns, rather than the Rust one.
type Bool = i32;

/// Big enough for whatever a driver hands over in one `GetTsStream()` call.
const BUFFER_SIZE: usize = 8 * 1024 * 1024;

/// How many tuning spaces and channels are enumerated before giving up on a
/// driver that never reports the end of the list.
const ENUM_LIMIT: u32 = 1024;

#[repr(C)]
struct IBonDriver2 {
    vtbl: *const IBonDriver2Vtbl,
}

/// The virtual table of `IBonDriver2`, which inherits `IBonDriver`.
///
/// Both `GetTsStream()` overloads share a name, so which of the two lands in
/// which slot is up to the compiler that built the driver: MSVC emits
/// overloads in reverse declaration order, others keep them as declared.
/// [`BonDriver::next_chunk`] therefore works out what it got at run time
/// instead of trusting the layout below.
#[repr(C)]
struct IBonDriver2Vtbl {
    // IBonDriver
    open_tuner: unsafe extern "system" fn(*mut IBonDriver2) -> Bool,
    close_tuner: unsafe extern "system" fn(*mut IBonDriver2),
    set_channel_by_index: unsafe extern "system" fn(*mut IBonDriver2, u8) -> Bool,
    get_signal_level: unsafe extern "system" fn(*mut IBonDriver2) -> f32,
    wait_ts_stream: unsafe extern "system" fn(*mut IBonDriver2, u32) -> u32,
    get_ready_count: unsafe extern "system" fn(*mut IBonDriver2) -> u32,
    get_ts_stream_first:
        unsafe extern "system" fn(*mut IBonDriver2, *mut u8, *mut u32, *mut u32) -> Bool,
    get_ts_stream_second:
        unsafe extern "system" fn(*mut IBonDriver2, *mut u8, *mut u32, *mut u32) -> Bool,
    purge_ts_stream: unsafe extern "system" fn(*mut IBonDriver2),
    release: unsafe extern "system" fn(*mut IBonDriver2),

    // IBonDriver2
    get_tuner_name: unsafe extern "system" fn(*mut IBonDriver2) -> *const u16,
    is_tuner_opening: unsafe extern "system" fn(*mut IBonDriver2) -> Bool,
    enum_tuning_space: unsafe extern "system" fn(*mut IBonDriver2, u32) -> *const u16,
    enum_channel_name: unsafe extern "system" fn(*mut IBonDriver2, u32, u32) -> *const u16,
    set_channel: unsafe extern "system" fn(*mut IBonDriver2, u32, u32) -> Bool,
    get_cur_space: unsafe extern "system" fn(*mut IBonDriver2) -> u32,
    get_cur_channel: unsafe extern "system" fn(*mut IBonDriver2) -> u32,
}

/// Which of the two `GetTsStream()` overloads the first vtable slot holds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StreamAbi {
    /// `GetTsStream(BYTE **ppDst, DWORD *pdwSize, DWORD *pdwRemain)`, which
    /// lends out the buffer the driver read into.
    Borrowed,
    /// `GetTsStream(BYTE *pDst, DWORD *pdwSize, DWORD *pdwRemain)`, which
    /// copies into the buffer passed to it.
    Copied,
}

/// A tuning space, along with the channels it holds.
#[derive(Clone, Debug)]
pub struct TuningSpace {
    pub index: u32,
    pub name: String,
    pub channels: Vec<String>,
}

/// Keeps COM initialised for as long as a driver is loaded on this thread.
struct Com {
    thread: std::thread::ThreadId,
    uninitialise: bool,
}

impl Com {
    fn initialise() -> Self {
        // Drivers build a DirectShow graph on the calling thread, so it has to
        // be in an apartment. A thread that already is stays as it is.
        let hr = unsafe { CoInitializeEx(null_mut(), COINIT_APARTMENTTHREADED as u32) };

        Self {
            thread: std::thread::current().id(),
            uninitialise: hr == S_OK || hr == S_FALSE,
        }
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        // CoUninitialize only balances a CoInitializeEx made on the same thread,
        // so leave the apartment alone if the driver ended up being dropped on
        // another one.
        if self.uninitialise && std::thread::current().id() == self.thread {
            unsafe { CoUninitialize() };
        }
    }
}

pub struct BonDriver {
    this: *mut IBonDriver2,
    module: HMODULE,
    opened: bool,
    abi: Option<StreamAbi>,
    buffer: Vec<u8>,
    _com: Com,
}

impl BonDriver {
    /// Loads a BonDriver DLL and instantiates the tuner it exports.
    ///
    /// Drivers read their configuration out of an `.ini` sitting next to the
    /// DLL and some of them pull in a helper DLL from the same directory, so
    /// the path is made absolute and the module is loaded with the search path
    /// rooted there.
    pub fn open(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = std::path::absolute(path.as_ref())
            .with_context(|| format!("Couldn't resolve {}", path.as_ref().display()))?;
        if !path.is_file() {
            bail!("{} is not a file", path.display());
        }

        let com = Com::initialise();

        let wide = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();

        let module =
            unsafe { LoadLibraryExW(wide.as_ptr(), null_mut(), LOAD_WITH_ALTERED_SEARCH_PATH) };
        if module.is_null() {
            bail!(
                "Couldn't load {}: {}",
                path.display(),
                std::io::Error::last_os_error(),
            );
        }

        let symbol = CString::new("CreateBonDriver").unwrap();
        let Some(create) = (unsafe { GetProcAddress(module, symbol.as_ptr() as *const u8) }) else {
            unsafe { FreeLibrary(module) };
            bail!("{} does not export CreateBonDriver()", path.display());
        };

        let create = unsafe {
            std::mem::transmute::<
                unsafe extern "system" fn() -> isize,
                unsafe extern "system" fn() -> *mut IBonDriver2,
            >(create)
        };

        let this = unsafe { create() };
        if this.is_null() {
            unsafe { FreeLibrary(module) };
            bail!("CreateBonDriver() returned no tuner");
        }

        Ok(Self {
            this,
            module,
            opened: false,
            abi: None,
            buffer: vec![0; BUFFER_SIZE],
            _com: com,
        })
    }

    fn vtbl(&self) -> &IBonDriver2Vtbl {
        unsafe { &*(*self.this).vtbl }
    }

    pub fn tuner_name(&self) -> Option<String> {
        unsafe { wide_to_string((self.vtbl().get_tuner_name)(self.this)) }
    }

    /// Enumerates every tuning space the driver offers and the channels in it.
    pub fn tuning_spaces(&self) -> Vec<TuningSpace> {
        let mut spaces = Vec::new();
        for index in 0..ENUM_LIMIT {
            let space = unsafe { (self.vtbl().enum_tuning_space)(self.this, index) };
            let Some(name) = (unsafe { wide_to_string(space) }) else {
                break;
            };

            let mut channels = Vec::new();
            for number in 0..ENUM_LIMIT {
                let channel = unsafe { (self.vtbl().enum_channel_name)(self.this, index, number) };
                let Some(name) = (unsafe { wide_to_string(channel) }) else {
                    break;
                };

                channels.push(name);
            }

            spaces.push(TuningSpace {
                index,
                name,
                channels,
            });
        }

        spaces
    }

    pub fn open_tuner(&mut self) -> anyhow::Result<()> {
        if unsafe { (self.vtbl().open_tuner)(self.this) } == 0 {
            bail!("OpenTuner() failed: the device is missing or already in use");
        }

        self.opened = true;
        Ok(())
    }

    pub fn set_channel(&mut self, space: u32, channel: u32) -> anyhow::Result<()> {
        if unsafe { (self.vtbl().set_channel)(self.this, space, channel) } == 0 {
            bail!("SetChannel({space}, {channel}) failed");
        }

        // A channel change leaves whatever the previous one buffered behind.
        unsafe { (self.vtbl().purge_ts_stream)(self.this) };
        Ok(())
    }

    /// Throws away whatever the driver has buffered so far.
    pub fn purge(&mut self) {
        unsafe { (self.vtbl().purge_ts_stream)(self.this) };
    }

    pub fn signal_level(&self) -> f32 {
        unsafe { (self.vtbl().get_signal_level)(self.this) }
    }

    pub fn ready_count(&self) -> u32 {
        unsafe { (self.vtbl().get_ready_count)(self.this) }
    }

    /// Waits up to `timeout_ms` for the driver to buffer something, then hands
    /// over one chunk of the raw transport stream, or [`None`] if nothing came.
    pub fn next_chunk(&mut self, timeout_ms: u32) -> anyhow::Result<Option<&[u8]>> {
        let vtbl = self.vtbl();
        let wait = vtbl.wait_ts_stream;
        let get = vtbl.get_ts_stream_first;

        unsafe { wait(self.this, timeout_ms) };

        // Whichever overload this slot holds, the call below is safe: the
        // copying one fills the buffer, the lending one only writes a pointer
        // into its first bytes.
        self.buffer[..size_of::<*const u8>()].fill(0);

        let buffer = self.buffer.as_mut_ptr();
        let mut size = 0_u32;
        let mut remain = 0_u32;
        // Drivers report an empty buffer by returning FALSE rather than a zero
        // size, so this means "nothing yet" rather than a failure.
        if unsafe { get(self.this, buffer, &mut size, &mut remain) } == 0 {
            return Ok(None);
        }

        let size = size as usize;
        if size == 0 {
            return Ok(None);
        }

        // Tell the two overloads apart by what the first bytes of the buffer
        // turned into: the lending one leaves a pointer to memory it owns,
        // whereas the copying one leaves stream data, which as an address is
        // not mapped.
        let lent = unsafe { *(self.buffer.as_ptr() as *const *const u8) };
        let abi = match self.abi {
            Some(abi) => abi,
            None => {
                let abi = if is_readable(lent, size) {
                    StreamAbi::Borrowed
                } else {
                    StreamAbi::Copied
                };

                self.abi = Some(abi);
                abi
            }
        };

        Ok(Some(match abi {
            StreamAbi::Copied => &self.buffer[..size],
            // The driver keeps this valid until the next call, which needs
            // `&mut self` and so cannot happen while the slice is alive.
            StreamAbi::Borrowed => unsafe { std::slice::from_raw_parts(lent, size) },
        }))
    }
}

impl Drop for BonDriver {
    fn drop(&mut self) {
        unsafe {
            if self.opened {
                (self.vtbl().close_tuner)(self.this);
            }

            (self.vtbl().release)(self.this);
            FreeLibrary(self.module);
        }
    }
}

// The interface is not thread-safe, but an owned driver may be moved to another
// thread as long as COM is initialised on whichever one uses it.
unsafe impl Send for BonDriver {}

/// Whether `len` bytes at `ptr` are committed and readable by this process.
fn is_readable(ptr: *const u8, len: usize) -> bool {
    const READABLE: u32 = PAGE_READONLY
        | PAGE_READWRITE
        | PAGE_WRITECOPY
        | PAGE_EXECUTE_READ
        | PAGE_EXECUTE_READWRITE
        | PAGE_EXECUTE_WRITECOPY;

    if ptr.is_null() || len == 0 {
        return false;
    }

    let mut region = unsafe { std::mem::zeroed::<MEMORY_BASIC_INFORMATION>() };
    let written = unsafe {
        VirtualQuery(
            ptr as *const c_void,
            &mut region,
            size_of::<MEMORY_BASIC_INFORMATION>(),
        )
    };

    if written == 0 || region.State != MEM_COMMIT {
        return false;
    }

    if region.Protect & READABLE == 0 || region.Protect & PAGE_GUARD != 0 {
        return false;
    }

    // The whole chunk has to fit in the region the pointer landed in.
    ptr as usize + len <= region.BaseAddress as usize + region.RegionSize
}

unsafe fn wide_to_string(ptr: *const u16) -> Option<String> {
    if ptr.is_null() {
        return None;
    }

    let mut len = 0;
    while unsafe { *ptr.add(len) } != 0 {
        len += 1;
    }

    let wide = unsafe { std::slice::from_raw_parts(ptr, len) };
    Some(String::from_utf16_lossy(wide))
}
