'use client';

import React, { useCallback, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { CalendarDays, ChevronDown, Loader2, Repeat, Users, X } from 'lucide-react';
import { toast } from 'sonner';

/**
 * Mirrors the Rust `FrozenCalendarContext` shape (calendar/types.rs).
 * Surfaced on `MeetingDetails.calendar_context` by `api_get_meeting`.
 */
interface FrozenCalendarContext {
  schema_version: number;
  source: string;
  event_id: string;
  recurrence_id: string | null;
  title: string;
  description: string | null;
  organizer: { display_name: string | null; email: string | null } | null;
  attendees: Array<{
    display_name: string | null;
    email: string | null;
    response_status: string | null;
  }>;
  start: string;
  end: string;
  captured_at: string;
}

/** DTO from `api_calendar_list_upcoming`. */
interface CalendarEventDto {
  id: string;
  title: string;
  start: string;
  end: string;
  organizer: string | null;
  attendee_count: number;
  is_recurring: boolean;
}

/** DTO from `api_calendar_status`. */
type ConnectionStatus =
  | { type: 'disconnected' }
  | { type: 'connected'; email: string };

interface Props {
  meetingId: string;
  /**
   * Frozen calendar context from `api_get_meeting`. `null` means the
   * recording was not auto-matched (or matching is still in progress
   * elsewhere). Refetched via `onLinkChanged` after the user edits.
   */
  context: FrozenCalendarContext | null;
  /** Refetches the parent's meeting object after a link/unlink. */
  onLinkChanged?: () => Promise<void> | void;
}

function formatTimeRange(startIso: string, endIso: string): string {
  const s = new Date(startIso);
  const e = new Date(endIso);
  if (isNaN(s.getTime()) || isNaN(e.getTime())) return `${startIso} – ${endIso}`;
  const fmt: Intl.DateTimeFormatOptions = { hour: 'numeric', minute: '2-digit' };
  const dateFmt: Intl.DateTimeFormatOptions = { month: 'short', day: 'numeric' };
  return `${s.toLocaleDateString(undefined, dateFmt)} · ${s.toLocaleTimeString(undefined, fmt)} – ${e.toLocaleTimeString(undefined, fmt)}`;
}

/**
 * Small "Calendar event" card on the meeting detail page.
 *
 * States:
 *   - calendar disconnected → render nothing (the user explicitly hid this surface)
 *   - linked → show event metadata + Change / Unlink
 *   - not linked → show "Link to event…" with a popover dropdown of today + tomorrow
 */
export function CalendarEventCard({ meetingId, context, onLinkChanged }: Props) {
  const [status, setStatus] = useState<ConnectionStatus | null>(null);
  const [pickerOpen, setPickerOpen] = useState(false);
  const [pickerEvents, setPickerEvents] = useState<CalendarEventDto[] | null>(null);
  const [pickerError, setPickerError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const alive = useRef(true);

  useEffect(() => {
    alive.current = true;
    invoke<ConnectionStatus>('api_calendar_status')
      .then((s) => {
        if (alive.current) setStatus(s);
      })
      .catch(() => {
        if (alive.current) setStatus({ type: 'disconnected' });
      });
    return () => {
      alive.current = false;
    };
  }, []);

  // Close the picker when the user clicks outside it. Single ref +
  // mousedown listener — no portal needed.
  const popoverRef = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    if (!pickerOpen) return;
    const onDocClick = (e: MouseEvent) => {
      if (popoverRef.current && !popoverRef.current.contains(e.target as Node)) {
        setPickerOpen(false);
      }
    };
    document.addEventListener('mousedown', onDocClick);
    return () => document.removeEventListener('mousedown', onDocClick);
  }, [pickerOpen]);

  const openPicker = useCallback(async () => {
    setPickerOpen(true);
    setPickerError(null);
    if (pickerEvents !== null) return; // already loaded once for this open
    try {
      const list = await invoke<CalendarEventDto[]>('api_calendar_list_upcoming');
      if (alive.current) setPickerEvents(list);
    } catch (err) {
      if (alive.current) setPickerError(err instanceof Error ? err.message : String(err));
    }
  }, [pickerEvents]);

  const handlePickEvent = useCallback(
    async (eventId: string, eventTitle: string) => {
      setBusy(true);
      try {
        await invoke('api_link_meeting_to_calendar_event', {
          meetingId,
          eventId,
        });
        setPickerOpen(false);
        setPickerEvents(null);
        toast.success(`Linked to "${eventTitle}"`);
        if (onLinkChanged) await onLinkChanged();
      } catch (err) {
        toast.error('Could not link calendar event', {
          description: err instanceof Error ? err.message : String(err),
        });
      } finally {
        if (alive.current) setBusy(false);
      }
    },
    [meetingId, onLinkChanged]
  );

  const handleUnlink = useCallback(async () => {
    setBusy(true);
    try {
      await invoke('api_unlink_meeting_calendar_context', { meetingId });
      toast.success('Calendar event unlinked');
      if (onLinkChanged) await onLinkChanged();
    } catch (err) {
      toast.error('Could not unlink calendar event', {
        description: err instanceof Error ? err.message : String(err),
      });
    } finally {
      if (alive.current) setBusy(false);
    }
  }, [meetingId, onLinkChanged]);

  // Hide the card entirely when the calendar isn't connected — the
  // user opted out of seeing this surface. Status loads async; show
  // nothing until it resolves to avoid a flash.
  if (status === null || status.type === 'disconnected') {
    return null;
  }

  const acceptedAttendees =
    context?.attendees.filter((a) => a.response_status === 'accepted').length ?? 0;
  const totalAttendees = context?.attendees.length ?? 0;

  return (
    <div className="mx-4 my-3 px-4 py-3 bg-white border border-gray-200 rounded-lg shadow-sm">
      <div className="flex items-start justify-between gap-3">
        <div className="flex items-start gap-3 min-w-0">
          <CalendarDays className="w-4 h-4 mt-0.5 text-gray-500 shrink-0" />
          <div className="min-w-0">
            {context ? (
              <>
                <div className="text-sm font-medium text-gray-900 truncate">
                  {context.title}
                  {context.recurrence_id && (
                    <Repeat className="inline w-3.5 h-3.5 ml-1.5 text-gray-400 align-middle" />
                  )}
                </div>
                <div className="text-xs text-gray-500 mt-0.5">
                  {formatTimeRange(context.start, context.end)}
                  {context.organizer && (
                    <> · {context.organizer.display_name || context.organizer.email}</>
                  )}
                  {totalAttendees > 0 && (
                    <>
                      {' · '}
                      <Users className="inline w-3 h-3 align-middle" /> {acceptedAttendees}/{totalAttendees}
                    </>
                  )}
                </div>
              </>
            ) : (
              <div className="text-sm text-gray-600">
                No calendar event linked to this meeting.
              </div>
            )}
          </div>
        </div>

        <div className="flex items-center gap-2 shrink-0 relative" ref={popoverRef}>
          <button
            type="button"
            onClick={openPicker}
            disabled={busy}
            className="inline-flex items-center gap-1 px-2 py-1 text-xs text-gray-700 hover:bg-gray-100 rounded transition-colors disabled:opacity-50"
          >
            {context ? 'Change' : 'Link event'}
            <ChevronDown className="w-3 h-3" />
          </button>
          {context && (
            <button
              type="button"
              onClick={handleUnlink}
              disabled={busy}
              aria-label="Unlink calendar event"
              className="p-1 text-gray-400 hover:text-gray-700 hover:bg-gray-100 rounded transition-colors disabled:opacity-50"
            >
              <X className="w-3.5 h-3.5" />
            </button>
          )}

          {pickerOpen && (
            <div className="absolute right-0 top-full mt-1 w-80 max-h-96 overflow-y-auto bg-white border border-gray-200 rounded-lg shadow-lg z-10 py-1">
              {pickerEvents === null && !pickerError && (
                <div className="flex items-center gap-2 px-3 py-2 text-xs text-gray-500">
                  <Loader2 className="w-3.5 h-3.5 animate-spin" /> Loading events…
                </div>
              )}
              {pickerError && (
                <div className="px-3 py-2 text-xs text-red-700">{pickerError}</div>
              )}
              {pickerEvents !== null && pickerEvents.length === 0 && (
                <div className="px-3 py-2 text-xs text-gray-500">
                  No upcoming events.
                </div>
              )}
              {pickerEvents !== null &&
                pickerEvents.map((ev) => (
                  <button
                    key={ev.id}
                    type="button"
                    onClick={() => handlePickEvent(ev.id, ev.title)}
                    disabled={busy}
                    className="w-full text-left px-3 py-2 hover:bg-gray-50 disabled:opacity-50 disabled:cursor-not-allowed"
                  >
                    <div className="text-sm text-gray-900 truncate">{ev.title}</div>
                    <div className="text-xs text-gray-500 mt-0.5">
                      {formatTimeRange(ev.start, ev.end)}
                      {ev.organizer && <> · {ev.organizer}</>}
                    </div>
                  </button>
                ))}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
