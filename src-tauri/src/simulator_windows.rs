//! WebView2 storage may outlive its native window. Retain its environment on
//! the UI thread until its exit event or independent process checks confirm release.
use std::{
    cell::RefCell,
    collections::HashMap,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, LazyLock, Mutex,
    },
    time::Duration,
};
use tauri::Manager;
use tokio::sync::{oneshot, watch};
#[cfg(feature = "native-e2e")]
use webview2_com::Microsoft::Web::WebView2::Win32::COREWEBVIEW2_PROCESS_KIND;
use webview2_com::{
    BrowserProcessExitedEventHandler,
    Microsoft::Web::WebView2::Win32::{
        ICoreWebView2BrowserProcessExitedEventHandler, ICoreWebView2Environment5,
        ICoreWebView2Environment8, ICoreWebView2_2,
    },
};
use windows::Win32::{
    Foundation::{ERROR_INVALID_PARAMETER, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT},
    System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE},
};
use windows_core::Interface;

type Outcome = Option<Result<(), String>>;
#[derive(Clone)]
struct BrowserExit {
    receiver: watch::Receiver<Outcome>,
    generation: u64,
    processes: Option<Arc<Vec<OwnedHandle>>>,
}
static EXITS: LazyLock<Mutex<HashMap<String, BrowserExit>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static GENERATION: AtomicU64 = AtomicU64::new(0);
thread_local! {
    static ENVIRONMENTS: RefCell<HashMap<String, Registration>> = RefCell::new(HashMap::new());
}
struct Registration {
    environment: ICoreWebView2Environment5,
    _handler: ICoreWebView2BrowserProcessExitedEventHandler,
    token: i64,
    generation: u64,
    outcome: watch::Sender<Outcome>,
}
impl Drop for Registration {
    fn drop(&mut self) {
        // Created, retained and released only on the UI thread.
        unsafe {
            let _ = self.environment.remove_BrowserProcessExited(self.token);
        }
    }
}
fn release_environment(label: &str, generation: u64) {
    let old = ENVIRONMENTS.with(|all| {
        let mut all = all.borrow_mut();
        if all
            .get(label)
            .is_some_and(|entry| entry.generation == generation)
        {
            all.remove(label)
        } else {
            None
        }
    });
    drop(old);
    clear_completed_handles(label, generation);
}

fn clear_completed_handles(label: &str, generation: u64) {
    if let Ok(mut all) = EXITS.lock() {
        if let Some(entry) = all
            .get_mut(label)
            .filter(|entry| entry.generation == generation)
        {
            let completed = matches!(*entry.receiver.borrow(), Some(Ok(())));
            if completed {
                entry.processes = None;
            }
        }
    }
}

#[cfg(feature = "native-e2e")]
fn trace_processes(label: &str, phase: &str) {
    let current = ENVIRONMENTS.with(|all| {
        all.borrow()
            .get(label)
            .map(|entry| (entry.environment.clone(), entry.generation))
    });
    let Some((environment, generation)) = current else {
        eprintln!("simulator-storage processes label={label} phase={phase} registration=none");
        return;
    };
    let result = (|| -> Result<Vec<(i32, i32)>, String> {
        let environment: ICoreWebView2Environment8 =
            environment.cast().map_err(|e| e.to_string())?;
        let processes = unsafe { environment.GetProcessInfos().map_err(|e| e.to_string())? };
        let mut count = 0;
        unsafe {
            processes.Count(&mut count).map_err(|e| e.to_string())?;
        }
        let mut snapshot = Vec::new();
        for index in 0..count {
            let process = unsafe {
                processes
                    .GetValueAtIndex(index)
                    .map_err(|e| e.to_string())?
            };
            let mut pid = 0;
            let mut kind = COREWEBVIEW2_PROCESS_KIND(0);
            unsafe {
                process.ProcessId(&mut pid).map_err(|e| e.to_string())?;
                process.Kind(&mut kind).map_err(|e| e.to_string())?;
            }
            snapshot.push((pid, kind.0));
        }
        Ok(snapshot)
    })();
    eprintln!("simulator-storage processes label={label} phase={phase} generation={generation} snapshot={result:?}");
}

