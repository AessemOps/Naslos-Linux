import { createHmac } from 'node:crypto';

// Decode an RFC 4648 base32 string (the format Authelia shows for a TOTP
// secret). Padding and separators are tolerated because authenticator apps and
// `otpauth://` URIs present the secret in several spellings.
function base32Decode(input: string): Buffer {
  const alphabet = 'ABCDEFGHIJKLMNOPQRSTUVWXYZ234567';
  const clean = input.toUpperCase().replace(/[^A-Z2-7]/g, '');
  let bits = 0;
  let value = 0;
  const out: number[] = [];

  for (const char of clean) {
    const index = alphabet.indexOf(char);
    if (index < 0) continue;
    value = (value << 5) | index;
    bits += 5;
    if (bits >= 8) {
      out.push((value >>> (bits - 8)) & 0xff);
      bits -= 8;
    }
  }

  return Buffer.from(out);
}

// RFC 6238 TOTP with the parameters Authelia is configured with
// (charts/naslos/templates/authelia-config.yaml: HMAC-SHA1, 6 digits, 30 s).
// Implemented here rather than pulling in a dependency for ~20 lines.
export function totp(secret: string, at: number = Date.now(), digits = 6, period = 30): string {
  const counter = Math.floor(at / 1000 / period);
  const message = Buffer.alloc(8);
  message.writeBigUInt64BE(BigInt(counter));

  const mac = createHmac('sha1', base32Decode(secret)).update(message).digest();
  const offset = mac[mac.length - 1] & 0x0f;
  const binary =
    ((mac[offset] & 0x7f) << 24) |
    ((mac[offset + 1] & 0xff) << 16) |
    ((mac[offset + 2] & 0xff) << 8) |
    (mac[offset + 3] & 0xff);

  return String(binary % 10 ** digits).padStart(digits, '0');
}
