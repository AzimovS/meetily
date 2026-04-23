'use client';

import React, { useCallback, useEffect, useRef, useState } from 'react';
import { RefreshCw, Repeat, Users } from 'lucide-react';
import { invoke } from '@tauri-apps/api/core';

interface CalendarEventDto {
  id: string;
  title: string;
  start: string;
  end: string;
  organizer: string | null;
  attendee_count: number;
  is_recurring: boolean;
}

interface EventGroup {
  /** Stable unique key for React, computed from the local date. */
  dateKey: string;
  /** Human-facing label — may repeat across groups (two "Monday"s). */
  label: string;
  events: CalendarEventDto[];
}

/** Returns 'Today', 'Tomorrow', or a weekday name for any later day. */
function dayLabel(d: Date): string {
  const now = new Date();
  const today = new Date(now.getFullYear(), now.getMonth(), now.getDate());
  const tomorrow = new Date(today);
  tomorrow.setDate(today.getDate() + 1);
  const day = new Date(d.getFullYear(), d.getMonth(), d.getDate());
  if (day.getTime() === today.getTime()) return 'Today';
  if (day.getTime() === tomorrow.getTime()) return 'Tomorrow';
  return day.toLocaleDateString(undefined, { weekday: 'long' });
}

function dateKey(d: Date): string {
  // Padded so lexicographic sort matches chronological.
  const y = d.getFullYear();
  const m = String(d.getMonth() + 1).padStart(2, '0');
  const day = String(d.getDate()).padStart(2, '0');
  return `${y}-${m}-${day}`;
}

function formatTimeRange(start: string, end: string): string {
  const s = new Date(start);
  const e = new Date(end);
  const fmt: Intl.DateTimeFormatOptions = { hour: 'numeric', minute: '2-digit' };
  const startStr = s.toLocaleTimeString(undefined, fmt);
  const endStr = e.toLocaleTimeString(undefined, fmt);
  // Cross-day spans (e.g. 11:30 PM → 12:30 AM) need a cue so users
  // don't misread the order.
  const sameLocalDay =
    s.getFullYear() === e.getFullYear() &&
    s.getMonth() === e.getMonth() &&
    s.getDate() === e.getDate();
  return sameLocalDay ? `${startStr} – ${endStr}` : `${startStr} – ${endStr} (next day)`;
}

function groupByDay(events: CalendarEventDto[]): EventGroup[] {
  const groups = new Map<string, CalendarEventDto[]>();
  const labels = new Map<string, string>();
  for (const ev of events) {
    const d = new Date(ev.start);
    const key = dateKey(d);
    const bucket = groups.get(key) ?? [];
    bucket.push(ev);
    groups.set(key, bucket);
    if (!labels.has(key)) labels.set(key, dayLabel(d));
  }
  return Array.from(groups.entries()).map(([k, evs]) => ({
    dateKey: k,
    label: labels.get(k) ?? k,
    events: evs,
  }));
}

export function EventList() {
  const [events, setEvents] = useState<CalendarEventDto[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [isRefreshing, setIsRefreshing] = useState(false);
  const alive = useRef(true);
  // Monotonic sequence so stale responses never overwrite fresh ones.
  // A slow A that returns after a fast B must not replace B's data.
  const loadSeq = useRef(0);

  const load = useCallback(async () => {
    const mySeq = ++loadSeq.current;
    setIsRefreshing(true);
    setError(null);
    try {
      const result = await invoke<CalendarEventDto[]>('api_calendar_list_upcoming');
      if (!alive.current || mySeq !== loadSeq.current) return;
      setEvents(result);
    } catch (err) {
      if (!alive.current || mySeq !== loadSeq.current) return;
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      if (alive.current && mySeq === loadSeq.current) {
        setIsRefreshing(false);
      }
    }
  }, []);

  useEffect(() => {
    alive.current = true;
    load();
    return () => {
      alive.current = false;
    };
  }, [load]);

  if (events === null && !error) {
    return (
      <div className="text-center py-12 text-gray-500 text-sm">
        Loading events…
      </div>
    );
  }

  if (error) {
    return (
      <div className="px-4 py-3 bg-red-50 border border-red-200 rounded-lg text-sm text-red-800">
        <div className="flex items-start justify-between gap-4">
          <div>{error}</div>
          <button
            type="button"
            onClick={load}
            disabled={isRefreshing}
            className="text-red-700 hover:text-red-900 underline underline-offset-2 disabled:opacity-50"
          >
            Retry
          </button>
        </div>
      </div>
    );
  }

  const groups = groupByDay(events ?? []);

  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between">
        <h3 className="text-sm font-medium text-gray-500 uppercase tracking-wide">
          Upcoming
        </h3>
        <button
          type="button"
          onClick={load}
          disabled={isRefreshing}
          className="inline-flex items-center gap-1.5 text-sm text-gray-600 hover:text-gray-900 disabled:opacity-50"
        >
          <RefreshCw className={`w-3.5 h-3.5 ${isRefreshing ? 'animate-spin' : ''}`} />
          Refresh
        </button>
      </div>

      {groups.length === 0 ? (
        <div className="text-center py-12 text-gray-500 text-sm">
          No upcoming meetings — enjoy the quiet.
        </div>
      ) : (
        groups.map((group) => (
          <div key={group.dateKey}>
            <h4 className="text-xs font-semibold text-gray-400 uppercase tracking-wider mb-2">
              {group.label}
            </h4>
            <ul className="space-y-2">
              {group.events.map((ev) => (
                <li
                  key={ev.id}
                  className="bg-white border border-gray-200 rounded-lg p-3 shadow-sm"
                >
                  <div className="flex items-start justify-between gap-3">
                    <div className="flex-1 min-w-0">
                      <div className="font-medium text-gray-900 truncate">
                        {ev.title}
                        {ev.is_recurring && (
                          <Repeat className="inline w-3.5 h-3.5 ml-1.5 text-gray-400 align-middle" />
                        )}
                      </div>
                      <div className="text-xs text-gray-500 mt-0.5">
                        {formatTimeRange(ev.start, ev.end)}
                        {ev.organizer && <> · {ev.organizer}</>}
                      </div>
                    </div>
                    {ev.attendee_count > 0 && (
                      <div className="flex items-center gap-1 text-xs text-gray-500 shrink-0">
                        <Users className="w-3.5 h-3.5" />
                        {ev.attendee_count}
                      </div>
                    )}
                  </div>
                </li>
              ))}
            </ul>
          </div>
        ))
      )}
    </div>
  );
}
