'use client';

import React, { useCallback, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import {
  CalendarDays,
  CalendarPlus,
  ChevronDown,
  Loader2,
  Repeat,
  Users,
  X,
} from 'lucide-react';
import { toast } from 'sonner';

import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from '@/components/ui/popover';

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
  /**
   * Recording start (`meetings.created_at`). Used to anchor the
   * link-event picker's query window on the day the recording
   * happened, so events that already ended are still selectable.
   */
  meetingCreatedAt: string;
  /** Refetches the parent's meeting object after a link/unlink. */
  onLinkChanged?: () => Promise<void> | void;
}

/** Compact "Today/Tomorrow · 2:00 – 2:30 PM" used inside the chip. */
function formatChipTimeRange(startIso: string, endIso: string): string {
  const s = new Date(startIso);
  const e = new Date(endIso);
  if (isNaN(s.getTime()) || isNaN(e.getTime())) return '';
  const now = new Date();
  const sameDay =
    s.getFullYear() === now.getFullYear() &&
    s.getMonth() === now.getMonth() &&
    s.getDate() === now.getDate();
  const tomorrow = new Date(now);
  tomorrow.setDate(now.getDate() + 1);
  const isTomorrow =
    s.getFullYear() === tomorrow.getFullYear() &&
    s.getMonth() === tomorrow.getMonth() &&
    s.getDate() === tomorrow.getDate();
  const dayLabel = sameDay
    ? 'Today'
    : isTomorrow
      ? 'Tomorrow'
      : s.toLocaleDateString(undefined, { month: 'short', day: 'numeric' });
  const fmt: Intl.DateTimeFormatOptions = { hour: 'numeric', minute: '2-digit' };
  return `${dayLabel} · ${s.toLocaleTimeString(undefined, fmt)} – ${e.toLocaleTimeString(undefined, fmt)}`;
}

/** "30 min" / "1h" / "1h 15m" — derived from start/end. Empty when unparseable. */
function formatDuration(startIso: string, endIso: string): string {
  const s = new Date(startIso);
  const e = new Date(endIso);
  if (isNaN(s.getTime()) || isNaN(e.getTime())) return '';
  const totalMin = Math.max(0, Math.round((e.getTime() - s.getTime()) / 60000));
  if (totalMin < 60) return `${totalMin} min`;
  const hr = Math.floor(totalMin / 60);
  const rem = totalMin % 60;
  return rem === 0 ? `${hr}h` : `${hr}h ${rem}m`;
}

/** "Synced 12 min ago" / "2 hours ago". For the captured_at footer. */
function formatRelative(iso: string): string {
  const then = new Date(iso);
  if (isNaN(then.getTime())) return iso;
  const diffMin = Math.max(0, Math.round((Date.now() - then.getTime()) / 60000));
  if (diffMin < 1) return 'just now';
  if (diffMin < 60) return `${diffMin} min ago`;
  const diffHr = Math.floor(diffMin / 60);
  if (diffHr < 24) return `${diffHr} hour${diffHr === 1 ? '' : 's'} ago`;
  const diffDay = Math.floor(diffHr / 24);
  if (diffDay < 30) return `${diffDay} day${diffDay === 1 ? '' : 's'} ago`;
  const diffMo = Math.floor(diffDay / 30);
  return `${diffMo} month${diffMo === 1 ? '' : 's'} ago`;
}

/** Small colored dot per Google `responseStatus` value. */
function responseDotClass(status: string | null | undefined): string {
  switch (status) {
    case 'accepted':
      return 'bg-green-500';
    case 'declined':
      return 'bg-red-400';
    case 'tentative':
      return 'bg-yellow-400';
    default:
      return 'bg-gray-300';
  }
}

/** Human label for `responseStatus`. */
function responseLabel(status: string | null | undefined): string {
  if (!status) return '';
  if (status === 'needsAction') return 'no response';
  return status;
}

