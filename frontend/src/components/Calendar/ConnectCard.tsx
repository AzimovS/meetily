'use client';

import React, { useEffect, useRef, useState } from 'react';
import { Calendar as CalendarIcon } from 'lucide-react';
import { invoke } from '@tauri-apps/api/core';
import { EventList } from './EventList';

type ConnectionStatus =
  | { type: 'disconnected' }
  | { type: 'connected'; email: string };

export function ConnectCard() {
  const [status, setStatus] = useState<ConnectionStatus | null>(null);
  const [isWorking, setIsWorking] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const alive = useRef(true);
  // Monotonically-incremented per status-changing action. Any in-flight
  // response whose id doesn't match is stale and gets discarded. Fixes
  // the disconnect-flickers-to-connected race.
  const statusSeq = useRef(0);
  // Synchronous guard against double-click: setState-driven `isWorking`
  // doesn't flush until next render, so a fast second click can slip
  // through. A ref is synchronous.
  const connectInFlight = useRef(false);

  useEffect(() => {
    alive.current = true;
    const myId = ++statusSeq.current;
    invoke<ConnectionStatus>('api_calendar_status')
      .then((result) => {
        if (!alive.current || myId !== statusSeq.current) return;
        setStatus(result);
      })
      .catch((err) => {
        if (!alive.current || myId !== statusSeq.current) return;
        setStatus({ type: 'disconnected' });
        setError(err instanceof Error ? err.message : String(err));
      });
    return () => {
      alive.current = false;
    };
  }, []);

  const handleConnect = async () => {
    if (connectInFlight.current) return;
    connectInFlight.current = true;
    setIsWorking(true);
    setError(null);
    try {
      const result = await invoke<ConnectionStatus>('api_calendar_connect');
      // Authoritative state transition — bump seq so any older in-flight
      // status query can't overwrite this.
      statusSeq.current++;
      if (alive.current) setStatus(result);
    } catch (err) {
      if (alive.current) setError(err instanceof Error ? err.message : String(err));
    } finally {
      connectInFlight.current = false;
      if (alive.current) setIsWorking(false);
    }
  };

  const handleDisconnect = async () => {
    setIsWorking(true);
    setError(null);
    try {
      await invoke('api_calendar_disconnect');
      statusSeq.current++;
      if (alive.current) setStatus({ type: 'disconnected' });
    } catch (err) {
      if (alive.current) setError(err instanceof Error ? err.message : String(err));
    } finally {
      if (alive.current) setIsWorking(false);
    }
  };

  if (status === null) {
    return (
      <div className="text-center py-12 text-gray-500 text-sm">Loading…</div>
    );
  }

  if (status.type === 'connected') {
    return (
      <div className="space-y-6">
        <div className="flex items-center justify-between bg-white border border-gray-200 rounded-lg px-4 py-3 shadow-sm">
          <div className="flex items-center gap-3">
            <div className="w-2 h-2 rounded-full bg-green-500" />
            <span className="text-sm text-gray-700">{status.email}</span>
          </div>
          <button
            type="button"
            onClick={handleDisconnect}
            disabled={isWorking}
            className="text-sm text-gray-600 hover:text-gray-900 disabled:opacity-50"
          >
            {isWorking ? 'Disconnecting…' : 'Disconnect'}
          </button>
        </div>

        {error && (
          <div className="px-4 py-3 bg-red-50 border border-red-200 rounded-lg text-sm text-red-800">
            {error}
          </div>
        )}

        <EventList />
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
