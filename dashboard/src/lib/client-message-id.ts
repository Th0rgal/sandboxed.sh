type BrowserCrypto = Pick<Crypto, "getRandomValues"> & {
  randomUUID?: () => string;
};

function fillRandomBytes(bytes: Uint8Array): Uint8Array {
  for (let i = 0; i < bytes.length; i += 1) {
    bytes[i] = Math.floor(Math.random() * 256);
  }
  return bytes;
}

function formatUuidV4(bytes: Uint8Array): string {
  bytes[6] = (bytes[6] & 0x0f) | 0x40;
  bytes[8] = (bytes[8] & 0x3f) | 0x80;

  const hex = Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0"));
  return [
    hex.slice(0, 4).join(""),
    hex.slice(4, 6).join(""),
    hex.slice(6, 8).join(""),
    hex.slice(8, 10).join(""),
    hex.slice(10, 16).join(""),
  ].join("-");
}

export function createClientMessageId(
  cryptoImpl: BrowserCrypto | undefined = globalThis.crypto,
): string {
  if (typeof cryptoImpl?.randomUUID === "function") {
    return cryptoImpl.randomUUID();
  }

  const bytes = new Uint8Array(16);
  if (typeof cryptoImpl?.getRandomValues === "function") {
    cryptoImpl.getRandomValues(bytes);
    return formatUuidV4(bytes);
  }

  // The message id is sent as `client_message_id`, which the backend parses as
  // a UUID. When no crypto source is available, fall back to a Math.random v4
  // UUID so the value still deserializes server-side.
  return formatUuidV4(fillRandomBytes(bytes));
}