/** "Google Calendar" / "google" passthrough — small label in the footer. */
function sourceLabel(source: string | null | undefined): string {
  if (!source) return '';
  if (source === 'google') return 'Google Calendar';
  return source;
}

/**
 * Compact calendar event chip with a popover for details + edit affordances.
 *
 * States:
 *   - calendar disconnected → render nothing (the user explicitly hid this surface)
 *   - linked → chip showing event title + time; popover reveals organizer,
 *     attendee count, and Change / Unlink controls
 *   - not linked → "Link event" pill; popover holds the event picker
 *
 * Designed to live in a panel header bar — minimal vertical footprint, all
 * detail and editing surfaces deferred behind the popover.
 */
export function CalendarEventCard({
  meetingId,
  context,
  meetingCreatedAt,
  onLinkChanged,
}: Props) {
  const [status, setStatus] = useState<ConnectionStatus | null>(null);
  // Popover open state. We control it manually so success-path actions
  // (link/unlink) can close the popover without waiting for the user.
  const [popoverOpen, setPopoverOpen] = useState(false);
  // When the popover is open on a linked event we default to "view".
  // Clicking Change flips us into "pick" mode so the same popover hosts
  // the event list inline rather than opening a second floating UI.
  const [mode, setMode] = useState<'view' | 'pick'>('view');
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

  // Reset picker scratch state whenever the popover closes — next open
  // should re-fetch fresh events rather than show stale results.
  useEffect(() => {
    if (!popoverOpen) {
      setMode('view');
      setPickerEvents(null);
      setPickerError(null);
    }
  }, [popoverOpen]);

  const loadPickerEvents = useCallback(async () => {
    setPickerError(null);
    setPickerEvents(null);
    // Anchor the picker window on the local day the recording started,
    // not on `now`. `api_calendar_list_upcoming` (timeMin = now) hides
    // events that already ended, which makes manual linking impossible
    // for any meeting that finished before the user opened the picker.
    // Window: [start of meeting's local day, +48h] — covers a same-day
    // recording's pre-existing events plus a buffer for next-day picks.
    const tMin = new Date(meetingCreatedAt);
    if (isNaN(tMin.getTime())) {
      if (alive.current) {
        setPickerError('Invalid meeting timestamp; cannot load events.');
      }
      return;
    }
    tMin.setHours(0, 0, 0, 0);
    const tMax = new Date(tMin);
    tMax.setDate(tMax.getDate() + 2);
    try {
      const list = await invoke<CalendarEventDto[]>(
        'api_calendar_list_events_in_window',
        {
          timeMin: tMin.toISOString(),
          timeMax: tMax.toISOString(),
        }
      );
      if (alive.current) setPickerEvents(list);
    } catch (err) {
      if (alive.current) setPickerError(err instanceof Error ? err.message : String(err));
    }
  }, [meetingCreatedAt]);

  const handleEnterPickMode = useCallback(() => {
    setMode('pick');
    void loadPickerEvents();
  }, [loadPickerEvents]);

  const handlePickEvent = useCallback(
    async (eventId: string, eventTitle: string) => {
      setBusy(true);
      try {
        await invoke('api_link_meeting_to_calendar_event', {
          meetingId,
          eventId,
        });
        setPopoverOpen(false);
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
      setPopoverOpen(false);
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

  // Hide entirely when the calendar isn't connected — the user opted
  // out of seeing this surface. Status loads async; render nothing
  // until it resolves to avoid a flash.
  if (status === null || status.type === 'disconnected') {
    return null;
  }

  // ---------- Trigger element ---------------------------------------

  // We open in pick mode directly when there's nothing linked yet —
  // there's no "view" content to show.
  const handleTriggerClick = () => {
    if (!context) {
      setMode('pick');
      // Lazy-load only when first opened in pick mode.
      if (pickerEvents === null) void loadPickerEvents();
    } else {
      setMode('view');
    }
  };

  const trigger = context ? (
    <button
      type="button"
      onClick={handleTriggerClick}
      disabled={busy}
      className="group inline-flex items-center gap-1.5 max-w-full px-2.5 py-1 rounded-full border border-gray-200 bg-gray-50 hover:bg-white hover:border-gray-300 transition-colors disabled:opacity-50"
    >
      <CalendarDays className="w-3.5 h-3.5 text-gray-500 shrink-0" />
      <span className="text-xs text-gray-800 font-medium truncate min-w-0">
        {context.title}
      </span>
      {context.recurrence_id && (
        <Repeat className="w-3 h-3 text-gray-400 shrink-0" />
      )}
      <span className="text-xs text-gray-500 shrink-0">
        · {formatChipTimeRange(context.start, context.end)}
      </span>
      <ChevronDown className="w-3 h-3 text-gray-400 shrink-0" />
    </button>
  ) : (
    <button
      type="button"
      onClick={handleTriggerClick}
      disabled={busy}
      className="inline-flex items-center gap-1.5 px-2.5 py-1 rounded-full border border-dashed border-gray-300 text-gray-500 hover:text-gray-800 hover:border-gray-400 hover:bg-gray-50 transition-colors disabled:opacity-50"
    >
      <CalendarPlus className="w-3.5 h-3.5 shrink-0" />
      <span className="text-xs font-medium">Link event</span>
    </button>
  );

  // ---------- Popover content ---------------------------------------

  const acceptedAttendees =
    context?.attendees.filter((a) => a.response_status === 'accepted').length ?? 0;
  const totalAttendees = context?.attendees.length ?? 0;

  const renderViewContent = () => {
    if (!context) return null;
    const duration = formatDuration(context.start, context.end);
    return (
      <div className="flex flex-col gap-3">
        {/* Scrollable inner region — keeps the popover from outgrowing
            the viewport for events with verbose descriptions or many
            attendees. Action row sits below, always visible. */}
        <div className="max-h-[420px] overflow-y-auto pr-1 space-y-3">
          {/* Header: title + time + duration + recurrence label */}
          <div className="flex items-start gap-2">
            <CalendarDays className="w-4 h-4 mt-0.5 text-gray-500 shrink-0" />
            <div className="min-w-0 flex-1">
              <div className="text-sm font-semibold text-gray-900 break-words">
                {context.title}
              </div>
              <div className="text-xs text-gray-500 mt-0.5 flex flex-wrap items-center gap-x-1.5">
                <span>{formatChipTimeRange(context.start, context.end)}</span>
                {duration && <span>· {duration}</span>}
                {context.recurrence_id && (
                  <span className="inline-flex items-center gap-1 text-gray-500">
                    · <Repeat className="w-3 h-3 text-gray-400" /> Repeats
                  </span>
                )}
              </div>
            </div>
          </div>

          {/* Organizer */}
          {context.organizer &&
            (context.organizer.display_name || context.organizer.email) && (
              <div className="text-xs">
                <div className="text-[10px] text-gray-400 uppercase tracking-wide font-medium mb-1">
                  Organizer
                </div>
                {context.organizer.display_name && (
                  <div className="text-gray-800 truncate">
                    {context.organizer.display_name}
                  </div>
                )}
                {context.organizer.email && (
                  <div className="text-gray-500 truncate">
                    {context.organizer.email}
                  </div>
                )}
              </div>
            )}

          {/* Description — preserves line breaks; Google may return either
              plain text or HTML, but we render as text to avoid XSS. */}
          {context.description && context.description.trim() && (
            <div className="text-xs">
              <div className="text-[10px] text-gray-400 uppercase tracking-wide font-medium mb-1">
                Description
              </div>
              <div className="text-gray-700 whitespace-pre-wrap break-words leading-relaxed">
                {context.description}
              </div>
            </div>
          )}

          {/* Attendees — full list, dot-coded by response status */}
          {totalAttendees > 0 && (
            <div className="text-xs">
              <div className="text-[10px] text-gray-400 uppercase tracking-wide font-medium mb-1 flex items-center gap-1.5">
                <Users className="w-3 h-3" />
                Attendees · {acceptedAttendees}/{totalAttendees} accepted
              </div>
              <div className="space-y-1">
                {context.attendees.map((a, i) => {
                  const name = a.display_name || a.email || 'Unknown';
                  return (
                    <div
                      key={`${a.email ?? 'noemail'}-${i}`}
                      className="flex items-center gap-2"
                    >
                      <span
                        className={`inline-block w-1.5 h-1.5 rounded-full shrink-0 ${responseDotClass(
                          a.response_status
                        )}`}
                        aria-label={a.response_status ?? 'unknown'}
                      />
                      <span className="text-gray-800 truncate flex-1 min-w-0">
                        {name}
                      </span>
                      {a.response_status && (
                        <span className="text-[10px] text-gray-500 capitalize shrink-0">
                          {responseLabel(a.response_status)}
                        </span>
                      )}
                    </div>
                  );
                })}
              </div>
            </div>
          )}

          {/* Footer: staleness hint + provenance */}
          <div className="text-[11px] text-gray-400 pt-2 border-t border-gray-100 flex flex-wrap items-center gap-x-1.5">
            <span>Synced {formatRelative(context.captured_at)}</span>
            {context.source && <span>· from {sourceLabel(context.source)}</span>}
          </div>
        </div>

        {/* Action row — Change / Unlink */}
        <div className="flex items-center gap-2">
          <button
            type="button"
            onClick={handleEnterPickMode}
            disabled={busy}
            className="flex-1 px-2 py-1.5 text-xs font-medium text-gray-700 bg-gray-50 hover:bg-gray-100 rounded transition-colors disabled:opacity-50"
          >
            Change
          </button>
          <button
            type="button"
            onClick={handleUnlink}
            disabled={busy}
            aria-label="Unlink calendar event"
            className="px-2 py-1.5 text-xs font-medium text-gray-500 hover:text-red-700 hover:bg-red-50 rounded transition-colors disabled:opacity-50 inline-flex items-center gap-1"
          >
            <X className="w-3 h-3" /> Unlink
          </button>
        </div>
      </div>
    );
  };

  const renderPickContent = () => (
    <div>
      <div className="flex items-center justify-between px-1 pb-2 border-b border-gray-100">
        <span className="text-xs font-medium text-gray-500 uppercase tracking-wide">
          Pick an event
        </span>
        {context && (
          <button
            type="button"
            onClick={() => setMode('view')}
            className="text-xs text-gray-500 hover:text-gray-800"
          >
            Cancel
          </button>
        )}
      </div>
      <div className="max-h-72 overflow-y-auto -mx-1 mt-1">
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
            No events found in this window.
          </div>
        )}
        {pickerEvents !== null &&
          pickerEvents.map((ev) => (
            <button
              key={ev.id}
              type="button"
              onClick={() => handlePickEvent(ev.id, ev.title)}
              disabled={busy}
              className="w-full text-left px-3 py-2 hover:bg-gray-50 rounded disabled:opacity-50 disabled:cursor-not-allowed"
            >
              <div className="text-sm text-gray-900 truncate">{ev.title}</div>
              <div className="text-xs text-gray-500 mt-0.5 truncate">
                {formatChipTimeRange(ev.start, ev.end)}
                {ev.organizer && <> · {ev.organizer}</>}
              </div>
            </button>
          ))}
      </div>
    </div>
  );

  return (
    <Popover open={popoverOpen} onOpenChange={setPopoverOpen}>
      <PopoverTrigger asChild>{trigger}</PopoverTrigger>
      <PopoverContent
        align="start"
        sideOffset={6}
        className="w-96 p-3"
      >
        {mode === 'view' ? renderViewContent() : renderPickContent()}
      </PopoverContent>
    </Popover>
  );
}
