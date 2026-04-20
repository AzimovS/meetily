//! macOS shim over `UNUserNotificationCenter` (from `UserNotifications.framework`).
//!
//! Replaces the deprecated `NSUserNotification` path that `tauri-plugin-notification` →
//! `notify-rust` → `mac-notification-sys` still uses. On modern macOS, `NSUserNotification`
//! delivers to Notification Center but no banner fires. The modern UN API does.
//!
//! See `docs/plans/2026-04-20-fix-macos-notification-banner-delivery-plan.md`.

use anyhow::{anyhow, Result};
use block2::RcBlock;
use log::{error as log_error, info as log_info};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{define_class, msg_send, AllocAnyThread};
use objc2_foundation::{NSError, NSObject, NSObjectProtocol, NSString};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNMutableNotificationContent, UNNotification,
    UNNotificationInterruptionLevel, UNNotificationPresentationOptions, UNNotificationRequest,
    UNNotificationSound, UNUserNotificationCenter, UNUserNotificationCenterDelegate,
};
use once_cell::sync::OnceCell;
use std::sync::Mutex;
use tokio::sync::oneshot;
use uuid::Uuid;

use crate::notifications::types::{Notification, NotificationPriority};

define_class!(
    // SAFETY: `NSObject` is a valid Objective-C superclass with no subclassing constraints
    // (no required inits, no `dealloc` hooks to preserve). We add no ivars (unit type) and
    // implement no `Drop`, so objc2's generated `dealloc` forwards cleanly to `[super dealloc]`.
    // The delegate is retained for app lifetime in `DELEGATE_CELL`, so `&self` in the methods
    // below always points at a live instance.
    #[unsafe(super = NSObject)]
    #[ivars = ()]
    struct BannerDelegate;

    // SAFETY: `NSObjectProtocol` requires `-isEqual:`, `-hash`, etc. which NSObject already
    // provides; we override none of them, so the default implementations are correct.
    unsafe impl NSObjectProtocol for BannerDelegate {}

    // SAFETY: `UNUserNotificationCenterDelegate` is a pure-optional protocol; conforming
    // without implementing every method is permitted by Apple's API. The one method we do
    // implement below matches its declared selector and Objective-C signature exactly.
    unsafe impl UNUserNotificationCenterDelegate for BannerDelegate {
        // SAFETY: Selector `userNotificationCenter:willPresentNotification:withCompletionHandler:`
        // matches this Rust fn's argument ABI one-for-one:
        //   (id, SEL, UNUserNotificationCenter *, UNNotification *,
        //    void(^)(UNNotificationPresentationOptions))
        // The `completion_handler` block must be invoked exactly once (Apple's contract) — we
        // invoke it synchronously below with the presentation options that tell macOS to show
        // a top-right banner even when our app is frontmost. Without this, foreground-app
        // notifications land in Notification Center silently.
        #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
        fn will_present_notification(
            &self,
            _center: &UNUserNotificationCenter,
            _notification: &UNNotification,
            completion_handler: &block2::DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
        ) {
            let options = UNNotificationPresentationOptions::Banner
                | UNNotificationPresentationOptions::List
                | UNNotificationPresentationOptions::Sound;
            completion_handler.call((options,));
        }
    }
);

impl BannerDelegate {
    fn new() -> Retained<Self> {
        let this = Self::alloc().set_ivars(());
        // SAFETY: `NSObject`'s `-init` is infallible and returns an owned `Retained<Self>`.
        // `alloc().set_ivars(())` has produced a freshly allocated instance with zero ivars,
        // which is exactly what the objc2 `define_class!` contract expects `init` to receive.
        unsafe { msg_send![super(this), init] }
    }
}

// Retain the delegate for the lifetime of the app — `setDelegate:` is a weak property,
// so dropping this would silently unwire our willPresent hook.
static DELEGATE_CELL: OnceCell<Retained<BannerDelegate>> = OnceCell::new();

fn install_delegate_if_needed() {
    DELEGATE_CELL.get_or_init(|| {
        let delegate = BannerDelegate::new();
        let center = UNUserNotificationCenter::currentNotificationCenter();
        center.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        log_info!(
            "Installed UNUserNotificationCenterDelegate (willPresent → Banner|List|Sound)"
        );
        delegate
    });
}

