import CheckCircleIcon from "@mui/icons-material/CheckCircle";
import ErrorIcon from "@mui/icons-material/Error";
import { useQuery } from "@tanstack/react-query";

import { getJson } from "../api";

const CHECK_EVERY_MS = 30_000;

/** Whether the API can be reached through the proxy. */
export default function ApiStatus() {
  const { isPending, isError } = useQuery({
    queryKey: ["health"],
    queryFn: () => getJson<{ status: string }>("/health"),
    refetchInterval: CHECK_EVERY_MS,
    retry: false,
  });

  if (isPending) {
    return <p className="text-xs text-stone-500 md:mt-auto">Checking API…</p>;
  }
  if (isError) {
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
      API connected
    </p>
  );
}
