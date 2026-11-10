//! Safe wrapper over native/serial-core (GizOS Host Core). Built only when
//! build.rs found the Bazel static library; otherwise every call reports that
//! this build has no H2Loader core.

pub enum Reboot {
    App,
    Loader,
    Upgrade,
}

/// Callbacks run on the port worker thread that called into the core.
pub trait Events {
    fn log(&self, bytes: &[u8]);
    fn cancelled(&self) -> bool;
    fn progress(&self, acknowledged: u64, total: u64);
}

#[cfg(gizmoth_serial_core)]
mod native {
    use super::{Events, Reboot};
    use std::ffi::{c_char, c_int, c_void, CStr, CString};

    #[repr(C)]
    struct Ctx {
        _private: [u8; 0],
    }
    type LogFn = extern "C" fn(*mut c_void, *const u8, usize);
    type CancelFn = extern "C" fn(*mut c_void) -> c_int;
    type ProgressFn = extern "C" fn(*mut c_void, u64, u64);
    extern "C" {
        fn gm_h2l_abi_version() -> c_int;
        fn gm_h2l_global_start() -> c_int;
        fn gm_h2l_result_name(result: c_int) -> *const c_char;
        fn gm_h2l_open(
            port: *const c_char,
            handshake_ms: u32,
            log: LogFn,
            user: *mut c_void,
            cancel: CancelFn,
            cancel_user: *mut c_void,
            out: *mut *mut Ctx,
        ) -> c_int;
        fn gm_h2l_close(ctx: *mut *mut Ctx);
        fn gm_h2l_status_json(
            ctx: *mut Ctx,
            out: *mut c_char,
            cap: usize,
            cancel: CancelFn,
            user: *mut c_void,
        ) -> c_int;
        fn gm_h2l_monitor(ctx: *mut Ctx, cancel: CancelFn, user: *mut c_void) -> c_int;
        fn gm_h2l_inspect_json(path: *const c_char, out: *mut c_char, cap: usize) -> c_int;
        fn gm_h2l_stage(
            ctx: *mut Ctx,
            path: *const c_char,
            cancel: CancelFn,
            cu: *mut c_void,
            progress: ProgressFn,
            pu: *mut c_void,
        ) -> c_int;
        fn gm_h2l_reboot(ctx: *mut Ctx, kind: c_int, cancel: CancelFn, cu: *mut c_void) -> c_int;
    }

    pub const CANCELLED: i32 = 1;

    fn start() -> Result<(), String> {
        static ONCE: std::sync::OnceLock<i32> = std::sync::OnceLock::new();
        let rc = *ONCE.get_or_init(|| unsafe {
            if gm_h2l_abi_version() != 2 {
                return -3;
            }
            gm_h2l_global_start()
        });
        if rc == 0 {
            Ok(())
        } else {
            Err(name(rc))
        }
    }
    pub fn name(rc: i32) -> String {
        unsafe { CStr::from_ptr(gm_h2l_result_name(rc)) }
            .to_string_lossy()
            .into_owned()
    }

    // The user pointer is always `&&dyn Events` living on the caller's stack.
    extern "C" fn on_log(user: *mut c_void, data: *const u8, len: usize) {
        let events = unsafe { &**(user as *const &dyn Events) };
        if !data.is_null() && len > 0 {
            events.log(unsafe { std::slice::from_raw_parts(data, len) });
        }
    }
    extern "C" fn on_cancel(user: *mut c_void) -> c_int {
        let events = unsafe { &**(user as *const &dyn Events) };
        events.cancelled() as c_int
    }
    extern "C" fn on_progress(user: *mut c_void, acknowledged: u64, total: u64) {
        let events = unsafe { &**(user as *const &dyn Events) };
        events.progress(acknowledged, total);
    }

    fn json(rc: c_int, buffer: &[u8]) -> Result<serde_json::Value, String> {
        if rc != 0 {
            return Err(name(rc));
        }
        let end = buffer.iter().position(|b| *b == 0).unwrap_or(buffer.len());
        serde_json::from_slice(&buffer[..end]).map_err(|e| e.to_string())
    }

