# Later: family dashboard and AI & Extras (Phases 3 and 4)

> **Goal:** the family-facing web app, then optional AI and integrations.
> **Status:** Later. `/weather` and per-person timezones done early.
> **Last updated:** 2026-11-02

## Phase 3: Dashboard

- React + Vite + Tailwind + DaisyUI, built into `web/dist` and embedded in
  the Rust binary (`rust-embed` or similar).
- Read-only first: lists, notes, reminders, streaks, obstacle charts. Then
  editing.
- Login: a magic link sent over WhatsApp.
- Installable as a PWA (an app on the phone's home screen).

## Phase 4: AI & Extras

In rough priority order. Each is optional and independent.

1. Weekly coaching summary from check-ins and obstacles (OpenRouter, small model).
2. Natural-language input ("remind me to call the garage tomorrow"), used
   only when the strict parser returns `None`.
3. 8:00 morning briefing: weather and today's todos and reminders.
   **`/weather` (now, tomorrow, week; Open-Meteo) is already built**; the
   briefing would reuse it.
4. Daily couple's conversation prompts.
5. Travel-aware timezones for prayer windows and reminders. **Mostly done**:
   each person has their own timezone and city, set by sharing their
   location. Still to do: noticing travel automatically.
6. Google Calendar sync and clash detection.
7. Home Assistant presence and ambient alerts.
