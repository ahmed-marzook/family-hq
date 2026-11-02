import ErrorIcon from "@mui/icons-material/Error";

import { ApiError } from "../api";

/** Why something didn't load, in words. */
export default function LoadError({
  error,
  notFound = "Not found.",
}: {
  error: Error;
  notFound?: string;
}) {
  let text = "Couldn't load this. Is family-hq running?";
  if (error instanceof ApiError && error.status === 404) text = notFound;
  if (error instanceof ApiError && error.status === 401) {
    text = "Password needed: reload the page to sign in.";
  }
  return (
    <p className="flex items-center gap-2 text-sm text-red-700 dark:text-red-400">
      <ErrorIcon className="size-5" />
      {text}
    </p>
  );
}