/// Request notification authorization. Idempotent; if the user has already granted (or denied),
/// macOS returns the stored decision without re-prompting.
pub async fn request_authorization() -> Result<bool> {
    install_delegate_if_needed();

    // Scope the ObjC handles (not Send) so they drop before we await.
    let (tx, rx) = oneshot::channel::<Result<bool>>();
    {
        let tx_slot: Mutex<Option<oneshot::Sender<Result<bool>>>> = Mutex::new(Some(tx));

        let block = RcBlock::new(move |granted: objc2::runtime::Bool, error: *mut NSError| {
            let result = if !error.is_null() {
                // SAFETY: `error` originates from UN's completion handler and is either null
            // (checked above) or points to a valid `NSError` for the duration of this
            // callback. `error_message` only dereferences when non-null.
            let msg = unsafe { error_message(error) };
                log_error!("UN requestAuthorization error: {}", msg);
                Err(anyhow!("UN requestAuthorization error: {}", msg))
            } else {
                Ok(granted.as_bool())
            };
            if let Some(tx) = tx_slot.lock().ok().and_then(|mut guard| guard.take()) {
                let _ = tx.send(result);
            }
        });

        let opts = UNAuthorizationOptions::Alert
            | UNAuthorizationOptions::Sound
            | UNAuthorizationOptions::Badge;
        let center = UNUserNotificationCenter::currentNotificationCenter();
        center.requestAuthorizationWithOptions_completionHandler(opts, &block);
    }

    match rx.await {
        Ok(r) => r,
        Err(_) => Err(anyhow!("UN authorization completion dropped")),
    }
}

/// Present a notification via `UNUserNotificationCenter`.
pub async fn show(notification: &Notification) -> Result<()> {
    install_delegate_if_needed();

    let id_str = notification
        .id
        .clone()
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let level = interruption_level(&notification.priority);
    log_info!(
        "UN present: id={} title={:?} level={:?}",
        id_str,
        notification.title,
        level
    );

    // Scope the ObjC handles (not Send) so they drop before we await.
    let (tx, rx) = oneshot::channel::<Result<()>>();
    {
        let content = UNMutableNotificationContent::new();
        content.setTitle(&NSString::from_str(&notification.title));
        content.setBody(&NSString::from_str(&notification.body));
        if notification.sound {
            let sound = UNNotificationSound::defaultSound();
            content.setSound(Some(&sound));
        }
        content.setInterruptionLevel(level);

        let id_ns = NSString::from_str(&id_str);
        let request =
            UNNotificationRequest::requestWithIdentifier_content_trigger(&id_ns, &content, None);

        let tx_slot: Mutex<Option<oneshot::Sender<Result<()>>>> = Mutex::new(Some(tx));
        let block = RcBlock::new(move |error: *mut NSError| {
            let result = if error.is_null() {
                Ok(())
            } else {
                // SAFETY: `error` originates from UN's completion handler and is either null
            // (checked above) or points to a valid `NSError` for the duration of this
            // callback. `error_message` only dereferences when non-null.
            let msg = unsafe { error_message(error) };
                Err(anyhow!("UN addNotificationRequest failed: {}", msg))
            };
            if let Some(tx) = tx_slot.lock().ok().and_then(|mut guard| guard.take()) {
                let _ = tx.send(result);
            }
        });

        let center = UNUserNotificationCenter::currentNotificationCenter();
        center.addNotificationRequest_withCompletionHandler(&request, Some(&block));
    }

    match rx.await {
        Ok(r) => r,
        Err(_) => Err(anyhow!("UN addNotificationRequest completion dropped")),
    }
}

/// Map our `NotificationPriority` to `UNNotificationInterruptionLevel`.
///
/// `Critical` maps to `TimeSensitive`, not `Critical`: the real Critical level requires Apple's
/// Critical Alerts entitlement, which our ad-hoc-signed build does not have. Requesting it would
/// fail silently at the OS layer.
fn interruption_level(priority: &NotificationPriority) -> UNNotificationInterruptionLevel {
    match priority {
        NotificationPriority::Low => UNNotificationInterruptionLevel::Passive,
        NotificationPriority::Normal => UNNotificationInterruptionLevel::Active,
        NotificationPriority::High => UNNotificationInterruptionLevel::TimeSensitive,
        NotificationPriority::Critical => UNNotificationInterruptionLevel::TimeSensitive,
    }
}

/// Extract a human-readable string from an `NSError` pointer supplied by an Objective-C
/// completion handler.
///
/// # Safety
///
/// If `error` is non-null, it must point to a live `NSError` that remains valid for the
/// duration of this call. UN framework completion handlers uphold this contract: the
/// `NSError` is alive for the callback scope. Passing a dangling or uninitialized pointer
/// is undefined behavior.
unsafe fn error_message(error: *mut NSError) -> String {
    if error.is_null() {
        return String::from("<null NSError>");
    }
    // SAFETY: null case handled above; caller upholds that `error` is a live NSError.
    let err: &NSError = unsafe { &*error };
    err.localizedDescription().to_string()
}
