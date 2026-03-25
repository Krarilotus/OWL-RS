export function apiBasePath(): string {
  const path = window.location.pathname.replace(/\/+$/, "");
  if (path.startsWith("/console")) {
    return "";
  }
  return "";
}

export function apiPath(path: string): string {
  return `${apiBasePath()}${path}`;
}
