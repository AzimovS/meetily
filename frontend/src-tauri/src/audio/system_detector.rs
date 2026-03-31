#[cfg(target_os = "macos")]
use std::time::{Duration, Instant};

#[cfg(target_os = "macos")]
use cidre::{core_audio as ca, os};

/// Extended app info with bundle ID, display name, and PID
#[derive(Debug, Clone)]
pub struct AudioAppInfo {
    pub bundle_id: String,
    pub display_name: String,
    pub pid: i32,
}

/// Event types for system audio detection
#[derive(Debug, Clone)]
pub enum SystemAudioEvent {
    /// System audio started — carries detailed app info (bundle IDs, PIDs)
    SystemAudioStarted(Vec<AudioAppInfo>),
    SystemAudioStopped,
}

pub type SystemAudioCallback = std::sync::Arc<dyn Fn(SystemAudioEvent) + Send + Sync + 'static>;

pub fn new_system_audio_callback<F>(f: F) -> SystemAudioCallback
where
    F: Fn(SystemAudioEvent) + Send + Sync + 'static,
{
    std::sync::Arc::new(f)
}

/// Background task manager for system audio detection
#[derive(Default)]
pub struct BackgroundTask {
    handle: Option<tokio::task::JoinHandle<()>>,
    stop_sender: Option<tokio::sync::oneshot::Sender<()>>,
}

impl BackgroundTask {
    pub fn start<F>(&mut self, task: F)
    where
        F: FnOnce(
                std::sync::Arc<std::sync::atomic::AtomicBool>,
                tokio::sync::oneshot::Receiver<()>,
            ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
            + Send
            + 'static,
    {
        if self.handle.is_some() {
            return; // Already running
        }

        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
        let running = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let running_clone = running.clone();

        let handle = tokio::spawn(async move {
            task(running_clone, stop_rx).await;
        });

        self.handle = Some(handle);
        self.stop_sender = Some(stop_tx);
    }

    pub fn stop(&mut self) {
        if let Some(sender) = self.stop_sender.take() {
            let _ = sender.send(());
        }

        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
    }
}

impl Drop for BackgroundTask {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Detects system audio usage on macOS
#[cfg(target_os = "macos")]
pub struct MacOSSystemAudioDetector {
    background: BackgroundTask,
}

#[cfg(target_os = "macos")]
impl Default for MacOSSystemAudioDetector {
    fn default() -> Self {
        Self {
            background: BackgroundTask::default(),
        }
    }
}

#[cfg(target_os = "macos")]
const DEVICE_IS_RUNNING_SOMEWHERE: ca::PropAddr = ca::PropAddr {
    selector: ca::PropSelector::DEVICE_IS_RUNNING_SOMEWHERE,
    scope: ca::PropScope::GLOBAL,
    element: ca::PropElement::MAIN,
};

#[cfg(target_os = "macos")]
struct DetectorState {
    last_state: bool,
    last_change: Instant,
    debounce_duration: Duration,
}

#[cfg(target_os = "macos")]
impl DetectorState {
    fn new() -> Self {
        Self {
            last_state: false,
            last_change: Instant::now(),
            debounce_duration: Duration::from_millis(500),
        }
    }