fn open_process(pid: u32) -> Result<Option<OwnedHandle>, String> {
    // Synchronization access is sufficient. Holding a handle pins this process
    // object, so PID reuse cannot make an exited browser appear to be a new one.
    match unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) } {
        Ok(handle) => Ok(Some(unsafe { OwnedHandle::from_raw_handle(handle.0) })),
        // A process from the environment snapshot can exit before OpenProcess.
        Err(error)
            if error.code() == windows_core::HRESULT::from_win32(ERROR_INVALID_PARAMETER.0) =>
        {
            Ok(None)
        }
        Err(error) => Err(error.to_string()),
    }
}

fn capture_processes(
    environment: &ICoreWebView2Environment5,
    browser_pid: u32,
) -> Result<Arc<Vec<OwnedHandle>>, String> {
    let environment: ICoreWebView2Environment8 = environment.cast().map_err(|e| e.to_string())?;
    let processes = unsafe { environment.GetProcessInfos().map_err(|e| e.to_string())? };
    let mut count = 0;
    unsafe {
        processes.Count(&mut count).map_err(|e| e.to_string())?;
    }
    let mut pids = vec![browser_pid];
    for index in 0..count {
        let process = unsafe {
            processes
                .GetValueAtIndex(index)
                .map_err(|e| e.to_string())?
        };
        let mut pid = 0;
        unsafe {
            process.ProcessId(&mut pid).map_err(|e| e.to_string())?;
        }
        if pid <= 0 {
            return Err("无法确认模拟器存储进程。".into());
        }
        pids.push(pid as u32);
    }
    pids.sort_unstable();
    pids.dedup();
    let mut handles = Vec::new();
    for pid in pids {
        if let Some(handle) = open_process(pid)? {
            handles.push(handle);
        }
    }
    Ok(Arc::new(handles))
}

fn processes_exited(handles: &[OwnedHandle]) -> Result<bool, String> {
    for handle in handles {
        match unsafe { WaitForSingleObject(HANDLE(handle.as_raw_handle()), 0) } {
            WAIT_OBJECT_0 => {}
            WAIT_TIMEOUT => return Ok(false),
            _ => return Err(windows_core::Error::from_thread().to_string()),
        }
    }
    Ok(true)
}

async fn confirm_empty_environment(
    app: &tauri::AppHandle,
    label: &str,
    generation: u64,
) -> Result<bool, String> {
    let label = label.to_string();
    let (send, receive) = oneshot::channel();
    app.run_on_main_thread(move || {
        let result = (|| -> Result<bool, String> {
            let current = ENVIRONMENTS.with(|all| {
                all.borrow().get(&label)
                    .filter(|entry| entry.generation == generation)
                    .map(|entry| (entry.environment.clone(), entry.outcome.clone()))
            });
            let Some((environment, outcome)) = current else { return Ok(false); };
            let environment: ICoreWebView2Environment8 = environment.cast().map_err(|e| e.to_string())?;
            let processes = unsafe { environment.GetProcessInfos().map_err(|e| e.to_string())? };
            let mut count = 0;
            unsafe { processes.Count(&mut count).map_err(|e| e.to_string())?; }
            if count != 0 { return Ok(false); }
            // COM can reenter the UI loop. Check the generation again after the
            // query so an old monitor cannot complete a recreated window's watch.
            let current = ENVIRONMENTS.with(|all| {
                all.borrow().get(&label).is_some_and(|entry| entry.generation == generation)
            });
            if !current { return Ok(false); }
            if let Some(result) = outcome.borrow().as_ref() { return Ok(result.is_ok()); }
            #[cfg(feature = "native-e2e")]
            eprintln!("simulator-storage independently-exited label={label} generation={generation} processes=0");
            outcome.send_replace(Some(Ok(())));
            release_environment(&label, generation);
            Ok(true)
        })();
        let _ = send.send(result);
    }).map_err(|e| e.to_string())?;
    receive
        .await
        .map_err(|_| "无法确认模拟器存储进程。".to_string())?
}

