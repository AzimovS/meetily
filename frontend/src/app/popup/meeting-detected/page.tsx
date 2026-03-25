'use client';

import { useEffect, useState, useCallback } from 'react';
import { listen } from '@tauri-apps/api/event';
import { invoke } from '@tauri-apps/api/core';
import { getCurrentWebviewWindow } from '@tauri-apps/api/webviewWindow';

interface MeetingDetectedData {
  appName: string;
  appIdentifier: string;
  meetingTitle: string | null;
  generation: number;
}

export default function MeetingDetectedPopup() {
  const [data, setData] = useState<MeetingDetectedData | null>(null);
  const [countdown, setCountdown] = useState(60);
  const [isStarting, setIsStarting] = useState(false);

  useEffect(() => {
    const unlisten = listen<MeetingDetectedData>('meeting-detected-data', (event) => {
      setData(event.payload);
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  // Auto-dismiss countdown
  useEffect(() => {
    if (countdown <= 0) {
      handleDismiss();
      return;
    }
    const timer = setTimeout(() => setCountdown((c) => c - 1), 1000);
    return () => clearTimeout(timer);
  }, [countdown]);

  const handleStartRecording = useCallback(async () => {
    if (!data || isStarting) return;
    setIsStarting(true);
    try {
      await invoke('popup_start_recording', {
        appIdentifier: data.appIdentifier,
        generation: data.generation,
      });
    } catch (e) {
      console.error('Failed to start recording:', e);
    }
    try {
      const win = getCurrentWebviewWindow();
      await win.close();
    } catch {
      // Window may already be closing
    }
  }, [data, isStarting]);

  const handleDismiss = useCallback(async () => {
    if (data) {
      try {
        await invoke('popup_dismiss', {
          appIdentifier: data.appIdentifier,
        });
      } catch {
        // Best effort
      }
    }
    try {
      const win = getCurrentWebviewWindow();
      await win.close();
    } catch {
      // Window may already be closing
    }
  }, [data]);

  const displayName = data?.meetingTitle || data?.appName || 'Unknown Meeting';

  return (
    <div
      style={{
        width: '100%',
        height: '100%',
        background: '#1a1a2e',
        color: '#ffffff',
        fontFamily: '-apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif',
        padding: '20px',
        display: 'flex',
        flexDirection: 'column',
        justifyContent: 'space-between',
        borderRadius: '12px',
        border: '1px solid rgba(255, 255, 255, 0.1)',
        overflow: 'hidden',
        userSelect: 'none',
        cursor: 'default',
      }}
      data-tauri-drag-region
    >
      <div>
        <div
          style={{
            fontSize: '12px',
            color: '#8b8ba7',
            textTransform: 'uppercase',
            letterSpacing: '0.5px',
            marginBottom: '6px',
          }}
        >
          Meeting Detected
        </div>
        <div
          style={{
            fontSize: '18px',
            fontWeight: 600,
            marginBottom: '4px',
            overflow: 'hidden',
            textOverflow: 'ellipsis',
            whiteSpace: 'nowrap',
          }}
        >
          {displayName}
        </div>
        {data?.appName && data.meetingTitle && (
          <div style={{ fontSize: '13px', color: '#8b8ba7' }}>
            via {data.appName}
          </div>
        )}
      </div>

      <div style={{ display: 'flex', gap: '10px', marginTop: '16px' }}>
        <button
          onClick={handleStartRecording}
          disabled={isStarting}
          style={{
            flex: 1,
            padding: '10px 16px',
            borderRadius: '8px',
            border: 'none',
            background: isStarting ? '#4a4a6a' : '#4f46e5',
            color: '#ffffff',
            fontSize: '14px',
            fontWeight: 600,
            cursor: isStarting ? 'wait' : 'pointer',
            transition: 'background 0.15s',
          }}
          onMouseEnter={(e) => {
            if (!isStarting) e.currentTarget.style.background = '#4338ca';
          }}
          onMouseLeave={(e) => {
            if (!isStarting) e.currentTarget.style.background = '#4f46e5';
          }}
        >
          {isStarting ? 'Starting...' : 'Start Recording'}
        </button>
        <button
          onClick={handleDismiss}
          style={{
            flex: 1,
            padding: '10px 16px',
            borderRadius: '8px',
            border: '1px solid rgba(255, 255, 255, 0.15)',
            background: 'transparent',
            color: '#8b8ba7',
            fontSize: '14px',
            fontWeight: 500,
            cursor: 'pointer',
            transition: 'background 0.15s',
          }}
          onMouseEnter={(e) => {
            e.currentTarget.style.background = 'rgba(255, 255, 255, 0.05)';
          }}
          onMouseLeave={(e) => {
            e.currentTarget.style.background = 'transparent';
          }}
        >
          Dismiss ({countdown}s)
        </button>
      </div>
    </div>
  );
}
