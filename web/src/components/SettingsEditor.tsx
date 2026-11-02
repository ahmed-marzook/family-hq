import EditIcon from "@mui/icons-material/Edit";
import { useQuery } from "@tanstack/react-query";
import { useState, type FormEvent, type ReactNode } from "react";

import { ApiError, getJson, type PrayerMethod, type Setting, type Settings } from "../api";
import { useChange, type Owner } from "../useChange";
import Switch from "./Switch";

const input =
  "rounded-md border border-stone-300 bg-white px-2 py-1 text-sm dark:border-stone-700 dark:bg-stone-950";
const primary =
  "rounded-md bg-teal-700 px-3 py-1 text-sm font-medium text-white hover:bg-teal-800 disabled:opacity-50 dark:bg-teal-600 dark:hover:bg-teal-500";
const secondary =
  "rounded-md px-3 py-1 text-sm font-medium text-stone-600 hover:bg-stone-100 dark:text-stone-400 dark:hover:bg-stone-800";

/**
 * A person's or a group's settings, each one editable. Changes are saved
 * at once. `where` names the chat prayer reminders go to; `note` explains
 * what a change does.
 */
export default function SettingsEditor({
  owner,
  name,
  settings,
  where,
  note,
}: {
  owner: Owner;
  name: string;
  settings: Settings;
  where: string;
  note: ReactNode;
}) {
  const { values } = settings;
  const reset = useChange(owner);
  const reminders = useChange(owner);
  const anyOwn = [
    settings.place,
    settings.timezone,
    settings.prayer_method,
    settings.asr_school,
    settings.ending_minutes,
  ].some((s) => s.own);

  return (
    <>
      <dl className="divide-y divide-stone-200 dark:divide-stone-800">
        <EditRow
          label="Town"
          owner={owner}
          setting={settings.place}
          hint="Looked up by name; its timezone comes with it."
          form={(save) => <TownForm city={values.city} country={values.country} save={save} />}
        />
        <EditRow
          label="Timezone"
          owner={owner}
          setting={settings.timezone}
          form={(save) => <TimezoneForm timezone={values.timezone} save={save} />}
        />
        <EditRow
          label="Prayer method"
          owner={owner}
          setting={settings.prayer_method}
          form={(save) => <MethodForm method={values.prayer_method} save={save} />}
        />
        <EditRow
          label="Asr"
          owner={owner}
          setting={settings.asr_school}
          form={(save) => <AsrForm school={values.asr_school} save={save} />}
        />
        <EditRow
          label="“Ending soon” reminder"
          owner={owner}
          setting={{
            value: `${settings.ending_minutes.value} minutes before`,
            own: settings.ending_minutes.own,
          }}
          form={(save) => <EndingForm minutes={values.ending_minutes} save={save} />}
        />
        <div className="flex flex-wrap items-center justify-between gap-x-4 gap-y-1 py-2">
          <dt className="text-sm text-stone-600 dark:text-stone-400">
            Prayer reminders ({where})
          </dt>
          <dd className="flex items-center gap-2">
            {settings.prayer_reminders ? "On" : "Off"}
            <Switch
              on={settings.prayer_reminders}
              label="Prayer reminders"
              disabled={reminders.isPending}
              onChange={(on) =>
                reminders.mutate({ method: "POST", path: "/prayer-reminders", body: { on } })
              }
            />
          </dd>
        </div>
      </dl>
      <Problem error={reminders.error} />

      <div className="mt-3 flex flex-wrap items-center gap-3">
        <button
          type="button"
          className={secondary}
          disabled={!anyOwn || reset.isPending}
          onClick={() => {
            if (window.confirm(`Put ${name}'s settings back to the defaults?`)) {
              reset.mutate({ method: "POST", path: "/settings/reset", body: {} });
            }
          }}
        >
          Reset to defaults
        </button>
        <Problem error={reset.error} />
      </div>
      <p className="mt-3 text-sm text-stone-600 dark:text-stone-400">{note}</p>
    </>
  );
}

type Save = (body: Record<string, unknown>) => void;

/** One setting: its value and "Own"/"Default", or its edit form. */
function EditRow({
  label,
  owner,
  setting,
  hint,
  form,
}: {
  label: string;
  owner: Owner;
  setting: Setting<string>;
  hint?: string;
  form: (save: Save) => ReactNode;
}) {
  const [editing, setEditing] = useState(false);
  const change = useChange(owner);
  const save: Save = (body) =>
    change.mutate(
      { method: "PATCH", path: "/settings", body },
      { onSuccess: () => setEditing(false) },
    );

  return (
    <div className="py-2">
      <div className="flex flex-wrap items-center justify-between gap-x-4 gap-y-1">
        <dt className="text-sm text-stone-600 dark:text-stone-400">{label}</dt>
        <dd className="flex items-center gap-2">
          {setting.value}
          <span
            className={`rounded px-1.5 py-0.5 text-xs font-medium ${
              setting.own
                ? "bg-teal-50 text-teal-800 dark:bg-teal-950 dark:text-teal-300"
                : "bg-stone-100 text-stone-600 dark:bg-stone-800 dark:text-stone-400"
            }`}
          >
            {setting.own ? "Own" : "Default"}
          </span>
          {!editing && (
            <button
              type="button"
              aria-label={`Edit ${label}`}
              onClick={() => {
                change.reset();
                setEditing(true);
              }}
              className="rounded p-1 text-stone-500 hover:bg-stone-100 hover:text-stone-800 dark:hover:bg-stone-800 dark:hover:text-stone-200"
            >
              <EditIcon className="size-4" />
            </button>
          )}
        </dd>
      </div>
      {editing && (
        <div className="mt-2 rounded-md bg-stone-50 p-3 dark:bg-stone-800/50">
          <fieldset disabled={change.isPending} className="flex flex-wrap items-end gap-2">
            {form(save)}
            <button type="button" className={secondary} onClick={() => setEditing(false)}>
              Cancel
            </button>
          </fieldset>
          {hint && <p className="mt-2 text-xs text-stone-500">{hint}</p>}
          <Problem error={change.error} />
        </div>
      )}
    </div>
  );
}

