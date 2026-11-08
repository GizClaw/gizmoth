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
        ICoreWebView2BrowserProcessExitedEventHandler, ICoreWebView2Environment,
        ICoreWebView2Environment5, ICoreWebView2Environment8,
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
    environment: ICoreWebView2Environment,
    subscription: Option<Subscription>,
    generation: u64,
    outcome: watch::Sender<Outcome>,
}
struct Subscription {
    environment: ICoreWebView2Environment5,
    _handler: ICoreWebView2BrowserProcessExitedEventHandler,
    token: i64,
}
impl Drop for Subscription {
    fn drop(&mut self) {
        // Created, retained and released only on the UI thread.
        unsafe {
            let _ = self.environment.remove_BrowserProcessExited(self.token);
        }
    }
}

pub struct TrackingError {
    pub message: String,
    pub environment_retained: bool,
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
    environment: &ICoreWebView2Environment,
    browser_pid: Option<u32>,
) -> Result<Arc<Vec<OwnedHandle>>, String> {
    let environment: ICoreWebView2Environment8 = environment.cast().map_err(|e| e.to_string())?;
    let processes = unsafe { environment.GetProcessInfos().map_err(|e| e.to_string())? };
    let mut count = 0;
    unsafe {
        processes.Count(&mut count).map_err(|e| e.to_string())?;
    }
    let mut pids: Vec<_> = browser_pid.into_iter().collect();
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

async fn capture_retained_processes(
    app: &tauri::AppHandle,
    label: &str,
    generation: u64,
) -> Result<Option<Arc<Vec<OwnedHandle>>>, String> {
    let label = label.to_string();
    let (send, receive) = oneshot::channel();
    app.run_on_main_thread(move || {
        let result = (|| -> Result<Option<Arc<Vec<OwnedHandle>>>, String> {
            let environment = ENVIRONMENTS.with(|all| {
                all.borrow()
                    .get(&label)
                    .filter(|entry| entry.generation == generation)
                    .map(|entry| entry.environment.clone())
            });
            let Some(environment) = environment else {
                return Ok(None);
            };
            let processes = capture_processes(&environment, None)?;
            let mut exits = EXITS.lock().map_err(|e| e.to_string())?;
            // The query can reenter COM and complete or supersede this watch.
            let Some(entry) = exits
                .get_mut(&label)
                .filter(|entry| entry.generation == generation)
            else {
                return Ok(None);
            };
            let pending = entry.receiver.borrow().is_none();
            if !pending {
                return Ok(None);
            }
            entry.processes = Some(processes.clone());
            Ok(Some(processes))
        })();
        let _ = send.send(result);
    })
    .map_err(|e| e.to_string())?;
    receive
        .await
        .map_err(|_| "无法确认模拟器存储进程。".to_string())?
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

// A failed controller getter must not revoke confirmed exit or discard the
// retained environment that can independently confirm a pending browser's exit.
fn reusable_close_exit(
    exit: Option<&BrowserExit>,
    retained_generation: Option<u64>,
) -> Option<BrowserExit> {
    exit.filter(|exit| match retained_generation {
        Some(generation) => generation == exit.generation,
        None => matches!(*exit.receiver.borrow(), Some(Ok(()))),
    })
    .cloned()
}

/// Acknowledge registration before destruction. Refresh on close to capture the
/// current browser generation; only Rust channels and owned OS handles cross the
/// UI-thread boundary.
pub async fn track(
    window: &tauri::WebviewWindow,
    close_controller: bool,
) -> Result<(), TrackingError> {
    #[cfg(feature = "native-e2e")]
    if !close_controller
        && std::env::var("GIZMOTH_E2E_FAIL_SIMULATOR_TRACKING").as_deref() == Ok("1")
    {
        // Exercise the gap after native window registration but before any
        // environment retention or exit watch exists. Shutdown must wait here.
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
    let label = window.label().to_string();
    let ui_label = label.clone();
    let generation = GENERATION.fetch_add(1, Ordering::Relaxed);
    let (outcome, exit) = watch::channel(None);
    let (armed, ready) = oneshot::channel();
    let queued = window.with_webview(move |native| {
        let mut retained = false;
        let result = (|| -> Result<BrowserExit, String> {
            // Tauri supplies the environment that created this controller. Keep
            // all environment and controller access within this UI callback.
            let environment = native.environment();
            let lookup_pid = || -> Result<u32, String> {
                #[cfg(feature = "native-e2e")]
                if close_controller && std::env::var("GIZMOTH_E2E_FAIL_SIMULATOR_CLOSE_PID").as_deref() == Ok("1") {
                    return Err("Injected close-time simulator PID failure".into());
                }
                let core = unsafe { native.controller().CoreWebView2().map_err(|e| e.to_string())? };
                let mut pid = 0;
                unsafe { core.BrowserProcessId(&mut pid).map_err(|e| e.to_string())?; }
                if pid == 0 { return Err("无法确认模拟器存储进程。".into()); }
                Ok(pid)
            };
            // Initial registration still retains the environment before invoking
            // fallible controller getters. At close, probe before replacing an
            // existing watch, since a dead controller may no longer answer.
            let close_pid = close_controller.then(lookup_pid);
            if matches!(close_pid, Some(Err(_))) {
                let retained_generation = ENVIRONMENTS.with(|all| {
                    all.borrow().get(&ui_label).map(|entry| entry.generation)
                });
                let previous = {
                    let exits = EXITS.lock().map_err(|e| e.to_string())?;
                    reusable_close_exit(exits.get(&ui_label), retained_generation)
                };
                if let Some(previous) = previous {
                    retained = true;
                    // Destruction follows track(). A failed Close on an already
                    // dead controller must not prevent it; storage still waits
                    // for the preserved event/independent exit confirmation.
                    unsafe { let _ = native.controller().Close(); }
                    return Ok(previous);
                }
            }
            let mut tracked = BrowserExit { receiver: exit, generation, processes: None };
            let mut exits = EXITS.lock().map_err(|e| e.to_string())?;
            let previous = ENVIRONMENTS.with(|all| {
                all.borrow_mut().insert(ui_label.clone(), Registration {
                    environment: environment.clone(),
                    subscription: None,
                    generation,
                    outcome: outcome.clone(),
                })
            });
            exits.insert(ui_label.clone(), tracked.clone());
            drop(exits);
            retained = true;
            drop(previous);
            #[cfg(feature = "native-e2e")]
            if !close_controller && std::env::var("GIZMOTH_E2E_FAIL_SIMULATOR_TRACKING").as_deref() == Ok("1") {
                return Err("Injected initial simulator tracking failure".into());
            }
            let expected_pid = match close_pid {
                Some(result) => result.ok(),
                None => Some(lookup_pid()?),
            };
            #[cfg(feature = "native-e2e")]
            eprintln!("simulator-storage track label={ui_label} pid={expected_pid:?} generation={generation} close={close_controller}");
            if close_controller {
                tracked.processes = Some(capture_processes(&environment, expected_pid)?);
                let mut exits = EXITS.lock().map_err(|e| e.to_string())?;
                let entry = exits.get_mut(&ui_label).filter(|entry| entry.generation == generation)
                    .ok_or("模拟器存储进程已改变，请重试。")?;
                entry.processes = tracked.processes.clone();
            }
            if let Some(expected_pid) = expected_pid {
                let callback_label = ui_label.clone();
                let handler = browser_exit_handler(expected_pid, outcome.clone(), move || {
                    #[cfg(feature = "native-e2e")]
                    eprintln!("simulator-storage exited label={callback_label} pid={expected_pid} generation={generation}");
                    release_environment(&callback_label, generation);
                });
                let events: ICoreWebView2Environment5 = environment.cast().map_err(|e| e.to_string())?;
                let mut token = 0_i64;
                unsafe { events.add_BrowserProcessExited(&handler, &mut token).map_err(|e| e.to_string())?; }
                let subscription = Subscription { environment: events, _handler: handler, token };
                ENVIRONMENTS.with(|all| -> Result<(), String> {
                    let mut all = all.borrow_mut();
                    let entry = all.get_mut(&ui_label).filter(|entry| entry.generation == generation)
                        .ok_or("模拟器存储进程已改变，请重试。")?;
                    entry.subscription = Some(subscription);
                    Ok(())
                })?;
            }
            if close_controller {
                #[cfg(feature = "native-e2e")]
                trace_processes(&ui_label, "before-close");
                let closed = unsafe { native.controller().Close() };
                if expected_pid.is_some() {
                    closed.map_err(|e| e.to_string())?;
                }
                // Without a controller PID, environment enumeration and pinned
                // handles confirm exit after destruction, even if Close failed.
            }
            Ok(tracked)
        })();
        // A setup error does not prove process exit. Its retained environment
        // stays pending so a later deletion/Quit/update can obtain fresh handles.
        let _ = armed.send((result, retained));
    });
    let (result, environment_retained) = match queued {
        Ok(()) => ready
            .await
            .unwrap_or_else(|_| (Err("无法确认模拟器存储进程。".to_string()), false)),
        Err(error) => (Err(error.to_string()), false),
    };
    match result {
        Ok(exit) => {
            if close_controller {
                let app = window.app_handle().clone();
                tauri::async_runtime::spawn(async move {
                    let _ = wait_for_tracked_exit(&app, &label, exit).await;
                });
            }
            Ok(())
        }
        Err(message) => Err(TrackingError {
            message,
            environment_retained,
        }),
    }
}

/// Failed-open destruction can happen after a Quit/update barrier has already
/// collected its closed views. Start confirmation immediately for that case.
pub fn monitor_exit(app: &tauri::AppHandle, label: &str) {
    let exit = EXITS.lock().ok().and_then(|all| all.get(label).cloned());
    if let Some(exit) = exit {
        let app = app.clone();
        let label = label.to_string();
        tauri::async_runtime::spawn(async move {
            let _ = wait_for_tracked_exit(&app, &label, exit).await;
        });
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
    let mut processes = exit.processes;
    let completed = async {
        loop {
            if let Some(result) = receiver.borrow().clone() {
                return result;
            }
            let current = EXITS
                .lock()
                .map_err(|e| e.to_string())?
                .get(label)
                .is_some_and(|entry| entry.generation == exit.generation);
            if !current {
                return Err("模拟器存储进程已改变，请重试。".into());
            }
            if processes.is_none() {
                processes = capture_retained_processes(app, label, exit.generation).await?;
            }
            if let Some(handles) = &processes {
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
                .filter(|(_, exit)| exit.receiver.borrow().is_none())
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
    fn failed_close_pid_preserves_confirmed_exit_without_an_environment() {
        let (_completed, receiver) = watch::channel(Some(Ok(())));
        let exit = BrowserExit {
            receiver,
            generation: 41,
            processes: None,
        };
        assert!(reusable_close_exit(Some(&exit), Some(42)).is_none());
        let recovered = reusable_close_exit(Some(&exit), None).unwrap();
        assert_eq!(recovered.generation, 41);
        assert!(matches!(*recovered.receiver.borrow(), Some(Ok(()))));
    }

    #[test]
    fn failed_close_pid_preserves_only_the_current_pending_watch() {
        let (completed, receiver) = watch::channel(None);
        let exit = BrowserExit {
            receiver,
            generation: 42,
            processes: Some(Arc::new(Vec::new())),
        };
        assert!(reusable_close_exit(None, Some(42)).is_none());
        assert!(reusable_close_exit(Some(&exit), None).is_none());
        assert!(reusable_close_exit(Some(&exit), Some(43)).is_none());
        let recovered = reusable_close_exit(Some(&exit), Some(42)).unwrap();
        assert_eq!(recovered.generation, 42);
        assert!(Arc::ptr_eq(
            recovered.processes.as_ref().unwrap(),
            exit.processes.as_ref().unwrap()
        ));
        // Recovery neither invents confirmation nor replaces the sender that
        // the retained event/independent environment probe will complete.
        assert!(recovered.receiver.borrow().is_none());
        completed.send_replace(Some(Ok(())));
        assert!(matches!(*recovered.receiver.borrow(), Some(Ok(()))));
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