fn browser_exit_handler(
    expected_pid: u32,
    completed: watch::Sender<Outcome>,
    on_confirmed: impl Fn() + 'static,
) -> ICoreWebView2BrowserProcessExitedEventHandler {
    BrowserProcessExitedEventHandler::create(Box::new(move |_, args| {
        let result = (|| -> Result<(), String> {
            let args = args.ok_or("无法确认模拟器存储进程。")?;
            let mut exited_pid = 0;
            unsafe {
                args.BrowserProcessId(&mut exited_pid)
                    .map_err(|e| e.to_string())?;
            }
            if exited_pid != expected_pid {
                return Err("模拟器存储进程已改变，请重试。".into());
            }
            Ok(())
        })();
        if result.is_ok() {
            completed.send_replace(Some(Ok(())));
            on_confirmed();
        } else {
            // An unreadable or unrelated notification does not prove exit.
            // Keep the watch pending and retain its environment/handles so a
            // later event or independent confirmation can still complete it.
            #[cfg(feature = "native-e2e")]
            eprintln!(
                "simulator-storage inconclusive-exit expected={expected_pid} result={result:?}"
            );
        }
        Ok(())
    }))
}

/// Acknowledge registration before destruction. Refresh on close to capture the
/// current browser generation; only Rust channels and owned OS handles cross the
/// UI-thread boundary.
pub async fn track(window: &tauri::WebviewWindow, close_controller: bool) -> Result<(), String> {
    let label = window.label().to_string();
    let ui_label = label.clone();
    let generation = GENERATION.fetch_add(1, Ordering::Relaxed);
    let (outcome, exit) = watch::channel(None);
    let failed_exit = exit.clone();
    let (armed, ready) = oneshot::channel();
    let queued = window.with_webview(move |native| {
        let result = (|| -> Result<BrowserExit, String> {
            let core: ICoreWebView2_2 = unsafe { native.controller().CoreWebView2().map_err(|e| e.to_string())? }
                .cast().map_err(|e| e.to_string())?;
            let environment: ICoreWebView2Environment5 = unsafe { core.Environment().map_err(|e| e.to_string())? }
                .cast().map_err(|e| e.to_string())?;
            let mut expected_pid = 0;
            unsafe { core.BrowserProcessId(&mut expected_pid).map_err(|e| e.to_string())?; }
            if expected_pid == 0 {
                return Err("无法确认模拟器存储进程。".into());
            }
            #[cfg(feature = "native-e2e")]
            eprintln!("simulator-storage track label={ui_label} pid={expected_pid} generation={generation} close={close_controller}");
            let processes = if close_controller {
                Some(capture_processes(&environment, expected_pid)?)
            } else {
                None
            };
            let callback_label = ui_label.clone();
            let handler = browser_exit_handler(expected_pid, outcome.clone(), move || {
                #[cfg(feature = "native-e2e")]
                eprintln!("simulator-storage exited label={callback_label} pid={expected_pid} generation={generation}");
                release_environment(&callback_label, generation);
            });
            let mut token = 0_i64;
            unsafe {
                environment
                    .add_BrowserProcessExited(&handler, &mut token)
                    .map_err(|e| e.to_string())?;
            }
            let previous = ENVIRONMENTS.with(|all| {
                all.borrow_mut().insert(
                    ui_label.clone(),
                    Registration {
                        environment,
                        _handler: handler,
                        token,
                        generation,
                        outcome: outcome.clone(),
                    },
                )
            });
            // Publish alongside registration on the same UI thread. Concurrent
            // close requests must not publish an older receiver after a newer one.
            let tracked = BrowserExit { receiver: exit, generation, processes };
            EXITS.lock().map_err(|e| e.to_string())?.insert(ui_label.clone(), tracked.clone());
            drop(previous);
            if close_controller {
                #[cfg(feature = "native-e2e")]
                trace_processes(&ui_label, "before-close");
                // Close explicitly: retained framework references can delay the
                // WebView destructor even after its native window is destroyed.
                unsafe { native.controller().Close().map_err(|e| e.to_string())?; }
            }
            Ok(tracked)
        })();
        if let Err(error) = &result {
            let _ = outcome.send_replace(Some(Err(error.clone())));
        }
        let _ = armed.send(result);
    });
    let result = match queued {
        Ok(()) => ready
            .await
            .unwrap_or_else(|_| Err("无法确认模拟器存储进程。".to_string())),
        Err(error) => Err(error.to_string()),
    };
    match result {
        Ok(exit) => {
            if close_controller {
                // Normal Close, Quit and updater preparation need the same proof
                // even when no deletion command is waiting for this window.
                let app = window.app_handle().clone();
                tauri::async_runtime::spawn(async move {
                    let _ = wait_for_tracked_exit(&app, &label, exit).await;
                });
            }
            Ok(())
        }
        Err(error) => {
            EXITS
                .lock()
                .map_err(|e| e.to_string())?
                .entry(label)
                .or_insert(BrowserExit {
                    receiver: failed_exit,
                    generation,
                    processes: None,
                });
            Err(error)
        }
    }
}