    fn should_trigger(&mut self, new_state: bool) -> bool {
        let now = Instant::now();

        if new_state == self.last_state {
            return false;
        }
        if now.duration_since(self.last_change) < self.debounce_duration {
            return false;
        }

        self.last_state = new_state;
        self.last_change = now;
        true
    }
}

#[cfg(target_os = "macos")]
impl MacOSSystemAudioDetector {
    pub fn start(&mut self, callback: SystemAudioCallback) {
        self.background.start(|running, mut stop_rx| {
            Box::pin(async move {
                let (tx, mut notify_rx) = tokio::sync::mpsc::channel(1);

                std::thread::spawn(move || {
                    let callback = std::sync::Arc::new(std::sync::Mutex::new(callback));
                    let current_device = std::sync::Arc::new(std::sync::Mutex::new(None::<ca::Device>));
                    let detector_state = std::sync::Arc::new(std::sync::Mutex::new(DetectorState::new()));

                    let callback_for_device = callback.clone();
                    let current_device_for_device = current_device.clone();
                    let detector_state_for_device = detector_state.clone();

                    extern "C-unwind" fn device_listener(
                        _obj_id: ca::Obj,
                        number_addresses: u32,
                        addresses: *const ca::PropAddr,
                        client_data: *mut (),
                    ) -> os::Status {
                        let data = unsafe {
                            &*(client_data as *const (
                                std::sync::Arc<std::sync::Mutex<SystemAudioCallback>>,
                                std::sync::Arc<std::sync::Mutex<Option<ca::Device>>>,
                                std::sync::Arc<std::sync::Mutex<DetectorState>>,
                            ))
                        };
                        let callback = &data.0;
                        let state = &data.2;

                        let addresses = unsafe { std::slice::from_raw_parts(addresses, number_addresses as usize) };

                        for addr in addresses {
                            if addr.selector == ca::PropSelector::DEVICE_IS_RUNNING_SOMEWHERE {
                                if let Ok(device) = ca::System::default_output_device() {
                                    if let Ok(is_running) = device.prop::<u32>(&DEVICE_IS_RUNNING_SOMEWHERE) {
                                        let system_audio_active = is_running != 0;

                                        if let Ok(mut state_guard) = state.lock() {
                                            if state_guard.should_trigger(system_audio_active) {
                                                if system_audio_active {
                                                    let cb = callback.clone();
                                                    std::thread::spawn(move || {
                                                        let apps = list_system_audio_using_apps();
                                                        // Privacy: don't log app names in release builds
                                                        #[cfg(debug_assertions)]
                                                        tracing::debug!("detect_system_audio_listener: {:?}", apps);

                                                        if let Ok(guard) = cb.lock() {
                                                            let event = SystemAudioEvent::SystemAudioStarted(apps);
                                                            #[cfg(debug_assertions)]
                                                            tracing::debug!(event = ?event, "detected");
                                                            (*guard)(event);
                                                        }
                                                    });
                                                } else {
                                                    if let Ok(guard) = callback.lock() {
                                                        let event = SystemAudioEvent::SystemAudioStopped;
                                                        #[cfg(debug_assertions)]
                                                        tracing::debug!(event = ?event, "detected");
                                                        (*guard)(event);
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }

                        os::Status::NO_ERR
                    }

                    extern "C-unwind" fn system_listener(
                        _obj_id: ca::Obj,
                        number_addresses: u32,
                        addresses: *const ca::PropAddr,
                        client_data: *mut (),
                    ) -> os::Status {
                        let data = unsafe {
                            &*(client_data as *const (
                                std::sync::Arc<std::sync::Mutex<SystemAudioCallback>>,
                                std::sync::Arc<std::sync::Mutex<Option<ca::Device>>>,
                                std::sync::Arc<std::sync::Mutex<DetectorState>>,
                                *mut (),
                            ))
                        };
                        let current_device = &data.1;
                        let state = &data.2;
                        let device_listener_data = data.3;

                        let addresses = unsafe { std::slice::from_raw_parts(addresses, number_addresses as usize) };

                        for addr in addresses {
                            if addr.selector == ca::PropSelector::HW_DEFAULT_OUTPUT_DEVICE {
                                if let Ok(mut device_guard) = current_device.lock() {
                                    if let Some(old_device) = device_guard.take() {
                                        let _ = old_device.remove_prop_listener(
                                            &DEVICE_IS_RUNNING_SOMEWHERE,
                                            device_listener,
                                            device_listener_data,
                                        );
                                    }

                                    if let Ok(new_device) = ca::System::default_output_device() {
                                        let system_audio_active = if let Ok(is_running) = new_device.prop::<u32>(&DEVICE_IS_RUNNING_SOMEWHERE) {
                                            is_running != 0
                                        } else {
                                            false
                                        };

                                        if new_device
                                            .add_prop_listener(
                                                &DEVICE_IS_RUNNING_SOMEWHERE,
                                                device_listener,
                                                device_listener_data,
                                            )
                                            .is_ok()
                                        {
                                            *device_guard = Some(new_device);

                                            if let Ok(mut state_guard) = state.lock() {
                                                if state_guard.should_trigger(system_audio_active) {
                                                    if system_audio_active {
                                                        let cb = data.0.clone();
                                                        std::thread::spawn(move || {
                                                            let apps = list_system_audio_using_apps();
                                                            #[cfg(debug_assertions)]
                                                            tracing::debug!("detect_system_listener: {:?}", apps);

                                                            if let Ok(callback_guard) = cb.lock() {
                                                                (*callback_guard)(SystemAudioEvent::SystemAudioStarted(apps));
                                                            }
                                                        });
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }

                        os::Status::NO_ERR
                    }

                    let device_listener_data = Box::new((
                        callback_for_device.clone(),
                        current_device_for_device.clone(),
                        detector_state_for_device.clone(),
                    ));
                    let device_listener_ptr = Box::into_raw(device_listener_data) as *mut ();

                    let system_listener_data = Box::new((
                        callback.clone(),
                        current_device.clone(),
                        detector_state.clone(),
                        device_listener_ptr,
                    ));
                    let system_listener_ptr = Box::into_raw(system_listener_data) as *mut ();

                    if let Err(e) = ca::System::OBJ.add_prop_listener(
                        &ca::PropSelector::HW_DEFAULT_OUTPUT_DEVICE.global_addr(),
                        system_listener,
                        system_listener_ptr,
                    ) {
                        tracing::error!("adding_system_listener_failed: {:?}", e);
                    } else {
                        tracing::info!("adding_system_listener_success");
                    }

                    if let Ok(device) = ca::System::default_output_device() {
                        let system_audio_active = if let Ok(is_running) = device.prop::<u32>(&DEVICE_IS_RUNNING_SOMEWHERE) {
                            is_running != 0
                        } else {
                            false
                        };

                        if device
                            .add_prop_listener(
                                &DEVICE_IS_RUNNING_SOMEWHERE,
                                device_listener,
                                device_listener_ptr,
                            )
                            .is_ok()
                        {
                            tracing::info!("adding_device_listener_success");

                            if let Ok(mut device_guard) = current_device.lock() {
                                *device_guard = Some(device);
                            }

                            if let Ok(mut state_guard) = detector_state.lock() {
                                state_guard.last_state = system_audio_active;
                            }
                        } else {
                            tracing::error!("adding_device_listener_failed");
                        }
                    } else {
                        tracing::warn!("no_default_output_device_found");
                    }

                    let _ = tx.blocking_send(());

                    loop {
                        std::thread::park();
                    }
                });

                let _ = notify_rx.recv().await;

                loop {
                    tokio::select! {
                        _ = &mut stop_rx => {
                            break;
                        }
                        _ = tokio::time::sleep(tokio::time::Duration::from_millis(500)) => {
                            if !running.load(std::sync::atomic::Ordering::SeqCst) {
                                break;
                            }
                        }
                    }
                }
            })
        });
    }

    pub fn stop(&mut self) {
        self.background.stop();
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn list_system_audio_using_apps() -> Vec<AudioAppInfo> {
    match ca::System::processes() {
        Ok(processes) => {
            let mut apps = Vec::new();

            for process in processes {
                let is_output = process.is_running_output().unwrap_or(false);
                let is_input = process.is_running_input().unwrap_or(false);

                if !(is_output || is_input) {
                    continue;
                }

                if let Ok(pid) = process.pid() {
                    // Try NSRunningApplication first (works for main app processes)
                    if let Some(running_app) = cidre::ns::RunningApp::with_pid(pid) {
                        let display_name = running_app
                            .localized_name()
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| format!("Process {}", pid));
                        let bundle_id = running_app
                            .bundle_id()
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| format!("pid.{}", pid));
                        tracing::info!(
                            "CoreAudio: {} ({}) pid={} output={} input={}",
                            display_name, bundle_id, pid, is_output, is_input
                        );
                        apps.push(AudioAppInfo { bundle_id, display_name, pid });
                    } else {
                        // Helper/subprocess — walk up process tree to find parent app
                        tracing::info!(
                            "CoreAudio: pid={} has no NSRunningApp, resolving via process tree",
                            pid
                        );
                        if let Some(app_info) = resolve_parent_app(pid) {
                            tracing::info!(
                                "CoreAudio: resolved pid={} → {} ({}) parent_pid={}",
                                pid, app_info.display_name, app_info.bundle_id, app_info.pid
                            );
                            // Avoid duplicates (multiple helpers from same parent)
                            if !apps.iter().any(|a| a.bundle_id == app_info.bundle_id) {
                                apps.push(app_info);
                            }
                        } else {
                            tracing::info!(
                                "CoreAudio: pid={} could not be resolved to a parent app",
                                pid
                            );
                        }
                    }
                }
            }

            apps
        }
        Err(e) => {
            tracing::error!("CoreAudio: failed to list processes: {:?}", e);
            Vec::new()
        }
    }
}

/// Walk up the process tree from a helper PID to find the parent application.
/// Chrome, Edge, Brave etc. spawn helper processes for audio that don't register
/// as NSRunningApplication — but their parent (the main browser) does.
#[cfg(target_os = "macos")]
fn resolve_parent_app(child_pid: i32) -> Option<AudioAppInfo> {
    use sysinfo::{Pid, System, ProcessesToUpdate};

    let mut sys = System::new();
    sys.refresh_processes(ProcessesToUpdate::All, true);

    let mut current_pid = Some(Pid::from_u32(child_pid as u32));

    // Walk up to 10 levels (Chrome helpers can be nested)
    for _ in 0..10 {
        let pid = current_pid?;
        let proc = sys.process(pid)?;

        // Try to look up this PID as an NSRunningApplication
        if let Some(running_app) = cidre::ns::RunningApp::with_pid(pid.as_u32() as i32) {
            let display_name = running_app
                .localized_name()
                .map(|s| s.to_string())
                .unwrap_or_else(|| proc.name().to_string_lossy().to_string());
            let bundle_id = running_app
                .bundle_id()
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("pid.{}", pid.as_u32()));

            // Skip launchd (pid 1) and other system processes
            if bundle_id.starts_with("com.apple.") && display_name == "launchd" {
                return None;
            }

            return Some(AudioAppInfo {
                bundle_id,
                display_name,
                pid: pid.as_u32() as i32,
            });
        }

        current_pid = proc.parent();
    }

    None
}

// Stub implementation for non-macOS platforms
#[cfg(not(target_os = "macos"))]
pub struct MacOSSystemAudioDetector;

#[cfg(not(target_os = "macos"))]
impl Default for MacOSSystemAudioDetector {
    fn default() -> Self {
        Self
    }
}

#[cfg(not(target_os = "macos"))]
impl MacOSSystemAudioDetector {
    pub fn start(&mut self, _callback: SystemAudioCallback) {
        tracing::warn!("System audio detection is only supported on macOS");
    }

    pub fn stop(&mut self) {}
}

/// Public interface for system audio detection
#[derive(Default)]
pub struct SystemAudioDetector {
    inner: MacOSSystemAudioDetector,
}

impl SystemAudioDetector {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn start(&mut self, callback: SystemAudioCallback) {
        self.inner.start(callback);
    }

    pub fn stop(&mut self) {
        self.inner.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore] // Only run manually as it requires audio hardware
    async fn test_system_audio_detector() {
        let mut detector = SystemAudioDetector::new();
        detector.start(new_system_audio_callback(|event| {
            println!("System audio event: {:?}", event);
        }));

        tokio::time::sleep(tokio::time::Duration::from_secs(30)).await;
        detector.stop();
    }
}
