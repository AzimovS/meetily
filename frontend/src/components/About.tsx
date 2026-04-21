import React, { useState, useEffect } from "react";
import { invoke } from '@tauri-apps/api/core';
import { getVersion } from '@tauri-apps/api/app';
import Image from 'next/image';
import { UpdateDialog } from "./UpdateDialog";
import { updateService, UpdateInfo } from '@/services/updateService';
import { Button } from './ui/button';
import {
    DropdownMenu,
    DropdownMenuContent,
    DropdownMenuItem,
    DropdownMenuTrigger,
} from './ui/dropdown-menu';
import { Loader2, CheckCircle2, Bell, ChevronDown } from 'lucide-react';
import { toast } from 'sonner';

type DebugNotificationKind =
    | 'recording_started'
    | 'recording_stopped'
    | 'recording_paused'
    | 'recording_resumed'
    | 'transcription_complete'
    | 'meeting_reminder'
    | 'system_error'
    | 'test'
    | 'meeting_detected'
    | 'meeting_ended';

// `prefKey` matches a Rust NotificationPreferences field; omitted when a type isn't gated by preference.
const DEBUG_NOTIFICATION_ITEMS: Array<{
    kind: DebugNotificationKind;
    label: string;
    prefKey?: string;
}> = [
    { kind: 'recording_started',      label: 'Recording started',        prefKey: 'show_recording_started' },
    { kind: 'recording_stopped',      label: 'Recording stopped',        prefKey: 'show_recording_stopped' },
    { kind: 'recording_paused',       label: 'Recording paused',         prefKey: 'show_recording_paused' },
    { kind: 'recording_resumed',      label: 'Recording resumed',        prefKey: 'show_recording_resumed' },
    { kind: 'transcription_complete', label: 'Transcription complete',   prefKey: 'show_transcription_complete' },
    { kind: 'meeting_reminder',       label: 'Meeting reminder (5 min)', prefKey: 'show_meeting_reminders' },
    { kind: 'system_error',           label: 'System error',             prefKey: 'show_system_errors' },
    { kind: 'test',                   label: 'Generic test notification' },
    { kind: 'meeting_detected',       label: 'Meeting detected (auto)',  prefKey: 'show_meeting_detected' },
    { kind: 'meeting_ended',          label: 'Meeting ended (auto)',     prefKey: 'show_meeting_ended' },
];