    /// One open H2Loader session. The log sink pointer passed at open time must
    /// outlive the session, so the session borrows it for its whole lifetime.
    pub struct Session<'a> {
        ctx: *mut Ctx,
        // Boxed so the C side can hold a thin pointer to this fat reference.
        #[allow(clippy::redundant_allocation)]
        sink: Box<&'a dyn Events>,
    }
    impl<'a> Session<'a> {
        pub fn open(
            port: &str,
            handshake_ms: u32,
            sink: &'a dyn Events,
        ) -> Result<Self, (i32, String)> {
            start().map_err(|e| (-2, e))?;
            let port = CString::new(port).map_err(|_| (-1, "invalid_argument".to_string()))?;
            let sink = Box::new(sink);
            let mut ctx = std::ptr::null_mut();
            let user = &*sink as *const &dyn Events as *mut c_void;
            let rc = unsafe {
                gm_h2l_open(
                    port.as_ptr(),
                    handshake_ms,
                    on_log,
                    user,
                    on_cancel,
                    user,
                    &mut ctx,
                )
            };
            if rc != 0 || ctx.is_null() {
                return Err((rc, name(rc)));
            }
            Ok(Self { ctx, sink })
        }
        fn user(&self) -> *mut c_void {
            &*self.sink as *const &dyn Events as *mut c_void
        }
        pub fn status(&mut self) -> Result<serde_json::Value, String> {
            let mut buffer = vec![0u8; 8192];
            let rc = unsafe {
                gm_h2l_status_json(
                    self.ctx,
                    buffer.as_mut_ptr() as *mut c_char,
                    buffer.len(),
                    on_cancel,
                    self.user(),
                )
            };
            json(rc, &buffer)
        }
        pub fn monitor(&mut self) -> i32 {
            unsafe { gm_h2l_monitor(self.ctx, on_cancel, self.user()) }
        }
        pub fn stage(&mut self, path: &str) -> Result<(), String> {
            let path = CString::new(path).map_err(|_| "invalid_argument".to_string())?;
            let user = self.user();
            let rc = unsafe {
                gm_h2l_stage(self.ctx, path.as_ptr(), on_cancel, user, on_progress, user)
            };
            if rc == 0 {
                Ok(())
            } else {
                Err(name(rc))
            }
        }
        pub fn reboot(&mut self, kind: Reboot) -> Result<(), String> {
            let kind = match kind {
                Reboot::App => 0,
                Reboot::Loader => 1,
                Reboot::Upgrade => 2,
            };
            let rc = unsafe { gm_h2l_reboot(self.ctx, kind, on_cancel, self.user()) };
            if rc == 0 {
                Ok(())
            } else {
                Err(name(rc))
            }
        }
    }
    impl Drop for Session<'_> {
        fn drop(&mut self) {
            unsafe { gm_h2l_close(&mut self.ctx) };
        }
    }

    pub fn inspect(path: &str) -> Result<serde_json::Value, String> {
        start()?;
        let path = CString::new(path).map_err(|_| "invalid_argument".to_string())?;
        let mut buffer = vec![0u8; 2048];
        let rc = unsafe {
            gm_h2l_inspect_json(
                path.as_ptr(),
                buffer.as_mut_ptr() as *mut c_char,
                buffer.len(),
            )
        };
        json(rc, &buffer)
    }
}

#[cfg(not(gizmoth_serial_core))]
mod native {
    use super::{Events, Reboot};
    pub const CANCELLED: i32 = 1;
    const MISSING: &str = "core_unavailable";
    pub fn name(_: i32) -> String {
        MISSING.into()
    }
    pub struct Session<'a>(std::marker::PhantomData<&'a ()>);
    impl<'a> Session<'a> {
        pub fn open(_: &str, _: u32, _: &'a dyn Events) -> Result<Self, (i32, String)> {
            Err((-3, MISSING.into()))
        }
        pub fn status(&mut self) -> Result<serde_json::Value, String> {
            Err(MISSING.into())
        }
        pub fn monitor(&mut self) -> i32 {
            -3
        }
        pub fn stage(&mut self, _: &str) -> Result<(), String> {
            Err(MISSING.into())
        }
        pub fn reboot(&mut self, _: Reboot) -> Result<(), String> {
            Err(MISSING.into())
        }
    }
    pub fn inspect(_: &str) -> Result<serde_json::Value, String> {
        Err(MISSING.into())
    }
}

pub use native::{inspect, name, Session, CANCELLED};

/// `command_availability` bits from GizOS `h2_h2loader_host.h`: what the
/// device accepts right now (it changes with App/Loader role and MFG mode).
pub const COMMANDS: &[(u32, &str)] = &[
    (1 << 0, "rebootApp"),
    (1 << 1, "rebootLoader"),
    (1 << 2, "help"),
    (1 << 3, "status"),
    (1 << 4, "stats"),
    (1 << 5, "memory"),
    (1 << 8, "coredumpStatus"),
    (1 << 9, "coredumpDump"),
    (1 << 10, "coredumpErase"),
    (1 << 11, "stagePayload"),
    (1 << 12, "stageAbort"),
    (1 << 13, "stageUrl"),
    (1 << 16, "wifiScan"),
    (1 << 17, "wifiConnect"),
    (1 << 18, "wifiDisconnect"),
    (1 << 19, "rebootUpgrade"),
    (1 << 20, "wifiStatus"),
    (1 << 21, "mfg"),
];
/// `capabilities` bits: transports the device supports.
const TRANSPORTS: &[(u32, &str)] = &[(1 << 0, "uart"), (1 << 1, "wifi"), (1 << 2, "ble")];

fn names(bits: u64, table: &'static [(u32, &'static str)]) -> Vec<&'static str> {
    table
        .iter()
        .filter(|(bit, _)| bits & u64::from(*bit) != 0)
        .map(|(_, name)| *name)
        .collect()
}

/// Adds decoded `commands` and `transports` lists to a status object.
pub fn annotate(status: &mut serde_json::Value) {
    let availability = status["commandAvailability"].as_u64().unwrap_or(0);
    let capabilities = status["capabilities"].as_u64().unwrap_or(0);
    status["commands"] = serde_json::json!(names(availability, COMMANDS));
    status["transports"] = serde_json::json!(names(capabilities, TRANSPORTS));
}

pub fn allows(status: &serde_json::Value, command: &str) -> bool {
    status["commands"]
        .as_array()
        .is_some_and(|all| all.iter().any(|c| c == command))
}
pub const AVAILABLE: bool = cfg!(gizmoth_serial_core);

/// Human-readable Chinese text for Host Core result names.
pub fn describe(code: &str) -> String {
    match code {
        "timeout" => "未在超时内收到 H2Loader 握手响应".into(),
        "busy" => "串口被其他程序占用".into(),
        "not_found" => "找不到串口或文件".into(),
        "invalid_state" => "设备状态不匹配（板型、目标或身份校验失败）".into(),
        "format" | "truncated" => "固件包格式无效".into(),
        "closed" | "io" => "串口连接中断".into(),
        "unavailable" => "串口不可用".into(),
        "unsupported" => "设备不支持此操作".into(),
        "cancelled" => "已取消".into(),
        "core_unavailable" => "此版本未包含 H2Loader 内核".into(),
        other => format!("H2Loader 错误：{other}"),
    }
}
