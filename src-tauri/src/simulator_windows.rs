//! WebView2 storage may outlive its native window. Retain its environment on
//! the UI thread until BrowserProcessExited confirms resource release.
use std::{
    cell::RefCell,
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        LazyLock, Mutex,
    },
    time::Duration,
};
use tokio::sync::{oneshot, watch};
use webview2_com::{
    BrowserProcessExitedEventHandler, Microsoft::Web::WebView2::Win32::ICoreWebView2Environment5,
};
use windows_core::Interface;

type Outcome = Option<Result<(), String>>;
#[derive(Clone)]
struct BrowserExit {
    receiver: watch::Receiver<Outcome>,
    generation: u64,
}
static EXITS: LazyLock<Mutex<HashMap<String, BrowserExit>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static GENERATION: AtomicU64 = AtomicU64::new(0);
thread_local! {
    static ENVIRONMENTS: RefCell<HashMap<String, Registration>> = RefCell::new(HashMap::new());
}
struct Registration {
    environment: ICoreWebView2Environment5,
    token: i64,
    generation: u64,
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
}

/// Acknowledge registration before destruction. Refresh on close to capture the
/// current browser generation; only Rust channels cross the UI-thread boundary.
pub async fn track(window: &tauri::WebviewWindow) -> Result<(), String> {
    let label = window.label().to_string();
    let ui_label = label.clone();
    let generation = GENERATION.fetch_add(1, Ordering::Relaxed);
    let (outcome, exit) = watch::channel(None);
    let failed_exit = exit.clone();
    let (armed, ready) = oneshot::channel();
    let queued = window.with_webview(move |native| {
        let result = (|| -> Result<(), String> {
            let environment: ICoreWebView2Environment5 =
                native.environment().cast().map_err(|e| e.to_string())?;
            let mut expected_pid = 0;
            unsafe {
                native
                    .controller()
                    .CoreWebView2()
                    .map_err(|e| e.to_string())?
                    .BrowserProcessId(&mut expected_pid)
                    .map_err(|e| e.to_string())?;
            }
            if expected_pid == 0 {
                return Err("无法确认模拟器存储进程。".into());
            }
            let callback_label = ui_label.clone();
            let completed = outcome.clone();
            let handler = BrowserProcessExitedEventHandler::create(Box::new(move |_, args| {
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
                let _ = completed.send_replace(Some(result));
                release_environment(&callback_label, generation);
                Ok(())
            }));
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
                        token,
                        generation,
                    },
                )
            });
            // Publish alongside registration on the same UI thread. Concurrent
            // close requests must not publish an older receiver after a newer one.
            EXITS.lock().map_err(|e| e.to_string())?.insert(
                ui_label,
                BrowserExit {
                    receiver: exit,
                    generation,
                },
            );
            drop(previous);
            Ok(())
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
    if result.is_err() {
        EXITS
            .lock()
            .map_err(|e| e.to_string())?
            .entry(label)
            .or_insert(BrowserExit {
                receiver: failed_exit,
                generation,
            });
    }
    result
}

pub async fn wait_for_exit(label: &str) -> Result<(), String> {
    let receiver = EXITS.lock().map_err(|e| e.to_string())?.get(label).cloned();
    let Some(exit) = receiver else {
        return Ok(());
    };
    let mut receiver = exit.receiver;
    let completed = async {
        loop {
            if let Some(result) = receiver.borrow().clone() {
                return result;
            }
            receiver
                .changed()
                .await
                .map_err(|_| "无法确认模拟器存储进程。".to_string())?;
        }
    };
    tokio::time::timeout(Duration::from_secs(30), completed)
        .await
        .map_err(|_| "等待模拟器存储退出超时，请重试删除。".to_string())?
}

pub fn idle() -> bool {
    EXITS.lock().is_ok_and(|all| {
        all.values()
            .all(|exit| exit.receiver.borrow().is_some() || exit.receiver.has_changed().is_err())
    })
}

pub fn forget(app: &tauri::AppHandle, label: &str) {
    let old = EXITS.lock().ok().and_then(|mut all| all.remove(label));
    if let Some(old) = old {
        let label = label.to_string();
        let _ = app.run_on_main_thread(move || release_environment(&label, old.generation));
    }
}
