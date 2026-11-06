import CheckBoxIcon from "@mui/icons-material/CheckBox";
import CheckBoxOutlineBlankIcon from "@mui/icons-material/CheckBoxOutlineBlank";
import { useQuery } from "@tanstack/react-query";
import { useState } from "react";

import { getJson, type GroupShop, type GroupTodos } from "../api";
import { dateTime } from "../format";
import LoadError from "./LoadError";

type Tab = "todos" | "shop";

/** A group's todos and shopping list, loaded when opened. */
export default function GroupLists({ groupId }: { groupId: number }) {
  const [open, setOpen] = useState<Tab | null>(null);

  const tab = (name: Tab, label: string) => (
    <button
      type="button"
      onClick={() => setOpen(open === name ? null : name)}
      aria-expanded={open === name}
      className={`rounded-md px-3 py-1.5 text-sm font-medium ${
        open === name
          ? "bg-teal-50 text-teal-800 dark:bg-teal-950 dark:text-teal-300"
          : "text-stone-600 hover:bg-stone-100 dark:text-stone-400 dark:hover:bg-stone-800"
      }`}
    >
      {label}
    </button>
  );

  return (
    <div className="mt-3">
      <div className="flex gap-1">
        {tab("todos", "Todos")}
        {tab("shop", "Shopping")}
      </div>
      {open === "todos" && <Todos groupId={groupId} />}
      {open === "shop" && <Shop groupId={groupId} />}
    </div>
  );
}

const listClass = "mt-2 flex flex-col gap-1.5 text-sm";

function Todos({ groupId }: { groupId: number }) {
  const { data, error, isPending } = useQuery({
    queryKey: ["group-todos", groupId],
    queryFn: () => getJson<GroupTodos>(`/chats/${groupId}/todos`),
  });
  if (isPending) return <p className="mt-2 text-sm text-stone-500">Loading…</p>;
  if (error) return <LoadError error={error} />;
  if (data.todos.length === 0) {
    return <p className="mt-2 text-sm text-stone-500">No todos.</p>;
  }
  return (
    <ul className={listClass}>
      {data.todos.map((todo) => (
        <li key={todo.id} className="flex items-start gap-2">
          <Tick done={todo.done_at !== null && !todo.skipped} />
          <span>
            <span className={todo.done_at ? "text-stone-500 line-through" : ""}>
              {todo.title}
            </span>
            {todo.assignee && (
              <span className="text-stone-500"> · {todo.assignee}</span>
            )}
            {todo.done_at && (
              <span className="block text-xs text-stone-500">
                {todo.skipped ? "Skipped" : "Done"} by {todo.done_by},{" "}
                {dateTime(todo.done_at, data.group.timezone)}
              </span>
            )}
          </span>
        </li>
      ))}
    </ul>
  );
}

function Shop({ groupId }: { groupId: number }) {
  const { data, error, isPending } = useQuery({
    queryKey: ["group-shop", groupId],
    queryFn: () => getJson<GroupShop>(`/chats/${groupId}/shop`),
  });
  if (isPending) return <p className="mt-2 text-sm text-stone-500">Loading…</p>;
  if (error) return <LoadError error={error} />;
  if (data.items.length === 0) {
    return <p className="mt-2 text-sm text-stone-500">The list is empty.</p>;
  }
  return (
    <ul className={listClass}>
      {data.items.map((item) => (
        <li key={item.id} className="flex items-center gap-2">
          <Tick done={item.bought} />
          <span className={item.bought ? "text-stone-500 line-through" : ""}>
            {item.name}
          </span>
        </li>
      ))}
    </ul>
  );
}

function Tick({ done }: { done: boolean }) {
  return done ? (
    <CheckBoxIcon className="size-5 shrink-0 text-teal-700 dark:text-teal-400" aria-label="Done" />
  ) : (
    <CheckBoxOutlineBlankIcon className="size-5 shrink-0 text-stone-400" aria-label="Open" />
  );
}
