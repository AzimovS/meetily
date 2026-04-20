use crate::notifications::types::{Notification, NotificationPriority, NotificationTimeout};
use anyhow::Result;
#[cfg(not(target_os = "macos"))]
use anyhow::anyhow;
use log::{info as log_info, error as log_error};
use tauri::{AppHandle, Runtime};
#[cfg(not(target_os = "macos"))]
use tauri_plugin_notification::NotificationExt;
use std::time::Duration;

/// Cross-platform system notification handler.
///
/// On macOS, delegates to `macos_un` (UNUserNotificationCenter) and never touches `app_handle`.
/// On Windows/Linux, routes through `tauri-plugin-notification`.
pub struct SystemNotificationHandler<R: Runtime> {
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    app_handle: AppHandle<R>,
}

impl<R: Runtime> SystemNotificationHandler<R> {
    pub fn new(app_handle: AppHandle<R>) -> Self {
        Self {
            app_handle,
        }
    }

    /// Show a notification.
    ///
    /// On macOS, routes through `UNUserNotificationCenter` directly (see `macos_un`) because
    /// `tauri-plugin-notification` still uses the deprecated `NSUserNotification` API whose
    /// banner delivery is broken on modern macOS. On other platforms, uses the plugin as before.
    pub async fn show_notification(&self, notification: Notification) -> Result<()> {
        log_info!("Attempting to show notification: {}", notification.title);

        // Check if DND is active and respect user settings
        if self.is_dnd_active().await && self.should_respect_dnd(&notification) {
            log_info!("DND is active, skipping notification: {}", notification.title);
            return Ok(());
        }

        #[cfg(target_os = "macos")]
        {
            return crate::notifications::macos_un::show(&notification).await;
        }

        #[cfg(not(target_os = "macos"))]
        {
            log_info!("Showing Tauri notification: {}", notification.title);

            let builder = self.app_handle.notification().builder()
                .title(&notification.title)
                .body(&notification.body);

            match builder.show() {
                Ok(_) => {
                    log_info!("Successfully showed Tauri notification: {}", notification.title);
                    Ok(())
                }
                Err(e) => {
                    log_error!("Failed to show Tauri notification: {}", e);
                    Err(anyhow!("Failed to show notification: {}", e))
                }
            }
        }
    }

    /// Check if Do Not Disturb is currently active
    /// Note: DND is managed through app settings, not system-level checks
    pub async fn is_dnd_active(&self) -> bool {
        // App manages DND through its own notification settings
        // No need to check system-level DND status
        false
    }

    /// Get the actual system DND status
    /// Note: DND is managed through app settings, not system-level checks
    pub async fn get_system_dnd_status(&self) -> bool {
        // App manages DND through its own notification settings
        // No need to check system-level DND status
        false
    }

    /// Request notification permission from the system.
    ///
    /// On macOS, triggers the real `UNUserNotificationCenter` authorization flow (which prompts
    /// the user on first call and returns the stored decision thereafter). On other platforms,
    /// Tauri handles permissions automatically.
    pub async fn request_permission(&self) -> Result<bool> {
        log_info!("Requesting notification permission");

        #[cfg(target_os = "macos")]
        {
            match crate::notifications::macos_un::request_authorization().await {
                Ok(granted) => {
                    log_info!("UN authorization result: granted={}", granted);
                    Ok(granted)
                }
                Err(e) => {
                    log_error!("UN authorization request failed: {}", e);
                    Err(e)
                }
            }
        }

        #[cfg(not(target_os = "macos"))]
        {
            log_info!("Notification permission granted (automatic for Tauri apps)");
            Ok(true)
        }
    }

    /// Show a test notification to verify the system is working
    #[allow(dead_code)] // Used by show_test_notification command for manual testing
    async fn show_test_notification(&self) -> Result<()> {
        let test_notification = Notification::test_notification();
        self.show_notification(test_notification).await
    }

    /// Determine if we should respect DND for this notification
    fn should_respect_dnd(&self, notification: &Notification) -> bool {
        match notification.priority {
            NotificationPriority::Critical => false, // Always show critical notifications
            _ => true, // Respect DND for all other priorities
        }
    }

    /// Clear all notifications (platform-specific)
    pub async fn clear_notifications(&self) -> Result<()> {
        log_info!("Clearing all notifications");

        // This is platform-specific and complex to implement
        // For now, we'll just log that we attempted to clear
        // Future enhancement can add platform-specific clearing

        Ok(())
    }
}

/// Convert notification timeout to duration
impl From<&NotificationTimeout> for Option<Duration> {
    fn from(timeout: &NotificationTimeout) -> Self {
        match timeout {
            NotificationTimeout::Never => None,
            NotificationTimeout::Seconds(secs) => Some(Duration::from_secs(*secs)),
            NotificationTimeout::Default => Some(Duration::from_secs(5)),
        }
    }
}