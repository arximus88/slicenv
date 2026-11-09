//! Межа C ABI; жоден C++-тип не перетинає її.
#[cfg_attr(not(bambu_plugin), allow(unused_imports))]
use std::ffi::{c_char, c_void};

#[repr(C)]
#[derive(Default)]
pub(crate) struct Bytes {
    pub data: *mut u8,
    pub len: usize,
}

impl Bytes {
    pub fn to_vec(&self) -> Vec<u8> {
        if self.len == 0 {
            return Vec::new();
        }
        // Буфер належить шиму; копіюємо до його звільнення.
        unsafe { std::slice::from_raw_parts(self.data, self.len) }.to_vec()
    }
    pub fn text(&self) -> String {
        if self.len == 0 {
            return String::new();
        }
        // Буфер створений шимом і живе до Drop.
        String::from_utf8_lossy(unsafe { std::slice::from_raw_parts(self.data, self.len) })
            .into_owned()
    }
}

impl Drop for Bytes {
    fn drop(&mut self) {
        // Передаємо володіння буфером назад C++.
        unsafe {
            cloud_free(Bytes {
                data: self.data,
                len: self.len,
            })
        };
    }
}

pub(crate) type Callback =
    unsafe extern "C" fn(*mut c_void, i32, *const u8, usize, *const u8, usize, i32, i32);

#[cfg(bambu_plugin)]
unsafe extern "C" {
    pub fn cloud_open(
        path: *const c_char,
        log: *const c_char,
        callback: Callback,
        user: *mut c_void,
        out: *mut *mut c_void,
    ) -> i32;
    pub fn cloud_close(handle: *mut c_void) -> i32;
    pub fn cloud_plugin_version(path: *const c_char, out: *mut Bytes) -> i32;
    fn cloud_free(bytes: Bytes);
    pub fn cloud_call(
        handle: *mut c_void,
        op: i32,
        a: *const c_char,
        b: *const c_char,
        number: i32,
        out: *mut Bytes,
    ) -> i32;
    pub fn cloud_camera_open(path: *const c_char, url: *const c_char, out: *mut *mut c_void)
    -> i32;
    pub fn cloud_camera_start(handle: *mut c_void) -> i32;
    pub fn cloud_camera_read(handle: *mut c_void, out: *mut Bytes, timestamp: *mut u64) -> i32;
    pub fn cloud_camera_close(handle: *mut c_void);
    pub fn cloud_print(
        handle: *mut c_void,
        fields: *const *const c_char,
        plate: i32,
        flags: u32,
    ) -> i32;
}

/// Без шима (не Linux x86_64) — ті самі сигнатури, що завжди повертають «dlopen не вдався»
/// (-10001). Сюди не мали б доходити: [`crate::Client::load`] і [`crate::setup`] відсікають
/// непідтримувану ОС раніше; заглушки лише дають крейту скомпілюватися.
#[cfg(not(bambu_plugin))]
#[allow(clippy::missing_safety_doc, unused_variables)]
mod unsupported {
    use super::{Bytes, Callback};
    use std::ffi::{c_char, c_void};
    const UNAVAILABLE: i32 = -10001;
    pub unsafe fn cloud_open(
        path: *const c_char,
        log: *const c_char,
        callback: Callback,
        user: *mut c_void,
        out: *mut *mut c_void,
    ) -> i32 {
        UNAVAILABLE
    }
    pub unsafe fn cloud_close(handle: *mut c_void) -> i32 {
        0
    }
    pub unsafe fn cloud_plugin_version(path: *const c_char, out: *mut Bytes) -> i32 {
        UNAVAILABLE
    }
    /// Буферів без шима не буває: `Bytes` лишається порожнім.
    pub(super) unsafe fn cloud_free(bytes: Bytes) {
        debug_assert!(bytes.data.is_null() && bytes.len == 0);
        std::mem::forget(bytes);
    }
    pub unsafe fn cloud_call(
        handle: *mut c_void,
        op: i32,
        a: *const c_char,
        b: *const c_char,
        number: i32,
        out: *mut Bytes,
    ) -> i32 {
        UNAVAILABLE
    }
    pub unsafe fn cloud_camera_open(
        path: *const c_char,
        url: *const c_char,
        out: *mut *mut c_void,
    ) -> i32 {
        UNAVAILABLE
    }
    pub unsafe fn cloud_camera_start(handle: *mut c_void) -> i32 {
        UNAVAILABLE
    }
    pub unsafe fn cloud_camera_read(handle: *mut c_void, out: *mut Bytes, timestamp: *mut u64) -> i32 {
        UNAVAILABLE
    }
    pub unsafe fn cloud_camera_close(handle: *mut c_void) {}
    pub unsafe fn cloud_print(
        handle: *mut c_void,
        fields: *const *const c_char,
        plate: i32,
        flags: u32,
    ) -> i32 {
        UNAVAILABLE
    }
}
#[cfg(not(bambu_plugin))]
pub use unsupported::*;
