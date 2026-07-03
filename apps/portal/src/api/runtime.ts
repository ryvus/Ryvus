export function gatewayUrl(path: string): string {
  const configured = import.meta.env.VITE_RYVUS_GATEWAY_URL;
  const baseUrl =
    typeof configured === "string" && configured.trim()
      ? configured
      : defaultGatewayUrl();

  return `${baseUrl.replace(/\/$/, "")}${path}`;
}

export function defaultGatewayUrl(): string {
  const url = new URL(window.location.href);
  url.port = "8080";
  url.pathname = "";
  url.search = "";
  url.hash = "";
  return url.toString().replace(/\/$/, "");
}
