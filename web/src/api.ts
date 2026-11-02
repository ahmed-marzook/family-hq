/** A response from the API that wasn't a success. */
export class ApiError extends Error {
  status: number;

  constructor(status: number, path: string) {
    super(`${path} answered ${status}`);
    this.status = status;
  }
}

/** GETs a path under `/api` and returns its JSON. */
export async function getJson<T>(path: string): Promise<T> {
  const response = await fetch(`/api${path}`);
  if (!response.ok) {
    throw new ApiError(response.status, path);
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
};

export type UserList = {
  recent_days: number;
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
};

export type Chat = {
  id: number;
  kind: "group" | "dm";
  name: string | null;
  open_todos: number;
  done_todos: number;
  to_buy: number;
  bought: number;
  todo_reminders: boolean;
};

export type UserDetail = {
  recent_days: number;
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