pub async fn wait_for_exit(app: &tauri::AppHandle, label: &str) -> Result<(), String> {
    let exit = EXITS.lock().map_err(|e| e.to_string())?.get(label).cloned();
    match exit {
        Some(exit) => wait_for_tracked_exit(app, label, exit).await,
        None => Ok(()),
    }
}

async fn wait_for_tracked_exit(
    app: &tauri::AppHandle,
    label: &str,
    exit: BrowserExit,
) -> Result<(), String> {
    let mut receiver = exit.receiver;
    let completed = async {
        loop {
            if let Some(result) = receiver.borrow().clone() {
                return result;
            }
            if let Some(handles) = &exit.processes {
                let current = EXITS
                    .lock()
                    .map_err(|e| e.to_string())?
                    .get(label)
                    .is_some_and(|entry| entry.generation == exit.generation);
                if !current {
                    return Err("模拟器存储进程已改变，请重试。".into());
                }
                if processes_exited(handles)?
                    && confirm_empty_environment(app, label, exit.generation).await?
                {
                    return Ok(());
                }
            }
            tokio::select! {
                changed = receiver.changed() => {
                    changed.map_err(|_| "无法确认模拟器存储进程。".to_string())?;
                }
                _ = tokio::time::sleep(Duration::from_millis(100)) => {}
            }
        }
    };
    let result = tokio::time::timeout(Duration::from_secs(30), completed).await;
    #[cfg(feature = "native-e2e")]
    if result.is_err() {
        let label = label.to_string();
        eprintln!("simulator-storage requesting UI process snapshot label={label}");
        let _ = app.run_on_main_thread(move || trace_processes(&label, "exit-timeout"));
    }
    let result = result.map_err(|_| "等待模拟器存储退出超时，请重试删除。".to_string())?;
    if result.is_ok() {
        clear_completed_handles(label, exit.generation);
    }
    result
}

/// A bounded normal-close monitor may time out before a slow process exits.
/// Each new Quit/update attempt restarts confirmation for already-closed views.
pub fn retry_closed(app: &tauri::AppHandle) {
    let pending: Vec<_> = EXITS
        .lock()
        .map(|all| {
            all.iter()
                .filter(|(_, exit)| exit.processes.is_some() && exit.receiver.borrow().is_none())
                .map(|(label, exit)| (label.clone(), exit.clone()))
                .collect()
        })
        .unwrap_or_default();
    for (label, exit) in pending {
        if app.get_webview_window(&label).is_none() {
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                let _ = wait_for_tracked_exit(&app, &label, exit).await;
            });
        }
    }
}

pub fn idle() -> bool {
    EXITS.lock().is_ok_and(|all| {
        all.values()
            .all(|exit| matches!(*exit.receiver.borrow(), Some(Ok(()))))
    })
}

