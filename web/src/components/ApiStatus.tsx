import CheckCircleIcon from "@mui/icons-material/CheckCircle";
import ErrorIcon from "@mui/icons-material/Error";
import LockIcon from "@mui/icons-material/Lock";
import { useQuery } from "@tanstack/react-query";

import { ApiError, getJson } from "../api";

const CHECK_EVERY_MS = 30_000;

/** Whether the API can be reached through the proxy, and we're signed in. */
export default function ApiStatus() {
  const { data, isPending, error } = useQuery({
    queryKey: ["health"],
    queryFn: () => getJson<{ status: string; version?: string }>("/health"),
    refetchInterval: CHECK_EVERY_MS,
    retry: false,
  });

  if (isPending) {
    return <p className="text-xs text-stone-500 md:mt-auto">Checking API…</p>;
  }
  // The browser asks for the password itself (it's GOWA's login). This shows
  // when that box was cancelled, or the password was wrong.
  if (error instanceof ApiError && error.status === 401) {
    return (
      <p className="flex items-center gap-1.5 text-xs text-amber-700 md:mt-auto dark:text-amber-400">
        <LockIcon className="size-4" />
        Password needed: reload to sign in
      </p>
    );
  }
  if (error) {
    return (
      <p className="flex items-center gap-1.5 text-xs text-red-700 md:mt-auto dark:text-red-400">
        <ErrorIcon className="size-4" />
        API unreachable
      </p>
    );
  }
  return (
    <p className="flex items-center gap-1.5 text-xs text-stone-500 md:mt-auto dark:text-stone-400">
      <CheckCircleIcon className="size-4 text-teal-700 dark:text-teal-400" />
      API connected{data.version && ` · v${data.version}`}
    </p>
  );
}
