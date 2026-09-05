export function utf8(text: string): Uint8Array {
  return new TextEncoder().encode(text);
}

export function base64ToBytes(base64: string): Uint8Array {
  const binary = atob(base64);
  return Uint8Array.from(binary, (c) => c.charCodeAt(0));
}

export function bytesToBase64Url(bytes: Uint8Array): string {
  const binary = Array.from(bytes, (b) => String.fromCharCode(b)).join("");
  return btoa(binary)
    .replace(/\+/g, "-")
    .replace(/\//g, "_")
    .replace(/=+$/, "");
}

export function base64UrlToBytes(text: string): Uint8Array {
  const padded = text.padEnd(Math.ceil(text.length / 4) * 4, "=");
  return base64ToBytes(padded.replace(/-/g, "+").replace(/_/g, "/"));
}

export function textToBase64Url(text: string): string {
  return bytesToBase64Url(utf8(text));
}

export function base64UrlToText(text: string): string {
  return new TextDecoder().decode(base64UrlToBytes(text));
}
