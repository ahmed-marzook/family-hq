/** A response from the API that wasn't a success. */
export class ApiError extends Error {
  status: number;
  /** The API's own explanation (`{"error": "..."}`), if it gave one. */
  reason: string | null;

  constructor(status: number, path: string, reason: string | null = null) {
    super(reason ?? `${path} answered ${status}`);
    this.status = status;
    this.reason = reason;
  }
}

/** The `error` text from a failed response's JSON, if there is one. */
async function reasonOf(response: Response): Promise<string | null> {
  try {
    const body = await response.json();
    return typeof body?.error === "string" ? body.error : null;
  } catch {
    return null;
  }
}

/** GETs a path under `/api` and returns its JSON. */
export async function getJson<T>(path: string): Promise<T> {
  const response = await fetch(`/api${path}`);
  if (!response.ok) {
    throw new ApiError(response.status, path, await reasonOf(response));
  }
  return response.json() as Promise<T>;
}

/** Sends `body` as JSON to a path under `/api` and returns the answer. */
export async function sendJson<T>(
  method: "PATCH" | "POST",
  path: string,
  body: unknown,
): Promise<T> {
  const response = await fetch(`/api${path}`, {
    method,
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
  if (!response.ok) {
    throw new ApiError(response.status, path, await reasonOf(response));
  }
  return response.json() as Promise<T>;
}

// The API's JSON, as sent by src/routes/admin.rs. Times are RFC 3339, UTC.

export type UserRow = {
  id: number;
  name: string;
  first_seen: string;
  last_seen: string | null;
  recent_uses: number;
  groups: number;
  has_dm: boolean;
  /** When they last used the bot (any message in their DM counts). */
  last_active: string;
  warned_at: string | null;
  /** Their data was cleared then; only their name is left. */
  cleared_at: string | null;
  /** When their data will be cleared if they don't use the bot. */
  clears_on: string | null;
};

/** How clearing data nobody uses is set up (INACTIVE_DAYS). */
export type Cleanup = { inactive_days: number; warn_days: number };

export type UserList = {
  recent_days: number;
  cleanup: Cleanup;
  users: UserRow[];
};

/** A setting's value, and whether it's the person's own or the default. */
export type Setting<T> = { value: T; own: boolean };

export type Settings = {
  place: Setting<string>;
  timezone: Setting<string>;
  prayer_method: Setting<string>;
  asr_school: Setting<string>;
  ending_minutes: Setting<number>;
  prayer_reminders: boolean;
  /** The same settings as plain values, for the edit forms. */
  values: {
    city: string;
    country: string;
    timezone: string;
    prayer_method: number;
    asr_school: number;
    ending_minutes: number;
  };
};

export type PrayerMethod = { id: number; name: string };

export type Chat = {
  id: number;
  kind: "group" | "dm";
  name: string | null;
  open_todos: number;
  done_todos: number;
  to_buy: number;
  bought: number;
  todo_reminders: boolean;
  /** For a group: when someone last used the bot there. */
  last_active: string;
  warned_at: string | null;
  /** For a group: the bot isn't in it any more (noticed then). */
  left_at: string | null;
  /** For a group: when the bot will leave it if nobody uses it. */
  leaves_on: string | null;
};

export type UserDetail = {
  recent_days: number;
  cleanup: Cleanup;
  user: UserRow;
  settings: Settings;
  chats: Chat[];
};

export type Group = { id: number; name: string | null; timezone: string };

export type Todo = {
  id: number;
  title: string;
  assignee: string | null;
  done_by: string | null;
  done_at: string | null;
};

export type GroupTodos = { group: Group; todos: Todo[] };

export type ShopItem = { id: number; name: string; bought: boolean };

export type GroupShop = { group: Group; items: ShopItem[] };

export type Metrics = {
  /** The period; `null` means all time. */
  days: number | null;
  /** The clock `per_day` and `hours` use. */
  timezone: string;
  totals: { uses: number; people: number; unknown: number; failures: number };
  per_day: { day: string; uses: number; people: number }[];
  /** Uses in each hour of the day, 0 to 23. */
  hours: number[];
  commands: { name: string; uses: number }[];
  people: { name: string; uses: number; active_days: number; last_seen: string | null }[];
  unknown: { typed: string; times: number; people: number }[];
  failures: { at: string; person: string; name: string; detail: string | null }[];
  slowest: { name: string; uses: number; avg_ms: number; max_ms: number }[];
};