pub fn forget(app: &tauri::AppHandle, label: &str) {
    let old = EXITS.lock().ok().and_then(|mut all| all.remove(label));
    if let Some(old) = old {
        let label = label.to_string();
        let _ = app.run_on_main_thread(move || release_environment(&label, old.generation));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        process::{Command, Stdio},
        sync::atomic::AtomicUsize,
    };
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        ICoreWebView2BrowserProcessExitedEventArgs,
        ICoreWebView2BrowserProcessExitedEventArgs_Impl, ICoreWebView2Environment,
        COREWEBVIEW2_BROWSER_PROCESS_EXIT_KIND,
    };
    use windows::Win32::Foundation::{E_FAIL, E_NOTIMPL};

    #[windows_core::implement(ICoreWebView2BrowserProcessExitedEventArgs)]
    struct ExitArgs {
        pid: u32,
        readable: bool,
    }
    impl ICoreWebView2BrowserProcessExitedEventArgs_Impl for ExitArgs_Impl {
        fn BrowserProcessExitKind(
            &self,
            _: *mut COREWEBVIEW2_BROWSER_PROCESS_EXIT_KIND,
        ) -> windows_core::Result<()> {
            Err(E_NOTIMPL.into())
        }
        fn BrowserProcessId(&self, value: *mut u32) -> windows_core::Result<()> {
            if !self.readable {
                return Err(E_FAIL.into());
            }
            unsafe {
                value.write(self.pid);
            }
            Ok(())
        }
    }

    #[test]
    fn inconclusive_native_notifications_keep_tracking_recoverable() {
        let (completed, exit) = watch::channel(None);
        let releases = Arc::new(AtomicUsize::new(0));
        let released = releases.clone();
        let handler = browser_exit_handler(42, completed, move || {
            released.fetch_add(1, Ordering::Relaxed);
        });
        let unrelated: ICoreWebView2BrowserProcessExitedEventArgs = ExitArgs {
            pid: 99,
            readable: true,
        }
        .into();
        let unreadable: ICoreWebView2BrowserProcessExitedEventArgs = ExitArgs {
            pid: 42,
            readable: false,
        }
        .into();
        // Invoke the real COM callback boundary with missing arguments, a PID
        // mismatch, and a failed native getter. None may discard the environment
        // or prevent a later notification/independent probe from completing.
        for args in [None, Some(unrelated), Some(unreadable)] {
            unsafe { handler.Invoke(None::<&ICoreWebView2Environment>, args.as_ref()) }.unwrap();
            assert!(exit.borrow().is_none());
            assert_eq!(releases.load(Ordering::Relaxed), 0);
        }
        let confirmed: ICoreWebView2BrowserProcessExitedEventArgs = ExitArgs {
            pid: 42,
            readable: true,
        }
        .into();
        unsafe { handler.Invoke(None::<&ICoreWebView2Environment>, &confirmed) }.unwrap();
        assert!(matches!(*exit.borrow(), Some(Ok(()))));
        assert_eq!(releases.load(Ordering::Relaxed), 1);
        // A late inconclusive event must not revoke an already-confirmed exit.
        unsafe {
            handler.Invoke(
                None::<&ICoreWebView2Environment>,
                None::<&ICoreWebView2BrowserProcessExitedEventArgs>,
            )
        }
        .unwrap();
        assert!(matches!(*exit.borrow(), Some(Ok(()))));
        assert_eq!(releases.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn pinned_process_handle_confirms_exit_without_a_browser_event() {
        // The child stays alive until its input closes. Capture its real process
        // object before exit, then verify the same handle changes to signaled.
        let mut child = Command::new("cmd")
            .args(["/D", "/C", "more >NUL"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let handles = vec![open_process(child.id()).unwrap().unwrap()];
        let running = processes_exited(&handles);
        drop(child.stdin.take());
        let status = child.wait().unwrap();
        assert_eq!(running, Ok(false));
        assert!(status.success());
        assert_eq!(processes_exited(&handles), Ok(true));
    }
}
