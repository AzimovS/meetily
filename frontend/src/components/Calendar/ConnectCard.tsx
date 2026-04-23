'use client';

import React, { useEffect, useRef, useState } from 'react';
import { Calendar as CalendarIcon, CheckCircle2 } from 'lucide-react';
import { invoke } from '@tauri-apps/api/core';

type ConnectionStatus =
  | { type: 'disconnected' }
  | { type: 'connected'; email: string };

export function ConnectCard() {
  const [status, setStatus] = useState<ConnectionStatus | null>(null);
  const [isWorking, setIsWorking] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const alive = useRef(true);

  useEffect(() => {
    alive.current = true;
    invoke<ConnectionStatus>('api_calendar_status')
      .then((result) => {
        if (alive.current) setStatus(result);
      })
      .catch((err) => {
        if (alive.current) {
          setStatus({ type: 'disconnected' });
          setError(err instanceof Error ? err.message : String(err));
        }
      });
    return () => {
      alive.current = false;
    };
  }, []);

  const handleConnect = async () => {
    setIsWorking(true);
    setError(null);
    try {
      const result = await invoke<ConnectionStatus>('api_calendar_connect');
      if (alive.current) setStatus(result);
    } catch (err) {
      if (alive.current) setError(err instanceof Error ? err.message : String(err));
    } finally {
      if (alive.current) setIsWorking(false);
    }
  };

  const handleDisconnect = async () => {
    setIsWorking(true);
    setError(null);
    try {
      await invoke('api_calendar_disconnect');
      if (alive.current) setStatus({ type: 'disconnected' });
    } catch (err) {
      if (alive.current) setError(err instanceof Error ? err.message : String(err));
    } finally {
      if (alive.current) setIsWorking(false);
    }
  };

  if (status === null) {
    return (
      <div className="bg-white border border-gray-200 rounded-xl p-8 max-w-lg mx-auto text-center shadow-sm">
        <p className="text-gray-500">Loading…</p>
      </div>
    );
  }

  if (status.type === 'connected') {
    return (
      <div className="bg-white border border-gray-200 rounded-xl p-8 max-w-lg mx-auto text-center shadow-sm">
        <div className="w-16 h-16 mx-auto mb-4 bg-green-50 rounded-full flex items-center justify-center">
          <CheckCircle2 className="w-8 h-8 text-green-600" />
        </div>
        <h2 className="text-xl font-semibold mb-2">Connected</h2>
        <p className="text-gray-600 mb-6">{status.email}</p>

        <button
          type="button"
          onClick={handleDisconnect}
          disabled={isWorking}
          className="inline-flex items-center justify-center px-5 py-2 text-sm font-medium text-gray-700 bg-gray-100 hover:bg-gray-200 rounded-lg transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
        >
          {isWorking ? 'Disconnecting…' : 'Disconnect'}
        </button>

        {error && (
          <div className="mt-6 px-4 py-3 bg-red-50 border border-red-200 rounded-lg text-sm text-red-800 text-left">
            {error}
          </div>
        )}

        <p className="text-xs text-gray-500 mt-6">
          Event list lands in the next slice.
        </p>
      </div>
    );
  }

  return (
    <div className="bg-white border border-gray-200 rounded-xl p-8 max-w-lg mx-auto text-center shadow-sm">
      <div className="w-16 h-16 mx-auto mb-4 bg-blue-50 rounded-full flex items-center justify-center">
        <CalendarIcon className="w-8 h-8 text-blue-600" />
      </div>

      <h2 className="text-xl font-semibold mb-2">Connect Google Calendar</h2>
      <p className="text-gray-600 mb-6">
        See your upcoming meetings here and let Meetily enrich summaries with
        attendees and agenda.
      </p>

      <button
        type="button"
        onClick={handleConnect}
        disabled={isWorking}
        className="inline-flex items-center justify-center px-6 py-3 bg-blue-600 hover:bg-blue-700 text-white font-medium rounded-lg transition-colors shadow-sm disabled:opacity-50 disabled:cursor-not-allowed"
      >
        {isWorking ? 'Waiting for Google consent…' : 'Connect Google Calendar'}
      </button>

      {error && (
        <div className="mt-6 px-4 py-3 bg-red-50 border border-red-200 rounded-lg text-sm text-red-800 text-left">
          {error}
        </div>
      )}

      <p className="text-xs text-gray-500 mt-6">
        Meetily only requests read-only access to your calendar events.
      </p>
    </div>
  );
}