/** Why a change failed: the API's own reason when it gave one. */
function Problem({ error }: { error: Error | null }) {
  if (!error) return null;
  const text =
    error instanceof ApiError && error.reason
      ? error.reason
      : "Couldn't save that. Is family-hq running?";
  return (
    <p role="alert" className="mt-2 text-sm text-red-700 dark:text-red-400">
      {text}
    </p>
  );
}

/** A form that calls `onSubmit` instead of reloading the page. */
function Form({ onSubmit, children }: { onSubmit: () => void; children: ReactNode }) {
  return (
    <form
      className="contents"
      onSubmit={(e: FormEvent) => {
        e.preventDefault();
        onSubmit();
      }}
    >
      {children}
      <button type="submit" className={primary}>
        Save
      </button>
    </form>
  );
}

function Field({ label, children }: { label: string; children: ReactNode }) {
  return (
    <label className="flex flex-col gap-1 text-xs text-stone-600 dark:text-stone-400">
      {label}
      {children}
    </label>
  );
}

function TownForm({ city, country, save }: { city: string; country: string; save: Save }) {
  const [c, setC] = useState(city);
  const [k, setK] = useState(country);
  return (
    <Form onSubmit={() => save({ city: c, country: k })}>
      <Field label="Town">
        <input className={input} value={c} onChange={(e) => setC(e.target.value)} required autoFocus />
      </Field>
      <Field label="Country">
        <input
          className={`${input} w-20 uppercase`}
          value={k}
          onChange={(e) => setK(e.target.value)}
          placeholder="GB"
          maxLength={40}
        />
      </Field>
    </Form>
  );
}

// Every timezone the browser knows, for suggestions while typing.
const TIMEZONES: string[] = Intl.supportedValuesOf?.("timeZone") ?? [];

function TimezoneForm({ timezone, save }: { timezone: string; save: Save }) {
  const [value, setValue] = useState(timezone);
  return (
    <Form onSubmit={() => save({ timezone: value })}>
      <Field label="Timezone">
        <input
          className={`${input} w-56`}
          value={value}
          onChange={(e) => setValue(e.target.value)}
          list="timezones"
          required
          autoFocus
        />
        <datalist id="timezones">
          {TIMEZONES.map((tz) => (
            <option key={tz} value={tz} />
          ))}
        </datalist>
      </Field>
    </Form>
  );
}

function MethodForm({ method, save }: { method: number; save: Save }) {
  const [value, setValue] = useState(method);
  const methods = useQuery({
    queryKey: ["prayer-methods"],
    queryFn: () => getJson<PrayerMethod[]>("/prayer-methods"),
    staleTime: Infinity,
  });
  return (
    <Form onSubmit={() => save({ prayer_method: value })}>
      <Field label="Method">
        <select
          className={`${input} max-w-72`}
          value={value}
          onChange={(e) => setValue(Number(e.target.value))}
          autoFocus
        >
          {(methods.data ?? [{ id: method, name: `Method ${method}` }]).map((m) => (
            <option key={m.id} value={m.id}>
              {m.name} ({m.id})
            </option>
          ))}
        </select>
      </Field>
    </Form>
  );
}

function AsrForm({ school, save }: { school: number; save: Save }) {
  const [value, setValue] = useState(school);
  return (
    <Form onSubmit={() => save({ asr_school: value })}>
      <Field label="Asr">
        <select
          className={input}
          value={value}
          onChange={(e) => setValue(Number(e.target.value))}
          autoFocus
        >
          <option value={0}>Shafi'i</option>
          <option value={1}>Hanafi</option>
        </select>
      </Field>
    </Form>
  );
}

function EndingForm({ minutes, save }: { minutes: number; save: Save }) {
  const [value, setValue] = useState(String(minutes));
  return (
    <Form onSubmit={() => save({ ending_minutes: Number(value) })}>
      <Field label="Minutes before the end">
        <input
          className={`${input} w-24`}
          type="number"
          min={1}
          max={120}
          value={value}
          onChange={(e) => setValue(e.target.value)}
          required
          autoFocus
        />
      </Field>
    </Form>
  );
}
