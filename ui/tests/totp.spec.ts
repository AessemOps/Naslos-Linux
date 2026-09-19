import { test, expect } from '@playwright/test';
import { totp } from './totp';

// RFC 6238 appendix B vectors (HMAC-SHA1, 8 digits, T0=0, X=30). The secret
// "12345678901234567890" in base32 is the string below. If this fails the auth
// setup would mint wrong codes and Authelia's regulation would ban the account,
// so the helper is pinned before it is trusted.
const SECRET = 'GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ';

test('TOTP matches the RFC 6238 test vectors', () => {
  expect(totp(SECRET, 59_000, 8)).toBe('94287082');
  expect(totp(SECRET, 1_111_111_109_000, 8)).toBe('07081804');
  expect(totp(SECRET, 1_111_111_111_000, 8)).toBe('14050471');
  expect(totp(SECRET, 1_234_567_890_000, 8)).toBe('89005924');
  expect(totp(SECRET, 2_000_000_000_000, 8)).toBe('69279037');
  expect(totp(SECRET, 20_000_000_000_000, 8)).toBe('65353130');
});
