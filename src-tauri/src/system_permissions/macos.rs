//! UserNotifications and CoreBluetooth authorization, without a WebView or a
//! permission cache. All callback data is copied before crossing into Rust tasks.
use super::State;
use block2::{DynBlock, RcBlock};
use objc2::{
    define_class, msg_send,
    rc::Retained,
    runtime::{Bool, ProtocolObject},
    AnyThread,
};
use objc2_foundation::{NSBundle, NSError, NSObject, NSObjectProtocol, NSString};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNAuthorizationStatus, UNMutableNotificationContent, UNNotification,
    UNNotificationPresentationOptions, UNNotificationRequest, UNNotificationSettings,
    UNNotificationSound, UNUserNotificationCenter, UNUserNotificationCenterDelegate,
};
use std::{ptr::NonNull, sync::Once, time::Duration};
use tokio::sync::mpsc;

define_class!(
    // SAFETY: NSObject has no subclassing requirements; this class has no state
    // or Drop implementation, and the delegate signature matches the protocol.
    #[unsafe(super = NSObject)]
    #[name = "GizMothSystemNotificationDelegate"]
    struct Delegate;
    unsafe impl NSObjectProtocol for Delegate {}
    unsafe impl UNUserNotificationCenterDelegate for Delegate {
        #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
        fn will_present(
            &self,
            _center: &UNUserNotificationCenter,
            _notification: &UNNotification,
            completion: &DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
        ) {
            // Authorized foreground notifications should remain visible too.
            completion.call((UNNotificationPresentationOptions::Banner
                | UNNotificationPresentationOptions::List
                | UNNotificationPresentationOptions::Sound,));
        }
    }
);
fn center() -> Option<Retained<UNUserNotificationCenter>> {
    // currentNotificationCenter throws an Objective-C exception for an unbundled
    // `cargo run` executable. Do not impersonate Terminal or another installed app.
    NSBundle::mainBundle().bundleIdentifier()?;
    Some(UNUserNotificationCenter::currentNotificationCenter())
}
fn install_delegate(center: &UNUserNotificationCenter) {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        let delegate: Retained<Delegate> = unsafe { msg_send![Delegate::alloc(), init] };
        center.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        // UserNotifications retains its delegate weakly. Keep this single,
        // stateless delegate alive for the entire process, including callbacks
        // delivered while all app windows are hidden.
        let _ = Retained::into_raw(delegate);
    });
}
fn authorization(value: UNAuthorizationStatus) -> State {
    match value {
        UNAuthorizationStatus::NotDetermined => State::Prompt,
        UNAuthorizationStatus::Denied => State::Denied,
        UNAuthorizationStatus::Authorized
        | UNAuthorizationStatus::Provisional
        | UNAuthorizationStatus::Ephemeral => State::Granted,
        _ => State::Unknown,
    }
}
async fn receive<T>(
    receiver: &mut mpsc::UnboundedReceiver<T>,
    duration: Duration,
) -> Result<T, String> {
    tokio::time::timeout(duration, receiver.recv())
        .await
        .map_err(|_| "等待系统通知服务响应超时。".to_owned())?
        .ok_or_else(|| "系统通知服务未返回结果。".to_owned())
}
pub async fn notification_state() -> Result<State, String> {
    let (sender, mut receiver) = mpsc::unbounded_channel();
    {
        let Some(center) = center() else {
            return Ok(State::Unavailable);
        };
        let callback = RcBlock::new(move |settings: NonNull<UNNotificationSettings>| {
            // SAFETY: Apple keeps the settings alive throughout this callback.
            let state = authorization(unsafe { settings.as_ref() }.authorizationStatus());
            let _ = sender.send(state);
        });
        center.getNotificationSettingsWithCompletionHandler(&callback);
    }
    receive(&mut receiver, Duration::from_secs(10)).await
}
fn callback_error(error: *mut NSError) -> Result<(), String> {
    // SAFETY: an NSError supplied by the framework is valid for its callback.
    if let Some(error) = unsafe { error.as_ref() } {
        Err(format!(
            "系统通知操作失败：{}",
            error.localizedDescription()
        ))
    } else {
        Ok(())
    }
}
pub async fn request_notifications() -> Result<State, String> {
    let (sender, mut receiver) = mpsc::unbounded_channel();
    {
        let Some(center) = center() else {
            return Ok(State::Unavailable);
        };
        let callback = RcBlock::new(move |_granted: Bool, error: *mut NSError| {
            let _ = sender.send(callback_error(error));
        });
        center.requestAuthorizationWithOptions_completionHandler(
            UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound,
            &callback,
        );
    }
    receive(&mut receiver, Duration::from_secs(120)).await??;
    notification_state().await
}
pub fn bluetooth_state() -> State {
    use objc2_core_bluetooth::{CBManager, CBManagerAuthorization};
    // SAFETY: this class property only reads authorization; unlike creating a
    // CBCentralManager, it cannot display a prompt, scan, pair, or power a radio.
    match unsafe { CBManager::authorization_class() } {
        CBManagerAuthorization::NotDetermined => State::Prompt,
        CBManagerAuthorization::Restricted | CBManagerAuthorization::Denied => State::Denied,
        CBManagerAuthorization::AllowedAlways => State::Granted,
        _ => State::Unknown,
    }
}
pub async fn send(title: &str, body: &str) -> Result<(), String> {
    let (sender, mut receiver) = mpsc::unbounded_channel();
    {
        let center = center().ok_or("请从已安装的 GizMoth 应用发送系统通知。")?;
        install_delegate(&center);
        let content = UNMutableNotificationContent::new();
        content.setTitle(&NSString::from_str(title));
        content.setBody(&NSString::from_str(body));
        content.setSound(Some(&UNNotificationSound::defaultSound()));
        let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
            &NSString::from_str(&uuid::Uuid::new_v4().to_string()),
            &content,
            None,
        );
        let callback = RcBlock::new(move |error: *mut NSError| {
            let _ = sender.send(callback_error(error));
        });
        center.addNotificationRequest_withCompletionHandler(&request, Some(&callback));
    }
    // Propagate the OS submission error rather than returning after dispatch.
    // Acceptance cannot prove presentation: Focus and user banner settings win.
    receive(&mut receiver, Duration::from_secs(10)).await?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn authorization_mapping_includes_not_determined_and_provisional() {
        assert_eq!(
            authorization(UNAuthorizationStatus::NotDetermined),
            State::Prompt
        );
        assert_eq!(authorization(UNAuthorizationStatus::Denied), State::Denied);
        assert_eq!(
            authorization(UNAuthorizationStatus::Authorized),
            State::Granted
        );
        assert_eq!(
            authorization(UNAuthorizationStatus::Provisional),
            State::Granted
        );
        assert_eq!(authorization(UNAuthorizationStatus(99)), State::Unknown);
    }
}
