const CROCKFORD = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";
const TIME_LEN = 10;
const ULID_LEN = 26;
const UUID_V7_RE =
  /^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-7[0-9a-fA-F]{3}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$/;

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

export function uuidV7TimestampMs(uuid: string): number {
  if (!UUID_V7_RE.test(uuid)) {
    throw new Error(`UUIDv7 expected; got ${uuid}`);
  }
  return Number.parseInt(uuid.replace(/-/g, "").slice(0, 12), 16);
}

export function orderedIdTimestampMs(id: string): number {
  if (id.length === ULID_LEN) return ulidTimestampMs(id);
  return uuidV7TimestampMs(id);
}
