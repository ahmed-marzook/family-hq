import { useMutation, useQueryClient } from "@tanstack/react-query";

import { sendJson } from "./api";

type Request = { method: "PATCH" | "POST" | "PUT"; path: string; body: unknown };

/** Whose settings a change is for: their API path and their page's query. */
export type Owner = { path: string; queryKey: string[] };

export const userOwner = (userId: number): Owner => ({
  path: `/users/${userId}`,
  queryKey: ["user", String(userId)],
});

export const groupOwner = (groupId: number): Owner => ({
  path: `/groups/${groupId}`,
  queryKey: ["group", String(groupId)],
});

/**
 * A change to one person or group (a setting, a reminder switch). The API
 * answers with their updated page data, which replaces what's shown, so
 * the page updates without loading again.
 */
export function useChange(owner: Owner) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ method, path, body }: Request) =>
      sendJson<unknown>(method, `${owner.path}${path}`, body),
    onSuccess: (data) => {
      queryClient.setQueryData(owner.queryKey, data);
      // The users list shows "private chat", which a first change can add.
      queryClient.invalidateQueries({ queryKey: ["users"] });
    },
  });
}

/** A change to one person. */
export function useUserChange(userId: number) {
  return useChange(userOwner(userId));
}
