const CROCKFORD = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";
const TIME_LEN = 10;
const ULID_LEN = 26;

export function ulidTimestampMs(ulid: string): number {
  if (ulid.length !== ULID_LEN) {
    throw new Error(`ULID must be 26 characters; got ${ulid.length}`);
  }
  const upper = ulid.toUpperCase();
  let ms = 0;
  for (let i = 0; i < TIME_LEN; i++) {
    const idx = CROCKFORD.indexOf(upper[i]);
    if (idx < 0) {
      throw new Error(`ULID character at ${i} (${upper[i]}) is not Crockford-base32`);
    }
    ms = ms * 32 + idx;
  }
  return ms;
}
