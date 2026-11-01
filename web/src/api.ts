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