export function About() {
    const [currentVersion, setCurrentVersion] = useState<string>('');
    const [updateInfo, setUpdateInfo] = useState<UpdateInfo | null>(null);
    const [isChecking, setIsChecking] = useState(false);
    const [showUpdateDialog, setShowUpdateDialog] = useState(false);

    useEffect(() => {
        // Get current version on mount
        getVersion().then(setCurrentVersion).catch(console.error);
    }, []);

    const handleContactClick = async () => {
        try {
            await invoke('open_external_url', { url: 'https://meetily.zackriya.com/#about' });
        } catch (error) {
            console.error('Failed to open link:', error);
        }
    };

    const handleDebugNotification = async (
        kind: DebugNotificationKind,
        prefKey?: string,
    ) => {
        try {
            const ready = await invoke<boolean>('is_notification_system_ready');
            if (!ready) {
                toast.error('Notification system not ready. Try restarting the app.');
                return;
            }

            const settings = await invoke<any>('get_notification_settings');
            if (settings?.consent_given === false) {
                toast.info(
                    'Notifications are disabled in Preferences → Notifications. The OS notification will be suppressed.',
                );
            } else if (
                prefKey &&
                settings?.notification_preferences?.[prefKey] === false
            ) {
                toast.info(
                    `"${prefKey}" is disabled in Preferences. This notification will be suppressed.`,
                );
            }

            await invoke('debug_show_notification', { kind });
            toast.success(`Fired: ${kind.replace(/_/g, ' ')}`, { duration: 2000 });
        } catch (e: any) {
            toast.error('Debug notification failed: ' + (e.message || String(e)));
        }
    };

    const handleCheckForUpdates = async () => {
        setIsChecking(true);
        try {
            const currentVer = await getVersion();
            toast.info(`Current version: v${currentVer}. Checking for updates...`);

            const info = await updateService.checkForUpdates(true);
            setUpdateInfo(info);
            if (info.available) {
                toast.success(`Update found: v${info.version}`);
                setShowUpdateDialog(true);
            } else {
                toast.success(`v${currentVer} is the latest version`);
            }
        } catch (error: any) {
            console.error('Failed to check for updates:', error);
            toast.error('Update check failed: ' + (error.message || String(error)));
        } finally {
            setIsChecking(false);
        }
    };

    return (
        <div className="p-4 space-y-4 h-[80vh] overflow-y-auto">
            {/* Compact Header */}
            <div className="text-center">
                <div className="mb-3">
                    <Image
                        src="icon_128x128.png"
                        alt="Meetily Logo"
                        width={64}
                        height={64}
                        className="mx-auto"
                    />
                </div>
                {/* <h1 className="text-xl font-bold text-gray-900">Meetily</h1> */}
                <span className="text-sm text-gray-500"> v{currentVersion}</span>
                <p className="text-medium text-gray-600 mt-1">
                    Real-time notes and summaries that never leave your machine.
                </p>
                <div className="mt-3">
                    <Button
                        onClick={handleCheckForUpdates}
                        disabled={isChecking}
                        variant="outline"
                        size="sm"
                        className="text-xs"
                    >
                        {isChecking ? (
                            <>
                                <Loader2 className="h-3 w-3 mr-2 animate-spin" />
                                Checking...
                            </>
                        ) : (
                            <>
                                <CheckCircle2 className="h-3 w-3 mr-2" />
                                Check for Updates
                            </>
                        )}
                    </Button>
                    <Button
                        onClick={async () => {
                            try {
                                const report = await invoke<string>('debug_check_update');
                                toast.info(report, { duration: 30000 });
                                console.log(report);
                            } catch (e: any) {
                                toast.error('Debug failed: ' + (e.message || String(e)), { duration: 15000 });
                            }
                        }}
                        variant="ghost"
                        size="sm"
                        className="text-xs ml-2"
                    >
                        Debug Updater
                    </Button>
                    <DropdownMenu>
                        <DropdownMenuTrigger asChild>
                            <Button variant="ghost" size="sm" className="text-xs ml-2">
                                <Bell className="h-3 w-3 mr-1" />
                                Debug Notifications
                                <ChevronDown className="h-3 w-3 ml-1" />
                            </Button>
                        </DropdownMenuTrigger>
                        <DropdownMenuContent align="end">
                            {DEBUG_NOTIFICATION_ITEMS.map((item) => (
                                <DropdownMenuItem
                                    key={item.kind}
                                    onClick={() => handleDebugNotification(item.kind, item.prefKey)}
                                >
                                    {item.label}
                                </DropdownMenuItem>
                            ))}
                        </DropdownMenuContent>
                    </DropdownMenu>
                    {updateInfo?.available && (
                        <div className="mt-2 text-xs text-blue-600">
                            Update available: v{updateInfo.version}
                        </div>
                    )}
                </div>
            </div>

            {/* Features Grid - Compact */}
            <div className="space-y-3">
                <h2 className="text-base font-semibold text-gray-800">What makes Meetily different</h2>
                <div className="grid grid-cols-2 gap-2">
                    <div className="bg-gray-50 rounded p-3 hover:bg-gray-100 transition-colors">
                        <h3 className="font-bold text-sm text-gray-900 mb-1">Privacy-first</h3>
                        <p className="text-xs text-gray-600 leading-relaxed">Your data & AI processing workflow can now stay within your premise. No cloud, no leaks.</p>
                    </div>
                    <div className="bg-gray-50 rounded p-3 hover:bg-gray-100 transition-colors">
                        <h3 className="font-bold text-sm text-gray-900 mb-1">Use Any Model</h3>
                        <p className="text-xs text-gray-600 leading-relaxed">Prefer local open-source model? Great. Want to plug in an external API? Also fine. No lock-in.</p>
                    </div>
                    <div className="bg-gray-50 rounded p-3 hover:bg-gray-100 transition-colors">
                        <h3 className="font-bold text-sm text-gray-900 mb-1">Cost-Smart</h3>
                        <p className="text-xs text-gray-600 leading-relaxed">Avoid pay-per-minute bills by running models locally (or pay only for the calls you choose).</p>
                    </div>
                    <div className="bg-gray-50 rounded p-3 hover:bg-gray-100 transition-colors">
                        <h3 className="font-bold text-sm text-gray-900 mb-1">Works everywhere</h3>
                        <p className="text-xs text-gray-600 leading-relaxed">Google Meet, Zoom, Teams-online or offline.</p>
                    </div>
                </div>
            </div>

            {/* Coming Soon - Compact */}
            <div className="bg-blue-50 rounded p-3">
                <p className="text-s text-blue-800">
                    <span className="font-bold">Coming soon:</span> A library of on-device AI agents-automating follow-ups, action tracking, and more.
                </p>
            </div>

            {/* CTA Section - Compact */}
            <div className="text-center space-y-2">
                <h3 className="text-medium font-semibold text-gray-800">Ready to push your business further?</h3>
                <p className="text-s text-gray-600">
                    If you're planning to build privacy-first custom AI agents or a fully tailored product for your <span className="font-bold">business</span>, we can help you build it.
                </p>
                <button
                    onClick={handleContactClick}
                    className="inline-flex items-center px-4 py-2 bg-blue-600 hover:bg-blue-700 text-white text-sm font-medium rounded transition-colors duration-200 shadow-sm hover:shadow-md"
                >
                    Chat with the Zackriya team
                </button>
            </div>

            {/* Footer - Compact */}
            <div className="pt-2 border-t border-gray-200 text-center">
                <p className="text-xs text-gray-400">
                    Built by Zackriya Solutions
                </p>
            </div>
            {/* Update Dialog */}
            <UpdateDialog
                open={showUpdateDialog}
                onOpenChange={setShowUpdateDialog}
                updateInfo={updateInfo}
            />
        </div>

    )
}