// audio/meeting_detection/mic_detector.rs
//
// Microphone-based meeting detection (like Char/Hyprnote).
// Monitors which apps are using the microphone (input device).
// If a non-ignored app is using the mic, a meeting is likely happening.
// This is the primary detection signal — no accessibility permission needed.

use super::AppInfo;

#[cfg(target_os = "macos")]
use cidre::core_audio as ca;

/// List apps currently using the microphone (input device).
/// This is the key meeting detection signal: YouTube doesn't use the mic,
/// but Google Meet, Zoom, Teams, etc. all do.
#[cfg(target_os = "macos")]
pub fn list_mic_using_apps() -> Vec<AppInfo> {
    match ca::System::processes() {
        Ok(processes) => {
            let mut apps = Vec::new();

            for process in processes {
                let is_input = process.is_running_input().unwrap_or(false);
                if !is_input {
                    continue;
                }

                if let Ok(pid) = process.pid() {
                    // Try NSRunningApplication first
                    if let Some(running_app) = cidre::ns::RunningApp::with_pid(pid) {
                        let display_name = running_app
                            .localized_name()
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| format!("Process {}", pid));
                        let bundle_id = running_app
                            .bundle_id()
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| format!("pid.{}", pid));

                        let is_browser = super::MACOS_BROWSER_BUNDLE_IDS
                            .iter()
                            .any(|bid| bundle_id.starts_with(bid));

                        apps.push(AppInfo {
                            identifier: bundle_id,
                            display_name,
                            is_browser,
                            pid: Some(pid),
                        });
                    } else {
                        // Helper process — walk up to find parent app
                        if let Some(app_info) = resolve_parent_app_for_mic(pid) {
                            // Deduplicate (multiple helpers from same parent)
                            if !apps.iter().any(|a| a.identifier == app_info.identifier) {
                                apps.push(app_info);
                            }
                        }
                    }
                }
            }

            apps
        }
        Err(e) => {
            tracing::error!("CoreAudio: failed to list processes for mic detection: {:?}", e);
            Vec::new()
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub fn list_mic_using_apps() -> Vec<AppInfo> {
    Vec::new()
}

/// Walk up process tree to find the parent application for a helper process.
#[cfg(target_os = "macos")]
fn resolve_parent_app_for_mic(child_pid: i32) -> Option<AppInfo> {
    use sysinfo::{Pid, System, ProcessesToUpdate};

    let mut sys = System::new();
    sys.refresh_processes(ProcessesToUpdate::All, true);

    let mut current_pid = Some(Pid::from_u32(child_pid as u32));

    for _ in 0..10 {
        let pid = current_pid?;
        let proc = sys.process(pid)?;

        if let Some(running_app) = cidre::ns::RunningApp::with_pid(pid.as_u32() as i32) {
            let display_name = running_app
                .localized_name()
                .map(|s| s.to_string())
                .unwrap_or_else(|| proc.name().to_string_lossy().to_string());
            let bundle_id = running_app
                .bundle_id()
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("pid.{}", pid.as_u32()));

            // Skip system processes
            if display_name == "launchd" {
                return None;
            }

            let is_browser = super::MACOS_BROWSER_BUNDLE_IDS
                .iter()
                .any(|bid| bundle_id.starts_with(bid));

            return Some(AppInfo {
                identifier: bundle_id,
                display_name,
                is_browser,
                pid: Some(pid.as_u32() as i32),
            });
        }

        current_pid = proc.parent();
    }

    None
}

/// Check if the default input device (microphone) is currently in use.
/// Used as a quick check before doing the more expensive process enumeration.
#[cfg(target_os = "macos")]
pub fn is_mic_active() -> bool {
    if let Ok(device) = ca::System::default_input_device() {
        let prop = ca::PropAddr {
            selector: ca::PropSelector::DEVICE_IS_RUNNING_SOMEWHERE,
            scope: ca::PropScope::GLOBAL,
            element: ca::PropElement::MAIN,
        };
        if let Ok(is_running) = device.prop::<u32>(&prop) {
            return is_running != 0;
        }
    }
    false
}

#[cfg(not(target_os = "macos"))]
pub fn is_mic_active() -> bool {
    false
}
