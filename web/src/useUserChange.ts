import { useMutation, useQueryClient } from "@tanstack/react-query";

import { sendJson, type UserDetail } from "./api";

type Request = { method: "PATCH" | "POST"; path: string; body: unknown };

/**
 * A change to one person (a setting, a reminder switch). The API answers
 * with their updated page data, which replaces what's shown, so the page
 * updates without loading again.
 */
export function useUserChange(userId: number) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ method, path, body }: Request) =>
      sendJson<UserDetail>(method, `/users/${userId}${path}`, body),
    onSuccess: (data) => {
      queryClient.setQueryData(["user", String(userId)], data);
      // The users list shows "private chat", which a first change can add.
      queryClient.invalidateQueries({ queryKey: ["users"] });
    },
  });
}
